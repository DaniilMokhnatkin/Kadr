//! Один рендер аннотаций для оверлея, миниатюры и экспорта. Панелей и рамок в нём нет.
use crate::gpu::{Gpu, Shared, bitmap_props};
use kadr_doc::annotation::{Annotation, Tool};
use kadr_doc::{Image, Point, Rect};
use std::sync::Arc;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{PCWSTR, Result, w};
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
    pub text_edit: Option<(u64, usize, usize)>,
}
#[derive(Default)]
pub struct Cache {
    blur: Option<(Vec<Annotation>, Rect, ID2D1Bitmap1)>,
}
pub fn bitmap(gpu: &Gpu, image: &Image) -> Result<ID2D1Bitmap1> {
    unsafe {
        gpu.d2d.CreateBitmap(
            D2D_SIZE_U {
                width: image.width(),
                height: image.height(),
            },
            Some(image.bgra.as_ptr() as *const _),
            image.width() * 4,
            &bitmap_props(D2D1_BITMAP_OPTIONS_NONE),
        )
    }
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
            let same = cache
                .blur
                .as_ref()
                .is_some_and(|(old, r, _)| *old == blurs && *r == view.crop);
            if !same {
                let mut base = Image::compose(view.frames.iter().map(Arc::as_ref), view.crop);
                for shape in &blurs {
                    let area = if shape.tool == Tool::Spotlight {
                        Some(view.crop)
                    } else {
                        shape.bounds().intersect(&view.crop)
                    };
                    if let Some(area) = area {
                        let source = Image::compose([&base], area);
                        let patch = kadr_doc::privacy::blur_patch(&source, shape);
                        for y in area.top..area.bottom {
                            let from = ((y - area.top) * area.width() * 4) as usize;
                            let to =
                                (((y - base.rect.top) * base.rect.width() + area.left - base.rect.left) * 4) as usize;
                            let len = area.width() as usize * 4;
                            base.bgra[to..to + len].copy_from_slice(&patch.bgra[from..from + len]);
                        }
                    }
                }
                cache.blur = Some((blurs, view.crop, bitmap(gpu, &base)?));
            }
            if let Some((_, _, image)) = &cache.blur {
                unsafe {
                    gpu.d2d.DrawBitmap(
                        image,
                        Some(&crop),
                        1.0,
                        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                        None,
                        None,
                    );
                }
            }
        } else {
            cache.blur = None;
        }
        for shape in shapes.filter(|s| !s.tool.is_blur()) {
            draw_shape(gpu, shared, origin, shape, view.text_edit)?;
        }
        Ok(())
    })();
    unsafe {
        gpu.d2d.PopAxisAlignedClip();
    }
    result
}
fn draw_shape(
    gpu: &Gpu,
    shared: &Shared,
    origin: Point,
    s: &Annotation,
    edit: Option<(u64, usize, usize)>,
) -> Result<()> {
    let p = |a: [i32; 2]| Vector2 {
        X: (a[0] - origin.x) as f32,
        Y: (a[1] - origin.y) as f32,
    };
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
                    point: Vector2 {
                        X: (r.left + r.right) / 2.0,
                        Y: (r.top + r.bottom) / 2.0,
                    },
                    radiusX: (r.right - r.left) / 2.0,
                    radiusY: (r.bottom - r.top) / 2.0,
                },
                &gpu.brush,
                s.width as f32,
                None,
            ),
            Tool::Line | Tool::Arrow => {
                gpu.d2d.DrawLine(a, b, &gpu.brush, s.width as f32, None);
                if s.tool == Tool::Arrow {
                    let angle = (b.Y - a.Y).atan2(b.X - a.X);
                    let len =
                        (s.width as f32 * 4.0 + 10.0).min(((b.X - a.X).powi(2) + (b.Y - a.Y).powi(2)).sqrt() * 0.6);
                    let tip = |delta: f32| Vector2 {
                        X: b.X - len * (angle + delta).cos(),
                        Y: b.Y - len * (angle + delta).sin(),
                    };
                    let geometry = shared.d2d.CreatePathGeometry()?;
                    let sink = geometry.Open()?;
                    sink.BeginFigure(b, D2D1_FIGURE_BEGIN_FILLED);
                    sink.AddLine(tip(0.45));
                    sink.AddLine(tip(-0.45));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    sink.Close()?;
                    gpu.d2d.FillGeometry(&geometry, &gpu.brush, None);
                }
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
                    if s.tool == Tool::Marker {
                        (s.width * 5) as f32
                    } else {
                        s.width as f32
                    },
                    &style,
                );
            }
            Tool::Text => {
                if s.text.is_empty() && !edit.is_some_and(|e| e.0 == s.id) {
                    return Ok(());
                }
                let display = if s.text.is_empty() { "Текст" } else { &s.text };
                draw_text(
                    gpu,
                    shared,
                    display,
                    a,
                    s.font_size() as f32,
                    s.color,
                    true,
                    edit.filter(|e| e.0 == s.id).map(|e| (e.1, e.2)),
                )?;
            }
            Tool::Counter => {
                let radius = s.font_size() as f32 * 0.75;
                gpu.d2d.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: a,
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &gpu.brush,
                );
                let value = s.number.to_string();
                let size = s.font_size() as f32;
                let light = (s.color[0] as u32 * 299 + s.color[1] as u32 * 587 + s.color[2] as u32 * 114) > 150000;
                draw_text(
                    gpu,
                    shared,
                    &value,
                    Vector2 {
                        X: a.X - value.len() as f32 * size * 0.28,
                        Y: a.Y - size * 0.68,
                    },
                    size,
                    if light { [17, 17, 17] } else { [255, 255, 255] },
                    false,
                    None,
                )?;
            }
            _ => {}
        }
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
        gpu.brush
            .SetColor(&ink(if light { [17, 17, 17] } else { [255, 255, 255] }, 0.94));
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
                        &D2D_RECT_F {
                            left: h.left,
                            top: h.top,
                            right: h.left + h.width,
                            bottom: h.top + h.height,
                        },
                        &gpu.brush,
                    );
                }
            }
            let (mut x, mut y, mut hit) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
            layout.HitTestTextPosition(utf16_at(cursor), false, &mut x, &mut y, &mut hit)?;
            gpu.brush.SetColor(&ink(color, 1.0));
            gpu.d2d.DrawLine(
                Vector2 {
                    X: at.X + x,
                    Y: at.Y + y,
                },
                Vector2 {
                    X: at.X + x,
                    Y: at.Y + y + hit.height.max(size),
                },
                &gpu.brush,
                1.5,
                None,
            );
        }
        gpu.brush.SetColor(&ink(color, 1.0));
        gpu.d2d
            .DrawTextLayout(at, &layout, &gpu.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    Ok(())
}
fn ink(rgb: [u8; 3], a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: rgb[0] as f32 / 255.0,
        g: rgb[1] as f32 / 255.0,
        b: rgb[2] as f32 / 255.0,
        a,
    }
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
            let size = D2D_SIZE_U {
                width: base.width(),
                height: base.height(),
            };
            let target = gpu
                .d2d
                .CreateBitmap(size, None, 0, &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET))?;
            let background = bitmap(gpu, &base)?;
            gpu.d2d.SetTarget(&target);
            gpu.d2d.BeginDraw();
            gpu.d2d.DrawBitmap(
                &background,
                None,
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
                None,
            );
            let drawn = draw(
                gpu,
                shared,
                Point::new(crop.left, crop.top),
                View {
                    shapes,
                    preview: None,
                    frames,
                    crop,
                    selected: None,
                    text_edit: None,
                },
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
