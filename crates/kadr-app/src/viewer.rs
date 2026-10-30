//! The capture opened from the corner thumbnail, Quick Look style: the picture grows out of the
//! thumbnail into the middle of the screen over a soft dim, with a small toolbar under it to draw,
//! blur, copy or save. Closing sends it back into the corner, where the thumbnail stays a few
//! seconds more — with whatever was drawn.
use crate::glass::{
    self, Device, Surface, client_size, fade, fill, inflate, inside, mix, rect, rgb, shadow, stroke, text,
};
use crate::thumbnail::Shot;
use kadr_doc::annotation::{COLORS, Tool};
use kadr_doc::{Image, Point, Rect};
use kadr_editor::drawing::Drawing;
use kadr_render::annotations::{self, Cache, View};
use kadr_render::motion::{Curve, Spring};
use kadr_render::scene::{ToolbarButton, icon};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_ROUNDED_RECT,
    ID2D1Bitmap1, ID2D1Geometry, ID2D1Layer,
};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::Interface;
use windows::core::{PCWSTR, w};
use windows_numerics::Matrix3x2;

pub enum Request {
    Copy(Arc<Image>),
    Save(Arc<Image>),
    /// Back in the corner: the shot as it now is, and whether anything was drawn.
    Closed(Shot, bool),
}

const TOOLS: [Tool; 10] = [
    Tool::Select,
    Tool::Arrow,
    Tool::Rectangle,
    Tool::Ellipse,
    Tool::Pen,
    Tool::Eraser,
    Tool::Lens,
    Tool::Text,
    Tool::Counter,
    Tool::Blur,
];
const BAR_H: f32 = 52.0;
const BUTTON: f32 = 36.0;
const SWATCH: f32 = 26.0;
const GAP: f32 = 16.0;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Tool(usize),
    Color(usize),
    Undo,
    Crop,
    Copy,
    Save,
    Close,
}

struct State {
    hwnd: HWND,
    host: HWND,
    message: u32,
    device: Option<Device>,
    surface: Option<Surface>,
    fonts: Option<(IDWriteTextFormat, IDWriteTextFormat)>,
    bitmap: Option<ID2D1Bitmap1>,
    cache: Cache,
    scale: f32,
    epoch: Instant,
    shot: Shot,
    frames: Vec<Arc<Image>>,
    drawing: Drawing,
    /// The thumbnail's card, in window pixels: where the picture comes from and goes back to.
    from: D2D_RECT_F,
    open: Spring,
    closing: bool,
    pressed: bool,
    hover: Option<Hit>,
    glow: Vec<(Hit, Spring)>,
    revision: u64,
    surrogate: Option<u16>,
    toast: Option<(&'static str, f64)>,
    tracking: bool,
    /// «Обрезать» is on: a drag frames what to keep instead of drawing.
    cropping: bool,
    /// The frame being dragged, in picture pixels.
    crop_drag: Option<(Point, Point)>,
    /// The picture before each crop, with what was drawn on it: «Назад» walks back through them.
    uncropped: Vec<(Arc<Image>, Drawing)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static REQUESTS: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
    static WINDOW: RefCell<Option<HWND>> = const { RefCell::new(None) };
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

impl State {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
    fn crop(&self) -> Rect {
        self.shot.image.rect
    }
    fn image_size(&self) -> (f32, f32) {
        (self.shot.image.width() as f32, self.shot.image.height() as f32)
    }
    /// The picture's place when fully open: as large as fits above the toolbar, never upscaled.
    fn target(&self, w: f32, h: f32) -> D2D_RECT_F {
        let s = self.scale;
        let (iw, ih) = self.image_size();
        let below = (BAR_H + GAP) * s;
        let k = (w * 0.84 / iw).min((h * 0.86 - below) / ih).min(1.0);
        let (cw, ch) = (iw * k, ih * k);
        let top = ((h - ch - below) / 2.0).max(24.0 * s);
        rect((w - cw) / 2.0, top, (w + cw) / 2.0, top + ch)
    }
    fn card(&self, w: f32, h: f32, e: f32) -> D2D_RECT_F {
        let (a, b) = (self.from, self.target(w, h));
        let l = |x: f32, y: f32| x + (y - x) * e;
        rect(l(a.left, b.left), l(a.top, b.top), l(a.right, b.right), l(a.bottom, b.bottom))
    }
    /// Toolbar items under the open picture, left to right.
    fn bar(&self, w: f32, h: f32) -> (D2D_RECT_F, Vec<(Hit, D2D_RECT_F)>) {
        let s = self.scale;
        let copy_w = 112.0 * s;
        let widths: Vec<(Hit, f32)> = (0..TOOLS.len())
            .map(|i| (Hit::Tool(i), BUTTON * s))
            .chain((0..COLORS.len()).map(|i| (Hit::Color(i), SWATCH * s)))
            .chain([
                (Hit::Undo, BUTTON * s),
                (Hit::Crop, BUTTON * s),
                (Hit::Save, BUTTON * s),
                (Hit::Copy, copy_w),
                (Hit::Close, BUTTON * s),
            ])
            .collect();
        let gaps = 3.0 * 12.0 * s;
        let total: f32 = widths.iter().map(|(_, w)| w).sum::<f32>() + gaps + 16.0 * s;
        let card = self.target(w, h);
        let top = card.bottom + GAP * s;
        let bar = rect((w - total) / 2.0, top, (w + total) / 2.0, top + BAR_H * s);
        let mut x = bar.left + 8.0 * s;
        let mut items = Vec::new();
        for (hit, width) in widths {
            if matches!(hit, Hit::Color(0) | Hit::Undo | Hit::Close) {
                x += 12.0 * s;
            }
            let pad = if matches!(hit, Hit::Color(_)) { (BUTTON * s - SWATCH * s) / 2.0 } else { 0.0 };
            let cy = (bar.top + bar.bottom) / 2.0;
            let half = if matches!(hit, Hit::Color(_)) { SWATCH * s / 2.0 } else { BUTTON * s / 2.0 };
            items.push((hit, rect(x, cy - half, x + width, cy + half)));
            let _ = pad;
            x += width;
        }
        (bar, items)
    }
    fn to_doc(&self, x: f32, y: f32) -> Point {
        let (w, h) = client_size(self.hwnd);
        let card = self.target(w as f32, h as f32);
        let k = self.image_size().0 / (card.right - card.left);
        let r = self.crop();
        Point::new(r.left + ((x - card.left) * k).round() as i32, r.top + ((y - card.top) * k).round() as i32)
    }
    /// The crop frame: two corners in any order, inside the picture.
    fn crop_frame(&self) -> Option<Rect> {
        let (a, b) = self.crop_drag?;
        let r = self.crop();
        let (x0, x1) = (a.x.min(b.x).clamp(r.left, r.right), a.x.max(b.x).clamp(r.left, r.right));
        let (y0, y1) = (a.y.min(b.y).clamp(r.top, r.bottom), a.y.max(b.y).clamp(r.top, r.bottom));
        Some(Rect::new(x0, y0, x1, y1))
    }
    /// Keeps only `area`: what is drawn is baked in, and the old picture waits for «Назад».
    fn apply_crop(&mut self, area: Rect) {
        self.cropping = false;
        self.crop_drag = None;
        if area.width() < 4 || area.height() < 4 || area == self.crop() {
            return;
        }
        let Some(device) = self.device.as_ref() else { return };
        let image = match annotations::flatten(&device.gpu, &device.shared, &self.frames, area, self.drawing.shapes()) {
            Ok(image) => Arc::new(image),
            Err(_) => return,
        };
        let mut fresh = Drawing::default();
        fresh.set_tool(self.drawing.tool);
        fresh.set_color(self.drawing.color);
        let drawing = std::mem::replace(&mut self.drawing, fresh);
        let before = std::mem::replace(&mut self.shot.image, image.clone());
        self.uncropped.push((before, drawing));
        self.frames = vec![image];
        self.bitmap = None;
        self.cache = Cache::default();
    }
    fn can_undo(&self) -> bool {
        self.drawing.can_undo() || !self.uncropped.is_empty()
    }
    fn undo(&mut self) {
        if self.drawing.can_undo() {
            self.drawing.undo();
        } else if let Some((image, drawing)) = self.uncropped.pop() {
            self.shot.image = image.clone();
            self.frames = vec![image];
            self.drawing = drawing;
            self.bitmap = None;
            self.cache = Cache::default();
        }
    }
    fn ready(&self) -> bool {
        !self.closing && self.open.value(self.now()) > 0.97
    }
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let (w, h) = client_size(self.hwnd);
        self.bar(w as f32, h as f32).1.into_iter().find(|(_, r)| inside(*r, x, y)).map(|(h, _)| h)
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
    fn flattened(&self) -> Option<Image> {
        let device = self.device.as_ref()?;
        annotations::flatten(&device.gpu, &device.shared, &self.frames, self.crop(), self.drawing.shapes()).ok()
    }
    fn result(&self) -> Arc<Image> {
        if self.drawing.shapes().is_empty() {
            self.shot.image.clone()
        } else {
            self.flattened().map(Arc::new).unwrap_or_else(|| self.shot.image.clone())
        }
    }
    fn press(&mut self, h: Hit) {
        match h {
            Hit::Tool(i) => {
                self.cropping = false;
                self.drawing.set_tool(TOOLS[i]);
            }
            Hit::Color(i) => self.drawing.set_color(i),
            Hit::Undo => self.undo(),
            Hit::Crop => {
                self.drawing.finish_gesture();
                self.drawing.finish_text();
                self.cropping = !self.cropping;
                self.crop_drag = None;
                if self.cropping {
                    self.toast = Some(("Обведите, что оставить", self.now()));
                }
            }
            Hit::Copy => {
                self.drawing.finish_text();
                request(self, Request::Copy(self.result()));
                self.toast = Some(("Скопировано", self.now()));
            }
            Hit::Save => {
                self.drawing.finish_text();
                request(self, Request::Save(self.result()));
                self.toast = Some(("Сохранено в «Изображения»", self.now()));
            }
            Hit::Close => self.close(),
        }
    }
    fn close(&mut self) {
        if self.closing {
            return;
        }
        self.drawing.finish_gesture();
        self.drawing.finish_text();
        self.closing = true;
        let now = self.now();
        self.open.target_with(0.0, Curve::GLIDE, now);
    }
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Opens `shot` growing out of `from` (the thumbnail's card, screen pixels).
pub fn open(instance: HINSTANCE, host: HWND, message: u32, shot: Shot, from: RECT) {
    let hwnd = WINDOW.with(|w| *w.borrow()).or_else(|| {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            hInstance: instance,
            lpfnWndProc: Some(proc),
            lpszClassName: w!("KadrViewer"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            ..Default::default()
        };
        let hwnd = unsafe {
            RegisterClassExW(&class);
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                w!("KadrViewer"),
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
        }
        .ok()?;
        WINDOW.with(|w| *w.borrow_mut() = Some(hwnd));
        Some(hwnd)
    });
    let Some(hwnd) = hwnd else { return };
    // The whole monitor the thumbnail sits on: the dim covers it, a click on the dim closes.
    let monitor = unsafe { MonitorFromPoint(POINT { x: from.left, y: from.top }, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut mi);
        let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
            monitor,
            windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
            &mut dx,
            &mut dy,
        );
    }
    let m = mi.rcMonitor;
    let spring = |c: Curve| Spring::with(0.0, c).precise(0.0015);
    let mut drawing = Drawing::default();
    drawing.set_tool(Tool::Arrow);
    let frames = vec![shot.image.clone()];
    let state = State {
        hwnd,
        host,
        message,
        device: None,
        surface: None,
        fonts: None,
        bitmap: None,
        cache: Cache::default(),
        scale: dx as f32 / 96.0,
        epoch: Instant::now(),
        shot,
        frames,
        drawing,
        from: rect(
            (from.left - m.left) as f32,
            (from.top - m.top) as f32,
            (from.right - m.left) as f32,
            (from.bottom - m.top) as f32,
        ),
        open: spring(Curve::POP),
        closing: false,
        pressed: false,
        hover: None,
        glow: Vec::new(),
        revision: 0,
        surrogate: None,
        toast: None,
        tracking: false,
        cropping: false,
        crop_drag: None,
        uncropped: Vec::new(),
    };
    STATE.with(|s| {
        if let Ok(mut g) = s.try_borrow_mut() {
            let mut state = state;
            state.revision = state.drawing.revision;
            state.open.target(1.0, state.now());
            *g = Some(state);
        }
    });
    unsafe {
        let _ =
            SetWindowPos(hwnd, Some(HWND_TOPMOST), m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
        redraw(hwnd);
    }
}

/// A new capture is starting: away at once, nothing to fly back.
pub fn close_now() -> bool {
    let mut visible = false;
    if let Some(hwnd) = WINDOW.with(|w| *w.borrow()) {
        STATE.with(|s| {
            if let Ok(mut g) = s.try_borrow_mut() {
                *g = None;
            }
        });
        unsafe {
            visible = IsWindowVisible(hwnd).as_bool();
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    visible
}

fn paint_window(hwnd: HWND) {
    let (moving, gone) = STATE.with(|s| {
        let Ok(mut g) = s.try_borrow_mut() else { return (false, false) };
        let Some(st) = g.as_mut() else { return (false, false) };
        if st.device.is_none() {
            st.device = Device::new("viewer").ok();
        }
        let Some(device) = st.device.as_ref() else { return (false, false) };
        if glass::ensure(&mut st.surface, device, hwnd).is_err() {
            return (false, false);
        }
        if st.bitmap.is_none() {
            st.bitmap = annotations::bitmap(&device.gpu, &st.shot.image).ok();
        }
        if st.fonts.is_none() {
            let shared = &device.shared;
            let make = |size: f32| unsafe {
                let f = shared
                    .dwrite
                    .CreateTextFormat(
                        PCWSTR(shared.ui_family.as_ptr()),
                        None,
                        DWRITE_FONT_WEIGHT_SEMI_BOLD,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size * st.scale,
                        w!("ru-ru"),
                    )
                    .ok()?;
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                Some(f)
            };
            st.fonts = make(13.0).zip(make(13.0));
        }
        let now = st.now();
        let gone = st.closing && st.open.value(now) < 0.02 && !st.open.active(now);
        if gone {
            return (false, true);
        }
        let size = client_size(hwnd);
        let st = &mut *st;
        let device = st.device.as_ref().unwrap();
        let surface = st.surface.as_ref().unwrap();
        let mut cache = std::mem::take(&mut st.cache);
        let moving = glass::frame(device, surface, || paint(device, st, size, &mut cache));
        st.cache = cache;
        (moving, false)
    });
    if gone {
        finish(hwnd);
    } else if moving {
        redraw(hwnd);
    }
}

/// Back in the corner: hand the shot (with what was drawn) to the app and step aside.
fn finish(hwnd: HWND) {
    let done = STATE.with(|s| {
        let Ok(mut g) = s.try_borrow_mut() else { return None };
        let st = g.as_ref()?;
        let edited = !st.drawing.shapes().is_empty() || !st.uncropped.is_empty();
        let mut shot = st.shot.clone();
        if edited {
            shot.image = st.result();
            // The saved file shows the old picture now; dragging takes the drawn one.
            shot.path = None;
        }
        let out = Some((st.host, st.message, shot, edited));
        *g = None;
        out
    });
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    if let Some((host, message, shot, edited)) = done {
        REQUESTS.with(|q| q.borrow_mut().push(Request::Closed(shot, edited)));
        unsafe {
            let _ = PostMessageW(Some(host), message, WPARAM(0), LPARAM(0));
        }
    }
}

fn paint(device: &Device, st: &State, size: (u32, u32), cache: &mut Cache) -> bool {
    let gpu = &device.gpu;
    let now = st.now();
    let s = st.scale;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let e = st.open.value(now);
    let ec = e.clamp(0.0, 1.0);
    let glow = |hit: Hit| st.glow.iter().find(|(k, _)| *k == hit).map_or(0.0, |(_, sp)| sp.value(now));
    fill(gpu, rgb(0x000000, 0.42 * ec), rect(0.0, 0.0, w, h), 0.0);
    let card = st.card(w, h, e);
    let radius = 10.0 * s + 4.0 * s * ec;
    shadow(gpu, card, radius, s, 18.0 + 22.0 * ec, 8.0 + 10.0 * ec, 0.28 + 0.12 * ec);
    if let Some(bitmap) = &st.bitmap {
        let (iw, ih) = st.image_size();
        glass::rounded_image(device, bitmap, (iw, ih), card, radius, 1.0, true);
    }
    // Drawings, in the picture's own pixels scaled onto the card.
    let shapes = st.drawing.shapes();
    if !shapes.is_empty() || st.drawing.preview().is_some() {
        let k = (card.right - card.left) / st.image_size().0;
        let origin = Point::new(st.crop().left, st.crop().top);
        // Clipped to the card's rounded shape: a blur repaints the whole picture under it.
        let rounded = D2D1_ROUNDED_RECT { rect: card, radiusX: radius, radiusY: radius };
        let mask = unsafe { device.shared.d2d.CreateRoundedRectangleGeometry(&rounded) }
            .ok()
            .and_then(|g| g.cast::<ID2D1Geometry>().ok());
        let mut layer = D2D1_LAYER_PARAMETERS1 {
            contentBounds: card,
            geometricMask: std::mem::ManuallyDrop::new(mask),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        unsafe {
            gpu.d2d.PushLayer(&layer, None::<&ID2D1Layer>);
            gpu.d2d.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: card.left, M32: card.top });
        }
        let _ = annotations::draw(
            gpu,
            &device.shared,
            origin,
            View {
                shapes,
                preview: st.drawing.preview(),
                frames: &st.frames,
                crop: st.crop(),
                selected: st.drawing.selected_bounds(),
                control: st.drawing.selected_control(),
                text_edit: st.drawing.text_selection(),
            },
            cache,
        );
        if let Some(bounds) = st.drawing.selected_bounds() {
            let area = rect(
                (bounds.left - origin.x) as f32,
                (bounds.top - origin.y) as f32,
                (bounds.right - origin.x) as f32,
                (bounds.bottom - origin.y) as f32,
            );
            let point = st
                .drawing
                .selected_control()
                .map(|p| windows_numerics::Vector2::new((p.x - origin.x) as f32, (p.y - origin.y) as f32));
            let _ = kadr_render::scene::object_controls(
                gpu,
                &device.shared,
                &kadr_render::theme::Theme::current(),
                area,
                point,
                s / k,
                (now, kadr_render::theme::motion_enabled()),
            );
        }
        unsafe {
            gpu.d2d.SetTransform(&Matrix3x2::identity());
            gpu.d2d.PopLayer();
            std::mem::ManuallyDrop::drop(&mut layer.geometricMask);
            std::mem::ManuallyDrop::drop(&mut layer.opacityBrush);
        }
    }
    stroke(gpu, rgb(0xffffff, 0.35 * ec), inflate(card, -0.5), radius, 1.0);
    // The crop frame: everything that goes away is dimmed.
    if let Some(frame) = st.crop_frame() {
        let k = (card.right - card.left) / st.image_size().0;
        let r = st.crop();
        let f = rect(
            card.left + (frame.left - r.left) as f32 * k,
            card.top + (frame.top - r.top) as f32 * k,
            card.left + (frame.right - r.left) as f32 * k,
            card.top + (frame.bottom - r.top) as f32 * k,
        );
        let dim = rgb(0x000000, 0.55);
        fill(gpu, dim, rect(card.left, card.top, card.right, f.top), 0.0);
        fill(gpu, dim, rect(card.left, f.bottom, card.right, card.bottom), 0.0);
        fill(gpu, dim, rect(card.left, f.top, f.left, f.bottom), 0.0);
        fill(gpu, dim, rect(f.right, f.top, card.right, f.bottom), 0.0);
        stroke(gpu, rgb(0xffffff, 0.95), f, 0.0, 1.5 * s);
    }
    // The toolbar rises in once the picture has arrived.
    let bar_a = ((e - 0.55) / 0.45).clamp(0.0, 1.0) * if st.closing { ec } else { 1.0 };
    if bar_a > 0.01 {
        let (bar, items) = st.bar(w, h);
        let lift = (1.0 - bar_a) * 10.0 * s;
        unsafe {
            gpu.d2d.SetTransform(&Matrix3x2::translation(0.0, lift));
        }
        let theme = kadr_render::theme::Theme::current();
        shadow(gpu, bar, BAR_H * s / 2.0, s, 12.0, 6.0, 0.25 * bar_a);
        fill(gpu, fade(theme.surface, 0.96 * bar_a), bar, BAR_H * s / 2.0);
        stroke(gpu, fade(theme.hairline, bar_a), bar, BAR_H * s / 2.0, 1.0);
        let (label_font, _) = st.fonts.as_ref().unwrap();
        for (hit, r) in items {
            let hover = glow(hit);
            match hit {
                Hit::Tool(i) => {
                    let on = !st.cropping && st.drawing.tool == TOOLS[i];
                    if on || hover > 0.01 {
                        let bg = if on { theme.selected } else { fade(theme.tint, hover) };
                        fill(gpu, fade(bg, bar_a), inflate(r, -2.0 * s), 10.0 * s);
                    }
                    let ink = if on { theme.on_selected } else { theme.text };
                    icon(gpu, ToolbarButton::Tool(TOOLS[i]), r, fade(ink, bar_a), s * 0.92);
                }
                Hit::Color(i) => {
                    let [r8, g8, b8] = COLORS[i];
                    let c = glass::rgb(((r8 as u32) << 16) | ((g8 as u32) << 8) | b8 as u32, 1.0);
                    let on = st.drawing.color == i;
                    let k = 1.0 + 0.1 * hover;
                    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                    let half = (r.right - r.left) / 2.0 * 0.72 * k;
                    if on {
                        stroke(
                            gpu,
                            fade(theme.text, bar_a),
                            rect(cx - half - 4.0 * s, cy - half - 4.0 * s, cx + half + 4.0 * s, cy + half + 4.0 * s),
                            half + 4.0 * s,
                            1.6 * s,
                        );
                    }
                    fill(gpu, fade(c, bar_a), rect(cx - half, cy - half, cx + half, cy + half), half);
                    stroke(
                        gpu,
                        fade(theme.hairline, bar_a),
                        rect(cx - half, cy - half, cx + half, cy + half),
                        half,
                        1.0,
                    );
                }
                Hit::Copy => {
                    let r = inflate(r, -2.0 * s);
                    fill(gpu, fade(mix(theme.accent, rgb(0xffffff, 1.0), 0.1 * hover), bar_a), r, 12.0 * s);
                    text(gpu, label_font, "Копировать", r, fade(theme.on_accent, bar_a));
                }
                Hit::Crop => {
                    if st.cropping || hover > 0.01 {
                        let bg = if st.cropping { theme.selected } else { fade(theme.tint, hover) };
                        fill(gpu, fade(bg, bar_a), inflate(r, -2.0 * s), 10.0 * s);
                    }
                    let ink = if st.cropping { theme.on_selected } else { theme.text };
                    icon(gpu, ToolbarButton::Crop, r, fade(ink, bar_a), s * 0.92);
                }
                Hit::Undo | Hit::Save | Hit::Close => {
                    let enabled = hit != Hit::Undo || st.can_undo();
                    if hover > 0.01 && enabled {
                        fill(gpu, fade(theme.tint, hover * bar_a), inflate(r, -2.0 * s), 10.0 * s);
                    }
                    let button = match hit {
                        Hit::Undo => ToolbarButton::Undo,
                        Hit::Save => ToolbarButton::Save,
                        _ => ToolbarButton::Close,
                    };
                    let ink = if enabled { theme.text } else { theme.text_muted };
                    icon(gpu, button, r, fade(ink, bar_a * if enabled { 1.0 } else { 0.5 }), s * 0.92);
                }
            }
        }
        unsafe {
            gpu.d2d.SetTransform(&Matrix3x2::identity());
        }
        // «Скопировано» / «Сохранено» for a moment above the bar.
        if let Some((label, at)) = st.toast {
            let age = (now - at) as f32;
            let a = (age / 0.15).min(1.0) * (1.0 - ((age - 1.4) / 0.3).clamp(0.0, 1.0)) * bar_a;
            if a > 0.01 {
                let tw = (label.chars().count() as f32 * 7.8 + 36.0) * s;
                let ty = bar.top - 44.0 * s - (1.0 - a) * 6.0 * s;
                let pill = rect((w - tw) / 2.0, ty, (w + tw) / 2.0, ty + 32.0 * s);
                shadow(gpu, pill, 16.0 * s, s, 8.0, 3.0, 0.2 * a);
                fill(gpu, fade(rgb(0x1c1c1e, 0.92), a), pill, 16.0 * s);
                text(gpu, label_font, label, pill, fade(rgb(0xffffff, 1.0), a));
            }
        }
    }
    let toast = st.toast.is_some_and(|(_, at)| now - at < 2.0);
    st.open.active(now)
        || st.glow.iter().any(|(_, sp)| sp.active(now))
        || toast
        || st.drawing.typing()
        || (kadr_render::theme::marching_ants()
            && kadr_render::theme::motion_enabled()
            && st.drawing.selected.is_some())
}

fn modifiers() -> (bool, bool) {
    unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0) }
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
        WM_LBUTTONDOWN => {
            let (x, y) = point();
            let mut capture = false;
            with(&mut |st| {
                if !st.ready() {
                    return;
                }
                if let Some(h) = st.hit(x, y) {
                    st.press(h);
                    return;
                }
                let (w, h) = client_size(hwnd);
                if !inside(st.target(w as f32, h as f32), x, y) {
                    // A click on the dim puts the picture back.
                    st.close();
                    return;
                }
                let p = st.to_doc(x, y);
                if st.cropping {
                    st.crop_drag = Some((p, p));
                    st.pressed = true;
                    capture = true;
                    return;
                }
                let mut seed = [0u8; 8];
                let _ = getrandom(&mut seed);
                if st.drawing.down(p, st.crop(), u64::from_le_bytes(seed)) {
                    st.pressed = true;
                    capture = true;
                }
            });
            if capture {
                unsafe {
                    SetCapture(hwnd);
                }
            }
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            let mut track = false;
            with(&mut |st| {
                if st.pressed {
                    let p = st.to_doc(x, y);
                    if let Some((_, end)) = &mut st.crop_drag {
                        *end = p;
                    } else {
                        st.drawing.motion(p, st.crop(), modifiers().1);
                    }
                }
                let h = if st.ready() { st.hit(x, y) } else { None };
                st.set_hover(h);
                track = !st.tracking;
                st.tracking = true;
            });
            if track {
                let mut t = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                    cbSize: size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut t);
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
            let mut release = false;
            with(&mut |st| {
                if st.pressed {
                    st.pressed = false;
                    release = true;
                    let p = st.to_doc(x, y);
                    if let Some((_, end)) = &mut st.crop_drag {
                        *end = p;
                        if let Some(area) = st.crop_frame() {
                            st.apply_crop(area);
                        }
                    } else {
                        st.drawing.up(p, st.crop(), modifiers().1);
                    }
                }
            });
            if release {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONDBLCLK => {
            with(&mut |st| {
                st.drawing.edit_text();
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) as u16 as i16) as i32;
            with(&mut |st| {
                let d = &mut st.drawing;
                if d.tool.is_blur() || d.tool == Tool::Lens {
                    d.set_strength((d.strength as i32 + delta.signum()).clamp(0, 2) as u8);
                } else {
                    d.set_width((d.width as i32 + delta.signum()).clamp(1, 12) as u32);
                }
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let key = wp.0 as u16;
            let (ctrl, shift) = modifiers();
            with(&mut |st| {
                if st.drawing.typing() {
                    if key == 0x1B {
                        st.drawing.finish_text();
                    } else {
                        st.drawing.text_key(key, shift, ctrl);
                    }
                    return;
                }
                match (key, ctrl) {
                    (0x1B, _) if st.cropping => {
                        st.cropping = false;
                        st.crop_drag = None;
                    }
                    (0x1B, _) => st.close(),
                    (0x5A, true) if shift => st.drawing.redo(),
                    (0x5A, true) => st.undo(),
                    (0x59, true) => st.drawing.redo(),
                    (0x43, true) => st.press(Hit::Copy),
                    (0x53, true) => st.press(Hit::Save),
                    (0x2E, _) | (0x08, _) => st.drawing.delete(),
                    _ => {}
                }
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_CHAR => {
            let unit = wp.0 as u16;
            if modifiers().0 {
                return LRESULT(0);
            }
            with(&mut |st| {
                if !st.drawing.typing() {
                    st.surrogate = None;
                    return;
                }
                if (0xD800..=0xDBFF).contains(&unit) {
                    st.surrogate = Some(unit);
                    return;
                }
                let ch = match st.surrogate.take() {
                    Some(high) if (0xDC00..=0xDFFF).contains(&unit) => {
                        char::from_u32(0x10000 + (((high as u32 - 0xD800) << 10) | (unit as u32 - 0xDC00)))
                    }
                    _ => char::from_u32(unit as u32),
                };
                if let Some(ch) = ch.filter(|c| *c != '\r' && *c != '\n') {
                    st.drawing.text_char(ch);
                }
            });
            redraw(hwnd);
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(hwnd, &mut p);
            }
            let mut cursor = IDC_ARROW;
            STATE.with(|s| {
                if let Ok(g) = s.try_borrow()
                    && let Some(st) = g.as_ref()
                    && st.ready()
                {
                    let (w, h) = client_size(hwnd);
                    if st.hit(p.x as f32, p.y as f32).is_some() {
                        cursor = IDC_HAND;
                    } else if inside(st.target(w as f32, h as f32), p.x as f32, p.y as f32) {
                        cursor = match st.drawing.tool {
                            _ if st.cropping => IDC_CROSS,
                            Tool::Select => IDC_ARROW,
                            Tool::Text => IDC_IBEAM,
                            _ => IDC_CROSS,
                        };
                    }
                }
            });
            unsafe {
                SetCursor(LoadCursorW(None, cursor).ok());
            }
            return LRESULT(1);
        }
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == 0 => {
            // Another window took over: finish what was being drawn so nothing hangs mid-stroke.
            with(&mut |st| {
                st.pressed = false;
                st.drawing.finish_gesture();
            });
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn getrandom(buf: &mut [u8]) -> windows::core::Result<()> {
    use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG).ok() }
}

/// Offscreen render for design review: a 1600×900 screen with the picture `e` of the way open,
/// an arrow, a numbered marker and a blur drawn on it.
/// With `crop`, «Обрезать» is on and a frame is being dragged.
pub fn preview(image: Arc<Image>, scale: f32, e: f32, crop: bool) -> windows::core::Result<Image> {
    let (w, h) = ((1600.0 * scale) as u32, (900.0 * scale) as u32);
    let mut drawing = Drawing::default();
    let r = image.rect;
    let at =
        |fx: f32, fy: f32| Point::new(r.left + (r.width() as f32 * fx) as i32, r.top + (r.height() as f32 * fy) as i32);
    for (tool, a, b) in [
        (Tool::Arrow, at(0.2, 0.75), at(0.42, 0.45)),
        (Tool::Counter, at(0.6, 0.35), at(0.68, 0.2)),
        (Tool::Blur, at(0.55, 0.6), at(0.85, 0.8)),
    ] {
        drawing.set_tool(tool);
        drawing.down(a, r, 7);
        drawing.motion(b, r, false);
        drawing.up(b, r, false);
    }
    drawing.set_tool(Tool::Arrow);
    let s = scale;
    let mut st = State {
        hwnd: HWND::default(),
        host: HWND::default(),
        message: 0,
        device: Some(Device::new("viewer-preview")?),
        surface: None,
        fonts: None,
        bitmap: None,
        cache: Cache::default(),
        scale,
        epoch: Instant::now(),
        shot: Shot { image: image.clone(), path: None, anchor: (0, 0) },
        frames: vec![image.clone()],
        drawing,
        from: rect(w as f32 - 268.0 * s, h as f32 - 188.0 * s, w as f32 - 36.0 * s, h as f32 - 58.0 * s),
        open: Spring::with(e, Curve::POP),
        closing: false,
        pressed: false,
        hover: None,
        glow: Vec::new(),
        revision: 0,
        surrogate: None,
        toast: None,
        tracking: false,
        cropping: false,
        crop_drag: None,
        uncropped: Vec::new(),
    };
    if crop {
        st.cropping = true;
        st.crop_drag = Some((at(0.12, 0.18), at(0.74, 0.88)));
    }
    let device = st.device.take().unwrap();
    st.bitmap = annotations::bitmap(&device.gpu, &image).ok();
    let shared = &device.shared;
    let make = |size: f32| unsafe {
        let f = shared
            .dwrite
            .CreateTextFormat(
                PCWSTR(shared.ui_family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * s,
                w!("ru-ru"),
            )
            .ok()?;
        let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        Some(f)
    };
    st.fonts = make(13.0).zip(make(13.0));
    let mut cache = Cache::default();
    glass::render_offscreen(&device, w, h, || {
        // A desktop behind, as the dim falls over it.
        fill(&device.gpu, rgb(0x5a6b85, 1.0), rect(0.0, 0.0, w as f32, h as f32), 0.0);
        paint(&device, &st, (w, h), &mut cache);
    })
}
