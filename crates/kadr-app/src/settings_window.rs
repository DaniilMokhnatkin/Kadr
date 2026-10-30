//! «Настройки»: grouped rows on acrylic, as in macOS System Settings. Switches slide, rows light up
//! under the pointer, a hotkey is changed by clicking it and pressing the new combination. Every
//! change is written to config.toml; the file watcher applies it like a hand edit (with the same
//! validation and rollback), so there is one source of truth.
use crate::glass::{self, Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, rgb, stroke, text};
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use kadr_store::config::Config;
use kadr_store::hotkey;
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_CONTROL, VK_DELETE, VK_ESCAPE, VK_LWIN,
    VK_MENU, VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
};
use windows::Win32::UI::Shell::{FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub enum Request {
    /// The edited configuration, ready to be written.
    Save(Config),
    /// Global hotkeys off while a new combination is being recorded, back on afterwards.
    Suspend(bool),
    OpenFile,
}

const WIDTH: f32 = 560.0;
const PAD: f32 = 24.0;
const ROW: f32 = 46.0;
const SECTION: f32 = 34.0;
const WM_MOUSELEAVE: u32 = 0x02A3;

const TOGGLES: [&str; 5] = [
    "Запускать вместе с Windows",
    "Миниатюра после снимка",
    "Уведомление о сохранении",
    "Помнить несохранённые снимки",
    "Мгновенный захват",
];
const TOGGLE_HINTS: [&str; 5] = [
    "",
    "Перетащите её в чат или нажмите, чтобы открыть",
    "",
    "До 10 снимков, зашифрованно",
    "≈8 мс вместо ≈23 мс, чуть больше памяти",
];
const KEYS: [&str; 5] = ["Выделить область", "Монитор под курсором", "Активное окно", "Последняя область", "Недавние"];
/// Segmented rows: label, hint, choices.
const CHOICES: [(&str, &str, &[&str]); 2] = [
    ("Тема", "", &["Авто", "Светлая", "Тёмная"]),
    ("Умное размытие", "Строгое — ещё имена, ники и числа", &["Обычное", "Строгое"]),
];
const SEGMENT_W: f32 = 84.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Toggle(usize),
    /// Row, choice.
    Choice(usize, usize),
    Folder,
    FolderReset,
    Key(usize),
    OpenFile,
}

struct Layout {
    close: D2D_RECT_F,
    toggles: [D2D_RECT_F; 5],
    choices: [D2D_RECT_F; 2],
    folder: D2D_RECT_F,
    folder_button: D2D_RECT_F,
    folder_reset: D2D_RECT_F,
    keys: [D2D_RECT_F; 5],
    open_file: D2D_RECT_F,
    groups: [D2D_RECT_F; 4],
    sections: [f32; 4],
    scale: f32,
    height: f32,
}

/// The segmented control's track at the right of a row.
fn track(r: D2D_RECT_F, s: f32, n: usize) -> D2D_RECT_F {
    rect(r.right - 12.0 * s - n as f32 * SEGMENT_W * s, r.top + 8.0 * s, r.right - 12.0 * s, r.bottom - 8.0 * s)
}
fn segment(t: D2D_RECT_F, s: f32, i: f32) -> D2D_RECT_F {
    let left = t.left + i * SEGMENT_W * s;
    rect(left, t.top, left + SEGMENT_W * s, t.bottom)
}

fn layout(width: f32, s: f32) -> Layout {
    let (left, right) = (PAD * s, width - PAD * s);
    let mut y = 84.0 * s;
    let mut sections = [0.0; 4];
    let mut groups = [rect(0.0, 0.0, 0.0, 0.0); 4];
    let mut rows = |n: usize, section: usize, y: &mut f32| -> Vec<D2D_RECT_F> {
        sections[section] = *y;
        *y += SECTION * s;
        let top = *y;
        let out: Vec<_> = (0..n).map(|i| rect(left, top + i as f32 * ROW * s, right, top + (i + 1) as f32 * ROW * s)).collect();
        *y += n as f32 * ROW * s;
        groups[section] = rect(left, top, right, *y);
        *y += 18.0 * s;
        out
    };
    let toggles: [D2D_RECT_F; 5] = rows(5, 0, &mut y).try_into().unwrap();
    let choices: [D2D_RECT_F; 2] = rows(2, 1, &mut y).try_into().unwrap();
    let folder = rows(1, 2, &mut y)[0];
    let keys: [D2D_RECT_F; 5] = rows(5, 3, &mut y).try_into().unwrap();
    let open_file = rect(left, y, left + 180.0 * s, y + 28.0 * s);
    let folder_button = rect(right - 112.0 * s, folder.top + 8.0 * s, right - 12.0 * s, folder.bottom - 8.0 * s);
    let folder_reset = rect(folder_button.left - 92.0 * s, folder_button.top, folder_button.left - 8.0 * s, folder_button.bottom);
    Layout {
        close: rect(width - 44.0 * s, 12.0 * s, width - 12.0 * s, 44.0 * s),
        toggles,
        choices,
        folder,
        folder_button,
        folder_reset,
        keys,
        open_file,
        groups,
        sections,
        height: y + 44.0 * s,
        scale: s,
    }
}

fn hit(l: &Layout, x: f32, y: f32, custom_folder: bool) -> Option<Hit> {
    if inside(l.close, x, y) {
        return Some(Hit::Close);
    }
    if let Some(i) = l.toggles.iter().position(|r| inside(*r, x, y)) {
        return Some(Hit::Toggle(i));
    }
    for (row, r) in l.choices.iter().enumerate() {
        let n = CHOICES[row].2.len();
        let t = track(*r, l.scale, n);
        if let Some(i) = (0..n).find(|&i| inside(segment(t, l.scale, i as f32), x, y)) {
            return Some(Hit::Choice(row, i));
        }
    }
    if inside(l.folder_button, x, y) {
        return Some(Hit::Folder);
    }
    if custom_folder && inside(l.folder_reset, x, y) {
        return Some(Hit::FolderReset);
    }
    if let Some(i) = l.keys.iter().position(|r| inside(*r, x, y)) {
        return Some(Hit::Key(i));
    }
    inside(l.open_file, x, y).then_some(Hit::OpenFile)
}

/// Autostart is the same value the installer writes: HKCU\…\Run\Kadr = "<exe>".
const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
fn autostart() -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, w!("Kadr"), RRF_RT_REG_SZ, None, None, Some(&mut size)) }.is_ok()
}
fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().map(|p| format!("\"{}\"", p.display())).unwrap_or_default();
            let value = kadr_win::wide(&exe);
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                w!("Kadr"),
                REG_SZ.0,
                Some(value.as_ptr() as *const _),
                (value.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("Kadr"));
        }
    }
}

fn toggle_value(c: &Config, i: usize, auto: bool) -> bool {
    match i {
        0 => auto,
        1 => c.save.thumbnail,
        2 => c.save.notify,
        3 => c.recent.enabled,
        _ => c.capture.warm,
    }
}

fn choice_value(c: &Config, row: usize) -> usize {
    use kadr_store::config::{SmartMode, Theme};
    match row {
        0 => match c.appearance.theme {
            Theme::Auto => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        },
        _ => match c.smart.mode {
            SmartMode::Standard => 0,
            SmartMode::Strict => 1,
        },
    }
}
fn set_choice(c: &mut Config, row: usize, i: usize) {
    use kadr_store::config::{SmartMode, Theme};
    match row {
        0 => c.appearance.theme = [Theme::Auto, Theme::Light, Theme::Dark][i.min(2)],
        _ => c.smart.mode = if i == 0 { SmartMode::Standard } else { SmartMode::Strict },
    }
}

fn key_text(c: &Config, i: usize) -> &str {
    let h = &c.hotkeys;
    [&h.region, &h.fullscreen, &h.window, &h.last_region, &h.recent][i]
}
fn key_slot(c: &mut Config, i: usize) -> &mut String {
    let h = &mut c.hotkeys;
    match i {
        0 => &mut h.region,
        1 => &mut h.fullscreen,
        2 => &mut h.window,
        3 => &mut h.last_region,
        _ => &mut h.recent,
    }
}
/// «Ctrl + Shift + PrtSc»: the chip shows the combination the way keycaps read.
fn pretty(text: &str) -> String {
    if text.trim().is_empty() {
        return "Выкл.".into();
    }
    text.split('+').map(|p| if p.eq_ignore_ascii_case("PrintScreen") { "PrtSc" } else { p }).collect::<Vec<_>>().join(" + ")
}

struct Fonts {
    title: IDWriteTextFormat,
    section: IDWriteTextFormat,
    label: IDWriteTextFormat,
    hint: IDWriteTextFormat,
    chip: IDWriteTextFormat,
}

struct State {
    hwnd: HWND,
    host: HWND,
    message: u32,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<Fonts>,
    config: Config,
    auto: bool,
    scale: f32,
    epoch: Instant,
    knobs: [Spring; 5],
    /// Where each segmented control's highlight sits (choice index, gliding).
    slides: [Spring; 2],
    hover: Option<Hit>,
    glow: Vec<(Hit, Spring)>,
    capturing: Option<usize>,
    error: Option<String>,
    tracking: bool,
}
impl State {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
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
    fn sync_knobs(&mut self, snap: bool) {
        let now = self.now();
        for i in 0..5 {
            let v = f32::from(toggle_value(&self.config, i, self.auto));
            if snap {
                self.knobs[i].snap(v);
            } else {
                self.knobs[i].target_with(v, Curve::POP, now);
            }
        }
        for row in 0..2 {
            let v = choice_value(&self.config, row) as f32;
            if snap {
                self.slides[row].snap(v);
            } else {
                self.slides[row].target_with(v, Curve::GLIDE, now);
            }
        }
    }
    fn send(&self, r: Request) {
        REQUEST.with(|q| q.borrow_mut().push(r));
        unsafe {
            let _ = PostMessageW(Some(self.host), self.message, WPARAM(0), LPARAM(0));
        }
    }
    fn commit(&mut self) {
        match kadr_store::config::Config::parse(&kadr_store::config::render(&self.config)) {
            Ok(_) => {
                self.error = None;
                self.send(Request::Save(self.config.clone()));
            }
            Err(e) => self.error = Some(e),
        }
    }
    fn stop_capture(&mut self) {
        if self.capturing.take().is_some() {
            self.send(Request::Suspend(false));
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
}

pub fn take_requests() -> Vec<Request> {
    REQUEST.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// The applied configuration changed (here or by hand): show what is in effect.
pub fn refresh(config: &Config) {
    STATE.with(|s| {
        if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
            && st.capturing.is_none()
        {
            st.config = config.clone();
            st.sync_knobs(false);
            unsafe {
                let _ = InvalidateRect(Some(st.hwnd), None, false);
            }
        }
    });
}

/// The appearance changed: new window material and ink.
pub fn restyle() {
    let hwnd = STATE.with(|s| s.try_borrow().ok().as_deref().and_then(Option::as_ref).map(|st| st.hwnd));
    if let Some(hwnd) = hwnd {
        unsafe {
            glass::match_appearance(hwnd);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

pub fn show(instance: HINSTANCE, host: HWND, message: u32, config: &Config) {
    if STATE.with(|s| s.borrow().is_none()) {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrSettings"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hIcon: unsafe { LoadIconW(Some(instance), PCWSTR(1 as _)) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrSettings"),
                w!("Настройки — Kadr"),
                WS_OVERLAPPED | WS_CAPTION,
                0,
                0,
                560,
                700,
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
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let knob = || Spring::with(0.0, Curve::POP).precise(0.002);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                host,
                message,
                device: None,
                surface: None,
                fonts: None,
                config: config.clone(),
                auto: false,
                scale: 1.0,
                epoch: Instant::now(),
                knobs: [knob(), knob(), knob(), knob(), knob()],
                slides: [knob(), knob()],
                hover: None,
                glow: Vec::new(),
                capturing: None,
                error: None,
                tracking: false,
            })
        });
    }
    // Positioning, showing and focusing make Windows call back into `proc`, which takes the state
    // itself: all of it happens after the state is released.
    let placed = STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return None };
        let st = state.as_mut()?;
        st.config = config.clone();
        st.auto = autostart();
        st.error = None;
        st.sync_knobs(true);
        // Centre on the monitor under the pointer, sized to the content.
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
        let h = (layout(w as f32, st.scale).height as i32).min(mi.rcWork.bottom - mi.rcWork.top - 32);
        let work = mi.rcWork;
        Some((st.hwnd, work.left + (work.right - work.left - w) / 2, work.top + (work.bottom - work.top - h) / 2, w, h))
    });
    let Some((hwnd, x, y, w, h)) = placed else { return };
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER);
        glass::match_appearance(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn ensure_fonts(st: &mut State) -> bool {
    let Some(device) = st.device.as_ref() else { return false };
    if st.fonts.is_some() {
        return true;
    }
    let shared = &device.shared;
    let s = st.scale;
    let make = |size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT| unsafe {
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
        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = f.SetTextAlignment(align);
        // Long folder paths end with an ellipsis instead of running under the buttons.
        let sign = shared.dwrite.CreateEllipsisTrimmingSign(&f).ok();
        let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        let _ = f.SetTrimming(&trimming, sign.as_ref());
        Some(f)
    };
    let fonts = (|| {
        Some(Fonts {
            title: make(24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            section: make(12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            label: make(14.0, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            hint: make(12.0, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            chip: make(13.0, DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        })
    })();
    st.fonts = fonts;
    st.fonts.is_some()
}

fn paint_window(hwnd: HWND) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        if st.device.is_none() {
            st.device = Device::new("settings").ok();
        }
        let Some(device) = st.device.as_ref() else { return };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() || !ensure_fonts(st) {
            return;
        }
        let st = &*st;
        let device = st.device.as_ref().unwrap();
        let size = client_size(hwnd);
        let moving = glass::frame(device, st.surface.as_ref().unwrap(), || paint(device, st, size));
        if moving {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    });
}

fn paint(device: &Device, st: &State, size: (u32, u32)) -> bool {
    let gpu = &device.gpu;
    let f = st.fonts.as_ref().unwrap();
    let theme = Theme::current();
    let now = st.now();
    let s = st.scale;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let l = layout(w, s);
    let mut moving = st.glow.iter().any(|(_, sp)| sp.active(now));
    let glow = |hit: Hit| st.glow.iter().find(|(k, _)| *k == hit).map_or(0.0, |(_, sp)| sp.value(now));
    fill(gpu, theme.wash, rect(0.0, 0.0, w, h), 0.0);
    text(gpu, &f.title, "Настройки", rect(PAD * s, 22.0 * s, w / 2.0, 56.0 * s), theme.text);
    // Close.
    let c = l.close;
    let hover = glow(Hit::Close);
    fill(gpu, fade(theme.tint, hover), inflate(c, -2.0 * s), 14.0 * s);
    let (cx, cy, k) = ((c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0, 5.0 * s);
    unsafe {
        gpu.brush.SetColor(&mix(theme.text_muted, theme.text, hover));
        for (p, q) in [((-k, -k), (k, k)), ((-k, k), (k, -k))] {
            gpu.d2d.DrawLine(
                windows_numerics::Vector2 { X: cx + p.0, Y: cy + p.1 },
                windows_numerics::Vector2 { X: cx + q.0, Y: cy + q.1 },
                &gpu.brush,
                1.5 * s,
                None,
            );
        }
    }
    // Groups: white cards with hairlines between rows.
    for (i, title) in ["ОСНОВНЫЕ", "ВИД", "СОХРАНЕНИЕ", "СОЧЕТАНИЯ КЛАВИШ"].iter().enumerate() {
        let y = l.sections[i];
        text(gpu, &f.section, title, rect(PAD * s + 12.0 * s, y + 8.0 * s, w, y + SECTION * s - 4.0 * s), theme.text_muted);
        let g = l.groups[i];
        fill(gpu, theme.group, g, 12.0 * s);
        stroke(gpu, fade(theme.hairline, 0.7), inflate(g, -0.5), 12.0 * s, 1.0);
    }
    let separator = |r: D2D_RECT_F| fill(gpu, fade(theme.hairline, 0.6), rect(r.left + 16.0 * s, r.top, r.right, r.top + 1.0), 0.0);
    let label_rect = |r: D2D_RECT_F, right: f32| rect(r.left + 16.0 * s, r.top, right, r.bottom);
    // Switches.
    for (i, r) in l.toggles.iter().enumerate() {
        if i > 0 {
            separator(*r);
        }
        let hover = glow(Hit::Toggle(i));
        fill(gpu, fade(theme.tint, 0.6 * hover), inflate(*r, -3.0 * s), 9.0 * s);
        let hint = TOGGLE_HINTS[i];
        if hint.is_empty() {
            text(gpu, &f.label, TOGGLES[i], label_rect(*r, r.right - 80.0 * s), theme.text);
        } else {
            let top = rect(r.left + 16.0 * s, r.top + 4.0 * s, r.right - 80.0 * s, r.top + ROW * s * 0.55);
            let bottom = rect(r.left + 16.0 * s, r.top + ROW * s * 0.5, r.right - 80.0 * s, r.bottom - 4.0 * s);
            text(gpu, &f.label, TOGGLES[i], top, theme.text);
            text(gpu, &f.hint, hint, bottom, theme.text_muted);
        }
        let on = st.knobs[i].value(now);
        moving |= st.knobs[i].active(now);
        let track = rect(r.right - 60.0 * s, r.top + (ROW - 24.0) / 2.0 * s, r.right - 16.0 * s, r.top + (ROW + 24.0) / 2.0 * s);
        fill(gpu, mix(theme.track, theme.accent, on.clamp(0.0, 1.0)), track, 12.0 * s);
        let d = 20.0 * s;
        let x = track.left + 2.0 * s + (track.right - track.left - d - 4.0 * s) * on;
        let knob = rect(x, track.top + 2.0 * s, x + d, track.top + 2.0 * s + d);
        glass::shadow(gpu, knob, d / 2.0, s, 2.0, 1.0, 0.18);
        fill(gpu, rgb(0xffffff, 1.0), knob, d / 2.0);
    }
    // Segmented controls: a raised pill glides to the chosen option.
    for (row, r) in l.choices.iter().enumerate() {
        if row > 0 {
            separator(*r);
        }
        let (label, hint, options) = CHOICES[row];
        let t = track(*r, s, options.len());
        if hint.is_empty() {
            text(gpu, &f.label, label, label_rect(*r, t.left - 8.0 * s), theme.text);
        } else {
            let top = rect(r.left + 16.0 * s, r.top + 4.0 * s, t.left - 8.0 * s, r.top + ROW * s * 0.55);
            let bottom = rect(r.left + 16.0 * s, r.top + ROW * s * 0.5, t.left - 8.0 * s, r.bottom - 4.0 * s);
            text(gpu, &f.label, label, top, theme.text);
            text(gpu, &f.hint, hint, bottom, theme.text_muted);
        }
        fill(gpu, theme.tint, t, 8.0 * s);
        let at = st.slides[row].value(now);
        moving |= st.slides[row].active(now);
        let pill = inflate(segment(t, s, at.clamp(0.0, (options.len() - 1) as f32)), -2.0 * s);
        glass::shadow(gpu, pill, 6.0 * s, s, 2.0, 1.0, 0.14);
        fill(gpu, if theme.dark { theme.control } else { theme.surface }, pill, 6.0 * s);
        for (i, option) in options.iter().enumerate() {
            let cell = segment(t, s, i as f32);
            // Ink follows the pill: full where it rests, muted elsewhere, brighter on hover.
            let near = (1.0 - (at - i as f32).abs()).clamp(0.0, 1.0);
            let ink = mix(theme.text_muted, theme.text, near.max(0.6 * glow(Hit::Choice(row, i))));
            text(gpu, &f.chip, option, cell, ink);
        }
    }
    // Folder.
    let r = l.folder;
    let custom = !st.config.save.folder.trim().is_empty();
    let folder = if custom { st.config.save.folder.clone() } else { "Изображения\\Screenshots (по умолчанию)".into() };
    let edge = if custom { l.folder_reset.left } else { l.folder_button.left };
    text(gpu, &f.label, &folder, label_rect(r, edge - 12.0 * s), theme.text);
    let button = |b: D2D_RECT_F, hit: Hit, label: &str| {
        let hover = glow(hit);
        fill(gpu, mix(theme.tint, theme.tint_strong, hover), b, 8.0 * s);
        text(gpu, &f.chip, label, b, theme.text);
    };
    button(l.folder_button, Hit::Folder, "Изменить…");
    if custom {
        button(l.folder_reset, Hit::FolderReset, "Сбросить");
    }
    // Hotkeys.
    for (i, r) in l.keys.iter().enumerate() {
        if i > 0 {
            separator(*r);
        }
        let hover = glow(Hit::Key(i));
        text(gpu, &f.label, KEYS[i], label_rect(*r, r.right - 200.0 * s), theme.text);
        let recording = st.capturing == Some(i);
        let value = if recording { "Нажмите сочетание…".to_string() } else { pretty(key_text(&st.config, i)) };
        let chip = rect(r.right - 196.0 * s, r.top + 8.0 * s, r.right - 12.0 * s, r.bottom - 8.0 * s);
        if recording {
            // A gently breathing accent ring: the window is listening.
            moving = true;
            let pulse = 0.55 + 0.45 * (now as f32 * 4.0).sin();
            fill(gpu, theme.selected, chip, 8.0 * s);
            stroke(gpu, fade(theme.accent, pulse), inflate(chip, -0.75 * s), 8.0 * s, 1.5 * s);
            text(gpu, &f.chip, &value, chip, theme.on_selected);
        } else {
            fill(gpu, mix(theme.tint, theme.tint_strong, hover), chip, 8.0 * s);
            let ink = if key_text(&st.config, i).trim().is_empty() { theme.text_muted } else { theme.text };
            text(gpu, &f.chip, &value, chip, ink);
        }
    }
    // Footer: the file itself, and any validation error.
    let hover = glow(Hit::OpenFile);
    text(gpu, &f.hint, "Открыть config.toml", l.open_file, mix(theme.accent, theme.on_selected, hover));
    let footer = rect(l.open_file.right + 12.0 * s, l.open_file.top, w - PAD * s, l.open_file.bottom);
    if let Some(e) = &st.error {
        text(gpu, &f.hint, e, footer, glass::error_ink(&theme));
    } else if st.capturing.is_some() {
        text(gpu, &f.hint, "Esc — отмена · Backspace — выключить", footer, theme.text_muted);
    }
    moving
}

/// Folder picker (IFileOpenDialog in folder mode), starting at the current folder.
fn pick_folder(owner: HWND) -> Option<String> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS).ok()?;
        dialog.SetTitle(w!("Папка для снимков")).ok()?;
        dialog.Show(Some(owner)).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = path.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        text
    }
}

fn modifiers_down() -> (bool, bool, bool, bool) {
    let down = |k: u16| unsafe { GetKeyState(i32::from(k)) } < 0;
    (down(VK_CONTROL.0), down(VK_MENU.0), down(VK_SHIFT.0), down(VK_LWIN.0) || down(VK_RWIN.0))
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
            let caption = STATE.with(|s| {
                s.try_borrow().is_ok_and(|g| {
                    g.as_ref().is_some_and(|st| {
                        (p.y as f32) < 72.0 * st.scale
                            && hit(&layout(width(), st.scale), p.x as f32, p.y as f32, true).is_none()
                    })
                })
            });
            return LRESULT(if caption { HTCAPTION as isize } else { HTCLIENT as isize });
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
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
                    let custom = !st.config.save.folder.trim().is_empty();
                    let h = hit(&layout(width(), st.scale), x, y, custom);
                    st.set_hover(h);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    st.tracking = false;
                    st.set_hover(None);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let hand = STATE.with(|s| s.try_borrow().is_ok_and(|g| g.as_ref().is_some_and(|st| st.hover.is_some())));
            unsafe {
                SetCursor(LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).ok());
            }
            return LRESULT(1);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            let (mut pick, mut hide) = (false, false);
            STATE.with(|s| {
                let Ok(mut state) = s.try_borrow_mut() else { return };
                let Some(st) = state.as_mut() else { return };
                let custom = !st.config.save.folder.trim().is_empty();
                let h = hit(&layout(width(), st.scale), x, y, custom);
                if !matches!(h, Some(Hit::Key(_))) {
                    st.stop_capture();
                }
                match h {
                    Some(Hit::Close) => hide = true,
                    Some(Hit::Toggle(0)) => {
                        st.auto = !st.auto;
                        set_autostart(st.auto);
                        st.auto = autostart();
                        st.sync_knobs(false);
                    }
                    Some(Hit::Toggle(i)) => {
                        match i {
                            1 => st.config.save.thumbnail ^= true,
                            2 => st.config.save.notify ^= true,
                            3 => st.config.recent.enabled ^= true,
                            _ => st.config.capture.warm ^= true,
                        }
                        st.sync_knobs(false);
                        st.commit();
                    }
                    Some(Hit::Choice(row, i)) => {
                        if choice_value(&st.config, row) != i {
                            set_choice(&mut st.config, row, i);
                            st.sync_knobs(false);
                            st.commit();
                        }
                    }
                    Some(Hit::Folder) => pick = true,
                    Some(Hit::FolderReset) => {
                        st.config.save.folder.clear();
                        st.commit();
                    }
                    Some(Hit::Key(i)) => {
                        if st.capturing != Some(i) {
                            if st.capturing.is_none() {
                                st.send(Request::Suspend(true));
                            }
                            st.capturing = Some(i);
                        }
                    }
                    Some(Hit::OpenFile) => st.send(Request::OpenFile),
                    None => {}
                }
            });
            // Hiding and the dialog make Windows call back into `proc`: no borrow may be held across them.
            if hide {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            if pick && let Some(folder) = pick_folder(hwnd) {
                STATE.with(|s| {
                    if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                        st.config.save.folder = folder;
                        st.commit();
                    }
                });
            }
            redraw();
            return LRESULT(0);
        }
        // PrtSc only produces a key-up; every other key is taken on key-down.
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wp.0 as u16;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let mut hide = false;
            let handled = STATE.with(|s| {
                let Ok(mut state) = s.try_borrow_mut() else { return false };
                let Some(st) = state.as_mut() else { return false };
                let Some(i) = st.capturing else {
                    if down && vk == VK_ESCAPE.0 {
                        hide = true;
                        return true;
                    }
                    return false;
                };
                if down && vk == VK_ESCAPE.0 {
                    st.stop_capture();
                    return true;
                }
                if down && (vk == VK_BACK.0 || vk == VK_DELETE.0) {
                    key_slot(&mut st.config, i).clear();
                    st.stop_capture();
                    st.commit();
                    return true;
                }
                let modifier = [VK_CONTROL.0, VK_MENU.0, VK_SHIFT.0, VK_LWIN.0, VK_RWIN.0, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5]
                    .contains(&vk);
                if modifier || (down == (vk == VK_SNAPSHOT.0)) {
                    return true;
                }
                let (ctrl, alt, shift, win) = modifiers_down();
                let Some(name) = hotkey::name(hotkey::Hotkey { ctrl, alt, shift, win, vk }) else {
                    st.error = Some("Эту клавишу нельзя назначить".into());
                    return true;
                };
                let previous = key_text(&st.config, i).to_string();
                *key_slot(&mut st.config, i) = name;
                st.stop_capture();
                st.commit();
                if st.error.is_some() {
                    *key_slot(&mut st.config, i) = previous;
                }
                true
            });
            if hide {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            if handled {
                redraw();
                return LRESULT(0);
            }
        }
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    st.stop_capture();
                }
            });
            redraw();
        }
        WM_CLOSE => {
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    st.stop_capture();
                }
            });
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    st.scale = (wp.0 & 0xffff) as f32 / 96.0;
                    st.fonts = None;
                }
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

/// Offscreen render for design review: one switch off, a custom folder, a hotkey being recorded.
pub fn preview(scale: f32) -> windows::core::Result<kadr_doc::Image> {
    let mut config = Config::default();
    config.capture.warm = false;
    config.smart.mode = kadr_store::config::SmartMode::Strict;
    config.save.folder = r"D:\Снимки экрана".into();
    let knob = |v: bool| Spring::with(f32::from(v), Curve::POP);
    let mut st = State {
        hwnd: HWND::default(),
        host: HWND::default(),
        message: 0,
        device: Device::new("settings-preview").ok(),
        surface: None,
        fonts: None,
        knobs: [knob(true), knob(true), knob(true), knob(true), knob(false)],
        slides: [Spring::with(0.0, Curve::GLIDE), Spring::with(1.0, Curve::GLIDE)],
        config,
        auto: true,
        scale,
        epoch: Instant::now(),
        hover: None,
        glow: Vec::new(),
        capturing: Some(4),
        error: None,
        tracking: false,
    };
    if !ensure_fonts(&mut st) {
        return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
    }
    let w = (WIDTH * scale).round();
    let h = layout(w, scale).height.round();
    let device = st.device.as_ref().unwrap();
    glass::render_offscreen(device, w as u32, h as u32, || {
        fill(&device.gpu, if Theme::current().dark { rgb(0x202022, 1.0) } else { rgb(0xeef0f4, 1.0) }, rect(0.0, 0.0, w, h), 0.0);
        paint(device, &st, (w as u32, h as u32));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rows_do_not_overlap_and_every_control_is_reachable_at_each_dpi() {
        for s in [1.0f32, 1.5, 2.0] {
            let w = WIDTH * s;
            let l = layout(w, s);
            let all: Vec<D2D_RECT_F> =
                l.toggles.iter().chain(l.choices.iter()).chain([&l.folder]).chain(l.keys.iter()).copied().collect();
            for (row, r) in l.choices.iter().enumerate() {
                let n = CHOICES[row].2.len();
                let t = track(*r, s, n);
                assert!(t.left > r.left + 150.0 * s, "the label keeps room");
                for i in 0..n {
                    let c = segment(t, s, i as f32);
                    assert_eq!(hit(&l, (c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0, false), Some(Hit::Choice(row, i)));
                }
            }
            for pair in all.windows(2) {
                assert!(pair[0].bottom <= pair[1].top + 0.01);
            }
            for (i, r) in l.toggles.iter().enumerate() {
                assert_eq!(hit(&l, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, false), Some(Hit::Toggle(i)));
            }
            let b = l.folder_button;
            assert_eq!(hit(&l, (b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0, false), Some(Hit::Folder));
            let r = l.folder_reset;
            assert_eq!(hit(&l, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, true), Some(Hit::FolderReset));
            assert!(l.open_file.bottom <= l.height);
        }
        assert_eq!(pretty("Ctrl+Shift+PrintScreen"), "Ctrl + Shift + PrtSc");
        assert_eq!(pretty(""), "Выкл.");
    }
}
