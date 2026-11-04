//! «Буфер обмена» — Kadr's Win+V. A small acrylic panel by the pointer: what was copied, newest
//! first, as cards with a preview. Click or Enter pastes into the window that was in front; the pin
//! keeps an entry for good; typing searches. One Direct2D surface like the other windows: the panel
//! pops in, cards cascade, hover lifts, a removed card folds away, the list glides when scrolled.
use crate::clip_history::{Entry, Kind};
use crate::glass::{
    self, Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, scaled_about, stroke, text,
};
use kadr_render::motion::{Curve, Spring};
use kadr_render::scene::{ToolbarButton, icon};
use kadr_render::theme::Theme;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub enum Request {
    Paste(u64),
    Pin(u64, bool),
    Remove(u64),
    Clear,
    /// «Снимки»: open «Недавние».
    Recent,
}

const WIDTH: f32 = 400.0;
const HEIGHT: f32 = 540.0;
const PAD: f32 = 14.0;
const LIST_TOP: f32 = 100.0;
const FOOTER: f32 = 30.0;
const GAP: f32 = 8.0;
const TEXT_CARD: f32 = 78.0;
const FILE_CARD: f32 = 62.0;
const THUMB_MAX: f32 = 120.0;
/// «Очистить» asks again within this long.
const CONFIRM_SECONDS: f64 = 3.0;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Recent,
    Clear,
    Card(usize),
    Pin(usize),
    Remove(usize),
}

struct Card {
    entry: Entry,
    enter: Spring,
    hover: Spring,
    /// 1 while in the list; folds to 0 when the entry is gone, then the card is dropped.
    stay: Spring,
    focus: Spring,
}

struct Fonts {
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    meta: IDWriteTextFormat,
    button: IDWriteTextFormat,
}

struct State {
    hwnd: HWND,
    host: HWND,
    message: u32,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<(f32, Fonts)>,
    bitmaps: HashMap<u64, (ID2D1Bitmap1, f32, f32)>,
    scale: f32,
    epoch: Instant,
    appear: Spring,
    closing: bool,
    cards: Vec<Card>,
    query: String,
    focus: usize,
    scroll: Spring,
    hover: Option<Hit>,
    glow: Vec<(Hit, Spring)>,
    confirm: Option<f64>,
    tracking: bool,
    /// An offscreen render for design review has no window to measure.
    fixed: Option<(f32, f32)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static REQUESTS: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
}

pub fn take_requests() -> Vec<Request> {
    REQUESTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

fn request(st: &State, r: Request) {
    REQUESTS.with(|q| q.borrow_mut().push(r));
    unsafe {
        let _ = PostMessageW(Some(st.host), st.message, WPARAM(0), LPARAM(0));
    }
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// How tall a card is, before folding.
fn card_height(kind: &Kind, inner: f32, s: f32) -> f32 {
    match kind {
        Kind::Text(_) => TEXT_CARD * s,
        Kind::Files(_) => FILE_CARD * s,
        Kind::Image(_, w, h) => {
            let th = (inner * *h as f32 / (*w).max(1) as f32).min(THUMB_MAX * s).max(36.0 * s);
            12.0 * s + th + 30.0 * s
        }
    }
}

/// What the search matches: the text, or the names of the files.
fn matches(entry: &Entry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    match &entry.kind {
        Kind::Text(t) => t.to_lowercase().contains(&q),
        Kind::Files(f) => f.iter().any(|p| p.to_lowercase().contains(&q)),
        Kind::Image(..) => false,
    }
}

/// One line of preview: whitespace runs fold into single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().take(80).collect::<Vec<_>>().join(" ")
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

impl State {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
    fn size(&self) -> (f32, f32) {
        if let Some(size) = self.fixed {
            return size;
        }
        let (w, h) = client_size(self.hwnd);
        (w as f32, h as f32)
    }
    /// The cards the search lets through, with where each sits now (scrolled).
    fn layout(&self, now: f64) -> Vec<(usize, D2D_RECT_F)> {
        let s = self.scale;
        let (w, _) = self.size();
        let inner = w - 2.0 * PAD * s - 24.0 * s;
        let mut y = LIST_TOP * s - self.scroll.value(now);
        let mut out = Vec::new();
        for (i, card) in self.cards.iter().enumerate() {
            if !matches(&card.entry, &self.query) {
                continue;
            }
            let k = card.stay.value(now).clamp(0.0, 1.0);
            let h = card_height(&card.entry.kind, inner, s);
            out.push((i, rect(PAD * s, y, w - PAD * s, y + h * k)));
            y += (h + GAP * s) * k;
        }
        out
    }
    fn content_height(&self, now: f64) -> f32 {
        let s = self.scale;
        let scroll = self.scroll.value(now);
        self.layout(now).last().map_or(0.0, |(_, r)| r.bottom + scroll - LIST_TOP * s)
    }
    fn visible(&self) -> Vec<usize> {
        self.cards
            .iter()
            .enumerate()
            .filter(|(_, c)| c.stay.goal() > 0.5 && matches(&c.entry, &self.query))
            .map(|(i, _)| i)
            .collect()
    }
    fn buttons(&self) -> (D2D_RECT_F, D2D_RECT_F) {
        let s = self.scale;
        let (w, _) = self.size();
        let recent = rect(w - PAD * s - 92.0 * s, 14.0 * s, w - PAD * s, 42.0 * s);
        let clear = rect(recent.left - 8.0 * s - 92.0 * s, 14.0 * s, recent.left - 8.0 * s, 42.0 * s);
        (recent, clear)
    }
    fn search_box(&self) -> D2D_RECT_F {
        let s = self.scale;
        let (w, _) = self.size();
        rect(PAD * s, 54.0 * s, w - PAD * s, 88.0 * s)
    }
    fn card_buttons(r: D2D_RECT_F, s: f32) -> (D2D_RECT_F, D2D_RECT_F) {
        let remove = rect(r.right - 34.0 * s, r.top + 6.0 * s, r.right - 6.0 * s, r.top + 34.0 * s);
        let pin = rect(remove.left - 30.0 * s, remove.top, remove.left - 2.0 * s, remove.bottom);
        (pin, remove)
    }
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let (recent, clear) = self.buttons();
        if inside(recent, x, y) {
            return Some(Hit::Recent);
        }
        if inside(clear, x, y) && self.cards.iter().any(|c| !c.entry.pinned) {
            return Some(Hit::Clear);
        }
        let s = self.scale;
        let (_, h) = self.size();
        if y < LIST_TOP * s - 4.0 * s || y > h - FOOTER * s {
            return None;
        }
        let now = self.now();
        let (i, r) = self.layout(now).into_iter().find(|(_, r)| inside(*r, x, y))?;
        let (pin, remove) = Self::card_buttons(r, s);
        Some(if inside(remove, x, y) {
            Hit::Remove(i)
        } else if inside(pin, x, y) {
            Hit::Pin(i)
        } else {
            Hit::Card(i)
        })
    }
    fn glow(&mut self, h: Hit) -> &mut Spring {
        if let Some(i) = self.glow.iter().position(|(k, _)| *k == h) {
            return &mut self.glow[i].1;
        }
        self.glow.push((h, Spring::with(0.0, Curve::HOVER_IN).precise(0.002)));
        &mut self.glow.last_mut().unwrap().1
    }
    fn set_hover(&mut self, h: Option<Hit>) {
        if self.hover == h {
            return;
        }
        let now = self.now();
        let card = |h: Option<Hit>| match h {
            Some(Hit::Card(i) | Hit::Pin(i) | Hit::Remove(i)) => Some(i),
            _ => None,
        };
        let (old, new) = (card(self.hover), card(h));
        if old != new {
            if let Some(c) = old.and_then(|i| self.cards.get_mut(i)) {
                c.hover.target_with(0.0, Curve::HOVER_OUT, now);
            }
            if let Some(c) = new.and_then(|i| self.cards.get_mut(i)) {
                c.hover.target_with(1.0, Curve::HOVER_IN, now);
            }
            // The card under the pointer is the one Enter pastes; it is not scrolled, the hand is on it.
            if let Some(i) = new {
                self.mark_focus(i);
            }
        }
        if let Some(old) = self.hover {
            self.glow(old).target_with(0.0, Curve::HOVER_OUT, now);
        }
        if let Some(new) = h {
            self.glow(new).target_with(1.0, Curve::HOVER_IN, now);
        }
        self.hover = h;
    }
    fn mark_focus(&mut self, i: usize) {
        let now = self.now();
        for (k, c) in self.cards.iter_mut().enumerate() {
            c.focus.target_with(f32::from(k == i), Curve::GLIDE, now);
        }
        self.focus = i;
    }
    fn set_focus(&mut self, i: usize) {
        self.mark_focus(i);
        let now = self.now();
        // Keep the focused card in view.
        let s = self.scale;
        let (_, h) = self.size();
        if let Some((_, r)) = self.layout(now).into_iter().find(|(k, _)| *k == i) {
            let top = LIST_TOP * s;
            let bottom = h - FOOTER * s;
            let scroll = self.scroll.goal();
            let target = if r.top < top {
                scroll - (top - r.top)
            } else if r.bottom > bottom {
                scroll + (r.bottom - bottom)
            } else {
                scroll
            };
            self.scroll_to(target);
        }
    }
    fn scroll_to(&mut self, target: f32) {
        let now = self.now();
        let s = self.scale;
        let (_, h) = self.size();
        let room = h - FOOTER * s - LIST_TOP * s;
        let max = (self.content_height(now) - room).max(0.0);
        self.scroll.target_with(target.clamp(0.0, max), Curve::GLIDE, now);
    }
    fn press(&mut self, h: Hit) {
        match h {
            Hit::Recent => {
                request(self, Request::Recent);
                self.close(false);
            }
            Hit::Clear => {
                let now = self.now();
                if self.confirm.is_some_and(|t| now - t < CONFIRM_SECONDS) {
                    self.confirm = None;
                    request(self, Request::Clear);
                } else {
                    self.confirm = Some(now);
                }
            }
            Hit::Card(i) => self.paste(i),
            Hit::Pin(i) => {
                if let Some(c) = self.cards.get(i) {
                    request(self, Request::Pin(c.entry.id, !c.entry.pinned));
                }
            }
            Hit::Remove(i) => self.remove(i),
        }
    }
    fn paste(&mut self, i: usize) {
        let Some(c) = self.cards.get(i).filter(|c| c.stay.goal() > 0.5) else { return };
        request(self, Request::Paste(c.entry.id));
        // Away at once: the window in front gets its focus back for the paste.
        self.close(true);
    }
    fn remove(&mut self, i: usize) {
        let now = self.now();
        let Some(c) = self.cards.get_mut(i) else { return };
        c.stay.target_with(0.0, Curve::LAYOUT, now);
        let id = c.entry.id;
        request(self, Request::Remove(id));
    }
    fn close(&mut self, now_hide: bool) {
        if now_hide {
            self.closing = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        if !self.closing {
            self.closing = true;
            let now = self.now();
            self.appear.target_with(0.0, Curve::FADE_OUT, now);
            redraw(self.hwnd);
        }
    }
    /// The list changed: keep each card's motion, cascade new ones in, fold the gone ones away.
    fn set_entries(&mut self, entries: Vec<Entry>, cascade: bool) {
        let now = self.now();
        let mut old: HashMap<u64, Card> = self.cards.drain(..).map(|c| (c.entry.id, c)).collect();
        let mut cards = Vec::with_capacity(entries.len());
        for (i, entry) in entries.into_iter().enumerate() {
            let card = match old.remove(&entry.id) {
                Some(mut c) => {
                    if !matches!(entry.kind, Kind::Image(Some(_), ..))
                        || !matches!(c.entry.kind, Kind::Image(Some(_), ..))
                    {
                        self.bitmaps.remove(&entry.id);
                    }
                    c.entry = entry;
                    c.stay.target_with(1.0, Curve::LAYOUT, now);
                    c
                }
                None => {
                    let mut enter = Spring::with(0.0, Curve::POP).precise(0.001);
                    let delay = if cascade { 0.04 + i.min(12) as f64 * 0.025 } else { 0.0 };
                    enter.target_at(1.0, now, now + delay);
                    let mut stay = Spring::with(if cascade { 1.0 } else { 0.0 }, Curve::LAYOUT).precise(0.001);
                    stay.target(1.0, now);
                    Card {
                        entry,
                        enter,
                        hover: Spring::with(0.0, Curve::HOVER_IN).precise(0.002),
                        stay,
                        focus: Spring::with(0.0, Curve::GLIDE).precise(0.002),
                    }
                }
            };
            cards.push(card);
        }
        // What is gone folds in place rather than vanishing.
        for (_, mut c) in old {
            if c.stay.value(now) > 0.02 {
                c.stay.target_with(0.0, Curve::LAYOUT, now);
                let at = self.cards_position(&cards, c.entry.created_ms);
                cards.insert(at, c);
            }
        }
        self.cards = cards;
        self.bitmaps.retain(|id, _| self.cards.iter().any(|c| c.entry.id == *id));
        let visible = self.visible();
        if !visible.contains(&self.focus) {
            self.focus = visible.first().copied().unwrap_or(0);
        }
        let focus = self.focus;
        for (k, c) in self.cards.iter_mut().enumerate() {
            c.focus.snap(f32::from(k == focus));
        }
    }
    fn cards_position(&self, cards: &[Card], created: u64) -> usize {
        cards.iter().position(|c| c.entry.created_ms < created).unwrap_or(cards.len())
    }
}

fn fonts(st: &mut State) -> bool {
    let Some(device) = st.device.as_ref() else { return false };
    if st.fonts.as_ref().is_some_and(|(s, _)| *s == st.scale) {
        return true;
    }
    let shared = &device.shared;
    let s = st.scale;
    let make = |size: f32, weight: DWRITE_FONT_WEIGHT, wrap: bool, center: bool| unsafe {
        let f = shared
            .dwrite
            .CreateTextFormat(
                PCWSTR(shared.ui_family.as_ptr()),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * s,
                w!("ru-ru"),
            )
            .ok()?;
        let _ = f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
        let _ = f.SetParagraphAlignment(if wrap {
            DWRITE_PARAGRAPH_ALIGNMENT_NEAR
        } else {
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER
        });
        if center {
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        // Long text ends in «…» where the card does.
        if let Ok(sign) = shared.dwrite.CreateEllipsisTrimmingSign(&f) {
            let trimming =
                DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let _ = f.SetTrimming(&trimming, &sign);
        }
        Some(f)
    };
    let made = (|| {
        Some(Fonts {
            title: make(17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false, false)?,
            body: make(14.0, DWRITE_FONT_WEIGHT_REGULAR, true, false)?,
            meta: make(11.5, DWRITE_FONT_WEIGHT_REGULAR, false, false)?,
            button: make(12.5, DWRITE_FONT_WEIGHT_MEDIUM, false, true)?,
        })
    })();
    match made {
        Some(f) => {
            st.fonts = Some((s, f));
            true
        }
        None => false,
    }
}

/// Opens the panel by `at` (the caret or the pointer) with `entries`.
pub fn show(instance: HINSTANCE, host: HWND, message: u32, entries: Vec<Entry>, at: POINT) {
    let exists = STATE.with(|s| s.borrow().is_some());
    if !exists {
        let Some(hwnd) = create(instance) else { return };
        let spring = |v: f32, c: Curve| Spring::with(v, c).precise(0.002);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                host,
                message,
                device: None,
                surface: None,
                fonts: None,
                bitmaps: HashMap::new(),
                scale: 1.0,
                epoch: Instant::now(),
                appear: spring(0.0, Curve::POP),
                closing: false,
                cards: Vec::new(),
                query: String::new(),
                focus: 0,
                scroll: spring(0.0, Curve::GLIDE),
                hover: None,
                glow: Vec::new(),
                confirm: None,
                tracking: false,
                fixed: None,
            })
        });
    }
    STATE.with(|s| {
        let Ok(mut guard) = s.try_borrow_mut() else { return };
        let Some(st) = guard.as_mut() else { return };
        let monitor = unsafe { MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST) };
        let (mut dx, mut dy) = (96u32, 96u32);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let _ = GetMonitorInfoW(monitor, &mut mi);
        }
        st.scale = dx as f32 / 96.0;
        if st.device.is_none() {
            st.device = Device::new("clipboard").ok();
        }
        let (w, h) = ((WIDTH * st.scale) as i32, (HEIGHT * st.scale) as i32);
        let work = mi.rcWork;
        let gap = (12.0 * st.scale) as i32;
        let x = (at.x - w / 2).clamp(work.left + gap, (work.right - w - gap).max(work.left));
        let below = at.y + gap * 2;
        let y = if below + h <= work.bottom - gap { below } else { (at.y - gap - h).max(work.top + gap) };
        st.query.clear();
        st.confirm = None;
        st.closing = false;
        st.hover = None;
        st.glow.clear();
        st.cards.clear();
        st.scroll.snap(0.0);
        st.focus = 0;
        st.set_entries(entries, true);
        let now = st.now();
        st.appear.snap(0.0);
        st.appear.target_with(1.0, Curve::POP, now);
        unsafe {
            glass::match_appearance(st.hwnd);
            let _ = SetWindowPos(st.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(st.hwnd);
        }
        redraw(st.hwnd);
    });
}

/// The history changed while the panel is open.
pub fn update(entries: Vec<Entry>) {
    STATE.with(|s| {
        let Ok(mut guard) = s.try_borrow_mut() else { return };
        let Some(st) = guard.as_mut() else { return };
        if unsafe { IsWindowVisible(st.hwnd) }.as_bool() {
            st.set_entries(entries, false);
            redraw(st.hwnd);
        }
    });
}

pub fn is_open() -> bool {
    STATE.with(|s| {
        s.try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|st| unsafe { IsWindowVisible(st.hwnd) }.as_bool()))
            .unwrap_or(false)
    })
}

fn create(instance: HINSTANCE) -> Option<HWND> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        hInstance: instance,
        lpfnWndProc: Some(proc),
        lpszClassName: w!("KadrClipboard"),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        ..Default::default()
    };
    unsafe {
        RegisterClassExW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            w!("KadrClipboard"),
            w!("Буфер обмена — Kadr"),
            WS_OVERLAPPED | WS_CAPTION,
            0,
            0,
            400,
            540,
            None,
            None,
            Some(instance),
            None,
        )
        .ok()?;
        // Acrylic behind a frameless client area; DWM keeps the shadow and the rounded corners.
        let _ = DwmExtendFrameIntoClientArea(
            hwnd,
            &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 },
        );
        let backdrop = DWMSBT_TRANSIENTWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const _ as *const _, 4);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        Some(hwnd)
    }
}

fn paint_window(hwnd: HWND) {
    let hide = STATE.with(|s| {
        let Ok(mut guard) = s.try_borrow_mut() else { return false };
        let Some(st) = guard.as_mut() else { return false };
        let Some(device) = st.device.as_ref() else { return false };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() || !fonts(st) {
            return false;
        }
        let now = st.now();
        if st.closing && st.appear.value(now) < 0.02 {
            st.closing = false;
            return true;
        }
        // Folded cards leave the list once they are flat.
        st.cards.retain(|c| c.stay.goal() > 0.5 || c.stay.active(now));
        let device = st.device.as_ref().unwrap();
        // Previews become bitmaps on this device the first time they are drawn.
        for c in &st.cards {
            if let Kind::Image(Some(thumb), ..) = &c.entry.kind
                && !st.bitmaps.contains_key(&c.entry.id)
                && let Ok(b) = kadr_render::annotations::bitmap(&device.gpu, thumb)
            {
                st.bitmaps.insert(c.entry.id, (b, thumb.width() as f32, thumb.height() as f32));
            }
        }
        let st = &*st;
        let surface = st.surface.as_ref().unwrap();
        let moving = glass::frame(device, surface, || paint(device, st, now));
        if moving {
            redraw(hwnd);
        }
        false
    });
    if hide {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn paint(device: &Device, st: &State, now: f64) -> bool {
    let gpu = &device.gpu;
    let ctx = &gpu.d2d;
    let f = &st.fonts.as_ref().unwrap().1;
    let theme = Theme::current();
    let s = st.scale;
    let (w, h) = st.size();
    let mut moving = false;
    let mut sample = |spring: &Spring| {
        moving |= spring.active(now);
        spring.value(now)
    };
    let appear = sample(&st.appear);
    let a = appear.clamp(0.0, 1.0);
    let glow = |hit: Hit| st.glow.iter().find(|(k, _)| *k == hit).map_or(0.0, |(_, sp)| sp.value(now));
    let mut busy = st.glow.iter().any(|(_, sp)| sp.active(now)) || st.scroll.active(now);
    let full = rect(0.0, 0.0, w, h);
    fill(gpu, fade(theme.wash, a), full, 0.0);
    // The panel pops in: a touch smaller and lower, settling into place.
    unsafe {
        ctx.SetTransform(&scaled_about(full, 0.96 + 0.04 * appear, 0.0, (1.0 - appear) * 10.0 * s));
    }

    // Header: the title, «Очистить» and «Снимки».
    text(gpu, &f.title, "Буфер обмена", rect(PAD * s + 2.0 * s, 14.0 * s, w * 0.5, 42.0 * s), fade(theme.text, a));
    let (recent, clear) = st.buttons();
    let hover = glow(Hit::Recent);
    fill(gpu, fade(mix(theme.accent, theme.on_selected, 0.12 * hover), a), recent, 14.0 * s);
    let icon_r = rect(recent.left + 6.0 * s, recent.top + 2.0 * s, recent.left + 30.0 * s, recent.bottom - 2.0 * s);
    icon(gpu, ToolbarButton::Recent, icon_r, fade(theme.on_accent, a), s * 0.75);
    text(
        gpu,
        &f.button,
        "Снимки",
        rect(icon_r.right - 4.0 * s, recent.top, recent.right - 6.0 * s, recent.bottom),
        fade(theme.on_accent, a),
    );
    let can_clear = st.cards.iter().any(|c| !c.entry.pinned && c.stay.goal() > 0.5);
    let confirming = st.confirm.is_some_and(|t| now - t < CONFIRM_SECONDS);
    busy |= confirming;
    if can_clear {
        let hover = glow(Hit::Clear);
        let (bg, ink) = if confirming {
            (glass::error_ink(&theme), theme.on_accent)
        } else {
            (mix(theme.tint, theme.tint_strong, hover), theme.text)
        };
        fill(gpu, fade(bg, a), clear, 14.0 * s);
        text(gpu, &f.button, if confirming { "Точно?" } else { "Очистить" }, clear, fade(ink, a));
    }

    // Search.
    let field = st.search_box();
    fill(gpu, fade(theme.tint, a), field, 10.0 * s);
    stroke(
        gpu,
        fade(if st.query.is_empty() { theme.hairline } else { theme.accent }, a),
        inflate(field, -0.5),
        10.0 * s,
        1.0,
    );
    let label = rect(field.left + 12.0 * s, field.top, field.right - 12.0 * s, field.bottom);
    if st.query.is_empty() {
        text(gpu, &f.meta, "Начните печатать, чтобы искать", label, fade(theme.text_muted, a));
    } else {
        // A caret that breathes after what was typed.
        let shown = format!("{}{}", st.query, if (now * 2.0).fract() < 0.55 { "|" } else { " " });
        busy = true;
        text(gpu, &f.meta, &shown, label, fade(theme.text, a));
    }

    // The list, clipped between the search and the footer.
    let list = rect(0.0, LIST_TOP * s - 6.0 * s, w, h - FOOTER * s);
    unsafe {
        ctx.PushAxisAlignedClip(&list, D2D1_ANTIALIAS_MODE_ALIASED);
    }
    let layout = st.layout(now);
    if layout.is_empty() {
        let empty = if st.query.is_empty() {
            "Скопируйте что-нибудь — оно появится здесь.\nЗакреплённое остаётся и после перезагрузки."
        } else {
            "Ничего не нашлось"
        };
        let r = rect(PAD * s * 2.0, LIST_TOP * s + 30.0 * s, w - PAD * s * 2.0, LIST_TOP * s + 120.0 * s);
        unsafe {
            gpu.brush.SetColor(&fade(theme.text_muted, a));
            let value: Vec<u16> = empty.encode_utf16().collect();
            ctx.DrawText(&value, &f.body, &r, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
    let now_ms = crate::recent_worker::now_ms();
    let radius = 12.0 * s;
    for (i, r) in layout {
        let card = &st.cards[i];
        let enter = sample(&card.enter);
        let hover = sample(&card.hover);
        let stay = sample(&card.stay).clamp(0.0, 1.0);
        let focus = sample(&card.focus);
        let opacity = (enter * 1.5).clamp(0.0, 1.0) * stay * a;
        if opacity <= 0.01 || r.bottom < list.top || r.top > list.bottom {
            continue;
        }
        let rise = (1.0 - enter) * 16.0 * s - 1.5 * s * hover;
        unsafe {
            ctx.SetTransform(
                &(scaled_about(r, 1.0 + 0.012 * hover, 0.0, rise)
                    * scaled_about(full, 0.96 + 0.04 * appear, 0.0, (1.0 - appear) * 10.0 * s)),
            );
        }
        glass::shadow(gpu, r, radius, s, 4.0 + 8.0 * hover, 1.0 + 4.0 * hover, (0.08 + 0.10 * hover) * opacity);
        fill(gpu, fade(mix(theme.tile, theme.surface, 0.35 + 0.4 * hover), opacity), r, radius);
        stroke(gpu, fade(mix(theme.hairline, theme.accent, focus), opacity), inflate(r, -0.5), radius, 1.0 + focus * s);
        let inner = rect(r.left + 12.0 * s, r.top + 10.0 * s, r.right - 12.0 * s, r.bottom - 8.0 * s);
        let meta_rect = rect(inner.left, inner.bottom - 16.0 * s, inner.right - 70.0 * s, inner.bottom);
        let when = crate::recent_window::stamp(card.entry.created_ms, now_ms);
        match &card.entry.kind {
            Kind::Text(t) => {
                let body = rect(
                    inner.left,
                    inner.top,
                    inner.right - 62.0 * s * hover.max(f32::from(card.entry.pinned)),
                    inner.bottom - 20.0 * s,
                );
                let value: Vec<u16> = one_line(t).encode_utf16().collect();
                unsafe {
                    gpu.brush.SetColor(&fade(theme.text, opacity));
                    ctx.DrawText(
                        &value,
                        &f.body,
                        &body,
                        &gpu.brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                let chars = t.chars().count();
                text(gpu, &f.meta, &format!("{when} · {chars} симв."), meta_rect, fade(theme.text_muted, opacity));
            }
            Kind::Files(files) => {
                let first = files.first().map_or("", |p| file_name(p));
                let more = if files.len() > 1 { format!(" и ещё {}", files.len() - 1) } else { String::new() };
                let line = rect(inner.left, inner.top, inner.right - 62.0 * s, inner.top + 20.0 * s);
                text(gpu, &f.body, &format!("📄 {first}{more}"), line, fade(theme.text, opacity));
                text(gpu, &f.meta, &format!("{when} · файлы"), meta_rect, fade(theme.text_muted, opacity));
            }
            Kind::Image(_, iw, ih) => {
                let th = (meta_rect.top - 6.0 * s - inner.top).max(1.0);
                let tw = (th * *iw as f32 / (*ih).max(1) as f32).min(inner.right - inner.left);
                let th = tw * *ih as f32 / (*iw).max(1) as f32;
                let pic = rect(inner.left, inner.top, inner.left + tw, inner.top + th);
                match st.bitmaps.get(&card.entry.id) {
                    Some((bitmap, bw, bh)) => {
                        glass::rounded_image(device, bitmap, (*bw, *bh), pic, 8.0 * s, opacity, true)
                    }
                    None => fill(gpu, fade(theme.tint, opacity), pic, 8.0 * s),
                }
                text(gpu, &f.meta, &format!("{when} · {iw}×{ih}"), meta_rect, fade(theme.text_muted, opacity));
            }
        }
        // Pin and remove: shown on hover; a pinned entry keeps its pin lit.
        let (pin, remove) = State::card_buttons(r, s);
        let pinned = card.entry.pinned;
        let show = hover.max(f32::from(pinned));
        if show > 0.01 {
            let ph = glow(Hit::Pin(i));
            fill(gpu, fade(theme.tint_strong, ph * opacity), pin, 8.0 * s);
            let ink = if pinned { theme.accent } else { mix(theme.text_muted, theme.text, ph) };
            icon(gpu, ToolbarButton::Pin, pin, fade(ink, show * opacity), s * 0.7);
        }
        if hover > 0.01 {
            let rh = glow(Hit::Remove(i));
            fill(gpu, fade(theme.tint_strong, rh * opacity), remove, 8.0 * s);
            icon(
                gpu,
                ToolbarButton::Close,
                remove,
                fade(mix(theme.text_muted, theme.text, rh), hover * opacity),
                s * 0.6,
            );
        }
    }
    unsafe {
        ctx.SetTransform(&scaled_about(full, 0.96 + 0.04 * appear, 0.0, (1.0 - appear) * 10.0 * s));
        ctx.PopAxisAlignedClip();
    }

    // Footer: the keys, quietly.
    let foot = rect(PAD * s, h - FOOTER * s, w - PAD * s, h - 6.0 * s);
    text(gpu, &f.meta, "Enter — вставить · Ctrl+P — закрепить · Del — удалить", foot, fade(theme.text_muted, 0.85 * a));
    unsafe {
        ctx.SetTransform(&windows_numerics::Matrix3x2::identity());
    }
    moving || busy
}

fn ctrl() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let point = || ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    let with = |f: &mut dyn FnMut(&mut State)| {
        STATE.with(|s| {
            if let Ok(mut g) = s.try_borrow_mut()
                && let Some(st) = g.as_mut()
            {
                f(st);
            }
        })
    };
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
            }
            paint_window(hwnd);
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        // Like Win+V: a click anywhere else puts it away.
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            let mut track = false;
            with(&mut |st| {
                let h = st.hit(x, y);
                st.set_hover(h);
                track = !st.tracking;
                st.tracking = true;
            });
            if track {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut t);
                }
            }
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(&mut |st| {
                st.tracking = false;
                st.set_hover(None);
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            with(&mut |st| {
                if let Some(h) = st.hit(x, y) {
                    st.press(h);
                }
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) as u16 as i16) as f32 / 120.0;
            with(&mut |st| {
                let target = st.scroll.goal() - notches * 110.0 * st.scale;
                st.scroll_to(target);
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let vk = wp.0 as u16;
            with(&mut |st| {
                let visible = st.visible();
                let at = visible.iter().position(|i| *i == st.focus);
                match vk {
                    k if k == VK_ESCAPE.0 => {
                        if st.query.is_empty() {
                            st.close(false);
                        } else {
                            st.query.clear();
                            st.scroll_to(0.0);
                        }
                    }
                    k if k == VK_DOWN.0 || k == VK_UP.0 || k == VK_HOME.0 || k == VK_END.0 => {
                        let n = visible.len();
                        if n > 0 {
                            let next = match (k, at) {
                                (k, _) if k == VK_HOME.0 => 0,
                                (k, _) if k == VK_END.0 => n - 1,
                                (k, Some(a)) if k == VK_DOWN.0 => (a + 1).min(n - 1),
                                (_, Some(a)) => a.saturating_sub(1),
                                _ => 0,
                            };
                            st.set_focus(visible[next]);
                        }
                    }
                    k if k == VK_RETURN.0 => {
                        if let Some(i) = at.map(|a| visible[a]) {
                            st.paste(i);
                        }
                    }
                    k if k == VK_DELETE.0 => {
                        if let Some(a) = at {
                            st.remove(visible[a]);
                            if let Some(next) = visible.get(a + 1).or(a.checked_sub(1).and_then(|p| visible.get(p))) {
                                st.set_focus(*next);
                            }
                        }
                    }
                    k if k == VK_BACK.0 => {
                        st.query.pop();
                        st.scroll_to(0.0);
                    }
                    0x50 if ctrl() => {
                        if let Some(i) = at.map(|a| visible[a]) {
                            st.press(Hit::Pin(i));
                        }
                    }
                    _ => {}
                }
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_CHAR => {
            let unit = wp.0 as u16;
            if ctrl() {
                return LRESULT(0);
            }
            with(&mut |st| {
                if let Some(c) = char::from_u32(unit as u32).filter(|c| !c.is_control()) {
                    st.query.push(c);
                    st.scroll.snap(0.0);
                    let first = st.visible().first().copied();
                    if let Some(i) = first {
                        st.set_focus(i);
                    }
                }
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Offscreen render for design review: the panel open with text, a picture, files and a pin,
/// the second card hovered.
pub fn preview(picture: std::sync::Arc<kadr_doc::Image>, scale: f32) -> windows::core::Result<kadr_doc::Image> {
    let (w, h) = ((WIDTH * scale) as u32, (HEIGHT * scale) as u32);
    let now_ms = crate::recent_worker::now_ms();
    let entry = |id: u64, ago: u64, pinned: bool, kind: Kind| Entry { id, created_ms: now_ms - ago, pinned, kind };
    let thumb =
        std::sync::Arc::new(kadr_doc::pixels::thumbnail(std::slice::from_ref(&picture), picture.rect, 360, 220));
    let entries = vec![
        entry(
            5,
            20_000,
            false,
            Kind::Text("Встреча перенесена на четверг, 15:30 — ссылка на звонок в календаре".into()),
        ),
        entry(4, 300_000, false, Kind::Image(Some(thumb), picture.width(), picture.height())),
        entry(3, 3_600_000, true, Kind::Text("ivan@example.com".into())),
        entry(
            2,
            7_200_000,
            false,
            Kind::Files(vec![r"C:\Users\me\Documents\Отчёт за октябрь.pdf".into(), r"C:\a.png".into()]),
        ),
        entry(1, 90_000_000, false, Kind::Text("cargo build --release --features uiaccess -p kadr-app".into())),
    ];
    let mut st = State {
        hwnd: HWND::default(),
        host: HWND::default(),
        message: 0,
        device: Some(Device::new("clipboard-preview")?),
        surface: None,
        fonts: None,
        bitmaps: HashMap::new(),
        scale,
        epoch: Instant::now(),
        appear: Spring::with(1.0, Curve::POP),
        closing: false,
        cards: Vec::new(),
        query: String::new(),
        focus: 0,
        scroll: Spring::with(0.0, Curve::GLIDE),
        hover: None,
        glow: Vec::new(),
        confirm: None,
        tracking: false,
        fixed: Some((w as f32, h as f32)),
    };
    st.set_entries(entries, false);
    for c in &mut st.cards {
        c.enter.snap(1.0);
        c.stay.snap(1.0);
    }
    st.cards[1].hover.snap(1.0);
    st.hover = Some(Hit::Card(1));
    if !fonts(&mut st) {
        return Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL));
    }
    let device = st.device.take().unwrap();
    for c in &st.cards {
        if let Kind::Image(Some(t), ..) = &c.entry.kind {
            let b = kadr_render::annotations::bitmap(&device.gpu, t)?;
            st.bitmaps.insert(c.entry.id, (b, t.width() as f32, t.height() as f32));
        }
    }
    let now = st.now();
    glass::render_offscreen(&device, w, h, || {
        let theme = Theme::current();
        fill(
            &device.gpu,
            if theme.dark { glass::rgb(0x2b2b2e, 1.0) } else { glass::rgb(0xeeeef2, 1.0) },
            rect(0.0, 0.0, w as f32, h as f32),
            0.0,
        );
        paint(&device, &st, now);
    })
}

/// Win+V again while open: put it away.
pub fn close() {
    STATE.with(|s| {
        if let Ok(mut g) = s.try_borrow_mut()
            && let Some(st) = g.as_mut()
        {
            st.close(false);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, kind: Kind) -> Entry {
        Entry { id, created_ms: id, pinned: false, kind }
    }

    #[test]
    fn search_reads_text_and_file_names_but_not_pictures() {
        let text = entry(1, Kind::Text("Привет, Мир".into()));
        let files = entry(2, Kind::Files(vec![r"C:\Docs\Отчёт.pdf".into()]));
        let picture = entry(3, Kind::Image(None, 10, 10));
        assert!(matches(&text, "мир") && matches(&files, "отчёт") && matches(&picture, ""));
        assert!(!matches(&picture, "мир") && !matches(&text, "pdf"));
    }

    #[test]
    fn previews_fold_whitespace_and_name_files() {
        assert_eq!(one_line("  a\n\n b\tc  "), "a b c");
        assert_eq!(file_name(r"C:\a\b\c.txt"), "c.txt");
        // A wide picture is shorter than a tall one, both within bounds.
        let wide = card_height(&Kind::Image(None, 1000, 100), 300.0, 1.0);
        let tall = card_height(&Kind::Image(None, 100, 1000), 300.0, 1.0);
        assert!(wide < tall && tall <= 12.0 + THUMB_MAX + 30.0 + 0.01);
    }
}
