//! The message input: multi-line editing, wrapped by grapheme so the cursor position is exact.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One wrapped row of the input.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

/// Text and cursor. Survives every resize, since nothing here depends on the terminal's size.
#[derive(Debug, Default, Clone)]
pub struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The cursor's byte offset into the text.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the whole text, leaving the cursor at the end (for history).
    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
    }

    /// Replaces the bytes `start..self.cursor` with `text`, leaving the cursor after it (for completion).
    pub fn replace_before_cursor(&mut self, start: usize, text: &str) {
        let start = start.min(self.cursor);
        self.text.replace_range(start..self.cursor, text);
        self.cursor = start + text.len();
    }

    /// Moves the cursor to the start of the previous word.
    pub fn word_left(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        self.cursor = trimmed.rfind(char::is_whitespace).map_or(0, |index| index + 1);
    }

    /// Moves the cursor past the end of the next word.
    pub fn word_right(&mut self) {
        let after = &self.text[self.cursor..];
        let skipped = after.len() - after.trim_start().len();
        let rest = &after[skipped..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.cursor += skipped + word;
    }

    /// Deletes from the cursor to the end of the line.
    pub fn delete_to_line_end(&mut self) {
        let end = self.text[self.cursor..].find('\n').map_or(self.text.len(), |index| self.cursor + index);
        self.text.replace_range(self.cursor..end, "");
    }

    /// Empties the input and returns what it held.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        let clean: String = text.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor].grapheme_indices(true).next_back().map_or(0, |(index, _)| index)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..].graphemes(true).next().map_or(self.cursor, |g| self.cursor + g.len())
    }

    pub fn backspace(&mut self) {
        let start = self.previous();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete(&mut self) {
        let end = self.next();
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.previous();
    }

    pub fn right(&mut self) {
        self.cursor = self.next();
    }

    pub fn line_start(&mut self) {
        self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |index| index + 1);
    }

    pub fn line_end(&mut self) {
        self.cursor = self.text[self.cursor..].find('\n').map_or(self.text.len(), |index| self.cursor + index);
    }

    /// Deletes back to the start of the current line.
    pub fn delete_to_line_start(&mut self) {
        let end = self.cursor;
        self.line_start();
        self.text.replace_range(self.cursor..end, "");
    }

    /// Deletes the word before the cursor.
    pub fn delete_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed.rfind(char::is_whitespace).map_or(0, |index| index + 1);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Deletes back to the start of the word before the cursor, as Ctrl+Backspace does in editors:
    /// spaces first, then a run of letters and digits, or a run of punctuation.
    pub fn delete_word_back(&mut self) {
        let before = &self.text[..self.cursor];
        let mut start = before.trim_end().len();
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let kind = before[..start].chars().next_back().map(word);
        for (index, c) in before[..start].char_indices().rev() {
            if c.is_whitespace() || Some(word(c)) != kind {
                break;
            }
            start = index;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Moves by visual rows. Returns false if there is no row in that direction.
    pub fn move_vertical(&mut self, width: usize, down: bool) -> bool {
        let rows = self.rows(width);
        let (row, column) = self.cursor_at(&rows);
        let target = if down { row + 1 } else { row.wrapping_sub(1) };
        let Some(target_row) = rows.get(target) else { return false };
        let mut used = 0;
        let mut position = target_row.start;
        for (index, grapheme) in self.text[target_row.start..target_row.end].grapheme_indices(true) {
            if used + grapheme.width() > column {
                break;
            }
            used += grapheme.width();
            position = target_row.start + index + grapheme.len();
        }
        self.cursor = position;
        true
    }

    /// Splits the text into rows no wider than `width`.
    #[must_use]
    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut start = 0;
        let mut used = 0;
        for (index, grapheme) in self.text.grapheme_indices(true) {
            if grapheme == "\n" {
                rows.push(Row { start, end: index });
                start = index + 1;
                used = 0;
                continue;
            }
            let cell = grapheme.width();
            if used + cell > width && used > 0 {
                rows.push(Row { start, end: index });
                start = index;
                used = 0;
            }
            used += cell;
        }
        rows.push(Row { start, end: self.text.len() });
        rows
    }

    /// Finds the cursor's row and column within the given rows.
    #[must_use]
    pub fn cursor_at(&self, rows: &[Row]) -> (usize, usize) {
        // The last row containing the cursor wins, so the cursor after a wrap sits on the new row.
        let row = rows.iter().rposition(|row| row.start <= self.cursor).unwrap_or(0);
        let column = self.text[rows[row].start..self.cursor.min(rows[row].end)].width();
        (row, column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_backspace_takes_one_word_at_a_time() {
        let mut input = Input::default();
        input.insert("cargo test --package re-code  ");
        input.delete_word_back();
        assert_eq!(input.text(), "cargo test --package re-");
        input.delete_word_back();
        assert_eq!(input.text(), "cargo test --package re");
        input.delete_word_back();
        assert_eq!(input.text(), "cargo test --package ");
        input.delete_word_back();
        assert_eq!(input.text(), "cargo test --");
        input.delete_word_back();
        input.delete_word_back();
        assert_eq!(input.text(), "cargo ");
        input.delete_word_back();
        input.delete_word_back();
        assert_eq!(input.text(), "");
        input.insert("héllo wörld");
        input.delete_word_back();
        assert_eq!(input.text(), "héllo ");
    }

    fn with(text: &str) -> Input {
        let mut input = Input::default();
        input.insert(text);
        input
    }

    fn row_text(input: &Input, width: usize) -> Vec<String> {
        input.rows(width).iter().map(|row| input.text()[row.start..row.end].to_owned()).collect()
    }

    #[test]
    fn editing_respects_graphemes() {
        let mut input = with("ae\u{301}世🦀");
        input.backspace();
        assert_eq!(input.text(), "ae\u{301}世");
        input.left();
        input.left();
        input.delete();
        assert_eq!(input.text(), "a世");
        input.insert("b");
        assert_eq!(input.text(), "ab世");
    }

    #[test]
    fn rows_wrap_and_respect_newlines() {
        assert_eq!(row_text(&with("abcdef\ngh"), 4), ["abcd", "ef", "gh"]);
        assert_eq!(row_text(&with("世界世"), 4), ["世界", "世"]);
        assert_eq!(row_text(&with(""), 4), [""]);
        assert_eq!(row_text(&with("a\n"), 4), ["a", ""]);
    }

    #[test]
    fn cursor_position_follows_wrapping() {
        let input = with("abcdef");
        assert_eq!(input.cursor_at(&input.rows(4)), (1, 2));
        assert_eq!(input.cursor_at(&input.rows(10)), (0, 6));
        let input = with("世界");
        assert_eq!(input.cursor_at(&input.rows(10)), (0, 4));
        let input = with("a\n");
        assert_eq!(input.cursor_at(&input.rows(10)), (1, 0));
    }

    #[test]
    fn vertical_movement_keeps_the_column() {
        let mut input = with("abcd\nef\nghij");
        assert!(input.move_vertical(10, false));
        assert_eq!(input.cursor_at(&input.rows(10)), (1, 2));
        assert!(input.move_vertical(10, false));
        assert_eq!(input.cursor_at(&input.rows(10)), (0, 2));
        assert!(!input.move_vertical(10, false));
        assert!(input.move_vertical(10, true));
        assert!(input.move_vertical(10, true));
        assert_eq!(input.cursor_at(&input.rows(10)), (2, 2));
        assert!(!input.move_vertical(10, true));
    }

    #[test]
    fn deleting_words_and_lines() {
        let mut input = with("one two  ");
        input.delete_word();
        assert_eq!(input.text(), "one ");
        let mut input = with("keep\nthis goes");
        input.delete_to_line_start();
        assert_eq!(input.text(), "keep\n");
    }

    #[test]
    fn pasted_control_characters_are_removed() {
        assert_eq!(with("a\x1b[31mb\r\nc\td").text(), "a[31mb\nc    d");
    }

    #[test]
    fn take_empties_the_input() {
        let mut input = with("sent");
        assert_eq!(input.take(), "sent");
        input.insert("next");
        assert_eq!(input.text(), "next");
    }
}
