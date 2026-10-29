//! Translation through Google's keyless web endpoint. Only the recognised text is sent (in the
//! request body, never in the URL); the screenshot stays on this computer.
use std::ffi::c_void;
use windows::Win32::Networking::WinHttp::*;
use windows::core::{PCWSTR, w};

pub struct Translation {
    pub text: String,
    /// Detected source language, e.g. "en".
    pub from: String,
}

/// Blocking: call from a worker thread. `to` is a language code such as "ru".
pub fn translate(text: &str, to: &str) -> Result<Translation, String> {
    let body = format!("q={}", encode(text));
    let path = kadr_win::wide(&format!("/translate_a/single?client=gtx&sl=auto&tl={to}&dt=t&ie=UTF-8&oe=UTF-8"));
    let response = post("translate.googleapis.com", &path, body.as_bytes())?;
    parse(&response).ok_or_else(|| "Переводчик вернул непонятный ответ".to_string())
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

fn post(host: &str, path: &[u16], body: &[u8]) -> Result<String, String> {
    let fail = |what: &str| format!("Нет связи с переводчиком ({what}): {}", windows::core::Error::from_thread().message());
    unsafe {
        let session = Handle(WinHttpOpen(
            w!("Kadr/1.0"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(fail("сеанс"));
        }
        let _ = WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 8000);
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
        let headers: Vec<u16> = "Content-Type: application/x-www-form-urlencoded;charset=UTF-8\r\n".encode_utf16().collect();
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
        if status != 200 {
            return Err(format!("Переводчик ответил кодом {status}"));
        }
        String::from_utf8(out).map_err(|_| "Переводчик вернул не UTF-8".to_string())
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
    (!text.is_empty()).then_some(Translation { text, from })
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
    #[ignore = "network: run with --ignored"]
    fn live_google_translates_to_russian() {
        let t = translate("The weather is nice today.", "ru").unwrap();
        assert_eq!(t.from, "en");
        assert!(t.text.chars().any(|c| ('а'..='я').contains(&c)), "{}", t.text);
        println!("{}", t.text);
    }
    #[test]
    fn form_encoding_keeps_unicode_and_reserved_characters_safe() {
        assert_eq!(encode("Hi & bye=1 ё"), "Hi+%26+bye%3D1+%D1%91");
    }
}
