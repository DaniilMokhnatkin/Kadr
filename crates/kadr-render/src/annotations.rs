//! Один рендер аннотаций для оверлея, миниатюры и экспорта. Панелей и рамок в нём нет.
use crate::gpu::{Gpu, Shared, bitmap_props};
use kadr_doc::annotation::{Annotation, TextStyle, Tool};
use kadr_doc::{Image, Point, Rect};
use std::sync::{Arc, Weak};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{Interface, PCWSTR, Result, w};
use windows_numerics::Vector2;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct View<'a> {
    pub shapes: &'a [Annotation],
    pub preview: Option<&'a Annotation>,
    pub frames: &'a [Arc<Image>],
    pub crop: Rect,
    pub selected: Option<Rect>,
    pub control: Option<Point>,
    pub text_edit: Option<(u64, usize, usize)>,
}
#[derive(Default)]
pub struct Cache {
    blur: Option<(Arc<Image>, ID2D1Bitmap1)>,
    lens_source: Option<(Rect, Vec<Weak<Image>>, ID2D1Bitmap1)>,
}
pub(crate) struct CpuBlur {
    shapes: Vec<Annotation>,
    crop: Rect,
    frames: Vec<Arc<Image>>,
    image: Arc<Image>,
}

/// One blurred area, remembered: the shape (its id aside), what it was cut from, and the result.
struct Patch {
    shape: Annotation,
    crop: Rect,
    frames: Vec<Weak<Image>>,
    area: Rect,
    image: Arc<Image>,
    mask: Arc<[f32]>,
}

/// Blurred areas, shared by every thread: the censor fills it on a worker before its blurs are
/// shown, so the frame that shows them only pastes ready pixels.
static PATCHES: std::sync::Mutex<Vec<Patch>> = std::sync::Mutex::new(Vec::new());
const PATCHES_KEPT: usize = 600;
const PATCH_BYTES: usize = 64 * 1024 * 1024;

fn same_frames(cached: &[Weak<Image>], frames: &[Arc<Image>]) -> bool {
    cached.len() == frames.len()
        && cached.iter().zip(frames).all(|(old, frame)| old.upgrade().is_some_and(|old| Arc::ptr_eq(&old, frame)))
}

fn same_blur(a: &Annotation, b: &Annotation) -> bool {
    a.tool == b.tool && a.points == b.points && a.strength == b.strength && a.seed == b.seed && a.width == b.width
}

/// The blurred pixels of `shape` and where they go: from the cache, or computed now.
type MaskedPatch = (Rect, Arc<Image>, Arc<[f32]>);
pub fn blur_patch(frames: &[Arc<Image>], crop: Rect, shape: &Annotation) -> Option<(Rect, Arc<Image>)> {
    cached_patch(frames, crop, shape).map(|(area, image, _)| (area, image))
}
fn cached_patch(frames: &[Arc<Image>], crop: Rect, shape: &Annotation) -> Option<MaskedPatch> {
    if let Ok(cache) = PATCHES.lock()
        && let Some(p) =
            cache.iter().find(|p| p.crop == crop && same_frames(&p.frames, frames) && same_blur(&p.shape, shape))
    {
        return Some((p.area, p.image.clone(), p.mask.clone()));
    }
    let area = if shape.tool == Tool::Spotlight {
        Some(crop)
    } else {
        // The soft edge lies outside the shape: give the blur room for it.
        let b = shape.bounds();
        let d = kadr_doc::privacy::reach(shape);
        kadr_doc::Rect::new(b.left - d, b.top - d, b.right + d, b.bottom + d).intersect(&crop)
    }?;
    let source = Image::compose(frames.iter().map(Arc::as_ref), area);
    let (image, mask) = kadr_doc::privacy::blur_patch_masked(&source, shape);
    let image = Arc::new(image);
    let mask: Arc<[f32]> = mask.into();
    if let Ok(mut cache) = PATCHES.lock() {
        cache.retain(|p| p.frames.iter().all(|frame| frame.strong_count() > 0));
        let incoming = image.bgra.len() + mask.len() * size_of::<f32>();
        let mut used: usize =
            cache.iter().map(|patch| patch.image.bgra.len() + patch.mask.len() * size_of::<f32>()).sum();
        while !cache.is_empty() && (cache.len() >= PATCHES_KEPT || used + incoming > PATCH_BYTES) {
            let removed = cache.remove(0);
            used -= removed.image.bgra.len() + removed.mask.len() * size_of::<f32>();
        }
        if incoming > PATCH_BYTES {
            return Some((area, image, mask));
        }
        cache.push(Patch {
            shape: shape.clone(),
            crop,
            frames: frames.iter().map(Arc::downgrade).collect(),
            area,
            image: image.clone(),
            mask: mask.clone(),
        });
    }
    Some((area, image, mask))
}

/// A complete CPU layer, prepared before the UI starts revealing the censor.
struct Prepared {
    shapes: Vec<Annotation>,
    crop: Rect,
    frames: Vec<Weak<Image>>,
    image: Arc<Image>,
}
static PREPARED: std::sync::Mutex<Vec<Prepared>> = std::sync::Mutex::new(Vec::new());

fn prepared_layer(frames: &[Arc<Image>], crop: Rect, shapes: &[Annotation]) -> Arc<Image> {
    if let Ok(cache) = PREPARED.lock()
        && let Some(layer) = cache.iter().find(|layer| {
            layer.crop == crop
                && same_frames(&layer.frames, frames)
                && layer.shapes.len() == shapes.len()
                && layer.shapes.iter().zip(shapes).all(|(a, b)| same_blur(a, b))
        })
    {
        return layer.image.clone();
    }
    let source = Image::compose(frames.iter().map(Arc::as_ref), crop);
    let mut base = source.clone();
    for shape in shapes {
        if let Some((area, patch, mask)) = cached_patch(frames, crop, shape) {
            for y in area.top..area.bottom {
                let from = ((y - area.top) * area.width() * 4) as usize;
                let to = (((y - base.rect.top) * base.rect.width() + area.left - base.rect.left) * 4) as usize;
                let len = area.width() as usize * 4;
                for x in (0..len).step_by(4) {
                    let alpha = mask[(from + x) / 4];
                    if alpha <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        // patch = original*(1-alpha) + blurred*alpha. Replace original by
                        // the already composed layer; untouched margins must never erase it.
                        let delta = base.bgra[to + x + c] as f32 - source.bgra[to + x + c] as f32;
                        base.bgra[to + x + c] =
                            (patch.bgra[from + x + c] as f32 + delta * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8;
                    }
                    base.bgra[to + x + 3] = 255;
                }
            }
        }
    }
    let image = Arc::new(base);
    if let Ok(mut cache) = PREPARED.lock() {
        cache.retain(|layer| layer.frames.iter().all(|f| f.strong_count() > 0));
        // Full desktop-sized layers are large; keep at most two recent versions.
        if cache.len() >= 2 {
            cache.remove(0);
        }
        cache.push(Prepared {
            shapes: shapes.to_vec(),
            crop,
            frames: frames.iter().map(Arc::downgrade).collect(),
            image: image.clone(),
        });
    }
    image
}

/// Prepares the entire layer (including composition), not just individual patches.
/// Call on a background worker; rendering then uploads ready pixels only.
pub fn prewarm(frames: &[Arc<Image>], crop: Rect, shapes: &[Annotation]) {
    let blurs: Vec<_> = shapes.iter().filter(|s| s.tool.is_blur()).cloned().collect();
    let _ = prepared_layer(frames, crop, &blurs);
}

/// An independent offscreen context owned by the calling worker, never the UI context.
pub fn flatten_background(frames: &[Arc<Image>], crop: Rect, shapes: &[Annotation]) -> Result<Image> {
    if shapes.is_empty() {
        return Ok(Image::compose(frames.iter().map(Arc::as_ref), crop));
    }
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let shared = Shared::new()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let adapter = unsafe { factory.EnumAdapters1(0)? };
    let gpu = Gpu::new(&shared, &adapter, "censor-worker".into())?;
    flatten(&gpu, &shared, frames, crop, shapes)
}

pub fn bitmap(gpu: &Gpu, image: &Image) -> Result<ID2D1Bitmap1> {
    unsafe {
        gpu.d2d.CreateBitmap(
            D2D_SIZE_U { width: image.width(), height: image.height() },
            Some(image.bgra.as_ptr() as *const _),
            image.width() * 4,
            &bitmap_props(D2D1_BITMAP_OPTIONS_NONE),
        )
    }
}
/// Upload a prepared layer before starting its reveal animation.
pub fn prepare_display(
    gpu: &Gpu,
    shared: &Shared,
    frames: &[Arc<Image>],
    crop: Rect,
    shapes: &[Annotation],
    cache: &mut Cache,
) -> Result<()> {
    let blurs: Vec<_> = shapes.iter().filter(|s| s.tool.is_blur()).cloned().collect();
    if blurs.is_empty() {
        cache.blur = None;
        return Ok(());
    }
    let same = shared.blur.borrow().as_ref().is_some_and(|old| {
        old.shapes == blurs
            && old.crop == crop
            && old.frames.len() == frames.len()
            && old.frames.iter().zip(frames).all(|(a, b)| Arc::ptr_eq(a, b))
    });
    if !same {
        let image = prepared_layer(frames, crop, &blurs);
        *shared.blur.borrow_mut() = Some(CpuBlur { shapes: blurs, crop, frames: frames.to_vec(), image });
    }
    let cpu = shared.blur.borrow().as_ref().unwrap().image.clone();
    if !cache.blur.as_ref().is_some_and(|(old, _)| Arc::ptr_eq(old, &cpu)) {
        cache.blur = Some((cpu.clone(), bitmap(gpu, &cpu)?));
    }
    Ok(())
}

pub fn draw(gpu: &Gpu, shared: &Shared, origin: Point, view: View, cache: &mut Cache) -> Result<()> {
    let crop = rect(view.crop, origin);
    unsafe {
        gpu.d2d.PushAxisAlignedClip(&crop, D2D1_ANTIALIAS_MODE_ALIASED);
    }
    let result = (|| -> Result<()> {
        let shapes = view.shapes.iter().chain(view.preview);
        let blurs: Vec<_> = shapes.clone().filter(|s| s.tool.is_blur()).cloned().collect();
        if !blurs.is_empty() {
            prepare_display(gpu, shared, view.frames, view.crop, &blurs, cache)?;
            if let Some((_, image)) = &cache.blur {
                unsafe {
                    gpu.d2d.DrawBitmap(image, Some(&crop), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
                }
            }
        } else {
            cache.blur = None;
        }
        for shape in shapes.filter(|s| !s.tool.is_blur()) {
            if shape.tool == Tool::Lens {
                if let Some((_, source)) = &cache.blur {
                    draw_lens(gpu, shared, origin, view.crop, shape, source)?;
                } else {
                    let same = cache
                        .lens_source
                        .as_ref()
                        .is_some_and(|(crop, frames, _)| *crop == view.crop && same_frames(frames, view.frames));
                    if !same {
                        let base = Image::compose(view.frames.iter().map(Arc::as_ref), view.crop);
                        cache.lens_source =
                            Some((view.crop, view.frames.iter().map(Arc::downgrade).collect(), bitmap(gpu, &base)?));
                    }
                    draw_lens(gpu, shared, origin, view.crop, shape, &cache.lens_source.as_ref().unwrap().2)?;
                }
                continue;
            }
            draw_shape(gpu, shared, origin, shape, view.text_edit)?;
        }
        Ok(())
    })();
    unsafe {
        gpu.d2d.PopAxisAlignedClip();
    }
    result
}
fn draw_lens(
    gpu: &Gpu,
    shared: &Shared,
    origin: Point,
    crop: Rect,
    shape: &Annotation,
    source: &ID2D1Bitmap1,
) -> Result<()> {
    let r = shape.rect();
    let dest = rect(r, origin);
    let zoom = [1.5, 2.0, 3.0][shape.strength.min(2) as usize];
    let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0);
    let (rx, ry) = (r.width() as f32 / 2.0, r.height() as f32 / 2.0);
    let ellipse =
        D2D1_ELLIPSE { point: Vector2::new(cx - origin.x as f32, cy - origin.y as f32), radiusX: rx, radiusY: ry };
    let src = D2D_RECT_F {
        left: cx - crop.left as f32 - rx / zoom,
        top: cy - crop.top as f32 - ry / zoom,
        right: cx - crop.left as f32 + rx / zoom,
        bottom: cy - crop.top as f32 + ry / zoom,
    };
    unsafe {
        let mask = shared.d2d.CreateEllipseGeometry(&ellipse)?;
        let mut layer = D2D1_LAYER_PARAMETERS1 {
            contentBounds: dest,
            geometricMask: std::mem::ManuallyDrop::new(Some(mask.cast()?)),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: windows_numerics::Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        gpu.d2d.PushLayer(&layer, None::<&ID2D1Layer>);
        gpu.d2d.DrawBitmap(source, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&src), None);
        gpu.d2d.PopLayer();
        std::mem::ManuallyDrop::drop(&mut layer.geometricMask);
        std::mem::ManuallyDrop::drop(&mut layer.opacityBrush);
        gpu.brush.SetColor(&ink([255, 255, 255], 1.0));
        gpu.d2d.DrawEllipse(&ellipse, &gpu.brush, 3.0, None::<&ID2D1StrokeStyle>);
        gpu.brush.SetColor(&ink(shape.color, 1.0));
        gpu.d2d.DrawEllipse(&ellipse, &gpu.brush, 1.5, None::<&ID2D1StrokeStyle>);
    }
    Ok(())
}

fn draw_shape(
    gpu: &Gpu,
    shared: &Shared,
    origin: Point,
    s: &Annotation,
    edit: Option<(u64, usize, usize)>,
) -> Result<()> {
    let p = |a: [i32; 2]| Vector2 { X: (a[0] - origin.x) as f32, Y: (a[1] - origin.y) as f32 };
    let a = p(s.points[0]);
    let b = p(s.points[1]);
    let r = rect(s.rect(), origin);
    let color = ink(s.color, if s.tool == Tool::Marker { 0.30 } else { 1.0 });
    unsafe {
        gpu.brush.SetColor(&color);
        match s.tool {
            Tool::Rectangle => gpu.d2d.DrawRectangle(&r, &gpu.brush, s.width as f32, None),
            Tool::Redact => {
                gpu.brush.SetColor(&ink([17, 17, 17], 1.0));
                gpu.d2d.FillRectangle(&r, &gpu.brush);
            }
            Tool::Ellipse => gpu.d2d.DrawEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: (r.left + r.right) / 2.0, Y: (r.top + r.bottom) / 2.0 },
                    radiusX: (r.right - r.left) / 2.0,
                    radiusY: (r.bottom - r.top) / 2.0,
                },
                &gpu.brush,
                s.width as f32,
                None,
            ),
            Tool::Line => gpu.d2d.DrawLine(a, b, &gpu.brush, s.width as f32, None),
            // One filled outline: a shaft that tapers from the chosen width at the head to a round
            // tail, and a broad head with swept-back wings. The shaft never pokes past the tip.
            Tool::Arrow => {
                let (dx, dy) = (b.X - a.X, b.Y - a.Y);
                let length = dx.hypot(dy);
                if length < 1.0 {
                    return Ok(());
                }
                let w = s.width as f32;
                let (u, n) = ((dx / length, dy / length), (-dy / length, dx / length));
                let head = (w * 4.0 + 18.0).min(length * 0.55);
                let (wing, neck, tail) = (head * 0.62, (w * 0.65).max(1.5), (w * 0.25).max(1.0));
                let at = |back: f32, side: f32| Vector2 {
                    X: b.X - u.0 * back + n.0 * side,
                    Y: b.Y - u.1 * back + n.1 * side,
                };
                let geometry = shared.d2d.CreatePathGeometry()?;
                let sink = geometry.Open()?;
                sink.BeginFigure(at(length, tail), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLines(&[
                    at(head * 0.78, neck),
                    at(head, wing),
                    b,
                    at(head, -wing),
                    at(head * 0.78, -neck),
                    at(length, -tail),
                ]);
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
                gpu.d2d.FillGeometry(&geometry, &gpu.brush, None);
                // A hairline with round joins softens the tip and wing corners.
                let round = shared.d2d.CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES1 { lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() },
                    None,
                )?;
                gpu.d2d.DrawGeometry(&geometry, &gpu.brush, 1.5, &round);
                gpu.d2d.FillEllipse(&D2D1_ELLIPSE { point: a, radiusX: tail, radiusY: tail }, &gpu.brush);
            }
            Tool::Pen | Tool::Marker => {
                let geometry = shared.d2d.CreatePathGeometry()?;
                let sink = geometry.Open()?;
                sink.BeginFigure(a, D2D1_FIGURE_BEGIN_HOLLOW);
                let points: Vec<_> = s.points.iter().skip(1).map(|xy| p(*xy)).collect();
                sink.AddLines(&points);
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                let style = shared.d2d.CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES1 {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        ..Default::default()
                    },
                    None,
                )?;
                gpu.d2d.DrawGeometry(
                    &geometry,
                    &gpu.brush,
                    if s.tool == Tool::Marker { (s.width * 5) as f32 } else { s.width as f32 },
                    &style,
                );
            }
            Tool::Text => {
                if s.text.is_empty() && !edit.is_some_and(|e| e.0 == s.id) {
                    return Ok(());
                }
                let display = if s.text.is_empty() { "Текст" } else { &s.text };
                if s.callout
                    && let Some(tip) = s.points.get(2)
                {
                    let target = p(*tip);
                    let anchor = Vector2::new(a.X + s.font_size() as f32 * 0.5, a.Y + s.font_size() as f32);
                    gpu.brush.SetColor(&ink(s.color, 1.0));
                    gpu.d2d.DrawLine(target, anchor, &gpu.brush, (s.width as f32).max(2.0), None::<&ID2D1StrokeStyle>);
                    gpu.d2d.FillEllipse(&D2D1_ELLIPSE { point: target, radiusX: 3.0, radiusY: 3.0 }, &gpu.brush);
                }
                draw_text(
                    gpu,
                    shared,
                    display,
                    a,
                    s.font_size() as f32,
                    s.color,
                    s.text_style == TextStyle::Filled || s.callout,
                    s.text_style,
                    edit.filter(|e| e.0 == s.id).map(|e| (e.1, e.2)),
                )?;
            }
            // The bubble sits at the second point; when it was dragged away from the first, a tail
            // narrows from the bubble down to that spot, as in ShareX.
            Tool::Counter => {
                let radius = s.font_size() as f32 * 0.75;
                let (dx, dy) = (a.X - b.X, a.Y - b.Y);
                let d = dx.hypot(dy);
                if d > radius * 1.15 {
                    let toward = dy.atan2(dx);
                    let half = (radius / d).acos();
                    let geometry = shared.d2d.CreatePathGeometry()?;
                    let sink = geometry.Open()?;
                    let on = |angle: f32| Vector2 { X: b.X + radius * angle.cos(), Y: b.Y + radius * angle.sin() };
                    sink.BeginFigure(a, D2D1_FIGURE_BEGIN_FILLED);
                    sink.AddLine(on(toward + half));
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: on(toward - half),
                        size: D2D_SIZE_F { width: radius, height: radius },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: D2D1_ARC_SIZE_LARGE,
                    });
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    sink.Close()?;
                    gpu.d2d.FillGeometry(&geometry, &gpu.brush, None);
                } else {
                    gpu.d2d.FillEllipse(&D2D1_ELLIPSE { point: b, radiusX: radius, radiusY: radius }, &gpu.brush);
                }
                let light = (s.color[0] as u32 * 299 + s.color[1] as u32 * 587 + s.color[2] as u32 * 114) > 150000;
                draw_number(
                    gpu,
                    shared,
                    &s.number.to_string(),
                    b,
                    s.font_size() as f32,
                    if light { [17, 17, 17] } else { [255, 255, 255] },
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}
/// A marker's number, centred by its ink rather than its advance box: a «1» has a wide right
/// side bearing and would otherwise sit visibly left of centre.
fn draw_number(gpu: &Gpu, shared: &Shared, value: &str, center: Vector2, size: f32, color: [u8; 3]) -> Result<()> {
    let text: Vec<_> = value.encode_utf16().collect();
    unsafe {
        let format = shared.dwrite.CreateTextFormat(
            PCWSTR(shared.ui_family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("ru-RU"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let layout = shared.dwrite.CreateTextLayout(&text, &format, 65536.0, 65536.0)?;

        let o = layout.GetOverhangMetrics()?;
        // Ink box inside the layout box: overhangs are positive outward of [0, max] (65536 wide).
        let (left, top) = (-o.left, -o.top);
        let (right, bottom) = (65536.0 + o.right, 65536.0 + o.bottom);
        let (ink_w, ink_h) = (right - left, bottom - top);
        let at = Vector2 { X: center.X - ink_w / 2.0 - left, Y: center.Y - ink_h / 2.0 - top };
        gpu.brush.SetColor(&ink(color, 1.0));
        gpu.d2d.DrawTextLayout(at, &layout, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    gpu: &Gpu,
    shared: &Shared,
    value: &str,
    at: Vector2,
    size: f32,
    color: [u8; 3],
    backdrop: bool,
    style: TextStyle,
    editing: Option<(usize, usize)>,
) -> Result<()> {
    let text: Vec<_> = value.encode_utf16().collect();
    unsafe {
        let format = shared.dwrite.CreateTextFormat(
            PCWSTR(shared.ui_family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("ru-RU"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let layout = shared.dwrite.CreateTextLayout(&text, &format, 65536.0, 65536.0)?;
        // Читаемая подложка текста — это часть аннотации и экспорта, не UI.
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        let light = (color[0] as u32 * 299 + color[1] as u32 * 587 + color[2] as u32 * 114) > 120000;
        gpu.brush.SetColor(&ink(if light { [17, 17, 17] } else { [255, 255, 255] }, 0.94));
        if backdrop {
            gpu.d2d.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: at.X - 4.0,
                        top: at.Y - 2.0,
                        right: at.X + metrics.width + 4.0,
                        bottom: at.Y + metrics.height + 2.0,
                    },
                    radiusX: 3.0,
                    radiusY: 3.0,
                },
                &gpu.brush,
            );
        }
        if let Some((cursor, anchor)) = editing {
            let utf16_at = |offset: usize| value.get(..offset).unwrap_or("").encode_utf16().count() as u32;
            if cursor != anchor {
                let start = utf16_at(cursor.min(anchor));
                let end = utf16_at(cursor.max(anchor));
                let mut hits = vec![DWRITE_HIT_TEST_METRICS::default(); 256];
                let mut count = 0;
                layout.HitTestTextRange(start, end - start, at.X, at.Y, Some(&mut hits), &mut count)?;
                gpu.brush.SetColor(&ink([76, 139, 245], 0.45));
                for h in hits.iter().take(count as usize) {
                    gpu.d2d.FillRectangle(
                        &D2D_RECT_F { left: h.left, top: h.top, right: h.left + h.width, bottom: h.top + h.height },
                        &gpu.brush,
                    );
                }
            }
            let (mut x, mut y, mut hit) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
            layout.HitTestTextPosition(utf16_at(cursor), false, &mut x, &mut y, &mut hit)?;
            gpu.brush.SetColor(&ink(color, 1.0));
            gpu.d2d.DrawLine(
                Vector2 { X: at.X + x, Y: at.Y + y },
                Vector2 { X: at.X + x, Y: at.Y + y + hit.height.max(size) },
                &gpu.brush,
                1.5,
                None,
            );
        }
        if style == TextStyle::Outline {
            gpu.brush.SetColor(&ink(color, 1.0));
            let radius = (size / 18.0).clamp(1.0, 3.0);
            for i in 0..16 {
                let angle = i as f32 * std::f32::consts::TAU / 16.0;
                gpu.d2d.DrawTextLayout(
                    Vector2::new(at.X + radius * angle.cos(), at.Y + radius * angle.sin()),
                    &layout,
                    &gpu.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
        gpu.brush.SetColor(&ink(if style == TextStyle::Outline { [255, 255, 255] } else { color }, 1.0));
        gpu.d2d.DrawTextLayout(at, &layout, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}
fn ink(rgb: [u8; 3], a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a }
}
fn rect(r: Rect, o: Point) -> D2D_RECT_F {
    D2D_RECT_F {
        left: (r.left - o.x) as f32,
        top: (r.top - o.y) as f32,
        right: (r.right - o.x) as f32,
        bottom: (r.bottom - o.y) as f32,
    }
}

/// Рендер чистого документа в CPU-изображение. Перед восстановлением прежней цели проверяем EndDraw.
pub fn flatten(gpu: &Gpu, shared: &Shared, frames: &[Arc<Image>], crop: Rect, shapes: &[Annotation]) -> Result<Image> {
    let base = Image::compose(frames.iter().map(Arc::as_ref), crop);
    if shapes.is_empty() {
        return Ok(base);
    }
    let old = unsafe { gpu.d2d.GetTarget().ok() };
    let result = (|| -> Result<Image> {
        unsafe {
            let size = D2D_SIZE_U { width: base.width(), height: base.height() };
            let target = gpu.d2d.CreateBitmap(size, None, 0, &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET))?;
            let background = bitmap(gpu, &base)?;
            gpu.d2d.SetTarget(&target);
            gpu.d2d.BeginDraw();
            gpu.d2d.DrawBitmap(&background, None, 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
            let drawn = draw(
                gpu,
                shared,
                Point::new(crop.left, crop.top),
                View { shapes, preview: None, frames, crop, selected: None, control: None, text_edit: None },
                &mut Cache::default(),
            );
            let ended = gpu.d2d.EndDraw(None, None);
            drawn?;
            ended?;
            let readback = gpu.d2d.CreateBitmap(
                size,
                None,
                0,
                &bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
            )?;
            readback.CopyFromBitmap(None, &target, None)?;
            let mapped = readback.Map(D2D1_MAP_OPTIONS_READ)?;
            let mut pixels = Vec::with_capacity(base.bgra.len());
            for y in 0..size.height {
                pixels.extend_from_slice(std::slice::from_raw_parts(
                    mapped.bits.add((y * mapped.pitch) as usize),
                    size.width as usize * 4,
                ));
            }
            readback.Unmap()?;
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            Ok(Image::new(crop, pixels))
        }
    })();
    unsafe {
        gpu.d2d.SetTarget(old.as_ref());
    }
    result
}
