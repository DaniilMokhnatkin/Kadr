//! «Недавние». One Direct2D surface over the Windows 11 acrylic backdrop: no child controls and no
//! GDI, so nothing flickers, and every state change (opening cascade, hover lift, focus ring,
//! button press, clear confirmation) is a spring. The title bar is our own; DWM keeps the shadow,
//! rounded corners and dragging.
use crate::glass::{self, Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, rgb, scaled_about, shadow, stroke, text};
use kadr_render::gpu::Shared;
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use kadr_store::recent::Entry;
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{FILETIME, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT,
    ScreenToClient,
};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

pub enum Request {
    Load(String),
    Clear,
    Close,
    Refresh,
}

/// Size of the window in DIPs before it is fitted to its rows.
const WIDTH: f32 = 920.0;
const PAD: f32 = 28.0;
const TOP: f32 = 92.0;
const GAP: f32 = 16.0;
const LABEL: f32 = 34.0;
/// Clear needs a second click within this time.
const CONFIRM_MS: u32 = 3500;
const TIMER_CONFIRM: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Close,
    Back,
    Clear,
    Card(usize),
}

struct Layout {
    close: D2D_RECT_F,
    back: Option<D2D_RECT_F>,
    clear: D2D_RECT_F,
    cards: Vec<D2D_RECT_F>,
}


fn columns(width: f32, s: f32) -> usize {
    if width / s >= 760.0 { 4 } else { 3 }
}

/// Everything in physical pixels; `s` is the DPI scale.
fn layout(width: f32, s: f32, count: usize, returning: bool) -> Layout {
    let close = rect(width - 44.0 * s, 12.0 * s, width - 12.0 * s, 44.0 * s);
    let (h, y) = (34.0 * s, 26.0 * s);
    let right = close.left - 10.0 * s;
    let back = returning.then(|| rect(right - 116.0 * s, y, right, y + h));
    let clear_right = back.map_or(right, |b| b.left - 8.0 * s);
    let clear = rect(clear_right - 112.0 * s, y, clear_right, y + h);
    let cols = columns(width, s);
    let tile_w = (width - 2.0 * PAD * s - (cols - 1) as f32 * GAP * s) / cols as f32;
    let tile_h = (tile_w * 0.625).round();
    let cards = (0..count)
        .map(|i| {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            let left = (PAD * s + c * (tile_w + GAP * s)).round();
            let top = (TOP * s + r * (tile_h + LABEL * s + GAP * s * 0.5)).round();
            rect(left, top, left + tile_w.round(), top + tile_h)
        })
        .collect();
    Layout { close, back, clear, cards }
}

fn desired_height(width: f32, s: f32, count: usize) -> f32 {
    let cols = columns(width, s);
    let rows = count.div_ceil(cols).max(1) as f32;
    let tile_w = (width - 2.0 * PAD * s - (cols - 1) as f32 * GAP * s) / cols as f32;
    let tile_h = (tile_w * 0.625).round();
    TOP * s + rows * (tile_h + LABEL * s) + (rows - 1.0) * GAP * s * 0.5 + 20.0 * s
}

fn hit(l: &Layout, x: f32, y: f32, cards: usize, can_clear: bool) -> Option<Hit> {
    if inside(l.close, x, y) {
        return Some(Hit::Close);
    }
    if l.back.is_some_and(|b| inside(b, x, y)) {
        return Some(Hit::Back);
    }
    if can_clear && inside(l.clear, x, y) {
        return Some(Hit::Clear);
    }
    l.cards
        .iter()
        .take(cards)
        .position(|c| inside(rect(c.left, c.top, c.right, c.bottom + LABEL * 0.6), x, y))
        .map(Hit::Card)
}

fn count_label(n: usize) -> String {
    let word = if (11..=14).contains(&(n % 100)) {
        "снимков"
    } else {
        match n % 10 {
            1 => "снимок",
            2..=4 => "снимка",
            _ => "снимков",
        }
    };
    format!("{n} {word}")
}

fn local(ms: u64) -> Option<SYSTEMTIME> {
    let ticks = ms.saturating_mul(10000).saturating_add(116444736000000000);
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut out) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        (FileTimeToSystemTime(&ft, &mut utc).is_ok() && SystemTimeToTzSpecificLocalTime(None, &utc, &mut out).is_ok())
            .then_some(out)
    }
}

/// «Сегодня, 02:41», «Вчера, 23:10» or «28.10, 14:02».
fn stamp(ms: u64, now_ms: u64) -> String {
    let (Some(t), Some(today)) = (local(ms), local(now_ms)) else {
        return "Время неизвестно".into();
    };
    let day = |s: &SYSTEMTIME| (s.wYear, s.wMonth, s.wDay);
    let yesterday = local(now_ms.saturating_sub(86_400_000));
    let when = if day(&t) == day(&today) {
        "Сегодня".to_string()
    } else if yesterday.is_some_and(|y| day(&y) == day(&t)) {
        "Вчера".to_string()
    } else {
        format!("{:02}.{:02}", t.wDay, t.wMonth)
    };
    format!("{when}, {:02}:{:02}", t.wHour, t.wMinute)
}

struct Fonts {
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    small: IDWriteTextFormat,
    button: IDWriteTextFormat,
}
impl Fonts {
    fn new(shared: &Shared, s: f32) -> Result<Self> {
        let make = |size: f32, weight: DWRITE_FONT_WEIGHT, center: bool| -> Result<IDWriteTextFormat> {
            unsafe {
                let f = shared.dwrite.CreateTextFormat(
                    PCWSTR(shared.ui_family.as_ptr()),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * s,
                    w!("ru-ru"),
                )?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                if center {
                    f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                }
                Ok(f)
            }
        };
        Ok(Self {
            title: make(24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
            body: make(14.0, DWRITE_FONT_WEIGHT_REGULAR, true)?,
            small: make(12.5, DWRITE_FONT_WEIGHT_REGULAR, false)?,
            button: make(13.0, DWRITE_FONT_WEIGHT_MEDIUM, true)?,
        })
    }
}

struct Card {
    id: String,
    created_ms: u64,
    bitmap: Option<(ID2D1Bitmap1, f32, f32)>,
    hover: Spring,
    enter: Spring,
}

/// Device resources: created on first show, kept while the app runs.
struct Ui {
    device: Device,
    fonts: Fonts,
}
impl Ui {
    fn new(s: f32) -> Result<Self> {
        let device = Device::new("recent")?;
        let fonts = Fonts::new(&device.shared, s)?;
        Ok(Self { device, fonts })
    }
}


struct State {
    hwnd: HWND,
    host: HWND,
    message: u32,
    ui: Option<Ui>,
    surface: Option<Surface>,
    cards: Vec<Card>,
    loading: bool,
    enabled: bool,
    error: Option<String>,
    returning: bool,
    scale: f32,
    epoch: Instant,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tracking: bool,
    /// Keyboard focus is drawn only after the keyboard is used, as on macOS.
    focus: usize,
    focus_visible: bool,
    ring: Option<[Spring; 4]>,
    buttons: [(Spring, Spring); 3],
    confirming: bool,
    danger: Spring,
}
impl State {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
    fn button(&mut self, h: Hit) -> &mut (Spring, Spring) {
        &mut self.buttons[match h {
            Hit::Close => 0,
            Hit::Back => 1,
            _ => 2,
        }]
    }
    fn layout(&self, width: f32) -> Layout {
        layout(width, self.scale, self.cards.len(), self.returning)
    }
    fn set_hover(&mut self, h: Option<Hit>) {
        if self.hover == h {
            return;
        }
        let now = self.now();
        for (which, value) in [(self.hover, 0.0), (h, 1.0)] {
            match which {
                Some(Hit::Card(i)) => {
                    if let Some(c) = self.cards.get_mut(i) {
                        let curve = if value > 0.5 { Curve::HOVER_IN } else { Curve::HOVER_OUT };
                        c.hover.target_with(value, curve, now);
                    }
                }
                Some(b) => {
                    let curve = if value > 0.5 { Curve::HOVER_IN } else { Curve::HOVER_OUT };
                    self.button(b).0.target_with(value, curve, now);
                }
                None => {}
            }
        }
        self.hover = h;
    }
    fn press(&mut self, h: Option<Hit>) {
        let now = self.now();
        if let Some(old) = self.pressed.filter(|o| !matches!(o, Hit::Card(_))) {
            self.button(old).1.target_with(0.0, Curve::POP, now);
        }
        if let Some(b) = h.filter(|b| !matches!(b, Hit::Card(_))) {
            self.button(b).1.target_with(1.0, Curve::HOVER_IN, now);
        }
        self.pressed = h;
    }
    fn set_confirm(&mut self, on: bool) {
        self.confirming = on;
        let now = self.now();
        self.danger.target(f32::from(on), now);
        unsafe {
            if on {
                SetTimer(Some(self.hwnd), TIMER_CONFIRM, CONFIRM_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_CONFIRM);
            }
        }
    }
}

thread_local! {
    static DATA: RefCell<Option<State>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Option<Request>> = const { RefCell::new(None) };
}

pub struct Window {
    pub hwnd: HWND,
}

fn new_state(hwnd: HWND, host: HWND, message: u32, scale: f32) -> State {
    let spring = || Spring::with(0.0, Curve::HOVER_IN).precise(0.002);
    State {
        hwnd,
        host,
        message,
        ui: None,
        surface: None,
        cards: Vec::new(),
        loading: true,
        enabled: true,
        error: None,
        returning: false,
        scale,
        epoch: Instant::now(),
        hover: None,
        pressed: None,
        tracking: false,
        focus: 0,
        focus_visible: false,
        ring: None,
        buttons: [(spring(), spring()), (spring(), spring()), (spring(), spring())],
        confirming: false,
        danger: Spring::with(0.0, Curve::FADE).precise(0.002),
    }
}

fn set_attribute<T>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: T) {
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, attribute, &value as *const T as *const _, size_of::<T>() as u32);
    }
}

impl Window {
    pub fn new(instance: HINSTANCE, host: HWND, message: u32, app_icon: HICON, small_icon: HICON) -> Result<Self> {
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                hInstance: instance,
                lpfnWndProc: Some(proc),
                lpszClassName: w!("KadrRecent"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hIcon: app_icon,
                hIconSm: small_icon,
                ..Default::default()
            };
            RegisterClassExW(&class);
            let scale = GetDpiForSystem() as f32 / 96.0;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrRecent"),
                w!("Недавние — Kadr"),
                WS_OVERLAPPED | WS_CAPTION,
                0,
                0,
                (WIDTH * scale) as i32,
                (420.0 * scale) as i32,
                None,
                None,
                Some(instance),
                None,
            )?;
            DATA.with(|d| *d.borrow_mut() = Some(new_state(hwnd, host, message, scale)));
            // Acrylic behind a frameless client area; DWM keeps shadow, corners and snapping.
            let _ = DwmExtendFrameIntoClientArea(
                hwnd,
                &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 },
            );
            set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW);
            set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
            set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, windows::core::BOOL(0));
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            Ok(Self { hwnd })
        }
    }

    pub fn show(&self, enabled: bool, returning: bool) {
        DATA.with(|d| {
            if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(d) = guard.as_mut() {
                d.loading = true;
                d.enabled = enabled;
                d.error = None;
                d.returning = returning;
                d.focus_visible = false;
                d.set_hover(None);
                if d.confirming {
                    d.set_confirm(false);
                }
            }
        });
        self.fit(true);
        unsafe {
            glass::match_appearance(self.hwnd);
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn set_entries(&self, entries: Vec<Entry>) {
        DATA.with(|d| {
            let Ok(mut state) = d.try_borrow_mut() else { return Default::default() };
            let Some(st) = state.as_mut() else { return };
            st.loading = false;
            st.error = None;
            let now = st.now();
            let ui = st.ui.as_ref();
            // Without a device yet the previews wait for the first paint.
            PENDING.with(|p| {
                let mut p = p.borrow_mut();
                p.clear();
                if ui.is_none() {
                    p.extend(entries.iter().enumerate().filter_map(|(i, e)| e.preview.clone().map(|img| (i, img))));
                }
            });
            st.cards = entries
                .into_iter()
                .enumerate()
                .map(|(i, e)| {
                    let bitmap = ui.and_then(|ui| {
                        let p = e.preview.as_ref()?;
                        let b = kadr_render::annotations::bitmap(&ui.device.gpu, p).ok()?;
                        Some((b, p.width() as f32, p.height() as f32))
                    });
                    // Cards cascade in: each rises and fades in a little after its neighbour.
                    let mut enter = Spring::with(0.0, Curve::POP).precise(0.001);
                    enter.target_at(1.0, now, now + 0.03 + i as f64 * 0.028);
                    Card {
                        id: e.id,
                        created_ms: e.created_ms,
                        bitmap,
                        hover: Spring::with(0.0, Curve::HOVER_IN).precise(0.002),
                        enter,
                    }
                })
                .collect();
            st.focus = st.focus.min(st.cards.len().saturating_sub(1));
            st.hover = None;
            st.ring = None;
        });
        self.fit(false);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Height follows the number of rows; the first show also centres the window on its monitor.
    fn fit(&self, center: bool) {
        let Some((scale, count)) =
            DATA.with(|d| d.try_borrow_mut().ok().as_deref_mut().and_then(Option::as_mut).map(|d| (d.scale, d.cards.len().max(if d.loading { 4 } else { 0 }))))
        else {
            return;
        };
        unsafe {
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut mi);
            let work = mi.rcWork;
            let width = ((WIDTH * scale) as i32).min(work.right - work.left - 32);
            let height = (desired_height(width as f32, scale, count) as i32)
                .clamp((300.0 * scale) as i32, work.bottom - work.top - 48);
            let mut outer = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut outer);
            let (x, y) = if center || !IsWindowVisible(self.hwnd).as_bool() {
                (work.left + (work.right - work.left - width) / 2, work.top + (work.bottom - work.top - height) / 2)
            } else {
                (outer.left, outer.top.min(work.bottom - height))
            };
            let _ = SetWindowPos(self.hwnd, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn error(&self, message: &str) {
        DATA.with(|d| {
            if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(d) = guard.as_mut() {
                d.loading = false;
                d.error = Some(message.to_string());
            }
        });
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn take_request(&self) -> Option<Request> {
        REQUEST.with(|r| r.borrow_mut().take())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        DATA.with(|d| d.borrow_mut().take());
    }
}

/// The window handles its own keys; nothing to route through IsDialogMessage any more.
pub fn dialog_message(_msg: &MSG) -> bool {
    false
}

fn request(value: Request) {
    REQUEST.with(|r| *r.borrow_mut() = Some(value));
    let target = DATA.with(|d| d.try_borrow().ok().as_deref().and_then(Option::as_ref).map(|d| (d.host, d.message)));
    if let Some((host, msg)) = target {
        unsafe {
            let _ = PostMessageW(Some(host), msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn close(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    request(Request::Close);
}




/// Thumbnails that were set before the device existed get their bitmaps now.
fn ensure_bitmaps(st: &mut State, previews: &mut Vec<(usize, kadr_doc::Image)>) {
    let Some(ui) = st.ui.as_ref() else { return };
    for (i, image) in previews.drain(..) {
        if let (Some(card), Ok(b)) = (st.cards.get_mut(i), kadr_render::annotations::bitmap(&ui.device.gpu, &image)) {
            card.bitmap = Some((b, image.width() as f32, image.height() as f32));
        }
    }
}

thread_local! {
    /// Previews kept until the first paint creates the device that turns them into bitmaps.
    static PENDING: RefCell<Vec<(usize, kadr_doc::Image)>> = const { RefCell::new(Vec::new()) };
}

fn paint_window(hwnd: HWND) {
    DATA.with(|d| {
        let Ok(mut state) = d.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        if st.ui.is_none() {
            match Ui::new(st.scale) {
                Ok(ui) => st.ui = Some(ui),
                Err(e) => {
                    kadr_win::log_warn!("«Недавние»: устройство: {e}");
                    return;
                }
            }
        }
        if let Err(e) = glass::ensure(&mut st.surface, &st.ui.as_ref().unwrap().device, st.hwnd) {
            kadr_win::log_warn!("«Недавние»: поверхность: {e}");
            return;
        }
        PENDING.with(|p| ensure_bitmaps(st, &mut p.borrow_mut()));
        let ui = st.ui.as_ref().unwrap();
        let surface = st.surface.as_ref().unwrap();
        let size = (surface.size.0 as f32, surface.size.1 as f32);
        let animating = glass::frame(&ui.device, surface, || paint(ui, st, size, st.now(), false));
        if animating {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    });
}


/// Draws the whole window; returns whether anything is still moving.
fn paint(ui: &Ui, st: &State, size: (f32, f32), now: f64, opaque: bool) -> bool {
    let gpu = &ui.device.gpu;
    let ctx = &gpu.d2d;
    let theme = Theme::current();
    let s = st.scale;
    let l = st.layout(size.0);
    let mut moving = false;
    let mut sample = |spring: &Spring| {
        moving |= spring.active(now);
        spring.value(now)
    };
    let full = rect(0.0, 0.0, size.0, size.1);
    if opaque || theme.glass.a >= 1.0 {
        fill(gpu, if theme.dark { rgb(0x202022, 1.0) } else { rgb(0xf3f3f6, 1.0) }, full, 0.0);
    }
    // A light wash over the acrylic keeps text contrast steady whatever is behind the window.
    fill(gpu, theme.wash, full, 0.0);

    // Header.
    text(gpu, &ui.fonts.title, "Недавние", rect(PAD * s, 20.0 * s, size.0 / 2.0, 52.0 * s), theme.text);
    let (subtitle, tone) = match (&st.error, st.loading, st.cards.len()) {
        (Some(e), _, _) => (e.clone(), glass::error_ink(&theme)),
        (None, true, 0) => ("Загрузка…".into(), theme.text_muted),
        (None, _, 0) => ("Несохранённых снимков нет".into(), theme.text_muted),
        (None, _, n) if !st.enabled => (format!("{} · новые снимки не сохраняются", count_label(n)), theme.text_muted),
        (None, _, n) => (format!("{} · нажмите, чтобы продолжить редактирование", count_label(n)), theme.text_muted),
    };
    text(gpu, &ui.fonts.small, &subtitle, rect(PAD * s, 52.0 * s, size.0 * 0.6, 72.0 * s), tone);

    // Close: a quiet circle that fills on hover.
    let (hover, press) = (sample(&st.buttons[0].0), sample(&st.buttons[0].1));
    let c = l.close;
    unsafe {
        ctx.SetTransform(&scaled_about(c, 1.0 - 0.08 * press, 0.0, 0.0));
    }
    fill(gpu, fade(theme.tint, hover), inflate(c, -2.0 * s), 14.0 * s);
    let (cx, cy, k) = ((c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0, 5.0 * s);
    unsafe {
        gpu.brush.SetColor(&mix(theme.text_muted, theme.text, hover));
        for (a, b) in [((-k, -k), (k, k)), ((-k, k), (k, -k))] {
            ctx.DrawLine(
                Vector2 { X: cx + a.0, Y: cy + a.1 },
                Vector2 { X: cx + b.0, Y: cy + b.1 },
                &gpu.brush,
                1.5 * s,
                None::<&ID2D1StrokeStyle>,
            );
        }
        ctx.SetTransform(&Matrix3x2::identity());
    }

    // «К снимку»: the one tinted action.
    if let Some(b) = l.back {
        let (hover, press) = (sample(&st.buttons[1].0), sample(&st.buttons[1].1));
        unsafe {
            ctx.SetTransform(&scaled_about(b, 1.0 - 0.04 * press, 0.0, 0.0));
        }
        shadow(gpu, b, 9.0 * s, s, 4.0, 1.5, 0.10 + 0.06 * hover);
        fill(gpu, mix(theme.accent, theme.on_accent, 0.12 * hover), b, 9.0 * s);
        text(gpu, &ui.fonts.button, "К снимку", b, theme.on_accent);
        unsafe {
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    // «Очистить» asks once more in place, turning red, instead of a system message box.
    let can_clear = !st.cards.is_empty();
    let (hover, press) = (sample(&st.buttons[2].0), sample(&st.buttons[2].1));
    let danger = sample(&st.danger);
    let b = l.clear;
    unsafe {
        ctx.SetTransform(&scaled_about(b, 1.0 - 0.04 * press, 0.0, 0.0));
    }
    let neutral = mix(theme.tint, theme.tint_strong, hover);
    fill(gpu, mix(neutral, rgb(0xd70015, 1.0), danger), b, 9.0 * s);
    let ink = if can_clear { mix(theme.text, theme.on_accent, danger) } else { theme.disabled };
    let label = if st.confirming { "Удалить все?" } else { "Очистить" };
    text(gpu, &ui.fonts.button, label, b, ink);
    unsafe {
        ctx.SetTransform(&Matrix3x2::identity());
    }

    // Cards.
    if st.cards.is_empty() && !st.loading {
        let r = rect(0.0, TOP * s, size.0, size.1 - 20.0 * s);
        text(gpu, &ui.fonts.body, "Здесь появятся снимки, которые вы не сохранили", r, theme.text_muted);
    }
    let radius = 12.0 * s;
    for (i, (card, r)) in st.cards.iter().zip(&l.cards).enumerate() {
        let enter = sample(&card.enter);
        let hover = sample(&card.hover);
        let pressed = st.pressed == Some(Hit::Card(i)) && st.hover == Some(Hit::Card(i));
        let opacity = (enter * 1.6).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }
        let k = 1.0 + 0.03 * hover - if pressed { 0.02 } else { 0.0 };
        let rise = (1.0 - enter) * 14.0 * s - 2.0 * s * hover;
        unsafe {
            ctx.SetTransform(&scaled_about(*r, k, 0.0, rise));
        }
        shadow(gpu, *r, radius, s, 6.0 + 10.0 * hover, 2.0 + 5.0 * hover, (0.12 + 0.10 * hover) * opacity);
        match &card.bitmap {
            // Fill the tile; the preview is already the selected area, so cropping edges is fine.
            Some((bitmap, w, h)) => glass::rounded_image(&ui.device, bitmap, (*w, *h), *r, radius, opacity, true),
            None => fill(gpu, fade(theme.tile, opacity), *r, radius),
        }
        stroke(gpu, fade(theme.hairline, opacity), inflate(*r, -0.5), radius, 1.0);
        unsafe {
            ctx.SetTransform(&scaled_about(*r, 1.0, 0.0, rise));
        }
        let label = rect(r.left + 2.0, r.bottom + 6.0 * s, r.right, r.bottom + 28.0 * s);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        text(
            gpu,
            &ui.fonts.small,
            &stamp(card.created_ms, now_ms),
            label,
            fade(mix(theme.text_muted, theme.text, hover), opacity),
        );
        unsafe {
            ctx.SetTransform(&Matrix3x2::identity());
        }
    }

    // Keyboard focus ring glides between cards.
    if st.focus_visible
        && let Some(ring) = &st.ring
    {
        let [a, b, c, d] = ring.each_ref().map(&mut sample);
        stroke(gpu, theme.accent, rect(a, b, c, d), radius + 4.0 * s, 2.0 * s);
    }
    moving
}

fn focus_ring_target(st: &mut State, width: f32) {
    let l = st.layout(width);
    let Some(r) = l.cards.get(st.focus).map(|r| inflate(*r, 4.0 * st.scale)) else { return };
    let now = st.now();
    let values = [r.left, r.top, r.right, r.bottom];
    match &mut st.ring {
        Some(ring) => {
            for (s, v) in ring.iter_mut().zip(values) {
                s.target(v, now);
            }
        }
        None => st.ring = Some(values.map(|v| Spring::with(v, Curve::GLIDE))),
    }
}

fn point(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let _ = hwnd;
    ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = || unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    let width = || client_size(hwnd).0 as f32;
    match msg {
        // The whole window is client area: no system caption, DWM still draws shadow and corners.
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        WM_NCHITTEST => {
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            let caption = DATA.with(|d| {
                d.try_borrow().ok().as_deref().and_then(Option::as_ref).is_some_and(|st| {
                    let l = st.layout(width());
                    (p.y as f32) < (TOP - 12.0) * st.scale
                        && hit(&l, p.x as f32, p.y as f32, st.cards.len(), !st.cards.is_empty()).is_none()
                })
            });
            return LRESULT(if caption { HTCAPTION } else { HTCLIENT } as isize);
        }
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
        WM_SIZE => {
            redraw();
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(hwnd, lp);
            DATA.with(|d| {
                if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(st) = guard.as_mut() {
                    if !st.tracking {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        unsafe {
                            let _ = TrackMouseEvent(&mut t);
                        }
                        st.tracking = true;
                    }
                    let l = st.layout(width());
                    let h = hit(&l, x, y, st.cards.len(), !st.cards.is_empty());
                    st.set_hover(h);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            DATA.with(|d| {
                if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(st) = guard.as_mut() {
                    st.tracking = false;
                    st.set_hover(None);
                    st.press(None);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let hand = DATA.with(|d| d.try_borrow().ok().as_deref().and_then(Option::as_ref).is_some_and(|st| st.hover.is_some()));
            unsafe {
                SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
            }
            return LRESULT(1);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(hwnd, lp);
            DATA.with(|d| {
                if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(st) = guard.as_mut() {
                    let l = st.layout(width());
                    let h = hit(&l, x, y, st.cards.len(), !st.cards.is_empty());
                    st.press(h);
                    st.focus_visible = false;
                }
            });
            unsafe {
                SetCapture(hwnd);
            }
            redraw();
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let (x, y) = point(hwnd, lp);
            let action = DATA.with(|d| {
                let Ok(mut state) = d.try_borrow_mut() else { return Default::default() };
                let st = state.as_mut()?;
                let l = st.layout(width());
                let h = hit(&l, x, y, st.cards.len(), !st.cards.is_empty());
                let pressed = st.pressed;
                st.press(None);
                let h = h.filter(|h| Some(*h) == pressed)?;
                Some(match h {
                    Hit::Clear if !st.confirming => {
                        st.set_confirm(true);
                        None
                    }
                    Hit::Clear => {
                        st.set_confirm(false);
                        Some(Request::Clear)
                    }
                    Hit::Card(i) => st.cards.get(i).map(|c| Request::Load(c.id.clone())),
                    Hit::Close | Hit::Back => Some(Request::Close),
                })
            });
            match action.flatten() {
                Some(Request::Close) => close(hwnd),
                Some(r) => request(r),
                None => {}
            }
            redraw();
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == TIMER_CONFIRM => {
            DATA.with(|d| {
                if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(st) = guard.as_mut() {
                    st.set_confirm(false);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let key = wp.0 as u16;
            if key == VK_ESCAPE.0 {
                close(hwnd);
                return LRESULT(0);
            }
            let load = DATA.with(|d| {
                let Ok(mut state) = d.try_borrow_mut() else { return Default::default() };
                let st = state.as_mut()?;
                let n = st.cards.len();
                if n == 0 {
                    return None;
                }
                let cols = columns(width(), st.scale);
                let f = st.focus;
                if key == VK_RETURN.0 || key == VK_SPACE.0 {
                    return st.cards.get(f).map(|c| c.id.clone());
                }
                st.focus = match key {
                    k if k == VK_LEFT.0 => f.saturating_sub(1),
                    k if k == VK_RIGHT.0 || k == VK_TAB.0 => (f + 1) % n,
                    k if k == VK_UP.0 => f.saturating_sub(cols),
                    k if k == VK_DOWN.0 => (f + cols).min(n - 1),
                    _ => return None,
                };
                st.focus_visible = true;
                focus_ring_target(st, width());
                None
            });
            if let Some(id) = load {
                request(Request::Load(id));
            }
            redraw();
            return LRESULT(0);
        }
        WM_CLOSE => {
            close(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            DATA.with(|d| {
                if let Ok(mut guard) = d.try_borrow_mut()
                    && let Some(st) = guard.as_mut() {
                    st.scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
                    if let Some(ui) = st.ui.as_mut()
                        && let Ok(fonts) = Fonts::new(&ui.device.shared, st.scale)
                    {
                        ui.fonts = fonts;
                    }
                }
            });
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER);
            }
            request(Request::Refresh);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Offscreen render of the window at rest, for design review (`--design-snapshots`).
pub fn preview(
    entries: Vec<Entry>,
    scale: f32,
    returning: bool,
    loading: bool,
    error: Option<String>,
) -> Result<kadr_doc::Image> {
    let ui = Ui::new(scale)?;
    let mut st = new_state(HWND::default(), HWND::default(), 0, scale);
    st.returning = returning;
    st.loading = loading;
    st.error = error;
    st.cards = entries
        .into_iter()
        .map(|e| Card {
            bitmap: e.preview.as_ref().and_then(|p| {
                kadr_render::annotations::bitmap(&ui.device.gpu, p).ok().map(|b| (b, p.width() as f32, p.height() as f32))
            }),
            id: e.id,
            created_ms: e.created_ms,
            hover: Spring::with(0.0, Curve::HOVER_IN),
            enter: Spring::with(1.0, Curve::POP),
        })
        .collect();
    if let Some(c) = st.cards.get_mut(1) {
        c.hover.snap(1.0);
    }
    let w = (WIDTH * scale).round();
    let h = desired_height(w, scale, st.cards.len().max(if loading { 4 } else { 0 })).round();
    glass::render_offscreen(&ui.device, w as u32, h as u32, || {
        paint(&ui, &st, (w, h), 10.0, true);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_fits_ten_previews_and_buttons_do_not_overlap_at_each_dpi() {
        for s in [1.0f32, 1.5, 2.0] {
            let w = WIDTH * s;
            let l = layout(w, s, 10, true);
            let h = desired_height(w, s, 10);
            assert_eq!(l.cards.len(), 10);
            for c in &l.cards {
                assert!(c.left >= 0.0 && c.right <= w && c.top >= TOP * s - 0.5 && c.bottom + LABEL * s <= h + 0.5);
            }
            let back = l.back.unwrap();
            assert!(l.clear.right <= back.left && back.right <= l.close.left);
            assert_eq!(hit(&l, (back.left + back.right) / 2.0, (back.top + back.bottom) / 2.0, 10, true), Some(Hit::Back));
            let c = l.cards[5];
            assert_eq!(hit(&l, (c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0, 10, true), Some(Hit::Card(5)));
            assert_eq!(hit(&l, 5.0, 5.0, 10, true), None, "empty header area drags the window");
        }
        assert_eq!(count_label(1), "1 снимок");
        assert_eq!(count_label(3), "3 снимка");
        assert_eq!(count_label(10), "10 снимков");
    }
    #[test]
    fn clear_is_not_clickable_without_snapshots() {
        let l = layout(WIDTH, 1.0, 0, false);
        let c = l.clear;
        assert_eq!(hit(&l, (c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0, 0, false), None);
    }
    #[test]
    fn window_is_created_hidden_and_does_not_take_focus() {
        unsafe {
            let before = GetForegroundWindow();
            let instance: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap().into();
            let window = Window::new(instance, HWND::default(), WM_APP + 90, HICON::default(), HICON::default()).unwrap();
            window.set_entries(Vec::new());
            assert!(!IsWindowVisible(window.hwnd).as_bool());
            assert_eq!(GetForegroundWindow(), before);
            let _ = SendMessageW(window.hwnd, WM_KEYDOWN, Some(WPARAM(VK_ESCAPE.0 as usize)), Some(LPARAM(0)));
            assert!(matches!(window.take_request(), Some(Request::Close)));
            // Windows calls back into the window while its own code holds the state (showing,
            // hiding, focusing). Such messages must be answered, not panic the process.
            DATA.with(|d| {
                let _busy = d.borrow_mut();
                for (msg, wp) in [(WM_NCHITTEST, 0), (WM_SETCURSOR, 1), (WM_MOUSEMOVE, 0), (WM_ACTIVATE, 0), (WM_KEYDOWN, 0x28)] {
                    let _ = SendMessageW(window.hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(0)));
                }
            });
            drop(window);
        }
    }
}
