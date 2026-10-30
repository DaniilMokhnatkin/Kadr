//! «Скачать офлайн-перевод» in the settings: runs the install script the setup left next to Kadr
//! (`setup\install.ps1 -TranslationOnly`) with administrator rights — one UAC prompt — and follows
//! the progress file it writes, line by line, as the setup itself does.
use std::path::PathBuf;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Downloading or checking: what it is doing now.
    Running(String),
    Done,
    Failed(String),
}

pub struct Job {
    process: HANDLE,
    progress: PathBuf,
    status: Status,
}

/// The script the setup copied next to Kadr; older installs have none.
pub fn script() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join("setup").join("install.ps1");
    path.is_file().then_some(path)
}

pub fn start(owner: HWND) -> Result<Job, String> {
    let script =
        script().ok_or("Нет файлов установки рядом с Kadr. Запустите Kadr-Setup ещё раз и отметьте офлайн-перевод.")?;
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let progress = std::env::temp_dir().join(format!("kadr-translation-{nonce}.txt"));
    std::fs::write(&progress, b"").map_err(|e| e.to_string())?;
    let params = HSTRING::from(format!(
        "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\" -TranslationOnly -Progress \"{}\"",
        script.display(),
        progress.display()
    ));
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
        Ok(()) if !info.hProcess.is_invalid() => {
            Ok(Job { process: info.hProcess, progress, status: Status::Running("Подготовка…".into()) })
        }
        Ok(()) => Err("Windows не запустила установку".into()),
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Err("Нужно разрешение администратора".into()),
        Err(e) => Err(e.message()),
    }
}

impl Job {
    /// Where it stood at the last `poll`.
    pub fn status(&self) -> &Status {
        &self.status
    }
    /// Where it stands now; reads what the script has written since.
    pub fn poll(&mut self) -> &Status {
        if !matches!(self.status, Status::Running(_)) {
            return &self.status;
        }
        // Exit first: once it has ended, its last line is surely on disk.
        let ended = unsafe { WaitForSingleObject(self.process, 0) == WAIT_OBJECT_0 };
        let text = std::fs::read(&self.progress).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        self.status = read(&text, ended).unwrap_or_else(|| self.status.clone());
        &self.status
    }
}

/// The state the progress lines describe; `None` while nothing new is said.
fn read(text: &str, ended: bool) -> Option<Status> {
    let mut status = None;
    let mut finished = false;
    for line in text.lines().map(|l| l.trim_start_matches('\u{feff}').trim()) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let (id, detail) = rest.split_once(' ').unwrap_or((rest, ""));
        match word {
            "progress" if id == "translation" => status = Some(Status::Running(detail.to_string())),
            "ok" if id == "translation" => status = Some(Status::Done),
            "warn" if id == "translation" => {
                status = Some(Status::Failed(detail.split(". Пока").next().unwrap_or(detail).to_string()))
            }
            "error" => status = Some(Status::Failed(rest.to_string())),
            "finish" => finished = true,
            _ => {}
        }
    }
    if ended && !matches!(status, Some(Status::Done | Status::Failed(_))) {
        return Some(Status::Failed(
            if finished {
                "Установка ничего не сообщила"
            } else {
                "Установка прервалась"
            }
            .into(),
        ));
    }
    status
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process);
        }
        let _ = std::fs::remove_file(&self.progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_script_from_download_to_the_end() {
        assert_eq!(read("", false), None);
        let running =
            "\u{feff}ok prepare\r\nstep translation\r\nprogress translation Загрузка 12 из 400 МБ · 6,1 МБ/с\r\n";
        assert_eq!(read(running, false), Some(Status::Running("Загрузка 12 из 400 МБ · 6,1 МБ/с".into())));
        assert_eq!(read(&format!("{running}ok translation\r\nfinish\r\n"), true), Some(Status::Done));
        let warned = format!("{running}warn translation Нет сети. Пока переводит Google; докачать…\r\nfinish\r\n");
        assert_eq!(read(&warned, true), Some(Status::Failed("Нет сети".into())));
        assert_eq!(
            read("error Kadr ещё не установлен.\r\n", true),
            Some(Status::Failed("Kadr ещё не установлен.".into()))
        );
        assert_eq!(read(running, true), Some(Status::Failed("Установка прервалась".into())));
    }
}
