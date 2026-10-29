//! Long-screenshot probe on a real browser: opens a tall page in Edge, scrolls it with the wheel the
//! way a hand would, captures the area with DXGI duplication like Kadr does, and reports every
//! stitcher step. Writes the glued result next to the page.
//!
//! cargo run --release --example scroll_probe -p kadr-app -- <folder>
use kadr_doc::stitch::{Step, Stitcher};
use kadr_doc::{Image, Rect};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, PostMessageW, SetCursorPos, WM_CLOSE};
use windows::core::Interface;

fn page(folder: &std::path::Path) -> std::path::PathBuf {
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><style>body{font:16px Segoe UI;margin:0}\
         header{position:sticky;top:0;background:#246;color:#fff;padding:14px 24px;font-size:20px}\
         main{padding:8px 32px}p{line-height:1.5}.b{height:120px;border-radius:12px;margin:18px 0}</style>\
         <header>Sticky header · Kadr scroll probe</header><main>",
    );
    for i in 0..60 {
        html += &format!(
            "<h2>Section {i}</h2><p>Paragraph {i}: the quick brown fox jumps over the lazy dog. Съешь же ещё этих мягких \
             французских булок, да выпей чаю. {}</p>",
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(i % 4 + 1)
        );
        if i % 5 == 2 {
            html += &format!("<div class=b style='background:hsl({},60%,70%)'></div>", i * 37 % 360);
        }
    }
    html += "</main>";
    let path = folder.join("probe.html");
    std::fs::write(&path, html).unwrap();
    path
}

fn wheel(delta: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { mouseData: delta as u32, dwFlags: MOUSEEVENTF_WHEEL, ..Default::default() } },
    };
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

fn main() {
    let folder = std::path::PathBuf::from(std::env::args().nth(1).expect("folder"));
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let html = page(&folder);
    std::process::Command::new(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
        .args(["--new-window", "--window-position=120,80", "--window-size=1100,950"])
        .arg(format!("file:///{}", html.display().to_string().replace('\\', "/")))
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(4));
    let hwnd = unsafe { GetForegroundWindow() };
    let mut w = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut w).unwrap() };
    let region = Rect::new(w.left + 60, w.top + 200, w.right - 60, w.bottom - 80);
    println!("window {w:?}, region {region:?}");

    let (adapters, mut outputs) = kadr_capture::enumerate().unwrap();
    let out = outputs.iter_mut().find(|o| o.bounds().intersect(&region).is_some()).unwrap();
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            &adapters[out.gpu].adapter.cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>().unwrap(),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .unwrap();
    }
    let (device, context) = (device.unwrap(), context.unwrap());
    let mut grab = || -> Image {
        out.capture(&device, &context).unwrap();
        Image::compose(&[out.read_pixels(&device, &context).unwrap()], region)
    };
    unsafe {
        let _ = SetCursorPos((region.left + region.right) / 2, (region.top + region.bottom) / 2);
    }
    let first = grab();
    let mut s = Stitcher::new(&first);
    let start = Instant::now();
    let mut tally = std::collections::BTreeMap::<&str, usize>::new();
    for tick in 0..90 {
        if tick % 3 == 0 && tick < 75 {
            wheel(-120);
        }
        std::thread::sleep(Duration::from_millis(66));
        let step = s.push(&grab());
        let name = match step {
            Step::Same => "same",
            Step::Grew(_) => "grew",
            Step::Lost => "LOST",
            Step::Back => "back",
            Step::Full => "full",
        };
        *tally.entry(name).or_default() += 1;
        if let Step::Grew(d) = step {
            print!("+{d} ");
        } else {
            print!("{name} ");
        }
    }
    println!("\n{:?} in {:.1}s, height {} (one frame {})", tally, start.elapsed().as_secs_f32(), s.height(), first.height());
    let result = s.finish();
    kadr_export::file::write_png(&folder.join("probe.png"), &result).unwrap();
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
    }
}
