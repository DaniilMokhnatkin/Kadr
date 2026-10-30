//! Повторяемый offscreen замер, без окон и ввода. Не измеряет реальную задержку монитора.
use kadr_doc::{Image, Point, Rect, annotation::Tool};
use kadr_editor::drawing::Drawing;
use kadr_render::{
    annotations::{self, Cache, View},
    gpu::{Gpu, Shared, bitmap_props},
    scene::{self, EditorBar, Marks, Scene, TextStyles},
    theme::Theme,
};
use std::{sync::Arc, time::Instant};
use windows::Win32::{
    Foundation::RECT,
    Graphics::{
        Direct2D::{Common::D2D_SIZE_U, D2D1_BITMAP_OPTIONS_TARGET},
        Dxgi::{CreateDXGIFactory1, IDXGIFactory1},
    },
};
pub fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let shared = Shared::new()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let gpu = Gpu::new(&shared, &unsafe { factory.EnumAdapters1(0)? }, "interaction-bench".into())?;
    let crop = Rect::new(0, 0, 1920, 1080);
    let frames = vec![Arc::new(Image::new(crop, vec![180; 1920 * 1080 * 4]))];
    let base = annotations::bitmap(&gpu, &frames[0])?;
    let target = unsafe {
        gpu.d2d.CreateBitmap(
            D2D_SIZE_U { width: 1920, height: 1080 },
            None,
            0,
            &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET),
        )?
    };
    unsafe {
        gpu.d2d.SetTarget(&target);
    }
    let text = TextStyles::new(&shared, 1.0)?;
    let theme = Theme::current();
    let mut lines = Vec::new();
    for tool in [Tool::Pen, Tool::BlurBrush] {
        let mut d = Drawing::default();
        d.set_tool(tool);
        d.down(Point::new(100, 400), crop, 42);
        let mut cache = Cache::default();
        let mut ms = Vec::new();
        for i in 0..80 {
            d.motion(Point::new(100 + i * 16, 400 + ((i as f64 / 5.0).sin() * 160.0) as i32), crop, false);
            let start = Instant::now();
            unsafe {
                gpu.d2d.BeginDraw();
            }
            let view = View {
                shapes: d.shapes(),
                preview: d.preview(),
                frames: &frames,
                crop,
                selected: None,
                control: None,
                text_edit: None,
            };
            scene::draw(
                &gpu,
                &shared,
                &text,
                &theme,
                &Scene {
                    frame: Some(&base),
                    origin: (0, 0),
                    size: (1920, 1080),
                    scale: 1.0,
                    marks: Marks {
                        selection: Some(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }),
                        drawing: Some(view),
                        ..Default::default()
                    },
                },
                &mut cache,
            )?;
            unsafe {
                gpu.d2d.EndDraw(None, None)?;
            }
            if i >= 10 {
                ms.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        ms.sort_by(f64::total_cmp);
        lines.push(format!(
            "{tool:?}: offscreen frame p50={:.3}ms, p95={:.3}ms",
            ms[ms.len() / 2],
            ms[ms.len() * 95 / 100]
        ));
    }
    let editor = Some(EditorBar {
        text_style: Default::default(),
        callout: false,
        tool: Tool::Select,
        color: 0,
        width: 3,
        strength: 1,
        undo: false,
        redo: false,
        smart: Default::default(),
    });
    let start = Instant::now();
    for _ in 0..400 {
        std::hint::black_box(scene::toolbar_hit(
            &shared,
            &text,
            RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            1.0,
            RECT { left: 100, top: 100, right: 1600, bottom: 900 },
            (600, 600),
            editor,
        ));
    }
    lines.push(format!("toolbar hit test average={:.3}ms", start.elapsed().as_secs_f64() * 1000.0 / 400.0));
    let report =
        lines.join("\n") + "\nCPU/GPU submission only; no live desktop, vsync or input-to-photon measurement.\n";
    std::fs::write(path, &report)?;
    kadr_win::log_info!("{report}");
    Ok(())
}
