//! Floating thumbnail after a capture, as on macOS. It slides in at the bottom-right of the monitor
//! the capture came from and leaves by itself after a few seconds; hover keeps it.
//! - drag it anywhere: a real PNG file is dropped into Telegram, a browser or Explorer;
//! - swipe it right or press ×: dismiss;
//! - click: back to the editor (unsaved capture) or to the file in Explorer (saved one).
use crate::glass::{self, Device, Surface, fade, fill, inflate, inside, rect, rgb, scaled_about, shadow, stroke};
use kadr_doc::Image;
use kadr_render::motion::{Curve, Spring};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT,
};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_NONE, IDropSource, OleInitialize};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use windows_numerics::{Matrix3x2, Vector2};

pub struct Shot {
    pub image: Arc<Image>,
    /// Saved file, if the capture went to disk.
    pub path: Option<PathBuf>,
    /// Draft in «Недавние» that reopens the capture in the editor.
    pub draft: Option<String>,
    /// A point on the monitor the capture came from.
    pub anchor: (i32, i32),
}

pub enum Request {
    Edit(String),
    Reveal(PathBuf),
}

/// Card box and margins in DIPs; the margin leaves room for the shadow and the slide.
const MAX_W: f32 = 232.0;
const MAX_H: f32 = 168.0;
const MARGIN: f32 = 36.0;
const EDGE: f32 = 20.0;
const STAY_MS: u32 = 5000;
const AFTER_HOVER_MS: u32 = 2500;
const TIMER_LEAVE: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;

enum Gesture {
    None,
    Pressed { x: f32, y: f32 },
    Swipe { x: f32 },
}

struct State {
    hwnd: HWND,
    host: HWND,
    message: u32,
    device: Option<Device>,
    surface: Option<Surface>,
    shot: Option<Shot>,
    bitmap: Option<(ID2D1Bitmap1, f32, f32)>,
    scale: f32,
    card: (f32, f32),
    epoch: Instant,
    slide: Spring,
    opacity: Spring,
    hover: Spring,
    close_hover: Spring,
    gesture: Gesture,
    leaving: bool,
    tracking: bool,
    dragging: bool,
}
impl State {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
    fn card_rect(&self) -> D2D_RECT_F {
        let s = self.scale;
        rect(MARGIN * s, MARGIN * s, MARGIN * s + self.card.0, MARGIN * s + self.card.1)
    }
    fn close_rect(&self) -> D2D_RECT_F {
        let c = self.card_rect();
        let s = self.scale;
        rect(c.left - 9.0 * s, c.top - 9.0 * s, c.left + 13.0 * s, c.top + 13.0 * s)
    }
    /// Off to the right, past the screen edge.
    fn away(&self) -> f32 {
        self.card.0 + (MARGIN + EDGE) * self.scale
    }
    fn leave(&mut self) {
        if self.leaving {
            return;
        }
        self.leaving = true;
        let now = self.now();
        let away = self.away();
        self.slide.target_with(away, Curve::LAYOUT, now);
        self.opacity.target_with(0.0, Curve::FADE, now);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_LEAVE);
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Option<Request>> = const { RefCell::new(None) };
}

pub fn take_request() -> Option<Request> {
    REQUEST.with(|r| r.borrow_mut().take())
}

fn request(value: Request) {
    REQUEST.with(|r| *r.borrow_mut() = Some(value));
    if let Some((host, msg)) = STATE.with(|s| s.borrow().as_ref().map(|s| (s.host, s.message))) {
        unsafe {
            let _ = PostMessageW(Some(host), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Card size in pixels: the capture's aspect inside the box, never thinner than a fingertip.
fn card_size(image: (f32, f32), s: f32) -> (f32, f32) {
    let k = (MAX_W / image.0).min(MAX_H / image.1).min(1.0);
    ((image.0 * k).max(56.0) * s, (image.1 * k).max(40.0) * s)
}

pub fn show(instance: HINSTANCE, host: HWND, message: u32, shot: Shot) {
    let created = STATE.with(|s| s.borrow().is_some());
    if !created {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrThumbnail"),
            hCursor: unsafe { LoadCursorW(None, IDC_HAND) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrThumbnail"),
                w!("Kadr"),
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
        let spring = |v: f32, c: Curve| Spring::with(v, c).precise(0.002);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                host,
                message,
                device: None,
                surface: None,
                shot: None,
                bitmap: None,
                scale: 1.0,
                card: (1.0, 1.0),
                epoch: Instant::now(),
                slide: spring(0.0, Curve::POP).precise(0.05),
                opacity: spring(0.0, Curve::FADE),
                hover: spring(0.0, Curve::HOVER_IN),
                close_hover: spring(0.0, Curve::HOVER_IN),
                gesture: Gesture::None,
                leaving: false,
                tracking: false,
                dragging: false,
            })
        });
    }
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(st) = state.as_mut() else { return };
        // Monitor of the capture, its DPI and work area.
        let monitor = unsafe { MonitorFromPoint(POINT { x: shot.anchor.0, y: shot.anchor.1 }, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetMonitorInfoW(monitor, &mut mi);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        st.scale = dx as f32 / 96.0;
        let s = st.scale;
        st.card = card_size((shot.image.width() as f32, shot.image.height() as f32), s);
        let (w, h) = (st.card.0 + 2.0 * MARGIN * s, st.card.1 + 2.0 * MARGIN * s);
        let work = mi.rcWork;
        let x = work.right as f32 - EDGE * s - st.card.0 - MARGIN * s;
        let y = work.bottom as f32 - EDGE * s - st.card.1 - MARGIN * s;
        st.bitmap = None;
        st.shot = Some(shot);
        st.leaving = false;
        st.gesture = Gesture::None;
        let now = st.now();
        let away = st.away();
        st.slide.snap(away);
        st.slide.target_with(0.0, Curve::POP, now);
        st.opacity.snap(0.0);
        st.opacity.target_with(1.0, Curve::FADE, now);
        st.hover.snap(0.0);
        unsafe {
            let _ = SetWindowPos(
                st.hwnd,
                Some(HWND_TOPMOST),
                x.round() as i32,
                y.round() as i32,
                w.round() as i32,
                h.round() as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetTimer(Some(st.hwnd), TIMER_LEAVE, STAY_MS, None);
            let _ = InvalidateRect(Some(st.hwnd), None, false);
        }
    });
}

/// Hides at once, without motion; returns whether it was on screen.
pub fn hide_now() -> bool {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(st) = state.as_mut() else { return false };
        let visible = unsafe { IsWindowVisible(st.hwnd) }.as_bool();
        if visible {
            unsafe {
                let _ = KillTimer(Some(st.hwnd), TIMER_LEAVE);
                let _ = ShowWindow(st.hwnd, SW_HIDE);
            }
            st.leaving = true;
            st.shot = None;
            st.bitmap = None;
        }
        visible
    })
}

fn paint_window(hwnd: HWND) {
    STATE.with(|s| {
        let Ok(mut state) = s.try_borrow_mut() else { return };
        let Some(st) = state.as_mut() else { return };
        if st.device.is_none() {
            match Device::new("thumbnail") {
                Ok(d) => st.device = Some(d),
                Err(e) => {
                    kadr_win::log_warn!("миниатюра: устройство: {e}");
                    return;
                }
            }
        }
        if glass::ensure(&mut st.surface, st.device.as_ref().unwrap(), hwnd).is_err() {
            return;
        }
        if st.bitmap.is_none()
            && let Some(shot) = &st.shot
            && let Ok(b) = kadr_render::annotations::bitmap(&st.device.as_ref().unwrap().gpu, &shot.image)
        {
            st.bitmap = Some((b, shot.image.width() as f32, shot.image.height() as f32));
        }
        let device = st.device.as_ref().unwrap();
        let surface = st.surface.as_ref().unwrap();
        let moving = glass::frame(device, surface, || paint(device, st));
        let gone = st.leaving && !st.slide.active(st.now()) && !st.opacity.active(st.now());
        unsafe {
            if gone {
                let _ = ShowWindow(hwnd, SW_HIDE);
                st.shot = None;
                st.bitmap = None;
            } else if moving {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    });
}

fn paint(device: &Device, st: &State) -> bool {
    let gpu = &device.gpu;
    let now = st.now();
    let s = st.scale;
    let mut moving = false;
    let mut sample = |spring: &Spring| {
        moving |= spring.active(now);
        spring.value(now)
    };
    let slide = sample(&st.slide);
    let opacity = sample(&st.opacity) * if st.dragging { 0.45 } else { 1.0 };
    let hover = sample(&st.hover);
    let close_hover = sample(&st.close_hover);
    let swipe = match st.gesture {
        Gesture::Swipe { x } => x,
        _ => 0.0,
    };
    let card = st.card_rect();
    let radius = 10.0 * s;
    unsafe {
        gpu.d2d.SetTransform(&scaled_about(card, 1.0 + 0.02 * hover, slide + swipe, 0.0));
    }
    shadow(gpu, card, radius, s, 14.0 + 6.0 * hover, 6.0 + 2.0 * hover, 0.30 * opacity);
    if let Some((bitmap, w, h)) = &st.bitmap {
        glass::rounded_image(device, bitmap, (*w, *h), card, radius, opacity, true);
    }
    stroke(gpu, fade(rgb(0x000000, 0.16), opacity), inflate(card, 0.5), radius + 0.5, 1.0);
    stroke(gpu, fade(rgb(0xffffff, 0.45), opacity), inflate(card, -0.5), radius - 0.5, 1.0);
    // × appears on hover at the top-left corner, as on macOS.
    let c = st.close_rect();
    let a = opacity * hover;
    if a > 0.01 {
        let (cx, cy) = ((c.left + c.right) / 2.0, (c.top + c.bottom) / 2.0);
        fill(gpu, fade(glass::mix(rgb(0x3a3a3c, 0.92), rgb(0x1c1c1e, 0.96), close_hover), a), c, 11.0 * s);
        stroke(gpu, fade(rgb(0xffffff, 0.25), a), c, 11.0 * s, 1.0);
        let k = 3.6 * s;
        unsafe {
            gpu.brush.SetColor(&fade(rgb(0xffffff, 1.0), a));
            for (p, q) in [((-k, -k), (k, k)), ((-k, k), (k, -k))] {
                gpu.d2d.DrawLine(
                    Vector2 { X: cx + p.0, Y: cy + p.1 },
                    Vector2 { X: cx + q.0, Y: cy + q.1 },
                    &gpu.brush,
                    1.6 * s,
                    None,
                );
            }
        }
    }
    unsafe {
        gpu.d2d.SetTransform(&Matrix3x2::identity());
    }
    moving || matches!(st.gesture, Gesture::Swipe { .. })
}

/// Saved file, or the capture written to a temporary PNG with a friendly name.
fn file_for_drag(shot: &Shot) -> Option<PathBuf> {
    if let Some(path) = shot.path.as_ref().filter(|p| p.exists()) {
        return Some(path.clone());
    }
    let dir = std::env::temp_dir().join("Kadr");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(kadr_export::file::timestamp_name());
    kadr_export::file::write_png(&path, &shot.image).ok().map(|()| path)
}

/// Hands the PNG to the shell's own drag source; returns whether it was dropped somewhere.
fn drag_out(hwnd: HWND, path: &std::path::Path) -> bool {
    let wide = kadr_win::wide(&path.to_string_lossy());
    unsafe {
        let _ = OleInitialize(None);
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None) else {
            return false;
        };
        let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else {
            return false;
        };
        SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY).is_ok_and(|e| e != DROPEFFECT_NONE)
    }
}

fn point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = || unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    };
    match msg {
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
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        // Only the card is solid; the shadow margin lets clicks through to what is underneath.
        WM_NCHITTEST => {
            let mut p = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
            }
            let on = STATE.with(|s| {
                s.borrow().as_ref().is_some_and(|st| {
                    !st.leaving
                        && (inside(inflate(st.card_rect(), 2.0), p.x as f32, p.y as f32)
                            || (st.hover.goal() > 0.5 && inside(st.close_rect(), p.x as f32, p.y as f32)))
                })
            });
            return LRESULT(if on { HTCLIENT as isize } else { HTTRANSPARENT as isize });
        }
        WM_TIMER if wp.0 == TIMER_LEAVE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut()
                    && st.hover.goal() < 0.5
                {
                    st.leave();
                }
            });
            redraw();
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            let drag = STATE.with(|s| {
                let mut state = s.borrow_mut();
                let st = state.as_mut()?;
                let now = st.now();
                if !st.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                        let _ = KillTimer(Some(hwnd), TIMER_LEAVE);
                    }
                    st.tracking = true;
                    st.hover.target_with(1.0, Curve::HOVER_IN, now);
                }
                let on_close = inside(st.close_rect(), x, y);
                let curve = if on_close { Curve::HOVER_IN } else { Curve::HOVER_OUT };
                st.close_hover.target_with(f32::from(on_close), curve, now);
                match st.gesture {
                    Gesture::Pressed { x: x0, y: y0 } => {
                        let (dx, dy) = (x - x0, y - y0);
                        if dx.hypot(dy) > 6.0 * st.scale {
                            // A rightward start is a swipe; anything else carries the file away.
                            if dx > 0.0 && dx > dy.abs() {
                                st.gesture = Gesture::Swipe { x: dx };
                            } else {
                                st.gesture = Gesture::None;
                                return st.shot.as_ref().and_then(file_for_drag);
                            }
                        }
                    }
                    // Following the hand is done below, from the press origin.
                    Gesture::Swipe { .. } | Gesture::None => {}
                }
                None
            });
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut()
                    && let Gesture::Swipe { .. } = st.gesture
                    && let Some(origin) = SWIPE_ORIGIN.with(|o| o.get())
                {
                    let dx = x - origin;
                    st.gesture = Gesture::Swipe { x: if dx < 0.0 { dx * 0.25 } else { dx } };
                }
            });
            if let Some(path) = drag {
                unsafe {
                    let _ = ReleaseCapture();
                }
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.dragging = true;
                    }
                });
                redraw();
                let dropped = drag_out(hwnd, &path);
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.dragging = false;
                        if dropped {
                            st.leave();
                        }
                    }
                });
            }
            redraw();
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lp);
            SWIPE_ORIGIN.with(|o| o.set(Some(x)));
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.gesture = Gesture::Pressed { x, y };
                }
            });
            unsafe {
                SetCapture(hwnd);
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let (x, y) = point(lp);
            let action = STATE.with(|s| {
                let mut state = s.borrow_mut();
                let st = state.as_mut()?;
                let gesture = std::mem::replace(&mut st.gesture, Gesture::None);
                match gesture {
                    Gesture::Swipe { x: dx } => {
                        let now = st.now();
                        if dx > 50.0 * st.scale {
                            // Continue from where the hand let go.
                            st.slide.snap(dx);
                            st.leave();
                        } else {
                            st.slide.snap(dx);
                            st.slide.target_with(0.0, Curve::POP, now);
                        }
                        None
                    }
                    Gesture::Pressed { .. } if st.hover.goal() > 0.5 && inside(st.close_rect(), x, y) => {
                        st.leave();
                        None
                    }
                    Gesture::Pressed { .. } => {
                        let shot = st.shot.as_ref()?;
                        let request = match (&shot.draft, &shot.path) {
                            (_, Some(path)) => Request::Reveal(path.clone()),
                            (Some(id), None) => Request::Edit(id.clone()),
                            (None, None) => return None,
                        };
                        st.leave();
                        Some(request)
                    }
                    Gesture::None => None,
                }
            });
            if let Some(r) = action {
                request(r);
            }
            redraw();
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.tracking = false;
                    let now = st.now();
                    st.hover.target_with(0.0, Curve::HOVER_OUT, now);
                    st.close_hover.target_with(0.0, Curve::HOVER_OUT, now);
                    if !st.leaving {
                        unsafe {
                            SetTimer(Some(hwnd), TIMER_LEAVE, AFTER_HOVER_MS, None);
                        }
                    }
                }
            });
            redraw();
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

thread_local! {
    /// Pointer x when the button went down; the swipe follows the distance from it.
    static SWIPE_ORIGIN: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// Offscreen render for design review: the card at rest, hovered (× visible), over a plain backdrop.
pub fn preview(image: Arc<Image>, scale: f32) -> windows::core::Result<Image> {
    let device = Device::new("thumbnail-preview")?;
    let card = card_size((image.width() as f32, image.height() as f32), scale);
    let bitmap = kadr_render::annotations::bitmap(&device.gpu, &image)?;
    let rest = |v: f32| Spring::with(v, Curve::FADE);
    let st = State {
        hwnd: HWND::default(),
        host: HWND::default(),
        message: 0,
        device: None,
        surface: None,
        bitmap: Some((bitmap, image.width() as f32, image.height() as f32)),
        shot: None,
        scale,
        card,
        epoch: Instant::now(),
        slide: rest(0.0),
        opacity: rest(1.0),
        hover: rest(1.0),
        close_hover: rest(0.0),
        gesture: Gesture::None,
        leaving: false,
        tracking: false,
        dragging: false,
    };
    let (w, h) = (card.0 + 2.0 * MARGIN * scale, card.1 + 2.0 * MARGIN * scale);
    glass::render_offscreen(&device, w as u32, h as u32, || {
        fill(&device.gpu, rgb(0x2b2d31, 1.0), rect(0.0, 0.0, w, h), 0.0);
        paint(&device, &st);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn card_keeps_the_capture_aspect_inside_the_box_at_each_dpi() {
        for s in [1.0f32, 1.5, 2.0] {
            let (w, h) = card_size((2560.0, 1440.0), s);
            assert!((w / h - 2560.0 / 1440.0).abs() < 0.02);
            assert!(w <= MAX_W * s + 0.5 && h <= MAX_H * s + 0.5);
            let (w, h) = card_size((80.0, 1200.0), s);
            assert!(w >= 56.0 * s - 0.5 && h <= MAX_H * s + 0.5);
            let (w, h) = card_size((60.0, 40.0), s);
            assert_eq!((w, h), (60.0 * s, 40.0 * s), "small captures are not enlarged");
        }
    }
}
