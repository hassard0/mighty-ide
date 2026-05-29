//! The authoritative editable text model (shim-side, per tab).
//!
//! ## Why this lives in the shim (L28 workaround)
//!
//! Under v0.36 native `mty build`, a Mighty `Vec[I32]` grown with the
//! `v = v.push(x)` capture-rebind idiom comes back EMPTY (confirmed codegen bug,
//! `docs/mighty-language-lessons.md` L28). So the IDE's old Mighty-side edit
//! buffer never accumulated, and live editing didn't actually work under the
//! native binary. To make editing genuinely live, the **authoritative text
//! model now lives here in the shim** — a `Vec<String>` of lines plus a cursor,
//! selection, scroll offset, and dirty flag. The Mighty side keeps owning the
//! event loop, key routing, command dispatch, find/diagnostics/tabs/etc.; it
//! just delegates buffer *storage + edits* to this model via scalar `mui_ed_*`
//! ops so edits reflect live on screen. Move this back to Mighty once the
//! codegen bug is fixed.
//!
//! The model is pure + GPU-free so it is exhaustively unit-testable. Columns are
//! **char** offsets within a line (not bytes); newlines are never stored inside
//! a line string (they are the boundaries between elements of `lines`).

// A few model helpers (selection mutation, `new`) are part of the complete
// TextModel API but not yet driven from the scalar ABI; keep them documented
// and tested without a dead-code warning.
#![allow(dead_code)]

/// Cursor / clamp movement directions (mirror the `mui_ed_move(dir)` contract).
pub const DIR_LEFT: i32 = 0;
pub const DIR_RIGHT: i32 = 1;
pub const DIR_UP: i32 = 2;
pub const DIR_DOWN: i32 = 3;
pub const DIR_HOME: i32 = 4;
pub const DIR_END: i32 = 5;

/// An editable document: lines of text + a cursor + an optional selection
/// anchor + the top visible line (scroll). Always holds at least one line.
#[derive(Debug, Clone)]
pub struct TextModel {
    /// The document lines (no embedded newlines). Always non-empty.
    lines: Vec<String>,
    /// 0-based cursor line.
    cur_line: usize,
    /// 0-based cursor column (in chars).
    cur_col: usize,
    /// Selection anchor `(line, col)`; `None` when there is no selection.
    anchor: Option<(usize, usize)>,
    /// Top visible line (scroll offset).
    first_visible: usize,
    /// True if edited since the last `mark_clean` (load / save).
    dirty: bool,
}

impl Default for TextModel {
    fn default() -> Self {
        TextModel {
            lines: vec![String::new()],
            cur_line: 0,
            cur_col: 0,
            anchor: None,
            first_visible: 0,
            dirty: false,
        }
    }
}

impl TextModel {
    pub fn new() -> Self {
        TextModel::default()
    }

    /// Build a model from raw file bytes (UTF-8 lossy), splitting on `\n` and
    /// stripping a trailing `\r` per line (so CRLF files load cleanly). The
    /// cursor/scroll reset to the top and the model is marked clean.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        TextModel {
            lines,
            cur_line: 0,
            cur_col: 0,
            anchor: None,
            first_visible: 0,
            dirty: false,
        }
    }

    /// Serialize the document to bytes, joining lines with `\n` (no trailing
    /// newline added). What [`mui_ed_save`] writes to disk.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.lines.join("\n").into_bytes()
    }

    /// The document as a single string (used for find / completion / nav).
    pub fn as_text(&self) -> String {
        self.lines.join("\n")
    }

    // ---- accessors ----

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn cursor_line(&self) -> usize {
        self.cur_line
    }

    pub fn cursor_col(&self) -> usize {
        self.cur_col
    }

    pub fn first_visible(&self) -> usize {
        self.first_visible
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn set_dirty(&mut self, d: bool) {
        self.dirty = d;
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// The text of line `i` (or `""` out of range).
    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map(|s| s.as_str()).unwrap_or("")
    }

    /// Char length of line `i`.
    pub fn line_len(&self, i: usize) -> usize {
        self.lines.get(i).map(|s| s.chars().count()).unwrap_or(0)
    }

    pub fn set_first_visible(&mut self, first: usize) {
        self.first_visible = first.min(self.lines.len().saturating_sub(1));
    }

    /// Set the selection anchor to the current cursor (begin selecting), or
    /// clear it. (Reserved for shift-select; the ABI exposes a clear.)
    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// `true` if there is an active selection spanning at least one char.
    pub fn has_selection(&self) -> bool {
        match self.anchor {
            Some(a) => a != (self.cur_line, self.cur_col),
            None => false,
        }
    }

    /// The normalized selection range `((l0,c0),(l1,c1))` with start <= end, or
    /// `None` when there is no selection.
    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let b = (self.cur_line, self.cur_col);
        if a == b {
            return None;
        }
        if (a.0, a.1) <= (b.0, b.1) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    // ---- internal helpers ----

    /// Clamp the cursor column to the current line's length.
    fn clamp_col(&mut self) {
        let len = self.line_len(self.cur_line);
        if self.cur_col > len {
            self.cur_col = len;
        }
    }

    /// Split line `li` at char column `col`, returning `(head, tail)` strings.
    fn split_line(s: &str, col: usize) -> (String, String) {
        let byte = s
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or_else(|| s.len());
        (s[..byte].to_string(), s[byte..].to_string())
    }

    /// Begin a mutation: drop any selection (edits replace it conceptually; we
    /// don't implement selection-delete-on-type yet, so just clear) and mark
    /// dirty.
    fn begin_edit(&mut self) {
        self.anchor = None;
        self.dirty = true;
    }

    // ---- edit operations ----

    /// Insert a single Unicode scalar at the cursor. A `\n` is treated as a
    /// newline (so streaming text in works), otherwise the char is inserted into
    /// the current line and the cursor advances.
    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.newline();
            return;
        }
        if ch == '\r' {
            return; // ignore bare CR
        }
        self.begin_edit();
        let li = self.cur_line;
        let col = self.cur_col;
        let line = &self.lines[li];
        let (head, tail) = Self::split_line(line, col);
        self.lines[li] = format!("{head}{ch}{tail}");
        self.cur_col = col + 1;
    }

    /// Insert a newline at the cursor, splitting the current line.
    pub fn newline(&mut self) {
        self.begin_edit();
        let li = self.cur_line;
        let col = self.cur_col;
        let (head, tail) = Self::split_line(&self.lines[li], col);
        self.lines[li] = head;
        self.lines.insert(li + 1, tail);
        self.cur_line = li + 1;
        self.cur_col = 0;
    }

    /// Delete the char before the cursor (joining lines at column 0). No-op at
    /// the very start of the document.
    pub fn backspace(&mut self) {
        if self.cur_col > 0 {
            self.begin_edit();
            let li = self.cur_line;
            let col = self.cur_col;
            let s = &self.lines[li];
            let (head, tail) = Self::split_line(s, col);
            // Drop the last char of `head`.
            let mut hc: Vec<char> = head.chars().collect();
            hc.pop();
            let new_head: String = hc.into_iter().collect();
            self.cur_col = col - 1;
            self.lines[li] = format!("{new_head}{tail}");
        } else if self.cur_line > 0 {
            // Join this line onto the end of the previous one.
            self.begin_edit();
            let li = self.cur_line;
            let cur = self.lines.remove(li);
            let prev_len = self.line_len(li - 1);
            self.cur_line = li - 1;
            self.cur_col = prev_len;
            self.lines[li - 1].push_str(&cur);
        }
    }

    /// Delete the char at the cursor (joining the next line when at end of
    /// line). No-op at the very end of the document.
    pub fn delete(&mut self) {
        let li = self.cur_line;
        let col = self.cur_col;
        let len = self.line_len(li);
        if col < len {
            self.begin_edit();
            let (head, tail) = Self::split_line(&self.lines[li], col);
            // Drop the first char of `tail`.
            let new_tail: String = tail.chars().skip(1).collect();
            self.lines[li] = format!("{head}{new_tail}");
        } else if li + 1 < self.lines.len() {
            self.begin_edit();
            let next = self.lines.remove(li + 1);
            self.lines[li].push_str(&next);
        }
    }

    /// Move the cursor one step in `dir` (one of the `DIR_*` constants),
    /// clamping to document/line bounds. Clears the selection.
    pub fn move_cursor(&mut self, dir: i32) {
        self.anchor = None;
        match dir {
            DIR_LEFT => {
                if self.cur_col > 0 {
                    self.cur_col -= 1;
                } else if self.cur_line > 0 {
                    self.cur_line -= 1;
                    self.cur_col = self.line_len(self.cur_line);
                }
            }
            DIR_RIGHT => {
                let len = self.line_len(self.cur_line);
                if self.cur_col < len {
                    self.cur_col += 1;
                } else if self.cur_line + 1 < self.lines.len() {
                    self.cur_line += 1;
                    self.cur_col = 0;
                }
            }
            DIR_UP => {
                if self.cur_line > 0 {
                    self.cur_line -= 1;
                    self.clamp_col();
                } else {
                    self.cur_col = 0;
                }
            }
            DIR_DOWN => {
                if self.cur_line + 1 < self.lines.len() {
                    self.cur_line += 1;
                    self.clamp_col();
                } else {
                    self.cur_col = self.line_len(self.cur_line);
                }
            }
            DIR_HOME => self.cur_col = 0,
            DIR_END => self.cur_col = self.line_len(self.cur_line),
            _ => {}
        }
    }

    /// Move the cursor to an explicit `(line, col)`, clamped to the document.
    /// Clears the selection. Used by click, go-to-line, find, go-to-definition.
    pub fn move_to(&mut self, line: i32, col: i32) {
        self.anchor = None;
        let li = (line.max(0) as usize).min(self.lines.len().saturating_sub(1));
        self.cur_line = li;
        let len = self.line_len(li);
        self.cur_col = (col.max(0) as usize).min(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> TextModel {
        TextModel::from_bytes(s.as_bytes())
    }

    #[test]
    fn from_bytes_splits_lines_and_strips_cr() {
        let m = doc("ab\r\ncd\nef");
        assert_eq!(m.line_count(), 3);
        assert_eq!(m.line(0), "ab");
        assert_eq!(m.line(1), "cd");
        assert_eq!(m.line(2), "ef");
        assert!(!m.dirty());
    }

    #[test]
    fn empty_bytes_is_one_empty_line() {
        let m = TextModel::from_bytes(b"");
        assert_eq!(m.line_count(), 1);
        assert_eq!(m.line(0), "");
    }

    #[test]
    fn insert_char_advances_cursor_and_dirties() {
        let mut m = TextModel::new();
        m.insert_char('h');
        m.insert_char('i');
        assert_eq!(m.line(0), "hi");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 2));
        assert!(m.dirty());
    }

    #[test]
    fn insert_char_in_middle() {
        let mut m = doc("ac");
        m.move_to(0, 1);
        m.insert_char('b');
        assert_eq!(m.line(0), "abc");
        assert_eq!(m.cursor_col(), 2);
    }

    #[test]
    fn newline_splits_line() {
        let mut m = doc("hello world");
        m.move_to(0, 5);
        m.newline();
        assert_eq!(m.line_count(), 2);
        assert_eq!(m.line(0), "hello");
        assert_eq!(m.line(1), " world");
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 0));
    }

    #[test]
    fn insert_newline_char_routes_to_newline() {
        let mut m = doc("ab");
        m.move_to(0, 1);
        m.insert_char('\n');
        assert_eq!(m.line_count(), 2);
        assert_eq!(m.line(0), "a");
        assert_eq!(m.line(1), "b");
    }

    #[test]
    fn backspace_within_line() {
        let mut m = doc("abc");
        m.move_to(0, 3);
        m.backspace();
        assert_eq!(m.line(0), "ab");
        assert_eq!(m.cursor_col(), 2);
    }

    #[test]
    fn backspace_at_line_start_joins_previous() {
        let mut m = doc("ab\ncd");
        m.move_to(1, 0);
        m.backspace();
        assert_eq!(m.line_count(), 1);
        assert_eq!(m.line(0), "abcd");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 2));
    }

    #[test]
    fn backspace_at_doc_start_is_noop() {
        let mut m = doc("abc");
        m.move_to(0, 0);
        m.backspace();
        assert_eq!(m.line(0), "abc");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 0));
    }

    #[test]
    fn delete_within_line_and_joins_next() {
        let mut m = doc("abc\ndef");
        m.move_to(0, 0);
        m.delete();
        assert_eq!(m.line(0), "bc");
        // Delete at end-of-line joins the next line.
        m.move_to(0, 2);
        m.delete();
        assert_eq!(m.line_count(), 1);
        assert_eq!(m.line(0), "bcdef");
    }

    #[test]
    fn delete_at_doc_end_is_noop() {
        let mut m = doc("ab");
        m.move_to(0, 2);
        m.delete();
        assert_eq!(m.line(0), "ab");
        assert_eq!(m.line_count(), 1);
    }

    #[test]
    fn move_left_wraps_to_prev_line_end() {
        let mut m = doc("ab\ncd");
        m.move_to(1, 0);
        m.move_cursor(DIR_LEFT);
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 2));
    }

    #[test]
    fn move_right_wraps_to_next_line_start() {
        let mut m = doc("ab\ncd");
        m.move_to(0, 2);
        m.move_cursor(DIR_RIGHT);
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 0));
    }

    #[test]
    fn move_up_down_preserve_column_then_clamp() {
        let mut m = doc("longline\nhi\nanother");
        m.move_to(0, 6);
        m.move_cursor(DIR_DOWN); // line 1 "hi" len 2 -> clamp to 2
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 2));
        m.move_cursor(DIR_DOWN); // line 2 "another" len 7 -> col stays 2
        assert_eq!((m.cursor_line(), m.cursor_col()), (2, 2));
        m.move_cursor(DIR_UP);
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 2));
    }

    #[test]
    fn move_home_end() {
        let mut m = doc("hello");
        m.move_to(0, 3);
        m.move_cursor(DIR_HOME);
        assert_eq!(m.cursor_col(), 0);
        m.move_cursor(DIR_END);
        assert_eq!(m.cursor_col(), 5);
    }

    #[test]
    fn move_to_clamps_out_of_range() {
        let mut m = doc("ab\ncd");
        m.move_to(99, 99);
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 2));
        m.move_to(-5, -5);
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 0));
    }

    #[test]
    fn round_trips_through_bytes() {
        let src = "fn main() {\n  log(\"hi\")\n}\n";
        let m = TextModel::from_bytes(src.as_bytes());
        // from_bytes on "a\n" yields ["a",""] -> to_bytes "a\n". So a trailing
        // newline survives as an empty final line.
        assert_eq!(String::from_utf8(m.to_bytes()).unwrap(), src);
    }

    #[test]
    fn unicode_columns_are_chars_not_bytes() {
        let mut m = doc("café");
        m.move_to(0, 4); // 4 chars (é is one char, two bytes)
        m.insert_char('!');
        assert_eq!(m.line(0), "café!");
        m.backspace();
        m.backspace(); // remove '!' then 'é'
        assert_eq!(m.line(0), "caf");
    }

    #[test]
    fn scroll_offset_clamps() {
        let mut m = doc("a\nb\nc");
        m.set_first_visible(99);
        assert_eq!(m.first_visible(), 2);
        m.set_first_visible(1);
        assert_eq!(m.first_visible(), 1);
    }

    #[test]
    fn scripted_edit_sequence_is_live() {
        // Mirrors the MUI_EDIT_PROBE script: type, newline, type, backspace.
        let mut m = TextModel::new();
        for ch in "hello".chars() {
            m.insert_char(ch);
        }
        m.newline();
        for ch in "world".chars() {
            m.insert_char(ch);
        }
        m.backspace();
        assert_eq!(m.line_count(), 2);
        assert_eq!(m.line(0), "hello");
        assert_eq!(m.line(1), "worl");
        assert!(m.dirty());
    }
}
