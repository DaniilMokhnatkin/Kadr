//! Updates from the project's GitHub releases. Shortly after start and then daily, Kadr asks for the
//! latest release; a newer one is offered once in a bubble (and any time from the tray menu).
//! «Обновить» downloads its Kadr-Setup.exe, checks the SHA-256 GitHub publishes for it, and starts
//! it: the setup stops Kadr, replaces it and starts the new one, as on a first install.
use std::sync::Mutex;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL};

/// Where the releases are published: «owner/repo», set when the release is built.
pub const REPO: Option<&str> = option_env!("KADR_REPO");
/// The setup's file name among a release's assets.
const ASSET: &str = "Kadr-Setup.exe";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// «0.2.0», without the «v».
    pub version: String,
    /// The first lines of the release notes.
    pub notes: String,
    url: String,
    /// The release asset's required SHA-256, supplied by GitHub.
    sha256: String,
}

/// What the worker found, for the app's UI thread.
pub enum Event {
    Found(Release),
    /// Asked from the tray: nothing newer.
    Current,
    Failed(String),
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
static LATEST: Mutex<Option<Release>> = Mutex::new(None);
/// The app window and its message for events: set once by `watch`.
static HOST: Mutex<Option<(isize, u32)>> = Mutex::new(None);

pub fn take_events() -> Vec<Event> {
    EVENTS.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
}

fn send(event: Event) {
    if let Ok(mut events) = EVENTS.lock() {
        events.push(event);
    }
    if let Some((host, message)) = HOST.lock().ok().and_then(|h| *h) {
        unsafe {
            let _ = PostMessageW(Some(HWND(host as *mut _)), message, WPARAM(0), LPARAM(0));
        }
    }
}

/// The daily check, on its own thread for as long as Kadr runs. A version is offered only once
/// on its own; the tray menu asks again whenever wanted.
pub fn watch(host: HWND, message: u32) {
    if let Ok(mut h) = HOST.lock() {
        *h = Some((host.0 as isize, message));
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(45));
        loop {
            match check() {
                Ok(Some(release)) if offered().as_deref() != Some(release.version.as_str()) => {
                    remember(&release.version);
                    send(Event::Found(release));
                }
                Ok(_) => {}
                Err(e) => kadr_win::log_info!("обновления: {e}"),
            }
            std::thread::sleep(Duration::from_secs(24 * 60 * 60));
        }
    });
}

/// From the tray: check now and say what came of it.
pub fn check_now() {
    std::thread::spawn(move || {
        let event = match check() {
            Ok(Some(release)) => Event::Found(release),
            Ok(None) => Event::Current,
            Err(e) => Event::Failed(e),
        };
        send(event);
    });
}

/// The latest release when it is newer than this Kadr.
fn check() -> Result<Option<Release>, String> {
    let repo = REPO.ok_or("в этой сборке не указано, откуда брать обновления")?;
    let path = format!("/repos/{repo}/releases/latest");
    let headers = "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n";
    let (status, body) = crate::http::request("GET", "api.github.com", &path, headers, &[], 1 << 20, "GitHub")?;
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!("GitHub ответил кодом {status}"));
    }
    let release = parse(&String::from_utf8_lossy(&body)).ok_or("GitHub вернул непонятный ответ")?;
    let fresh = newer(&release.version, env!("CARGO_PKG_VERSION"));
    if let Ok(mut latest) = LATEST.lock() {
        *latest = fresh.then(|| release.clone());
    }
    Ok(fresh.then_some(release))
}

fn parse(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = v["tag_name"].as_str()?;
    let version = tag.strip_prefix('v').or_else(|| tag.strip_prefix('V')).unwrap_or(tag).to_string();
    version_parts(&version)?;
    let asset = v["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(ASSET))?;
    let url = asset["browser_download_url"].as_str()?.to_string();
    let expected_url = format!("https://github.com/{}/releases/download/{tag}/{ASSET}", REPO?);
    if url != expected_url {
        return None;
    }
    let sha256 = asset["digest"].as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let notes = readable_notes(v["body"].as_str().unwrap_or(""));
    Some(Release { version, notes, url, sha256 })
}

/// Release notes as a person reads them: no Markdown marks, no links, no GitHub's automatic
/// «Full Changelog: …/compare/…» line; a plain sentence when nothing is left.
fn readable_notes(body: &str) -> String {
    let lines: Vec<String> = body
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            // The tag's own title, «Kadr 1.0.5»: the bubble already names the version.
            let title = line.strip_prefix("Kadr ").is_some_and(|v| version_parts(v).is_some());
            if title || line.contains("Full Changelog") || line.contains("/compare/") || line.starts_with("http") {
                return None;
            }
            let line = line.trim_start_matches('#').trim().replace("**", "").replace("__", "").replace('`', "");
            // Generated entries end with «by @someone in https://…».
            let line = line.split(" in https://").next().unwrap_or_default();
            let line = line.split(" by @").next().unwrap_or_default().trim().to_string();
            let line = match line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                Some(rest) => format!("• {rest}"),
                None => line,
            };
            (!line.is_empty()).then_some(line)
        })
        .take(6)
        .collect();
    if lines.is_empty() {
        return "Исправления и улучшения. Kadr скачает установщик, проверит его и обновится.".into();
    }
    lines.join("\n")
}

/// «0.10.1» is newer than «0.9.3»: numbers, not text.
fn version_parts(version: &str) -> Option<[u64; 3]> {
    let mut p = version.split('.');
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0')))
            .then(|| part.parse().ok())
            .flatten()
    };
    let result = [number(p.next()?)?, number(p.next()?)?, number(p.next()?)?];
    p.next().is_none().then_some(result)
}

fn newer(candidate: &str, current: &str) -> bool {
    match (version_parts(candidate), version_parts(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn note_path() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("Kadr").join("update-offered.txt"))
}
fn offered() -> Option<String> {
    std::fs::read_to_string(note_path()?).ok().map(|s| s.trim().to_string())
}
fn remember(version: &str) {
    if let Some(path) = note_path() {
        let _ = std::fs::write(path, version);
    }
}

/// «Обновить» in the bubble: download, verify and start the setup, on a worker.
pub fn install() {
    let Some(release) = LATEST.lock().ok().and_then(|l| l.clone()) else { return };
    std::thread::spawn(move || {
        if let Err(e) = download_and_run(&release) {
            kadr_win::log_warn!("обновление {}: {e}", release.version);
            send(Event::Failed(e));
        }
    });
}

fn installer_bytes(release: &Release) -> Result<Vec<u8>, String> {
    let (host, path) = crate::http::split(&release.url).ok_or("Странный адрес обновления")?;
    let (status, bytes) = crate::http::request("GET", host, path, "", &[], 512 << 20, "GitHub")?;
    if status != 200 {
        return Err(format!("GitHub ответил кодом {status}"));
    }
    if !bytes.starts_with(b"MZ") {
        return Err("Скачался не установщик".into());
    }
    if sha256(&bytes)? != release.sha256 {
        return Err("Установщик скачался повреждённым — контрольная сумма не совпала".into());
    }
    Ok(bytes)
}

fn download_and_run(release: &Release) -> Result<(), String> {
    let bytes = installer_bytes(release)?;
    let file = std::env::temp_dir().join(format!("Kadr-Setup-{}.exe", release.version));
    std::fs::write(&file, &bytes).map_err(|e| format!("Не удалось сохранить установщик: {e}"))?;
    kadr_win::log_info!("обновление {}: запускаю {}", release.version, file.display());
    let file = kadr_win::wide(&file.to_string_lossy());
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(file.as_ptr()),
            // The setup then updates at once with the choices made when Kadr was installed.
            windows::core::w!("--update"),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err("Windows не запустила установщик".into());
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> Result<String, String> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut out) };
    if status.is_err() {
        return Err("Не удалось проверить установщик".into());
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn reads_the_release_and_its_setup() {
        let json = r#"{"tag_name":"v0.2.0","draft":false,"prerelease":false,"body":"Что нового:\r\n- GIF\r\n- QR",
            "assets":[{"name":"notes.txt","browser_download_url":"https://x/notes.txt"},
                      {"name":"Kadr-Setup.exe","browser_download_url":"https://github.com/DaniilMokhnatkin/Kadr/releases/download/v0.2.0/Kadr-Setup.exe",
                       "digest":"sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.notes, "Что нового:\n• GIF\n• QR");
        assert!(r.url.ends_with("/Kadr-Setup.exe"));
        assert_eq!(r.sha256, "a".repeat(64));
        assert!(parse(&json.replace("\"prerelease\":false", "\"prerelease\":true")).is_none());
        assert!(parse(&json.replace("Kadr-Setup.exe\",\"browser", "Other.exe\",\"browser")).is_none());
    }

    /// What 1.0.1–1.0.3 showed in the update bubble: GitHub's automatic line, not news.
    #[test]
    fn notes_lose_github_boilerplate_and_markdown() {
        let generated = "**Full Changelog**: https://github.com/DaniilMokhnatkin/Kadr/compare/v1.0.2...v1.0.3";
        assert_eq!(
            readable_notes(generated),
            "Исправления и улучшения. Kadr скачает установщик, проверит его и обновится."
        );
        assert_eq!(readable_notes("Kadr 1.0.5\n\n• Быстрее перевод"), "• Быстрее перевод");
        let mixed = "## What's Changed\n* **Перевод** прямо на экране by @someone in https://github.com/x/y/pull/1\n\n- Без `чёрного` экрана\nhttps://example.com";
        assert_eq!(readable_notes(mixed), "What's Changed\n• Перевод прямо на экране\n• Без чёрного экрана");
    }

    #[test]
    fn hashes_like_sha256() {
        assert_eq!(sha256(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn rejects_non_numeric_versions_and_unverified_downloads() {
        let sha = "a".repeat(64);
        let release = |tag: &str, url: &str, digest: &str| {
            format!(
                r#"{{"tag_name":"{tag}","assets":[{{"name":"Kadr-Setup.exe","browser_download_url":"{url}","digest":"{digest}"}}]}}"#
            )
        };
        let repo = REPO.unwrap();
        let url = format!("https://github.com/{repo}/releases/download/v1.0.1/Kadr-Setup.exe");
        assert!(parse(&release("v1.0.1", &url, &format!("sha256:{sha}"))).is_some());
        assert!(parse(&release("release", &url, &format!("sha256:{sha}"))).is_none());
        assert!(parse(&release("v1.0.1", &url, "")).is_none());
        assert!(parse(&release("v1.0.1", "https://evil.example/Kadr-Setup.exe", &format!("sha256:{sha}"))).is_none());
        assert!(!newer("1.0.1-evil", "1.0.0"));
        assert!(!newer("1.0", "1.0.0"));
    }

    #[test]
    fn previous_release_finds_next_stable_release_once() {
        let tag = "v1.0.0";
        let sha = "0123456789abcdef".repeat(4);
        let payload = format!(
            r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,
          "assets":[{{"name":"Kadr-Setup.exe","browser_download_url":"https://github.com/{}/releases/download/{tag}/Kadr-Setup.exe","digest":"sha256:{sha}"}}]}}"#,
            REPO.unwrap()
        );
        let release = parse(&payload).unwrap();
        assert!(newer(&release.version, "0.1.0"));
        assert!(!newer(&release.version, "1.0.0"));
        assert!(newer("1.0.1", &release.version));
    }

    #[test]
    #[ignore = "network: downloads the published installer but never runs it"]
    fn live_latest_release_download_passes_integrity_check_without_installing() {
        let path = format!("/repos/{}/releases/latest", REPO.unwrap());
        let (status, body) = crate::http::request(
            "GET",
            "api.github.com",
            &path,
            "Accept: application/vnd.github+json\r\n",
            &[],
            1 << 20,
            "GitHub",
        )
        .unwrap();
        assert_eq!(status, 200);
        let release = parse(&String::from_utf8_lossy(&body)).expect("numeric version and verifiable GitHub asset");
        let bytes = installer_bytes(&release).unwrap();
        println!(
            "Verified Kadr {}: {} bytes, SHA-256 {}. Installer not started.",
            release.version,
            bytes.len(),
            release.sha256
        );
    }
}
