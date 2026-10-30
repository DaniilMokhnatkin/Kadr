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
    /// A one-time code on its own line, under «Вот твой код» or «Your verification code».
    Code,
    // Strict mode only.
    /// A first name, surname, patronymic or initial.
    Name,
    /// «@handle», «raptor_92», «DarkRaptor».
    Nick,
    /// Any number of two digits or more.
    Number,
    Link,
    /// The words after «ул.», «д.», «кв.» and the like.
    Address,
    /// Full mode: a whole line of text.
    Text,
}

/// Boxes to blur, one per finding, slightly padded so glyph edges are covered too.
pub fn find(lines: &[Line]) -> Vec<(Kind, Rect)> {
    let mut out = Vec::new();
    for line in lines {
        let words: Vec<(String, Rect)> = line
            .words
            .iter()
            .map(|(w, r)| (w.trim_matches(|c: char| ",;:!?()[]{}<>«»\"'".contains(c)).to_string(), *r))
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
                if !w.chars().all(|c| c.is_ascii_digit() || "+-()".contains(c))
                    || !w.chars().any(|c| c.is_ascii_digit())
                {
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
    codes_on_their_own_line(lines, &mut out);
    out
}

/// Words that announce a one-time code on a nearby line.
const CODE_WORDS: [&str; 12] = [
    "код",
    "code",
    "pin",
    "пин",
    "otp",
    "парол",
    "password",
    "подтвержд",
    "verification",
    "одноразов",
    "2fa",
    "passcode",
];

/// «Вот твой код» / «758062»: a 4–8 digit number alone on its line, with a code mentioned on one of
/// the three lines above or the line below, close enough to belong together.
fn codes_on_their_own_line(lines: &[Line], out: &mut Vec<(Kind, Rect)>) {
    for (i, line) in lines.iter().enumerate() {
        let compact: String = line.text.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
        if !(4..=8).contains(&compact.len()) || !compact.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if out.iter().any(|(_, r)| r.intersect(&line.rect).is_some()) {
            continue;
        }
        let reach = line.rect.height().max(8) * 8;
        let near = lines[i.saturating_sub(3)..(i + 2).min(lines.len())].iter().any(|other| {
            !std::ptr::eq(other, line)
                && (other.rect.top - line.rect.bottom).abs().min((line.rect.top - other.rect.bottom).abs()) <= reach
                && CODE_WORDS.iter().any(|w| other.text.to_lowercase().contains(w))
        });
        if near {
            let r = line.rect;
            let pad = (r.height() / 6).max(2);
            out.push((Kind::Code, Rect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad)));
        }
    }
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
    "пароль",
    "password",
    "passwd",
    "pass",
    "pwd",
    "пин",
    "pin",
    "пин-код",
    "pin-код",
    "код",
    "otp",
    "token",
    "токен",
    "secret",
    "секрет",
    "ключ",
    "key",
    "apikey",
    "api_key",
    "api-key",
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
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
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
                let v = if c.is_ascii_digit() {
                    c as u32 - '0' as u32
                } else {
                    c.to_ascii_uppercase() as u32 - 'A' as u32 + 10
                };
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
    allowed
        && ((known.iter().any(|p| w.starts_with(p)) && w.len() >= 16) || (w.len() >= 20 && letters >= 4 && digits >= 3))
}

/// Strict mode: on top of `find`, whatever in the text could point to a person — names, nicknames,
/// links, addresses and every number of two digits or more. Errs on the side of hiding.
pub fn find_strict(lines: &[Line]) -> Vec<(Kind, Rect)> {
    let standard = find(lines);
    let mut out = standard.clone();
    for line in lines {
        let clean: Vec<String> = line.words.iter().map(|(w, _)| trim(w).to_string()).collect();
        // «Иван Петров», «Anna Smith» on a line of their own; «Сашечка» or «Danissimo» with a time
        // or a date beside it: a chat list row or a contact.
        let names: Vec<&String> = clean.iter().filter(|w| !w.is_empty() && !stamp(w)).collect();
        let dated = clean.iter().any(|w| stamp(w)) || stamped_row(line, lines);
        let names_only =
            (1..=3).contains(&names.len()) && (names.len() >= 2 || dated) && names.iter().all(|w| capitalized(w));
        let mut address = 0;
        let mut kinds: Vec<Option<Kind>> = vec![None; line.words.len()];
        for (i, (raw, r)) in line.words.iter().enumerate() {
            let w = clean[i].as_str();
            // Already hidden by the standard pass. Padded boxes touch the next word, so «mostly
            // inside» counts, not «touches».
            if w.is_empty() || standard.iter().any(|(_, o)| mostly_inside(r, o)) {
                continue;
            }
            // The first word of a line usually starts a sentence: a capital letter there says little.
            let sentence_start = i == 0 || line.words[i - 1].0.ends_with(['.', '!', '?', '…']);
            let kind = if address > 0 {
                address = if raw.ends_with(',') { 0 } else { address - 1 };
                Some(Kind::Address)
            } else if address_word(raw) {
                address = 3;
                None
            } else if link(w) {
                Some(Kind::Link)
            } else if (raw.trim_start_matches(['(', '«', '"']).starts_with('@') && w.len() > 2) || nick(w) {
                Some(Kind::Nick)
            } else if w.chars().filter(char::is_ascii_digit).count() >= 2 {
                Some(Kind::Number)
            } else if initial(raw) || (capitalized(w) && (names_only || known_name(w) || surname(w) || !sentence_start))
            {
                Some(Kind::Name)
            } else {
                None
            };
            kinds[i] = kind;
        }
        // A name is often several capitalised words: «Just Studios», «Иван Мохнаткин». One found
        // part takes its neighbours along, the sentence-starting first word included.
        for i in 0..kinds.len() {
            if kinds[i] == Some(Kind::Name) {
                // «Подпись: Сидорова» — a colon or comma ends the name.
                let ends = |j: usize| line.words[j].0.ends_with([':', ',', ';', '.', '!', '?']);
                let free = |j: usize, kinds: &[Option<Kind>]| kinds[j].is_none() && capitalized(&clean[j]);
                let before: Vec<usize> = (0..i).rev().take_while(|&j| free(j, &kinds) && !ends(j)).collect();
                let after: Vec<usize> = (i + 1..kinds.len()).take_while(|&j| free(j, &kinds) && !ends(j - 1)).collect();
                for j in before.into_iter().chain(after) {
                    kinds[j] = Some(Kind::Name);
                }
            }
        }
        for (kind, (_, r)) in kinds.into_iter().zip(&line.words) {
            if let Some(kind) = kind {
                let pad = (r.height() / 6).max(2);
                out.push((kind, Rect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad)));
            }
        }
    }
    out
}

/// Full mode: every line of text, padded like the other findings.
pub fn find_all(lines: &[Line]) -> Vec<(Kind, Rect)> {
    lines
        .iter()
        .filter(|l| !l.text.trim().is_empty())
        .map(|l| {
            let r = l.rect;
            let pad = (r.height() / 6).max(2);
            (Kind::Text, Rect::new(r.left - pad, r.top - pad, r.right + pad, r.bottom + pad))
        })
        .collect()
}

fn mostly_inside(word: &Rect, hidden: &Rect) -> bool {
    let area = |r: Rect| r.width() as i64 * r.height() as i64;
    word.intersect(hidden).is_some_and(|i| area(i) * 2 > area(*word))
}

/// «12:24», «0:18», «Wed», «Пн», «30.10», «10.2026»: the time or date at the end of a chat row.
fn stamp(w: &str) -> bool {
    let lower = w.to_lowercase();
    let days = [
        "mon",
        "tue",
        "wed",
        "thu",
        "fri",
        "sat",
        "sun",
        "пн",
        "вт",
        "ср",
        "чт",
        "пт",
        "сб",
        "вс",
        "yesterday",
        "вчера",
    ];
    let clock = |sep: char| {
        let parts: Vec<&str> = w.split(sep).collect();
        (2..=3).contains(&parts.len())
            && parts.iter().all(|p| (1..=4).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()))
    };
    days.contains(&lower.as_str()) || clock(':') || clock('.')
}

/// Another recognised line on the same row is a time or a date: Windows OCR often reads the
/// name on the left and the time on the right of a chat row as separate lines.
fn stamped_row(line: &Line, lines: &[Line]) -> bool {
    lines.iter().any(|other| {
        let shared = other.rect.bottom.min(line.rect.bottom) - other.rect.top.max(line.rect.top);
        !std::ptr::eq(other, line)
            && other.rect.left > line.rect.right
            && shared * 2 > line.rect.height().min(other.rect.height())
            && other.text.split_whitespace().count() == 1
            && stamp(trim(other.text.trim()))
    })
}

fn trim(w: &str) -> &str {
    w.trim_matches(|c: char| ",;:!?()[]{}<>«»\"'….".contains(c))
}

/// «Иван», «Smith», «Петрова-Водкина»: a capital, then lower-case letters.
fn capitalized(w: &str) -> bool {
    let mut chars = w.chars();
    chars.next().is_some_and(char::is_uppercase)
        && w.chars().count() >= 2
        && w.chars().all(|c| c.is_alphabetic() || c == '-')
        && w.chars().skip(1).any(char::is_lowercase)
}

/// «И.», «А.С.»: initials.
fn initial(raw: &str) -> bool {
    let parts: Vec<&str> = raw.trim_end_matches([',', ':']).split_inclusive('.').collect();
    !parts.is_empty()
        && parts.len() <= 3
        && parts.iter().all(|p| {
            let mut c = p.chars();
            c.next().is_some_and(char::is_uppercase) && c.next() == Some('.') && c.next().is_none()
        })
}

/// Base of a name in any case: «Ивану», «Ивана», «Иваном» → «иван».
fn stem(w: &str) -> String {
    let w = w.to_lowercase().replace('ё', "е");
    for end in ["ой", "ей", "ом", "ем", "ою", "ею", "у", "ю", "а", "я", "е", "ы", "и"] {
        if let Some(base) = w.strip_suffix(end)
            && base.chars().count() >= 3
        {
            return base.to_string();
        }
    }
    w
}

const NAMES: &[&str] = &[
    // Russian, full and everyday forms.
    "александр",
    "алексей",
    "андрей",
    "антон",
    "артем",
    "арсений",
    "аркадий",
    "богдан",
    "борис",
    "вадим",
    "валентин",
    "валерий",
    "василий",
    "виктор",
    "виталий",
    "владимир",
    "владислав",
    "всеволод",
    "вячеслав",
    "геннадий",
    "георгий",
    "глеб",
    "григорий",
    "давид",
    "даниил",
    "данил",
    "данила",
    "денис",
    "дмитрий",
    "евгений",
    "егор",
    "захар",
    "иван",
    "игорь",
    "илья",
    "кирилл",
    "константин",
    "леонид",
    "максим",
    "матвей",
    "мирон",
    "михаил",
    "никита",
    "николай",
    "олег",
    "павел",
    "петр",
    "роман",
    "руслан",
    "савелий",
    "семен",
    "сергей",
    "станислав",
    "степан",
    "тимофей",
    "тимур",
    "федор",
    "филипп",
    "эдуард",
    "юрий",
    "ярослав",
    "саша",
    "саня",
    "леша",
    "леха",
    "тема",
    "вова",
    "володя",
    "витя",
    "гоша",
    "гриша",
    "даня",
    "дима",
    "димон",
    "женя",
    "ваня",
    "костя",
    "лева",
    "макс",
    "миша",
    "коля",
    "паша",
    "петя",
    "рома",
    "сережа",
    "серега",
    "стас",
    "степа",
    "тима",
    "федя",
    "юра",
    "ярик",
    "эдик",
    "сема",
    "боря",
    "александра",
    "алина",
    "алиса",
    "алла",
    "анастасия",
    "ангелина",
    "анна",
    "антонина",
    "арина",
    "валентина",
    "валерия",
    "варвара",
    "василиса",
    "вера",
    "вероника",
    "виктория",
    "галина",
    "дарья",
    "диана",
    "ева",
    "евгения",
    "екатерина",
    "елена",
    "елизавета",
    "жанна",
    "злата",
    "зоя",
    "инна",
    "ирина",
    "карина",
    "кира",
    "кристина",
    "ксения",
    "лариса",
    "лидия",
    "любовь",
    "людмила",
    "маргарита",
    "марина",
    "мария",
    "милана",
    "надежда",
    "наталья",
    "наталия",
    "нина",
    "оксана",
    "олеся",
    "ольга",
    "полина",
    "светлана",
    "софия",
    "софья",
    "таисия",
    "тамара",
    "татьяна",
    "ульяна",
    "эвелина",
    "юлия",
    "яна",
    "аня",
    "настя",
    "катя",
    "лена",
    "лиза",
    "маша",
    "наташа",
    "оля",
    "света",
    "таня",
    "юля",
    "вика",
    "даша",
    "ира",
    "ксюша",
    "люба",
    "люда",
    "надя",
    "соня",
    "галя",
    "валя",
    "лера",
    // English.
    "john",
    "james",
    "michael",
    "david",
    "robert",
    "william",
    "richard",
    "thomas",
    "daniel",
    "matthew",
    "christopher",
    "chris",
    "paul",
    "peter",
    "george",
    "alex",
    "alexander",
    "andrew",
    "steven",
    "kevin",
    "brian",
    "jason",
    "ryan",
    "jacob",
    "ethan",
    "noah",
    "liam",
    "oliver",
    "lucas",
    "mike",
    "nick",
    "tom",
    "sam",
    "ben",
    "jack",
    "harry",
    "mary",
    "jennifer",
    "linda",
    "elizabeth",
    "sarah",
    "jessica",
    "emily",
    "emma",
    "olivia",
    "sophia",
    "anna",
    "kate",
    "laura",
    "lisa",
    "maria",
    "julia",
    "anne",
    "jane",
    "amy",
    "rachel",
    "hannah",
];

/// Affectionate suffixes without their case ending: «Саш-ечк-а», «Наст-юш-а», «Дим-асик», «Кат-юл-я».
const FOND: &[&str] = &[
    "ечк", "очк", "еньк", "оньк", "ушк", "юшк", "юш", "уш", "ул", "юл", "ус", "юс", "ечек", "очек", "асик", "ик",
    "чик", "ок", "ек", "к",
];

fn known_name(w: &str) -> bool {
    let s = stem(w);
    if NAMES.iter().any(|n| stem(n) == s) {
        return true;
    }
    // «Сашечка» = «саш» + «ечк» + «а»: a name's base, an affectionate suffix, a case ending.
    let lower = w.to_lowercase().replace('ё', "е");
    NAMES.iter().any(|n| {
        let base = n.trim_end_matches(['а', 'я', 'й', 'ь']);
        base.chars().count() >= 3
            && lower.strip_prefix(base).is_some_and(|rest| {
                FOND.iter().any(|f| rest.strip_prefix(f).is_some_and(|tail| tail.chars().count() <= 2))
            })
    })
}

/// Russian surnames and patronymics by their endings, in any case: «Петрову», «Сидоровой», «Ивановна».
fn surname(w: &str) -> bool {
    let w = w.to_lowercase().replace('ё', "е");
    w.chars().count() >= 5
        && [
            "ов",
            "ев",
            "ова",
            "ева",
            "ову",
            "еву",
            "овым",
            "евым",
            "овой",
            "евой",
            "ин",
            "ина",
            "ину",
            "иным",
            "иной",
            "ский",
            "ская",
            "ского",
            "ской",
            "скому",
            "цкий",
            "цкая",
            "цкого",
            "цкой",
            "енко",
            "вич",
            "вна",
            "вича",
            "вичу",
            "вны",
            "вне",
        ]
        .iter()
        .any(|end| w.ends_with(end))
}

/// «raptor_92», «Daniil2003», «DarkRaptor»: a handle rather than a word.
fn nick(w: &str) -> bool {
    let ascii = w.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    let letters = w.chars().any(|c| c.is_ascii_alphabetic());
    let mixed = w.chars().any(|c| c.is_ascii_digit()) || w.contains('_');
    let camel = w.chars().zip(w.chars().skip(1)).any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase());
    ascii && letters && w.len() >= 3 && (mixed || camel)
}

fn link(w: &str) -> bool {
    if w.contains("://") || w.starts_with("www.") {
        return true;
    }
    let lower = w.to_lowercase();
    let host = lower.split('/').next().unwrap_or("");
    let tld = host.rsplit('.').next().unwrap_or("");
    host.contains('.')
        && host.split('.').all(|p| !p.is_empty())
        && [
            "ru", "com", "net", "org", "me", "io", "рф", "su", "ua", "by", "kz", "tv", "gg", "dev", "app", "info",
            "pro",
        ]
        .contains(&tld)
}

/// «ул.», «д.», «кв.», «проспект», «street»: the next words are an address.
fn address_word(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    let short =
        ["ул.", "пр.", "просп.", "пер.", "наб.", "пл.", "мкр.", "д.", "кв.", "корп.", "стр.", "st.", "ave.", "apt."];
    let long = [
        "улица",
        "проспект",
        "переулок",
        "набережная",
        "бульвар",
        "шоссе",
        "площадь",
        "микрорайон",
        "квартира",
        "street",
        "avenue",
    ];
    short.contains(&lower.as_str()) || long.contains(&lower.trim_end_matches(',').trim_end_matches('.'))
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
    fn line_at(text: &str, top: i32) -> Line {
        let mut l = line(text);
        l.rect = Rect::new(l.rect.left, top, l.rect.right, top + 20);
        l.words.iter_mut().for_each(|(_, r)| *r = Rect::new(r.left, top, r.right, top + 20));
        l
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
    fn a_code_on_its_own_line_under_a_heading_is_hidden() {
        // The letter from the screenshot: heading, the code in a box, an instruction under it.
        let letter = [
            line_at("Иви <info@notify.ivi.ru>", 100),
            line_at("Вот твой код", 300),
            line_at("758062", 350),
            line_at("Введи эти цифры на экране входа или регистрации", 400),
        ];
        let found: Vec<Kind> = find(&letter).into_iter().map(|(k, _)| k).collect();
        assert_eq!(found, [Kind::Email, Kind::Code]);
        assert_eq!(kinds_of(&[line_at("Your verification code", 0), line_at("483 921", 40)]), [Kind::Code]);
        // A lone number with no code nearby, or a code word far away, stays sharp.
        assert!(kinds_of(&[line_at("Итого", 0), line_at("758062", 40)]).is_empty());
        assert!(kinds_of(&[line_at("Код страны", 0), line_at("2026", 2000)]).is_empty());
    }
    fn kinds_of(lines: &[Line]) -> Vec<Kind> {
        find(lines).into_iter().map(|(k, _)| k).collect()
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
    /// Strict findings as (kind, word) pairs, in reading order.
    fn strict(text: &str) -> Vec<(Kind, String)> {
        let l = line(text);
        let mut found: Vec<(Kind, String)> = find_strict(std::slice::from_ref(&l))
            .into_iter()
            .map(|(k, r)| {
                let words: Vec<&str> =
                    l.words.iter().filter(|(_, w)| r.intersect(w).is_some()).map(|(w, _)| w.as_str()).collect();
                (k, words.join(" "))
            })
            .collect();
        found.sort_by_key(|(_, w)| text.find(w.as_str()));
        found
    }
    #[test]
    fn strict_mode_hides_names_nicks_numbers_links_and_addresses() {
        use Kind::*;
        let s = |k: Kind, w: &str| (k, w.to_string());
        assert_eq!(strict("Иван Петров"), [s(Name, "Иван"), s(Name, "Петров")]);
        assert_eq!(strict("Вчера написал Даниилу, он ответил"), [s(Name, "Даниилу,")]);
        assert_eq!(strict("Привет, это Raptor_92 из чата"), [s(Nick, "Raptor_92")]);
        assert_eq!(strict("Пиши мне @danya_dev или в tg"), [s(Nick, "@danya_dev")]);
        assert_eq!(
            strict("Встреча 14 марта в 18:30, комната 305"),
            [s(Number, "14"), s(Number, "18:30,"), s(Number, "305")]
        );
        assert_eq!(
            strict("Сайт kadr.app и https://t.me/raptor"),
            [s(Link, "kadr.app"), s(Link, "https://t.me/raptor")]
        );
        assert_eq!(strict("Доставка: ул. Ленина, д. 5"), [s(Address, "Ленина,"), s(Address, "5")]);
        assert_eq!(strict("Подпись: Сидорова А.В."), [s(Name, "Сидорова"), s(Name, "А.В.")]);
        // The standard findings stay; ordinary words and a sentence's first capital stay visible.
        assert_eq!(strict("Почта a.b@mail.ru"), [s(Email, "a.b@mail.ru")]);
        assert!(strict("Отправить сообщение").is_empty());
        assert!(strict("Сегодня хорошая погода. Завтра тоже").is_empty());
    }
    /// Hidden words of a strict pass over several lines.
    fn hidden(lines: &[Line]) -> Vec<String> {
        let found = find_strict(lines);
        lines
            .iter()
            .flat_map(|l| l.words.iter())
            .filter(|(_, r)| found.iter().any(|(_, f)| mostly_inside(r, f)))
            .map(|(w, _)| w.clone())
            .collect()
    }
    /// A line at `left`, `top` with words `gap` px apart.
    fn placed(text: &str, left: i32, top: i32, gap: i32) -> Line {
        let mut x = left;
        let words: Vec<(String, Rect)> = text
            .split(' ')
            .map(|w| {
                let r = Rect::new(x, top, x + 10 * w.chars().count() as i32, top + 20);
                x = r.right + gap;
                (w.to_string(), r)
            })
            .collect();
        Line { text: text.into(), rect: Rect::new(left, top, x - gap, top + 20), words }
    }
    #[test]
    fn strict_mode_catches_what_a_telegram_window_shows() {
        // Chat list: the name on the left, the time read as a separate line on the right.
        let list = [
            placed("Сашечка", 70, 180, 6),
            placed("12:24", 280, 182, 6),
            placed("Думаю уже ладно завтра заберу", 70, 205, 6),
            placed("Danissimo", 70, 860, 6),
            placed("0:18", 280, 862, 6),
        ];
        let h = hidden(&list);
        assert!(h.contains(&"Сашечка".to_string()) && h.contains(&"Danissimo".to_string()), "{h:?}");
        assert!(!h.iter().any(|w| w == "Думаю" || w == "заберу"), "{h:?}");
        // Profile: first name and surname set tight together; the padded box of one must not shield the other.
        let h = hidden(&[placed("Тимофей Мохнаткин", 580, 175, 3)]);
        assert_eq!(h, ["Тимофей", "Мохнаткин"]);
        // A channel name: its first word goes with the rest.
        let h = hidden(&[placed("Just Lorem Studios", 500, 262, 6), placed("Wed", 770, 262, 6)]);
        assert_eq!(h, ["Just", "Lorem", "Studios"]);
        // Affectionate forms in any case.
        for w in ["Сашечка", "Настюше", "Димасик", "Катюлей", "Ванечку", "Сашка"]
        {
            assert!(known_name(w), "{w}");
        }
        for w in ["Максимум", "Романтика", "Привет", "Сегодня", "Отправить"] {
            assert!(!known_name(w), "{w}");
        }
    }
    #[test]
    fn full_mode_covers_every_line_of_text() {
        let lines = [placed("Привет, как дела?", 10, 10, 6), placed("Всё отлично", 10, 40, 6), placed(" ", 10, 70, 6)];
        let found = find_all(&lines);
        assert_eq!(found.len(), 2);
        for (line, (kind, r)) in lines.iter().zip(&found) {
            assert_eq!(*kind, Kind::Text);
            assert!(line.words.iter().all(|(_, w)| mostly_inside(w, r)));
        }
    }
}
