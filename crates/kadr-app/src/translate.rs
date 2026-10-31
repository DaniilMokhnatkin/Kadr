//! Translation through Google's keyless web endpoint. Only the recognised text is sent (in the
//! request body, never in the URL); the screenshot stays on this computer.

static CHOSEN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
pub fn choose(engine: kadr_store::config::TranslationEngine) {
    CHOSEN.store(
        match engine {
            kadr_store::config::TranslationEngine::Auto => 0,
            kadr_store::config::TranslationEngine::Offline => 1,
            kadr_store::config::TranslationEngine::Google => 2,
        },
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub struct Translation {
    pub text: String,
    /// Detected source language, e.g. "en".
    pub from: String,
    /// Target language, "ru" or "en".
    pub to: String,
}

/// What goes over each block of text on the screen: its translation, or `None` when the block is
/// already in the reader's language or comes back unchanged (a nickname, a number).
pub struct Blocks {
    pub texts: Vec<Option<String>>,
    /// Detected source language, e.g. "en".
    pub from: String,
    /// Target language, "ru" or "en".
    pub to: String,
}

/// What a reader on a Russian system wants: foreign blocks in Russian, Russian ones untouched.
/// When there is nothing foreign, everything goes to English, so the button still does something.
pub fn for_reader(blocks: &[String]) -> Result<Blocks, String> {
    plan(blocks, translate)
}

fn cyrillic(c: char) -> bool {
    let c = c.to_lowercase().next().unwrap_or(c);
    ('а'..='я').contains(&c) || "ёєіїўђћјљњџ".contains(c)
}

/// Latin letters that look like Cyrillic ones; recognisers put them into Russian words.
const LOOK_ALIKES: &str = "aceopxyABCEHKMOPTX";

/// A line is Russian when most of its words are Cyrillic. A word read with Latin look-alikes in it
/// («Кoманда» with a Latin o) still counts; Cyrillic read as Latin («CKonhp0BaTb») does not.
fn russian(line: &str) -> bool {
    let words: Vec<&str> =
        line.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().any(char::is_alphabetic)).collect();
    let cyrillic_word =
        |w: &&&str| w.chars().any(cyrillic) && w.chars().all(|c| cyrillic(c) || LOOK_ALIKES.contains(c));
    let whole = words.iter().filter(cyrillic_word).count();
    !words.is_empty() && whole * 2 > words.len()
}

fn plan(blocks: &[String], translate: impl Fn(&str, &str) -> Result<Translation, String>) -> Result<Blocks, String> {
    let lettered: Vec<usize> = (0..blocks.len()).filter(|&i| blocks[i].chars().any(char::is_alphabetic)).collect();
    if lettered.is_empty() {
        return Err("В выделении не нашлось текста".into());
    }
    let foreign: Vec<usize> = lettered.iter().copied().filter(|&i| !russian(&blocks[i])).collect();
    if !foreign.is_empty() {
        let done = batch(blocks, &foreign, "ru", &translate)?;
        // Google read it as Russian after all (look-alike letters), or nothing changed (names):
        // then the text is Russian for the reader, and English is what is useful.
        if !done.from.starts_with("ru") && done.texts.iter().any(Option::is_some) {
            return Ok(done);
        }
    }
    batch(blocks, &lettered, "en", &translate)
}

/// `wanted` blocks in one request, a line each; when the lines do not come back one for one, each
/// block goes on its own so nothing lands over the wrong text.
fn batch(
    blocks: &[String],
    wanted: &[usize],
    to: &str,
    translate: &impl Fn(&str, &str) -> Result<Translation, String>,
) -> Result<Blocks, String> {
    let texts: Vec<String> = wanted.iter().map(|&i| blocks[i].replace('\n', " ")).collect();
    let all = translate(&texts.join("\n"), to)?;
    let back: Vec<&str> = all.text.split('\n').collect();
    let done: Vec<String> = if texts.len() == 1 {
        vec![all.text.replace('\n', " ")]
    } else if back.len() == texts.len() {
        back.iter().map(|s| s.to_string()).collect()
    } else {
        texts.iter().map(|t| translate(t, to).map(|t| t.text.replace('\n', " "))).collect::<Result<_, _>>()?
    };
    let key = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let mut out = vec![None; blocks.len()];
    for ((&i, original), t) in wanted.iter().zip(&texts).zip(done) {
        let t = t.trim();
        if !t.is_empty() && key(t) != key(original) {
            out[i] = Some(t.to_string());
        }
    }
    Ok(Blocks { texts: out, from: all.from, to: to.into() })
}

/// Blocking: call from a worker thread. `to` is a language code such as "ru".
pub fn translate(text: &str, to: &str) -> Result<Translation, String> {
    let chosen = CHOSEN.load(std::sync::atomic::Ordering::Relaxed);
    if chosen == 1 || (chosen == 0 && crate::translate_local::available()) {
        return crate::translate_local::translate(text, to);
    }
    translate_google(text, to)
}
/// Google's keyless endpoints, tried in order. They are limited separately, so when one answers
/// 429 (seen on Windows 10, whose WinHTTP looks less like a browser) the next one usually works.
type Reader = fn(&str) -> Option<Translation>;
const ENDPOINTS: [(&str, &str, Reader); 3] = [
    ("translate.googleapis.com", "/translate_a/single?client=gtx&sl=auto&dt=t&ie=UTF-8&oe=UTF-8&tl=", parse),
    ("clients5.google.com", "/translate_a/t?client=dict-chrome-ex&sl=auto&tl=", parse_short),
    ("translate.google.com", "/translate_a/single?client=gtx&sl=auto&dt=t&ie=UTF-8&oe=UTF-8&tl=", parse),
];

static WORKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn translate_google(text: &str, to: &str) -> Result<Translation, String> {
    let body = format!("q={}", encode(text));
    let mut limited = false;
    let mut last = String::new();
    // Start from the address that answered last time: a throttled one stays throttled for a while.
    let first = WORKED.load(std::sync::atomic::Ordering::Relaxed);
    for k in 0..ENDPOINTS.len() {
        let index = (first + k) % ENDPOINTS.len();
        let (host, path, read) = ENDPOINTS[index];
        let path = format!("{path}{to}");
        match post(host, &path, body.as_bytes()) {
            Ok((200, response)) => match read(&response) {
                Some(mut t) => {
                    WORKED.store(index, std::sync::atomic::Ordering::Relaxed);
                    t.to = to.into();
                    return Ok(t);
                }
                None => last = "Переводчик вернул непонятный ответ".into(),
            },
            Ok((status, _)) => {
                limited |= status == 429;
                last = format!("Переводчик ответил кодом {status}");
            }
            Err(e) => last = e,
        }
    }
    if limited {
        return Err("Google временно ограничил запросы с этого компьютера (429). Включите офлайн-перевод в настройках или повторите позже.".into());
    }
    Err(last)
}

/// The status code and the body.
fn post(host: &str, path: &str, body: &[u8]) -> Result<(u32, String), String> {
    let headers = "Content-Type: application/x-www-form-urlencoded;charset=UTF-8\r\n";
    let (status, out) = crate::http::request("POST", host, path, headers, body, 4 << 20, "переводчиком")?;
    Ok((status, String::from_utf8_lossy(&out).into_owned()))
}

/// application/x-www-form-urlencoded.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Just enough JSON for `[[["перевод","source",…],…],null,"en",…]`.
#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool,
    Number,
    Str(String),
    List(Vec<Json>),
    Object,
}

/// The dict-chrome-ex answer: `[["перевод","en"]]`.
fn parse_short(body: &str) -> Option<Translation> {
    let mut chars = body.chars().peekable();
    let Json::List(top) = value(&mut chars)? else { return None };
    let (text, from) = match top.first()? {
        Json::List(pair) => match (pair.first(), pair.get(1)) {
            (Some(Json::Str(text)), Some(Json::Str(from))) => (text.clone(), from.clone()),
            (Some(Json::Str(text)), _) => (text.clone(), String::new()),
            _ => return None,
        },
        Json::Str(text) => (text.clone(), String::new()),
        _ => return None,
    };
    (!text.is_empty()).then_some(Translation { text, from, to: String::new() })
}

fn parse(body: &str) -> Option<Translation> {
    let mut chars = body.chars().peekable();
    let Json::List(top) = value(&mut chars)? else { return None };
    let Json::List(segments) = top.first()? else { return None };
    let mut text = String::new();
    for segment in segments {
        if let Json::List(parts) = segment
            && let Some(Json::Str(piece)) = parts.first()
        {
            text.push_str(piece);
        }
    }
    let from = match top.get(2) {
        Some(Json::Str(lang)) => lang.clone(),
        _ => String::new(),
    };
    (!text.is_empty()).then_some(Translation { text, from, to: String::new() })
}

fn value(c: &mut std::iter::Peekable<std::str::Chars>) -> Option<Json> {
    while c.peek().is_some_and(|ch| ch.is_whitespace()) {
        c.next();
    }
    match *c.peek()? {
        '[' => {
            c.next();
            let mut items = Vec::new();
            loop {
                while c.peek().is_some_and(|ch| ch.is_whitespace() || *ch == ',') {
                    c.next();
                }
                if *c.peek()? == ']' {
                    c.next();
                    return Some(Json::List(items));
                }
                items.push(value(c)?);
            }
        }
        '{' => {
            // Objects never carry the translation; skip them with their nesting.
            let mut depth = 0;
            let mut in_string = false;
            while let Some(ch) = c.next() {
                match ch {
                    '"' => in_string = !in_string,
                    '\\' if in_string => {
                        c.next();
                    }
                    '{' if !in_string => depth += 1,
                    '}' if !in_string => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(Json::Object);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        '"' => {
            c.next();
            let mut s = String::new();
            loop {
                match c.next()? {
                    '"' => return Some(Json::Str(s)),
                    '\\' => match c.next()? {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => {}
                        'u' => {
                            let hex: String = (0..4).filter_map(|_| c.next()).collect();
                            let mut code = u32::from_str_radix(&hex, 16).ok()?;
                            // Surrogate pair.
                            if (0xD800..0xDC00).contains(&code) && c.next() == Some('\\') && c.next() == Some('u') {
                                let low: String = (0..4).filter_map(|_| c.next()).collect();
                                let low = u32::from_str_radix(&low, 16).ok()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => s.push(other),
                    },
                    ch => s.push(ch),
                }
            }
        }
        'n' => {
            (0..4).for_each(|_| {
                c.next();
            });
            Some(Json::Null)
        }
        't' | 'f' => {
            while c.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
                c.next();
            }
            Some(Json::Bool)
        }
        _ => {
            while c.peek().is_some_and(|ch| ch.is_ascii_digit() || "+-.eE".contains(*ch)) {
                c.next();
            }
            Some(Json::Number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn joins_segments_and_reads_the_detected_language() {
        let body = r#"[[["Быстрая лиса. ","The quick fox. ",null,null,10],["Прыгает \"высоко\"\nи é 😀","Jumps high",null,null,3,null,null,[[]],[[["abc"]]]]],null,"en",null,null,null,1,[],[["en"],null,[1],["en"]]]"#;
        let t = parse(body).unwrap();
        assert_eq!(t.text, "Быстрая лиса. Прыгает \"высоко\"\nи é 😀");
        assert_eq!(t.from, "en");
        assert!(parse("<html>blocked</html>").is_none());
        assert!(parse("[]").is_none());
    }
    #[test]
    fn reads_the_short_answer_of_the_spare_endpoint() {
        let t = parse_short(r#"[["Привет, мир.\nВторая линия","en"]]"#).unwrap();
        assert_eq!((t.text.as_str(), t.from.as_str()), ("Привет, мир.\nВторая линия", "en"));
        assert_eq!(parse_short(r#"["Привет"]"#).unwrap().text, "Привет");
        assert!(parse_short("<html>429</html>").is_none());
        assert!(parse_short("[]").is_none());
    }
    /// Stand-in translator: tags each line it got, records calls; Russian lines come back as they are.
    fn fake(
        calls: &std::cell::RefCell<Vec<(String, String)>>,
    ) -> impl Fn(&str, &str) -> Result<Translation, String> + '_ {
        move |text, to| {
            calls.borrow_mut().push((text.to_string(), to.to_string()));
            let lines: Vec<String> = text.split('\n').map(|l| format!("[{to}] {l}")).collect();
            Ok(Translation { text: lines.join("\n"), from: if to == "en" { "ru" } else { "en" }.into(), to: to.into() })
        }
    }
    fn blocks(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|t| t.to_string()).collect()
    }
    #[test]
    fn russian_blocks_stay_and_foreign_ones_go_in_one_request() {
        let calls = std::cell::RefCell::new(Vec::new());
        let input = blocks(&["Заголовок на русском", "The quick fox\njumps over", "Ещё строка по-русски", "See you"]);
        let t = plan(&input, fake(&calls)).unwrap();
        assert_eq!(t.texts, [None, Some("[ru] The quick fox jumps over".into()), None, Some("[ru] See you".into())]);
        assert_eq!((t.from.as_str(), t.to.as_str()), ("en", "ru"));
        assert_eq!(calls.borrow().as_slice(), [("The quick fox jumps over\nSee you".into(), "ru".into())]);
    }
    #[test]
    fn when_lines_come_back_merged_each_block_is_translated_on_its_own() {
        let calls = std::cell::RefCell::new(Vec::new());
        let merging = |text: &str, to: &str| {
            calls.borrow_mut().push(text.to_string());
            Ok(Translation { text: format!("ru:{}", text.replace('\n', " ")), from: "en".into(), to: to.into() })
        };
        let t = plan(&blocks(&["One", "Два", "Three four"]), merging).unwrap();
        assert_eq!(t.texts, [Some("ru:One".into()), None, Some("ru:Three four".into())]);
        assert_eq!(calls.borrow().len(), 3, "the batch, then each block");
    }
    #[test]
    fn nothing_foreign_goes_to_english_and_a_mostly_russian_line_is_russian() {
        let calls = std::cell::RefCell::new(Vec::new());
        let t = plan(&blocks(&["Привет, мир", "Нажмите кнопку Copy", "2026"]), fake(&calls)).unwrap();
        assert_eq!(t.to, "en");
        assert_eq!(calls.borrow()[0], ("Привет, мир\nНажмите кнопку Copy".to_string(), "en".to_string()));
        assert_eq!(t.texts[2], None, "no letters, nothing to put over it");
        assert!(russian("если там есть хоть чуток русского OK"));
        assert!(!russian("Click Отмена to close the dialog"));
        assert!(!russian("CKonhp0BaTb TeKCT"));
        assert!(!russian("2026 · 12:30"));
        assert!(plan(&blocks(&["12:30", "—"]), fake(&calls)).is_err());
    }
    /// The «Русский → Русский» bug: Russian read with Latin look-alikes was sent to be translated
    /// into Russian, and Google answered with the same Russian text.
    #[test]
    fn russian_with_latin_look_alikes_is_russian_and_never_goes_ru_to_ru() {
        assert!(russian("Кoманда 1"), "Latin o inside");
        assert!(russian("HАБЛЮДАТЕЛЬ"), "Latin H inside");
        assert!(!russian("аnеrurkiоlа"), "a Latin nickname with Cyrillic look-alikes is not Russian");
        let calls = std::cell::RefCell::new(Vec::new());
        // Google sees Russian where the planner saw a foreign block, and echoes it back.
        let echo = |text: &str, to: &str| {
            calls.borrow_mut().push(to.to_string());
            let text = if to == "ru" { text.to_string() } else { format!("EN {text}") };
            Ok(Translation { text, from: "ru".into(), to: to.into() })
        };
        let t = plan(&blocks(&["Кoмaндa", "StarKoff"]), echo).unwrap();
        assert_eq!((t.from.as_str(), t.to.as_str()), ("ru", "en"));
        assert_eq!(calls.borrow().as_slice(), ["ru", "en"]);
        // Names that come back unchanged get nothing over them.
        let names = |text: &str, to: &str| Ok(Translation { text: text.into(), from: "en".into(), to: to.into() });
        let t = plan(&blocks(&["HAILUN", "petre"]), names).unwrap();
        assert_eq!(t.texts, [None, None]);
    }
    #[test]
    #[ignore = "network: run with --ignored"]
    fn live_every_spare_endpoint_translates_on_its_own() {
        let body = format!("q={}", encode("The weather is nice today.\nSecond line"));
        for (host, path, read) in ENDPOINTS {
            let (status, response) = post(host, &format!("{path}ru"), body.as_bytes()).unwrap();
            assert_eq!(status, 200, "{host}");
            let t = read(&response).unwrap_or_else(|| panic!("{host}: {response}"));
            assert_eq!(t.from, "en", "{host}");
            assert!(t.text.contains('\n') && t.text.chars().any(|c| ('а'..='я').contains(&c)), "{host}: {}", t.text);
        }
    }
    #[test]
    #[ignore = "network: run with --ignored"]
    fn live_google_translates_to_russian() {
        let t = translate_google("The weather is nice today.", "ru").unwrap();
        assert_eq!(t.from, "en");
        assert!(t.text.chars().any(|c| ('а'..='я').contains(&c)), "{}", t.text);
        println!("{}", t.text);
    }
    #[test]
    #[ignore = "network: run with --ignored"]
    fn live_reader_translation_keeps_russian_and_turns_all_russian_into_english() {
        let mixed = for_reader(&blocks(&["Сохранить как", "The file was saved."])).unwrap();
        assert_eq!(mixed.texts[0], None);
        assert!(
            mixed.texts[1].as_deref().is_some_and(|t| t.contains("Файл") || t.contains("файл")),
            "{:?}",
            mixed.texts
        );
        assert_eq!(mixed.to, "ru");
        let russian = for_reader(&blocks(&["Погода сегодня хорошая."])).unwrap();
        assert_eq!((russian.from.as_str(), russian.to.as_str()), ("ru", "en"));
        assert!(russian.texts[0].as_deref().unwrap().to_lowercase().contains("weather"), "{:?}", russian.texts);
    }
    #[test]
    fn form_encoding_keeps_unicode_and_reserved_characters_safe() {
        assert_eq!(encode("Hi & bye=1 ё"), "Hi+%26+bye%3D1+%D1%91");
    }
}
