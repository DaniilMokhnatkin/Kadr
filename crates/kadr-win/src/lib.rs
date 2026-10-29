//! Тонкие обёртки над Win32 и общие утилиты процесса: лог, тайминги, привилегии, строки UTF-16.

pub mod log;
pub mod perf;
pub mod privileges;

/// UTF-16 с нулём на конце — для параметров Win32.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// UTF-16 до первого нуля → String.
pub fn from_wide(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}
pub mod protect;
