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

    // -----------------------------------------------------------------------
    // Selection-aware motion (Shift+motion extends; plain motion collapses)
    // -----------------------------------------------------------------------

    /// If there is no anchor, drop one at the current cursor (begin selecting).
    fn begin_or_keep_anchor(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some((self.cur_line, self.cur_col));
        }
    }

    /// Move the cursor one step in `dir`, extending the selection when `extend`
    /// is set (Shift held) or collapsing it otherwise. The motion itself mirrors
    /// [`move_cursor`]; only the anchor handling differs.
    pub fn move_cursor_ext(&mut self, dir: i32, extend: bool) {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.anchor = None;
        }
        self.step(dir);
    }

    /// Pure cursor step in `dir` with NO anchor side effects (used by both the
    /// plain and extending motions).
    fn step(&mut self, dir: i32) {
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

    /// Smart Home: first press moves to the first non-whitespace char of the
    /// line; if already there (or before it), moves to column 0. Optionally
    /// extends the selection. Returns the resulting column.
    pub fn home_smart(&mut self, extend: bool) -> usize {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.anchor = None;
        }
        let chars: Vec<char> = self.lines[self.cur_line].chars().collect();
        let first_non_ws = chars
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(chars.len());
        self.cur_col = if self.cur_col == first_non_ws {
            0
        } else {
            first_non_ws
        };
        self.cur_col
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
            self.anchor = None;
        }
        if self.cur_col == 0 {
            // Hop to the end of the previous line.
            if self.cur_line > 0 {
                self.cur_line -= 1;
                self.cur_col = self.line_len(self.cur_line);
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.cur_line].chars().collect();
        let mut i = self.cur_col;
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
        self.cur_col = i;
    }

    /// Move one "word" right (skip a run of word chars then whitespace), or to
    /// the start of the next line. Optionally extends selection.
    pub fn move_word_right(&mut self, extend: bool) {
        if extend {
            self.begin_or_keep_anchor();
        } else {
            self.anchor = None;
        }
        let chars: Vec<char> = self.lines[self.cur_line].chars().collect();
        let len = chars.len();
        if self.cur_col >= len {
            // Hop to the start of the next line.
            if self.cur_line + 1 < self.lines.len() {
                self.cur_line += 1;
                self.cur_col = 0;
            }
            return;
        }
        let mut i = self.cur_col;
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
        self.cur_col = i;
    }

    /// Select the word under the cursor (sets the anchor at its start and the
    /// cursor at its end). No-op (clears selection) if not on a word char.
    /// Returns the selected text.
    pub fn select_word(&mut self) -> String {
        let chars: Vec<char> = self.lines[self.cur_line].chars().collect();
        let len = chars.len();
        // Find the word boundaries around (or just before) the cursor.
        let mut s = self.cur_col.min(len);
        // If sitting just past a word char, step back onto it.
        if s == len && s > 0 && is_word_char(chars[s - 1]) {
            s -= 1;
        }
        if s >= len || !is_word_char(chars[s]) {
            // Try the char to the left.
            if self.cur_col > 0 && is_word_char(chars[self.cur_col - 1]) {
                s = self.cur_col - 1;
            } else {
                self.anchor = None;
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
        self.anchor = Some((self.cur_line, start));
        self.cur_col = end;
        chars[start..end].iter().collect()
    }

    /// Select the whole current line (anchor at col 0, cursor at end of line —
    /// or start of the next line so a trailing newline is included when there is
    /// one). Used by Ctrl+L-style line select / as the basis for duplicate.
    pub fn select_line(&mut self) {
        self.anchor = Some((self.cur_line, 0));
        self.cur_col = self.line_len(self.cur_line);
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
            None => (self.cur_line, self.cur_line),
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
        let li = self.cur_line;
        let col = self.cur_col;
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
            self.cur_line = li + 1;
            self.cur_col = inner.chars().count();
        } else if opens {
            let new_line = format!("{indent}{one}{tail}");
            let caret = format!("{indent}{one}").chars().count();
            self.lines.insert(li + 1, new_line);
            self.cur_line = li + 1;
            self.cur_col = caret;
        } else if closes_next && !indent.is_empty() {
            // New line is (or starts with) `}`: dedent one level.
            let dedent: String = indent.chars().skip(one.len()).collect();
            let new_line = format!("{dedent}{tail}");
            self.lines.insert(li + 1, new_line);
            self.cur_line = li + 1;
            self.cur_col = dedent.chars().count();
        } else {
            let new_line = format!("{indent}{tail}");
            self.lines.insert(li + 1, new_line);
            self.cur_line = li + 1;
            self.cur_col = indent.chars().count();
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
        self.lines[self.cur_line].chars().nth(self.cur_col)
    }

    /// The char immediately to the left of the cursor, or `None` at col 0.
    fn char_before(&self) -> Option<char> {
        if self.cur_col == 0 {
            return None;
        }
        self.lines[self.cur_line].chars().nth(self.cur_col - 1)
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
            self.cur_col += 1;
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
            let li = self.cur_line;
            let col = self.cur_col;
            let (head, tail) = Self::split_line(&self.lines[li], col);
            self.lines[li] = format!("{head}{ch}{close}{tail}");
            self.cur_col = col + 1; // between the pair
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
                let li = self.cur_line;
                let col = self.cur_col;
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
                self.cur_col = col - 1;
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
            if let Some(m) = self.match_from(self.cur_line, self.cur_col, c) {
                return Some(m);
            }
        }
        // Then the bracket just to the left.
        if self.cur_col > 0 {
            if let Some(c) = self.char_before() {
                if let Some(m) = self.match_from(self.cur_line, self.cur_col - 1, c) {
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
        self.cur_line += n;
        if let Some((al, ac)) = self.anchor {
            self.anchor = Some((al + n, ac));
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
        self.cur_line -= 1;
        if let Some((al, ac)) = self.anchor {
            self.anchor = Some((al.saturating_sub(1), ac));
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
        self.cur_line += 1;
        if let Some((al, ac)) = self.anchor {
            self.anchor = Some((al + 1, ac));
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
        let from = (self.cur_line, self.cur_col);
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
            self.anchor = None;
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
        self.anchor = None;
        let chars: Vec<char> = self.lines[line].chars().collect();
        let needle_chars = needle.chars().count();
        let head: String = chars[..col].iter().collect();
        let tail: String = chars[(col + needle_chars).min(chars.len())..].iter().collect();
        self.lines[line] = format!("{head}{repl}{tail}");
        self.cur_line = line;
        self.cur_col = col + repl.chars().count();
        self.clamp_col();
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
        m.anchor = Some((0, 0));
        m.cur_line = 2;
        m.cur_col = 1;
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
        m.anchor = Some((0, 0));
        m.cur_line = 1;
        m.cur_col = 1;
        m.toggle_line_comment();
        assert_eq!(m.line(0), "// // a");
        assert_eq!(m.line(1), "// b");
    }

    #[test]
    fn comment_toggle_skips_blank_lines() {
        let mut m = doc("a\n\nb");
        m.anchor = Some((0, 0));
        m.cur_line = 2;
        m.cur_col = 1;
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
        m.anchor = Some((0, 0));
        m.cur_line = 1;
        m.cur_col = 1;
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
}
