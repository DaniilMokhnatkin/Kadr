//! Позиции — границы UTF-8; в DirectWrite они переводятся в UTF-16 при отрисовке.
#[derive(Default)]
pub struct TextInput {
    pub cursor: usize,
    pub anchor: usize,
}
impl TextInput {
    pub fn at_end(value: &str) -> Self {
        Self {
            cursor: value.len(),
            anchor: value.len(),
        }
    }
    pub fn select_all(&mut self, value: &str) {
        self.anchor = 0;
        self.cursor = value.len();
    }
    pub fn range(&self) -> std::ops::Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }
    pub fn insert(&mut self, value: &mut String, insert: &str) {
        let range = self.range();
        let mut next = value.clone();
        next.replace_range(range.clone(), insert);
        if next.len() > 4096 || next.lines().count() > 64 {
            return;
        }
        *value = next;
        self.cursor = range.start + insert.len();
        self.anchor = self.cursor;
    }
    pub fn erase(&mut self, value: &mut String, back: bool) {
        if self.cursor == self.anchor {
            self.anchor = if back {
                previous(value, self.cursor)
            } else {
                next(value, self.cursor)
            };
        }
        self.insert(value, "");
    }
    pub fn navigate(&mut self, value: &str, key: u16, shift: bool) {
        self.cursor = match key {
            0x25 if !shift && self.cursor != self.anchor => self.range().start,
            0x27 if !shift && self.cursor != self.anchor => self.range().end,
            0x25 => previous(value, self.cursor),
            0x27 => next(value, self.cursor),
            0x24 => value[..self.cursor].rfind('\n').map_or(0, |i| i + 1),
            0x23 => value[self.cursor..].find('\n').map_or(value.len(), |i| i + self.cursor),
            0x26 => 0,
            0x28 => value.len(),
            _ => self.cursor,
        };
        if !shift {
            self.anchor = self.cursor;
        }
    }
}
fn previous(value: &str, at: usize) -> usize {
    value[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}
fn next(value: &str, at: usize) -> usize {
    value[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_selection_insertion_and_delete_keep_character_boundaries() {
        let mut text = "Привет 🌍".to_owned();
        let mut edit = TextInput::at_end(&text);
        edit.navigate(&text, 0x25, true);
        edit.insert(&mut text, "мир");
        assert_eq!(text, "Привет мир");
        edit.erase(&mut text, true);
        assert_eq!(text, "Привет ми");
        edit.select_all(&text);
        edit.insert(&mut text, "A🌍B");
        edit.navigate(&text, 0x24, false);
        edit.navigate(&text, 0x27, false);
        edit.erase(&mut text, false);
        assert_eq!(text, "AB");
    }
    #[test]
    fn rejected_paste_preserves_selection() {
        let mut text = "Привет".to_owned();
        let mut edit = TextInput::at_end(&text);
        edit.select_all(&text);
        edit.insert(&mut text, &"x".repeat(4097));
        assert_eq!(text, "Привет");
        assert_eq!(edit.range(), 0..text.len());
    }
}
