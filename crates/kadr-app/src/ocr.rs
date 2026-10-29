//! Text recognition with the OCR built into Windows (Windows.Media.Ocr): local, no network.
//! Each installed recognizer language is tried and the richest reading wins, so English text is
//! read well on a Russian system and the other way round.
use kadr_doc::{Image, Point, Rect};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::core::{HSTRING, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    /// In the coordinates of the recognised image's `rect` (desktop pixels).
    pub rect: Rect,
}

/// Recognises `image`; lines come back in reading order with desktop coordinates.
pub fn recognize(image: &Image) -> Result<Vec<Line>> {
    let max = OcrEngine::MaxImageDimension()? as i32;
    // Very large areas are scaled to the engine's limit; coordinates are mapped back.
    let (source, k) = if image.rect.width() > max || image.rect.height() > max {
        let k = max as f64 / image.rect.width().max(image.rect.height()) as f64;
        let small = kadr_doc::pixels::thumbnail(
            &[std::sync::Arc::new(image.clone())],
            image.rect,
            (image.width() as f64 * k) as u32,
            (image.height() as f64 * k) as u32,
        );
        (small, k)
    } else {
        (Image::new(Rect::new(0, 0, image.rect.width(), image.rect.height()), image.bgra.clone()), 1.0)
    };
    let buffer = CryptographicBuffer::CreateFromByteArray(&source.bgra)?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        source.rect.width(),
        source.rect.height(),
        BitmapAlphaMode::Ignore,
    )?;
    let mut best: Option<(usize, Vec<Line>)> = None;
    for engine in engines()? {
        let result = engine.RecognizeAsync(&bitmap)?.join()?;
        let lines = result.Lines()?;
        let mut out = Vec::new();
        for i in 0..lines.Size()? {
            let line = lines.GetAt(i)?;
            let words = line.Words()?;
            let mut bounds: Option<Rect> = None;
            for j in 0..words.Size()? {
                let r = words.GetAt(j)?.BoundingRect()?;
                let rect = Rect::new(
                    image.rect.left + (r.X as f64 / k) as i32,
                    image.rect.top + (r.Y as f64 / k) as i32,
                    image.rect.left + ((r.X + r.Width) as f64 / k).ceil() as i32,
                    image.rect.top + ((r.Y + r.Height) as f64 / k).ceil() as i32,
                );
                bounds = Some(bounds.map_or(rect, |b| b.union(&rect)));
            }
            if let Some(rect) = bounds {
                out.push(Line { text: line.Text()?.to_string(), rect });
            }
        }
        let score = out.iter().map(|l| l.text.chars().filter(|c| c.is_alphanumeric()).count()).sum();
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, out));
        }
    }
    Ok(best.map(|(_, l)| l).unwrap_or_default())
}

/// The user's own recognizer first, then English and Russian if installed and different.
fn engines() -> Result<Vec<OcrEngine>> {
    let mut out = Vec::new();
    let mut tags = Vec::new();
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        tags.push(engine.RecognizerLanguage()?.LanguageTag()?.to_string());
        out.push(engine);
    }
    for tag in ["en-US", "ru-RU"] {
        let language = Language::CreateLanguage(&HSTRING::from(tag))?;
        let primary = tag.split('-').next().unwrap();
        if tags.iter().any(|t| t.starts_with(primary)) || !OcrEngine::IsLanguageSupported(&language)? {
            continue;
        }
        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
            tags.push(tag.into());
            out.push(engine);
        }
    }
    Ok(out)
}

/// All lines joined: one paragraph per visual block, separated by blank lines.
pub fn text(lines: &[Line]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let prev = &lines[i - 1];
            out.push_str(if same_block(prev, line) { "\n" } else { "\n\n" });
        }
        out.push_str(&line.text);
    }
    out
}

fn same_block(a: &Line, b: &Line) -> bool {
    let h = a.rect.height().max(b.rect.height()).max(1);
    let gap = b.rect.top - a.rect.bottom;
    let aligned = (a.rect.left - b.rect.left).abs() < h * 3 || a.rect.intersect(&Rect::new(b.rect.left, a.rect.top, b.rect.right, a.rect.bottom)).is_some();
    (-h / 2..=h).contains(&gap) && aligned
}

/// The paragraph under `p`: its line plus the neighbours that continue it above and below.
/// Joined with spaces, since a wrapped paragraph is one sentence for the translator.
pub fn paragraph_at(lines: &[Line], p: Point) -> Option<(String, Rect)> {
    let near = |l: &Line| {
        let r = l.rect;
        let pad = r.height() / 2;
        Rect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad).contains(p)
    };
    let hit = lines.iter().position(near)?;
    let (mut first, mut last) = (hit, hit);
    while first > 0 && same_block(&lines[first - 1], &lines[first]) {
        first -= 1;
    }
    while last + 1 < lines.len() && same_block(&lines[last], &lines[last + 1]) {
        last += 1;
    }
    let block = &lines[first..=last];
    let text = block.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
    let rect = block.iter().skip(1).fold(block[0].rect, |r, l| r.union(&l.rect));
    Some((text, rect))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(text: &str, l: i32, t: i32, r: i32, b: i32) -> Line {
        Line { text: text.into(), rect: Rect::new(l, t, r, b) }
    }
    #[test]
    fn paragraph_under_the_click_joins_its_wrapped_lines_only() {
        let lines = vec![
            line("Title", 10, 10, 200, 40),
            line("The quick brown fox", 10, 80, 400, 100),
            line("jumps over the lazy dog.", 10, 104, 420, 124),
            line("Another paragraph", 10, 170, 300, 190),
        ];
        let (text, rect) = paragraph_at(&lines, Point::new(50, 110)).unwrap();
        assert_eq!(text, "The quick brown fox jumps over the lazy dog.");
        assert_eq!(rect, Rect::new(10, 80, 420, 124));
        assert_eq!(paragraph_at(&lines, Point::new(600, 600)), None);
        assert_eq!(text_of(&lines), "Title\n\nThe quick brown fox\njumps over the lazy dog.\n\nAnother paragraph");
    }
    fn text_of(lines: &[Line]) -> String {
        text(lines)
    }
    #[test]
    fn recognizes_rendered_text_with_the_windows_engine() {
        // Skipped where no recognizer language is installed (some CI images).
        let Ok(engines) = engines() else { return };
        if engines.is_empty() {
            return;
        }
        let image = render("Hello Kadr 2026");
        let lines = recognize(&image).unwrap();
        let all = text(&lines);
        assert!(all.contains("Hello") && all.contains("2026"), "{all:?}");
        assert!(lines[0].rect.left >= image.rect.left && lines[0].rect.right <= image.rect.right);
    }
    /// White text image via GDI, offset on the desktop to check coordinate mapping.
    fn render(value: &str) -> Image {
        use windows::Win32::Graphics::Gdi::*;
        let (w, h) = (360, 80);
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bitmap.into());
            let all = windows::Win32::Foundation::RECT { left: 0, top: 0, right: w, bottom: h };
            FillRect(dc, &all, HBRUSH(GetStockObject(WHITE_BRUSH).0));
            let font = CreateFontW(-32, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, windows::core::w!("Segoe UI"));
            let old_font = SelectObject(dc, font.into());
            let mut wide: Vec<u16> = value.encode_utf16().collect();
            let mut r = windows::Win32::Foundation::RECT { left: 16, top: 16, right: w, bottom: h };
            DrawTextW(dc, &mut wide, &mut r, DT_LEFT | DT_TOP | DT_SINGLELINE);
            let _ = GdiFlush();
            let mut px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
            for p in px.as_chunks_mut::<4>().0 {
                p[3] = 255;
            }
            SelectObject(dc, old_font);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            Image::new(Rect::new(-500, 300, -500 + w, 300 + h), px)
        }
    }
}
