//! Finds what should not leave the computer in a screenshot: e-mail addresses, phone numbers, card
//! numbers (Luhn-checked), passport, SNILS and INN numbers (checksums), IPv4/IPv6 addresses, IBANs,
//! key- or token-like strings and the value after «Пароль:», «PIN:», «token=» and the like. Works on recognised
//! words, so the result is the exact boxes to blur. A helper, not a guarantee: the user sees and
//! can undo every blur.
use crate::ocr::Line;
use kadr_doc::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Email,
    Phone,
    Card,
    Ip,
    Iban,
    Secret,
    Passport,
    Snils,
    Inn,
    /// The value written after a label such as «Пароль:».
    Password,
    /// The value after «Логин», «Login:», «Username:».
    Login,
}

/// Boxes to blur, one per finding, slightly padded so glyph edges are covered too.
pub fn find(lines: &[Line]) -> Vec<(Kind, Rect)> {
    let mut out = Vec::new();
    for line in lines {
        let words: Vec<(String, Rect)> = line
            .words
            .iter()
            .map(|(w, r)| (w.trim_matches(|c: char| ",;:!?()[]{}«»\"'".contains(c)).to_string(), *r))
            .collect();
        let mut taken = vec![false; words.len()];
        let mark = |from: usize, to: usize, kind: Kind, taken: &mut Vec<bool>, out: &mut Vec<(Kind, Rect)>| {
            let r = words[from..to].iter().skip(1).fold(words[from].1, |r, (_, b)| r.union(b));
            let pad = (r.height() / 6).max(2);
            out.push((kind, Rect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad)));
            taken[from..to].iter_mut().for_each(|t| *t = true);
        };
        // Multi-word numbers first: a card or phone often arrives as several groups of digits.
        let mut i = 0;
        while i < words.len() {
            let mut best: Option<(usize, Kind)> = None;
            let mut digits = String::new();
            for j in i..words.len().min(i + 6) {
                let w = &words[j].0;
                if !w.chars().all(|c| c.is_ascii_digit() || "+-()".contains(c)) || !w.chars().any(|c| c.is_ascii_digit()) {
                    break;
                }
                digits.extend(w.chars().filter(char::is_ascii_digit));
                let groups: Vec<&str> = words[i..=j].iter().map(|(w, _)| w.as_str()).collect();
                if let Some(kind) = number(&groups, &digits) {
                    best = Some((j + 1, kind));
                }
            }
            match best {
                Some((end, kind)) => {
                    mark(i, end, kind, &mut taken, &mut out);
                    i = end;
                }
                None => i += 1,
            }
        }
        // IBAN: two letters, two digits, then groups.
        for i in 0..words.len() {
            if taken[i] {
                continue;
            }
            let mut joined = String::new();
            for j in i..words.len().min(i + 9) {
                if taken[j] || !words[j].0.chars().all(|c| c.is_ascii_alphanumeric()) {
                    break;
                }
                joined.push_str(&words[j].0);
                if iban(&joined) {
                    mark(i, j + 1, Kind::Iban, &mut taken, &mut out);
                    break;
                }
            }
        }
        // The value after a label. Raw words: trimming above would lose the colon.
        let raw: Vec<&str> = line.words.iter().map(|(w, _)| w.as_str()).collect();
        for i in 0..raw.len() {
            if taken[i] {
                continue;
            }
            let Some((how, field)) = label(raw[i], raw.get(i + 1).copied()) else { continue };
            let kind = if field == Field::Login { Kind::Login } else { Kind::Password };
            match how {
                Label::Inline => mark(i, i + 1, kind, &mut taken, &mut out),
                Label::Next => {
                    let mut j = i + 1;
                    if raw.get(j).is_some_and(|w| matches!(*w, ":" | "=")) {
                        j += 1;
                    }
                    if j < raw.len() && !taken[j] {
                        mark(j, j + 1, kind, &mut taken, &mut out);
                    }
                }
                Label::Bare => {
                    let j = i + 1;
                    if j < raw.len() && !taken[j] && looks_like_value(raw[i], raw[j]) {
                        mark(j, j + 1, kind, &mut taken, &mut out);
                    }
                }
            }
        }
        for (i, (w, _)) in words.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let kind = if email(w) {
                Some(Kind::Email)
            } else if ip(w) || ipv6(w) {
                Some(Kind::Ip)
            } else if secret(w) {
                Some(Kind::Secret)
            } else {
                None
            };
            if let Some(kind) = kind {
                mark(i, i + 1, kind, &mut taken, &mut out);
            }
        }
    }
    out
}

/// What a run of digit groups is, most specific first: checksums beat shapes.
fn number(groups: &[&str], digits: &str) -> Option<Kind> {
    let digits_only = |w: &&str| w.chars().all(|c| c.is_ascii_digit());
    let plain = groups.iter().all(|w| w.chars().all(|c| c.is_ascii_digit() || c == '-'));
    if plain && (13..=19).contains(&digits.len()) && luhn(digits) {
        return Some(Kind::Card);
    }
    if plain && digits.len() == 11 && snils(digits) {
        return Some(Kind::Snils);
    }
    if groups.len() == 1 && digits_only(&groups[0]) && matches!(digits.len(), 10 | 12) && inn(digits) {
        return Some(Kind::Inn);
    }
    // Russian passport: «4510 123456» or «45 10 123456».
    let lens: Vec<usize> = groups.iter().map(|w| w.len()).collect();
    if groups.iter().all(digits_only) && (lens == [4, 6] || lens == [2, 2, 6]) {
        return Some(Kind::Passport);
    }
    phone(groups, digits).then_some(Kind::Phone)
}

/// SNILS control number: weights 9..1 over the first nine digits, modulo 101.
fn snils(d: &str) -> bool {
    let n: Vec<u32> = d.bytes().map(|b| (b - b'0') as u32).collect();
    let sum: u32 = n[..9].iter().zip((1..=9).rev()).map(|(d, w)| d * w).sum();
    let control = match sum {
        s if s < 100 => s,
        100 | 101 => 0,
        s => match s % 101 {
            100 => 0,
            r => r,
        },
    };
    control == n[9] * 10 + n[10]
}

/// INN (10 digits for companies, 12 for people) with its control digits.
fn inn(d: &str) -> bool {
    let n: Vec<u32> = d.bytes().map(|b| (b - b'0') as u32).collect();
    let check = |weights: &[u32], len: usize| weights.iter().zip(&n[..len]).map(|(w, d)| w * d).sum::<u32>() % 11 % 10;
    match n.len() {
        10 => check(&[2, 4, 10, 3, 5, 9, 4, 6, 8], 9) == n[9],
        12 => {
            check(&[7, 2, 4, 10, 3, 5, 9, 4, 6, 8], 10) == n[10]
                && check(&[3, 7, 2, 4, 10, 3, 5, 9, 4, 6, 8], 11) == n[11]
        }
        _ => false,
    }
}

/// «2001:db8::1», «fe80:0:0:0:200:f8ff:fe21:67cf». Times («12:30:45») have too few groups.
fn ipv6(w: &str) -> bool {
    let groups: Vec<&str> = w.split(':').collect();
    let compressed = w.contains("::");
    let valid = groups.iter().all(|g| g.len() <= 4 && g.chars().all(|c| c.is_ascii_hexdigit()));
    valid && groups.len() <= 9 && (compressed && groups.len() >= 3 || groups.len() >= 6)
}

/// Labels whose value is a secret, and labels whose value is a login.
const SECRET_LABELS: [&str; 20] = [
    "пароль", "password", "passwd", "pass", "pwd", "пин", "pin", "пин-код", "pin-код", "код", "otp", "token", "токен",
    "secret", "секрет", "ключ", "key", "apikey", "api_key", "api-key",
];
const LOGIN_LABELS: [&str; 7] = ["логин", "login", "username", "user", "пользователь", "учётка", "аккаунт"];

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Secret,
    Login,
}

enum Label {
    /// «password=hunter2»: the word itself holds the value.
    Inline,
    /// «Пароль:» or «Пароль :»: the value is the next word.
    Next,
    /// «Пароль FSDf*4»: no separator, so the next word counts only if it looks like a value.
    Bare,
}

fn field(head: &str) -> Option<Field> {
    if SECRET_LABELS.contains(&head) {
        Some(Field::Secret)
    } else if LOGIN_LABELS.contains(&head) {
        Some(Field::Login)
    } else {
        None
    }
}

fn label(word: &str, next: Option<&str>) -> Option<(Label, Field)> {
    let lower = word.to_lowercase();
    if let Some(k) = lower.find([':', '=']) {
        let head = lower[..k].trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        let f = field(head)?;
        return Some((if lower[k + 1..].trim().is_empty() { Label::Next } else { Label::Inline }, f));
    }
    let f = field(lower.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_'))?;
    Some((if matches!(next, Some(":" | "=")) { Label::Next } else { Label::Bare }, f))
}

fn cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// After a label without a colon: a value looks generated or foreign to the sentence around it.
/// «Пароль изменён», «Логин или пароль неверны», «Password must be…» stay sharp.
fn looks_like_value(label: &str, value: &str) -> bool {
    let v = value.trim_matches(|c: char| ",;!?()[]{}«»\"'".contains(c));
    if v.chars().count() < 3 {
        return false;
    }
    let digits = v.chars().any(|c| c.is_ascii_digit());
    let symbols = v.chars().any(|c| !c.is_alphanumeric());
    let inner_upper = v.chars().skip(1).any(char::is_uppercase) && v.chars().any(char::is_lowercase);
    // A Latin word right after a Russian label is a value, not the rest of a Russian sentence.
    let foreign = label.chars().any(cyrillic) && !v.chars().any(cyrillic);
    digits || symbols || inner_upper || foreign
}

fn luhn(digits: &str) -> bool {
    let sum: u32 = digits
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| {
            let d = (b - b'0') as u32;
            if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// «+7 912 345-67-89», «8 (912) 345 67 89», «89123456789», «+1-202-555-0143». Dates and times
/// use other separators.
fn phone(groups: &[&str], digits: &str) -> bool {
    let text = groups.join(" ");
    let international = text.starts_with('+') && (10..=15).contains(&digits.len());
    let national = (text.starts_with('8') || text.starts_with('7')) && digits.len() == 11;
    let punctuated = (text.contains('-') || text.contains('(')) && (10..=12).contains(&digits.len());
    // «89123456789» in one run counts too: 11 digits after 8 or 7 is a Russian number.
    international || national || punctuated
}

fn email(w: &str) -> bool {
    let Some((local, domain)) = w.split_once('@') else { return false };
    let local_ok = !local.is_empty() && local.chars().all(|c| c.is_alphanumeric() || "._%+-".contains(c));
    let Some((name, tld)) = domain.rsplit_once('.') else { return false };
    local_ok
        && !name.is_empty()
        && domain.chars().all(|c| c.is_alphanumeric() || ".-".contains(c))
        && tld.len() >= 2
        && tld.chars().all(char::is_alphabetic)
}

fn ip(w: &str) -> bool {
    let host = w.split(':').next().unwrap_or(w);
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n <= 255))
}

fn iban(s: &str) -> bool {
    let b = s.as_bytes();
    (15..=34).contains(&s.len())
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && {
            // ISO 13616 checksum: move the first four characters to the end, letters as numbers.
            let moved = format!("{}{}", &s[4..], &s[..4]);
            let mut rem = 0u32;
            for c in moved.chars() {
                let v = if c.is_ascii_digit() { c as u32 - '0' as u32 } else { c.to_ascii_uppercase() as u32 - 'A' as u32 + 10 };
                rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
            }
            rem == 1
        }
}

/// API keys, tokens and passwords that look generated: long, mixed letters and digits, or a known
/// prefix. Ordinary long words, paths and links are left alone.
fn secret(w: &str) -> bool {
    if w.contains("://") || w.contains('/') && w.contains('.') {
        return false;
    }
    let known = ["sk-", "sk_live_", "pk_live_", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "AIza", "eyJ"];
    let allowed = w.chars().all(|c| c.is_ascii_alphanumeric() || "_-+/=.".contains(c));
    let letters = w.chars().filter(char::is_ascii_alphabetic).count();
    let digits = w.chars().filter(char::is_ascii_digit).count();
    allowed && ((known.iter().any(|p| w.starts_with(p)) && w.len() >= 16) || (w.len() >= 20 && letters >= 4 && digits >= 3))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A line whose words sit next to each other, 10 px per character.
    fn line(text: &str) -> Line {
        let mut x = 0;
        let words = text
            .split(' ')
            .map(|w| {
                let r = Rect::new(x, 100, x + 10 * w.chars().count() as i32, 120);
                x = r.right + 10;
                (w.to_string(), r)
            })
            .collect();
        Line { text: text.into(), rect: Rect::new(0, 100, x, 120), words }
    }
    fn kinds(text: &str) -> Vec<Kind> {
        find(&[line(text)]).into_iter().map(|(k, _)| k).collect()
    }
    #[test]
    fn finds_each_kind_and_covers_only_its_words() {
        assert_eq!(kinds("Почта: daniil.petrov@gmail.com, пишите"), [Kind::Email]);
        assert_eq!(kinds("Телефон +7 912 345-67-89 после обеда"), [Kind::Phone]);
        assert_eq!(kinds("Card 4111 1111 1111 1111 exp 12/27"), [Kind::Card]);
        assert_eq!(kinds("server 192.168.1.20:8080 is up"), [Kind::Ip]);
        assert_eq!(kinds("IBAN DE89 3704 0044 0532 0130 00"), [Kind::Iban]);
        // After the label «key» it is the labelled value; alone it is still a secret.
        assert_eq!(kinds("key sk-proj-4f9aZ81kLq0x77Y2bN token"), [Kind::Password]);
        assert_eq!(kinds("use sk-proj-4f9aZ81kLq0x77Y2bN here"), [Kind::Secret]);
        assert_eq!(kinds("ghp_16C7e42F292c6912E7710c838347Ae178B4a"), [Kind::Secret]);
        assert_eq!(kinds("Звоните 89123456789 вечером"), [Kind::Phone]);
        assert_eq!(kinds("Паспорт 4510 123456 выдан"), [Kind::Passport]);
        assert_eq!(kinds("Паспорт 45 10 123456"), [Kind::Passport]);
        assert_eq!(kinds("СНИЛС 112-233-445 95"), [Kind::Snils]);
        assert_eq!(kinds("ИНН 7707083893 КПП"), [Kind::Inn]);
        assert_eq!(kinds("ИНН 500100732259"), [Kind::Inn]);
        assert_eq!(kinds("host 2001:db8::1 up"), [Kind::Ip]);
        assert_eq!(kinds("mac fe80:0:0:0:200:f8ff:fe21:67cf"), [Kind::Ip]);
        assert_eq!(kinds("Пароль: qwerty123 потом"), [Kind::Password]);
        assert_eq!(kinds("Password : hunter2"), [Kind::Password]);
        assert_eq!(kinds("pin=4815 ok"), [Kind::Password]);
        assert_eq!(kinds("Код: 573 920"), [Kind::Password]);
        // Without a colon, as people actually write it.
        assert_eq!(kinds("Логин Raptor"), [Kind::Login]);
        assert_eq!(kinds("Пароль FSDfSSR*EUITS4"), [Kind::Password]);
        assert_eq!(kinds("Login: raptor"), [Kind::Login]);
        assert_eq!(kinds("username raptor_92"), [Kind::Login]);
        assert_eq!(kinds("Пароль qwerty"), [Kind::Password]);
        // The value is blurred, not the label.
        let (_, r) = find(&[line("Пароль: секрет")])[0];
        assert!(r.left >= 70, "{r:?}");
        let found = find(&[line("Email me: a@b.co now")]);
        let (_, r) = found[0];
        // «Email» and «me:» are 0..50 and 60..90; the address is 100..160, padded by 3.
        assert!(r.left >= 95 && r.right <= 165, "{r:?}");
    }
    #[test]
    fn ordinary_text_dates_and_versions_are_left_alone() {
        for text in [
            "Встреча 30.10.2026 в 14:30",
            "Версия 1.2.3 вышла 2026-10-30",
            "Сохранено в C:/Users/me/Pictures/Screenshot-2026-10-30.png",
            "https://example.com/a1b2c3d4e5f6g7h8i9",
            "Internationalization and localization",
            "Card 4111 1111 1111 1112 is not Luhn-valid",
            "Заказ №12345 на 3 400 ₽",
            "Пароль изменён вчера",
            "Логин или пароль неверны",
            "Password must be 8 characters",
            "User guide and login help",
            "Пароль должен быть длиннее",
            "Ключевые слова: скриншот",
            "Время 12:30:45 и 09:15",
            "Номер 1234567890 не ИНН",
            "Версия 4510 сборка 1234567",
        ] {
            assert!(kinds(text).is_empty(), "{text}: {:?}", kinds(text));
        }
    }
}
