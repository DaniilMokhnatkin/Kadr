//! Recording in progress: a red frame around the area and a small panel under it with a pulsing
//! dot, the time so far and «Отмена / Стоп». The area itself is a hole in the window, so clicks
//! and the wheel reach whatever is being recorded; only the panel takes clicks. Nothing is drawn
//! inside the area, so the frame never ends up in the video.
use crate::glass::{self, Device, Surface, client_size, fade, fill, inside, mix, rect, shadow, stroke, text};
use kadr_doc::Rect;
use kadr_render::motion::{Curve, Spring};
use kadr_render::theme::Theme;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub enum Request {
    /// Keep the video.
    Stop,
    /// Throw it away.
    Cancel,
}

/// Frame margin around the area, panel size and gap, in DIPs.
const MARGIN: f32 = 6.0;
const PANEL_W: f32 = 340.0;
const PANEL_H: f32 = 48.0;
const GAP: f32 = 10.0;

struct State {
    hwnd: HWND,
    host: HWND,
    message: u32,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<(IDWriteTextFormat, IDWriteTextFormat)>,
    scale: f32,
    /// Area in window coordinates, and whether the panel sits above it.
    area: D2D_RECT_F,
    above: bool,
    /// Time recorded so far; none while the encoder is still starting.
    elapsed: Option<Duration>,
    epoch: Instant,
    appear: Spring,
    hover: [Spring; 2],
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Option<Request>> = const { RefCell::new(None) };
}

pub fn take_request() -> Option<Request> {
    REQUEST.with(|r| r.borrow_mut().take())
}

fn panel(st: &State, w: f32, h: f32) -> D2D_RECT_F {
    let s = st.scale;
    let pw = (PANEL_W * s).min(w);
    let x = ((st.area.left + st.area.right - pw) / 2.0).clamp(0.0, (w - pw).max(0.0));
    let y = if st.above { 0.0 } else { h - PANEL_H * s };
    rect(x, y, x + pw, y + PANEL_H * s)
}
fn buttons(p: D2D_RECT_F, s: f32) -> (D2D_RECT_F, D2D_RECT_F) {
    let done = rect(p.right - 96.0 * s, p.top + 8.0 * s, p.right - 8.0 * s, p.bottom - 8.0 * s);
    let cancel = rect(done.left - 88.0 * s, done.top, done.left - 6.0 * s, done.bottom);
    (cancel, done)
}

pub fn open(instance: HINSTANCE, host: HWND, message: u32, area: Rect) {
    if STATE.with(|s| s.borrow().is_none()) {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrRecord"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrRecord"),
                w!("Kadr — запись"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )
        };
        let Ok(hwnd) = hwnd else { return };
        let spring = |c: Curve| Spring::with(0.0, c).precise(0.002);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                host,
                message,
                device: None,
                surface: None,
                fonts: None,
                scale: 1.0,
                area: rect(0.0, 0.0, 1.0, 1.0),
                above: false,
                elapsed: None,
                epoch: Instant::now(),
                appear: spring(Curve::POP),
                hover: [spring(Curve::HOVER_IN), spring(Curve::HOVER_IN)],
            })
        });
    }
    // Compute under the borrow, move the window after it: SetWindowPos sends messages to `proc`.
    let placed = STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return None };
        let st = state.as_mut()?;
        let monitor = unsafe { MonitorFromPoint(POINT { x: area.left, y: area.top }, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetMonitorInfoW(monitor, &mut mi);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        st.scale = dx as f32 / 96.0;
        st.fonts = None;
        let s = st.scale;
        let m = (MARGIN * s) as i32;
        let extra = ((PANEL_H + GAP) * s) as i32;
        let below = area.bottom + m + extra <= mi.rcMonitor.bottom;
        st.above = !below;
        let (left, right) = (area.left - m, area.right + m);
        let (top, bottom) = if below { (area.top - m, area.bottom + m + extra) } else { (area.top - m - extra, area.bottom + m) };
        // The panel may be wider than a narrow area.
        let panel_w = (PANEL_W * s) as i32;
        let (left, right) = if right - left < panel_w {
            let c = (left + right) / 2;
            (c - panel_w / 2, c + panel_w / 2)
        } else {
            (left, right)
        };
        st.area = rect(
            (area.left - left) as f32,
            (area.top - top) as f32,
            (area.right - left) as f32,
            (area.bottom - top) as f32,
        );
        st.elapsed = None;
        let now = st.epoch.elapsed().as_secs_f64();
        st.appear.snap(0.0);
        st.appear.target(1.0, now);
        // Window shape = the frame ring + the panel. HTTRANSPARENT only passes the mouse to windows
        // of this same thread, so the area itself must be a real hole for the wheel to reach the
        // page underneath.
        let (w, h) = ((right - left) as f32, (bottom - top) as f32);
        let ring = glass::inflate(st.area, MARGIN * s);
        let pane = glass::inflate(panel(st, w, h), 18.0 * s);
        Some((st.hwnd, left, top, right - left, bottom - top, ring, st.area, pane))
    });
    let Some((hwnd, x, y, w, h, ring, hole, pane)) = placed else { return };
    unsafe {
        use windows::Win32::Graphics::Gdi::{CombineRgn, CreateRectRgn, DeleteObject, RGN_DIFF, RGN_OR, SetWindowRgn};
        let r = |x: D2D_RECT_F| CreateRectRgn(x.left as i32, x.top as i32, x.right.ceil() as i32, x.bottom.ceil() as i32);
        let (shape, hole, pane) = (r(ring), r(hole), r(pane));
        CombineRgn(Some(shape), Some(shape), Some(hole), RGN_DIFF);
        CombineRgn(Some(shape), Some(shape), Some(pane), RGN_OR);
        let _ = DeleteObject(hole.into());
        let _ = DeleteObject(pane.into());
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        // The window owns the region from here on.
        SetWindowRgn(hwnd, Some(shape), true);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn update(elapsed: Duration) {
    STATE.with(|s| {
        if let Ok(mut guard) = s.try_borrow_mut()
            && let Some(st) = guard.as_mut()
        {
            st.elapsed = Some(elapsed);
            unsafe {
                let _ = InvalidateRect(Some(st.hwnd), None, false);
            }
        }
    });
}

pub fn close() {
    STATE.with(|s| {
        if let Some(st) = s.try_borrow().ok().as_deref().and_then(Option::as_ref) {
            unsafe {
                let _ = ShowWindow(st.hwnd, SW_HIDE);
            }
        }
    });
}

fn paint_window(hwnd: HWND) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        if st.device.is_none() {
            st.device = Device::new("record").ok();
        }
        let Some(device) = st.device.as_ref() else { return };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() {
            return;
        }
        if st.fonts.is_none() {
            let shared = &device.shared;
            let make = |size: f32, center: bool| unsafe {
                let f = shared
                    .dwrite
                    .CreateTextFormat(
                        PCWSTR(shared.ui_family.as_ptr()),
                        None,
                        DWRITE_FONT_WEIGHT_MEDIUM,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size * st.scale,
                        w!("ru-ru"),
                    )
                    .ok()?;
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                if center {
                    let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                }
                Some(f)
            };
            st.fonts = make(13.0, false).zip(make(13.0, true));
        }
        if st.fonts.is_none() {
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

/// Apple's recording red.
fn red() -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    glass::rgb(0xff3b30, 1.0)
}

fn paint(device: &Device, st: &State, size: (u32, u32)) -> bool {
    let gpu = &device.gpu;
    let theme = Theme::current();
    let (label_font, button_font) = st.fonts.as_ref().unwrap();
    let now = st.epoch.elapsed().as_secs_f64();
    let s = st.scale;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let appear = st.appear.value(now);
    // The frame and the dot breathe slowly: the recording is live.
    let breathe = 0.7 + 0.3 * (now as f32 * 2.2).sin();
    let a = appear.clamp(0.0, 1.0);
    let ring = st.area;
    let line = 2.0 * s;
    let outer = rect(ring.left - line, ring.top - line, ring.right + line, ring.bottom + line);
    // Drawn as four bands outside the area, never over recorded pixels.
    let ink = fade(red(), a * breathe);
    for band in [
        rect(outer.left, outer.top, outer.right, ring.top),
        rect(outer.left, ring.bottom, outer.right, outer.bottom),
        rect(outer.left, ring.top, ring.left, ring.bottom),
        rect(ring.right, ring.top, outer.right, ring.bottom),
    ] {
        fill(gpu, ink, band, 0.0);
    }
    // Panel.
    let p = panel(st, w, h);
    let lift = (1.0 - appear) * 8.0 * s * if st.above { -1.0 } else { 1.0 };
    let p = rect(p.left, p.top + lift, p.right, p.bottom + lift);
    shadow(gpu, p, 14.0 * s, s, 10.0, 4.0, 0.25 * a);
    fill(gpu, fade(theme.surface, a), p, 14.0 * s);
    stroke(gpu, fade(theme.hairline, a), p, 14.0 * s, 1.0);
    let (cancel, stop) = buttons(p, s);
    let cy = (p.top + p.bottom) / 2.0;
    let dot = 5.0 * s;
    let label_left = p.left + 16.0 * s + dot * 2.0 + 10.0 * s;
    let (label, tone) = match st.elapsed {
        Some(t) => {
            fill(gpu, fade(red(), a * breathe), rect(p.left + 16.0 * s, cy - dot, p.left + 16.0 * s + 2.0 * dot, cy + dot), dot);
            (clock(t), theme.text)
        }
        None => ("Готовлю запись…".to_string(), theme.text_muted),
    };
    let left = if st.elapsed.is_some() { label_left } else { p.left + 16.0 * s };
    text(gpu, label_font, &label, rect(left, p.top, cancel.left - 8.0 * s, p.bottom), fade(tone, a));
    let hc = st.hover[0].value(now);
    fill(gpu, fade(mix(theme.tint, theme.tint_strong, hc), a), cancel, 9.0 * s);
    text(gpu, button_font, "Отмена", cancel, fade(theme.text, a));
    let hs = st.hover[1].value(now);
    fill(gpu, fade(mix(red(), glass::rgb(0xffffff, 1.0), 0.12 * hs), a), stop, 9.0 * s);
    text(gpu, button_font, "Стоп", stop, fade(glass::rgb(0xffffff, 1.0), a));
    true
}

/// «0:07», «12:45», «1:02:03».
fn clock(t: Duration) -> String {
    let s = t.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn request(r: Request) {
    REQUEST.with(|q| *q.borrow_mut() = Some(r));
    if let Some((host, msg)) = STATE.with(|s| s.try_borrow().ok().as_deref().and_then(Option::as_ref).map(|s| (s.host, s.message))) {
        unsafe {
            let _ = PostMessageW(Some(host), msg, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let point = || ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
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
        // Everything but the panel lets the mouse (and the wheel) through to the page.
        WM_NCHITTEST => {
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            let on = STATE.with(|s| {
                s.try_borrow().ok().as_deref().and_then(Option::as_ref).is_some_and(|st| {
                    let (w, h) = client_size(hwnd);
                    inside(panel(st, w as f32, h as f32), p.x as f32, p.y as f32)
                })
            });
            return LRESULT(if on { HTCLIENT as isize } else { HTTRANSPARENT as isize });
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            STATE.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut()
                    && let Some(st) = guard.as_mut() {
                    let (w, h) = client_size(hwnd);
                    let (cancel, done) = buttons(panel(st, w as f32, h as f32), st.scale);
                    let now = st.epoch.elapsed().as_secs_f64();
                    for (spring, on) in st.hover.iter_mut().zip([inside(cancel, x, y), inside(done, x, y)]) {
                        let curve = if on { Curve::HOVER_IN } else { Curve::HOVER_OUT };
                        spring.target_with(f32::from(on), curve, now);
                    }
                }
            });
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            let r = STATE.with(|s| {
                let Ok(state) = s.try_borrow() else { return Default::default() };
                let st = state.as_ref()?;
                let (w, h) = client_size(hwnd);
                let (cancel, done) = buttons(panel(st, w as f32, h as f32), st.scale);
                if inside(done, x, y) {
                    Some(Request::Stop)
                } else if inside(cancel, x, y) {
                    Some(Request::Cancel)
                } else {
                    None
                }
            });
            if let Some(r) = r {
                request(r);
            }
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
    fn the_clock_reads_like_a_stopwatch() {
        assert_eq!(clock(Duration::from_millis(7400)), "0:07");
        assert_eq!(clock(Duration::from_secs(765)), "12:45");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }

    use windows::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, GetWindowRgn, PtInRegion};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    #[test]
    fn the_area_is_a_hole_so_the_wheel_reaches_the_page() {
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }.unwrap().into();
        let area = Rect::new(300, 200, 900, 700);
        open(instance, HWND::default(), 0, area);
        let hwnd = STATE.with(|s| s.borrow().as_ref().map(|st| st.hwnd)).unwrap();
        let mut origin = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut origin);
        }
        // Region points are in window coordinates.
        let at = |x: i32, y: i32| (x - origin.left, y - origin.top);
        let rgn = unsafe { CreateRectRgn(0, 0, 0, 0) };
        unsafe { GetWindowRgn(hwnd, rgn) };
        let has = |(x, y): (i32, i32)| unsafe { PtInRegion(rgn, x, y) }.as_bool();
        assert!(!has(at(600, 450)), "the middle of the area belongs to the page");
        assert!(!has(at(302, 202)), "so do its corners");
        assert!(has(at(298, 450)), "the frame ring stays ours");
        assert!(has(at(600, 720)), "and so does the panel");
        unsafe {
            let _ = DeleteObject(rgn.into());
        }
        close();
    }
}
