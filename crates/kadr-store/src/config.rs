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
    pub appearance: Appearance,
    pub recognition: Recognition,
    pub translation: Translation,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all="lowercase")]
pub enum TranslationEngine { #[default] Auto, Offline, Google }
impl TranslationEngine {pub fn name(self)->&'static str {match self {Self::Auto=>"auto",Self::Offline=>"offline",Self::Google=>"google"}}}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Translation {pub engine:TranslationEngine}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OcrEngine {
    /// What was chosen in the setup; PaddleOCR when the setup said nothing.
    #[default]
    Auto,
    Windows,
    Paddle,
}
impl OcrEngine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Windows => "windows",
            Self::Paddle => "paddle",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Recognition {
    pub engine: OcrEngine,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follows Windows «app mode».
    #[default]
    Auto,
    Light,
    Dark,
}
impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: Theme,
    pub marching_ants: bool,
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
    /// The zoomed patch with the pixel's colour beside the cursor while aiming.
    pub magnifier: bool,
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
        Self { warm: true, magnifier: true }
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
magnifier = true  # лупа с цветом пикселя у курсора, пока выбираете область

[save]
folder = ""     # пусто — «Изображения\Screenshots»
notify = true   # уведомление после сохранения
thumbnail = true  # миниатюра в углу: перетащите в чат, клик — открыть

[recent]
enabled = true  # сохранять несохранённые снимки зашифрованно
max = 10        # от 1 до 10; старые записи вытесняются

[appearance]
theme = "auto"  # auto — как в Windows, light — светлая, dark — тёмная
marching_ants = false # бегущий пунктир рамки и выбранных объектов

[recognition]
engine = "auto"  # auto — как выбрано при установке, windows — встроенное в Windows, paddle — PaddleOCR

[translation]
engine = "auto"  # auto — офлайн при наличии моделей, offline — только локально, google — онлайн
"#;

/// The file as the settings window writes it: the template's layout and comments with these values.
pub fn render(c: &Config) -> String {
    let quoted = |s: &str| {
        if s.is_empty() {
            "\"\"".to_string()
        } else if !s.contains('\'') && !s.chars().any(char::is_control) {
            // Literal string: Windows paths keep their backslashes as typed.
            format!("'{s}'")
        } else {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
    };
    let basic = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let key = |name: &str, value: &str, comment: &str| format!("{name:<11} = {:<26}# {comment}\n", basic(value));
    let h = &c.hotkeys;
    let mut out = String::from("# Настройки Kadr. Изменения применяются сразу после сохранения файла.\n\n[hotkeys]\n");
    out.push_str("# Пустая строка выключает хоткей. Модификаторы: Ctrl, Alt, Shift, Win.\n");
    out.push_str(&key("region", &h.region, "выделить область"));
    out.push_str(&key("fullscreen", &h.fullscreen, "монитор под курсором — сразу в буфер"));
    out.push_str(&key("window", &h.window, "активное окно — сразу в буфер"));
    out.push_str(&key("last_region", &h.last_region, "та же область, что в прошлый раз"));
    out.push_str(&key("recent", &h.recent, "вернуть несохранённый снимок"));
    out.push_str(&format!(
        "\n[capture]\nwarm = {:<7}# держать захват наготове: ~8 мс вместо ~23 мс\nmagnifier = {:<6}# лупа с цветом пикселя у курсора, пока выбираете область\n",
        c.capture.warm, c.capture.magnifier
    ));
    out.push_str(&format!(
        "\n[save]\nfolder = {:<7}# пусто — «Изображения\\Screenshots»\nnotify = {:<7}# уведомление после сохранения\nthumbnail = {:<6}# миниатюра в углу: перетащите в чат, клик — открыть\n",
        quoted(&c.save.folder),
        c.save.notify,
        c.save.thumbnail
    ));
    out.push_str(&format!(
        "\n[recent]\nenabled = {:<6}# сохранять несохранённые снимки зашифрованно\nmax = {:<10}# от 1 до 10; старые записи вытесняются\n",
        c.recent.enabled, c.recent.max
    ));
    out.push_str(&format!(
        "\n[appearance]\ntheme = {:<8}# auto — как в Windows, light — светлая, dark — тёмная\n",
        basic(c.appearance.theme.name())
    ));
    out.push_str(&format!("marching_ants = {} # бегущий пунктир рамки и выбранных объектов\n", c.appearance.marching_ants));
    out.push_str(&format!(
        "\n[recognition]\nengine = {:<8}# auto — как выбрано при установке, windows — встроенное в Windows, paddle — PaddleOCR\n",
        basic(c.recognition.engine.name())
    ));
    out.push_str(&format!("\n[translation]\nengine = {:<8}# auto — офлайн при наличии моделей, offline — только локально, google — онлайн\n",basic(c.translation.engine.name())));
    out
}

/// Validates and writes atomically; the file watcher then applies it like a hand edit.
pub fn save(path: &Path, c: &Config) -> Result<(), String> {
    let text = render(c);
    Config::parse(&text)?;
    let part = path.with_extension("toml.part");
    std::fs::write(&part, &text).map_err(|e| e.to_string())?;
    std::fs::rename(&part, path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        e.to_string()
    })
}

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
    fn rendered_file_matches_the_template_and_round_trips() {
        assert_eq!(render(&Config::default()), TEMPLATE.replace("\r\n", "\n"));
        let mut c = Config::default();
        c.hotkeys.region = "Ctrl+Alt+Shift+Win+F12".into();
        c.hotkeys.window = String::new();
        c.save.folder = r#"D:\Снимки\It's "here""#.into();
        c.save.thumbnail = false;
        c.capture.warm = false;
        c.recent.max = 3;
        c.appearance.theme = Theme::Dark;
        c.recognition.engine = OcrEngine::Paddle;
        assert_eq!(Config::parse(&render(&c)).unwrap(), c);
        c.save.folder = r"C:\Users\me\Pictures".into();
        assert!(render(&c).contains(r"folder = 'C:\Users\me\Pictures'"));
        assert_eq!(Config::parse(&render(&c)).unwrap(), c);
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
