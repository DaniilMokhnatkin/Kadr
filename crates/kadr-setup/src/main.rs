//! Kadr-Setup: a friendly installer. It greets, walks through what Kadr does with small live
//! scenes, explains the one administrator permission it needs, installs (the certificate, the
//! program, autostart, Windows text recognition) while showing every step, and starts Kadr.
//! The same visual system as Kadr itself: Mica, Direct2D, springs.
#![windows_subsystem = "windows"]

#[allow(dead_code)]
#[path = "../../kadr-app/src/glass.rs"]
mod glass;
mod art;
mod install;

use glass::{Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, rgb, shadow, stroke, text};
use install::{Job, Payload, Report, Start};
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use windows_numerics::{Matrix3x2, Vector2};

const WIDTH: f32 = 820.0;
const HEIGHT: f32 = 560.0;
const WM_MOUSELEAVE: u32 = 0x02A3;

const WELCOME: usize = 0;
const TOUR: usize = 6;
const READY: usize = TOUR + 1;
const INSTALLING: usize = READY + 1;
const DONE: usize = INSTALLING + 1;
const PAGES: usize = DONE + 1;

/// Tour pages: title, text, key hints.
const FEATURES: [(&str, &str, &[&str]); TOUR] = [
    (
        "Снимок за секунду",
        "Нажмите PrtSc — экран замрёт. Выделите область мышью или просто кликните по окну. Двойной клик внутри — и снимок уже в буфере обмена.",
        &["PrtSc", "Ctrl + C", "Ctrl + S"],
    ),
    (
        "Рисуйте поверх",
        "Стрелки, рамки, текст, маркер и размытие. Нумерация как в ShareX: потяните — и у номера появится хвостик к нужному месту.",
        &["Стрелка · A", "Нумерация · N", "Ctrl + Z"],
    ),
    (
        "Цензура одной кнопкой",
        "Лёгкая прячет почту, телефоны, карты и пароли. Жёсткая — ещё имена, ники и числа. Полная размывает весь текст. Всё распознаётся прямо на компьютере — снимок никуда не уходит.",
        &["Лёгкая", "Жёсткая", "Полная"],
    ),
    (
        "Текст и перевод",
        "«Текст» копирует надписи прямо с картинки. Зажмите Alt и кликните по абзацу — перевод появится рядом. Наружу уходит только распознанный текст, не картинка.",
        &["Alt + клик", "Ctrl + Shift + C", "Ctrl + T"],
    ),
    (
        "Видео со звуком",
        "Выделите область и нажмите «Запись». Kadr запишет её вместе со звуком компьютера. PrtSc — стоп, и видео сразу в буфере: вставляйте в Telegram.",
        &["Запись", "PrtSc — стоп"],
    ),
    (
        "Миниатюра и «Недавние»",
        "После снимка в углу появляется миниатюра: перетащите её в чат или нажмите, чтобы открыть. «Недавние» хранят несохранённые снимки — зашифрованно.",
        &["Перетащить в чат", "Ctrl + Alt + PrtSc"],
    ),
];

/// Install steps as the script reports them: id, title, what is happening while it runs.
const STEPS: [(&str, &str, &str); 7] = [
    ("prepare", "Подготовка", ""),
    ("cert", "Сертификат подписи", "создаётся на этом компьютере, ключ никуда не уходит"),
    ("copy", "Копирование и подпись", "C:\\Program Files\\Kadr"),
    ("autostart", "Автозапуск и ярлык в «Пуск»", ""),
    ("ocr-en", "Распознавание текста · английский", "скачивается из Центра обновления Windows — до пары минут"),
    ("ocr-ru", "Распознавание текста · русский", "скачивается из Центра обновления Windows — до пары минут"),
    ("launch", "Запуск Kadr", ""),
];

#[derive(Clone, PartialEq)]
enum Step {
    Waiting,
    Running(f64),
    Done(f64),
    Skipped,
    Warn(String, f64),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Primary,
    Secondary,
    Dot(usize),
    Toggle(usize),
}

struct Fonts {
    hero: IDWriteTextFormat,
    title: IDWriteTextFormat,
    lead: IDWriteTextFormat,
    lead_center: IDWriteTextFormat,
    caption: IDWriteTextFormat,
    button: IDWriteTextFormat,
    small: IDWriteTextFormat,
    small_bold: IDWriteTextFormat,
    chip: IDWriteTextFormat,
    art_small: IDWriteTextFormat,
    art_bold: IDWriteTextFormat,
    art_center: IDWriteTextFormat,
}

struct App {
    hwnd: HWND,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<Fonts>,
    logo: Option<ID2D1Bitmap1>,
    scale: f32,
    epoch: Instant,
    page: usize,
    slide: Spring,
    entered: [f64; PAGES],
    hover: Option<Hit>,
    glow: Vec<(Hit, Spring)>,
    options: [bool; 2],
    knobs: [Spring; 2],
    payload: Result<Payload, String>,
    job: Option<Job>,
    steps: Vec<Step>,
    declined: bool,
    failed: Option<String>,
    progress: Spring,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn now(app: &App) -> f64 {
    app.epoch.elapsed().as_secs_f64()
}

impl App {
    fn go(&mut self, page: usize) {
        let page = page.min(PAGES - 1);
        if page == self.page {
            return;
        }
        let t = now(self);
        self.page = page;
        self.entered[page] = t;
        self.slide.target_with(page as f32, Curve::LAYOUT, t);
    }
    fn primary_label(&self) -> Option<&'static str> {
        Some(match self.page {
            WELCOME => "Начать",
            p if p <= TOUR => "Далее",
            READY => "Установить",
            INSTALLING => return None,
            _ if self.failed.is_some() => "Повторить",
            _ => "Готово",
        })
    }
    fn secondary_label(&self) -> Option<&'static str> {
        match self.page {
            WELCOME => Some("Сразу к установке"),
            p if p <= READY => Some("Назад"),
            DONE if self.failed.is_some() => Some("Закрыть"),
            _ => None,
        }
    }
    fn primary(&mut self) {
        match self.page {
            READY => self.install(),
            DONE if self.failed.is_some() => {
                self.failed = None;
                self.go(READY);
            }
            DONE => close(self.hwnd),
            INSTALLING => {}
            p => self.go(p + 1),
        }
    }
    fn secondary(&mut self) {
        match self.page {
            WELCOME => self.go(READY),
            DONE => close(self.hwnd),
            p if p <= READY => self.go(p - 1),
            _ => {}
        }
    }
    fn install(&mut self) {
        let payload = match &self.payload {
            Ok(p) => p,
            Err(e) => {
                self.failed = Some(e.clone());
                self.go(DONE);
                return;
            }
        };
        self.declined = false;
        match install::start(self.hwnd, payload, self.options[0], self.options[1]) {
            Start::Running(job) => {
                self.job = Some(job);
                self.steps = vec![Step::Waiting; STEPS.len()];
                let t = now(self);
                self.progress.snap(0.0);
                self.progress.target(0.02, t);
                self.go(INSTALLING);
            }
            Start::Declined => self.declined = true,
            Start::Failed(e) => {
                self.failed = Some(e);
                self.go(DONE);
            }
        }
    }
    /// Follows the script: steps light up, tick off, or warn.
    fn follow(&mut self) {
        let Some(job) = &mut self.job else { return };
        let (reports, exit) = job.poll();
        let t = now(self);
        let index = |id: &str| STEPS.iter().position(|(s, _, _)| *s == id);
        let mut finished = false;
        for r in reports {
            match r {
                Report::Step(id) => {
                    if let Some(i) = index(&id) {
                        self.steps[i] = Step::Running(t);
                    }
                }
                Report::Ok(id) => {
                    if let Some(i) = index(&id) {
                        self.steps[i] = Step::Done(t);
                    }
                }
                Report::Skip(id) => {
                    if let Some(i) = index(&id) {
                        self.steps[i] = Step::Skipped;
                    }
                }
                Report::Warn(id, text) => {
                    if let Some(i) = index(&id) {
                        self.steps[i] = Step::Warn(text, t);
                    }
                }
                Report::Error(text) => self.failed = Some(text),
                Report::Finish => finished = true,
            }
        }
        let done = self.steps.iter().filter(|s| !matches!(s, Step::Waiting | Step::Running(_))).count();
        self.progress.target_with(done as f32 / STEPS.len() as f32, Curve::GLIDE, t);
        if finished || exit.is_some() {
            if !finished && self.failed.is_none() {
                self.failed = Some("Установка прервалась. Попробуйте ещё раз.".into());
            }
            self.job = None;
            // Let the last tick be seen before moving on.
            self.entered[DONE] = t + 0.6;
            self.page = DONE;
            self.slide.target_at(DONE as f32, t, t + 0.6);
        }
    }
    fn warned(&self) -> bool {
        self.steps.iter().any(|s| matches!(s, Step::Warn(..)))
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
        let t = now(self);
        if let Some(old) = self.hover {
            self.glow(old).target_with(0.0, Curve::HOVER_OUT, t);
        }
        if let Some(new) = h {
            self.glow(new).target_with(1.0, Curve::HOVER_IN, t);
        }
        self.hover = h;
    }
}

fn close(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Where things are, in pixels, for a window `w`×`h` at scale `s`.
struct Layout {
    close: D2D_RECT_F,
    primary: D2D_RECT_F,
    secondary: D2D_RECT_F,
    dots: Vec<D2D_RECT_F>,
    toggles: [D2D_RECT_F; 2],
    art: D2D_RECT_F,
}

fn layout(app: &App, w: f32, h: f32) -> Layout {
    let s = app.scale;
    let bh = 40.0 * s;
    let bottom = h - 28.0 * s;
    let width = |label: Option<&str>| label.map_or(0.0, |l| (l.chars().count() as f32 * 8.6 + 44.0).max(132.0) * s);
    let pw = width(app.primary_label());
    let primary = rect(w - 40.0 * s - pw, bottom - bh, w - 40.0 * s, bottom);
    let sw = width(app.secondary_label());
    let secondary = rect(primary.left - 10.0 * s - sw, primary.top, primary.left - 10.0 * s, primary.bottom);
    let dots = (0..TOUR)
        .map(|i| {
            let x = 44.0 * s + i as f32 * 18.0 * s;
            rect(x - 6.0 * s, bottom - bh / 2.0 - 8.0 * s, x + 10.0 * s, bottom - bh / 2.0 + 8.0 * s)
        })
        .collect();
    let toggles = [0, 1].map(|i| {
        let top = 282.0 * s + i as f32 * 56.0 * s;
        rect(40.0 * s, top, w - 40.0 * s, top + 56.0 * s)
    });
    Layout {
        close: rect(w - 48.0 * s, 14.0 * s, w - 14.0 * s, 48.0 * s),
        primary,
        secondary,
        dots,
        toggles,
        art: rect(40.0 * s, 64.0 * s, 40.0 * s + 380.0 * s, 64.0 * s + 330.0 * s),
    }
}

fn hit(app: &App, x: f32, y: f32) -> Option<Hit> {
    let (w, h) = client_size(app.hwnd);
    let l = layout(app, w as f32, h as f32);
    if app.page != INSTALLING && inside(l.close, x, y) {
        return Some(Hit::Close);
    }
    if app.primary_label().is_some() && inside(l.primary, x, y) {
        return Some(Hit::Primary);
    }
    if app.secondary_label().is_some() && inside(l.secondary, x, y) {
        return Some(Hit::Secondary);
    }
    if (1..=TOUR).contains(&app.page)
        && let Some(i) = l.dots.iter().position(|r| inside(*r, x, y))
    {
        return Some(Hit::Dot(i));
    }
    if app.page == READY
        && let Some(i) = l.toggles.iter().position(|r| inside(*r, x, y))
    {
        return Some(Hit::Toggle(i));
    }
    None
}

fn format(shared: &kadr_render::gpu::Shared, size: f32, weight: DWRITE_FONT_WEIGHT, center: bool, middle: bool, wrap: bool) -> Option<IDWriteTextFormat> {
    unsafe {
        let f = shared
            .dwrite
            .CreateTextFormat(PCWSTR(shared.ui_family.as_ptr()), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("ru-ru"))
            .ok()?;
        let _ = f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
        if center {
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        if middle {
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        }
        Some(f)
    }
}

fn fonts(shared: &kadr_render::gpu::Shared, s: f32) -> Option<Fonts> {
    let (semi, regular, bold) = (DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_BOLD);
    Some(Fonts {
        hero: format(shared, 34.0 * s, bold, true, false, true)?,
        title: format(shared, 27.0 * s, bold, false, false, true)?,
        lead: format(shared, 15.0 * s, regular, false, false, true)?,
        lead_center: format(shared, 16.0 * s, regular, true, false, true)?,
        caption: format(shared, 12.0 * s, semi, false, false, false)?,
        button: format(shared, 14.0 * s, semi, true, true, false)?,
        small: format(shared, 12.5 * s, regular, false, false, true)?,
        small_bold: format(shared, 14.0 * s, semi, false, true, false)?,
        chip: format(shared, 12.5 * s, semi, true, true, false)?,
        art_small: format(shared, 11.5 * s, regular, false, false, false)?,
        art_bold: format(shared, 11.5 * s, semi, false, false, false)?,
        art_center: format(shared, 11.5 * s, semi, true, true, false)?,
    })
}

/// The app icon from the assets, premultiplied for Direct2D.
fn logo(device: &Device) -> Option<ID2D1Bitmap1> {
    let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!("../../../assets/icons/kadr.png").as_slice()));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let bgra: Vec<u8> = buf[..info.buffer_size()]
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3] as u32;
            let m = |c: u8| (c as u32 * a / 255) as u8;
            [m(p[2]), m(p[1]), m(p[0]), p[3]]
        })
        .collect();
    unsafe {
        device
            .gpu
            .d2d
            .CreateBitmap(
                D2D_SIZE_U { width: info.width, height: info.height },
                Some(bgra.as_ptr() as *const _),
                info.width * 4,
                &glass::target_props(D2D1_BITMAP_OPTIONS_NONE),
            )
            .ok()
    }
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // One look for the setup: light, whatever Windows uses.
    kadr_render::theme::choose(Some(false));
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--snapshots") {
        let folder = std::path::PathBuf::from(args.get(i + 1).map_or("setup-snapshots", String::as_str));
        let scale = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(1.5);
        let _ = snapshots(&folder, scale);
        return;
    }
    let instance = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        hInstance: instance.into(),
        lpfnWndProc: Some(proc),
        lpszClassName: w!("KadrSetup"),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hIcon: unsafe { LoadIconW(Some(instance.into()), PCWSTR(1 as _)) }.unwrap_or_default(),
        ..Default::default()
    };
    let hwnd = unsafe {
        RegisterClassExW(&class);
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("KadrSetup"),
            w!("Установка Kadr"),
            WS_OVERLAPPED | WS_CAPTION | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            800,
            560,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    let Ok(hwnd) = hwnd else { return };
    let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
        let backdrop = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const _ as *const _, 4);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        glass::match_appearance(hwnd);
    }
    let spring = |c: Curve| Spring::with(0.0, c).precise(0.001);
    let mut knobs = [spring(Curve::POP), spring(Curve::POP)];
    knobs.iter_mut().for_each(|k| k.snap(1.0));
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            hwnd,
            device: None,
            surface: None,
            fonts: None,
            logo: None,
            scale,
            epoch: Instant::now(),
            page: WELCOME,
            slide: spring(Curve::LAYOUT),
            entered: [0.0; PAGES],
            hover: None,
            glow: Vec::new(),
            options: [true, true],
            knobs,
            payload: install::payload(),
            job: None,
            steps: vec![Step::Waiting; STEPS.len()],
            declined: false,
            failed: None,
            progress: spring(Curve::GLIDE),
        })
    });
    // Centred on the monitor under the pointer.
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let monitor = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut mi);
        let (w, h) = ((WIDTH * scale) as i32, (HEIGHT * scale) as i32);
        let r = mi.rcWork;
        let _ = SetWindowPos(hwnd, None, r.left + (r.right - r.left - w) / 2, r.top + (r.bottom - r.top - h) / 2, w, h, SWP_NOZORDER | SWP_FRAMECHANGED);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn paint_window(hwnd: HWND) {
    let moving = APP.with(|a| {
        let Ok(mut guard) = a.try_borrow_mut() else { return false };
        let Some(app) = guard.as_mut() else { return false };
        if app.device.is_none() {
            app.device = Device::new("setup").ok();
        }
        let Some(device) = app.device.as_ref() else { return false };
        if glass::ensure(&mut app.surface, device, hwnd).is_err() {
            return false;
        }
        if app.fonts.is_none() {
            app.fonts = fonts(&device.shared, app.scale);
        }
        if app.logo.is_none() {
            app.logo = logo(device);
        }
        if app.fonts.is_none() {
            return false;
        }
        app.follow();
        let app = &*app;
        let device = app.device.as_ref().unwrap();
        let size = client_size(hwnd);
        glass::frame(device, app.surface.as_ref().unwrap(), || paint(device, app, size))
    });
    if moving {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

/// Soft colour drifting behind everything, like light through frosted glass.
fn aurora(device: &Device, theme: &Theme, w: f32, h: f32, t: f64) {
    let gpu = &device.gpu;
    let blobs = [
        (theme.accent, 0.18, 0.2, 0.9),
        (rgb(0xaf52de, 1.0), 0.82, 0.25, 0.7),
        (rgb(0x30d5c8, 1.0), 0.55, 0.95, 0.8),
    ];
    let strength = if theme.dark { 0.20 } else { 0.14 };
    for (i, (c, fx, fy, k)) in blobs.iter().enumerate() {
        let drift = t * 0.12 + i as f64 * 2.1;
        let cx = w * (fx + 0.05 * drift.sin() as f32);
        let cy = h * (fy + 0.05 * (drift * 1.3).cos() as f32);
        let radius = w.max(h) * 0.42 * k;
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: fade(*c, strength) },
            D2D1_GRADIENT_STOP { position: 1.0, color: fade(*c, 0.0) },
        ];
        unsafe {
            // The device context overloads this; the render target's simple form is the one needed.
            let target: &ID2D1RenderTarget = &gpu.d2d;
            let Ok(collection) = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { continue };
            let Ok(brush) = gpu.d2d.CreateRadialGradientBrush(
                &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                    center: Vector2::new(cx, cy),
                    gradientOriginOffset: Vector2::new(0.0, 0.0),
                    radiusX: radius,
                    radiusY: radius,
                },
                None,
                &collection,
            ) else {
                continue;
            };
            gpu.d2d.FillRectangle(&rect(0.0, 0.0, w, h), &brush);
        }
    }
}

fn paint(device: &Device, app: &App, size: (u32, u32)) -> bool {
    let gpu = &device.gpu;
    let theme = Theme::current();
    let f = app.fonts.as_ref().unwrap();
    let t = now(app);
    let s = app.scale;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let l = layout(app, w, h);
    let glow = |hit: Hit| app.glow.iter().find(|(k, _)| *k == hit).map_or(0.0, |(_, sp)| sp.value(t));
    fill(gpu, theme.wash, rect(0.0, 0.0, w, h), 0.0);
    aurora(device, &theme, w, h, t);
    // Pages: the current one and its neighbour while gliding, sliding and fading.
    let at = app.slide.value(t);
    for page in 0..PAGES {
        let d = page as f32 - at;
        if d.abs() >= 1.0 {
            continue;
        }
        let a = (1.0 - d.abs() * 1.4).clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        }
        unsafe {
            gpu.d2d.SetTransform(&Matrix3x2::translation(d * 90.0 * s, 0.0));
        }
        let local = (t - app.entered[page]).max(0.0);
        draw_page(device, app, f, &theme, &l, page, w, h, a, local);
        unsafe {
            gpu.d2d.SetTransform(&Matrix3x2::identity());
        }
    }
    // Close.
    if app.page != INSTALLING {
        let c = l.close;
        let hover = glow(Hit::Close);
        fill(gpu, fade(theme.tint_strong, hover), inflate(c, -3.0 * s), 15.0 * s);
        let (cx, cy, k) = ((c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0, 5.0 * s);
        unsafe {
            gpu.brush.SetColor(&mix(theme.text_muted, theme.text, hover));
            for (p, q) in [((-k, -k), (k, k)), ((-k, k), (k, -k))] {
                gpu.d2d.DrawLine(Vector2::new(cx + p.0, cy + p.1), Vector2::new(cx + q.0, cy + q.1), &gpu.brush, 1.6 * s, None);
            }
        }
    }
    // Dots for the tour.
    let tour = (at - 0.5).clamp(0.0, 1.0).min((TOUR as f32 + 0.5 - at).clamp(0.0, 1.0));
    if tour > 0.0 {
        for (i, r) in l.dots.iter().enumerate() {
            let on = (1.0 - ((i + 1) as f32 - at).abs()).clamp(0.0, 1.0);
            let cx = (r.left + r.right) / 2.0;
            let cy = (r.top + r.bottom) / 2.0;
            let half = (3.5 + 6.0 * on) * s;
            let dot = rect(cx - half, cy - 3.5 * s, cx + half, cy + 3.5 * s);
            fill(gpu, fade(mix(theme.track, theme.accent, on.max(0.4 * glow(Hit::Dot(i)))), tour), dot, 3.5 * s);
        }
    }
    // Buttons.
    if let Some(label) = app.secondary_label() {
        let r = l.secondary;
        fill(gpu, mix(theme.tint, theme.tint_strong, glow(Hit::Secondary)), r, 11.0 * s);
        text(gpu, &f.button, label, r, theme.text);
    }
    if let Some(label) = app.primary_label() {
        let r = l.primary;
        let hover = glow(Hit::Primary);
        let base = theme.accent;
        shadow(gpu, r, 11.0 * s, s, 6.0, 3.0, 0.18 + 0.1 * hover);
        fill(gpu, mix(base, rgb(0xffffff, 1.0), 0.1 * hover), r, 11.0 * s);
        let label_r = if app.page == READY { rect(r.left + 20.0 * s, r.top, r.right, r.bottom) } else { r };
        text(gpu, &f.button, label, label_r, theme.on_accent);
        if app.page == READY {
            shield(device, (r.left + 22.0 * s, (r.top + r.bottom) / 2.0), s * 0.8, theme.on_accent);
        }
    }
    true
}

/// Height `value` takes in `format` when wrapped to `width`.
fn text_height(device: &Device, format: &IDWriteTextFormat, value: &str, width: f32) -> f32 {
    let utf16: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        device
            .shared
            .dwrite
            .CreateTextLayout(&utf16, format, width.max(1.0), 10_000.0)
            .and_then(|l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m).map(|()| m.height)
            })
            .unwrap_or(0.0)
    }
}

/// The administrator shield, as Windows marks actions that ask for permission.
fn shield(device: &Device, (cx, cy): (f32, f32), s: f32, c: D2D1_COLOR_F) {
    let pts = [(0.0, -9.0), (7.5, -6.0), (7.0, 2.5), (0.0, 9.5), (-7.0, 2.5), (-7.5, -6.0)];
    unsafe {
        let Ok(g) = device.shared.d2d.CreatePathGeometry() else { return };
        let Ok(sink) = g.Open() else { return };
        sink.BeginFigure(Vector2::new(cx + pts[0].0 * s, cy + pts[0].1 * s), windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_FILLED);
        let rest: Vec<Vector2> = pts[1..].iter().map(|(x, y)| Vector2::new(cx + x * s, cy + y * s)).collect();
        sink.AddLines(&rest);
        sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_CLOSED);
        let _ = sink.Close();
        device.gpu.brush.SetColor(&c);
        device.gpu.d2d.FillGeometry(&g, &device.gpu.brush, None);
    }
}

/// A tick mark drawn along its length: `p` from 0 to 1.
fn tick(device: &Device, (cx, cy): (f32, f32), size: f32, p: f32, c: D2D1_COLOR_F, width: f32) {
    let a = (cx - size * 0.45, cy + size * 0.02);
    let b = (cx - size * 0.12, cy + size * 0.32);
    let e = (cx + size * 0.48, cy - size * 0.3);
    let first = (b.0 - a.0).hypot(b.1 - a.1);
    let second = (e.0 - b.0).hypot(e.1 - b.1);
    let mut left = p.clamp(0.0, 1.0) * (first + second);
    let mut pts = vec![a];
    let k1 = (left / first).min(1.0);
    pts.push((a.0 + (b.0 - a.0) * k1, a.1 + (b.1 - a.1) * k1));
    left -= first;
    if left > 0.0 {
        let k2 = (left / second).min(1.0);
        pts.push((b.0 + (e.0 - b.0) * k2, b.1 + (e.1 - b.1) * k2));
    }
    unsafe {
        let Ok(g) = device.shared.d2d.CreatePathGeometry() else { return };
        let Ok(sink) = g.Open() else { return };
        sink.BeginFigure(Vector2::new(pts[0].0, pts[0].1), windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_HOLLOW);
        let rest: Vec<Vector2> = pts[1..].iter().map(|(x, y)| Vector2::new(*x, *y)).collect();
        sink.AddLines(&rest);
        sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_OPEN);
        let _ = sink.Close();
        let style = device.shared.d2d.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES1 { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() },
            None,
        );
        device.gpu.brush.SetColor(&c);
        device.gpu.d2d.DrawGeometry(&g, &device.gpu.brush, width, style.ok().as_ref().map(|s| s.into()));
    }
}

/// A spinning arc: the step that is running.
fn spinner(device: &Device, (cx, cy): (f32, f32), r: f32, t: f64, c: D2D1_COLOR_F, width: f32) {
    let start = (t * 5.0) as f32;
    let sweep = 4.2;
    let n = 24;
    let pts: Vec<Vector2> = (0..=n)
        .map(|i| {
            let a = start + sweep * i as f32 / n as f32;
            Vector2::new(cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    unsafe {
        let Ok(g) = device.shared.d2d.CreatePathGeometry() else { return };
        let Ok(sink) = g.Open() else { return };
        sink.BeginFigure(pts[0], windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddLines(&pts[1..]);
        sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_OPEN);
        let _ = sink.Close();
        let style = device.shared.d2d.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES1 { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, ..Default::default() },
            None,
        );
        device.gpu.brush.SetColor(&c);
        device.gpu.d2d.DrawGeometry(&g, &device.gpu.brush, width, style.ok().as_ref().map(|s| s.into()));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_page(device: &Device, app: &App, f: &Fonts, theme: &Theme, l: &Layout, page: usize, w: f32, h: f32, a: f32, local: f64) {
    let gpu = &device.gpu;
    let s = app.scale;
    let c = |col: D2D1_COLOR_F| fade(col, a);
    // Elements arrive one after another: a gentle rise and fade, staggered.
    let rise = |i: usize| art::ease(art::phase(local, 0.08 * i as f64, 0.5));
    let lifted = |r: D2D_RECT_F, i: usize| {
        let k = rise(i);
        (rect(r.left, r.top + (1.0 - k) * 14.0 * s, r.right, r.bottom + (1.0 - k) * 14.0 * s), k * a)
    };
    match page {
        WELCOME => {
            let k = art::pop(art::phase(local, 0.1, 0.7));
            let size = 128.0 * s * (0.86 + 0.14 * k);
            let cx = w / 2.0;
            let top = 92.0 * s;
            let icon = rect(cx - size / 2.0, top + (128.0 * s - size) / 2.0, cx + size / 2.0, top + (128.0 * s + size) / 2.0);
            if let Some(logo) = &app.logo {
                shadow(gpu, inflate(icon, -10.0 * s), 28.0 * s, s, 18.0, 10.0, 0.22 * a * k);
                unsafe {
                    gpu.d2d.DrawBitmap(logo, Some(&icon), a * k.min(1.0), D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
            }
            let (r, alpha) = lifted(rect(60.0 * s, 248.0 * s, w - 60.0 * s, 300.0 * s), 3);
            text(gpu, &f.hero, "Добро пожаловать в Kadr", r, fade(theme.text, alpha));
            let (r, alpha) = lifted(rect(120.0 * s, 304.0 * s, w - 120.0 * s, 380.0 * s), 4);
            text(
                gpu,
                &f.lead_center,
                "Быстрые и красивые скриншоты для Windows — с рисованием, цензурой, переводом и записью видео. Сейчас покажем, что он умеет, и установим за минуту.",
                r,
                fade(theme.text_muted, alpha),
            );
        }
        p if p <= TOUR => {
            let i = p - 1;
            let (title, body, keys) = FEATURES[i];
            // The scene card.
            let (card, ca) = lifted(l.art, 0);
            shadow(gpu, card, 18.0 * s, s, 12.0, 6.0, 0.14 * ca);
            fill(gpu, fade(theme.group, ca), card, 18.0 * s);
            stroke(gpu, fade(theme.hairline, 0.6 * ca), inflate(card, -0.5), 18.0 * s, 1.0);
            let scene = art::Scene {
                gpu,
                shared: &device.shared,
                theme,
                ink: art::Ink { small: &f.art_small, bold: &f.art_bold, center: &f.art_center },
                r: inflate(card, -14.0 * s),
                s,
                t: local,
                alpha: ca,
            };
            unsafe {
                gpu.d2d.PushAxisAlignedClip(&card, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            match i {
                0 => scene.select(),
                1 => scene.draw(),
                2 => {
                    scene.censor();
                }
                3 => scene.translate(),
                4 => scene.record(),
                _ => scene.thumbnail(),
            }
            unsafe {
                gpu.d2d.PopAxisAlignedClip();
            }
            // The words.
            let x = l.art.right + 40.0 * s;
            let (r, alpha) = lifted(rect(x, 92.0 * s, w - 44.0 * s, 112.0 * s), 1);
            text(gpu, &f.caption, &format!("{} ИЗ {}", i + 1, TOUR), r, fade(theme.accent, alpha));
            // Title, text and keys follow each other by their real heights.
            let right = w - 44.0 * s;
            let title_h = text_height(device, &f.title, title, right - x);
            let (r, alpha) = lifted(rect(x, 116.0 * s, right, 116.0 * s + title_h), 2);
            text(gpu, &f.title, title, r, fade(theme.text, alpha));
            let top = 116.0 * s + title_h + 12.0 * s;
            let body_h = text_height(device, &f.lead, body, right - x);
            let (r, alpha) = lifted(rect(x, top, right, top + body_h), 3);
            text(gpu, &f.lead, body, r, fade(theme.text_muted, alpha));
            // Key hints as little keycaps, wrapping to a second row when needed.
            let (mut cx, mut cy) = (x, top + body_h + 22.0 * s);
            for (n, key) in keys.iter().enumerate() {
                let kw = (key.chars().count() as f32 * 7.6 + 24.0) * s;
                if cx + kw > w - 44.0 * s {
                    cx = x;
                    cy += 36.0 * s;
                }
                let k = art::pop(art::phase(local, 0.35 + 0.08 * n as f64, 0.45));
                let chip = rect(cx, cy + (1.0 - k) * 8.0 * s, cx + kw, cy + 28.0 * s + (1.0 - k) * 8.0 * s);
                let ka = (a * k).clamp(0.0, 1.0);
                fill(gpu, fade(theme.surface, ka), chip, 7.0 * s);
                stroke(gpu, fade(theme.hairline, ka), chip, 7.0 * s, 1.0);
                fill(gpu, fade(theme.hairline, 0.5 * ka), rect(chip.left + 4.0 * s, chip.bottom - 2.0 * s, chip.right - 4.0 * s, chip.bottom - 1.0 * s), 1.0);
                text(gpu, &f.chip, key, chip, fade(theme.text, ka));
                cx += kw + 8.0 * s;
            }
        }
        READY => {
            let (r, alpha) = lifted(rect(40.0 * s, 64.0 * s, w - 80.0 * s, 104.0 * s), 0);
            text(gpu, &f.title, "Одно разрешение — и готово", r, fade(theme.text, alpha));
            let (r, alpha) = lifted(rect(40.0 * s, 110.0 * s, w - 80.0 * s, 136.0 * s), 1);
            text(gpu, &f.lead, "Windows попросит разрешение администратора. Оно нужно один раз, чтобы:", r, fade(theme.text_muted, alpha));
            let reasons = [
                "Kadr работал поверх любых окон — даже Диспетчера задач",
                "подписать Kadr сертификатом, созданным прямо на этом компьютере",
                "положить Kadr в Program Files и докачать распознавание текста Windows",
            ];
            for (n, reason) in reasons.iter().enumerate() {
                let y = 150.0 * s + n as f32 * 34.0 * s;
                let (row, alpha) = lifted(rect(40.0 * s, y, w - 40.0 * s, y + 30.0 * s), 2 + n);
                let dot = rect(row.left, row.top + 3.0 * s, row.left + 24.0 * s, row.top + 27.0 * s);
                fill(gpu, fade(theme.accent, 0.16 * alpha), dot, 12.0 * s);
                tick(device, ((dot.left + dot.right) / 2.0, (dot.top + dot.bottom) / 2.0), 12.0 * s, art::ease(art::phase(local, 0.3 + 0.12 * n as f64, 0.4)), fade(theme.accent, alpha), 2.0 * s);
                text(gpu, &f.small_bold, reason, rect(row.left + 36.0 * s, row.top, row.right, row.bottom), fade(theme.text, alpha));
            }
            // Options.
            let options = [
                ("Запускать вместе с Windows", "Kadr будет ждать PrtSc сразу после входа"),
                ("Скачать распознавание текста", "Английское и русское, если их нет. Нужен интернет"),
            ];
            let group = rect(l.toggles[0].left, l.toggles[0].top, l.toggles[1].right, l.toggles[1].bottom);
            let (g, alpha) = lifted(group, 5);
            fill(gpu, fade(theme.group, alpha), g, 14.0 * s);
            stroke(gpu, fade(theme.hairline, 0.7 * alpha), inflate(g, -0.5), 14.0 * s, 1.0);
            for (n, (label, hint)) in options.iter().enumerate() {
                let r = l.toggles[n];
                let r = rect(r.left, r.top + (g.top - group.top), r.right, r.bottom + (g.top - group.top));
                if n > 0 {
                    fill(gpu, fade(theme.hairline, 0.6 * alpha), rect(r.left + 16.0 * s, r.top, r.right, r.top + 1.0), 0.0);
                }
                let hover = app.glow.iter().find(|(k, _)| *k == Hit::Toggle(n)).map_or(0.0, |(_, sp)| sp.value(now(app)));
                fill(gpu, fade(theme.tint, 0.6 * hover * alpha), inflate(r, -3.0 * s), 10.0 * s);
                text(gpu, &f.small_bold, label, rect(r.left + 18.0 * s, r.top + 6.0 * s, r.right - 80.0 * s, r.top + 30.0 * s), fade(theme.text, alpha));
                text(gpu, &f.small, hint, rect(r.left + 18.0 * s, r.top + 30.0 * s, r.right - 80.0 * s, r.bottom), fade(theme.text_muted, alpha));
                let on = app.knobs[n].value(now(app));
                let track = rect(r.right - 62.0 * s, r.top + 16.0 * s, r.right - 18.0 * s, r.top + 40.0 * s);
                fill(gpu, fade(mix(theme.track, theme.accent, on.clamp(0.0, 1.0)), alpha), track, 12.0 * s);
                let d = 20.0 * s;
                let x = track.left + 2.0 * s + (track.right - track.left - d - 4.0 * s) * on;
                let knob = rect(x, track.top + 2.0 * s, x + d, track.top + 2.0 * s + d);
                shadow(gpu, knob, d / 2.0, s, 2.0, 1.0, 0.18 * alpha);
                fill(gpu, fade(rgb(0xffffff, 1.0), alpha), knob, d / 2.0);
            }
            if app.declined {
                text(
                    gpu,
                    &f.small,
                    "Без разрешения установить не получится — нажмите «Установить» ещё раз и выберите «Да».",
                    rect(40.0 * s, l.primary.top - 30.0 * s, w - 40.0 * s, l.primary.top - 8.0 * s),
                    c(rgb(0xff9f0a, 1.0)),
                );
            }
        }
        INSTALLING => {
            let (r, alpha) = lifted(rect(40.0 * s, 64.0 * s, w - 80.0 * s, 104.0 * s), 0);
            text(gpu, &f.title, "Устанавливаю Kadr…", r, fade(theme.text, alpha));
            // Overall progress.
            let bar = rect(40.0 * s, 116.0 * s, w - 40.0 * s, 122.0 * s);
            fill(gpu, c(theme.track), bar, 3.0 * s);
            let p = app.progress.value(now(app)).clamp(0.0, 1.0);
            fill(gpu, c(theme.accent), rect(bar.left, bar.top, bar.left + (bar.right - bar.left) * p.max(0.02), bar.bottom), 3.0 * s);
            let list = rect(40.0 * s, 138.0 * s, w - 40.0 * s, 138.0 * s + STEPS.len() as f32 * 46.0 * s);
            let (g, alpha) = lifted(list, 1);
            fill(gpu, fade(theme.group, alpha), g, 14.0 * s);
            stroke(gpu, fade(theme.hairline, 0.7 * alpha), inflate(g, -0.5), 14.0 * s, 1.0);
            let t = now(app);
            for (n, (_, title, note)) in STEPS.iter().enumerate() {
                let row = rect(g.left, g.top + n as f32 * 46.0 * s, g.right, g.top + (n + 1) as f32 * 46.0 * s);
                if n > 0 {
                    fill(gpu, fade(theme.hairline, 0.5 * alpha), rect(row.left + 52.0 * s, row.top, row.right, row.top + 1.0), 0.0);
                }
                let center = (row.left + 28.0 * s, (row.top + row.bottom) / 2.0);
                let step = app.steps.get(n).cloned().unwrap_or(Step::Waiting);
                let (ink, sub) = match &step {
                    Step::Waiting => {
                        unsafe {
                            gpu.brush.SetColor(&fade(theme.track, alpha));
                            gpu.d2d.DrawEllipse(&D2D1_ELLIPSE { point: Vector2::new(center.0, center.1), radiusX: 9.0 * s, radiusY: 9.0 * s }, &gpu.brush, 1.6 * s, None);
                        }
                        (theme.text_muted, String::new())
                    }
                    Step::Running(_) => {
                        spinner(device, center, 9.0 * s, t, fade(theme.accent, alpha), 2.2 * s);
                        (theme.text, note.to_string())
                    }
                    Step::Done(since) => {
                        let k = art::pop(art::phase(t, *since, 0.45));
                        let r = 11.0 * s * (0.6 + 0.4 * k);
                        fill(gpu, fade(art::GREEN, alpha), rect(center.0 - r, center.1 - r, center.0 + r, center.1 + r), r);
                        tick(device, center, 13.0 * s, art::ease(art::phase(t, *since + 0.1, 0.35)), fade(rgb(0xffffff, 1.0), alpha), 2.2 * s);
                        (theme.text, String::new())
                    }
                    Step::Skipped => {
                        fill(gpu, fade(theme.track, alpha), rect(center.0 - 6.0 * s, center.1 - 1.0 * s, center.0 + 6.0 * s, center.1 + 1.0 * s), 1.0 * s);
                        (theme.text_muted, "пропущено".to_string())
                    }
                    Step::Warn(text, since) => {
                        let k = art::pop(art::phase(t, *since, 0.45));
                        let r = 11.0 * s * (0.6 + 0.4 * k);
                        fill(gpu, fade(rgb(0xff9f0a, 1.0), alpha), rect(center.0 - r, center.1 - r, center.0 + r, center.1 + r), r);
                        glass::text(gpu, &f.chip, "!", rect(center.0 - r, center.1 - r, center.0 + r, center.1 + r), fade(rgb(0xffffff, 1.0), alpha));
                        (theme.text, format!("не получилось: {}", text.chars().take(90).collect::<String>()))
                    }
                };
                let label = rect(row.left + 52.0 * s, row.top, row.right - 16.0 * s, row.bottom);
                if sub.is_empty() {
                    text(gpu, &f.small_bold, title, label, fade(ink, alpha));
                } else {
                    text(gpu, &f.small_bold, title, rect(label.left, label.top + 3.0 * s, label.right, label.top + 25.0 * s), fade(ink, alpha));
                    text(gpu, &f.small, &sub, rect(label.left, label.top + 24.0 * s, label.right, label.bottom), fade(theme.text_muted, alpha));
                }
            }
        }
        _ => {
            // Done, or failed.
            let failed = app.failed.as_deref();
            let k = art::pop(art::phase(local, 0.05, 0.6));
            let center = (w / 2.0, 150.0 * s);
            let r = 44.0 * s * (0.7 + 0.3 * k);
            let badge = rect(center.0 - r, center.1 - r, center.0 + r, center.1 + r);
            let color = if failed.is_some() { rgb(0xff3b30, 1.0) } else { art::GREEN };
            shadow(gpu, badge, r, s, 14.0, 8.0, 0.22 * a);
            fill(gpu, c(color), badge, r);
            if failed.is_some() {
                glass::text(gpu, &f.hero, "!", badge, c(rgb(0xffffff, 1.0)));
            } else {
                tick(device, center, 48.0 * s, art::ease(art::phase(local, 0.35, 0.5)), c(rgb(0xffffff, 1.0)), 6.0 * s);
            }
            let (title, body) = match failed {
                Some(e) => ("Не получилось установить", e.to_string()),
                None => (
                    "Kadr установлен",
                    format!(
                        "Значок Kadr уже в трее, рядом с часами. Нажмите PrtSc — и выделите область.\n{}",
                        if app.options[0] {
                            "Kadr будет запускаться вместе с Windows, а если закроете — он в «Пуск» → Kadr."
                        } else {
                            "Найти Kadr можно в «Пуск» → Kadr."
                        }
                    ),
                ),
            };
            let (r1, a1) = lifted(rect(60.0 * s, 222.0 * s, w - 60.0 * s, 272.0 * s), 3);
            text(gpu, &f.hero, title, r1, fade(theme.text, a1));
            let (r2, a2) = lifted(rect(110.0 * s, 280.0 * s, w - 110.0 * s, 380.0 * s), 4);
            text(gpu, &f.lead_center, &body, r2, fade(theme.text_muted, a2));
            if failed.is_none() && app.warned() {
                let (r3, a3) = lifted(rect(110.0 * s, 384.0 * s, w - 110.0 * s, 430.0 * s), 5);
                text(
                    gpu,
                    &f.lead_center,
                    "Распознавание текста не скачалось — без него не работают «Текст», «Перевод» и «Цензура». Подключите интернет и запустите установщик ещё раз.",
                    r3,
                    fade(rgb(0xff9f0a, 1.0), a3),
                );
            }
        }
    }
    let _ = h;
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = || unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    let point = || ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
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
            // The top strip drags the window, except over the close button.
            let caption = APP.with(|a| {
                a.try_borrow().is_ok_and(|g| {
                    g.as_ref().is_some_and(|app| (p.y as f32) < 56.0 * app.scale && hit(app, p.x as f32, p.y as f32).is_none())
                })
            });
            return LRESULT(if caption { HTCAPTION as isize } else { HTCLIENT as isize });
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            APP.with(|a| {
                if let Ok(mut g) = a.try_borrow_mut()
                    && let Some(app) = g.as_mut()
                {
                    let h = hit(app, x, y);
                    app.set_hover(h);
                }
            });
            let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
            redraw();
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            APP.with(|a| {
                if let Ok(mut g) = a.try_borrow_mut()
                    && let Some(app) = g.as_mut()
                {
                    app.set_hover(None);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            // Actions may show the UAC prompt or close the window: run them outside the borrow
            // where they call back into Windows.
            let mut close_now = false;
            APP.with(|a| {
                let Ok(mut g) = a.try_borrow_mut() else { return };
                let Some(app) = g.as_mut() else { return };
                match hit(app, x, y) {
                    Some(Hit::Close) => close_now = true,
                    Some(Hit::Primary) => app.primary(),
                    Some(Hit::Secondary) => app.secondary(),
                    Some(Hit::Dot(i)) => app.go(i + 1),
                    Some(Hit::Toggle(i)) => {
                        app.options[i] ^= true;
                        let (v, t) = (f32::from(app.options[i]), now(app));
                        app.knobs[i].target_with(v, Curve::POP, t);
                    }
                    None => {}
                }
            });
            if close_now {
                close(hwnd);
            }
            redraw();
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let key = wp.0 as u16;
            APP.with(|a| {
                let Ok(mut g) = a.try_borrow_mut() else { return };
                let Some(app) = g.as_mut() else { return };
                if key == VK_RETURN.0 || (key == VK_RIGHT.0 && app.page <= TOUR) {
                    app.primary();
                } else if key == VK_LEFT.0 && (1..=READY).contains(&app.page) {
                    app.go(app.page - 1);
                } else if key == VK_ESCAPE.0 && app.page != INSTALLING {
                    close(hwnd);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_CLOSE => {
            // Closing mid-install would leave a half-done install: wait for it.
            let installing = APP.with(|a| a.try_borrow().is_ok_and(|g| g.as_ref().is_some_and(|app| app.job.is_some())));
            if !installing {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            APP.with(|a| {
                if let Ok(mut g) = a.try_borrow_mut()
                    && let Some(app) = g.as_mut()
                {
                    app.scale = (wp.0 & 0xffff) as f32 / 96.0;
                    app.fonts = None;
                }
            });
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            glass::match_appearance(hwnd);
            redraw();
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// `kadr-setup --snapshots <folder>`: every page rendered offscreen at a telling moment, for
/// reviewing the design without clicking through.
fn snapshots(folder: &std::path::Path, scale: f32) -> windows::core::Result<()> {
    let device = Device::new("setup-snapshots")?;
    fonts(&device.shared, scale).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    let logo = logo(&device);
    let (w, h) = ((WIDTH * scale) as u32, (HEIGHT * scale) as u32);
    let _ = std::fs::create_dir_all(folder);
    // (name, page, seconds into the page)
    let shots: [(&str, usize, f64); 14] = [
        ("01-welcome", WELCOME, 2.0),
        ("02-select", 1, 1.9),
        ("03-draw", 2, 3.2),
        ("04-censor-light", 3, 1.9),
        ("05-censor-hard", 3, 4.3),
        ("06-censor-full", 3, 6.7),
        ("07-translate", 4, 3.6),
        ("08-record", 5, 3.2),
        ("09-record-done", 5, 5.3),
        ("10-thumbnail", 6, 2.2),
        ("11-ready", READY, 2.0),
        ("12-installing", INSTALLING, 2.0),
        ("13-done", DONE, 2.0),
        ("14-failed", DONE, 2.0),
    ];
    for (name, page, at) in shots {
        let spring = |c: Curve| Spring::with(0.0, c).precise(0.001);
        let mut app = App {
            hwnd: HWND::default(),
            device: None,
            surface: None,
            fonts: None,
            logo: logo.clone(),
            scale,
            epoch: Instant::now(),
            page,
            slide: spring(Curve::LAYOUT),
            entered: [-at; PAGES],
            hover: None,
            glow: Vec::new(),
            options: [true, true],
            knobs: [spring(Curve::POP), spring(Curve::POP)],
            payload: Err(String::new()),
            job: None,
            steps: vec![Step::Waiting; STEPS.len()],
            declined: false,
            failed: (name == "14-failed").then(|| "Подпись не удалась: сертификат недоступен.".to_string()),
            progress: spring(Curve::GLIDE),
        };
        app.fonts = Some(fonts(&device.shared, scale).unwrap());
        app.slide.snap(page as f32);
        app.knobs.iter_mut().for_each(|k| k.snap(1.0));
        if page == INSTALLING {
            app.steps = vec![
                Step::Done(-3.0),
                Step::Done(-2.0),
                Step::Done(-1.0),
                Step::Done(-0.5),
                Step::Warn("Нет связи с Центром обновления".into(), -0.3),
                Step::Running(0.0),
                Step::Waiting,
            ];
            app.progress.snap(5.0 / 7.0);
        }
        let image = glass::render_offscreen(&device, w, h, || {
            let theme = Theme::current();
            fill(&device.gpu, if theme.dark { rgb(0x202022, 1.0) } else { rgb(0xeef0f4, 1.0) }, rect(0.0, 0.0, w as f32, h as f32), 0.0);
            paint(&device, &app, (w, h));
        })?;
        let png = kadr_doc::pixels::encode_png(&image).map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let _ = std::fs::write(folder.join(format!("{name}.png")), png);
    }
    Ok(())
}
