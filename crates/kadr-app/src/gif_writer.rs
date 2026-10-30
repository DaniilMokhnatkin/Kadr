//! GIF for chats: the recorder's frames are shrunk, deduplicated and reduced to 256 colours on a
//! worker, so the capture keeps its pace. A frame the worker cannot take in time is skipped, and
//! the one before simply stays on screen a little longer: the timing stays true either way.
use std::io::BufWriter;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// Longest side of the animation: big enough to read, small enough to send.
pub const MAX_SIDE: u32 = 960;
/// NeuQuant's speed: 1 is best and slowest, 30 fastest. Screens are mostly flat colour.
const SPEED: i32 = 20;

/// A frame (BGRA, top to bottom) and when it was taken; an empty frame marks the end.
type Message = (Vec<u8>, Duration);

pub struct GifWriter {
    tx: SyncSender<Message>,
    worker: JoinHandle<Result<(), String>>,
}

impl GifWriter {
    pub fn new(path: &Path, w: u32, h: u32) -> Result<Self, String> {
        let file = std::fs::File::create(path).map_err(|e| format!("GIF: {e}"))?;
        let (tx, rx) = sync_channel::<Message>(6);
        let worker = std::thread::spawn(move || encode(BufWriter::new(file), w, h, rx));
        Ok(Self { tx, worker })
    }

    /// Never waits: when the worker is behind, this frame is skipped.
    pub fn frame(&self, bgra: &[u8], at: Duration) {
        let _ = self.tx.try_send((bgra.to_vec(), at));
    }

    pub fn finish(self, end: Duration) -> Result<(), String> {
        let _ = self.tx.send((Vec::new(), end));
        drop(self.tx);
        self.worker.join().map_err(|_| "GIF: кодировщик упал".to_string())?
    }
}

/// The animation's size: the area itself, or shrunk so its longest side is `MAX_SIDE`.
pub fn size(w: u32, h: u32) -> (u32, u32) {
    let k = (MAX_SIDE as f64 / w.max(h) as f64).min(1.0);
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

fn encode(out: impl std::io::Write, w: u32, h: u32, rx: Receiver<Message>) -> Result<(), String> {
    let (ow, oh) = size(w, h);
    let fail = |e: gif::EncodingError| format!("GIF: {e}");
    let mut encoder = gif::Encoder::new(out, ow as u16, oh as u16, &[]).map_err(fail)?;
    encoder.set_repeat(gif::Repeat::Infinite).map_err(fail)?;
    // Delays are whole centiseconds: rounding each moment, not each gap, keeps the total true.
    let cs = |t: Duration| (t.as_millis() / 10) as u64;
    let mut write = |mut rgba: Vec<u8>, from: Duration, to: Duration| {
        let mut frame = gif::Frame::from_rgba_speed(ow as u16, oh as u16, &mut rgba, SPEED);
        frame.delay = cs(to).saturating_sub(cs(from)).clamp(2, u16::MAX as u64) as u16;
        encoder.write_frame(&frame).map_err(fail)
    };
    let mut pending: Option<(Vec<u8>, Duration)> = None;
    let mut end = Duration::ZERO;
    for (bgra, at) in rx {
        if bgra.is_empty() {
            end = at;
            break;
        }
        let rgba = shrink(&bgra, w, h, ow, oh);
        if pending.as_ref().is_some_and(|(previous, _)| *previous == rgba) {
            continue;
        }
        if let Some((previous, from)) = pending.take() {
            write(previous, from, at)?;
        }
        pending = Some((rgba, at));
    }
    if let Some((previous, from)) = pending {
        write(previous, from, end.max(from + Duration::from_millis(100)))?;
    }
    Ok(())
}

/// BGRA at `w`×`h` to RGBA at `ow`×`oh`, each pixel the average of the ones it covers.
fn shrink(bgra: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> Vec<u8> {
    let (w, h, ow, oh) = (w as usize, h as usize, ow as usize, oh as usize);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        let (y0, y1) = (y * h / oh, ((y + 1) * h / oh).max(y * h / oh + 1));
        for x in 0..ow {
            let (x0, x1) = (x * w / ow, ((x + 1) * w / ow).max(x * w / ow + 1));
            let mut sum = [0u32; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * w + sx) * 4;
                    sum[0] += bgra[i + 2] as u32;
                    sum[1] += bgra[i + 1] as u32;
                    sum[2] += bgra[i] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (y * ow + x) * 4;
            out[o] = (sum[0] / n) as u8;
            out[o + 1] = (sum[1] / n) as u8;
            out[o + 2] = (sum[2] / n) as u8;
            out[o + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_areas_shrink_to_the_longest_side_and_small_ones_stay() {
        assert_eq!(size(1920, 1080), (960, 540));
        assert_eq!(size(600, 1200), (480, 960));
        assert_eq!(size(400, 300), (400, 300));
    }

    #[test]
    fn shrinking_averages_and_swaps_to_rgba() {
        // 2×1 → 1×1: blue and red, both opaque.
        let out = shrink(&[255, 0, 0, 255, 0, 0, 255, 255], 2, 1, 1, 1);
        assert_eq!(out, [127, 0, 127, 255]);
    }

    #[test]
    fn writes_a_looping_animation_with_true_timing_and_no_repeated_frames() {
        let dir = std::env::temp_dir().join(format!("kadr-gif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.gif");
        let (w, h) = (64u32, 48u32);
        let solid = |b: u8, g: u8, r: u8| [b, g, r, 255].repeat((w * h) as usize);
        let gif = GifWriter::new(&path, w, h).unwrap();
        let ms = Duration::from_millis;
        gif.frame(&solid(0, 0, 255), ms(0));
        // The same picture again: it only lengthens the first frame.
        gif.frame(&solid(0, 0, 255), ms(66));
        gif.frame(&solid(255, 0, 0), ms(500));
        gif.finish(ms(1200)).unwrap();

        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!((decoder.width(), decoder.height()), (64, 48));
        let mut frames = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            frames.push((frame.delay, frame.buffer[..4].to_vec()));
        }
        assert_eq!(frames, [(50, vec![255, 0, 0, 255]), (70, vec![0, 0, 255, 255])]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
