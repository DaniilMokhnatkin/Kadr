//! Ожидание уведомлений файловой системы: в простое не читаем диск, UI-поток не ждёт I/O.
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, channel},
};
use std::time::Duration;

use kadr_store::config::Config;
use kadr_win::wide;
use windows::Win32::Foundation::{HWND, LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::PCWSTR;

pub struct Watcher {
    pub updates: Receiver<Result<Config, String>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn start(path: PathBuf, host: HWND, message: u32) -> Self {
        let (tx, updates) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let host = host.0 as isize;
        let thread = std::thread::spawn(move || {
            let send = |result| {
                if stopped.load(Ordering::Relaxed) || tx.send(result).is_err() {
                    return;
                }
                unsafe {
                    let _ = PostMessageW(Some(HWND(host as *mut _)), message, WPARAM(0), LPARAM(0));
                }
            };
            let Some(parent) = path.parent() else { return };
            let dir = wide(&parent.to_string_lossy());
            let handle = unsafe {
                FindFirstChangeNotificationW(
                    PCWSTR(dir.as_ptr()),
                    false,
                    FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
                )
            };
            let handle = match handle {
                Ok(handle) => handle,
                Err(e) => {
                    send(Err(format!("наблюдение за конфигом: {e}")));
                    return;
                }
            };
            // Перечитываем после установки наблюдения: правка при запуске не потеряется.
            let mut previous = None;
            loop {
                let current = std::fs::read_to_string(&path).map_err(|e| e.to_string());
                if previous.as_ref() != Some(&current) {
                    send(current.as_ref().map_err(Clone::clone).and_then(|text| Config::parse(text)));
                    previous = Some(current);
                }
                loop {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let status = unsafe { WaitForSingleObject(handle, 1000) };
                    if status == WAIT_OBJECT_0 {
                        break;
                    }
                    if status != WAIT_TIMEOUT {
                        send(Err("не удалось дождаться изменения конфига".into()));
                        stopped.store(true, Ordering::Relaxed);
                    }
                }
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = unsafe { FindNextChangeNotification(handle) } {
                    send(Err(e.to_string()));
                    break;
                }
                // Редакторы могут писать в несколько приёмов или заменять файл переименованием.
                std::thread::sleep(Duration::from_millis(120));
            }
            unsafe {
                let _ = FindCloseChangeNotification(handle);
            }
        });
        Self { updates, stop, thread: Some(thread) }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_invalid_then_repaired_config_and_atomic_replacement() {
        let dir = std::env::temp_dir().join(format!("kadr-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, kadr_store::config::TEMPLATE).unwrap();
        let watcher = Watcher::start(path.clone(), HWND::default(), 0);
        let next = || watcher.updates.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next().unwrap(), Config::default());
        std::fs::write(&path, "[hotkeys]\nregion = 'Hyper+Q'").unwrap();
        assert!(next().is_err());
        let replacement = dir.join("next.toml");
        std::fs::write(&replacement, "[capture]\nwarm = false\n[save]\nnotify = false").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let repaired = next().unwrap();
        assert!(!repaired.capture.warm && !repaired.save.notify);
        drop(watcher);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
