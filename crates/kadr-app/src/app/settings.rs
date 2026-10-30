//! Применение настроек и регистрация хоткеев резидентного приложения.
use super::*;

impl App {
    pub(super) fn register_hotkeys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for (id, (_, text)) in
            [HK_PRTSC, HK_FULLSCREEN, HK_WINDOW, HK_LAST_REGION, HK_RECENT].into_iter().zip(self.config.hotkeys.entries())
        {
            if let Ok(Some(key)) = hotkey::parse(text) {
                keys.push((id, key, text));
            }
        }
        let backup = hotkey::parse("Ctrl+Shift+F12").unwrap().unwrap();
        if !keys.iter().any(|(_, key, _)| *key == backup) {
            keys.push((HK_BACKUP, backup, "Ctrl+Shift+F12"));
        }
        let mut conflicts = Vec::new();
        for (id, key, name) in keys {
            match unsafe { RegisterHotKey(Some(self.hwnd), id, hotkey_modifiers(key), u32::from(key.vk)) } {
                Ok(()) => log_info!("хоткей {name}: зарегистрирован"),
                Err(e) => {
                    log_warn!("хоткей {name}: не удалось зарегистрировать ({e})");
                    conflicts.push(name.to_string());
                }
            }
        }
        conflicts
    }

    pub(super) fn unregister_hotkeys(&self) {
        for id in [HK_PRTSC, HK_BACKUP, HK_FULLSCREEN, HK_WINDOW, HK_LAST_REGION, HK_RECENT] {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
        }
    }

    pub(super) fn on_config(&mut self) {
        while let Ok(result) = self.watcher.updates.try_recv() {
            match result {
                Ok(config) => self.pending_config = Some(config),
                Err(error) => {
                    self.pending_config = None;
                    log_warn!("настройки не применены: {error}");
                    self.notify("Настройки не применены", &error);
                }
            }
        }
        if self.active.is_none() {
            self.apply_pending_config();
        }
    }

    pub(super) fn apply_pending_config(&mut self) {
        let Some(config) = self.pending_config.take() else { return };
        if config == self.config {
            return;
        }
        let changed_keys = config.hotkeys != self.config.hotkeys;
        let changed_look = config.appearance != self.config.appearance;
        if changed_keys {
            self.unregister_hotkeys();
        }
        let previous = std::mem::replace(&mut self.config, config);
        if changed_keys {
            let conflicts = self.register_hotkeys();
            if !conflicts.is_empty() {
                self.unregister_hotkeys();
                self.config = previous;
                let _ = self.register_hotkeys();
                log_warn!("настройки не применены: заняты {}", conflicts.join(", "));
                self.notify(
                    "Настройки не применены",
                    &format!("Хоткей занят: {}. Сохранены прежние настройки.", conflicts.join(", ")),
                );
                crate::settings_window::refresh(&self.config);
                return;
            }
        }
        self.set_warm(self.warm_override.unwrap_or(self.config.capture.warm));
        crate::ocr::choose(self.config.recognition.engine);
        self.recent.send(crate::recent_worker::Command::Limit(self.config.recent.max));
        crate::settings_window::refresh(&self.config);
        if changed_look {
            super::choose_theme(&self.config);
            self.on_theme_change();
        }
        log_info!("настройки применены: {}", self.config_path.display());
    }

    /// Changes from the settings window go through config.toml, so the watcher applies them
    /// exactly like a hand edit.
    pub(super) fn on_settings_window(&mut self) {
        for request in crate::settings_window::take_requests() {
            match request {
                crate::settings_window::Request::Save(config) => {
                    if let Err(e) = kadr_store::config::save(&self.config_path, &config) {
                        log_warn!("настройки не записаны: {e}");
                        self.notify("Настройки не сохранены", &e);
                    }
                }
                crate::settings_window::Request::Suspend(true) => self.unregister_hotkeys(),
                crate::settings_window::Request::Suspend(false) => {
                    let _ = self.register_hotkeys();
                }
                crate::settings_window::Request::OpenFile => {
                    let path = wide(&format!("\"{}\"", self.config_path.display()));
                    unsafe {
                        ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(path.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
                    }
                }
            }
        }
    }

    pub(super) fn set_warm(&mut self, warm: bool) {
        if self.warm == warm {
            return;
        }
        self.warm = warm;
        if warm {
            self.warm_up();
        } else {
            for out in &mut self.outputs {
                out.release_duplication();
                out.release_frame();
            }
        }
    }
}

fn hotkey_modifiers(key: hotkey::Hotkey) -> HOT_KEY_MODIFIERS {
    let mut modifiers = MOD_NOREPEAT;
    for (on, flag) in [(key.ctrl, MOD_CONTROL), (key.alt, MOD_ALT), (key.shift, MOD_SHIFT), (key.win, MOD_WIN)] {
        if on {
            modifiers |= flag;
        }
    }
    modifiers
}
