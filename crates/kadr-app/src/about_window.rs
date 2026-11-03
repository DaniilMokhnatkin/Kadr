//! «О программе»: the version and the update check, what Kadr does, and two ways to reach the
//! author. «Нашли ошибку?» turns the window into a short form that goes to his Telegram through
//! the report relay (`KADR_REPORT_URL`); «Поддержать» opens Boosty. One Direct2D surface on Mica,
//! drawn like «Настройки».
use crate::glass::{self, Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, rgb, stroke, text};
use kadr_editor::text_input::TextInput;
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};
use windows_numerics::Vector2;

const WIDTH: f32 = 400.0;
const PAD: f32 = 8.0;
const ROW: f32 = 44.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The send worker is done: its outcome waits in `SENT`.
const WM_SENT: u32 = WM_APP + 1;
const BLINK: usize = 1;
const THANKED: usize = 2;
const BOOSTY: &str = "https://boosty.to/justvoiceacting/donate";
/// Where reports go and the key the relay expects, set when Kadr is built (`.cargo/config.toml`).
const RELAY: Option<&str> = option_env!("KADR_REPORT_URL");
const RELAY_KEY: Option<&str> = option_env!("KADR_REPORT_KEY");
/// What a person may type: enough for a story, short enough for one Telegram message with the log.
const TEXT_LIMIT: usize = 1500;
const CONTACT_LIMIT: usize = 100;
/// The relay takes 3000 characters of text; the log fills what the story leaves.
const SENT_LIMIT: usize = 2900;
const LOG_LINES: usize = 20;

const FEATURES: [&str; 6] = [
    "Снимок и запись экрана",
    "15 инструментов разметки",
    "Скрытие личных данных",
    "Распознавание и перевод",
    "Недавние с закреплением",
    "Горячие клавиши",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum View {
    About,
    Report,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    Text,
    Contact,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Check,
    Report,
    Support,
    Back,
    Field(Field),
    Details,
    Send,
}

struct Layout {
    close: D2D_RECT_F,
    // «О программе».
    icon: D2D_RECT_F,
    name: D2D_RECT_F,
    version: D2D_RECT_F,
    update: D2D_RECT_F,
    check: D2D_RECT_F,
    about: D2D_RECT_F,
    features: [D2D_RECT_F; 6],
    report: D2D_RECT_F,
    support: D2D_RECT_F,
    // «Сообщить об ошибке».
    back: D2D_RECT_F,
    title: D2D_RECT_F,
    text_label: D2D_RECT_F,
    text: D2D_RECT_F,
    contact_label: D2D_RECT_F,
    contact: D2D_RECT_F,
    details: D2D_RECT_F,
    send: D2D_RECT_F,
    status: D2D_RECT_F,
    height: f32,
}

fn layout(view: View, width: f32, s: f32) -> Layout {
    let (left, right) = (PAD * s, width - PAD * s);
    let zero = rect(0.0, 0.0, 0.0, 0.0);
    let mut l = Layout {
        close: rect(width - 44.0 * s, 0.0, width, 44.0 * s),
        icon: zero,
        name: zero,
        version: zero,
        update: zero,
        check: zero,
        about: zero,
        features: [zero; 6],
        report: zero,
        support: zero,
        back: zero,
        title: zero,
        text_label: zero,
        text: zero,
        contact_label: zero,
        contact: zero,
        details: zero,
        send: zero,
        status: zero,
        height: 0.0,
    };
    match view {
        View::About => {
            let mid = width / 2.0;
            l.icon = rect(mid - 32.0 * s, 32.0 * s, mid + 32.0 * s, 96.0 * s);
            l.name = rect(left, 108.0 * s, right, 134.4 * s);
            l.version = rect(left, 138.4 * s, right, 155.2 * s);
            l.update = rect(left, 171.2 * s, right, (171.2 + ROW) * s);
            l.check = rect(l.update.right - 93.3 * s, l.update.top, l.update.right, l.update.bottom);
            l.about = rect(left + 14.0 * s, 231.2 * s, right - 14.0 * s, 270.2 * s);
            let top = 286.2 * s;
            let column = 186.0 * s;
            for (i, r) in l.features.iter_mut().enumerate() {
                let (x, y) = (left + 14.0 * s + (i % 2) as f32 * column, top + (i / 2) as f32 * 26.8 * s);
                *r = rect(x, y, x + 170.0 * s, y + 16.8 * s);
            }
            let y = 380.6 * s;
            l.report = rect(left, y, mid - 4.0 * s, y + 44.0 * s);
            l.support = rect(mid + 4.0 * s, y, right, y + 44.0 * s);
            l.height = l.report.bottom + PAD * s;
        }
        View::Report => {
            l.back = rect(left, 8.0 * s, left + 132.0 * s, 52.0 * s);
            l.close = rect(width - 52.0 * s, 8.0 * s, width - 8.0 * s, 52.0 * s);
            l.title = rect(left + 14.0 * s, 64.0 * s, right - 14.0 * s, 86.1 * s);
            l.text_label = rect(left + 14.0 * s, 98.1 * s, right, 114.9 * s);
            l.text = rect(left, 122.9 * s, right, 226.9 * s);
            l.contact_label = rect(left + 14.0 * s, 239.0 * s, right, 255.8 * s);
            l.contact = rect(left, 263.8 * s, right, 307.8 * s);
            l.details = rect(left, 315.8 * s, right, (315.8 + ROW) * s);
            l.status = rect(left + 14.0 * s, 367.8 * s, right - 14.0 * s, 403.8 * s);
            l.send = rect(left, 411.8 * s, right, 455.8 * s);
            l.height = l.send.bottom + 8.0 * s;
        }
    }
    l
}

fn hit(view: View, l: &Layout, x: f32, y: f32) -> Option<Hit> {
    if inside(l.close, x, y) {
        return Some(Hit::Close);
    }
    let targets: &[(D2D_RECT_F, Hit)] = match view {
        View::About => &[(l.check, Hit::Check), (l.report, Hit::Report), (l.support, Hit::Support)],
        View::Report => &[
            (l.back, Hit::Back),
            (l.text, Hit::Field(Field::Text)),
            (l.contact, Hit::Field(Field::Contact)),
            (l.details, Hit::Details),
            (l.send, Hit::Send),
        ],
    };
    targets.iter().find(|(r, _)| inside(*r, x, y)).map(|(_, h)| *h)
}

struct Fonts {
    name: IDWriteTextFormat,
    title: IDWriteTextFormat,
    label: IDWriteTextFormat,
    hint: IDWriteTextFormat,
    chip: IDWriteTextFormat,
    version: IDWriteTextFormat,
    small: IDWriteTextFormat,
    primary: IDWriteTextFormat,
    body: IDWriteTextFormat,
    field: IDWriteTextFormat,
}

struct State {
    hwnd: HWND,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<Fonts>,
    scale: f32,
    epoch: Instant,
    view: View,
    hover: Option<Hit>,
    glow: Vec<(Hit, Spring)>,
    tracking: bool,
    /// The line beside «Проверить»: what the last check said.
    update: String,
    checking: bool,
    text: String,
    text_edit: TextInput,
    contact: String,
    contact_edit: TextInput,
    focus: Field,
    /// When the caret last moved: it stays lit while typing and blinks at rest.
    typed: f64,
    details: bool,
    knob: Spring,
    sending: bool,
    /// The last send: `Ok` thanks the person, `Err` says what went wrong. Kept with its time.
    sent: Option<Result<(), String>>,
    /// The first half of a character outside the BMP, waiting for its second WM_CHAR.
    surrogate: Option<u16>,
}

impl State {
    fn new(hwnd: HWND, device: Option<Device>, scale: f32) -> Self {
        Self {
            hwnd,
            device,
            surface: None,
            fonts: None,
            scale,
            epoch: Instant::now(),
            view: View::About,
            hover: None,
            glow: Vec::new(),
            tracking: false,
            update: "Обновления проверяются сами раз в день".into(),
            checking: false,
            text: String::new(),
            text_edit: TextInput::default(),
            contact: String::new(),
            contact_edit: TextInput::default(),
            focus: Field::Text,
            typed: 0.0,
            details: true,
            knob: Spring::with(1.0, Curve::POP).precise(0.002),
            sending: false,
            sent: None,
            surrogate: None,
        }
    }
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
    fn layout(&self, width: f32) -> Layout {
        layout(self.view, width, self.scale)
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
        if let Some(old) = self.hover {
            self.glow(old).target_with(0.0, Curve::HOVER_OUT, now);
        }
        if let Some(new) = h {
            self.glow(new).target_with(1.0, Curve::HOVER_IN, now);
        }
        self.hover = h;
    }
    fn can_send(&self) -> bool {
        !self.sending && self.text.trim().chars().count() >= 3
    }
    fn field(&mut self, field: Field) -> (&mut String, &mut TextInput, usize, bool) {
        match field {
            Field::Text => (&mut self.text, &mut self.text_edit, TEXT_LIMIT, false),
            Field::Contact => (&mut self.contact, &mut self.contact_edit, CONTACT_LIMIT, true),
        }
    }
    /// Typing or pasting into the focused field: within its length, one line for the contact.
    fn put(&mut self, insert: &str) {
        let (value, edit, limit, single_line) = self.field(self.focus);
        put(edit, value, insert, limit, single_line);
        self.after_edit();
    }
    fn after_edit(&mut self) {
        self.typed = self.now();
        if matches!(self.sent, Some(Err(_))) {
            self.sent = None;
        }
    }
}

fn put(edit: &mut TextInput, value: &mut String, insert: &str, limit: usize, single_line: bool) {
    let insert = if single_line {
        insert.replace(['\r', '\n', '\t'], " ")
    } else {
        insert.replace("\r\n", "\n").replace('\r', "\n")
    };
    let selected = value[edit.range()].chars().count();
    let room = limit.saturating_sub(value.chars().count() - selected);
    let insert: String = insert.chars().take(room).collect();
    edit.insert(value, &insert);
}

fn utf16_at(value: &str, byte: usize) -> u32 {
    value.get(..byte).unwrap_or("").encode_utf16().count() as u32
}
fn byte_at(value: &str, unit: u32) -> usize {
    let mut n = 0u32;
    for (i, c) in value.char_indices() {
        if n >= unit {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    value.len()
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}
/// The send worker's outcome, picked up on WM_SENT.
static SENT: Mutex<Option<Result<(), String>>> = Mutex::new(None);
/// «Проверить» was pressed here: the answer belongs to this window, not to a bubble.
static ASKED: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// What an update check found, from the app. Returns true when «Проверить» here asked for it:
/// then the window says it and the app needs no bubble (unless there is something to install).
pub fn update_result(line: String) -> bool {
    let asked = ASKED.swap(false, Ordering::SeqCst);
    let hwnd = with(|st| {
        st.update = line;
        st.checking = false;
        st.hwnd
    });
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
    asked
}

/// The appearance changed: new window material and ink.
pub fn restyle() {
    if let Some(hwnd) = with(|st| st.hwnd) {
        glass::match_appearance(hwnd);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}
/// Esc is a global capture hotkey while the editor is open. Its owner forwards it here.
pub fn escape() -> bool {
    let action = with(|st| unsafe { IsWindowVisible(st.hwnd) }.as_bool().then_some((st.hwnd, st.view))).flatten();
    if let Some((hwnd, view)) = action {
        if view == View::Report {
            switch(hwnd, View::About);
        } else {
            hide(hwnd);
        }
        return true;
    }
    false
}

pub fn show(instance: HINSTANCE) {
    if STATE.with(|s| s.borrow().is_none()) {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrAbout"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hIcon: unsafe { LoadIconW(Some(instance), PCWSTR(1 as _)) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrAbout"),
                w!("О программе — Kadr"),
                WS_OVERLAPPED | WS_CAPTION,
                0,
                0,
                440,
                460,
                None,
                None,
                Some(instance),
                None,
            )
        };
        let Ok(hwnd) = hwnd else { return };
        unsafe {
            let _ = DwmExtendFrameIntoClientArea(
                hwnd,
                &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 },
            );
            let backdrop = DWMSBT_MAINWINDOW;
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
        }
        STATE.with(|s| *s.borrow_mut() = Some(State::new(hwnd, None, 1.0)));
    }
    // Positioning and showing call back into `proc`, which takes the state itself.
    let placed = with(|st| {
        st.view = View::About;
        st.set_hover(None);
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let monitor = unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetMonitorInfoW(monitor, &mut mi);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        st.scale = dx as f32 / 96.0;
        st.fonts = None;
        let w = (WIDTH * st.scale) as i32;
        let h = layout(View::About, w as f32, st.scale).height as i32;
        let work = mi.rcWork;
        (st.hwnd, work.left + (work.right - work.left - w) / 2, work.top + (work.bottom - work.top - h) / 2, w, h)
    });
    let Some((hwnd, x, y, w, h)) = placed else { return };
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SET_WINDOW_POS_FLAGS(0));
        glass::match_appearance(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Switches between «О программе» and the report form; the window takes the new view's height,
/// keeping its top edge where it is.
fn switch(hwnd: HWND, view: View) {
    let Some(height) = with(|st| {
        st.view = view;
        st.set_hover(None);
        st.surrogate = None;
        if view == View::Report {
            st.focus = Field::Text;
            st.typed = st.now();
        }
        layout(view, client_size(hwnd).0 as f32, st.scale).height
    }) else {
        return;
    };
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
        let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, height as i32, SWP_NOZORDER);
        if view == View::Report {
            SetTimer(Some(hwnd), BLINK, 530, None);
        } else {
            let _ = KillTimer(Some(hwnd), BLINK);
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn hide(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), BLINK);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

fn ensure_fonts(st: &mut State) -> bool {
    let Some(device) = st.device.as_ref() else { return false };
    if st.fonts.is_some() {
        return true;
    }
    let shared = &device.shared;
    let s = st.scale;
    let make = |size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT, wrap: bool| unsafe {
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
        let _ = f.SetTextAlignment(align);
        if wrap {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        } else {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let sign = shared.dwrite.CreateEllipsisTrimmingSign(&f).ok();
            let trimming =
                DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let _ = f.SetTrimming(&trimming, sign.as_ref());
        }
        Some(f)
    };
    let (lead, center) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
    let fonts = (|| {
        Some(Fonts {
            name: make(22.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, center, false)?,
            title: make(17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, lead, false)?,
            label: make(13.0, DWRITE_FONT_WEIGHT_REGULAR, lead, false)?,
            hint: make(12.0, DWRITE_FONT_WEIGHT_REGULAR, lead, false)?,
            chip: make(13.0, DWRITE_FONT_WEIGHT_REGULAR, center, false)?,
            version: make(12.0, DWRITE_FONT_WEIGHT_REGULAR, center, false)?,
            small: make(11.0, DWRITE_FONT_WEIGHT_REGULAR, lead, false)?,
            primary: make(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, center, false)?,
            body: make(13.0, DWRITE_FONT_WEIGHT_REGULAR, lead, true)?,
            field: make(13.0, DWRITE_FONT_WEIGHT_REGULAR, lead, true)?,
        })
    })();
    st.fonts = fonts;
    st.fonts.is_some()
}

/// A field's text laid out inside its box, and how far it is scrolled so the caret stays in view.
struct FieldText {
    layout: IDWriteTextLayout,
    origin: Vector2,
}

fn field_text(st: &State, l: &Layout, field: Field) -> Option<FieldText> {
    let device = st.device.as_ref()?;
    let fonts = st.fonts.as_ref()?;
    let s = st.scale;
    let (r, value, edit) = match field {
        Field::Text => (l.text, &st.text, &st.text_edit),
        Field::Contact => (l.contact, &st.contact, &st.contact_edit),
    };
    let inner = rect(r.left + 12.0 * s, r.top + 10.0 * s, r.right - 12.0 * s, r.bottom - 10.0 * s);
    let (w, h) = (inner.right - inner.left, inner.bottom - inner.top);
    let utf16: Vec<u16> = value.encode_utf16().collect();
    let layout = unsafe {
        let layout = device
            .shared
            .dwrite
            .CreateTextLayout(&utf16, &fonts.field, if field == Field::Text { w } else { 100_000.0 }, h)
            .ok()?;
        if field == Field::Contact {
            let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        layout
    };
    let (mut x, mut y, mut m) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
    unsafe {
        let _ = layout.HitTestTextPosition(utf16_at(value, edit.cursor), false, &mut x, &mut y, &mut m);
    }
    let line = m.height.max(18.0 * s);
    let dx = (x + 2.0 * s - w).max(0.0);
    let dy = (y + line - h).max(0.0);
    // The contact is one line: it sits in the middle of its box.
    let top = if field == Field::Contact { r.top + ((r.bottom - r.top) - line) / 2.0 } else { inner.top };
    Some(FieldText { layout, origin: Vector2 { X: inner.left - dx, Y: top - dy } })
}

/// Where a click lands in a field's text, as a byte offset.
fn caret_at(st: &State, l: &Layout, field: Field, x: f32, y: f32) -> Option<usize> {
    let t = field_text(st, l, field)?;
    let (mut trailing, mut within, mut m) = (BOOL(0), BOOL(0), DWRITE_HIT_TEST_METRICS::default());
    unsafe {
        t.layout.HitTestPoint(x - t.origin.X, y - t.origin.Y, &mut trailing, &mut within, &mut m).ok()?;
    }
    let value = if field == Field::Text { &st.text } else { &st.contact };
    let unit = m.textPosition + if trailing.as_bool() { m.length } else { 0 };
    Some(byte_at(value, unit))
}

fn paint_window(hwnd: HWND) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        if st.device.is_none() {
            st.device = Device::new("about").ok();
        }
        let Some(device) = st.device.as_ref() else { return };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() || !ensure_fonts(st) {
            return;
        }
        let st = &*st;
        let device = st.device.as_ref().unwrap();
        let size = client_size(hwnd);
        if glass::frame(device, st.surface.as_ref().unwrap(), || paint(device, st, size)) {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    });
}

fn paint(device: &Device, st: &State, size: (u32, u32)) -> bool {
    let gpu = &device.gpu;
    let f = st.fonts.as_ref().unwrap();
    let mut theme = Theme::current();
    if theme.shadow.a > 0.0 {
        theme.text_muted = rgb(if theme.dark { 0xa1a1a6 } else { 0x636366 }, 1.0);
        theme.track = rgb(if theme.dark { 0x48484b } else { 0xd6d6db }, 1.0);
    }
    let now = st.now();
    let s = st.scale;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let l = st.layout(w);
    let mut moving = st.glow.iter().any(|(_, sp)| sp.active(now)) || st.knob.active(now);
    let glow = |hit: Hit| st.glow.iter().find(|(k, _)| *k == hit).map_or(0.0, |(_, sp)| sp.value(now));
    let link = |hit: Hit| mix(theme.accent, theme.on_selected, glow(hit));
    let group = |r: D2D_RECT_F| {
        fill(gpu, theme.control, r, 22.0 * s);
    };
    let button = |r: D2D_RECT_F, hit: Hit, label: &str, enabled: bool| {
        let color = if enabled { theme.text } else { theme.text_muted };
        if hit != Hit::Check || glow(hit) > 0.001 {
            fill(gpu, fade(theme.control, if hit == Hit::Check { glow(hit) } else { 1.0 }), r, 22.0 * s);
        }
        let name = match hit {
            Hit::Report => Some("bug"),
            Hit::Support => Some("heart"),
            _ => None,
        };
        if let Some(name) = name {
            let units: Vec<u16> = label.encode_utf16().collect();
            if let Ok(t) = unsafe { device.shared.dwrite.CreateTextLayout(&units, &f.chip, 10000.0, 44.0 * s) } {
                let mut m = DWRITE_TEXT_METRICS::default();
                if unsafe { t.GetMetrics(&mut m) }.is_ok() {
                    let left = (r.left + r.right - m.width - 26.0 * s) / 2.0;
                    kadr_render::scene::concept_icon(
                        gpu,
                        name,
                        rect(left, r.top, left + 20.0 * s, r.bottom),
                        color,
                        20.0 * s,
                    );
                    text(gpu, &f.chip, label, rect(left + 26.0 * s, r.top, left + 26.0 * s + m.width, r.bottom), color);
                }
            }
        } else {
            text(gpu, &f.chip, label, r, color);
        }
    };
    fill(gpu, theme.surface, rect(0.0, 0.0, w, h), 0.0);

    // Close, as in «Настройки».
    let c = l.close;
    let hover = glow(Hit::Close);
    fill(gpu, fade(theme.tint, hover), inflate(c, -2.0 * s), 14.0 * s);
    let line = |a: (f32, f32), b: (f32, f32), width: f32| unsafe {
        gpu.d2d.DrawLine(Vector2 { X: a.0, Y: a.1 }, Vector2 { X: b.0, Y: b.1 }, &gpu.brush, width, None);
    };
    unsafe {
        gpu.brush.SetColor(&mix(theme.text_muted, theme.text, hover));
    }
    kadr_render::scene::concept_icon(gpu, "close", c, mix(theme.text_muted, theme.text, hover), 20.0 * s);

    match st.view {
        View::About => {
            // The source's blue app mark and the same SVG outlines as the editor.
            let i = l.icon;
            fill(gpu, theme.accent, i, 16.0 * s);
            unsafe {
                gpu.brush.SetColor(&if theme.dark { rgb(0x1c1c1e, 1.0) } else { rgb(0xffffff, 1.0) });
            }
            kadr_render::scene::concept_icon(gpu, "mark", i, theme.on_accent, 36.0 * s);
            text(gpu, &f.name, "Kadr", l.name, theme.text);
            text(gpu, &f.version, &format!("Версия {}", env!("CARGO_PKG_VERSION")), l.version, theme.text_muted);

            group(l.update);
            let status = rect(l.update.left + 40.0 * s, l.update.top, l.check.left - 8.0 * s, l.update.bottom);
            kadr_render::scene::concept_icon(
                gpu,
                if st.checking { "spinner" } else { "check-circle" },
                rect(l.update.left + 14.0 * s, l.update.top, l.update.left + 32.0 * s, l.update.bottom),
                theme.text_muted,
                18.0 * s,
            );
            let ink = if st.checking { theme.text_muted } else { theme.text };
            text(gpu, &f.label, if st.checking { "Проверяю…" } else { st.update.as_str() }, status, ink);
            button(l.check, Hit::Check, "Проверить", !st.checking);

            text(
                gpu,
                &f.body,
                "Kadr снимает и записывает экран, размечает снимки и сам скрывает личные данные, прежде чем вы ими поделитесь.",
                l.about,
                theme.text,
            );
            for ((r, feature), icon) in
                l.features.iter().zip(FEATURES).zip(["record", "pen", "shield", "translate", "history", "keyboard"])
            {
                kadr_render::scene::concept_icon(
                    gpu,
                    icon,
                    rect(r.left, r.top, r.left + 14.0 * s, r.bottom),
                    theme.text_muted,
                    14.0 * s,
                );
                text(gpu, &f.hint, feature, rect(r.left + 22.0 * s, r.top, r.right, r.bottom), theme.text);
            }
            button(l.report, Hit::Report, "Нашли ошибку?", true);
            button(l.support, Hit::Support, "Поддержать", true);
        }
        View::Report => {
            text(
                gpu,
                &f.label,
                "О программе",
                rect(l.back.left + 30.0 * s, l.back.top, l.back.right, l.back.bottom),
                link(Hit::Back),
            );
            kadr_render::scene::concept_icon(
                gpu,
                "back",
                rect(l.back.left + 8.0 * s, l.back.top, l.back.left + 28.0 * s, l.back.bottom),
                link(Hit::Back),
                20.0 * s,
            );
            text(gpu, &f.title, "Сообщить об ошибке", l.title, theme.text);
            text(gpu, &f.hint, "Что случилось?", l.text_label, theme.text_muted);
            text(gpu, &f.hint, "Как вам ответить · необязательно", l.contact_label, theme.text_muted);
            let blink = now - st.typed < 0.6 || ((now - st.typed) / 0.53) as i64 % 2 == 0;
            for (field, r, value, edit, placeholder) in [
                (Field::Text, l.text, &st.text, &st.text_edit, "Например: после записи не открылись «Недавние»"),
                (Field::Contact, l.contact, &st.contact, &st.contact_edit, "@ник в Telegram или почта"),
            ] {
                let focused = st.focus == field;
                fill(gpu, theme.control, r, 14.0 * s);
                let edge = if focused {
                    theme.accent
                } else {
                    mix(theme.hairline, theme.text_muted, 0.5 * glow(Hit::Field(field)))
                };
                if focused {
                    stroke(gpu, edge, inflate(r, -0.5), 14.0 * s, 2.0 * s);
                }
                let inner = inflate(r, -1.5 * s);
                unsafe {
                    gpu.d2d.PushAxisAlignedClip(&inner, D2D1_ANTIALIAS_MODE_ALIASED);
                }
                if let Some(t) = field_text(st, &l, field) {
                    if value.is_empty() {
                        let at = rect(t.origin.X, t.origin.Y, inner.right - 12.0 * s, t.origin.Y + 22.0 * s);
                        let format = if field == Field::Text { &f.body } else { &f.label };
                        text(gpu, format, placeholder, at, theme.text_muted);
                    } else {
                        let range = edit.range();
                        if focused && !range.is_empty() {
                            let (start, end) = (utf16_at(value, range.start), utf16_at(value, range.end));
                            let mut hits = vec![DWRITE_HIT_TEST_METRICS::default(); 64];
                            let mut count = 0;
                            unsafe {
                                let _ = t.layout.HitTestTextRange(
                                    start,
                                    end - start,
                                    t.origin.X,
                                    t.origin.Y,
                                    Some(&mut hits),
                                    &mut count,
                                );
                            }
                            for m in hits.iter().take(count as usize) {
                                fill(
                                    gpu,
                                    fade(theme.accent, 0.25),
                                    rect(m.left, m.top, m.left + m.width, m.top + m.height),
                                    2.0 * s,
                                );
                            }
                        }
                        unsafe {
                            gpu.brush.SetColor(&theme.text);
                            gpu.d2d.DrawTextLayout(t.origin, &t.layout, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        }
                    }
                    if focused && blink && !st.sending {
                        let (mut x, mut y, mut m) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
                        unsafe {
                            let _ = t.layout.HitTestTextPosition(
                                utf16_at(value, edit.cursor),
                                false,
                                &mut x,
                                &mut y,
                                &mut m,
                            );
                            gpu.brush.SetColor(&theme.text);
                        }
                        let (x, y) = (t.origin.X + x, t.origin.Y + y);
                        line((x, y), (x, y + m.height.max(18.0 * s)), 1.5 * s);
                    }
                }
                unsafe {
                    gpu.d2d.PopAxisAlignedClip();
                }
            }

            // «Версия, система и журнал»: a switch, as in «Настройки».
            let r = l.details;
            fill(gpu, fade(theme.tint, 0.6 * glow(Hit::Details)), inflate(r, -3.0 * s), 9.0 * s);
            let top = rect(r.left + 16.0 * s, r.top + 4.0 * s, r.right - 80.0 * s, r.top + ROW * s * 0.55);
            let bottom = rect(r.left + 16.0 * s, r.top + ROW * s * 0.5, r.right - 80.0 * s, r.bottom - 4.0 * s);
            text(gpu, &f.label, "Версия, система и журнал", top, theme.text);
            text(gpu, &f.small, "Без снимков и текста с экрана", bottom, theme.text_muted);
            let on = st.knob.value(now);
            let track = rect(
                r.right - 52.0 * s,
                r.top + (ROW - 24.0) / 2.0 * s,
                r.right - 12.0 * s,
                r.top + (ROW + 24.0) / 2.0 * s,
            );
            fill(gpu, mix(theme.track, theme.accent, on.clamp(0.0, 1.0)), track, 12.0 * s);
            let d = 20.0 * s;
            let x = track.left + 2.0 * s + (track.right - track.left - d - 4.0 * s) * on;
            let knob = rect(x, track.top + 2.0 * s, x + d, track.top + 2.0 * s + d);
            glass::shadow(gpu, knob, d / 2.0, s, 2.0, 1.0, 0.18);
            fill(gpu, rgb(0xffffff, 1.0), knob, d / 2.0);

            // «Отправить»: the window's one filled button.
            let ready = st.can_send();
            let hover = glow(Hit::Send);
            let (bg, ink) = if ready {
                (mix(theme.accent, theme.on_selected, 0.12 * hover), theme.on_accent)
            } else {
                (fade(theme.accent, 0.38), fade(theme.on_accent, 0.38))
            };
            fill(gpu, bg, l.send, 22.0 * s);
            text(gpu, &f.primary, if st.sending { "Отправляю…" } else { "Отправить" }, l.send, ink);
            match &st.sent {
                Some(Ok(())) => text(gpu, &f.hint, "Спасибо! Отчёт ушёл автору.", l.status, theme.text),
                Some(Err(e)) => text(gpu, &f.hint, e, l.status, glass::error_ink(&theme)),
                None if !st.sending => text(gpu, &f.hint, "Ctrl + Enter — отправить", l.status, theme.text_muted),
                None => {}
            }
            moving |= st.sending;
        }
    }
    moving
}

/// What goes to the relay: the person's words, how to answer, and (when allowed) where it ran.
struct Outgoing {
    text: String,
    contact: String,
    details: Option<Details>,
}
struct Details {
    version: String,
    os: String,
    screen: String,
    log: Vec<String>,
}

fn details() -> Details {
    let metric = |m| unsafe { GetSystemMetrics(m) };
    Details {
        version: env!("CARGO_PKG_VERSION").into(),
        os: crate::report::windows(),
        screen: format!("{} × {}, мониторов: {}", metric(SM_CXSCREEN), metric(SM_CYSCREEN), metric(SM_CMONITORS)),
        log: kadr_win::log::tail(LOG_LINES),
    }
}

/// Form fields for the relay. The log rides at the end of the text: the newest lines that fit.
fn fields(o: &Outgoing) -> Vec<(&'static str, String)> {
    let mut text = o.text.trim().to_string();
    let mut out = Vec::new();
    if let Some(d) = &o.details {
        const HEADER: &str = "\n\nЖурнал:\n";
        let room = SENT_LIMIT.saturating_sub(text.chars().count() + HEADER.chars().count());
        let mut tail: Vec<String> = Vec::new();
        let mut used = 0;
        for line in d.log.iter().rev() {
            let line: String = line.chars().take(200).collect();
            let n = line.chars().count() + 1;
            if used + n > room {
                break;
            }
            used += n;
            tail.push(line);
        }
        if !tail.is_empty() {
            tail.reverse();
            text.push_str(HEADER);
            text.push_str(&tail.join("\n"));
        }
        out.push(("version", d.version.clone()));
        out.push(("os", d.os.clone()));
        out.push(("screen", d.screen.clone()));
    }
    out.insert(0, ("text", text));
    out.push(("contact", o.contact.trim().to_string()));
    out.retain(|(_, v)| !v.is_empty());
    out
}

fn multipart(fields: &[(&str, String)], boundary: &str) -> Vec<u8> {
    let mut out = String::new();
    for (name, value) in fields {
        out.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{value}\r\n"
        ));
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    out.into_bytes()
}

/// The relay's answer, in words for the person who sent the report.
fn outcome(status: u32) -> Result<(), String> {
    match status {
        200 => Ok(()),
        429 => Err("Слишком часто. Попробуйте через минуту.".into()),
        400 | 413 => Err("Отчёт не принят. Сократите текст и попробуйте ещё раз.".into()),
        403 => Err("Эта сборка Kadr не может отправлять отчёты.".into()),
        502..=504 => Err("Telegram сейчас недоступен. Попробуйте позже.".into()),
        s => Err(format!("Сервер отчётов ответил кодом {s}.")),
    }
}

fn deliver(o: &Outgoing) -> Result<(), String> {
    let url = RELAY.ok_or("В этой сборке Kadr не указан адрес для отчётов.")?;
    let (host, path) = crate::http::split(url).ok_or("Странный адрес для отчётов.")?;
    let boundary = format!(
        "kadr-{:x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    );
    let headers = format!(
        "Content-Type: multipart/form-data; boundary={boundary}\r\nX-Kadr-Key: {}\r\n",
        RELAY_KEY.unwrap_or("")
    );
    let body = multipart(&fields(o), &boundary);
    let (status, _) = crate::http::request("POST", host, path, &headers, &body, 4096, "сервером отчётов")?;
    outcome(status)
}

/// Sends on a worker; the window hears back through WM_SENT.
fn send(st: &mut State) {
    if !st.can_send() {
        return;
    }
    st.sending = true;
    st.sent = None;
    let outgoing = Outgoing { text: st.text.clone(), contact: st.contact.clone(), details: st.details.then(details) };
    let host = st.hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = deliver(&outgoing);
        match &result {
            Ok(()) => kadr_win::log_info!("отчёт об ошибке отправлен"),
            Err(e) => kadr_win::log_warn!("отчёт об ошибке: {e}"),
        }
        if let Ok(mut slot) = SENT.lock() {
            *slot = Some(result);
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(host as *mut _)), WM_SENT, WPARAM(0), LPARAM(0));
        }
    });
}

fn ctrl() -> bool {
    unsafe { GetKeyState(i32::from(VK_CONTROL.0)) < 0 }
}
fn shift() -> bool {
    unsafe { GetKeyState(i32::from(VK_SHIFT.0)) < 0 }
}

/// Clicks and Enter on a target. Returns the view to switch to, and whether to hide.
fn press(st: &mut State, h: Hit) -> (Option<View>, bool) {
    match h {
        Hit::Close => return (None, true),
        Hit::Check if !st.checking => {
            st.checking = true;
            ASKED.store(true, Ordering::SeqCst);
            crate::update::check_now();
        }
        Hit::Report => return (Some(View::Report), false),
        Hit::Back => return (Some(View::About), false),
        Hit::Support => {
            let url = kadr_win::wide(BOOSTY);
            unsafe {
                ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        Hit::Field(field) => {
            st.focus = field;
            st.typed = st.now();
        }
        Hit::Details => {
            st.details = !st.details;
            let now = st.now();
            st.knob.target_with(f32::from(st.details), Curve::POP, now);
        }
        Hit::Send => send(st),
        Hit::Check => {}
    }
    (None, false)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = || unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    let point = || ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    let width = || client_size(hwnd).0 as f32;
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
        WM_NCHITTEST => {
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            // The top band drags the window, except where a button sits.
            let caption = with(|st| {
                (p.y as f32) < 52.0 * st.scale && hit(st.view, &st.layout(width()), p.x as f32, p.y as f32).is_none()
            });
            return LRESULT(if caption == Some(true) { HTCAPTION as isize } else { HTCLIENT as isize });
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            with(|st| {
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
                let h = hit(st.view, &st.layout(width()), x, y);
                st.set_hover(h);
            });
            redraw();
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|st| {
                st.tracking = false;
                st.set_hover(None);
            });
            redraw();
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let cursor = with(|st| match st.hover {
                Some(Hit::Field(_)) => IDC_IBEAM,
                Some(_) => IDC_HAND,
                None => IDC_ARROW,
            })
            .unwrap_or(IDC_ARROW);
            unsafe {
                SetCursor(LoadCursorW(None, cursor).ok());
            }
            return LRESULT(1);
        }
        WM_LBUTTONDOWN => {
            // A click in a field puts the caret there; Shift extends the selection.
            let (x, y) = point();
            with(|st| {
                let l = st.layout(width());
                if let Some(Hit::Field(field)) = hit(st.view, &l, x, y) {
                    st.focus = field;
                    if let Some(at) = caret_at(st, &l, field, x, y) {
                        let extend = shift();
                        let (_, edit, _, _) = st.field(field);
                        edit.cursor = at;
                        if !extend {
                            edit.anchor = at;
                        }
                    }
                    st.typed = st.now();
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            let action = with(|st| match hit(st.view, &st.layout(width()), x, y) {
                Some(Hit::Field(_)) | None => (None, false),
                Some(h) => press(st, h),
            });
            // Resizing and hiding call back into `proc`: no borrow is held across them.
            match action {
                Some((Some(view), _)) => switch(hwnd, view),
                Some((None, true)) => hide(hwnd),
                _ => redraw(),
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let vk = wp.0 as u16;
            let mut action = (None, false);
            let handled = with(|st| {
                if vk == VK_ESCAPE.0 {
                    action = if st.view == View::Report { (Some(View::About), false) } else { (None, true) };
                    return true;
                }
                if st.view == View::About {
                    if vk == VK_RETURN.0 {
                        action = press(st, Hit::Report);
                        return true;
                    }
                    return false;
                }
                if st.sending {
                    return true;
                }
                let focus = st.focus;
                match vk {
                    k if k == VK_TAB.0 => {
                        st.focus = if focus == Field::Text { Field::Contact } else { Field::Text };
                        st.typed = st.now();
                    }
                    k if k == VK_RETURN.0 && (ctrl() || focus == Field::Contact) => send(st),
                    k if k == VK_RETURN.0 => st.put("\n"),
                    k if k == VK_BACK.0 || k == VK_DELETE.0 => {
                        let (value, edit, _, _) = st.field(focus);
                        edit.erase(value, k == VK_BACK.0);
                        st.after_edit();
                    }
                    k if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0, VK_HOME.0, VK_END.0].contains(&k) => {
                        let extend = shift();
                        let (value, edit, _, _) = st.field(focus);
                        edit.navigate(value, k, extend);
                        st.typed = st.now();
                    }
                    0x41 if ctrl() => {
                        let (value, edit, _, _) = st.field(focus);
                        edit.select_all(value);
                    }
                    0x43 | 0x58 if ctrl() => {
                        let (value, edit, _, _) = st.field(focus);
                        let chosen = value[edit.range()].to_string();
                        if !chosen.is_empty() {
                            if let Err(e) = kadr_export::clipboard::put_text(hwnd, &chosen) {
                                kadr_win::log_warn!("отчёт: копирование: {e}");
                            }
                            if vk == 0x58 {
                                edit.insert(value, "");
                                st.after_edit();
                            }
                        }
                    }
                    0x56 if ctrl() => {
                        if let Ok(pasted) = kadr_export::clipboard::text(hwnd) {
                            st.put(&pasted);
                        }
                    }
                    _ => return false,
                }
                true
            });
            match action {
                (Some(view), _) => switch(hwnd, view),
                (None, true) => hide(hwnd),
                _ => {}
            }
            if handled == Some(true) {
                redraw();
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            let unit = wp.0 as u16;
            if ctrl() {
                return LRESULT(0);
            }
            with(|st| {
                if st.view != View::Report || st.sending {
                    return;
                }
                let c = match unit {
                    0xD800..=0xDBFF => {
                        st.surrogate = Some(unit);
                        return;
                    }
                    0xDC00..=0xDFFF => {
                        st.surrogate.take().and_then(|high| char::decode_utf16([high, unit]).next()?.ok())
                    }
                    _ => char::from_u32(u32::from(unit)),
                };
                if let Some(c) = c.filter(|c| !c.is_control()) {
                    st.put(c.encode_utf8(&mut [0; 4]));
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_TIMER => {
            let thanked = wp.0 == THANKED;
            if thanked {
                unsafe {
                    let _ = KillTimer(Some(hwnd), THANKED);
                }
                if with(|st| st.view == View::Report && matches!(st.sent, Some(Ok(())))) == Some(true) {
                    with(|st| st.sent = None);
                    switch(hwnd, View::About);
                }
            }
            redraw();
            return LRESULT(0);
        }
        WM_SENT => {
            let result = SENT.lock().ok().and_then(|mut slot| slot.take());
            let thanked = with(|st| {
                st.sending = false;
                let ok = matches!(result, Some(Ok(())));
                if ok {
                    st.text.clear();
                    st.text_edit = TextInput::default();
                    st.contact.clear();
                    st.contact_edit = TextInput::default();
                }
                st.sent = result;
                ok
            });
            if thanked == Some(true) {
                unsafe {
                    SetTimer(Some(hwnd), THANKED, 1800, None);
                }
            }
            redraw();
            return LRESULT(0);
        }
        WM_CLOSE => {
            hide(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            with(|st| {
                st.scale = (wp.0 & 0xffff) as f32 / 96.0;
                st.fonts = None;
            });
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Offscreen render for design review: «О программе», or the report form half filled in.
pub fn preview(scale: f32, report: bool) -> windows::core::Result<kadr_doc::Image> {
    let mut st = State::new(HWND::default(), Device::new("about-preview").ok(), scale);
    if report {
        st.view = View::Report;
        st.text = "После записи экрана не открылись «Недавние».\nЖду пару секунд, ничего не меняется.".into();
        st.text_edit = TextInput::at_end(&st.text);
        st.contact = "@r4pto".into();
        st.contact_edit = TextInput::at_end(&st.contact);
    } else {
        st.update = "Установлена последняя версия".into();
    }
    if !ensure_fonts(&mut st) {
        return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
    }
    let w = (WIDTH * scale).round();
    let h = st.layout(w).height.round();
    let device = st.device.as_ref().unwrap();
    glass::render_offscreen(device, w as u32, h as u32, || {
        fill(
            &device.gpu,
            if Theme::current().dark { rgb(0x202022, 1.0) } else { rgb(0xeef0f4, 1.0) },
            rect(0.0, 0.0, w, h),
            0.0,
        );
        paint(device, &st, (w as u32, h as u32));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_is_reachable_and_inside_the_window_at_each_dpi() {
        for s in [1.0f32, 1.5, 2.0] {
            let w = WIDTH * s;
            for (view, targets) in [
                (View::About, vec![Hit::Close, Hit::Check, Hit::Report, Hit::Support]),
                (
                    View::Report,
                    vec![
                        Hit::Close,
                        Hit::Back,
                        Hit::Field(Field::Text),
                        Hit::Field(Field::Contact),
                        Hit::Details,
                        Hit::Send,
                    ],
                ),
            ] {
                let l = layout(view, w, s);
                for target in targets {
                    let r = match target {
                        Hit::Close => l.close,
                        Hit::Check => l.check,
                        Hit::Report => l.report,
                        Hit::Support => l.support,
                        Hit::Back => l.back,
                        Hit::Field(Field::Text) => l.text,
                        Hit::Field(Field::Contact) => l.contact,
                        Hit::Details => l.details,
                        Hit::Send => l.send,
                    };
                    assert!(r.right <= w && r.bottom <= l.height, "{target:?} fits at {s}");
                    assert!(r.bottom - r.top >= 32.0 * s, "{target:?} is easy to hit at {s}");
                    let (x, y) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                    assert_eq!(hit(view, &l, x, y), Some(target), "{target:?} at {s}");
                }
            }
        }
    }

    #[test]
    fn fields_keep_their_length_and_the_contact_stays_one_line() {
        let (mut value, mut edit) = (String::new(), TextInput::default());
        put(&mut edit, &mut value, &"я".repeat(TEXT_LIMIT + 50), TEXT_LIMIT, false);
        assert_eq!(value.chars().count(), TEXT_LIMIT);
        put(&mut edit, &mut value, "ещё", TEXT_LIMIT, false);
        assert_eq!(value.chars().count(), TEXT_LIMIT);
        let (mut contact, mut edit) = (String::new(), TextInput::default());
        put(&mut edit, &mut contact, "@kadr\r\nfan", CONTACT_LIMIT, true);
        assert_eq!(contact, "@kadr  fan");
        let (mut text, mut edit) = (String::new(), TextInput::default());
        put(&mut edit, &mut text, "раз\r\nдва\rтри", TEXT_LIMIT, false);
        assert_eq!(text, "раз\nдва\nтри");
    }

    #[test]
    fn utf16_positions_round_trip_through_byte_offsets() {
        let value = "Kadr 🌍 снимок";
        for (byte, _) in value.char_indices() {
            assert_eq!(byte_at(value, utf16_at(value, byte)), byte);
        }
        assert_eq!(byte_at(value, 999), value.len());
    }

    #[test]
    fn the_log_rides_after_the_story_and_the_whole_text_fits_the_relay() {
        let story = "Недавние не открылись. ".repeat(60);
        let o = Outgoing {
            text: format!("  {story}  "),
            contact: " @r4pto ".into(),
            details: Some(Details {
                version: "1.0.8".into(),
                os: "Windows 11 Pro 24H2 (сборка 26100.1)".into(),
                screen: "2560 × 1440, мониторов: 2".into(),
                log: (0..LOG_LINES).map(|i| format!("12:{i:02} строка журнала {}", "x".repeat(150))).collect(),
            }),
        };
        let f = fields(&o);
        let get = |name: &str| f.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str());
        let text = get("text").unwrap();
        assert!(text.starts_with("Недавние не открылись."));
        assert!(text.contains("\n\nЖурнал:\n"));
        assert!(text.ends_with(&format!("строка журнала {}", "x".repeat(150))), "the newest line comes last");
        assert!(text.chars().count() <= SENT_LIMIT, "{}", text.chars().count());
        assert_eq!(get("contact"), Some("@r4pto"));
        assert_eq!(get("version"), Some("1.0.8"));

        let private = Outgoing { text: "Не работает".into(), contact: String::new(), details: None };
        assert_eq!(fields(&private), vec![("text", "Не работает".to_string())]);
    }

    #[test]
    fn the_form_body_is_multipart_with_one_part_per_field() {
        let body = String::from_utf8(multipart(&[("text", "Привет".into()), ("os", "Windows".into())], "b0")).unwrap();
        assert!(body.starts_with("--b0\r\nContent-Disposition: form-data; name=\"text\"\r\n"));
        assert!(body.contains("\r\n\r\nПривет\r\n--b0\r\nContent-Disposition: form-data; name=\"os\""));
        assert!(body.ends_with("\r\n--b0--\r\n"));
    }

    #[test]
    fn relay_answers_become_plain_words() {
        assert_eq!(outcome(200), Ok(()));
        assert!(outcome(429).unwrap_err().contains("минуту"));
        assert!(outcome(502).unwrap_err().contains("Telegram"));
        assert!(outcome(418).unwrap_err().contains("418"));
    }

    #[test]
    #[ignore = "network: sends one real report to the author's Telegram"]
    fn live_report_reaches_the_relay() {
        let o = Outgoing {
            text: "Проверка: отчёт из окна «О программе» собран и отправлен самим Kadr.".into(),
            contact: String::new(),
            details: Some(details()),
        };
        deliver(&o).unwrap();
    }

    #[test]
    fn this_build_knows_where_reports_go() {
        let (host, path) = crate::http::split(RELAY.expect("KADR_REPORT_URL in .cargo/config.toml")).unwrap();
        assert!(host.ends_with(".workers.dev") && path == "/report", "{host}{path}");
        assert!(RELAY_KEY.is_some_and(|k| k.len() >= 16));
    }
}
