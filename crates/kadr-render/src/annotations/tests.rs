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
#[test]
fn blur_cpu_work_is_shared_between_surfaces_and_invalidated_for_different_frames() {
    let (shared, gpu) = setup();
    let crop = Rect::new(0, 0, 128, 64);
    let frames = vec![image(crop, false)];
    let mut blur = Annotation::new(1, Tool::Blur, kadr_doc::Point::new(10, 10));
    blur.points[1] = [80, 50];
    flatten(&gpu, &shared, &frames, crop, std::slice::from_ref(&blur)).unwrap();
    let first = shared.blur.borrow().as_ref().unwrap().image.clone();
    flatten(&gpu, &shared, &frames, crop, std::slice::from_ref(&blur)).unwrap();
    assert!(Arc::ptr_eq(&first, &shared.blur.borrow().as_ref().unwrap().image));
    flatten(&gpu, &shared, &[image(crop, true)], crop, &[blur]).unwrap();
    assert!(!Arc::ptr_eq(&first, &shared.blur.borrow().as_ref().unwrap().image));
    shared.clear_annotation_cache();
    assert!(shared.blur.borrow().is_none());
}
#[test]
fn gpu_exposes_a_ready_frame_event_without_showing_a_window() {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_ALPHA_MODE_IGNORE;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::WaitForSingleObject;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let (_, gpu) = setup();
    unsafe {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 64,
            Height: 64,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
            ..Default::default()
        };
        let chain = gpu
            .factory
            .CreateSwapChainForComposition(&gpu.device, &desc, None::<&IDXGIOutput>)
            .unwrap();
        use windows::core::Interface;
        let chain: IDXGISwapChain2 = chain.cast().unwrap();
        let foreground = GetForegroundWindow();
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Kadr frame pacing test"),
            WS_POPUP,
            0,
            0,
            64,
            64,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let composition = gpu.dcomp.CreateTargetForHwnd(window, true).unwrap();
        let visual = gpu.dcomp.CreateVisual().unwrap();
        visual.SetContent(&chain).unwrap();
        composition.SetRoot(&visual).unwrap();
        gpu.dcomp.Commit().unwrap();
        chain.SetMaximumFrameLatency(1).unwrap();
        let event = chain.GetFrameLatencyWaitableObject();
        assert!(!event.is_invalid());
        let ready = WaitForSingleObject(event, 1000);
        let shown = chain.Present(1, DXGI_PRESENT_DO_NOT_WAIT);
        assert!(!IsWindowVisible(window).as_bool());
        assert_eq!(GetForegroundWindow(), foreground);
        let _ = DestroyWindow(window);
        let _ = CloseHandle(event);
        assert_eq!(ready, WAIT_OBJECT_0);
        assert!(
            shown.is_ok() || shown == DXGI_ERROR_WAS_STILL_DRAWING,
            "Present returned {shown:?}"
        );
    }
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
            selected: None, control: None,
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

#[test]
fn prewarmed_patch_reuses_pixels_but_not_other_frames_or_strength() {
    let crop = Rect::new(0, 0, 80, 60);
    let frames = vec![image(crop, false)];
    let mut shape = Annotation::new(0, Tool::Blur, Point::new(10, 10));
    shape.points[1] = [59, 39];
    shape.seed = 123;
    prewarm(&frames, crop, &[shape.clone()]);
    let (_, cached) = blur_patch(&frames, crop, &shape).unwrap();
    shape.id = 42;
    let (_, reused) = blur_patch(&frames, crop, &shape).unwrap();
    assert!(Arc::ptr_eq(&cached, &reused));
    let other = vec![image(crop, true)];
    let (_, different) = blur_patch(&other, crop, &shape).unwrap();
    assert!(!Arc::ptr_eq(&cached, &different));
    shape.strength = 2;
    let (_, stronger) = blur_patch(&frames, crop, &shape).unwrap();
    assert!(!Arc::ptr_eq(&cached, &stronger));
    let weak = frames.iter().map(Arc::downgrade).collect::<Vec<_>>();
    assert!(same_frames(&weak, &frames));
    drop(frames);
    assert!(!same_frames(&weak, &other));
}

#[test]
fn prewarm_caches_the_complete_layer_across_assigned_shape_ids() {
    let crop = Rect::new(-40, 0, 120, 80);
    let frames = vec![image(crop, false)];
    let mut shape = Annotation::new(0, Tool::Blur, Point::new(0, 10));
    shape.points[1] = [89, 49];
    shape.seed = 987;
    prewarm(&frames, crop, &[shape.clone()]);
    let first = prepared_layer(&frames, crop, &[shape.clone()]);
    shape.id = 72;
    let reused = prepared_layer(&frames, crop, &[shape.clone()]);
    assert!(Arc::ptr_eq(&first, &reused), "first UI frame must reuse the entire prepared layer");
    shape.strength = 2;
    let changed = prepared_layer(&frames, crop, &[shape]);
    assert!(!Arc::ptr_eq(&first, &changed));
}

#[test]
fn an_ellipse_patch_never_restores_text_hidden_by_a_neighbour() {
    let crop=Rect::new(0,0,100,100); let frames=vec![image(crop,false)];
    let mut first=Annotation::new(1,Tool::Blur,Point::new(0,0)); first.points[1]=[99,99];first.seed=123;
    let alone=prepared_layer(&frames,crop,&[first.clone()]);
    let mut ellipse=Annotation::new(2,Tool::BlurEllipse,Point::new(10,10));ellipse.points[1]=[89,89];ellipse.seed=777;
    let both=prepared_layer(&frames,crop,&[first,ellipse]);
    // This point is inside the second patch's bounding box, but outside its ellipse and feather.
    let point=Point::new(8,8);
    assert_eq!(both.pixel(point),alone.pixel(point));
    assert_ne!(both.pixel(point),frames[0].pixel(point));
}

#[test]
fn text_styles_export_distinct_pixels_and_callout_is_part_of_the_image() {
    let (shared,gpu)=setup();let crop=Rect::new(0,0,500,260);let frames=vec![image(crop,false)];
    let mut text=Annotation::new(1,Tool::Text,Point::new(50,40));text.text="Текст / Text".into();text.color=[239,68,68];
    let plain=flatten(&gpu,&shared,&frames,crop,&[text.clone()]).unwrap();
    text.text_style=TextStyle::Filled;let filled=flatten(&gpu,&shared,&frames,crop,&[text.clone()]).unwrap();
    text.text_style=TextStyle::Outline;let outline=flatten(&gpu,&shared,&frames,crop,&[text.clone()]).unwrap();
    assert_ne!(plain,filled);assert_ne!(plain,outline);assert_ne!(filled,outline);
    text.callout=true;text.points.push([140,190]);let callout=flatten(&gpu,&shared,&frames,crop,&[text]).unwrap();
    assert_ne!(callout.pixel(Point::new(140,190)),frames[0].pixel(Point::new(140,190)));
    if let Ok(dir)=std::env::var("KADR_TEST_ARTIFACTS") {for (name,img) in [("text-plain",plain),("text-filled",filled),("text-outline",outline),("text-callout",callout)] {std::fs::write(format!("{dir}/{name}.png"),kadr_doc::pixels::encode_png(&img).unwrap()).unwrap();}}
}
#[test]
fn lens_magnifies_only_the_censored_layer_not_the_original_secret() {
    let (shared,gpu)=setup();let crop=Rect::new(0,0,150,150);let frames=vec![image(crop,false)];
    let mut blur=Annotation::new(1,Tool::Blur,Point::new(0,0));blur.points[1]=[149,149];blur.seed=987;
    let mut lens=Annotation::new(2,Tool::Lens,Point::new(20,20));lens.points[1]=[129,129];lens.strength=2;
    let base=flatten(&gpu,&shared,&frames,crop,&[blur.clone()]).unwrap();
    let censored=flatten(&gpu,&shared,&frames,crop,&[blur,lens.clone()]).unwrap();
    let raw=flatten(&gpu,&shared,&frames,crop,&[lens]).unwrap();
    // The centre maps to the centre at every zoom factor, with no original bitmap mixed back in.
    let centre=Point::new(75,75);
    let a=censored.pixel(centre).unwrap();let b=base.pixel(centre).unwrap();
    assert!(a.iter().zip(b).all(|(a,b)|a.abs_diff(b)<=2));
    assert_ne!(censored,raw);
}

#[test]
fn marching_selection_changes_phase_and_reduced_motion_stays_static_without_exporting_ui() {
    let (shared,gpu)=setup();let area=Rect::new(0,0,200,100);let frames=vec![image(area,false)];
    let target=unsafe {gpu.d2d.CreateBitmap(D2D_SIZE_U {width:200,height:100},None,0,&bitmap_props(D2D1_BITMAP_OPTIONS_TARGET)).unwrap()};
    let theme=Theme::current();
    let sample=|time,animate|unsafe {
        gpu.d2d.SetTarget(&target);gpu.d2d.BeginDraw();gpu.d2d.Clear(Some(&D2D1_COLOR_F {r:0.0,g:0.0,b:0.0,a:1.0}));
        scene::marching_border(&gpu,&shared,&theme,D2D_RECT_F {left:10.0,top:10.0,right:190.0,bottom:90.0},1.5,time,animate).unwrap();
        gpu.d2d.EndDraw(None,None).unwrap();read(&gpu,&target,area)
    };
    assert_ne!(sample(0.0,true),sample(0.2,true));
    assert_eq!(sample(0.0,false),sample(0.2,false));
    assert_eq!(flatten(&gpu,&shared,&frames,area,&[]).unwrap(),*frames[0]);
}
