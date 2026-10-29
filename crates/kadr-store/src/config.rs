//! `%APPDATA%\Kadr\config.toml`. Нет файла — пишем шаблон с комментариями; ошибка в файле —
//! работаем на значениях по умолчанию и сообщаем, что не так. Неизвестные поля не мешают.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub hotkeys: Hotkeys,
    pub capture: Capture,
    pub save: Save,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hotkeys {
    pub region: String,
    pub fullscreen: String,
    pub window: String,
    pub last_region: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Capture {
    pub warm: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Save {
    /// Пусто — «Изображения\Screenshots».
    pub folder: String,
    pub notify: bool,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            region: "PrintScreen".into(),
            fullscreen: "Shift+PrintScreen".into(),
            window: "Ctrl+PrintScreen".into(),
            last_region: "Ctrl+Shift+PrintScreen".into(),
        }
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self { warm: true }
    }
}

impl Default for Save {
    fn default() -> Self {
        Self { folder: String::new(), notify: true }
    }
}

/// Шаблон, который пишется при первом запуске. Тест следит, чтобы он совпадал с `Default`.
pub const TEMPLATE: &str = r#"# Настройки Kadr. Изменения применяются сразу после сохранения файла.

[hotkeys]
# Пустая строка выключает хоткей. Модификаторы: Ctrl, Alt, Shift, Win.
region      = "PrintScreen"             # выделить область
fullscreen  = "Shift+PrintScreen"       # монитор под курсором — сразу в буфер
window      = "Ctrl+PrintScreen"        # активное окно — сразу в буфер
last_region = "Ctrl+Shift+PrintScreen"  # та же область, что в прошлый раз

[capture]
warm = true   # держать захват наготове: ~8 мс вместо ~23 мс

[save]
folder = ""     # пусто — «Изображения\Screenshots»
notify = true   # уведомление после сохранения
"#;

pub fn path() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Kadr").join("config.toml")
}

/// Читает конфиг; вторым значением — текст ошибки, если файл не разобрался.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(config) => (config, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        },
        Err(_) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, TEMPLATE);
            (Config::default(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_defaults() {
        assert_eq!(toml::from_str::<Config>(TEMPLATE).unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config: Config = toml::from_str("[capture]\nwarm = false\nunknown = 1\n").unwrap();
        assert!(!config.capture.warm);
        assert_eq!(config.hotkeys, Hotkeys::default());
    }
}
