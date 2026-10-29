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
    pub recent: Recent,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hotkeys {
    pub region: String,
    pub fullscreen: String,
    pub window: String,
    pub last_region: String,
    pub recent: String,
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
    /// Floating thumbnail in the corner after a capture.
    pub thumbnail: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Recent { pub enabled: bool, pub max: usize }
impl Default for Recent { fn default() -> Self { Self { enabled: true, max: 10 } } }

impl Config {
    /// Проверяем весь набор до замены действующих настроек и регистрации клавиш.
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if !(1..=10).contains(&config.recent.max) { return Err("recent.max должен быть от 1 до 10".into()); }
        // Относительный путь считался бы от текущей папки процесса, а при автозапуске она случайна.
        let folder = config.save.folder.trim();
        if !folder.is_empty() && !Path::new(folder).is_absolute() {
            return Err(format!("save.folder: нужен полный путь, например C:\\Снимки, а не «{folder}»"));
        }
        let mut seen = Vec::new();
        for (name, text) in config.hotkeys.entries() {
            if let Some(key) = crate::hotkey::parse(text).map_err(|e| format!("hotkeys.{name}: {e}"))? {
                if let Some((other, _)) = seen.iter().find(|(_, previous)| *previous == key) {
                    return Err(format!("hotkeys.{name} и hotkeys.{other}: одна комбинация «{text}»"));
                }
                seen.push((name, key));
            }
        }
        Ok(config)
    }
}

impl Hotkeys {
    pub fn entries(&self) -> [(&'static str, &str); 5] {
        [
            ("region", &self.region),
            ("fullscreen", &self.fullscreen),
            ("window", &self.window),
            ("last_region", &self.last_region),
            ("recent", &self.recent),
        ]
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            region: "PrintScreen".into(),
            fullscreen: "Shift+PrintScreen".into(),
            window: "Ctrl+PrintScreen".into(),
            last_region: "Ctrl+Shift+PrintScreen".into(),
            recent: "Ctrl+Alt+PrintScreen".into(),
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
        Self { folder: String::new(), notify: true, thumbnail: true }
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
recent      = "Ctrl+Alt+PrintScreen"    # вернуть несохранённый снимок

[capture]
warm = true   # держать захват наготове: ~8 мс вместо ~23 мс

[save]
folder = ""     # пусто — «Изображения\Screenshots»
notify = true   # уведомление после сохранения
thumbnail = true  # миниатюра в углу: перетащите в чат, клик — открыть

[recent]
enabled = true  # сохранять несохранённые снимки зашифрованно
max = 10        # от 1 до 10; старые записи вытесняются
"#;

pub fn path() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Kadr").join("config.toml")
}

/// Читает конфиг; вторым значением — текст ошибки, если файл не разобрался.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match Config::parse(&text) {
            Ok(config) => (config, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent()
                && let Err(e) = std::fs::create_dir_all(dir)
            {
                return (Config::default(), Some(e.to_string()));
            }
            let error = std::fs::write(path, TEMPLATE).err().map(|e| e.to_string());
            (Config::default(), error)
        }
        Err(e) => (Config::default(), Some(e.to_string())),
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

    #[test]
    fn rejects_invalid_or_duplicate_hotkeys_before_applying() {
        assert!(Config::parse("[hotkeys]\nregion = 'Hyper+Q'").is_err());
        assert!(Config::parse("[hotkeys]\nfullscreen = 'PrtSc'").is_err());
        let config = Config::parse("[hotkeys]\nfullscreen = ''\nwindow = 'Alt+F9'").unwrap();
        assert!(config.hotkeys.fullscreen.is_empty());
        assert_eq!(config.hotkeys.window, "Alt+F9");
    }

    #[test]
    fn save_folder_must_be_absolute_or_empty() {
        assert!(Config::parse("[save]\nfolder = 'Снимки'").is_err());
        assert!(Config::parse("[save]\nfolder = '\\Снимки'").is_err());
        assert!(Config::parse("[save]\nfolder = 'D:\\Снимки'").is_ok());
        assert!(Config::parse("[save]\nfolder = '\\\\nas\\shots'").is_ok());
        assert!(Config::parse("[save]\nfolder = ''").is_ok());
    }

    #[test]
    fn read_error_does_not_overwrite_existing_path() {
        let dir = std::env::temp_dir().join(format!("kadr-config-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (_, error) = load(&dir);
        assert!(error.is_some(), "ошибку чтения нельзя скрывать как отсутствие конфига");
        assert!(dir.is_dir());
        std::fs::remove_dir(&dir).unwrap();
    }
}
