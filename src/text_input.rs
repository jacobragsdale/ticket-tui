use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single-line text field: the text plus a caret measured in characters, with the
/// editing behaviour shared by the search box, the command palette, and the
/// view-name field.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TextInput {
    text: String,
    cursor: usize,
}

impl TextInput {
    /// Creates a field holding `text` with the caret at the end.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the text and moves the caret to the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        *self = Self::new(text);
    }

    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.character_count());
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn insert_char(&mut self, character: char) {
        let byte = byte_index(&self.text, self.cursor);
        self.text.insert(byte, character);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let byte = byte_index(&self.text, self.cursor);
        self.text.insert_str(byte, text);
        self.cursor += text.chars().count();
    }

    /// Deletes the character before the caret, reporting whether it removed one.
    pub fn backspace(&mut self) -> bool {
        let Some(index) = self.cursor.checked_sub(1) else {
            return false;
        };
        self.remove_range(index, index + 1);
        self.cursor = index;
        true
    }

    /// Deletes the character under the caret, reporting whether it removed one.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.character_count() {
            return false;
        }
        self.remove_range(self.cursor, self.cursor + 1);
        true
    }

    /// Deletes the whitespace before the caret and the word before that,
    /// reporting whether it removed anything.
    pub fn delete_word(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let characters: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && characters[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !characters[start - 1].is_whitespace() {
            start -= 1;
        }
        self.remove_range(start, self.cursor);
        self.cursor = start;
        true
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.cursor.saturating_add(1).min(self.character_count());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.character_count();
    }

    /// Inserts pasted text at the caret. Fields that hold one logical line of query
    /// text fold newlines and tabs into spaces; the rest simply drop control
    /// characters.
    pub fn paste(&mut self, pasted: &str, multiline_to_spaces: bool) {
        let sanitized = if multiline_to_spaces {
            sanitize_multiline(pasted)
        } else {
            sanitize_single_line(pasted)
        };
        self.insert_str(&sanitized);
    }

    /// Applies one editing key, reporting whether the field consumed it. Callers
    /// keep the keys that mean something beyond editing (submit, cancel, history,
    /// list navigation) for themselves.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),
            KeyCode::Backspace => {
                self.backspace();
            }
            KeyCode::Delete => {
                self.delete();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_word();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.clear(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_char(character);
            }
            _ => return false,
        }
        true
    }

    fn character_count(&self) -> usize {
        self.text.chars().count()
    }

    fn remove_range(&mut self, start: usize, end: usize) {
        let start_byte = byte_index(&self.text, start);
        let end_byte = byte_index(&self.text, end);
        self.text.replace_range(start_byte..end_byte, "");
    }
}

fn byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(index, _)| index)
}

fn sanitize_multiline(pasted: &str) -> String {
    pasted
        .chars()
        .filter_map(|character| match character {
            '\r' | '\n' | '\t' => Some(' '),
            character if character.is_control() => None,
            character => Some(character),
        })
        .collect()
}

fn sanitize_single_line(pasted: &str) -> String {
    pasted
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn control(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn editing_keys_insert_and_delete_around_a_unicode_caret() {
        let mut input = TextInput::new("café");
        assert_eq!(input.cursor(), 4);

        input.handle_key(key(KeyCode::Left));
        input.handle_key(key(KeyCode::Char('x')));
        assert_eq!(input.text(), "cafxé");
        assert_eq!(input.cursor(), 4);

        assert!(input.handle_key(key(KeyCode::Backspace)));
        assert_eq!(input.text(), "café");
        assert_eq!(input.cursor(), 3);

        assert!(input.handle_key(key(KeyCode::Delete)));
        assert_eq!(input.text(), "caf");
        assert_eq!(input.cursor(), 3);

        input.handle_key(key(KeyCode::Home));
        assert_eq!(input.cursor(), 0);
        assert!(!input.backspace(), "nothing to delete at the start");
        assert!(!input.delete_word());
        input.handle_key(key(KeyCode::End));
        assert!(!input.delete(), "nothing to delete at the end");
        assert_eq!(input.text(), "caf");
    }

    #[test]
    fn word_deletion_takes_trailing_space_and_the_word_before_it() {
        let mut input = TextInput::new("alpha café");
        assert!(input.handle_key(control(KeyCode::Char('w'))));
        assert_eq!(input.text(), "alpha ");
        assert_eq!(input.cursor(), 6);

        assert!(input.handle_key(control(KeyCode::Char('w'))));
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);

        let mut clearing = TextInput::new("alpha beta");
        clearing.set_cursor(5);
        assert!(clearing.handle_key(control(KeyCode::Char('u'))));
        assert!(clearing.is_empty());
        assert_eq!(clearing.cursor(), 0);
    }

    #[test]
    fn paste_folds_or_strips_control_characters() {
        let mut query = TextInput::new("alpha ");
        query.paste("tea\nshop\u{7}", true);
        assert_eq!(query.text(), "alpha tea shop");
        assert_eq!(query.cursor(), 14);

        let mut name = TextInput::new("alpha");
        name.paste(" beta\u{7}", false);
        assert_eq!(name.text(), "alpha beta");
        assert_eq!(name.cursor(), 10);

        let mut middle = TextInput::new("ab");
        middle.set_cursor(1);
        middle.paste("\u{7}", true);
        assert_eq!(middle.text(), "ab", "an all-control paste inserts nothing");
        assert_eq!(middle.cursor(), 1);
    }

    #[test]
    fn cursor_is_clamped_and_non_editing_keys_are_left_alone() {
        let mut input = TextInput::new("abc");
        input.set_cursor(99);
        assert_eq!(input.cursor(), 3);
        input.move_right();
        assert_eq!(input.cursor(), 3);

        input.set_text("é");
        assert_eq!(input.cursor(), 1);
        input.set_cursor(0);
        input.move_left();
        assert_eq!(input.cursor(), 0);

        assert!(!input.handle_key(key(KeyCode::Enter)));
        assert!(!input.handle_key(key(KeyCode::Up)));
        assert!(!input.handle_key(control(KeyCode::Char('p'))));
        assert!(
            !input.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
            "alt chords belong to the caller"
        );
        assert_eq!(input.text(), "é");
    }
}
