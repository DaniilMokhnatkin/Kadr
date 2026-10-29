//! Хоткей из строки вида `"Ctrl+Shift+PrintScreen"`. Пустая строка — хоткей выключен.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Виртуальный код клавиши Windows (VK_*).
    pub vk: u16,
}

pub fn parse(text: &str) -> Result<Option<Hotkey>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mut hotkey = Hotkey::default();
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().ok_or_else(|| format!("пустой хоткей «{text}»"))?;
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => hotkey.ctrl = true,
            "alt" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "win" => hotkey.win = true,
            other => return Err(format!("неизвестный модификатор «{other}» в «{text}»")),
        }
    }
    hotkey.vk = vk(key).ok_or_else(|| format!("неизвестная клавиша «{key}» в «{text}»"))?;
    Ok(Some(hotkey))
}

fn vk(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "printscreen" | "prtsc" | "print" => 0x2C,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "space" => 0x20,
        _ => {
            let bytes = lower.as_bytes();
            if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
                return Some(bytes[0].to_ascii_uppercase() as u16);
            }
            let n: u16 = lower.strip_prefix('f')?.parse().ok()?;
            return (1..=24).contains(&n).then_some(0x70 + n - 1);
        }
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_keys() {
        let h = parse("Ctrl+Shift+PrintScreen").unwrap().unwrap();
        assert!(h.ctrl && h.shift && !h.alt);
        assert_eq!(h.vk, 0x2C);
        assert_eq!(parse("alt + f12").unwrap().unwrap().vk, 0x7B);
        assert_eq!(parse("Win+S").unwrap().unwrap().vk, b'S' as u16);
    }

    #[test]
    fn empty_disables_and_garbage_errors() {
        assert_eq!(parse("  "), Ok(None));
        assert!(parse("Hyper+Q").is_err());
        assert!(parse("Ctrl+F99").is_err());
    }
}
