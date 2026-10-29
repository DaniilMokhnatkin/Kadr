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
    Ok(merge(readings(image)?))
}

/// Every installed recognizer's reading; the flag marks those that read Cyrillic.
fn readings(image: &Image) -> Result<Vec<(bool, Vec<Line>)>> {
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
    let mut readings = Vec::new();
    for (cyrillic, engine) in engines()? {
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
        readings.push((cyrillic, out));
    }
    Ok(readings)
}

/// Words written wholly in one script (Cyrillic or Latin) with no digits inside: real words.
/// Misreadings mix scripts («Неllо») or swap letters for digits («CKonhp0BaTb»).
fn clean_words(text: &str, cyrillic_script: bool) -> usize {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().any(char::is_alphabetic))
        .filter(|w| {
            w.chars().all(|c| {
                let cyr = ('а'..='я').contains(&c.to_lowercase().next().unwrap_or(c)) || "ёєіїўђћјљњџ".contains(c.to_lowercase().next().unwrap_or(c));
                c.is_alphabetic() && cyr == cyrillic_script
            })
        })
        .count()
}

fn overlap(a: Rect, b: Rect) -> f32 {
    let Some(i) = a.intersect(&b) else { return 0.0 };
    let area = |r: Rect| r.width() as f32 * r.height() as f32;
    area(i) / (area(a) + area(b) - area(i)).max(1.0)
}

/// One reading from two recognizers, line by line. The Latin-only recognizer turns Cyrillic into
/// look-alike letters and digits («CKonhp0BaTb»), so it may never win on a Cyrillic line; the
/// Cyrillic recognizer is weaker on English, so a Latin line takes the Latin reading.
fn merge(readings: Vec<(bool, Vec<Line>)>) -> Vec<Line> {
    let latin = readings.iter().find(|(c, _)| !c).map(|(_, l)| l.clone());
    let Some(mut base) = readings.into_iter().find(|(c, _)| *c).map(|(_, l)| l) else {
        return latin.unwrap_or_default();
    };
    let Some(latin) = latin else { return base };
    let mut used = vec![false; latin.len()];
    for line in &mut base {
        let best = latin
            .iter()
            .enumerate()
            .map(|(i, l)| (i, overlap(line.rect, l.rect)))
            .filter(|(_, o)| *o > 0.3)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            used[i] = true;
            let words = line.text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().any(char::is_alphabetic)).count();
            // Mostly whole Cyrillic words: a Russian line. Otherwise the Latin reader knows better.
            if clean_words(&line.text, true) * 2 <= words {
                *line = latin[i].clone();
            }
        }
    }
    // Lines only the Latin recognizer saw, as long as they are not Cyrillic in disguise.
    base.extend(latin.into_iter().zip(used).filter(|(_, u)| !u).map(|(l, _)| l));
    base.sort_by_key(|l| (l.rect.top / 4, l.rect.left));
    base
}

/// The user's own recognizer first, then English and Russian if installed and different.
/// The flag marks recognizers that read Cyrillic.
fn engines() -> Result<Vec<(bool, OcrEngine)>> {
    let reads_cyrillic = |tag: &str| ["ru", "uk", "be", "bg", "sr", "kk"].iter().any(|p| tag.starts_with(p));
    let mut out = Vec::new();
    let mut tags = Vec::new();
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        let tag = engine.RecognizerLanguage()?.LanguageTag()?.to_string();
        out.push((reads_cyrillic(&tag), engine));
        tags.push(tag);
    }
    for tag in ["en-US", "ru-RU"] {
        let language = Language::CreateLanguage(&HSTRING::from(tag))?;
        let primary = tag.split('-').next().unwrap();
        if tags.iter().any(|t| t.starts_with(primary)) || !OcrEngine::IsLanguageSupported(&language)? {
            continue;
        }
        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
            out.push((reads_cyrillic(tag), engine));
            tags.push(tag.into());
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
    #[test]
    fn merge_keeps_cyrillic_lines_from_the_cyrillic_reader_and_latin_from_the_latin_one() {
        let ru = vec![
            line("Скопировать текст", 10, 10, 300, 30),
            line("Неllо wоrld", 10, 50, 300, 70),
        ];
        let en = vec![
            line("CKonhp0BaTb TeKCT", 12, 11, 298, 31),
            line("Hello world", 10, 50, 300, 70),
            line("only latin saw this", 10, 90, 300, 110),
        ];
        let merged = merge(vec![(true, ru), (false, en)]);
        let texts: Vec<_> = merged.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["Скопировать текст", "Hello world", "only latin saw this"]);
    }
    #[test]
    fn reads_russian_and_mixed_text_without_latin_look_alikes() {
        let Ok(engines) = engines() else { return };
        if !engines.iter().any(|(c, _)| *c) {
            return;
        }
        let all = text(&recognize(&render("Скопировать текст
Hello Kadr 2026")).unwrap());
        assert!(all.contains("Скопировать") && all.contains("текст"), "{all:?}");
        assert!(all.contains("Hello") && all.contains("2026"), "{all:?}");
    }
    /// White text image via GDI, offset on the desktop to check coordinate mapping.
    #[test]
    fn dark_russian_with_english_words_keeps_the_cyrillic_reading() {
        let Ok(engines) = engines() else { return };
        if !engines.iter().any(|(c, _)| *c) {
            return;
        }
        let image = render_with(
            "Перевод починил. Изменения закоммичены, новая сборка лежит в dist/Kadr.exe, тесты (117) и
clippy проходят. На настоящем экране не проверял, только через живые запросы к Google
Русские строки остаются как есть. Строка считается русской, если больше половины её",
            19,
            true,
        );
        let all = text(&recognize(&image).unwrap());
        for phrase in ["Перевод починил", "проходят", "Русские строки остаются"] {
            assert!(all.contains(phrase), "{phrase:?} in {all:?}");
        }
    }
    fn render(value: &str) -> Image {
        render_with(value, 32, false)
    }
    /// `px` font height; `dark` — light text on a dark background, as in a dark chat window.
    fn render_with(value: &str, px: i32, dark: bool) -> Image {
        use windows::Win32::Graphics::Gdi::*;
        let (w, h) = (1400, 40 + (px + px / 2) * value.lines().count() as i32);
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
            if dark {
                let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x1f1f1f));
                FillRect(dc, &all, brush);
                let _ = DeleteObject(brush.into());
                SetTextColor(dc, windows::Win32::Foundation::COLORREF(0xe8e8e8));
                SetBkMode(dc, TRANSPARENT);
            } else {
                FillRect(dc, &all, HBRUSH(GetStockObject(WHITE_BRUSH).0));
            }
            let font = CreateFontW(-px, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, windows::core::w!("Segoe UI"));
            let old_font = SelectObject(dc, font.into());
            let mut wide: Vec<u16> = value.encode_utf16().collect();
            let mut r = windows::Win32::Foundation::RECT { left: 16, top: 16, right: w, bottom: h };
            DrawTextW(dc, &mut wide, &mut r, DT_LEFT | DT_TOP);
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
