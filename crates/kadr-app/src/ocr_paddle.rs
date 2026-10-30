//! PaddleOCR inside Kadr: the PP-OCRv5 detector finds text lines, the East Slavic recogniser
//! (Russian, Ukrainian, Belarusian and Latin) reads them, both through ONNX Runtime loaded from
//! Kadr's own `ocr` folder. Works offline and on any Windows, with or without Windows' OCR packs.
//!
//! The pipeline follows PaddleOCR's own pre- and post-processing: DB detection on a probability
//! map, boxes widened by the «unclip» ratio, lines read at height 48 and decoded with CTC. Word
//! boxes come from the CTC time steps, so the censor can blur a single word of a line.
use crate::ocr::Line;
use kadr_doc::{Image, Rect};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Detector input: the longer side at most this many pixels (multiples of 32).
const DET_MAX_SIDE: f32 = 1920.0;
const DET_THRESHOLD: f32 = 0.3;
const BOX_THRESHOLD: f32 = 0.6;
const UNCLIP: f32 = 1.5;
const REC_HEIGHT: usize = 48;
const REC_MIN_WIDTH: usize = 320;

struct Engine {
    det: Session,
    rec: Session,
    /// CTC classes: 0 is the blank, then the dictionary, then a space.
    chars: Vec<String>,
}

/// Where Kadr keeps PaddleOCR: `ocr` next to the program; in a development build the fetched
/// copy in `third_party/ocr`.
pub fn folder() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let near = exe.parent()?.join("ocr");
    let dev = exe
        .ancestors()
        .skip(2)
        .take(3)
        .map(|a| a.join("third_party").join("ocr"));
    std::iter::once(near).chain(dev).find(|d| {
        ["det.onnx", "rec.onnx", "dict.txt", "onnxruntime.dll"]
            .iter()
            .all(|f| d.join(f).is_file())
    })
}

pub fn available() -> bool {
    folder().is_some()
}

fn engine() -> Result<&'static Mutex<Engine>, String> {
    static ENGINE: OnceLock<Result<Mutex<Engine>, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| load().map(Mutex::new))
        .as_ref()
        .map_err(Clone::clone)
}

fn load() -> Result<Engine, String> {
    let dir = folder().ok_or("PaddleOCR не установлен: нет папки ocr рядом с Kadr.")?;
    let env = ort::init_from(dir.join("onnxruntime.dll"))
        .map_err(|e| format!("ONNX Runtime: {e}"))?
        .with_name("kadr-ocr")
        .build()
        .map_err(|e| format!("ONNX Runtime: {e}"))?;
    // Run inference on the low-priority caller. Dedicated pools in both sessions otherwise
    // compete with animation, including spin-waiting while the other session is active.
    let threads = 1;
    let session = |file: &str| -> Result<Session, String> {
        let builder = Session::builder(&env).map_err(|e| e.to_string())?;
        let builder = builder
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?;
        let builder = builder.with_intra_threads(threads).map_err(|e| e.to_string())?;
        let builder = builder.with_inter_threads(1).map_err(|e| e.to_string())?;
        let builder = builder.with_intra_op_spinning(false).map_err(|e| e.to_string())?;
        let mut builder = builder.with_inter_op_spinning(false).map_err(|e| e.to_string())?;
        builder
            .commit_from_file(dir.join(file))
            .map_err(|e| format!("{file}: {e}"))
    };
    let det = session("det.onnx")?;
    let rec = session("rec.onnx")?;
    let dict = std::fs::read_to_string(dir.join("dict.txt")).map_err(|e| e.to_string())?;
    let mut chars = vec![String::new()];
    chars.extend(dict.lines().map(|l| l.trim_end_matches('\r').to_string()));
    chars.push(" ".into());
    Ok(Engine { det, rec, chars })
}

/// Reads `image` into lines with word boxes, in the image's own (desktop) coordinates.
pub fn recognize(image: &Image) -> Result<Vec<Line>, String> {
    let engine = engine()?;
    let mut engine = engine.lock().map_err(|_| "PaddleOCR занят".to_string())?;
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w < 4 || h < 4 {
        return Ok(Vec::new());
    }
    let boxes = detect(&mut engine, image)?;
    let mut lines = Vec::new();
    for b in boxes {
        if let Some(line) = read(&mut engine, image, b)? {
            lines.push(line);
        }
    }
    lines.sort_by_key(|l| (l.rect.top / 8, l.rect.left));
    Ok(lines)
}

/// Detect text bounds without reading every character. Full censor does not need recognition.
pub fn regions(image: &Image) -> Result<Vec<Rect>, String> {
    let mut engine=engine()?.lock().map_err(|_| "PaddleOCR занят".to_string())?;
    if image.width()<4 || image.height()<4 {return Ok(Vec::new());}
    let mut result=Vec::new();
    // Native-size overlapping tiles keep small text legible to the detector on large displays.
    let tile=1600i32; let step=tile-128;
    let mut top=image.rect.top;
    while top<image.rect.bottom {
        let mut left=image.rect.left;
        while left<image.rect.right {
            let area=Rect::new(left,top,(left+tile).min(image.rect.right),(top+tile).min(image.rect.bottom));
            let patch=Image::compose([image],area);
            for (l,t,r,b) in detect(&mut engine,&patch)? {
                let pad=((b-t)*0.15).ceil().max(3.0) as i32;
                let rect=Rect::new(area.left+l.floor() as i32-pad,area.top+t.floor() as i32-pad,area.left+r.ceil() as i32+pad,area.top+b.ceil() as i32+pad);
                if let Some(rect)=rect.intersect(&image.rect) { result.push(rect); }
            }
            if area.right==image.rect.right {break;} left+=step;
        }
        if top+tile>=image.rect.bottom {break;} top+=step;
    }
    result.sort_by_key(|r|(r.top/8,r.left)); result.dedup();
    Ok(result)
}

/// The pixel at (x, y) as B, G, R in 0..=255.
fn bgr(image: &Image, x: usize, y: usize) -> [f32; 3] {
    let i = (y * image.width() as usize + x) * 4;
    let p = &image.bgra[i..i + 3];
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

/// Bilinear sample at a fractional position, clamped to the image.
fn sample(image: &Image, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let (a, b, c, d) = (
        bgr(image, x0, y0),
        bgr(image, x1, y0),
        bgr(image, x0, y1),
        bgr(image, x1, y1),
    );
    let mut out = [0.0; 3];
    for k in 0..3 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        out[k] = top + (bottom - top) * fy;
    }
    out
}

/// Box in image pixels (left, top, right, bottom), not yet offset to the desktop.
type PixelBox = (f32, f32, f32, f32);

fn detect(engine: &mut Engine, image: &Image) -> Result<Vec<PixelBox>, String> {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let k = (DET_MAX_SIDE / w.max(h)).min(1.0);
    let round32 = |v: f32| (((v * k) / 32.0).round() as usize).max(1) * 32;
    let (dw, dh) = (round32(w), round32(h));
    let (sx, sy) = (w / dw as f32, h / dh as f32);
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let mut input = vec![0f32; 3 * dw * dh];
    for y in 0..dh {
        for x in 0..dw {
            let p = sample(
                image,
                (x as f32 + 0.5) * sx - 0.5,
                (y as f32 + 0.5) * sy - 0.5,
            );
            for c in 0..3 {
                input[c * dw * dh + y * dw + x] = (p[c] / 255.0 - mean[c]) / std[c];
            }
        }
    }
    let tensor = Tensor::from_array(([1usize, 3, dh, dw], input)).map_err(|e| e.to_string())?;
    let outputs = engine
        .det
        .run(ort::inputs![tensor])
        .map_err(|e| format!("поиск строк: {e}"))?;
    let (shape, prob) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| e.to_string())?;
    let (mh, mw) = (
        shape[shape.len() - 2] as usize,
        shape[shape.len() - 1] as usize,
    );
    let boxes = boxes_from_map(prob, mw, mh);
    // Back to image pixels.
    let (bx, by) = (w / mw as f32, h / mh as f32);
    Ok(boxes
        .into_iter()
        .map(|(l, t, r, b)| {
            (
                (l * bx).max(0.0),
                (t * by).max(0.0),
                (r * bx).min(w),
                (b * by).min(h),
            )
        })
        .collect())
}

/// DB post-processing for upright text: connected regions above the threshold, kept when their
/// box is confident enough, then widened by the unclip distance.
pub fn boxes_from_map(prob: &[f32], w: usize, h: usize) -> Vec<PixelBox> {
    let mut label = vec![0u32; w * h];
    let mut next = 0u32;
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if label[start] != 0 || prob[start] <= DET_THRESHOLD {
            continue;
        }
        next += 1;
        label[start] = next;
        stack.push(start);
        let (mut l, mut t, mut r, mut b) = (usize::MAX, usize::MAX, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            l = l.min(x);
            r = r.max(x);
            t = t.min(y);
            b = b.max(y);
            let mut visit = |j: usize| {
                if label[j] == 0 && prob[j] > DET_THRESHOLD {
                    label[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        let (bw, bh) = ((r - l + 1) as f32, (b - t + 1) as f32);
        if bw.min(bh) < 3.0 {
            continue;
        }
        let mut sum = 0.0;
        for y in t..=b {
            sum += prob[y * w + l..=y * w + r].iter().sum::<f32>();
        }
        if sum / (bw * bh) < BOX_THRESHOLD {
            continue;
        }
        let d = bw * bh * UNCLIP / (2.0 * (bw + bh));
        out.push((
            l as f32 - d,
            t as f32 - d,
            r as f32 + 1.0 + d,
            b as f32 + 1.0 + d,
        ));
    }
    out
}

fn read(
    engine: &mut Engine,
    image: &Image,
    (l, t, r, b): PixelBox,
) -> Result<Option<Line>, String> {
    let (cw, ch) = (r - l, b - t);
    if cw < 2.0 || ch < 2.0 {
        return Ok(None);
    }
    // Height 48, width by the line's aspect; short lines padded to the model's usual 320.
    let resized = ((REC_HEIGHT as f32 * cw / ch).ceil() as usize).clamp(1, 4096);
    let width = resized.max(REC_MIN_WIDTH);
    let mut input = vec![0f32; 3 * REC_HEIGHT * width];
    let (sx, sy) = (cw / resized as f32, ch / REC_HEIGHT as f32);
    for y in 0..REC_HEIGHT {
        for x in 0..resized {
            let p = sample(
                image,
                l + (x as f32 + 0.5) * sx - 0.5,
                t + (y as f32 + 0.5) * sy - 0.5,
            );
            for c in 0..3 {
                input[c * REC_HEIGHT * width + y * width + x] = (p[c] / 255.0 - 0.5) / 0.5;
            }
        }
    }
    let tensor =
        Tensor::from_array(([1usize, 3, REC_HEIGHT, width], input)).map_err(|e| e.to_string())?;
    let outputs = engine
        .rec
        .run(ort::inputs![tensor])
        .map_err(|e| format!("чтение строки: {e}"))?;
    let (shape, probs) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| e.to_string())?;
    let (steps, classes) = (shape[1] as usize, shape[2] as usize);
    let decoded = decode(probs, steps, classes, &engine.chars);
    if decoded.text.trim().is_empty() || decoded.confidence < 0.5 {
        return Ok(None);
    }
    // Time step → x in the line box.
    let step_w = width as f32 / steps as f32 * sx;
    let origin = (image.rect.left as f32, image.rect.top as f32);
    let to_rect = |x0: f32, x1: f32| {
        Rect::new(
            (origin.0 + l + x0).floor() as i32,
            (origin.1 + t).floor() as i32,
            (origin.0 + l + x1).ceil() as i32,
            (origin.1 + b).ceil() as i32,
        )
    };
    let words = words(&decoded)
        .into_iter()
        .map(|(word, first, last)| {
            (
                one_script(&word),
                to_rect(first as f32 * step_w, ((last + 1) as f32 * step_w).min(cw)),
            )
        })
        .collect::<Vec<_>>();
    let text = words
        .iter()
        .map(|(w, _)| w.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some(Line {
        text,
        rect: to_rect(0.0, cw),
        words,
    }))
}

pub struct Decoded {
    pub text: String,
    /// Each character with the time step it was read at.
    pub chars: Vec<(char, usize)>,
    pub confidence: f32,
}

/// Greedy CTC: the likeliest class per step, repeats merged, blanks dropped.
pub fn decode(probs: &[f32], steps: usize, classes: usize, chars: &[String]) -> Decoded {
    let mut out = Vec::new();
    let mut scores = Vec::new();
    let mut previous = 0usize;
    for s in 0..steps {
        let row = &probs[s * classes..(s + 1) * classes];
        let (best, p) =
            row.iter().enumerate().fold(
                (0, f32::MIN),
                |acc, (i, &v)| if v > acc.1 { (i, v) } else { acc },
            );
        if best != 0
            && best != previous
            && let Some(c) = chars.get(best).and_then(|c| c.chars().next())
        {
            out.push((c, s));
            scores.push(p);
        }
        previous = best;
    }
    let confidence = if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    };
    Decoded {
        text: out.iter().map(|(c, _)| c).collect(),
        chars: out,
        confidence,
    }
}

/// Latin and Cyrillic letters that look alike, the pairs the recogniser confuses.
const TWINS: [(char, char); 27] = [
    ('a', 'а'),
    ('b', 'Ь'),
    ('c', 'с'),
    ('e', 'е'),
    ('i', 'і'),
    ('j', 'ј'),
    ('k', 'к'),
    ('m', 'м'),
    ('o', 'о'),
    ('p', 'р'),
    ('r', 'г'),
    ('s', 'ѕ'),
    ('t', 'т'),
    ('x', 'х'),
    ('y', 'у'),
    ('A', 'А'),
    ('B', 'В'),
    ('C', 'С'),
    ('E', 'Е'),
    ('H', 'Н'),
    ('K', 'К'),
    ('M', 'М'),
    ('O', 'О'),
    ('P', 'Р'),
    ('T', 'Т'),
    ('X', 'Х'),
    ('Y', 'У'),
];

fn cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04ff}').contains(&c)
}

/// One script per word: a word written in both (`cоm`, `Тeлефон`) takes the script most of its
/// letters are in; an address, a handle or a link (`@`, `_`, letters with digits) is Latin.
pub fn one_script(word: &str) -> String {
    let latin = word.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let cyr = word.chars().filter(|c| cyrillic(*c)).count();
    if latin == 0 && cyr == 0 {
        return word.to_string();
    }
    let digits = word.chars().any(|c| c.is_ascii_digit());
    let technical = word.contains('@')
        || word.contains('_')
        || word.contains("://")
        || (digits && (latin + cyr) >= 2);
    let to_latin = if technical {
        true
    } else if latin == 0 || cyr == 0 {
        return word.to_string();
    } else {
        latin >= cyr
    };
    word.chars()
        .map(|c| {
            TWINS
                .iter()
                .find(|(l, k)| if to_latin { *k == c } else { *l == c })
                .map_or(c, |(l, k)| if to_latin { *l } else { *k })
        })
        .collect()
}

/// Words with the first and last time step of their letters.
pub fn words(d: &Decoded) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut current: Option<(String, usize, usize)> = None;
    for &(c, s) in &d.chars {
        if c.is_whitespace() {
            out.extend(current.take());
            continue;
        }
        match &mut current {
            Some((w, _, last)) => {
                w.push(c);
                *last = s;
            }
            None => current = Some((c.to_string(), s, s)),
        }
    }
    out.extend(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ctc_merges_repeats_drops_blanks_and_keeps_where_each_letter_was() {
        let chars: Vec<String> = ["", "a", "b", " "].iter().map(|s| s.to_string()).collect();
        // Steps: a a _ b _ space a
        let classes = 4;
        let pick = [1, 1, 0, 2, 0, 3, 1];
        let mut probs = vec![0.0; pick.len() * classes];
        for (s, &c) in pick.iter().enumerate() {
            probs[s * classes + c] = 0.9;
        }
        let d = decode(&probs, pick.len(), classes, &chars);
        assert_eq!(d.text, "ab a");
        assert_eq!(
            words(&d),
            [("ab".to_string(), 0, 3), ("a".to_string(), 6, 6)]
        );
        assert!((d.confidence - 0.9).abs() < 1e-5);
    }
    #[test]
    fn a_confident_region_becomes_a_widened_box_and_noise_does_not() {
        let (w, h) = (40, 20);
        let mut prob = vec![0.0f32; w * h];
        for y in 5..10 {
            for x in 4..30 {
                prob[y * w + x] = 0.9;
            }
        }
        prob[18 * w + 38] = 0.95;
        let boxes = boxes_from_map(&prob, w, h);
        assert_eq!(boxes.len(), 1);
        let (l, t, r, b) = boxes[0];
        assert!(l < 4.0 && t < 5.0 && r > 30.0 && b > 10.0, "{:?}", boxes[0]);
    }
    #[test]
    fn a_word_keeps_one_script() {
        assert_eq!(one_script("gmail.cоm"), "gmail.com");
        assert_eq!(
            one_script("daniil.petrov@gmail.cоm"),
            "daniil.petrov@gmail.com"
        );
        assert_eq!(one_script("гартог_92"), "raptor_92");
        assert_eq!(one_script("Тeлефон"), "Телефон");
        assert_eq!(one_script("Тeлeфон"), "Телефон");
        // Whole words in one script stay as they are, even made of look-alikes.
        assert_eq!(one_script("сок"), "сок");
        assert_eq!(one_script("coke"), "coke");
        assert_eq!(one_script("2026"), "2026");
    }
}
