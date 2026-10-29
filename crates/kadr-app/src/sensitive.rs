//! Finds what should not leave the computer in a screenshot: e-mail addresses, phone numbers, card
//! numbers (Luhn-checked), IP addresses, IBANs and key- or token-like strings. Works on recognised
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
                let groups = &words[i..=j];
                let plain = groups.iter().all(|(w, _)| w.chars().all(|c| c.is_ascii_digit() || c == '-'));
                if plain && (13..=19).contains(&digits.len()) && luhn(&digits) {
                    best = Some((j + 1, Kind::Card));
                } else if phone(groups.iter().map(|(w, _)| w.as_str()), &digits) {
                    best = Some((j + 1, Kind::Phone));
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
        for (i, (w, _)) in words.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let kind = if email(w) {
                Some(Kind::Email)
            } else if ip(w) {
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

/// «+7 912 345-67-89», «8 (912) 345 67 89», «+1-202-555-0143». Dates and times use other separators.
fn phone<'a>(groups: impl Iterator<Item = &'a str> + Clone, digits: &str) -> bool {
    let text: String = groups.clone().collect::<Vec<_>>().join(" ");
    let separated = groups.count() > 1 || text.contains(['-', '(', '+']);
    let international = text.starts_with('+') && (10..=15).contains(&digits.len());
    let national = (text.starts_with('8') || text.starts_with('7')) && digits.len() == 11;
    let punctuated = (text.contains('-') || text.contains('(')) && (10..=12).contains(&digits.len());
    separated && (international || national || punctuated)
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
        assert_eq!(kinds("key sk-proj-4f9aZ81kLq0x77Y2bN token"), [Kind::Secret]);
        assert_eq!(kinds("ghp_16C7e42F292c6912E7710c838347Ae178B4a"), [Kind::Secret]);
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
        ] {
            assert!(kinds(text).is_empty(), "{text}: {:?}", kinds(text));
        }
    }
}
