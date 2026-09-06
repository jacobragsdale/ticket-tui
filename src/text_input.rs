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

    /// Inserts pasted text at the caret keeping its line breaks, for a field
    /// that holds a document rather than a line: `\r\n` becomes `\n`, a tab
    /// becomes two spaces, and other control characters are dropped.
    pub fn paste_block(&mut self, pasted: &str) {
        let mut sanitized = String::with_capacity(pasted.len());
        for character in pasted.replace("\r\n", "\n").chars() {
            match character {
                '\n' => sanitized.push('\n'),
                '\t' => sanitized.push_str("  "),
                character if character.is_control() => {}
                character => sanitized.push(character),
            }
        }
        self.insert_str(&sanitized);
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Moves the caret up one wrapped row at `width` columns, keeping its
    /// column where the row above is long enough.
    pub fn move_up(&mut self, width: usize) {
        self.move_rows(-1, width);
    }

    /// Moves the caret down one wrapped row, the way [`Self::move_up`] moves
    /// it up.
    pub fn move_down(&mut self, width: usize) {
        self.move_rows(1, width);
    }

    fn move_rows(&mut self, delta: isize, width: usize) {
        let layout = wrap_with_cursor(&self.text, self.cursor, width);
        let (row, column) = layout.cursor;
        let Some(target) = row
            .checked_add_signed(delta)
            .filter(|target| *target < layout.rows.len())
        else {
            return;
        };
        let (start, text) = &layout.rows[target];
        self.cursor = start + column.min(text.chars().count());
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

/// A multi-line text soft-wrapped to a width: each row's first character
/// index and its text, and the row and column the caret lands on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WrapLayout {
    pub rows: Vec<(usize, String)>,
    /// The caret as `(row, column)` in the rows above.
    pub cursor: (usize, usize),
}

/// Wraps `text` to `width` columns and finds the caret at character `cursor`.
/// A row ends at a newline, at a space that would not fit, or after the last
/// space before the width runs out; a word longer than the width is cut. There
/// is always at least one row, so the caret always has somewhere to be, and a
/// text ending in a newline ends in an empty row for the same reason.
// ponytail: a character is one cell, as the rest of the pane measures; add
// unicode-width if CJK or emoji misplace the caret.
#[must_use]
pub fn wrap_with_cursor(text: &str, cursor: usize, width: usize) -> WrapLayout {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut start = 0;
    loop {
        let mut end = start;
        let mut last_space = None;
        while end < chars.len() && chars[end] != '\n' && end - start < width {
            if chars[end] == ' ' {
                last_space = Some(end);
            }
            end += 1;
        }
        let (row_end, next) = if end == chars.len() || matches!(chars[end], '\n' | ' ') {
            // The text ends the row, or a newline does, or a space that
            // would not fit and is dropped with the break.
            (end, end + 1)
        } else if let Some(space) = last_space.filter(|space| *space > start) {
            (space, space + 1)
        } else {
            (end, end)
        };
        rows.push((start, chars[start..row_end].iter().collect()));
        if end == chars.len() {
            break;
        }
        start = next;
    }
    let cursor = cursor.min(chars.len());
    let row = rows
        .iter()
        .rposition(|(start, _)| *start <= cursor)
        .unwrap_or(0);
    WrapLayout {
        cursor: (row, cursor - rows[row].0),
        rows,
    }
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

    fn rows(layout: &WrapLayout) -> Vec<&str> {
        layout.rows.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn wrapping_breaks_at_spaces_newlines_and_the_width_and_places_the_caret() {
        let layout = wrap_with_cursor("alpha beta\ngamma", 6, 7);
        assert_eq!(rows(&layout), ["alpha", "beta", "gamma"]);
        assert_eq!(
            layout.cursor,
            (1, 0),
            "the caret after the dropped space starts the next row"
        );
        assert_eq!(
            wrap_with_cursor("alpha beta", 5, 7).cursor,
            (0, 5),
            "the caret on the dropped space ends its row"
        );
        assert_eq!(
            rows(&wrap_with_cursor("abcdefgh", 0, 3)),
            ["abc", "def", "gh"],
            "a word wider than the row is cut"
        );
        assert_eq!(wrap_with_cursor("abcdefgh", 3, 3).cursor, (1, 0));
        assert_eq!(
            rows(&wrap_with_cursor("abc def", 0, 3)),
            ["abc", "def"],
            "a space on the boundary is dropped with the break"
        );
        assert_eq!(rows(&wrap_with_cursor("", 0, 10)), [""]);
        let trailing = wrap_with_cursor("ab\n", 3, 10);
        assert_eq!(rows(&trailing), ["ab", ""]);
        assert_eq!(trailing.cursor, (1, 0), "a trailing newline opens a row");
    }

    #[test]
    fn vertical_moves_keep_the_column_and_stop_at_the_edges() {
        // Wrapped at 6: alpha / beta / gamma / delta.
        let mut input = TextInput::new("alpha beta\ngamma delta");
        input.move_up(6);
        assert_eq!(input.cursor(), 16, "the same column on the row above");
        input.move_up(6);
        assert_eq!(input.cursor(), 10, "clamped to a shorter row");
        input.move_up(6);
        assert_eq!(input.cursor(), 4);
        input.move_up(6);
        assert_eq!(input.cursor(), 4, "the first row is as far up as it goes");
        input.move_down(6);
        assert_eq!(input.cursor(), 10);
        input.move_down(6);
        assert_eq!(input.cursor(), 15);
        input.move_down(6);
        assert_eq!(input.cursor(), 21);
        input.move_down(6);
        assert_eq!(input.cursor(), 21, "the last row is as far down as it goes");

        input.insert_newline();
        assert_eq!(input.text(), "alpha beta\ngamma delt\na");
    }

    #[test]
    fn a_block_paste_keeps_its_line_breaks() {
        let mut input = TextInput::new("");
        input.paste_block("one\r\ntwo\tthree\u{7}");
        assert_eq!(input.text(), "one\ntwo  three");
        assert_eq!(input.cursor(), 14);
    }
}
