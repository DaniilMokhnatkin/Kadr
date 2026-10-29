//! Kadr — M1: выделение, экспорт PNG, быстрые режимы захвата и конфиг. См. STATUS.md.
//!
//! Запуск:
//!   kadr                     — резидентный режим (трей), PrtSc или Ctrl+Shift+F12 — скрин
//!   kadr --warm / --cold     — переопределить capture.warm из конфига
//!   kadr --perf-hud          — показывать панель замеров на оверлее
//!   kadr --bench [N]         — N захватов подряд (по умолчанию 20), p50/p95 в лог, выход
//!   kadr --snapshot [a.png] [--scale 1.5] — сцена оверлея в PNG для ревью дизайна

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod capture_mode;
mod config_watch;
mod recent_worker;
mod recent_window;
mod snapshot;

use std::error::Error;

use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

fn main() {
    kadr_win::log::init();
    // Основной способ — манифест; это страховка, если exe собран без него.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name);
    let result: Result<(), Box<dyn Error>> = match flag("--snapshot") {
        Some(i) => {
            let path = args.get(i + 1).filter(|a| !a.starts_with("--")).map_or("kadr-snapshot.png", String::as_str);
            let scale = flag("--scale").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1.0);
            snapshot::run(path, scale)
        }
        None => {
            let bench = flag("--bench").map(|i| args.get(i + 1).and_then(|n| n.parse().ok()).unwrap_or(20));
            let warm = if flag("--cold").is_some() { Some(false) }
                else { flag("--warm").map(|_| true) };
            app::run(warm, flag("--perf-hud").is_some(), bench).map_err(Into::into)
        }
    };
    if let Err(e) = result {
        kadr_win::log_warn!("ошибка: {e}");
        std::process::exit(1);
    }
}
