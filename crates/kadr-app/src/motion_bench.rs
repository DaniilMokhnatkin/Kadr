//! Synthetic 170 Hz animation timeline with a D3D11 completion query. Never shows/captures a window.
use kadr_doc::{Image, Rect, annotation::Tool};
use kadr_render::{
    annotations::{self, Cache},
    gpu::{Gpu, Shared, bitmap_props},
    scene::{self, EditorBar, Marks, Scene, TextStyles, Toolbar, ToolbarButton},
    theme::Theme,
};
use std::{sync::Arc, time::Instant};
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct2D::{Common::D2D_SIZE_U, D2D1_BITMAP_OPTIONS_TARGET},
            Direct3D11::{D3D11_QUERY_DESC, D3D11_QUERY_EVENT},
            Dxgi::{CreateDXGIFactory1, IDXGIFactory1},
            Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW},
        },
    },
    core::{BOOL, PCWSTR},
};

pub fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let shared = Shared::new()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let adapter = unsafe { factory.EnumAdapters1(0)? };
    let gpu = Gpu::new(&shared, &adapter, "motion-bench".into())?;
    let mut report = Vec::new();
    let mut output = 0;
    while let Ok(out) = unsafe { adapter.EnumOutputs(output) } {
        let desc = unsafe { out.GetDesc()? };
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if unsafe { EnumDisplaySettingsW(PCWSTR(desc.DeviceName.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool()
        {
            report.push(format!(
                "Display {}: {}x{}, current mode {} Hz",
                output + 1,
                mode.dmPelsWidth,
                mode.dmPelsHeight,
                mode.dmDisplayFrequency
            ));
        }
        output += 1;
    }
    let frames = [Arc::new(Image::new(
        Rect::new(0, 0, 2560, 1440),
        vec![160; 2560 * 1440 * 4],
    ))];
    let background = annotations::bitmap(&gpu, &frames[0])?;
    let target = unsafe {
        gpu.d2d.CreateBitmap(
            D2D_SIZE_U {
                width: 2560,
                height: 1440,
            },
            None,
            0,
            &bitmap_props(D2D1_BITMAP_OPTIONS_TARGET),
        )?
    };
    let mut query = None;
    unsafe {
        gpu.device.CreateQuery(
            &D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            },
            Some(&mut query),
        )?;
        gpu.d2d.SetTarget(&target);
    }
    let query = query.ok_or("GPU query unavailable")?;
    let text = TextStyles::new(&shared, 1.0)?;
    text.enable_motion(true);
    let theme = Theme::current();
    let mut cache = Cache::default();
    let mut samples = Vec::new();
    let pictures = std::path::Path::new(path).with_extension("frames");
    std::fs::create_dir_all(&pictures)?;
    for frame in 0..340 {
        let now = frame as f64 / 170.0;
        text.set_animation_time(Some(now));
        let tool = match frame {
            0..34 => Tool::Select,
            34..102 => Tool::Rectangle,
            102..170 => Tool::BlurBrush,
            170..238 => Tool::Text,
            238..272 => Tool::Select,
            _ => Tool::Arrow,
        };
        let start = Instant::now();
        unsafe {
            gpu.d2d.BeginDraw();
        }
        scene::draw(
            &gpu,
            &shared,
            &text,
            &theme,
            &Scene {
                frame: Some(&background),
                origin: (0, 0),
                size: (2560, 1440),
                scale: 1.0,
                marks: Marks {
                    selection: Some(RECT {
                        left: 400,
                        top: 200,
                        right: 2000,
                        bottom: 1100,
                    }),
                    toolbar: Some(Toolbar {
                        hovered: Some(if frame % 50 < 25 {
                            ToolbarButton::Tool(tool)
                        } else {
                            ToolbarButton::Save
                        }),
                        editor: Some(EditorBar {
                            tool,
                            color: 0,
                            width: 3,
                            strength: 1,
                            undo: true,
                            redo: false,
                        }),
                    }),
                    ..Default::default()
                },
            },
            &mut cache,
        )?;
        unsafe {
            gpu.d2d.EndDraw(None, None)?;
            gpu.context.End(&query);
            gpu.context.Flush();
        }
        loop {
            let mut ready = BOOL(0);
            unsafe {
                gpu.context.GetData(
                    &query,
                    Some(&mut ready as *mut _ as *mut _),
                    size_of::<BOOL>() as u32,
                    0,
                )?;
            }
            if ready.as_bool() {
                break;
            }
            if start.elapsed().as_secs_f64() > 2.0 {
                return Err("GPU completion timeout".into());
            }
            std::thread::yield_now();
        }
        if frame >= 10 {
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        if [0, 34, 35, 38, 42, 50, 68, 102, 108, 124, 238, 244, 270, 339].contains(&frame) {
            let image = read(&gpu, &target)?;
            std::fs::write(
                pictures.join(format!("frame-{frame:03}.png")),
                kadr_doc::pixels::encode_png(&image)?,
            )?;
        }
    }
    samples.sort_by(f64::total_cmp);
    let budget = 1000.0 / 170.0;
    report.push(format!(
        "2560x1440 UI animation, CPU + GPU completion: p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
        samples.last().unwrap()
    ));
    report.push(format!(
        "170 Hz work budget {:.3} ms: {}/{} sampled frames inside budget",
        budget,
        samples.iter().filter(|v| **v <= budget).count(),
        samples.len()
    ));
    report.push("Synthetic timeline only. Hidden/offscreen GPU work; actual scanout cadence and input-to-photon are not measured.".into());
    std::fs::write(path, report.join("\n") + "\n")?;
    Ok(())
}
fn read(gpu: &Gpu, target: &windows::Win32::Graphics::Direct2D::ID2D1Bitmap1) -> windows::core::Result<Image> {
    use windows::Win32::Graphics::Direct2D::*;
    unsafe {
        let bitmap = gpu.d2d.CreateBitmap(
            D2D_SIZE_U {
                width: 2560,
                height: 1440,
            },
            None,
            0,
            &bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
        )?;
        bitmap.CopyFromBitmap(None, target, None)?;
        let mapped = bitmap.Map(D2D1_MAP_OPTIONS_READ)?;
        let mut pixels = Vec::with_capacity(2560 * 1440 * 4);
        for y in 0..1440 {
            pixels.extend_from_slice(std::slice::from_raw_parts(
                mapped.bits.add((y * mapped.pitch) as usize),
                2560 * 4,
            ));
        }
        bitmap.Unmap()?;
        Ok(Image::new(Rect::new(0, 0, 2560, 1440), pixels))
    }
}
