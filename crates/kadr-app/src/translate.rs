//! Translation through Google's keyless web endpoint. Only the recognised text is sent (in the
//! request body, never in the URL); the screenshot stays on this computer.
use std::ffi::c_void;
use windows::Win32::Networking::WinHttp::*;
use windows::core::{PCWSTR, w};

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

/// What a reader on a Russian system wants: foreign lines in Russian, Russian lines untouched.
/// Text that is all Russian goes to English instead, so the button still does something useful.
pub fn for_reader(text: &str) -> Result<Translation, String> {
    plan_and_translate(text, translate)
}

/// A line is Russian when most of its words are written wholly in Cyrillic.
fn russian(line: &str) -> bool {
    let words: Vec<&str> =
        line.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().any(char::is_alphabetic)).collect();
    let cyrillic = |c: char| {
        let c = c.to_lowercase().next().unwrap_or(c);
        ('а'..='я').contains(&c) || "ёєіїўђћјљњџ".contains(c)
    };
    let whole = words.iter().filter(|w| w.chars().all(|c| c.is_alphabetic() && cyrillic(c))).count();
    !words.is_empty() && whole * 2 > words.len()
}

fn plan_and_translate(
    text: &str,
    translate: impl Fn(&str, &str) -> Result<Translation, String>,
) -> Result<Translation, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let foreign = |l: &str| l.chars().any(char::is_alphabetic) && !russian(l);
    if !lines.iter().any(|l| foreign(l)) {
        return translate(text, "en");
    }
    // Runs of foreign lines go to the translator together (context helps); the rest stays as is.
    let mut out = Vec::new();
    let mut from = String::new();
    let mut run: Vec<&str> = Vec::new();
    let mut flush = |run: &mut Vec<&str>, out: &mut Vec<String>| -> Result<(), String> {
        if !run.is_empty() {
            let t = translate(&run.join("\n"), "ru")?;
            if from.is_empty() {
                from = t.from;
            }
            out.push(t.text);
            run.clear();
        }
        Ok(())
    };
    for line in lines {
        if foreign(line) {
            run.push(line);
        } else {
            flush(&mut run, &mut out)?;
            out.push(line.to_string());
        }
    }
    flush(&mut run, &mut out)?;
    Ok(Translation { text: out.join("\n"), from, to: "ru".into() })
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

fn translate_google(text: &str, to: &str) -> Result<Translation, String> {
    let body = format!("q={}", encode(text));
    let mut limited = false;
    let mut last = String::new();
    for (host, path, read) in ENDPOINTS {
        let path = kadr_win::wide(&format!("{path}{to}"));
        match post(host, &path, body.as_bytes()) {
            Ok((200, response)) => match read(&response) {
                Some(mut t) => {
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

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// The status code and the body.
fn post(host: &str, path: &[u16], body: &[u8]) -> Result<(u32, String), String> {
    let fail =
        |what: &str| format!("Нет связи с переводчиком ({what}): {}", windows::core::Error::from_thread().message());
    unsafe {
        let session = Handle(WinHttpOpen(
            w!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
            ),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(fail("сеанс"));
        }
        let _ = WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 8000);
        // Windows 10 offers old TLS and HTTP/1.1 unless asked; Google throttles such clients first.
        // TLS 1.3 is unknown to older builds, so fall back to 1.2 alone when it is refused.
        let option = |id: u32, value: u32| {
            WinHttpSetOption(Some(session.0 as *const c_void), id, Some(&value.to_ne_bytes())).is_ok()
        };
        if !option(
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3,
        ) {
            option(WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2);
        }
        option(WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL, WINHTTP_PROTOCOL_FLAG_HTTP2);
        let host = kadr_win::wide(host);
        let connect = Handle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if connect.0.is_null() {
            return Err(fail("сервер"));
        }
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            w!("POST"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(fail("запрос"));
        }
        let headers: Vec<u16> =
            "Content-Type: application/x-www-form-urlencoded;charset=UTF-8\r\n".encode_utf16().collect();
        WinHttpSendRequest(
            request.0,
            Some(&headers),
            Some(body.as_ptr() as *const c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .map_err(|_| fail("отправка"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|_| fail("ответ"))?;
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        let _ = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        );
        let mut out = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available).map_err(|_| fail("чтение"))?;
            if available == 0 || out.len() > 4 << 20 {
                break;
            }
            let mut chunk = vec![0u8; available as usize];
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut c_void, available, &mut read)
                .map_err(|_| fail("чтение"))?;
            out.extend_from_slice(&chunk[..read as usize]);
        }
        Ok((status, String::from_utf8_lossy(&out).into_owned()))
    }
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
    /// Stand-in translator: tags what it got, records calls.
    fn fake(
        calls: &std::cell::RefCell<Vec<(String, String)>>,
    ) -> impl Fn(&str, &str) -> Result<Translation, String> + '_ {
        move |text, to| {
            calls.borrow_mut().push((text.to_string(), to.to_string()));
            Ok(Translation {
                text: format!("[{to}] {text}"),
                from: if to == "en" { "ru" } else { "en" }.into(),
                to: to.into(),
            })
        }
    }
    #[test]
    fn russian_lines_stay_and_only_foreign_runs_are_translated() {
        let calls = std::cell::RefCell::new(Vec::new());
        let text = "Заголовок на русском\nThe quick fox\njumps over\n\nЕщё строка по-русски\nSee you";
        let t = plan_and_translate(text, fake(&calls)).unwrap();
        assert_eq!(
            t.text,
            "Заголовок на русском\n[ru] The quick fox\njumps over\n\nЕщё строка по-русски\n[ru] See you"
        );
        assert_eq!((t.from.as_str(), t.to.as_str()), ("en", "ru"));
        assert_eq!(calls.borrow().len(), 2, "one request per foreign run");
    }
    #[test]
    fn all_russian_text_goes_to_english_and_a_mostly_russian_line_is_russian() {
        let calls = std::cell::RefCell::new(Vec::new());
        let t = plan_and_translate("Привет, мир\nНажмите кнопку Copy", fake(&calls)).unwrap();
        assert_eq!(t.to, "en");
        assert_eq!(calls.borrow()[0], ("Привет, мир\nНажмите кнопку Copy".to_string(), "en".to_string()));
        assert!(russian("если там есть хоть чуток русского OK"));
        assert!(!russian("Click Отмена to close the dialog"));
        assert!(!russian("CKonhp0BaTb TeKCT"));
        assert!(!russian("2026 · 12:30"));
    }
    #[test]
    #[ignore = "network: run with --ignored"]
    fn live_every_spare_endpoint_translates_on_its_own() {
        let body = format!("q={}", encode("The weather is nice today.\nSecond line"));
        for (host, path, read) in ENDPOINTS {
            let (status, response) = post(host, &kadr_win::wide(&format!("{path}ru")), body.as_bytes()).unwrap();
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
        let mixed = for_reader(
            "Сохранить как
The file was saved.",
        )
        .unwrap();
        assert!(
            mixed.text.starts_with(
                "Сохранить как
"
            ),
            "{}",
            mixed.text
        );
        assert_eq!(mixed.to, "ru");
        let russian = for_reader("Погода сегодня хорошая.").unwrap();
        assert_eq!((russian.from.as_str(), russian.to.as_str()), ("ru", "en"));
        assert!(russian.text.to_lowercase().contains("weather"), "{}", russian.text);
        println!(
            "{}
---
{}",
            mixed.text, russian.text
        );
    }
    #[test]
    fn form_encoding_keeps_unicode_and_reserved_characters_safe() {
        assert_eq!(encode("Hi & bye=1 ё"), "Hi+%26+bye%3D1+%D1%91");
    }
}
