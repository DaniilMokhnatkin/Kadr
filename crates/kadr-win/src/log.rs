//! Минимальный лог: stderr (в debug-сборке есть консоль) и файл `%LOCALAPPDATA%\Kadr\logs\kadr.log`.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

static FILE: OnceLock<Mutex<File>> = OnceLock::new();
static PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn init() {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return };
    let dir = PathBuf::from(base).join("Kadr").join("logs");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("kadr.log");
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = FILE.set(Mutex::new(file));
        let _ = PATH.set(path);
    }
}

pub fn path() -> Option<&'static PathBuf> {
    PATH.get()
}

/// The last `n` lines of the log file, for a problem report.
pub fn tail(n: usize) -> Vec<String> {
    let Some(text) = path().and_then(|p| fs::read(p).ok()) else { return Vec::new() };
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| l.to_string()).collect()
}

pub fn write(level: &str, args: fmt::Arguments) {
    let t = unsafe { GetLocalTime() };
    let line = format!("{:02}:{:02}:{:02}.{:03} {level:<5} {args}\n", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
    eprint!("{line}");
    if let Some(Ok(mut file)) = FILE.get().map(Mutex::lock) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! log_info {
    ($($t:tt)*) => { $crate::log::write("INFO", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($t:tt)*) => { $crate::log::write("WARN", format_args!($($t)*)) };
}
