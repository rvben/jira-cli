//! Small terminal text editor shared by the workbench prompts.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug)]
pub(super) struct Editor {
    text: String,
    cursor: usize,
    multiline: bool,
}

impl Editor {
    pub(super) fn new(text: String, multiline: bool) -> Self {
        let cursor = text.len();
        Self {
            text,
            cursor,
            multiline,
        }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }
    pub(super) fn into_text(self) -> String {
        self.text
    }
    pub(super) fn is_multiline(&self) -> bool {
        self.multiline
    }
    pub(super) fn cursor_line(&self) -> usize {
        self.text[..self.cursor]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
    }
    pub(super) fn cursor_column(&self) -> usize {
        self.text[self.line_start()..self.cursor].chars().count()
    }

    pub(super) fn display(&self) -> String {
        let mut shown = self.text.clone();
        shown.insert(self.cursor, '▏');
        shown
    }

    pub(super) fn paste(&mut self, value: &str) {
        let normalized = value.replace("\r\n", "\n");
        let limit: usize = if self.multiline { 10_000 } else { 500 };
        let room = limit.saturating_sub(self.text.chars().count());
        let mut insert = String::new();
        let mut inserted = 0;
        for character in normalized.chars() {
            if inserted >= room {
                break;
            }
            match character {
                '\n' if self.multiline => {
                    insert.push('\n');
                    inserted += 1;
                }
                '\r' | '\n' | '\t' => {
                    insert.push(' ');
                    inserted += 1;
                }
                c if !c.is_control() => {
                    insert.push(c);
                    inserted += 1;
                }
                _ => {}
            }
        }
        self.text.insert_str(self.cursor, &insert);
        self.cursor += insert.len();
    }

    pub(super) fn key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
            (KeyCode::Char('u'), true) => {
                self.text.clear();
                self.cursor = 0;
            }
            (KeyCode::Char('a'), true) | (KeyCode::Home, _) => self.cursor = self.line_start(),
            (KeyCode::Char('e'), true) | (KeyCode::End, _) => self.cursor = self.line_end(),
            (KeyCode::Left, _) => self.cursor = self.previous(),
            (KeyCode::Right, _) => self.cursor = self.next(),
            (KeyCode::Up, _) if self.multiline => self.move_up(),
            (KeyCode::Down, _) if self.multiline => self.move_down(),
            (KeyCode::Backspace, _) => {
                let prev = self.previous();
                if prev < self.cursor {
                    self.text.drain(prev..self.cursor);
                    self.cursor = prev;
                }
            }
            (KeyCode::Delete, _) => {
                let next = self.next();
                if next > self.cursor {
                    self.text.drain(self.cursor..next);
                }
            }
            (KeyCode::Enter, false) if self.multiline => self.paste("\n"),
            (KeyCode::Char(c), false) => self.paste(&c.to_string()),
            _ => {}
        }
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.text.len())
    }
    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }
    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len())
    }
    fn move_up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            return;
        }
        let column = self.cursor_column();
        let previous_end = start - 1;
        let previous_start = self.text[..previous_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.cursor = self.column_offset(previous_start, previous_end, column);
    }
    fn move_down(&mut self) {
        let end = self.line_end();
        if end == self.text.len() {
            return;
        }
        let column = self.cursor_column();
        let next_start = end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map(|i| next_start + i)
            .unwrap_or(self.text.len());
        self.cursor = self.column_offset(next_start, next_end, column);
    }
    fn column_offset(&self, start: usize, end: usize, column: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(column)
            .map(|(offset, _)| start + offset)
            .unwrap_or(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_navigation_and_paste_are_safe() {
        let mut e = Editor::new("A🧩B".into(), true);
        e.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        e.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(e.text(), "AB");
        e.paste("\nnext\r\u{1b}");
        assert_eq!(e.text(), "A\nnext B");
        assert!(e.display().contains('▏'));
    }
    #[test]
    fn single_line_paste_stays_on_one_line() {
        let mut e = Editor::new(String::new(), false);
        e.paste("project = APP\nORDER BY updated");
        assert_eq!(e.text(), "project = APP ORDER BY updated");
    }
    #[test]
    fn multiline_arrows_keep_character_column() {
        let mut e = Editor::new("abc\n🧩z\nlast".into(), true);
        e.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        e.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        e.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(e.cursor_line(), 1);
        assert_eq!(e.cursor_column(), 1);
        e.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(e.cursor_line(), 0);
        assert_eq!(e.cursor_column(), 1);
        e.paste("\r\nmore");
        assert!(e.text().contains("\nmore"));
    }
}
