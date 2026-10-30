//! Text bubble next to the recognised text: the translation (or the copied text) on acrylic,
//! with a copy button. It opens with a soft pop, waits with breathing dots, and closes on Esc,
//! on × or as soon as the user clicks elsewhere.
use crate::glass::{self, Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, scaled_about, text};
use kadr_doc::Rect;
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

#[derive(Clone)]
pub enum Content {
    Working(&'static str),
    Translated {
        text: String,
        from: String,
        to: String,
    },
    Copied(String),
    /// A QR code's contents, already in the clipboard; a web link gets «Открыть».
    Qr(String),
    /// A newer Kadr: its version and release notes; the button installs it.
    Update {
        version: String,
        notes: String,
    },
    /// A plain message: its header and text.
    Info(String, String),
    Failed(String),
}

const WIDTH_MIN: f32 = 300.0;
const WIDTH_MAX: f32 = 480.0;
const PAD: f32 = 16.0;
const HEADER: f32 = 22.0;
const TIMER_COPIED: usize = 1;

struct Fonts {
    header: IDWriteTextFormat,
    body: IDWriteTextFormat,
    button: IDWriteTextFormat,
}

struct State {
    hwnd: HWND,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<(f32, Fonts)>,
    content: Content,
    anchor: Rect,
    scale: f32,
    epoch: Instant,
    appear: Spring,
    hover_copy: Spring,
    hover_close: Spring,
    copied: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn language(code: &str) -> String {
    match code.split('-').next().unwrap_or(code) {
        "en" => "Английский".into(),
        "de" => "Немецкий".into(),
        "fr" => "Французский".into(),
        "es" => "Испанский".into(),
        "it" => "Итальянский".into(),
        "pt" => "Португальский".into(),
        "uk" => "Украинский".into(),
        "pl" => "Польский".into(),
        "tr" => "Турецкий".into(),
        "zh" => "Китайский".into(),
        "ja" => "Японский".into(),
        "ko" => "Корейский".into(),
        "ru" => "Русский".into(),
        "" => "Текст".into(),
        other => other.to_uppercase(),
    }
}

fn header(content: &Content) -> String {
    match content {
        Content::Working(what) => (*what).into(),
        Content::Translated { from, to, .. } => format!("{} → {}", language(from), language(to)),
        Content::Copied(_) => "Текст скопирован".into(),
        Content::Qr(_) => "QR-код скопирован".into(),
        Content::Update { version, .. } => format!("Доступна версия {version}"),
        Content::Info(header, _) => header.clone(),
        Content::Failed(_) => "Не получилось".into(),
    }
}

fn body(content: &Content) -> &str {
    match content {
        Content::Working(_) => "",
        Content::Translated { text, .. } | Content::Copied(text) | Content::Qr(text) | Content::Failed(text) => text,
        Content::Update { notes, .. } => notes,
        Content::Info(_, text) => text,
    }
}

fn fonts(st: &mut State) -> Option<&Fonts> {
    let device = st.device.as_ref()?;
    if st.fonts.as_ref().is_none_or(|(s, _)| *s != st.scale) {
        let shared = &device.shared;
        let make = |size: f32, weight: DWRITE_FONT_WEIGHT, wrap: bool, center: bool| unsafe {
            let f = shared
                .dwrite
                .CreateTextFormat(
                    PCWSTR(shared.ui_family.as_ptr()),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * st.scale,
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
            Some(f)
        };
        st.fonts = Some((
            st.scale,
            Fonts {
                header: make(12.5, DWRITE_FONT_WEIGHT_MEDIUM, false, false)?,
                body: make(15.0, DWRITE_FONT_WEIGHT_REGULAR, true, false)?,
                button: make(12.5, DWRITE_FONT_WEIGHT_MEDIUM, false, true)?,
            },
        ));
    }
    st.fonts.as_ref().map(|(_, f)| f)
}

/// Window size for the content: width from the text's natural length, height from wrapping.
fn measure(st: &mut State) -> (f32, f32) {
    let s = st.scale;
    let value: Vec<u16> = body(&st.content).encode_utf16().collect();
    let Some(device) = st.device.as_ref().map(|d| d.shared.dwrite.clone()) else {
        return (WIDTH_MIN * s, 120.0 * s);
    };
    let Some(f) = fonts(st) else { return (WIDTH_MIN * s, 120.0 * s) };
    let natural = unsafe {
        device.CreateTextLayout(&value, &f.body, 10_000.0, 10_000.0).ok().and_then(|l| {
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).ok().map(|_| m.widthIncludingTrailingWhitespace)
        })
    }
    .unwrap_or(0.0);
    let width = (natural + 2.0 * PAD * s).clamp(WIDTH_MIN * s, WIDTH_MAX * s);
    let height = unsafe {
        device.CreateTextLayout(&value, &f.body, width - 2.0 * PAD * s, 10_000.0).ok().and_then(|l| {
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).ok().map(|_| m.height)
        })
    }
    .unwrap_or(0.0);
    let body_h = if value.is_empty() { 22.0 * s } else { height.min(420.0 * s) };
    (width.round(), (PAD * s + HEADER * s + 10.0 * s + body_h + PAD * s).round())
}

/// Places the window under the text (or above it when there is no room), inside the work area.
fn place(st: &mut State) {
    let (w, h) = measure(st);
    let a = st.anchor;
    let monitor = unsafe { MonitorFromPoint(POINT { x: a.left, y: a.top }, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut mi);
    }
    let work = mi.rcWork;
    let gap = (10.0 * st.scale) as i32;
    let (w, h) = (w as i32, h as i32);
    let x = a.left.clamp(work.left + gap, (work.right - w - gap).max(work.left));
    let below = a.bottom + gap;
    let y = if below + h <= work.bottom { below } else { (a.top - gap - h).max(work.top + gap) };
    unsafe {
        let _ = SetWindowPos(st.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
}

fn layout(width: f32, s: f32) -> (D2D_RECT_F, D2D_RECT_F) {
    let close = rect(width - 34.0 * s, 10.0 * s, width - 10.0 * s, 34.0 * s);
    let copy = rect(close.left - 6.0 * s - 104.0 * s, 10.0 * s, close.left - 6.0 * s, 34.0 * s);
    (copy, close)
}

/// Creates the window once; every new job resets it to «working».
pub fn open(instance: HINSTANCE, anchor: Rect, content: Content) {
    let exists = STATE.with(|s| s.borrow().is_some());
    if !exists {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrBubble"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrBubble"),
                w!("Kadr — текст"),
                WS_OVERLAPPED | WS_CAPTION,
                0,
                0,
                300,
                120,
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
        }
        let spring = |c: Curve| Spring::with(0.0, c).precise(0.002);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                device: None,
                surface: None,
                fonts: None,
                content: content.clone(),
                anchor,
                scale: 1.0,
                epoch: Instant::now(),
                appear: spring(Curve::POP),
                hover_copy: spring(Curve::HOVER_IN),
                hover_close: spring(Curve::HOVER_IN),
                copied: false,
            })
        });
    }
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return Default::default() };
        let Some(st) = state.as_mut() else { return };
        let monitor = unsafe { MonitorFromPoint(POINT { x: anchor.left, y: anchor.top }, MONITOR_DEFAULTTONEAREST) };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        st.scale = dx as f32 / 96.0;
        if st.device.is_none() {
            st.device = Device::new("bubble").ok();
        }
        st.anchor = anchor;
        st.content = content;
        st.copied = false;
        let now = st.epoch.elapsed().as_secs_f64();
        st.appear.snap(0.0);
        st.appear.target(1.0, now);
        place(st);
        unsafe {
            glass::match_appearance(st.hwnd);
            let _ = ShowWindow(st.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(st.hwnd);
            let _ = InvalidateRect(Some(st.hwnd), None, false);
        }
    });
}

/// New content for the open bubble; `anchor` moves it next to the text that was found.
pub fn update(content: Content, anchor: Option<Rect>) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return Default::default() };
        let Some(st) = state.as_mut() else { return };
        st.content = content;
        if let Some(a) = anchor {
            st.anchor = a;
        }
        if unsafe { IsWindowVisible(st.hwnd) }.as_bool() {
            place(st);
            unsafe {
                let _ = InvalidateRect(Some(st.hwnd), None, false);
            }
        }
    });
}

pub fn is_open() -> bool {
    STATE.with(|s| {
        s.try_borrow()
            .ok()
            .as_deref()
            .and_then(Option::as_ref)
            .is_some_and(|st| unsafe { IsWindowVisible(st.hwnd) }.as_bool())
    })
}

pub fn close() {
    let hwnd = STATE.with(|s| s.try_borrow().ok().and_then(|st| st.as_ref().map(|st| st.hwnd)));
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn paint_window(hwnd: HWND) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        let Some(device) = st.device.as_ref() else { return };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() {
            return;
        }
        if fonts(st).is_none() {
            return;
        }
        let st = &*st;
        let device = st.device.as_ref().unwrap();
        let surface = st.surface.as_ref().unwrap();
        let moving = glass::frame(device, surface, || paint(device, st));
        if moving {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    });
}

fn paint(device: &Device, st: &State) -> bool {
    let gpu = &device.gpu;
    let f = &st.fonts.as_ref().unwrap().1;
    let theme = Theme::current();
    let now = st.epoch.elapsed().as_secs_f64();
    let s = st.scale;
    let (w, h) = client_size(st.hwnd);
    let (w, h) = (w as f32, h as f32);
    let mut moving = false;
    let mut sample = |spring: &Spring| {
        moving |= spring.active(now);
        spring.value(now)
    };
    let mut breathing = false;
    let appear = sample(&st.appear);
    let full = rect(0.0, 0.0, w, h);
    fill(gpu, theme.wash, full, 0.0);
    unsafe {
        gpu.d2d.SetTransform(&scaled_about(full, 0.97 + 0.03 * appear, 0.0, (1.0 - appear) * 4.0 * s));
    }
    let a = appear.clamp(0.0, 1.0);
    let (copy, close) = layout(w, s);
    let tone = match st.content {
        Content::Failed(_) => glass::error_ink(&theme),
        _ => theme.text_muted,
    };
    text(gpu, &f.header, &header(&st.content), rect(PAD * s, 10.0 * s, copy.left - 8.0 * s, 34.0 * s), fade(tone, a));
    let body_rect = rect(PAD * s, (PAD + HEADER + 10.0) * s, w - PAD * s, h - PAD * s);
    match &st.content {
        Content::Working(_) => {
            // Three breathing dots while recognising and translating.
            breathing = true;
            for i in 0..3 {
                let t = now as f32 * 5.0 - i as f32 * 0.7;
                let pulse = 0.35 + 0.65 * (0.5 + 0.5 * t.sin());
                let x = body_rect.left + 6.0 * s + i as f32 * 14.0 * s;
                let y = body_rect.top + 10.0 * s;
                fill(
                    gpu,
                    fade(theme.text_muted, pulse * a),
                    rect(x - 4.0 * s, y - 4.0 * s, x + 4.0 * s, y + 4.0 * s),
                    4.0 * s,
                );
            }
        }
        content => {
            let value: Vec<u16> = body(content).encode_utf16().collect();
            unsafe {
                gpu.brush.SetColor(&fade(theme.text, a));
                gpu.d2d.DrawText(
                    &value,
                    &f.body,
                    &body_rect,
                    &gpu.brush,
                    windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
    }
    // Copy: only when there is something to copy; turns into a confirmation for a moment.
    if matches!(st.content, Content::Translated { .. } | Content::Copied(_) | Content::Qr(_) | Content::Update { .. }) {
        let hover = sample(&st.hover_copy);
        fill(gpu, fade(mix(theme.tint, theme.tint_strong, hover), a), copy, 8.0 * s);
        let opens = matches!(&st.content, Content::Qr(t) if crate::qr::link(t).is_some());
        let label = if opens {
            "Открыть"
        } else if st.copied {
            "Скопировано"
        } else {
            "Копировать"
        };
        let ink = if st.copied { theme.on_selected } else { theme.text };
        text(gpu, &f.button, label, copy, fade(ink, a));
    }
    let hover = sample(&st.hover_close);
    fill(gpu, fade(theme.tint, a * hover), inflate(close, -1.0), 12.0 * s);
    let (cx, cy, k) = ((close.left + close.right) / 2.0, (close.top + close.bottom) / 2.0, 4.0 * s);
    unsafe {
        gpu.brush.SetColor(&fade(mix(theme.text_muted, theme.text, hover), a));
        for (p, q) in [((-k, -k), (k, k)), ((-k, k), (k, -k))] {
            gpu.d2d.DrawLine(
                windows_numerics::Vector2 { X: cx + p.0, Y: cy + p.1 },
                windows_numerics::Vector2 { X: cx + q.0, Y: cy + q.1 },
                &gpu.brush,
                1.4 * s,
                None,
            );
        }
    }
    moving || breathing
}

fn copy_text(st: &mut State) {
    let value = match &st.content {
        Content::Translated { text, .. } | Content::Copied(text) | Content::Qr(text) => text.clone(),
        _ => return,
    };
    if kadr_export::clipboard::put_text(st.hwnd, &value).is_ok() {
        st.copied = true;
        unsafe {
            SetTimer(Some(st.hwnd), TIMER_COPIED, 1500, None);
        }
    }
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
        // Clicking anywhere else puts it away, like a popover.
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == 'C' as usize => {
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    copy_text(st);
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    let (w, _) = client_size(hwnd);
                    let (copy, close) = layout(w as f32, st.scale);
                    let now = st.epoch.elapsed().as_secs_f64();
                    for (spring, on) in
                        [(&mut st.hover_copy, inside(copy, x, y)), (&mut st.hover_close, inside(close, x, y))]
                    {
                        let curve = if on { Curve::HOVER_IN } else { Curve::HOVER_OUT };
                        spring.target_with(f32::from(on), curve, now);
                    }
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            let mut hide = false;
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    let (w, _) = client_size(hwnd);
                    let (copy, close) = layout(w as f32, st.scale);
                    if inside(close, x, y) {
                        hide = true;
                    } else if inside(copy, x, y) {
                        if matches!(st.content, Content::Update { .. }) {
                            crate::update::install();
                            st.content = Content::Working("Скачиваю обновление…");
                            place(st);
                        } else if let Content::Qr(code) = &st.content
                            && let Some(url) = crate::qr::link(code)
                        {
                            let url = kadr_win::wide(url);
                            unsafe {
                                windows::Win32::UI::Shell::ShellExecuteW(
                                    None,
                                    w!("open"),
                                    PCWSTR(url.as_ptr()),
                                    PCWSTR::null(),
                                    PCWSTR::null(),
                                    SW_SHOWNORMAL,
                                );
                            }
                            hide = true;
                        } else {
                            copy_text(st);
                        }
                    }
                }
            });
            if hide {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            redraw();
            return LRESULT(0);
        }
        // The text area drags the bubble out of the way.
        WM_NCHITTEST => {
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
            }
            let on_button = STATE.with(|s| {
                s.try_borrow().ok().as_deref().and_then(Option::as_ref).is_some_and(|st| {
                    let (w, _) = client_size(hwnd);
                    let (copy, close) = layout(w as f32, st.scale);
                    inside(copy, p.x as f32, p.y as f32) || inside(close, p.x as f32, p.y as f32)
                })
            });
            return LRESULT(if on_button { HTCLIENT as isize } else { HTCAPTION as isize });
        }
        WM_TIMER if wp.0 == TIMER_COPIED => {
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut()
                {
                    st.copied = false;
                }
            });
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_COPIED);
            }
            redraw();
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_names_the_language_and_the_state() {
        assert_eq!(
            header(&Content::Translated { text: "x".into(), from: "en".into(), to: "ru".into() }),
            "Английский → Русский"
        );
        assert_eq!(
            header(&Content::Translated { text: "x".into(), from: "zh-CN".into(), to: "ru".into() }),
            "Китайский → Русский"
        );
        assert_eq!(
            header(&Content::Translated { text: "x".into(), from: "ru".into(), to: "en".into() }),
            "Русский → Английский"
        );
        assert_eq!(header(&Content::Copied("x".into())), "Текст скопирован");
        assert_eq!(body(&Content::Working("Перевожу")), "");
    }
}
