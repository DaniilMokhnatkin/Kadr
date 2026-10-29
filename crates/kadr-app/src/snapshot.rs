//! `kadr --snapshot [файл.png] [--scale 1.5]` — сцена оверлея в PNG без окон и без реального экрана.
//! Фон — полосы «худших» цветов (белый, светло-серый, средне-серый, цвет акцента, чёрный) и радуга
//! снизу: сразу видно, читаются ли рамка и подписи на любом скрине. Для ревью дизайна и будущих
//! golden-тестов.

use std::error::Error;
use std::ffi::c_void;
use std::fs::File;
use std::io::BufWriter;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_MAP_OPTIONS_READ,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

use kadr_render::gpu::{Gpu, Shared, bitmap_props};
use kadr_render::scene::{self, Hud, HudRow, Magnifier, Marks, Scene, TextStyles, Toolbar, ToolbarButton};
use kadr_render::theme::Theme;

const BANDS: [u32; 5] = [0xffffff, 0xd4d4d8, 0x777777, 0x5e6ad2, 0x000000];

pub fn run(path: &str, scale: f32, editor: bool, exit: Option<f32>) -> Result<(), Box<dyn Error>> {
    run_tool(path, scale, editor, kadr_doc::annotation::Tool::Arrow, exit)
}
fn run_tool(
    path: &str,
    scale: f32,
    editor: bool,
    tool: kadr_doc::annotation::Tool,
    exit: Option<f32>,
) -> Result<(), Box<dyn Error>> {
    if !(0.5..=3.0).contains(&scale) {
        return Err("масштаб должен быть от 0.5 до 3".into());
    }
    let shared = Shared::new()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let adapter = unsafe { factory.EnumAdapters1(0)? };
    let gpu = Gpu::new(&shared, &adapter, "snapshot".into())?;

    let (w, h) = ((1600.0 * scale) as u32, (900.0 * scale) as u32);
    let size = D2D_SIZE_U { width: w, height: h };
    let pixels = background(w, h);
    let at = |v: f32| (v * scale) as i32;
    // Выделение пересекает светлую, серую и акцентную полосы и радугу; правый край — на цвете акцента.
    let selection = RECT {
        left: at(470.0),
        top: at(300.0),
        right: at(1270.0),
        bottom: at(700.0),
    };
    let crop = kadr_doc::Rect::new(selection.left, selection.top, selection.right, selection.bottom);
    let frames = vec![std::sync::Arc::new(kadr_doc::Image::new(
        kadr_doc::Rect::new(0, 0, w as i32, h as i32),
        pixels.clone(),
    ))];
    let shapes = if editor { sample_shapes(scale) } else { Vec::new() };
    let hud = sample_hud();
    // Курсор у правого нижнего угла, как при растягивании: видны и маркеры, и лупа.
    let cursor = (at(1268.0), at(698.0));
    let i = ((cursor.1 as u32 * w + cursor.0 as u32) * 4) as usize;
    let magnifier = Magnifier {
        at: cursor,
        rgb: Some([pixels[i + 2], pixels[i + 1], pixels[i]]),
    };

    let rgba = unsafe {
        let ctx = &gpu.d2d;
        let background = ctx.CreateBitmap(
            size,
            Some(pixels.as_ptr() as *const c_void),
            w * 4,
            &bitmap_props(D2D1_BITMAP_OPTIONS_NONE),
        )?;
        let target = ctx.CreateBitmap(size, None, 0, &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET))?;
        let scene = Scene {
            frame: Some(&background),
            origin: (0, 0),
            size: (w, h),
            scale,
            marks: Marks {
                selection: Some(selection),
                handles: !editor,
                magnifier: if editor { None } else { Some(&magnifier) },
                toolbar: Some(Toolbar {
                    hovered: Some(ToolbarButton::Save),
                    editor: editor.then_some(scene::EditorBar {
                        tool,
                        color: 0,
                        width: 3,
                        strength: 1,
                        undo: true,
                        redo: false,
                    }),
                }),
                hud: if editor { None } else { Some(&hud) },
                drawing: editor.then_some(kadr_render::annotations::View {
                    shapes: &shapes,
                    preview: None,
                    frames: &frames,
                    crop,
                    selected: None,
                    text_edit: None,
                }),
                glide: false,
                exit: exit.unwrap_or(0.0),
                flash: exit.is_some(),
                hint: !editor,
            },
        };
        ctx.SetTarget(&target);
        ctx.BeginDraw();
        let drawn = scene::draw(
            &gpu,
            &shared,
            &TextStyles::new(&shared, scale)?,
            &Theme::current(),
            &scene,
            &mut Default::default(),
        );
        ctx.EndDraw(None, None)?;
        drawn?;

        let readback = ctx.CreateBitmap(
            size,
            None,
            0,
            &bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
        )?;
        readback.CopyFromBitmap(None, &target, None)?;
        let mapped = readback.Map(D2D1_MAP_OPTIONS_READ)?;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let row = std::slice::from_raw_parts(mapped.bits.add((y * mapped.pitch) as usize), (w * 4) as usize);
            for [b, g, r, _] in row.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[*r, *g, *b, 255]);
            }
        }
        readback.Unmap()?;
        rgba
    };

    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    if editor {
        let image = kadr_render::annotations::flatten(&gpu, &shared, &frames, crop, &shapes)?;
        let export = std::path::Path::new(path).with_file_name(format!(
            "{}-export.png",
            std::path::Path::new(path).file_stem().unwrap().to_string_lossy()
        ));
        std::fs::write(export, kadr_doc::pixels::encode_png(&image)?)?;
        let pin = crate::pin_window::preview(&image, scale)?;
        let pin_path = std::path::Path::new(path).with_file_name(format!(
            "{}-pin.png",
            std::path::Path::new(path).file_stem().unwrap().to_string_lossy()
        ));
        std::fs::write(pin_path, kadr_doc::pixels::encode_png(&pin)?)?;
    }
    kadr_win::log_info!("снимок сцены сохранён: {path} ({w}×{h}, масштаб {scale})");
    Ok(())
}
/// Same production painters, synthetic pixels only: no desktop, focus or input.
pub fn design_set(dir: &str) -> Result<(), Box<dyn Error>> {
    use kadr_doc::annotation::Tool;
    let dir = std::path::Path::new(dir);
    std::fs::create_dir_all(dir)?;
    for scale in [1.0, 1.5, 2.0] {
        for (name, tool) in [
            ("select", Tool::Select),
            ("arrow", Tool::Arrow),
            ("blur", Tool::BlurBrush),
            ("text", Tool::Text),
            ("redact", Tool::Redact),
        ] {
            run_tool(
                dir.join(format!("{name}-{}.png", (scale * 100.0) as i32))
                    .to_str()
                    .unwrap(),
                scale,
                true,
                tool,
                None,
            )?;
        }
        let thumb = std::sync::Arc::new(kadr_doc::Image::new(
            kadr_doc::Rect::new(0, 0, 1280, 720),
            background(1280, 720),
        ));
        std::fs::write(
            dir.join(format!("settings-{}.png", (scale * 100.0) as i32)),
            kadr_doc::pixels::encode_png(&crate::settings_window::preview(scale)?)?,
        )?;
        std::fs::write(
            dir.join(format!("thumbnail-{}.png", (scale * 100.0) as i32)),
            kadr_doc::pixels::encode_png(&crate::thumbnail::preview(thumb, scale)?)?,
        )?;
        for (name, n, loading, error) in [
            ("recent", 6, false, None),
            ("recent-full", 10, false, None),
            ("recent-empty", 0, false, None),
            ("recent-loading", 0, true, None),
            (
                "recent-error",
                0,
                false,
                Some("Не удалось прочитать снимки. Откройте «Недавние» ещё раз.".into()),
            ),
        ] {
            let entries = (0..n)
                .map(|i| kadr_store::recent::Entry {
                    id: format!("fixture-{i}"),
                    created_ms: 1793300000000 - i as u64 * 300000,
                    open: true,
                    preview: Some(kadr_doc::Image::new(
                        kadr_doc::Rect::new(0, 0, 320, 180),
                        background(320, 180),
                    )),
                })
                .collect();
            let image = crate::recent_window::preview(entries, scale, true, loading, error)?;
            std::fs::write(
                dir.join(format!("{name}-{}.png", (scale * 100.0) as i32)),
                kadr_doc::pixels::encode_png(&image)?,
            )?;
        }
    }
    Ok(())
}
fn sample_shapes(scale: f32) -> Vec<kadr_doc::annotation::Annotation> {
    use kadr_doc::{
        Point, Rect,
        annotation::{Annotation, Tool},
    };
    let mut shapes = Vec::new();
    for (tool, a, b) in [
        (Tool::Arrow, [570, 580], [770, 420]),
        (Tool::Rectangle, [820, 410], [1080, 510]),
        (Tool::Text, [820, 350], [820, 350]),
        (Tool::Counter, [540, 360], [540, 360]),
        (Tool::Blur, [840, 550], [1160, 650]),
    ] {
        let mut s = Annotation::new(shapes.len() as u64 + 1, tool, Point::new(a[0], a[1]));
        s.points[1] = b;
        s.text = "Здесь важная деталь".into();
        s.seed = 42;
        shapes.push(s.transformed(
            Rect::new(0, 0, 1600, 900),
            Rect::new(0, 0, (1600.0 * scale) as i32, (900.0 * scale) as i32),
        ));
    }
    shapes
}

/// Цифры — из реального бенчмарка прогретого режима на машине разработки.
fn sample_hud() -> Hud {
    let ms = |label: &str, value: &str| HudRow::new(label, value.into(), "мс");
    Hud {
        title: "Kadr".into(),
        scope: "M0 · PrtSc · Medium · прогретый (пример)".into(),
        lead_label: "хоткей → экран".into(),
        lead: Some(("8.2".into(), "мс".into())),
        groups: vec![
            vec![
                ms("захват", "0.2"),
                ms("рендер", "0.8"),
                ms("показ", "2.1"),
                ms("до экрана", "5.0"),
            ],
            vec![
                ms("p50", "8.2"),
                ms("p95", "13.6"),
                HudRow::new("замеров", "10".into(), ""),
            ],
            vec![
                ms("фокус после показа", "0.0"),
                HudRow::new("очередь сообщений", "~0".into(), "мс"),
            ],
        ],
        hint: "Esc закрыть · H скрыть панель".into(),
    }
}

/// BGRA: пять вертикальных полос, нижняя треть — радуга.
fn background(w: u32, h: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if y * 3 >= h * 2 {
                hue(x as f32 / w as f32)
            } else {
                BANDS[(x * 5 / w) as usize]
            };
            pixels.extend_from_slice(&[(c & 0xff) as u8, ((c >> 8) & 0xff) as u8, ((c >> 16) & 0xff) as u8, 255]);
        }
    }
    pixels
}

/// HSV(t, 0.8, 0.9) → 0xRRGGBB.
fn hue(t: f32) -> u32 {
    let (v, s) = (0.9, 0.8);
    let h = t * 6.0;
    let f = h - h.floor();
    let (p, q, r) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (red, green, blue) = match (h.floor() as i32).rem_euclid(6) {
        0 => (v, r, p),
        1 => (q, v, p),
        2 => (p, v, r),
        3 => (p, q, v),
        4 => (r, p, v),
        _ => (v, p, q),
    };
    let byte = |c: f32| (c * 255.0) as u32;
    (byte(red) << 16) | (byte(green) << 8) | byte(blue)
}
