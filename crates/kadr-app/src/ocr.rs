//! Text recognition, local and without network, by one of two engines: PaddleOCR shipped with
//! Kadr (see `ocr_paddle`), or the OCR built into Windows (Windows.Media.Ocr). The setup picks one,
//! Settings can change it, and when the chosen one is missing the other steps in.
//!
//! With Windows' OCR each installed recognizer language is tried and the readings are merged word
//! by word, so English text is read well on a Russian system and the other way round.
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
    /// Each word with its own box, for marking exactly the sensitive part of a line.
    pub words: Vec<(String, Rect)>,
}

/// Settings › recognition: 0 auto, 1 Windows, 2 PaddleOCR.
static CHOSEN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn choose(engine: kadr_store::config::OcrEngine) {
    use kadr_store::config::OcrEngine;
    let v = match engine {
        OcrEngine::Auto => 0,
        OcrEngine::Windows => 1,
        OcrEngine::Paddle => 2,
    };
    CHOSEN.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Whether PaddleOCR is the one wanted: Settings first; on «auto», what the setup chose (it leaves
/// `ocr-engine.txt` next to Kadr); with neither, PaddleOCR when it is there.
fn wants_paddle() -> bool {
    match CHOSEN.load(std::sync::atomic::Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => std::env::current_exe()
            .ok()
            .and_then(|e| std::fs::read_to_string(e.with_file_name("ocr-engine.txt")).ok())
            .is_none_or(|s| s.trim() != "windows"),
    }
}

/// Recognises `image`; lines come back in reading order with desktop coordinates.
pub fn recognize(image: &Image) -> Result<Vec<Line>> {
    let paddle = crate::ocr_paddle::available();
    let windows = engines().is_ok_and(|e| !e.is_empty());
    // The wanted engine if it is there, else whichever is.
    let use_paddle = paddle && (wants_paddle() || !windows);
    if use_paddle {
        return crate::ocr_paddle::recognize(image).map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e));
    }
    recognize_windows(image)
}

/// All text bounds, even when the recognizer cannot decode the letters.
pub fn text_regions(image: &Image) -> Result<Vec<Rect>> {
    if crate::ocr_paddle::available() {
        match crate::ocr_paddle::regions(image) {
            Ok(regions) if !regions.is_empty()=>return Ok(regions),
            Ok(_)=>{},
            Err(e)=>kadr_win::log_warn!("поиск текстовых областей: {e}"),
        }
    }
    Ok(crate::sensitive::find_all(&recognize_windows(image)?).into_iter().map(|(_,rect)|rect).collect())
}

/// Windows' OCR alone.
pub fn recognize_windows(image: &Image) -> Result<Vec<Line>> {
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
    let engines = engines()?;
    if engines.is_empty() {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "Нет распознавания текста: ни языковых пакетов Windows, ни PaddleOCR. Запустите установщик Kadr и выберите PaddleOCR — он работает без пакетов Windows и без интернета.",
        ));
    }
    for (cyrillic, engine) in engines {
        let result = engine.RecognizeAsync(&bitmap)?.join()?;
        let lines = result.Lines()?;
        let mut out = Vec::new();
        for i in 0..lines.Size()? {
            let line = lines.GetAt(i)?;
            let words = line.Words()?;
            let mut bounds: Option<Rect> = None;
            let mut boxes = Vec::new();
            for j in 0..words.Size()? {
                let word = words.GetAt(j)?;
                let r = word.BoundingRect()?;
                let rect = Rect::new(
                    image.rect.left + (r.X as f64 / k) as i32,
                    image.rect.top + (r.Y as f64 / k) as i32,
                    image.rect.left + ((r.X + r.Width) as f64 / k).ceil() as i32,
                    image.rect.top + ((r.Y + r.Height) as f64 / k).ceil() as i32,
                );
                bounds = Some(bounds.map_or(rect, |b| b.union(&rect)));
                boxes.push((word.Text()?.to_string(), rect));
            }
            if let Some(rect) = bounds {
                out.push(Line { text: line.Text()?.to_string(), rect, words: boxes });
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

/// A word the Latin recognizer read as a believable Latin word: letters with normal casing
/// («raptor», «Raptor», «KADR»), digits only around them («Win11»). Its readings of Cyrillic
/// break these rules («norv1H», «napoJlb», «TeKCT»).
fn plausible_latin(w: &str) -> bool {
    let w = w.trim_matches(|c: char| !c.is_alphanumeric());
    let letters: String = w.chars().filter(char::is_ascii_alphabetic).collect();
    if letters.is_empty() || !w.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let core = w.trim_matches(|c: char| c.is_ascii_digit());
    let digits_inside = core.chars().any(|c| c.is_ascii_digit());
    let rest = &letters[1..];
    let casing = rest.chars().all(|c| c.is_ascii_lowercase()) || letters.chars().all(|c| c.is_ascii_uppercase());
    !digits_inside && casing
}

fn whole_cyrillic(w: &str) -> bool {
    let w = w.trim_matches(|c: char| !c.is_alphanumeric());
    !w.is_empty()
        && w.chars().all(|c| {
            let l = c.to_lowercase().next().unwrap_or(c);
            ('а'..='я').contains(&l) || "ёєіїўђћјљњџ".contains(l)
        })
}

/// One reading from two recognizers, word by word. The Latin-only recognizer turns Cyrillic into
/// look-alikes («CKonhp0BaTb», «norv1H»); the Cyrillic one turns English into Cyrillic
/// look-alikes («Немо» for «Hello»). Each word takes the reading that is a real word in its
/// script; when both are, the line's clear words decide. So «Логин Raptor» stays as written.
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
        let Some((i, _)) = best else { continue };
        used[i] = true;
        let other = &latin[i];
        // Pair every word with the Latin reading of the same spot.
        let pairs: Vec<(usize, Option<usize>)> = line
            .words
            .iter()
            .enumerate()
            .map(|(k, (_, r))| {
                let m = other
                    .words
                    .iter()
                    .enumerate()
                    .map(|(j, (_, b))| (j, overlap(*r, *b)))
                    .filter(|(_, o)| *o > 0.3)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(j, _)| j);
                (k, m)
            })
            .collect();
        let verdict = |k: usize, m: Option<usize>| -> (bool, bool) {
            let cyr = whole_cyrillic(&line.words[k].0);
            let lat = m.is_some_and(|j| plausible_latin(&other.words[j].0));
            (cyr, lat)
        };
        let cyr_clear = pairs.iter().filter(|(k, m)| matches!(verdict(*k, *m), (true, false))).count();
        let lat_clear = pairs.iter().filter(|(k, m)| matches!(verdict(*k, *m), (false, true))).count();
        let mut words = Vec::with_capacity(pairs.len());
        for (k, m) in pairs {
            let take_latin = match verdict(k, m) {
                (false, true) => true,
                (true, true) => lat_clear > cyr_clear,
                _ => false,
            };
            words.push(match (take_latin, m) {
                // The Latin reader may split what the other read as one word («raptor» + «_92,»):
                // every Latin piece centred inside this word belongs to it.
                (true, Some(_)) => {
                    let r = line.words[k].1;
                    let parts: Vec<&(String, Rect)> = other
                        .words
                        .iter()
                        .filter(|(_, b)| (r.left..=r.right).contains(&((b.left + b.right) / 2)) && r.intersect(b).is_some())
                        .collect();
                    let mut text = String::new();
                    for (n, (w, b)) in parts.iter().enumerate() {
                        if n > 0 && b.left - parts[n - 1].1.right > b.height() / 6 {
                            text.push(' ');
                        }
                        text.push_str(w);
                    }
                    (text, r)
                }
                _ => line.words[k].clone(),
            });
        }
        if line.words.is_empty() {
            // No word boxes (tests, odd engines): fall back to the whole line.
            if clean_words(&line.text, true) * 2 <= line.text.split_whitespace().count() {
                *line = other.clone();
            }
            continue;
        }
        line.text = words.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>().join(" ");
        line.words = words;
    }
    // Lines only the Latin recognizer saw.
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
        Line { text: text.into(), rect: Rect::new(l, t, r, b), words: Vec::new() }
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
    #[test]
    fn full_censor_detector_finds_small_russian_and_english_lines_without_decoding_words() {
        assert!(crate::ocr_paddle::available(),"test requires the bundled PaddleOCR models");
        for dark in [false,true] {
            let image=render_with("Логин Raptor\nThis is private text\nПароль test-123",16,dark);
            let started=std::time::Instant::now();
            let regions=crate::ocr_paddle::regions(&image).unwrap();
            assert!(regions.len()>=3,"{dark}: {regions:?}");
            assert!(regions.iter().all(|r|image.rect.intersect(r)==Some(*r)));
            eprintln!("full detector dark={dark}: {} regions, {:?}",regions.len(),started.elapsed());
        }
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
        let lines = recognize_windows(&image).unwrap();
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
        let all = text(&recognize_windows(&render("Скопировать текст
Hello Kadr 2026")).unwrap());
        assert!(all.contains("Скопировать") && all.contains("текст"), "{all:?}");
        assert!(all.contains("Hello") && all.contains("2026"), "{all:?}");
    }
    #[test]
    fn login_and_password_without_colons_are_found_after_real_recognition() {
        let Ok(engines) = engines() else { return };
        if !engines.iter().any(|(c, _)| *c) {
            return;
        }
        for dark in [false, true] {
            let lines = recognize_windows(&render_with("Логин Raptor
Пароль FSDfSSR*EUITS4", 20, dark)).unwrap();
            let kinds: Vec<_> = crate::sensitive::find(&lines).into_iter().map(|(k, _)| k).collect();
            assert_eq!(
                kinds,
                [crate::sensitive::Kind::Login, crate::sensitive::Kind::Password],
                "dark {dark}: {:?}",
                lines.iter().map(|l| &l.text).collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn strict_mode_finds_a_chat_after_real_recognition() {
        let Ok(engines) = engines() else { return };
        if !engines.iter().any(|(c, _)| *c) {
            return;
        }
        use crate::sensitive::Kind::*;
        for dark in [false, true] {
            let lines = recognize_windows(&render_with("Анна Смирнова\nПривет, это raptor_92, встреча в 18:30", 20, dark)).unwrap();
            let mut kinds: Vec<_> = crate::sensitive::find_strict(&lines).into_iter().map(|(k, _)| k).collect();
            kinds.sort_by_key(|k| *k as u8);
            assert_eq!(
                kinds,
                [Name, Name, Nick, Number],
                "dark {dark}: {:?}",
                lines.iter().map(|l| &l.text).collect::<Vec<_>>()
            );
        }
    }
    /// The same checks through PaddleOCR: the censor gets exact words, email and password included.
    #[test]
    fn paddle_reads_what_the_censor_needs_word_by_word() {
        if !crate::ocr_paddle::available() {
            return;
        }
        use crate::sensitive::Kind::*;
        for dark in [false, true] {
            let read = |text: &str, px: i32| crate::ocr_paddle::recognize(&render_with(text, px, dark)).unwrap();
            let kinds = |lines: &[Line]| {
                let mut k: Vec<_> = crate::sensitive::find_strict(lines).into_iter().map(|(k, _)| k).collect();
                k.sort_by_key(|k| *k as u8);
                k
            };
            let lines = read("Логин Raptor\nПароль FSDfSSR*EUITS4", 20);
            assert_eq!(text(&lines), "Логин Raptor\nПароль FSDfSSR*EUITS4", "dark {dark}");
            let lines = read("Анна Смирнова\nПривет, это raptor_92, встреча в 18:30", 20);
            assert_eq!(kinds(&lines), [Name, Name, Nick, Number], "dark {dark}: {:?}", text(&lines));
            let lines = read("Почта: daniil.petrov@gmail.com", 16);
            assert!(crate::sensitive::find(&lines).iter().any(|(k, _)| *k == Email), "dark {dark}: {:?}", text(&lines));
            // Word boxes sit on their words: the address box starts right of «Почта:».
            let line = &lines[0];
            let (label, address) = (&line.words[0].1, &line.words[1].1);
            assert!(address.left > label.right - 2 && address.right <= line.rect.right + 2, "{:?}", line.words);
        }
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
        let all = text(&recognize_windows(&image).unwrap());
        for phrase in ["Перевод починил", "проходят", "Русские строки остаются"] {
            assert!(all.contains(phrase), "{phrase:?} in {all:?}");
        }
    }
    fn render(value: &str) -> Image {
        render_with(value, 32, false)
    }
    /// `px` font height; `dark` — light text on a dark background, as in a dark chat window.
    /// Both engines on the same rendered text, side by side: `cargo test -p kadr-app compare_engines -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn compare_engines() {
        let samples = [
            ("Логин Raptor\nПароль FSDfSSR*EUITS4", 20),
            ("Скопировать текст\nHello Kadr 2026", 20),
            ("Анна Смирнова\nПривет, это raptor_92, встреча в 18:30", 15),
            ("Встреча перенесена на пятницу, 14 марта.\nThe meeting moved to Friday, March 14.", 13),
            ("Почта: daniil.petrov@gmail.com · Телефон +7 912 345-67-89", 12),
            ("Съешь же ещё этих мягких французских булок, да выпей чаю", 11),
        ];
        for dark in [false, true] {
            for (text, px) in samples {
                let image = render_with(text, px, dark);
                let t = std::time::Instant::now();
                let windows = recognize_windows(&image).map(|l| self::text(&l)).unwrap_or_else(|e| format!("ошибка: {e}"));
                let wt = t.elapsed().as_millis();
                let t = std::time::Instant::now();
                let paddle = crate::ocr_paddle::recognize(&image).map(|l| self::text(&l)).unwrap_or_else(|e| format!("ошибка: {e}"));
                let pt = t.elapsed().as_millis();
                eprintln!("--- {px}px {}\n  нужно:   {:?}\n  Windows: {:?} ({wt} мс)\n  Paddle:  {:?} ({pt} мс)", if dark { "тёмный" } else { "светлый" }, text.replace('\n', " / "), windows.replace('\n', " / "), paddle.replace('\n', " / "));
            }
        }
    }
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
