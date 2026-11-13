use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

/// A single-line text field with a character-indexed cursor.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    pub value: String,
    /// Cursor position as a character index (not a byte offset).
    pub cursor: usize,
    pub masked: bool,
}

/// The part of an input that fits into a given width, plus the cursor column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleInput {
    pub text: String,
    pub cursor_column: u16,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            masked: false,
        }
    }

    pub fn masked(value: impl Into<String>) -> Self {
        Self {
            masked: true,
            ..Self::new(value)
        }
    }

    pub fn char_len(&self) -> usize {
        self.value.chars().count()
    }

    /// Replace a character range and place the cursor after the new text.
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        let len = self.char_len();
        let start = start.min(len);
        let end = end.min(len).max(start);
        let from = self.byte_offset(start);
        let to = self.byte_offset(end);
        self.value.replace_range(from..to, text);
        self.cursor = start + text.chars().count();
    }

    fn byte_offset(&self, char_index: usize) -> usize {
        if char_index == 0 {
            return 0;
        }
        self.value
            .char_indices()
            .nth(char_index)
            .map(|(offset, _)| offset)
            .unwrap_or(self.value.len())
    }

    pub fn insert_char(&mut self, ch: char) {
        let offset = self.byte_offset(self.cursor);
        self.value.insert(offset, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        let cleaned: String = text
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        let offset = self.byte_offset(self.cursor);
        self.value.insert_str(offset, &cleaned);
        self.cursor += cleaned.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_offset(self.cursor - 1);
        let end = self.byte_offset(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.char_len() {
            return;
        }
        let start = self.byte_offset(self.cursor);
        let end = self.byte_offset(self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    /// Delete the whitespace and word immediately before the cursor.
    pub fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let chars: Vec<char> = self.value.chars().collect();
        let mut start = self.cursor.min(chars.len());
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let from = self.byte_offset(start);
        let to = self.byte_offset(self.cursor);
        self.value.replace_range(from..to, "");
        self.cursor = start;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.char_len();
    }

    /// Apply the editing portion of a key event. Returns true when consumed.
    pub fn handle_edit_key(&mut self, key: &KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('a') => self.move_home(),
                KeyCode::Char('e') => self.move_end(),
                KeyCode::Char('w') => self.delete_word_backward(),
                _ => return false,
            }
            return true;
        }

        match key.code {
            KeyCode::Char(ch) => {
                self.insert_char(ch);
                true
            }
            KeyCode::Backspace => {
                self.backspace();
                true
            }
            KeyCode::Delete => {
                self.delete();
                true
            }
            KeyCode::Left => {
                self.move_left();
                true
            }
            KeyCode::Right => {
                self.move_right();
                true
            }
            KeyCode::Home => {
                self.move_home();
                true
            }
            KeyCode::End => {
                self.move_end();
                true
            }
            _ => false,
        }
    }

    /// Build the visible slice for a field `width` columns wide.
    ///
    /// Long values scroll horizontally so the cursor remains visible. Masked
    /// values use bullets and one column per character.
    pub fn visible(&self, width: usize) -> VisibleInput {
        if width == 0 {
            return VisibleInput {
                text: String::new(),
                cursor_column: 0,
            };
        }

        let chars: Vec<char> = self.value.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let char_width = |ch: char| {
            if self.masked {
                1
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(0)
            }
        };

        // Move the start to the right until the text before the cursor fits.
        let mut start = 0;
        while start < cursor {
            let before: usize =
                chars[start..cursor].iter().map(|&ch| char_width(ch)).sum();
            if before < width {
                break;
            }
            start += 1;
        }

        let mut text = String::new();
        let mut used = 0;
        for &ch in &chars[start..] {
            let w = char_width(ch);
            if used + w > width {
                break;
            }
            if self.masked {
                text.push('•');
            } else {
                text.push(ch);
            }
            used += w;
        }

        let cursor_column: usize =
            chars[start..cursor].iter().map(|&ch| char_width(ch)).sum();
        VisibleInput {
            text,
            cursor_column: cursor_column.min(width.saturating_sub(1)) as u16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_unicode_text() {
        let mut input = TextInput::new("héllo");
        assert_eq!(input.cursor, 5);
        input.move_left();
        input.insert_char('!');
        assert_eq!(input.value, "héll!o");
        input.backspace();
        assert_eq!(input.value, "héllo");
        input.move_home();
        input.delete();
        assert_eq!(input.value, "éllo");
        input.move_end();
        input.insert_str(" wörld\n");
        assert_eq!(input.value, "éllo wörld ");
    }

    #[test]
    fn scrolls_to_keep_cursor_visible() {
        let mut input = TextInput::new("abcdefghij");
        let visible = input.visible(5);
        assert!(visible.text.ends_with("ghij"));
        assert!(visible.cursor_column <= 4);

        input.move_left();
        input.move_left();
        let visible = input.visible(5);
        assert!(visible.text.contains('h'));
    }

    #[test]
    fn control_keys_move_cursor_and_delete_words() {
        let mut input = TextInput::new("hello world");

        assert!(input.handle_edit_key(&KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL
        )));
        assert_eq!(input.cursor, 0);

        assert!(input.handle_edit_key(&KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL
        )));
        assert_eq!(input.cursor, 11);

        assert!(input.handle_edit_key(&KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL
        )));
        assert_eq!(input.value, "hello ");

        assert!(!input.handle_edit_key(&KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
        assert_eq!(input.value, "hello ");
    }

    #[test]
    fn deletes_words_backward() {
        let mut input = TextInput::new("rust tui bookmarks");
        input.delete_word_backward();
        assert_eq!(input.value, "rust tui ");
        input.delete_word_backward();
        assert_eq!(input.value, "rust ");
        input.delete_word_backward();
        assert_eq!(input.value, "");

        let mut input = TextInput::new("hello world");
        input.move_left();
        input.move_left();
        input.move_left();
        input.delete_word_backward();
        assert_eq!(input.value, "hello rld");
    }

    #[test]
    fn masks_values() {
        let input = TextInput::masked("secret");
        let visible = input.visible(10);
        assert_eq!(visible.text, "••••••");
        assert_eq!(visible.cursor_column, 6);
    }
}
