use super::*;
use crate::{
    scene::{self, Marks, Scene, TextStyles},
    theme::Theme,
};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

fn setup() -> (Shared, Gpu) {
    let shared = Shared::new().unwrap();
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
    let gpu = Gpu::new(&shared, &unsafe { factory.EnumAdapters1(0).unwrap() }, "test".into()).unwrap();
    (shared, gpu)
}
fn image(r: Rect, inverse: bool) -> Arc<Image> {
    let mut pixels = Vec::new();
    for y in 0..r.height() {
        for x in 0..r.width() {
            let c = if (x + y) % 2 == 0 { 0 } else { 254 };
            let c = if inverse { 254 - c } else { c };
            pixels.extend_from_slice(&[c, c, c, 255]);
        }
    }
    Arc::new(Image::new(r, pixels))
}
unsafe fn read(gpu: &Gpu, target: &ID2D1Bitmap1, r: Rect) -> Image {
    unsafe {
        let size = D2D_SIZE_U {
            width: r.width() as u32,
            height: r.height() as u32,
        };
        let cpu = gpu
            .d2d
            .CreateBitmap(
                size,
                None,
                0,
                &bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
            )
            .unwrap();
        cpu.CopyFromBitmap(None, target, None).unwrap();
        let map = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
        let mut pixels = Vec::new();
        for y in 0..size.height {
            pixels.extend_from_slice(std::slice::from_raw_parts(
                map.bits.add((y * map.pitch) as usize),
                size.width as usize * 4,
            ));
        }
        cpu.Unmap().unwrap();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Image::new(r, pixels)
    }
}
#[test]
fn overlay_crop_matches_export_and_all_tools_render_on_negative_monitor() {
    let (shared, gpu) = setup();
    let whole = Rect::new(-200, -50, 700, 450);
    let crop = Rect::new(-120, 20, 550, 420);
    let frames = vec![image(whole, false)];
    let mut shapes = Vec::new();
    for (i, tool) in Tool::ALL.into_iter().filter(|t| *t != Tool::Select).enumerate() {
        let x = -80 + (i % 5) as i32 * 120;
        let y = 80 + (i / 5) as i32 * 100;
        let mut s = Annotation::new(i as u64 + 1, tool, Point::new(x, y));
        s.points[1] = [x + 70, y + 50];
        s.seed = 42;
        s.text = "Я 🌍".into();
        shapes.push(s);
    }
    let exported = flatten(&gpu, &shared, &frames, crop, &shapes).unwrap();
    unsafe {
        let target = gpu
            .d2d
            .CreateBitmap(
                D2D_SIZE_U {
                    width: 900,
                    height: 500,
                },
                None,
                0,
                &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET),
            )
            .unwrap();
        let base = bitmap(&gpu, &frames[0]).unwrap();
        gpu.d2d.SetTarget(&target);
        gpu.d2d.BeginDraw();
        let view = View {
            shapes: &shapes,
            preview: None,
            frames: &frames,
            crop,
            selected: None,
            text_edit: None,
        };
        let scene = Scene {
            frame: Some(&base),
            origin: (whole.left, whole.top),
            size: (900, 500),
            scale: 1.0,
            marks: Marks {
                selection: Some(RECT {
                    left: crop.left,
                    top: crop.top,
                    right: crop.right,
                    bottom: crop.bottom,
                }),
                drawing: Some(view),
                ..Default::default()
            },
        };
        scene::draw(
            &gpu,
            &shared,
            &TextStyles::new(&shared, 1.0).unwrap(),
            &Theme::current(),
            &scene,
            &mut Cache::default(),
        )
        .unwrap();
        gpu.d2d.EndDraw(None, None).unwrap();
        let screen = read(&gpu, &target, whole);
        let selected = Image::compose([&screen], crop);
        let differences: Vec<_> = selected
            .bgra
            .iter()
            .zip(&exported.bgra)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i, a.abs_diff(*b)))
            .collect();
        // D2D tessellation rounds a translated ellipse edge by one 8-bit level.
        // Larger errors (wrong crop, UI, missing shape/blur) still fail; redaction is exact below.
        if differences.iter().any(|(_, delta)| *delta > 1) {
            if let Ok(dir) = std::env::var("KADR_TEST_ARTIFACTS") {
                std::fs::write(
                    format!("{dir}/screen.png"),
                    kadr_doc::pixels::encode_png(&selected).unwrap(),
                )
                .unwrap();
                std::fs::write(
                    format!("{dir}/export.png"),
                    kadr_doc::pixels::encode_png(&exported).unwrap(),
                )
                .unwrap();
            }
            panic!(
                "{} channels differ, max {}, first {:?}",
                differences.len(),
                differences.iter().map(|d| d.1).max().unwrap(),
                &differences[..differences.len().min(16)]
            );
        }
    }
}
#[test]
fn solid_redaction_has_no_source_pixels_and_target_is_restored() {
    let (shared, gpu) = setup();
    let crop = Rect::new(0, 0, 128, 64);
    let mut s = Annotation::new(1, Tool::Redact, Point::new(0, 0));
    s.points[1] = [127, 63];
    let a = flatten(&gpu, &shared, &[image(crop, false)], crop, &[s.clone()]).unwrap();
    let b = flatten(&gpu, &shared, &[image(crop, true)], crop, &[s]).unwrap();
    assert_eq!(a, b);
    assert!(a.bgra.as_chunks::<4>().0.iter().all(|p| *p == [17, 17, 17, 255]));
    assert!(unsafe { gpu.d2d.GetTarget() }.is_err());
}
