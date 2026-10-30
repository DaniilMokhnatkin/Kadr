//! «Скопировать отчёт» in the settings: what is needed to understand a problem from afar — the
//! versions, which engines are chosen and present, and the end of the log. No screenshots, no
//! recognised text: the log itself only ever records sizes and file names.
use kadr_store::config::Config;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::{HSTRING, PCWSTR, w};

const LOG_LINES: usize = 80;

pub fn build(config: &Config) -> String {
    let build = registry_text("CurrentBuild").unwrap_or_default();
    let mut product = registry_text("ProductName").unwrap_or_else(|| "Windows".into());
    // Windows 11 still calls itself «Windows 10» there; its builds start at 22000.
    if build.parse::<u32>().is_ok_and(|b| b >= 22000) {
        product = product.replace("Windows 10", "Windows 11");
    }
    let windows = format!(
        "{product} {} (сборка {build}.{})",
        registry_text("DisplayVersion").unwrap_or_default(),
        registry_number("UBR").unwrap_or(0),
    );
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let log = kadr_win::log::tail(LOG_LINES);
    compose(&Facts {
        version: env!("CARGO_PKG_VERSION"),
        windows,
        exe,
        recognition: config.recognition.engine.name(),
        paddle: crate::ocr_paddle::available(),
        translation: config.translation.engine.name(),
        offline: crate::translate_local::available(),
        log,
    })
}

struct Facts {
    version: &'static str,
    windows: String,
    exe: String,
    recognition: &'static str,
    paddle: bool,
    translation: &'static str,
    offline: bool,
    log: Vec<String>,
}

fn compose(f: &Facts) -> String {
    let yes = |b: bool| if b { "установлен" } else { "нет" };
    let mut out = format!(
        "Отчёт Kadr {}\nWindows: {}\nПрограмма: {}\nРаспознавание: {} (PaddleOCR {})\nПеревод: {} (офлайн {})\n\nЛог, последние строки:\n",
        f.version,
        f.windows.trim(),
        f.exe,
        f.recognition,
        yes(f.paddle),
        f.translation,
        yes(f.offline),
    );
    if f.log.is_empty() {
        out.push_str("(лог пуст или недоступен)\n");
    }
    for line in &f.log {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn registry_text(name: &str) -> Option<String> {
    let name = HSTRING::from(name);
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn registry_number(name: &str) -> Option<u32> {
    let name = HSTRING::from(name);
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_versions_engines_and_ends_with_the_log() {
        let text = compose(&Facts {
            version: "1.2.3",
            windows: "Windows 10 Pro 22H2 (сборка 19045.5011)".into(),
            exe: r"C:\Program Files\Kadr\kadr.exe".into(),
            recognition: "auto",
            paddle: true,
            translation: "google",
            offline: false,
            log: vec!["12:00 перевод: ошибка 429".into()],
        });
        assert!(text.starts_with("Отчёт Kadr 1.2.3\nWindows: Windows 10 Pro 22H2"));
        assert!(text.contains("Распознавание: auto (PaddleOCR установлен)"));
        assert!(text.contains("Перевод: google (офлайн нет)"));
        assert!(text.ends_with("12:00 перевод: ошибка 429\n"));
    }

    #[test]
    fn reads_this_windows_version() {
        let text = build(&Config::default());
        assert!(text.contains("Windows: Windows"), "{text}");
        assert!(!text.contains("(сборка ."), "{text}");
    }
}
