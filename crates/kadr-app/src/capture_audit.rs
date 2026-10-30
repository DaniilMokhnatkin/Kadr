//! Read-only capture diagnostic: no overlay, input simulation, hotkeys or focus changes.
use kadr_render::gpu::{Gpu, Shared};
use std::path::Path;
pub fn run(folder: &str) -> Result<(), Box<dyn std::error::Error>> {
    let folder = Path::new(folder);
    std::fs::create_dir_all(folder)?;
    let shared = Shared::new()?;
    let (adapters, mut outputs) = kadr_capture::enumerate()?;
    let gpus: Vec<_> =
        adapters.iter().map(|a| Gpu::new(&shared, &a.adapter, a.name.clone())).collect::<windows::core::Result<_>>()?;
    let mut report = String::new();
    for (index, output) in outputs.iter_mut().enumerate() {
        let gpu = &gpus[output.gpu];
        let start = std::time::Instant::now();
        output.capture(&gpu.device, &gpu.context)?;
        let image = output.read_pixels(&gpu.device, &gpu.context)?;
        if image.rect != output.bounds() {
            return Err("Capture dimensions differ from desktop coordinates".into());
        }
        report.push_str(&format!(
            "monitor {}: {} rotation={} rect={:?} pixels={}x{} capture+readback={:.2} ms\n",
            index + 1,
            output.name,
            output.rotation.0,
            image.rect,
            image.width(),
            image.height(),
            start.elapsed().as_secs_f64() * 1000.0
        ));
        std::fs::write(folder.join(format!("monitor-{}.png", index + 1)), kadr_doc::pixels::encode_png(&image)?)?;
    }
    std::fs::write(folder.join("capture.txt"), report)?;
    Ok(())
}
