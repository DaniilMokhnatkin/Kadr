//! HTTPS through WinHTTP, Windows' own client: the translator's requests and the update check.
use std::ffi::c_void;
use windows::Win32::Networking::WinHttp::*;
use windows::core::{PCWSTR, w};

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

/// One HTTPS request to `host` + `path`: the status code and up to `limit` bytes of the body.
/// `who` names the other side in errors, e.g. «переводчиком». Redirects are followed.
pub fn request(
    verb: &str,
    host: &str,
    path: &str,
    headers: &str,
    body: &[u8],
    limit: usize,
    who: &str,
) -> Result<(u32, Vec<u8>), String> {
    let fail = |what: &str| format!("Нет связи с {who} ({what}): {}", windows::core::Error::from_thread().message());
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
        let (verb, path) = (kadr_win::wide(verb), kadr_win::wide(path));
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(fail("запрос"));
        }
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(headers.as_slice()),
            (!body.is_empty()).then_some(body.as_ptr() as *const c_void),
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
            if available == 0 {
                break;
            }
            if out.len() > limit {
                return Err(format!("Ответ от {who} слишком большой"));
            }
            let mut chunk = vec![0u8; available as usize];
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut c_void, available, &mut read)
                .map_err(|_| fail("чтение"))?;
            out.extend_from_slice(&chunk[..read as usize]);
        }
        Ok((status, out))
    }
}

/// `https://host/path` split for `request`; other schemes are refused.
pub fn split(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let (host, path) = rest.split_at(slash);
    (!host.is_empty()).then_some((host, if path.is_empty() { "/" } else { path }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_addresses_are_split() {
        assert_eq!(
            split("https://github.com/o/kadr/releases/download/v1/Kadr-Setup.exe"),
            Some(("github.com", "/o/kadr/releases/download/v1/Kadr-Setup.exe"))
        );
        assert_eq!(split("https://example.com"), Some(("example.com", "/")));
        assert_eq!(split("http://example.com/x"), None);
        assert_eq!(split("https:///x"), None);
    }
}
