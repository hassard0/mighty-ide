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

/// A single caret: a cursor position plus an optional selection anchor.
///
/// A `TextModel` always holds at least one caret; `carets[0]` is the PRIMARY
/// caret, which every legacy single-cursor accessor reads/writes. With exactly
/// one caret the model behaves identically to the old single-cursor model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    /// 0-based cursor line.
    pub line: usize,
    /// 0-based cursor column (in chars).
    pub col: usize,
    /// Selection anchor `(line, col)`; `None` when this caret has no selection.
    pub anchor: Option<(usize, usize)>,
}

impl Caret {
    fn at(line: usize, col: usize) -> Self {
        Caret { line, col, anchor: None }
    }

    /// `true` if this caret spans a non-empty selection.
    fn has_selection(&self) -> bool {
        match self.anchor {
            Some(a) => a != (self.line, self.col),
            None => false,
        }
    }

    /// Normalized selection range `((l0,c0),(l1,c1))`, start <= end, or `None`.
    fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let b = (self.line, self.col);
        if a == b {
            return None;
        }
        if a <= b {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }
}

/// An editable document: lines of text + one or more carets (each a cursor +
/// optional selection anchor) + the top visible line (scroll). Always holds at
/// least one line and at least one caret.
///
/// ## Multi-cursor invariant
///
/// `carets[0]` is the PRIMARY caret. With exactly one caret every operation is
/// byte-for-byte identical to the historical single-cursor model, so all legacy
/// accessors (`cursor_line`/`cursor_col`/`anchor`/…) just read `carets[0]`.
#[derive(Debug, Clone)]
pub struct TextModel {
    /// The document lines (no embedded newlines). Always non-empty.
    lines: Vec<String>,
    /// The carets; `carets[0]` is primary. Always non-empty.
    carets: Vec<Caret>,
    /// Top visible line (scroll offset).
    first_visible: usize,
    /// True if edited since the last `mark_clean` (load / save).
    dirty: bool,
}

impl Default for TextModel {
    fn default() -> Self {
        TextModel {
            lines: vec![String::new()],
            carets: vec![Caret::at(0, 0)],
            first_visible: 0,
            dirty: false,
        }
    }
}

// Internal accessors for the primary caret's fields. These keep the rest of the
// file (which was written against `cur_line`/`cur_col`/`anchor`) unchanged.
impl TextModel {
    #[inline]
    fn primary(&self) -> &Caret {
        &self.carets[0]
    }
    #[inline]
    fn primary_mut(&mut self) -> &mut Caret {
        &mut self.carets[0]
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
            carets: vec![Caret::at(0, 0)],
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
        self.carets[0].line
    }

    pub fn cursor_col(&self) -> usize {
        self.carets[0].col
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
        self.carets[0].anchor = None;
    }

    /// `true` if there is an active selection spanning at least one char.
    pub fn has_selection(&self) -> bool {
        match self.carets[0].anchor {
            Some(a) => a != (self.carets[0].line, self.carets[0].col),
            None => false,
        }
    }

    /// The normalized selection range `((l0,c0),(l1,c1))` with start <= end, or
    /// `None` when there is no selection.
    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.carets[0].anchor?;
        let b = (self.carets[0].line, self.carets[0].col);
        if a == b {
            return None;
        }
        if (a.0, a.1) <= (b.0, b.1) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Collapse to the PRIMARY caret and set an explicit selection from
    /// `start (line,col)` to `end (line,col)` (the cursor lands at `end`, the
    /// anchor at `start`), clamped to the document. When `start == end` this just
    /// places the cursor there with no selection. Used by the snippet engine to
    /// select a tab-stop's placeholder. Does not mark dirty (it is pure motion).
    pub fn set_selection(&mut self, start: (usize, usize), end: (usize, usize)) {
        let last = self.lines.len().saturating_sub(1);
        let clamp = |(l, c): (usize, usize)| {
            let l = l.min(last);
            let len = self.lines.get(l).map(|s| s.chars().count()).unwrap_or(0);
            (l, c.min(len))
        };
        let s = clamp(start);
        let e = clamp(end);
        // Collapse to a single primary caret carrying this selection.
        let caret = Caret {
            line: e.0,
            col: e.1,
            anchor: if s == e { None } else { Some(s) },
        };
        self.carets.clear();
        self.carets.push(caret);
    }

    /// Delete the PRIMARY caret's selected text (if any), collapsing the cursor to
    /// the selection start. No-op (returns `false`) when there is no selection.
    /// Marks dirty when it removes text. Used so typing over a tab-stop placeholder
    /// replaces it.
    pub fn delete_selection(&mut self) -> bool {
        let Some(((l0, c0), (l1, c1))) = self.selection_range() else {
            return false;
        };
        self.dirty = true;
        if l0 == l1 {
            let chars: Vec<char> = self.lines[l0].chars().collect();
            let head: String = chars[..c0.min(chars.len())].iter().collect();
            let tail: String = chars[c1.min(chars.len())..].iter().collect();
            self.lines[l0] = format!("{head}{tail}");
        } else {
            let first: Vec<char> = self.lines[l0].chars().collect();
            let head: String = first[..c0.min(first.len())].iter().collect();
            let last: Vec<char> = self.lines[l1].chars().collect();
            let tail: String = last[c1.min(last.len())..].iter().collect();
            // Remove the intervening lines (l0+1 ..= l1) and merge head+tail.
            self.lines.drain((l0 + 1)..=l1);
            self.lines[l0] = format!("{head}{tail}");
        }
        self.carets.clear();
        self.carets.push(Caret::at(l0, c0));
        true
    }

    // ---- internal helpers ----

    /// Clamp the cursor column to the current line's length.
    fn clamp_col(&mut self) {
        let len = self.line_len(self.carets[0].line);
        if self.carets[0].col > len {
            self.carets[0].col = len;
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
        self.carets[0].anchor = None;
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
        let li = self.carets[0].line;
        let col = self.carets[0].col;
        let line = &self.lines[li];
        let (head, tail) = Self::split_line(line, col);
        self.lines[li] = format!("{head}{ch}{tail}");
        self.carets[0].col = col + 1;
    }

    /// Insert a newline at the cursor, splitting the current line.
    pub fn newline(&mut self) {
        self.begin_edit();
        let li = self.carets[0].line;
        let col = self.carets[0].col;
        let (head, tail) = Self::split_line(&self.lines[li], col);
        self.lines[li] = head;
        self.lines.insert(li + 1, tail);
        self.carets[0].line = li + 1;
        self.carets[0].col = 0;
    }

    /// Delete the char before the cursor (joining lines at column 0). No-op at
    /// the very start of the document.
    pub fn backspace(&mut self) {
        if self.carets[0].col > 0 {
            self.begin_edit();
            let li = self.carets[0].line;
            let col = self.carets[0].col;
            let s = &self.lines[li];
            let (head, tail) = Self::split_line(s, col);
            // Drop the last char of `head`.
            let mut hc: Vec<char> = head.chars().collect();
            hc.pop();
            let new_head: String = hc.into_iter().collect();
            self.carets[0].col = col - 1;
            self.lines[li] = format!("{new_head}{tail}");
        } else if self.carets[0].line > 0 {
            // Join this line onto the end of the previous one.
            self.begin_edit();
            let li = self.carets[0].line;
            let cur = self.lines.remove(li);
            let prev_len = self.line_len(li - 1);
            self.carets[0].line = li - 1;
            self.carets[0].col = prev_len;
            self.lines[li - 1].push_str(&cur);
        }
    }

    /// Delete the char at the cursor (joining the next line when at end of
    /// line). No-op at the very end of the document.
    pub fn delete(&mut self) {
        let li = self.carets[0].line;
        let col = self.carets[0].col;
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
        self.carets[0].anchor = None;
        match dir {
            DIR_LEFT => {
                if self.carets[0].col > 0 {
                    self.carets[0].col -= 1;
                } else if self.carets[0].line > 0 {
                    self.carets[0].line -= 1;
                    self.carets[0].col = self.line_len(self.carets[0].line);
                }
            }
            DIR_RIGHT => {
                let len = self.line_len(self.carets[0].line);
                if self.carets[0].col < len {
                    self.carets[0].col += 1;
                } else if self.carets[0].line + 1 < self.lines.len() {
                    self.carets[0].line += 1;
                    self.carets[0].col = 0;
                }
            }
            DIR_UP => {
                if self.carets[0].line > 0 {
                    self.carets[0].line -= 1;
                    self.clamp_col();
                } else {
                    self.carets[0].col = 0;
                }
            }
            DIR_DOWN => {
                if self.carets[0].line + 1 < self.lines.len() {
                    self.carets[0].line += 1;
                    self.clamp_col();
                } else {
                    self.carets[0].col = self.line_len(self.carets[0].line);
                }
            }
            DIR_HOME => self.carets[0].col = 0,
            DIR_END => self.carets[0].col = self.line_len(self.carets[0].line),
            _ => {}
        }
    }

    /// Move the cursor to an explicit `(line, col)`, clamped to the document.
    /// Clears the selection. Used by click, go-to-line, find, go-to-definition.
    pub fn move_to(&mut self, line: i32, col: i32) {
        self.carets[0].anchor = None;
        let li = (line.max(0) as usize).min(self.lines.len().saturating_sub(1));
        self.carets[0].line = li;
        let len = self.line_len(li);
        self.carets[0].col = (col.max(0) as usize).min(len);
    }

    // -----------------------------------------------------------------------
    // Selection-aware motion (Shift+motion extends; plain motion collapses)
    // -----------------------------------------------------------------------

    /// If there is no anchor, drop one at the current cursor (begin selecting).
    fn begin_or_keep_anchor(&mut self) {
        if self.carets[0].anchor.is_none() {
            self.carets[0].anchor = Some((self.carets[0].line, self.carets[0].col));
        }
    }

    /// Move the cursor one step in `dir`, extending the selection when `extend`
    /// is set (Shift held) or collapsing it otherwise. The motion itself mirrors
    /// [`move_cursor`]; only the anchor handling differs.
    pub fn move_cursor_ext(&mut self, dir: i32, extend: bool) {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.carets[0].anchor = None;
        }
        self.step(dir);
    }

    /// Pure cursor step in `dir` with NO anchor side effects (used by both the
    /// plain and extending motions).
    fn step(&mut self, dir: i32) {
        match dir {
            DIR_LEFT => {
                if self.carets[0].col > 0 {
                    self.carets[0].col -= 1;
                } else if self.carets[0].line > 0 {
                    self.carets[0].line -= 1;
                    self.carets[0].col = self.line_len(self.carets[0].line);
                }
            }
            DIR_RIGHT => {
                let len = self.line_len(self.carets[0].line);
                if self.carets[0].col < len {
                    self.carets[0].col += 1;
                } else if self.carets[0].line + 1 < self.lines.len() {
                    self.carets[0].line += 1;
                    self.carets[0].col = 0;
                }
            }
            DIR_UP => {
                if self.carets[0].line > 0 {
                    self.carets[0].line -= 1;
                    self.clamp_col();
                } else {
                    self.carets[0].col = 0;
                }
            }
            DIR_DOWN => {
                if self.carets[0].line + 1 < self.lines.len() {
                    self.carets[0].line += 1;
                    self.clamp_col();
                } else {
                    self.carets[0].col = self.line_len(self.carets[0].line);
                }
            }
            DIR_HOME => self.carets[0].col = 0,
            DIR_END => self.carets[0].col = self.line_len(self.carets[0].line),
            _ => {}
        }
    }

    /// Smart Home: first press moves to the first non-whitespace char of the
    /// line; if already there (or before it), moves to column 0. Optionally
    /// extends the selection. Returns the resulting column.
    pub fn home_smart(&mut self, extend: bool) -> usize {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.carets[0].anchor = None;
        }
        let chars: Vec<char> = self.lines[self.carets[0].line].chars().collect();
        let first_non_ws = chars
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(chars.len());
        self.carets[0].col = if self.carets[0].col == first_non_ws {
            0
        } else {
            first_non_ws
        };
        self.carets[0].col
    }

    // -----------------------------------------------------------------------
    // Word-wise motion (Ctrl+Left / Ctrl+Right)
    // -----------------------------------------------------------------------

    /// Move one "word" left (skip whitespace then a run of word chars), or to
    /// the end of the previous line at column 0. Optionally extends selection.
    pub fn move_word_left(&mut self, extend: bool) {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.carets[0].anchor = None;
        }
        if self.carets[0].col == 0 {
            // Hop to the end of the previous line.
            if self.carets[0].line > 0 {
                self.carets[0].line -= 1;
                self.carets[0].col = self.line_len(self.carets[0].line);
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.carets[0].line].chars().collect();
        let mut i = self.carets[0].col;
        // Skip whitespace immediately to the left.
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        // Then skip the run of same-class chars.
        if i > 0 {
            let word = is_word_char(chars[i - 1]);
            while i > 0 && !chars[i - 1].is_whitespace() && is_word_char(chars[i - 1]) == word {
                i -= 1;
            }
        }
        self.carets[0].col = i;
    }

    /// Move one "word" right (skip a run of word chars then whitespace), or to
    /// the start of the next line. Optionally extends selection.
    pub fn move_word_right(&mut self, extend: bool) {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.carets[0].anchor = None;
        }
        let chars: Vec<char> = self.lines[self.carets[0].line].chars().collect();
        let len = chars.len();
        if self.carets[0].col >= len {
            // Hop to the start of the next line.
            if self.carets[0].line + 1 < self.lines.len() {
                self.carets[0].line += 1;
                self.carets[0].col = 0;
            }
            return;
        }
        let mut i = self.carets[0].col;
        // Skip the run of same-class chars under/after the cursor.
        if i < len && !chars[i].is_whitespace() {
            let word = is_word_char(chars[i]);
            while i < len && !chars[i].is_whitespace() && is_word_char(chars[i]) == word {
                i += 1;
            }
        }
        // Then skip trailing whitespace.
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        self.carets[0].col = i;
    }

    /// Select the word under the cursor (sets the anchor at its start and the
    /// cursor at its end). No-op (clears selection) if not on a word char.
    /// Returns the selected text.
    pub fn select_word(&mut self) -> String {
        let chars: Vec<char> = self.lines[self.carets[0].line].chars().collect();
        let len = chars.len();
        // Find the word boundaries around (or just before) the cursor.
        let mut s = self.carets[0].col.min(len);
        // If sitting just past a word char, step back onto it.
        if s == len && s > 0 && is_word_char(chars[s - 1]) {
            s -= 1;
        }
        if s >= len || !is_word_char(chars[s]) {
            // Try the char to the left.
            if self.carets[0].col > 0 && is_word_char(chars[self.carets[0].col - 1]) {
                s = self.carets[0].col - 1;
            } else {
                self.carets[0].anchor = None;
                return String::new();
            }
        }
        let mut start = s;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = s;
        while end < len && is_word_char(chars[end]) {
            end += 1;
        }
        self.carets[0].anchor = Some((self.carets[0].line, start));
        self.carets[0].col = end;
        chars[start..end].iter().collect()
    }

    /// Select the whole current line (anchor at col 0, cursor at end of line —
    /// or start of the next line so a trailing newline is included when there is
    /// one). Used by Ctrl+L-style line select / as the basis for duplicate.
    pub fn select_line(&mut self) {
        self.carets[0].anchor = Some((self.carets[0].line, 0));
        self.carets[0].col = self.line_len(self.carets[0].line);
    }

    /// The text currently selected (empty string when there is no selection).
    pub fn selected_text(&self) -> String {
        let Some(((l0, c0), (l1, c1))) = self.selection_range() else {
            return String::new();
        };
        if l0 == l1 {
            let chars: Vec<char> = self.lines[l0].chars().collect();
            return chars[c0.min(chars.len())..c1.min(chars.len())].iter().collect();
        }
        let mut out = String::new();
        for li in l0..=l1 {
            let chars: Vec<char> = self.lines[li].chars().collect();
            let (s, e) = if li == l0 {
                (c0.min(chars.len()), chars.len())
            } else if li == l1 {
                (0, c1.min(chars.len()))
            } else {
                (0, chars.len())
            };
            out.extend(chars[s..e].iter());
            if li != l1 {
                out.push('\n');
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // Feature 1 — toggle line comment (Ctrl+/)
    // -----------------------------------------------------------------------

    /// The inclusive `[start, end]` line range covered by the cursor or the
    /// active selection.
    fn affected_line_range(&self) -> (usize, usize) {
        match self.selection_range() {
            Some(((l0, _), (l1, _))) => (l0, l1),
            None => (self.carets[0].line, self.carets[0].line),
        }
    }

    /// Toggle a `// ` line comment on the cursor line or every selected line.
    /// If ALL non-blank lines in the range are already commented, uncomment;
    /// otherwise comment them all. Comment markers are inserted at each line's
    /// first non-whitespace column so indentation is preserved.
    pub fn toggle_line_comment(&mut self) {
        let (l0, l1) = self.affected_line_range();
        // Decide direction: comment unless every non-blank line is commented.
        let mut all_commented = true;
        let mut any_nonblank = false;
        for li in l0..=l1 {
            let line = &self.lines[li];
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            any_nonblank = true;
            if !trimmed.starts_with("//") {
                all_commented = false;
                break;
            }
        }
        if !any_nonblank {
            return;
        }
        self.begin_edit_keep_sel();
        if all_commented {
            // Uncomment: strip the first `// ` (or `//`) after the indent.
            for li in l0..=l1 {
                let line = &self.lines[li];
                let indent_len = line.len() - line.trim_start().len();
                let (indent, rest) = line.split_at(indent_len);
                if let Some(after) = rest.strip_prefix("// ") {
                    self.lines[li] = format!("{indent}{after}");
                } else if let Some(after) = rest.strip_prefix("//") {
                    self.lines[li] = format!("{indent}{after}");
                }
            }
        } else {
            // Comment: insert `// ` at the first non-whitespace column.
            for li in l0..=l1 {
                let line = &self.lines[li];
                if line.trim().is_empty() {
                    continue;
                }
                let indent_len = line.len() - line.trim_start().len();
                let (indent, rest) = line.split_at(indent_len);
                self.lines[li] = format!("{indent}// {rest}");
            }
        }
        // Keep the cursor on a sane column for the (possibly shifted) line.
        self.clamp_col();
    }

    /// Begin a mutation but preserve the selection (for line-range ops where the
    /// selection should survive, like comment toggle).
    fn begin_edit_keep_sel(&mut self) {
        self.dirty = true;
    }

    // -----------------------------------------------------------------------
    // Feature 2 — auto-indent on Enter
    // -----------------------------------------------------------------------

    /// Insert a newline that copies the current line's leading whitespace. If
    /// the line (up to the cursor) ends with `{`, add one indent level (2
    /// spaces). If the text after the cursor begins with `}`, the closing brace
    /// is dedented onto its own line one level shallower (open/close on Enter
    /// between `{` and `}` yields a blank indented line + a dedented `}`).
    pub fn newline_auto_indent(&mut self) {
        self.begin_edit();
        let li = self.carets[0].line;
        let col = self.carets[0].col;
        let line = self.lines[li].clone();
        let (head, tail) = Self::split_line(&line, col);

        // Leading whitespace of the current line.
        let indent: String = head.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let head_trim_end = head.trim_end();
        let opens = head_trim_end.ends_with('{');
        let tail_trim = tail.trim_start();
        let closes_next = tail_trim.starts_with('}');

        // One indent level = the configured tab width in spaces (Settings panel).
        let one_str = " ".repeat(crate::settings::tab_width().max(1) as usize);
        let one: &str = &one_str;

        self.lines[li] = head;
        if opens && closes_next {
            // Between a `{` and a `}`: blank indented line, then the dedented `}`.
            let inner = format!("{indent}{one}");
            self.lines.insert(li + 1, inner.clone());
            self.lines.insert(li + 2, format!("{indent}{tail}"));
            self.carets[0].line = li + 1;
            self.carets[0].col = inner.chars().count();
        } else if opens {
            let new_line = format!("{indent}{one}{tail}");
            let caret = format!("{indent}{one}").chars().count();
            self.lines.insert(li + 1, new_line);
            self.carets[0].line = li + 1;
            self.carets[0].col = caret;
        } else if closes_next && !indent.is_empty() {
            // New line is (or starts with) `}`: dedent one level.
            let dedent: String = indent.chars().skip(one.len()).collect();
            let new_line = format!("{dedent}{tail}");
            self.lines.insert(li + 1, new_line);
            self.carets[0].line = li + 1;
            self.carets[0].col = dedent.chars().count();
        } else {
            let new_line = format!("{indent}{tail}");
            self.lines.insert(li + 1, new_line);
            self.carets[0].line = li + 1;
            self.carets[0].col = indent.chars().count();
        }
    }

    // -----------------------------------------------------------------------
    // Feature 3 — bracket / quote auto-close
    // -----------------------------------------------------------------------

    /// The closing partner for an opening bracket/quote, or `None`.
    fn close_for(open: char) -> Option<char> {
        match open {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' => Some('"'),
            _ => None,
        }
    }

    /// `true` if `ch` is a closing bracket/quote we manage skip-over for.
    fn is_close_char(ch: char) -> bool {
        matches!(ch, ')' | ']' | '}' | '"')
    }

    /// The char immediately to the right of the cursor (the one `delete` would
    /// remove), or `None` at end of line.
    fn char_after(&self) -> Option<char> {
        self.lines[self.carets[0].line].chars().nth(self.carets[0].col)
    }

    /// The char immediately to the left of the cursor, or `None` at col 0.
    fn char_before(&self) -> Option<char> {
        if self.carets[0].col == 0 {
            return None;
        }
        self.lines[self.carets[0].line].chars().nth(self.carets[0].col - 1)
    }

    /// Smart insert of `ch` with bracket/quote auto-close + skip-over.
    ///
    /// * Typing an opener `(`/`[`/`{`/`"` inserts the matching closer and leaves
    ///   the cursor between the pair.
    /// * Typing a closer when the very next char is that same closer just steps
    ///   over it (no duplicate insert).
    /// * For `"`, an opening quote is only auto-closed when the cursor isn't
    ///   sitting right after a word char (so `say"` doesn't add a stray `"`),
    ///   and a `"` directly before another `"` skips over.
    ///
    /// Returns `true` if smart handling applied (caller should NOT also insert);
    /// `false` to fall back to a plain [`insert_char`].
    pub fn insert_char_smart(&mut self, ch: char) -> bool {
        // Skip-over: typing a closer that already sits to the right.
        if Self::is_close_char(ch) && self.char_after() == Some(ch) {
            self.carets[0].col += 1;
            return true;
        }
        // Auto-close openers.
        if let Some(close) = Self::close_for(ch) {
            // For a quote, don't auto-close right after a word char or right
            // before one (avoids doubling inside identifiers/strings).
            if ch == '"' {
                let after_word = self.char_after().map(is_word_char).unwrap_or(false);
                if after_word {
                    return false;
                }
            }
            self.begin_edit();
            let li = self.carets[0].line;
            let col = self.carets[0].col;
            let (head, tail) = Self::split_line(&self.lines[li], col);
            self.lines[li] = format!("{head}{ch}{close}{tail}");
            self.carets[0].col = col + 1; // between the pair
            return true;
        }
        false
    }

    /// Backspace that also removes the matching closer when deleting the opener
    /// of an empty pair (cursor between `()`, `[]`, `{}`, `""`). Returns `true`
    /// if the pair was deleted; `false` to fall back to a plain [`backspace`].
    pub fn backspace_smart(&mut self) -> bool {
        let (before, after) = (self.char_before(), self.char_after());
        if let (Some(b), Some(a)) = (before, after) {
            if Self::close_for(b) == Some(a) {
                self.begin_edit();
                let li = self.carets[0].line;
                let col = self.carets[0].col;
                let s = &self.lines[li];
                let chars: Vec<char> = s.chars().collect();
                // Remove the char before AND the char at the cursor.
                let mut out = String::new();
                for (i, c) in chars.iter().enumerate() {
                    if i == col - 1 || i == col {
                        continue;
                    }
                    out.push(*c);
                }
                self.lines[li] = out;
                self.carets[0].col = col - 1;
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Feature 4 — bracket match
    // -----------------------------------------------------------------------

    /// If the cursor is on or next to a bracket, return the `(line, col)` of its
    /// matching bracket (depth-counted across lines), else `None`. Checks the
    /// char to the right of the cursor first, then the char to the left.
    pub fn bracket_match(&self) -> Option<(usize, usize)> {
        // Prefer the bracket the cursor is sitting just before.
        if let Some(c) = self.char_after() {
            if let Some(m) = self.match_from(self.carets[0].line, self.carets[0].col, c) {
                return Some(m);
            }
        }
        // Then the bracket just to the left.
        if self.carets[0].col > 0 {
            if let Some(c) = self.char_before() {
                if let Some(m) = self.match_from(self.carets[0].line, self.carets[0].col - 1, c) {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Depth-count from the bracket `ch` at `(line, col)` to find its partner.
    fn match_from(&self, line: usize, col: usize, ch: char) -> Option<(usize, usize)> {
        let (open, close, forward) = match ch {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => return None,
        };
        let mut depth = 0i32;
        if forward {
            let mut li = line;
            let mut ci = col;
            loop {
                let chars: Vec<char> = self.lines[li].chars().collect();
                while ci < chars.len() {
                    let c = chars[ci];
                    if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            return Some((li, ci));
                        }
                    }
                    ci += 1;
                }
                if li + 1 >= self.lines.len() {
                    return None;
                }
                li += 1;
                ci = 0;
            }
        } else {
            let mut li = line as isize;
            let mut ci = col as isize;
            loop {
                let chars: Vec<char> = self.lines[li as usize].chars().collect();
                while ci >= 0 {
                    let c = chars[ci as usize];
                    if c == close {
                        depth += 1;
                    } else if c == open {
                        depth -= 1;
                        if depth == 0 {
                            return Some((li as usize, ci as usize));
                        }
                    }
                    ci -= 1;
                }
                if li == 0 {
                    return None;
                }
                li -= 1;
                ci = self.lines[li as usize].chars().count() as isize - 1;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Feature 5 — duplicate line/selection, move line up/down
    // -----------------------------------------------------------------------

    /// Duplicate the current line (no selection) or the selected line range,
    /// inserting the copy directly below. The cursor moves onto the copy so a
    /// repeated press stacks duplicates.
    pub fn duplicate(&mut self) {
        self.begin_edit_keep_sel();
        let (l0, l1) = self.affected_line_range();
        let block: Vec<String> = self.lines[l0..=l1].to_vec();
        let n = block.len();
        let insert_at = l1 + 1;
        for (i, line) in block.into_iter().enumerate() {
            self.lines.insert(insert_at + i, line);
        }
        // Move the cursor (and selection, if any) down onto the copy.
        self.carets[0].line += n;
        if let Some((al, ac)) = self.carets[0].anchor {
            self.carets[0].anchor = Some((al + n, ac));
        }
        self.clamp_col();
    }

    /// Move the current line (or selected line range) up by one. No-op when the
    /// range already starts at line 0.
    pub fn move_lines_up(&mut self) {
        let (l0, l1) = self.affected_line_range();
        if l0 == 0 {
            return;
        }
        self.begin_edit_keep_sel();
        let above = self.lines.remove(l0 - 1);
        self.lines.insert(l1, above);
        self.carets[0].line -= 1;
        if let Some((al, ac)) = self.carets[0].anchor {
            self.carets[0].anchor = Some((al.saturating_sub(1), ac));
        }
        self.clamp_col();
    }

    /// Move the current line (or selected line range) down by one. No-op when
    /// the range already ends at the last line.
    pub fn move_lines_down(&mut self) {
        let (l0, l1) = self.affected_line_range();
        if l1 + 1 >= self.lines.len() {
            return;
        }
        self.begin_edit_keep_sel();
        let below = self.lines.remove(l1 + 1);
        self.lines.insert(l0, below);
        self.carets[0].line += 1;
        if let Some((al, ac)) = self.carets[0].anchor {
            self.carets[0].anchor = Some((al + 1, ac));
        }
        self.clamp_col();
    }

    // -----------------------------------------------------------------------
    // Feature 6 — in-file replace
    // -----------------------------------------------------------------------

    /// Replace the next occurrence of `needle` at or after the cursor with
    /// `repl`, moving the cursor just past the replacement. Wraps to the top of
    /// the document if there is no match after the cursor. Returns `true` if a
    /// replacement was made. Preserves the scroll offset.
    pub fn replace_next(&mut self, needle: &str, repl: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        let from = (self.carets[0].line, self.carets[0].col);
        // Search forward from the cursor, then wrap.
        if let Some((l, c)) = self.find_from(needle, from) {
            self.apply_replace_at(l, c, needle, repl);
            return true;
        }
        if let Some((l, c)) = self.find_from(needle, (0, 0)) {
            self.apply_replace_at(l, c, needle, repl);
            return true;
        }
        false
    }

    /// Replace ALL occurrences of `needle` with `repl` throughout the document.
    /// Returns the number of replacements. Preserves the scroll offset; the
    /// cursor is clamped back into range.
    pub fn replace_all(&mut self, needle: &str, repl: &str) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let mut count = 0usize;
        for li in 0..self.lines.len() {
            let line = &self.lines[li];
            if line.contains(needle) {
                let n = line.matches(needle).count();
                if n > 0 {
                    self.lines[li] = line.replace(needle, repl);
                    count += n;
                }
            }
        }
        if count > 0 {
            self.dirty = true;
            self.carets[0].anchor = None;
            self.clamp_col();
        }
        count
    }

    /// Find `needle` at or after `(line, col)` searching forward line by line.
    /// Returns the `(line, char_col)` of the match start.
    fn find_from(&self, needle: &str, from: (usize, usize)) -> Option<(usize, usize)> {
        let (fl, fc) = from;
        for li in fl..self.lines.len() {
            let chars: Vec<char> = self.lines[li].chars().collect();
            let start_col = if li == fl { fc } else { 0 };
            let line_str: String = chars.iter().collect();
            // Char-index search via byte find translated back to char col.
            let search_from_byte: usize = chars
                .iter()
                .take(start_col.min(chars.len()))
                .map(|c| c.len_utf8())
                .sum();
            if search_from_byte <= line_str.len() {
                if let Some(byte_idx) = line_str[search_from_byte..].find(needle) {
                    let abs_byte = search_from_byte + byte_idx;
                    let char_col = line_str[..abs_byte].chars().count();
                    return Some((li, char_col));
                }
            }
        }
        None
    }

    /// Replace `needle` at char `(line, col)` with `repl`; move the cursor just
    /// past the replacement.
    fn apply_replace_at(&mut self, line: usize, col: usize, needle: &str, repl: &str) {
        self.dirty = true;
        self.carets[0].anchor = None;
        let chars: Vec<char> = self.lines[line].chars().collect();
        let needle_chars = needle.chars().count();
        let head: String = chars[..col].iter().collect();
        let tail: String = chars[(col + needle_chars).min(chars.len())..].iter().collect();
        self.lines[line] = format!("{head}{repl}{tail}");
        self.carets[0].line = line;
        self.carets[0].col = col + repl.chars().count();
        self.clamp_col();
    }

    // =======================================================================
    // Multi-cursor (multiple simultaneous carets / selections)
    // =======================================================================
    //
    // The model holds `carets[0..n]` with `carets[0]` PRIMARY. Every legacy
    // single-cursor op above reads/writes `carets[0]` and is unchanged. The
    // helpers below let the IDE add/move/collapse secondary carets and apply
    // edits + motion to ALL carets at once.
    //
    // Edits are applied BACK-TO-FRONT (highest document position first) by
    // running an existing single-caret op with that caret swapped into the
    // primary slot, then translating the *other* carets by the edit's effect.
    // Translation is computed from the change to the active caret's position
    // plus the change in this/other lines' lengths, which is enough for the
    // char-level edits the IDE performs (insert/backspace/delete/newline/etc.).

    /// Number of carets (>= 1).
    pub fn caret_count(&self) -> usize {
        self.carets.len()
    }

    /// The `i`-th caret's `(line, col)`, or `None` out of range.
    pub fn caret_at(&self, i: usize) -> Option<(usize, usize)> {
        self.carets.get(i).map(|c| (c.line, c.col))
    }

    /// The `i`-th caret's selection range, or `None` (out of range / no sel).
    pub fn caret_selection(&self, i: usize) -> Option<((usize, usize), (usize, usize))> {
        self.carets.get(i).and_then(|c| c.selection_range())
    }

    /// Collapse to the PRIMARY caret only and clear its selection (Esc).
    pub fn collapse_carets(&mut self) {
        let mut p = self.carets[0];
        p.anchor = None;
        self.carets.clear();
        self.carets.push(p);
    }

    /// Sort carets by document position (line, col) ascending and merge any
    /// that coincide at the same cursor position (keeping the FIRST, which for
    /// a back-to-front edit pass is the one whose selection we want to keep).
    /// The PRIMARY caret (the one currently at `carets[0]`) is preserved as the
    /// representative of its position so the primary identity survives a merge.
    fn normalize_carets(&mut self) {
        if self.carets.len() <= 1 {
            return;
        }
        // Remember the primary's identity by value; after sort/dedup we restore
        // it to slot 0 (or its merge representative at the same position).
        let primary = self.carets[0];
        // Stable sort by (line, col) so equal positions keep insertion order.
        self.carets.sort_by_key(|c| (c.line, c.col));
        let mut deduped: Vec<Caret> = Vec::with_capacity(self.carets.len());
        for c in self.carets.drain(..) {
            match deduped.last() {
                Some(last) if last.line == c.line && last.col == c.col => {
                    // Same cursor position: merge. Prefer a caret that carries a
                    // selection so a Ctrl+D match isn't silently dropped.
                    if deduped.last().unwrap().anchor.is_none() && c.anchor.is_some() {
                        *deduped.last_mut().unwrap() = c;
                    }
                }
                _ => deduped.push(c),
            }
        }
        self.carets = deduped;
        // Restore the primary to slot 0: find the caret at the primary position.
        if let Some(idx) = self
            .carets
            .iter()
            .position(|c| c.line == primary.line && c.col == primary.col)
        {
            self.carets.swap(0, idx);
        }
    }

    /// Run a single-caret op (closure receiving `&mut self` with the chosen
    /// caret installed as primary) at EVERY caret, processed back-to-front, and
    /// translate the remaining carets by the active caret's net displacement so
    /// their offsets stay valid. Returns nothing; callers mark dirty as needed.
    ///
    /// `op` must mutate ONLY `lines` and the primary caret (every existing edit
    /// op does). Because we process highest-position carets first, edits at a
    /// later caret never shift the positions of earlier (lower) carets, so we
    /// only translate carets that sit AFTER the active one, by the active
    /// caret's delta.
    fn for_each_caret_edit(&mut self, op: impl Fn(&mut TextModel)) {
        if self.carets.len() == 1 {
            op(self);
            return;
        }
        // Indices sorted by descending document position (back-to-front).
        let mut order: Vec<usize> = (0..self.carets.len()).collect();
        order.sort_by(|&a, &b| {
            (self.carets[b].line, self.carets[b].col).cmp(&(self.carets[a].line, self.carets[a].col))
        });
        for &idx in &order {
            // Install caret `idx` as primary, snapshot pre-op state.
            self.carets.swap(0, idx);
            let before_line = self.carets[0].line;
            let before_col = self.carets[0].col;
            let before_lines = self.lines.len();
            op(self);
            let after_col = self.carets[0].col;
            let line_delta = self.lines.len() as isize - before_lines as isize;
            // Persist the mutated caret back, restore slot order.
            let mutated = self.carets[0];
            self.carets.swap(0, idx);
            self.carets[idx] = mutated;
            // Translate every OTHER caret that sits strictly after the edit
            // point so its (line, col) tracks the inserted/removed text.
            for (j, c) in self.carets.iter_mut().enumerate() {
                if j == idx {
                    continue;
                }
                // Only carets at or beyond the edited line are affected.
                if c.line < before_line {
                    continue;
                }
                if c.line == before_line {
                    // Same line as the edit's start. If the caret is at/after the
                    // edit column, shift its column by the col delta (and possibly
                    // onto a new line when a newline split occurred).
                    if c.col >= before_col {
                        if line_delta > 0 {
                            // A newline was inserted here: text after the edit
                            // column moved down `line_delta` lines, rebased to the
                            // column delta on the final new line.
                            let col_shift = after_col as isize - before_col as isize;
                            c.line = (c.line as isize + line_delta) as usize;
                            c.col = (c.col as isize + col_shift).max(0) as usize;
                            shift_anchor(c, before_line, before_col, line_delta, col_shift, true);
                        } else {
                            let col_shift = after_col as isize - before_col as isize;
                            c.col = (c.col as isize + col_shift).max(0) as usize;
                            shift_anchor(c, before_line, before_col, line_delta, col_shift, false);
                        }
                    }
                } else {
                    // A line strictly after the edit's start line: only the line
                    // index shifts (by how many lines were added/removed).
                    c.line = (c.line as isize + line_delta).max(0) as usize;
                    if let Some((al, ac)) = c.anchor {
                        if al >= before_line {
                            c.anchor = Some(((al as isize + line_delta).max(0) as usize, ac));
                        }
                    }
                }
            }
        }
        self.clamp_all_carets();
        self.normalize_carets();
    }

    /// Clamp every caret (and its anchor) into the current document bounds.
    fn clamp_all_carets(&mut self) {
        let last_line = self.lines.len().saturating_sub(1);
        for c in &mut self.carets {
            c.line = c.line.min(last_line);
            let len = self.lines.get(c.line).map(|s| s.chars().count()).unwrap_or(0);
            c.col = c.col.min(len);
            if let Some((al, ac)) = c.anchor {
                let al = al.min(last_line);
                let alen = self.lines.get(al).map(|s| s.chars().count()).unwrap_or(0);
                c.anchor = Some((al, ac.min(alen)));
            }
        }
    }

    // ---- multi-caret edits (apply at EVERY caret, back-to-front) ----

    /// Insert one scalar at every caret. With one caret == [`insert_char`].
    pub fn insert_char_multi(&mut self, ch: char) {
        self.for_each_caret_edit(|m| m.insert_char(ch));
    }

    /// Smart insert (auto-close/skip-over) at every caret; falls back to a plain
    /// insert at carets the smart path declined. With one caret == identical to
    /// `insert_char_smart` followed by the caller's fallback.
    pub fn insert_char_smart_multi(&mut self, ch: char) {
        self.for_each_caret_edit(|m| {
            if !m.insert_char_smart(ch) {
                m.insert_char(ch);
            }
        });
    }

    /// Newline (auto-indent) at every caret. With one caret == `newline_auto_indent`.
    pub fn newline_indent_multi(&mut self) {
        self.for_each_caret_edit(|m| m.newline_auto_indent());
    }

    /// Plain newline at every caret. With one caret == [`newline`].
    pub fn newline_multi(&mut self) {
        self.for_each_caret_edit(|m| m.newline());
    }

    /// Backspace at every caret. With one caret == [`backspace`].
    pub fn backspace_multi(&mut self) {
        self.for_each_caret_edit(|m| {
            if !m.backspace_smart() {
                m.backspace();
            }
        });
    }

    /// Delete-forward at every caret. With one caret == [`delete`].
    pub fn delete_multi(&mut self) {
        self.for_each_caret_edit(|m| m.delete());
    }

    // ---- multi-caret motion (move EVERY caret; Shift extends each) ----

    /// Single-step motion at every caret (`extend` keeps/grows each selection).
    pub fn move_ext_multi(&mut self, dir: i32, extend: bool) {
        for c in &mut self.carets {
            Self::caret_step_ext(&self.lines, c, dir, extend);
        }
        self.normalize_carets();
    }

    /// Word motion at every caret.
    pub fn move_word_multi(&mut self, right: bool, extend: bool) {
        // Reuse the single-caret word ops by swapping each caret to primary.
        let n = self.carets.len();
        for i in 0..n {
            self.carets.swap(0, i);
            if right {
                self.move_word_right(extend);
            } else {
                self.move_word_left(extend);
            }
            self.carets.swap(0, i);
        }
        self.normalize_carets();
    }

    /// Smart-home at every caret.
    pub fn home_smart_multi(&mut self, extend: bool) {
        let n = self.carets.len();
        for i in 0..n {
            self.carets.swap(0, i);
            self.home_smart(extend);
            self.carets.swap(0, i);
        }
        self.normalize_carets();
    }

    /// Pure step of one caret in `dir`, with anchor handling for `extend`. The
    /// motion mirrors [`step`] but operates on an arbitrary caret against
    /// `lines` (so it can run for every caret without touching `self`).
    fn caret_step_ext(lines: &[String], c: &mut Caret, dir: i32, extend: bool) {
        if extend {
            if c.anchor.is_none() {
                c.anchor = Some((c.line, c.col));
            }
        } else {
            c.anchor = None;
        }
        let line_len = |li: usize| lines.get(li).map(|s| s.chars().count()).unwrap_or(0);
        match dir {
            DIR_LEFT => {
                if c.col > 0 {
                    c.col -= 1;
                } else if c.line > 0 {
                    c.line -= 1;
                    c.col = line_len(c.line);
                }
            }
            DIR_RIGHT => {
                let len = line_len(c.line);
                if c.col < len {
                    c.col += 1;
                } else if c.line + 1 < lines.len() {
                    c.line += 1;
                    c.col = 0;
                }
            }
            DIR_UP => {
                if c.line > 0 {
                    c.line -= 1;
                    c.col = c.col.min(line_len(c.line));
                } else {
                    c.col = 0;
                }
            }
            DIR_DOWN => {
                if c.line + 1 < lines.len() {
                    c.line += 1;
                    c.col = c.col.min(line_len(c.line));
                } else {
                    c.col = line_len(c.line);
                }
            }
            DIR_HOME => c.col = 0,
            DIR_END => c.col = line_len(c.line),
            _ => {}
        }
    }

    // ---- add carets ----

    /// Add a caret on the line `delta` (±1) away from the PRIMARY caret at the
    /// same column (column-block carets; Ctrl+Alt+Up/Down). Clamps the column to
    /// the target line's length. No-op at the document edge. Returns `true` when
    /// a caret was added.
    pub fn add_caret_vertical(&mut self, delta: isize) -> bool {
        let p = self.carets[0];
        let target = p.line as isize + delta;
        if target < 0 || target as usize >= self.lines.len() {
            return false;
        }
        let tl = target as usize;
        let col = p.col.min(self.line_len(tl));
        let new = Caret::at(tl, col);
        // Don't duplicate an existing caret at that exact spot.
        if self.carets.iter().any(|c| c.line == tl && c.col == col) {
            return false;
        }
        // Insert and make it primary so a repeated press keeps extending.
        self.carets.insert(0, new);
        self.normalize_carets();
        // Re-make the just-added caret primary (normalize may have reordered).
        if let Some(idx) = self.carets.iter().position(|c| c.line == tl && c.col == col) {
            self.carets.swap(0, idx);
        }
        true
    }

    /// Ctrl+D. If the primary caret has no selection, select the word under it
    /// (and make that the primary selection). If it already has a selection, add
    /// a NEW caret selecting the next occurrence of the selected text (searching
    /// forward from the end of the last/primary selection, wrapping), make it
    /// primary, and return `true`. Returns `false` when there is no other match
    /// (or no word under the caret).
    pub fn add_caret_next_occurrence(&mut self) -> bool {
        // Phase 1: no selection -> select the word under the primary caret.
        if !self.carets[0].has_selection() {
            let word = self.select_word();
            return !word.is_empty();
        }
        // Phase 2: a selection exists -> find the next occurrence of its text.
        let needle = self.selected_text();
        if needle.is_empty() || needle.contains('\n') {
            return false;
        }
        // Start searching just after the primary selection's end.
        let start = self
            .carets[0]
            .selection_range()
            .map(|(_, end)| end)
            .unwrap_or((self.carets[0].line, self.carets[0].col));
        // Collect positions already covered by a caret's selection so we skip them.
        let occupied: Vec<((usize, usize), (usize, usize))> =
            self.carets.iter().filter_map(|c| c.selection_range()).collect();
        let needle_chars = needle.chars().count();
        // Search forward from `start`, then wrap to the top.
        let found = self
            .find_occurrence_from(&needle, start)
            .or_else(|| self.find_occurrence_from(&needle, (0, 0)));
        let Some((fl, fc)) = found else {
            return false;
        };
        let new = Caret {
            line: fl,
            col: fc + needle_chars,
            anchor: Some((fl, fc)),
        };
        // Skip if this exact selection is already held by a caret.
        let new_range = ((fl, fc), (fl, fc + needle_chars));
        if occupied.contains(&new_range) {
            return false;
        }
        self.carets.insert(0, new);
        true
    }

    /// Find `needle` at or after `(line, col)` (char coords), returning the match
    /// start. Single-line needles only (callers guarantee no `\n`).
    fn find_occurrence_from(&self, needle: &str, from: (usize, usize)) -> Option<(usize, usize)> {
        let (fl, fc) = from;
        for li in fl..self.lines.len() {
            let line_str = &self.lines[li];
            let start_col = if li == fl { fc } else { 0 };
            let chars: Vec<char> = line_str.chars().collect();
            let search_byte: usize = chars.iter().take(start_col.min(chars.len())).map(|c| c.len_utf8()).sum();
            if search_byte <= line_str.len() {
                if let Some(b) = line_str[search_byte..].find(needle) {
                    let abs = search_byte + b;
                    let col = line_str[..abs].chars().count();
                    return Some((li, col));
                }
            }
        }
        None
    }

    /// Toggle a caret at `(line, col)` (Alt+Click). If a caret already sits at
    /// that exact position, remove it (unless it's the only one); otherwise add
    /// one and make it primary.
    pub fn toggle_caret_at(&mut self, line: i32, col: i32) {
        let li = (line.max(0) as usize).min(self.lines.len().saturating_sub(1));
        let col = (col.max(0) as usize).min(self.line_len(li));
        if let Some(idx) = self.carets.iter().position(|c| c.line == li && c.col == col) {
            if self.carets.len() > 1 {
                self.carets.remove(idx);
            }
        } else {
            self.carets.insert(0, Caret::at(li, col));
        }
    }

    #[cfg(test)]
    fn set_primary_for_test(&mut self, line: usize, col: usize, anchor: Option<(usize, usize)>) {
        self.carets[0] = Caret { line, col, anchor };
    }
}

/// Shift a caret's anchor by an edit at `(before_line, before_col)` that moved
/// content by `line_delta` lines / `col_shift` columns. `crossed_line` marks an
/// edit that pushed content onto a new line (newline insert).
fn shift_anchor(
    c: &mut Caret,
    before_line: usize,
    before_col: usize,
    line_delta: isize,
    col_shift: isize,
    crossed_line: bool,
) {
    if let Some((al, ac)) = c.anchor {
        if al == before_line && ac >= before_col {
            if crossed_line && line_delta > 0 {
                c.anchor = Some(((al as isize + line_delta).max(0) as usize, (ac as isize + col_shift).max(0) as usize));
            } else {
                c.anchor = Some((al, (ac as isize + col_shift).max(0) as usize));
            }
        } else if al > before_line {
            c.anchor = Some(((al as isize + line_delta).max(0) as usize, ac));
        }
    }
}

/// A "word" char for word-motion / select-word: alphanumeric or underscore.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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
    fn set_selection_selects_range_and_clamps() {
        let mut m = doc("hello world");
        m.set_selection((0, 6), (0, 11));
        assert!(m.has_selection());
        assert_eq!(m.selected_text(), "world");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 11));
        // Equal start==end places the cursor with no selection.
        m.set_selection((0, 3), (0, 3));
        assert!(!m.has_selection());
        assert_eq!(m.cursor_col(), 3);
        // Out-of-range cols clamp to the line length.
        m.set_selection((0, 0), (0, 999));
        assert_eq!(m.selected_text(), "hello world");
    }

    #[test]
    fn delete_selection_single_line_and_multi_line() {
        let mut m = doc("hello world");
        m.set_selection((0, 5), (0, 11)); // " world"
        assert!(m.delete_selection());
        assert_eq!(m.line(0), "hello");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 5));
        assert!(!m.has_selection());
        // No selection -> no-op.
        assert!(!m.delete_selection());

        // Multi-line selection.
        let mut m2 = doc("aaa\nbbb\nccc");
        m2.set_selection((0, 1), (2, 1)); // from "a|aa" to "c|cc"
        assert!(m2.delete_selection());
        assert_eq!(m2.line_count(), 1);
        assert_eq!(m2.line(0), "acc");
        assert_eq!((m2.cursor_line(), m2.cursor_col()), (0, 1));
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

    // ---- Feature 1: toggle line comment ----

    #[test]
    fn comment_toggle_single_line() {
        let mut m = doc("  let x = 1");
        m.move_to(0, 6);
        m.toggle_line_comment();
        assert_eq!(m.line(0), "  // let x = 1");
        assert!(m.dirty());
        // Toggle back off.
        m.toggle_line_comment();
        assert_eq!(m.line(0), "  let x = 1");
    }

    #[test]
    fn comment_toggle_multi_line_all_commented_uncomments() {
        let mut m = doc("a\nb\nc");
        m.set_primary_for_test(2, 1, Some((0, 0)));
        m.toggle_line_comment();
        assert_eq!(m.line(0), "// a");
        assert_eq!(m.line(1), "// b");
        assert_eq!(m.line(2), "// c");
        // All commented -> next toggle uncomments all.
        m.toggle_line_comment();
        assert_eq!(m.line(0), "a");
        assert_eq!(m.line(1), "b");
        assert_eq!(m.line(2), "c");
    }

    #[test]
    fn comment_toggle_mixed_comments_all() {
        // One line commented, one not -> "not all commented" so comment both.
        let mut m = doc("// a\nb");
        m.set_primary_for_test(1, 1, Some((0, 0)));
        m.toggle_line_comment();
        assert_eq!(m.line(0), "// // a");
        assert_eq!(m.line(1), "// b");
    }

    #[test]
    fn comment_toggle_skips_blank_lines() {
        let mut m = doc("a\n\nb");
        m.set_primary_for_test(2, 1, Some((0, 0)));
        m.toggle_line_comment();
        assert_eq!(m.line(0), "// a");
        assert_eq!(m.line(1), ""); // blank stays blank
        assert_eq!(m.line(2), "// b");
    }

    // ---- Feature 2: auto-indent on Enter ----

    /// Auto-indent uses the configured tab width (default 2). The settings global
    /// is shared across tests, so pin it to the default (under the shared lock)
    /// for the brace-indent assertions below.
    fn pin_default_settings() -> std::sync::MutexGuard<'static, ()> {
        let g = crate::settings::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::settings::set_active(crate::settings::Settings::default());
        g
    }

    #[test]
    fn auto_indent_copies_leading_whitespace() {
        let mut m = doc("    foo");
        m.move_to(0, 7);
        m.newline_auto_indent();
        assert_eq!(m.line(0), "    foo");
        assert_eq!(m.line(1), "    ");
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 4));
    }

    #[test]
    fn auto_indent_after_open_brace_adds_level() {
        let _g = pin_default_settings();
        let mut m = doc("fn main() {");
        m.move_to(0, 11);
        m.newline_auto_indent();
        assert_eq!(m.line(1), "  ");
        assert_eq!(m.cursor_col(), 2);
    }

    #[test]
    fn auto_indent_between_braces_splits_and_dedents() {
        let _g = pin_default_settings();
        let mut m = doc("  fn f() {}");
        // Cursor between { and }.
        m.move_to(0, 10);
        m.newline_auto_indent();
        assert_eq!(m.line(0), "  fn f() {");
        assert_eq!(m.line(1), "    ");
        assert_eq!(m.line(2), "  }");
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 4));
    }

    #[test]
    fn auto_indent_dedents_line_starting_with_close_brace() {
        let _g = pin_default_settings();
        // Cursor before a `}` on an indented continuation line.
        let mut m = doc("    }");
        m.move_to(0, 4);
        m.newline_auto_indent();
        assert_eq!(m.line(0), "    ");
        assert_eq!(m.line(1), "  }");
        assert_eq!(m.cursor_col(), 2);
    }

    // ---- Feature 3: bracket / quote auto-close ----

    #[test]
    fn autoclose_inserts_pair_and_cursor_between() {
        let mut m = TextModel::new();
        assert!(m.insert_char_smart('('));
        assert_eq!(m.line(0), "()");
        assert_eq!(m.cursor_col(), 1);
        assert!(m.insert_char_smart('['));
        assert_eq!(m.line(0), "([])");
        assert_eq!(m.cursor_col(), 2);
    }

    #[test]
    fn autoclose_skip_over_existing_close() {
        let mut m = TextModel::new();
        m.insert_char_smart('('); // "()" cursor at 1
        // Typing ')' skips over the existing one rather than inserting.
        assert!(m.insert_char_smart(')'));
        assert_eq!(m.line(0), "()");
        assert_eq!(m.cursor_col(), 2);
    }

    #[test]
    fn autoclose_quote_pairs_but_not_after_word() {
        let mut m = TextModel::new();
        assert!(m.insert_char_smart('"'));
        assert_eq!(m.line(0), "\"\"");
        assert_eq!(m.cursor_col(), 1);
        // skip-over the close quote
        assert!(m.insert_char_smart('"'));
        assert_eq!(m.cursor_col(), 2);
    }

    #[test]
    fn autoclose_quote_not_doubled_before_word() {
        let mut m = doc("xy");
        m.move_to(0, 0); // before 'x' (a word char)
        // Should NOT auto-close (returns false -> caller inserts plainly).
        assert!(!m.insert_char_smart('"'));
    }

    #[test]
    fn backspace_smart_deletes_empty_pair() {
        let mut m = TextModel::new();
        m.insert_char_smart('('); // "()" cursor at 1
        assert!(m.backspace_smart());
        assert_eq!(m.line(0), "");
        assert_eq!(m.cursor_col(), 0);
    }

    #[test]
    fn backspace_smart_falls_through_when_not_pair() {
        let mut m = doc("ab");
        m.move_to(0, 1);
        // 'a' then 'b' is not a bracket pair.
        assert!(!m.backspace_smart());
    }

    // ---- Feature 4: bracket match ----

    #[test]
    fn bracket_match_forward_and_back() {
        let mut m = doc("a(b(c)d)e");
        m.move_to(0, 1); // just before the outer '('
        assert_eq!(m.bracket_match(), Some((0, 7)));
        m.move_to(0, 8); // just after the outer ')'
        assert_eq!(m.bracket_match(), Some((0, 1)));
    }

    #[test]
    fn bracket_match_across_lines() {
        let mut m = doc("fn f() {\n  body\n}");
        m.move_to(0, 7); // just before the '{'
        assert_eq!(m.bracket_match(), Some((2, 0)));
    }

    #[test]
    fn bracket_match_none_when_not_on_bracket() {
        let mut m = doc("abc");
        m.move_to(0, 1);
        assert_eq!(m.bracket_match(), None);
    }

    // ---- Feature 5: duplicate + move line ----

    #[test]
    fn duplicate_single_line() {
        let mut m = doc("foo\nbar");
        m.move_to(0, 2);
        m.duplicate();
        assert_eq!(m.line_count(), 3);
        assert_eq!(m.line(0), "foo");
        assert_eq!(m.line(1), "foo");
        assert_eq!(m.line(2), "bar");
        assert_eq!(m.cursor_line(), 1); // cursor moved onto the copy
    }

    #[test]
    fn duplicate_selection_range() {
        let mut m = doc("a\nb\nc");
        m.set_primary_for_test(1, 1, Some((0, 0)));
        m.duplicate();
        assert_eq!(m.line_count(), 5);
        assert_eq!(m.line(0), "a");
        assert_eq!(m.line(1), "b");
        assert_eq!(m.line(2), "a");
        assert_eq!(m.line(3), "b");
        assert_eq!(m.line(4), "c");
    }

    #[test]
    fn move_line_down_and_up() {
        let mut m = doc("one\ntwo\nthree");
        m.move_to(0, 0);
        m.move_lines_down();
        assert_eq!(m.line(0), "two");
        assert_eq!(m.line(1), "one");
        assert_eq!(m.cursor_line(), 1);
        m.move_lines_up();
        assert_eq!(m.line(0), "one");
        assert_eq!(m.line(1), "two");
        assert_eq!(m.cursor_line(), 0);
    }

    #[test]
    fn move_line_at_edges_is_noop() {
        let mut m = doc("a\nb");
        m.move_to(0, 0);
        m.move_lines_up(); // already at top
        assert_eq!(m.line(0), "a");
        m.move_to(1, 0);
        m.move_lines_down(); // already at bottom
        assert_eq!(m.line(1), "b");
    }

    // ---- Feature 6: in-file replace ----

    #[test]
    fn replace_next_replaces_and_advances() {
        let mut m = doc("foo bar foo");
        m.move_to(0, 0);
        assert!(m.replace_next("foo", "X"));
        assert_eq!(m.line(0), "X bar foo");
        assert_eq!(m.cursor_col(), 1);
        // Next replace hits the second occurrence.
        assert!(m.replace_next("foo", "X"));
        assert_eq!(m.line(0), "X bar X");
    }

    #[test]
    fn replace_next_wraps_to_top() {
        let mut m = doc("foo\nbar");
        m.move_to(1, 0); // past the only "foo"
        assert!(m.replace_next("foo", "Z"));
        assert_eq!(m.line(0), "Z");
    }

    #[test]
    fn replace_all_counts_and_replaces() {
        let mut m = doc("aa\naba\naa");
        let n = m.replace_all("a", "b");
        assert_eq!(n, 6);
        assert_eq!(m.line(0), "bb");
        assert_eq!(m.line(1), "bbb");
        assert_eq!(m.line(2), "bb");
        assert!(m.dirty());
    }

    #[test]
    fn replace_empty_needle_is_noop() {
        let mut m = doc("abc");
        assert!(!m.replace_next("", "x"));
        assert_eq!(m.replace_all("", "x"), 0);
        assert_eq!(m.line(0), "abc");
    }

    // ---- Feature 7: word motion + selection + smart home ----

    #[test]
    fn word_motion_right_and_left() {
        let mut m = doc("foo bar  baz");
        m.move_to(0, 0);
        m.move_word_right(false);
        assert_eq!(m.cursor_col(), 4); // start of "bar"
        m.move_word_right(false);
        assert_eq!(m.cursor_col(), 9); // start of "baz"
        m.move_word_left(false);
        assert_eq!(m.cursor_col(), 4); // back to "bar"
    }

    #[test]
    fn word_motion_wraps_lines() {
        let mut m = doc("ab\ncd");
        m.move_to(0, 2); // end of line 0
        m.move_word_right(false);
        assert_eq!((m.cursor_line(), m.cursor_col()), (1, 0));
        m.move_word_left(false);
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 2));
    }

    #[test]
    fn shift_motion_extends_selection() {
        let mut m = doc("hello");
        m.move_to(0, 0);
        m.move_cursor_ext(DIR_RIGHT, true);
        m.move_cursor_ext(DIR_RIGHT, true);
        assert!(m.has_selection());
        assert_eq!(m.selected_text(), "he");
        // Plain motion collapses the selection.
        m.move_cursor_ext(DIR_RIGHT, false);
        assert!(!m.has_selection());
    }

    #[test]
    fn smart_home_toggles_indent_and_col0() {
        let mut m = doc("    code");
        m.move_to(0, 8);
        assert_eq!(m.home_smart(false), 4); // first non-ws
        assert_eq!(m.home_smart(false), 0); // then col 0
        assert_eq!(m.home_smart(false), 4); // back to first non-ws
    }

    #[test]
    fn select_word_picks_identifier() {
        let mut m = doc("foo bar_baz qux");
        m.move_to(0, 6); // inside "bar_baz"
        assert_eq!(m.select_word(), "bar_baz");
        assert!(m.has_selection());
        let ((l0, c0), (l1, c1)) = m.selection_range().unwrap();
        assert_eq!((l0, c0, l1, c1), (0, 4, 0, 11));
    }

    #[test]
    fn select_line_spans_whole_line() {
        let mut m = doc("abc\ndef");
        m.move_to(0, 1);
        m.select_line();
        assert_eq!(m.selected_text(), "abc");
    }

    // ---- Multi-cursor ----

    /// Helper: positions of all carets as a sorted Vec for order-independent asserts.
    fn caret_positions(m: &TextModel) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = (0..m.caret_count()).filter_map(|i| m.caret_at(i)).collect();
        v.sort();
        v
    }

    #[test]
    fn single_caret_is_default_and_primary_unchanged() {
        let mut m = doc("hello");
        assert_eq!(m.caret_count(), 1);
        // Single-caret multi-ops behave exactly like the legacy single-cursor ops.
        m.move_to(0, 0);
        m.insert_char_multi('X');
        assert_eq!(m.line(0), "Xhello");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 1));
        assert_eq!(m.caret_count(), 1);
    }

    #[test]
    fn add_caret_vertical_adds_column_block_and_clamps_short_lines() {
        let mut m = doc("aaaa\nbb\ncccc");
        m.move_to(0, 3);
        assert!(m.add_caret_vertical(1)); // add on line 1 at col 3 -> clamped to 2
        assert_eq!(m.caret_count(), 2);
        assert_eq!(caret_positions(&m), vec![(0, 3), (1, 2)]);
        // Add again downward from the new primary (line 1) -> line 2 col... primary col is 2.
        assert!(m.add_caret_vertical(1));
        assert_eq!(m.caret_count(), 3);
        assert_eq!(caret_positions(&m), vec![(0, 3), (1, 2), (2, 2)]);
        // At the top edge, add-above is a no-op.
        m.collapse_carets();
        m.move_to(0, 0);
        assert!(!m.add_caret_vertical(-1));
        assert_eq!(m.caret_count(), 1);
    }

    #[test]
    fn multi_insert_applies_at_every_caret_back_to_front() {
        let mut m = doc("aa\nbb\ncc");
        // Carets at the start of each line.
        m.move_to(0, 0);
        m.add_caret_vertical(1);
        m.add_caret_vertical(1); // primary now line 2, plus line 1 and line 0
        assert_eq!(m.caret_count(), 3);
        m.insert_char_multi('>');
        assert_eq!(m.line(0), ">aa");
        assert_eq!(m.line(1), ">bb");
        assert_eq!(m.line(2), ">cc");
        // Every caret advanced one column.
        assert_eq!(caret_positions(&m), vec![(0, 1), (1, 1), (2, 1)]);
    }

    #[test]
    fn multi_insert_same_line_offsets_stay_correct() {
        // Two carets on the SAME line at different columns: back-to-front edit
        // must keep the earlier caret's offset valid.
        let mut m = doc("abcdef");
        m.move_to(0, 1);
        m.carets.push(Caret::at(0, 4));
        assert_eq!(m.caret_count(), 2);
        m.insert_char_multi('*');
        // Inserts at col 4 then col 1: "a*bc d*ef" -> "a*bcd*ef"
        assert_eq!(m.line(0), "a*bcd*ef");
        // carets now after each inserted '*'
        assert_eq!(caret_positions(&m), vec![(0, 2), (0, 6)]);
    }

    #[test]
    fn multi_backspace_at_every_caret() {
        let mut m = doc("xa\nxb\nxc");
        m.move_to(0, 1);
        m.add_caret_vertical(1);
        m.add_caret_vertical(1);
        assert_eq!(m.caret_count(), 3);
        m.backspace_multi(); // delete the 'x' before each caret
        assert_eq!(m.line(0), "a");
        assert_eq!(m.line(1), "b");
        assert_eq!(m.line(2), "c");
        assert_eq!(caret_positions(&m), vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn multi_newline_splits_at_every_caret() {
        let mut m = doc("aXb\ncXd");
        m.move_to(0, 1);
        m.add_caret_vertical(1); // caret on line 1 col 1
        assert_eq!(m.caret_count(), 2);
        m.newline_multi();
        assert_eq!(m.line_count(), 4);
        assert_eq!(m.line(0), "a");
        assert_eq!(m.line(1), "Xb");
        assert_eq!(m.line(2), "c");
        assert_eq!(m.line(3), "Xd");
    }

    #[test]
    fn multi_motion_moves_every_caret_and_merges_on_collision() {
        let mut m = doc("ab\nab");
        m.move_to(0, 0);
        m.add_caret_vertical(1); // carets (0,0) and (1,0)
        assert_eq!(m.caret_count(), 2);
        m.move_ext_multi(DIR_RIGHT, false);
        assert_eq!(caret_positions(&m), vec![(0, 1), (1, 1)]);
        // Move both to End -> distinct lines, stay 2.
        m.move_ext_multi(DIR_END, false);
        assert_eq!(caret_positions(&m), vec![(0, 2), (1, 2)]);
    }

    #[test]
    fn carets_at_same_position_merge() {
        let mut m = doc("abc");
        m.move_to(0, 1);
        m.carets.push(Caret::at(0, 2));
        // Move left: (0,1)->(0,0) and (0,2)->(0,1) — distinct, stays 2.
        m.move_ext_multi(DIR_LEFT, false);
        assert_eq!(m.caret_count(), 2);
        // Move left again: (0,0) stays at 0, (0,1)->(0,0) — collide, merge to 1.
        m.move_ext_multi(DIR_LEFT, false);
        assert_eq!(m.caret_count(), 1);
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 0));
    }

    #[test]
    fn esc_collapses_to_primary_and_clears_selection() {
        let mut m = doc("hello world hello");
        m.move_to(0, 0);
        // Ctrl+D: select word, then add next occurrence.
        assert!(m.add_caret_next_occurrence()); // selects "hello"
        assert!(m.add_caret_next_occurrence()); // adds caret on 2nd "hello"
        assert!(m.caret_count() >= 2);
        m.collapse_carets();
        assert_eq!(m.caret_count(), 1);
        assert!(!m.has_selection());
    }

    #[test]
    fn ctrl_d_selects_word_then_adds_next_occurrence_with_wrap() {
        let mut m = doc("foo bar foo baz foo");
        m.move_to(0, 0);
        // 1st Ctrl+D: select "foo" under the caret (no new caret yet).
        assert!(m.add_caret_next_occurrence());
        assert_eq!(m.caret_count(), 1);
        assert_eq!(m.selected_text(), "foo");
        // 2nd: add a caret on the 2nd "foo" (col 8..11).
        assert!(m.add_caret_next_occurrence());
        assert_eq!(m.caret_count(), 2);
        // 3rd: add a caret on the 3rd "foo" (col 16..19).
        assert!(m.add_caret_next_occurrence());
        assert_eq!(m.caret_count(), 3);
        // Every caret holds a "foo" selection.
        for i in 0..m.caret_count() {
            assert!(m.caret_selection(i).is_some());
        }
        // Multi-edit: typing replaces conceptually? We only insert; verify all 3
        // "foo" got a char inserted at the caret (end of each selection).
        m.insert_char_multi('!');
        assert_eq!(m.line(0), "foo! bar foo! baz foo!");
    }

    #[test]
    fn ctrl_d_no_match_returns_false() {
        let mut m = doc("unique stuff here");
        m.move_to(0, 0);
        assert!(m.add_caret_next_occurrence()); // selects "unique"
        // No second "unique" -> no new caret.
        assert!(!m.add_caret_next_occurrence());
        assert_eq!(m.caret_count(), 1);
    }

    #[test]
    fn ctrl_d_wraps_around() {
        let mut m = doc("aa zz aa");
        // Put the cursor on the SECOND "aa" then Ctrl+D twice; the 2nd should
        // wrap to the first occurrence.
        m.move_to(0, 6);
        assert!(m.add_caret_next_occurrence()); // selects 2nd "aa" (6..8)
        assert_eq!(m.selected_text(), "aa");
        assert!(m.add_caret_next_occurrence()); // wraps to 1st "aa" (0..2)
        assert_eq!(m.caret_count(), 2);
        assert_eq!(caret_positions(&m), vec![(0, 2), (0, 8)]);
    }

    #[test]
    fn toggle_caret_adds_and_removes() {
        let mut m = doc("abc\ndef");
        m.move_to(0, 0);
        m.toggle_caret_at(1, 2); // add a 2nd caret
        assert_eq!(m.caret_count(), 2);
        m.toggle_caret_at(1, 2); // toggle it off
        assert_eq!(m.caret_count(), 1);
        // Cannot remove the last caret.
        m.toggle_caret_at(0, 0);
        assert_eq!(m.caret_count(), 1);
    }

    #[test]
    fn caret_accessors_match_legacy_for_single_caret() {
        let mut m = doc("abc");
        m.move_to(0, 2);
        assert_eq!(m.caret_count(), 1);
        assert_eq!(m.caret_at(0), Some((0, 2)));
        assert_eq!(m.caret_at(1), None);
        assert_eq!(m.caret_selection(0), None);
    }
}
