//! The installing itself: the setup carries kadr.exe and install.ps1 appended to its own file,
//! puts them in a temporary folder, and runs the script once with administrator rights (the one
//! UAC prompt). The script reports each step into a small text file that the window follows.
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

/// Tail of Kadr-Setup.exe: `[kadr.exe][install.ps1][exe length u64][script length u64][MAGIC]`.
pub const MAGIC: &[u8; 8] = b"KADRSETP";

pub struct Payload {
    pub exe: Vec<u8>,
    pub script: Vec<u8>,
}

/// The program and the script carried by this very file; in a development build, the release
/// build and the script from the project instead.
pub fn payload() -> Result<Payload, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&me).map_err(|e| e.to_string())?;
    if let Some(p) = split(&bytes) {
        return Ok(p);
    }
    let root = me.ancestors().nth(3).map(Path::to_path_buf).unwrap_or_default();
    let exe = std::fs::read(root.join("target/release/kadr.exe"));
    let script = std::fs::read(root.join("installer/install.ps1"));
    match (exe, script) {
        (Ok(exe), Ok(script)) => Ok(Payload { exe, script }),
        _ => Err("Установщик повреждён: внутри нет Kadr. Скачайте его заново.".into()),
    }
}

pub fn split(bytes: &[u8]) -> Option<Payload> {
    let n = bytes.len();
    if n < 24 || &bytes[n - 8..] != MAGIC {
        return None;
    }
    let num = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
    let (exe_len, script_len) = (num(n - 24), num(n - 16));
    let end = n - 24;
    let start = end.checked_sub(exe_len + script_len)?;
    Some(Payload {
        exe: bytes[start..start + exe_len].to_vec(),
        script: bytes[start + exe_len..end].to_vec(),
    })
}

/// What the script reported.
#[derive(Debug, PartialEq)]
pub enum Report {
    Step(String),
    Ok(String),
    Skip(String),
    Warn(String, String),
    Error(String),
    Finish,
}

pub fn parse(line: &str) -> Option<Report> {
    let line = line.trim_start_matches('\u{feff}').trim();
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let id = || rest.split(' ').next().unwrap_or("").to_string();
    Some(match word {
        "step" => Report::Step(id()),
        "ok" => Report::Ok(id()),
        "skip" => Report::Skip(id()),
        "warn" => Report::Warn(id(), rest.split_once(' ').map_or("", |(_, t)| t).to_string()),
        "error" => Report::Error(rest.to_string()),
        "finish" => Report::Finish,
        _ => return None,
    })
}

pub enum Start {
    Running(Job),
    /// The user said «Нет» to the administrator prompt.
    Declined,
    Failed(String),
}

pub struct Job {
    process: HANDLE,
    progress: PathBuf,
    seen: usize,
    folder: PathBuf,
}

pub fn start(owner: HWND, payload: &Payload, autostart: bool, ocr: bool) -> Start {
    let folder = std::env::temp_dir().join(format!("KadrSetup-{}", std::process::id()));
    let (exe, script, progress) = (folder.join("kadr.bin"), folder.join("install.ps1"), folder.join("progress.txt"));
    let written = std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&exe, &payload.exe))
        .and_then(|()| std::fs::write(&script, &payload.script))
        .and_then(|()| std::fs::write(&progress, b""));
    if let Err(e) = written {
        return Start::Failed(format!("Не удалось подготовить файлы во временной папке: {e}"));
    }
    let mut params = format!(
        "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\" -Exe \"{}\" -Progress \"{}\"",
        script.display(),
        exe.display(),
        progress.display()
    );
    if !autostart {
        params.push_str(" -NoAutostart");
    }
    if !ocr {
        params.push_str(" -NoOcr");
    }
    let params = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: owner,
        lpVerb: w!("runas"),
        lpFile: w!("powershell.exe"),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) if !info.hProcess.is_invalid() => Start::Running(Job { process: info.hProcess, progress, seen: 0, folder }),
        Ok(()) => Start::Failed("Windows не запустила установку.".into()),
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => {
            let _ = std::fs::remove_dir_all(&folder);
            Start::Declined
        }
        Err(e) => Start::Failed(e.message()),
    }
}

impl Job {
    /// New reports since the last call, and the exit code once the script has ended.
    pub fn poll(&mut self) -> (Vec<Report>, Option<u32>) {
        let text = std::fs::read(&self.progress).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        // Only whole lines: the last one may still be being written.
        let complete = if text.ends_with('\n') { lines.len() } else { lines.len().saturating_sub(1) };
        let new: Vec<Report> = lines[self.seen.min(complete)..complete].iter().filter_map(|l| parse(l)).collect();
        self.seen = self.seen.max(complete);
        let exit = unsafe {
            (WaitForSingleObject(self.process, 0) == WAIT_OBJECT_0).then(|| {
                let mut code = 0u32;
                let _ = GetExitCodeProcess(self.process, &mut code);
                code
            })
        };
        (new, exit)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process);
        }
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_payload_round_trips_through_the_tail_of_the_file() {
        let mut file = b"MZ setup program".to_vec();
        let (exe, script) = (b"kadr exe bytes".to_vec(), "скрипт".as_bytes().to_vec());
        file.extend(&exe);
        file.extend(&script);
        file.extend((exe.len() as u64).to_le_bytes());
        file.extend((script.len() as u64).to_le_bytes());
        file.extend(MAGIC);
        let p = split(&file).unwrap();
        assert_eq!((p.exe, p.script), (exe, script));
        assert!(split(b"MZ plain program without payload").is_none());
    }
    #[test]
    fn reports_read_as_the_script_writes_them() {
        assert_eq!(parse("\u{feff}step prepare"), Some(Report::Step("prepare".into())));
        assert_eq!(parse("ok ocr-en"), Some(Report::Ok("ocr-en".into())));
        assert_eq!(parse("warn ocr-ru Нет связи с сервером"), Some(Report::Warn("ocr-ru".into(), "Нет связи с сервером".into())));
        assert_eq!(parse("error Подпись не удалась"), Some(Report::Error("Подпись не удалась".into())));
        assert_eq!(parse("finish"), Some(Report::Finish));
        assert_eq!(parse("что-то ещё"), None);
    }
}
