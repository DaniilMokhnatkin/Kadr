//! Updates from the project's GitHub releases. A day after start and then daily, Kadr asks for the
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
    /// Lowercase hex, when GitHub published it.
    sha256: Option<String>,
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
    let version = v["tag_name"].as_str()?.trim_start_matches(['v', 'V']).to_string();
    let asset = v["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(ASSET))?;
    let url = asset["browser_download_url"].as_str()?.to_string();
    let sha256 = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase);
    let notes: String = v["body"].as_str().unwrap_or("").lines().take(6).collect::<Vec<_>>().join("\n");
    Some(Release { version, notes: notes.trim().to_string(), url, sha256 })
}

/// «0.10.1» is newer than «0.9.3»: numbers, not text.
fn newer(candidate: &str, current: &str) -> bool {
    let parts = |s: &str| -> Vec<u64> { s.split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect() };
    parts(candidate) > parts(current)
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

fn download_and_run(release: &Release) -> Result<(), String> {
    let (host, path) = crate::http::split(&release.url).ok_or("Странный адрес обновления")?;
    let (status, bytes) = crate::http::request("GET", host, path, "", &[], 512 << 20, "GitHub")?;
    if status != 200 {
        return Err(format!("GitHub ответил кодом {status}"));
    }
    if !bytes.starts_with(b"MZ") {
        return Err("Скачался не установщик".into());
    }
    if let Some(expected) = &release.sha256
        && sha256(&bytes)? != *expected
    {
        return Err("Установщик скачался повреждённым — контрольная сумма не совпала".into());
    }
    let file = std::env::temp_dir().join(format!("Kadr-Setup-{}.exe", release.version));
    std::fs::write(&file, &bytes).map_err(|e| format!("Не удалось сохранить установщик: {e}"))?;
    kadr_win::log_info!("обновление {}: запускаю {}", release.version, file.display());
    let file = kadr_win::wide(&file.to_string_lossy());
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(file.as_ptr()),
            windows::core::PCWSTR::null(),
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
                      {"name":"Kadr-Setup.exe","browser_download_url":"https://github.com/o/kadr/releases/download/v0.2.0/Kadr-Setup.exe",
                       "digest":"sha256:ABCDEF"}]}"#;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.notes, "Что нового:\n- GIF\n- QR");
        assert!(r.url.ends_with("/Kadr-Setup.exe"));
        assert_eq!(r.sha256.as_deref(), Some("abcdef"));
        assert!(parse(&json.replace("\"prerelease\":false", "\"prerelease\":true")).is_none());
        assert!(parse(&json.replace("Kadr-Setup.exe\",\"browser", "Other.exe\",\"browser")).is_none());
    }

    #[test]
    fn hashes_like_sha256() {
        assert_eq!(sha256(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
