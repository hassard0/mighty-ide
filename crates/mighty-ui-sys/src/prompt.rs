//! Bottom-prompt input mode + find-search engine (pure, unit-testable).
//!
//! Mighty (v0.36) can't hold or transfer strings (L17: scalar-only FFI), so the
//! one-line prompt buffer and the find machinery live here on the Rust side and
//! are driven by the IDE through the scalar ABI in [`crate::abi`]. Keeping the
//! logic in this module (no GPU/context dependency) makes it directly testable:
//!   * [`PromptState`] — the prompt buffer (open/push/backspace/cancel) and the
//!     goto-line parse (`goto_target`);
//!   * [`FindState`] — a substring search over a buffer that is streamed in byte
//!     by byte from Mighty, exposing per-match `(offset, line, col)`.

/// What a prompt is collecting. Mirrors the scalar `kind` passed to
/// `mui_prompt_open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Go-to-line: the query is a decimal line number.
    Goto = 1,
    /// Find: the query is a substring to search for.
    Find = 2,
}

impl PromptKind {
    pub fn from_i32(k: i32) -> Option<PromptKind> {
        match k {
            1 => Some(PromptKind::Goto),
            2 => Some(PromptKind::Find),
            _ => None,
        }
    }

    /// Human label drawn in front of the query (`Go to line: `, `Find: `).
    pub fn label(self) -> &'static str {
        match self {
            PromptKind::Goto => "Go to line: ",
            PromptKind::Find => "Find: ",
        }
    }
}

/// A one-line bottom prompt. The query is a `Vec<char>` so backspace and
/// per-char readback (needed by the find streaming path) are trivial.
#[derive(Debug, Default)]
pub struct PromptState {
    active: bool,
    kind: Option<PromptKind>,
    query: Vec<char>,
}

impl PromptState {
    pub fn new() -> Self {
        PromptState::default()
    }

    /// Open the prompt for `kind`, clearing any prior query. Unknown kinds are
    /// ignored (the prompt stays closed).
    pub fn open(&mut self, kind: i32) {
        if let Some(k) = PromptKind::from_i32(kind) {
            self.active = true;
            self.kind = Some(k);
            self.query.clear();
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The kind of the active prompt (used by tests; the IDE tracks kind on
    /// the Mighty side from the codepath that opened it).
    #[allow(dead_code)]
    pub fn kind(&self) -> Option<PromptKind> {
        self.kind
    }

    /// Append one Unicode scalar value to the query (ignores invalid/control
    /// codepoints below space except nothing — control filtering is the
    /// caller's job; we accept any real `char`).
    pub fn push(&mut self, codepoint: u32) {
        if !self.active {
            return;
        }
        if let Some(ch) = char::from_u32(codepoint) {
            self.query.push(ch);
        }
    }

    /// Remove the last query char (no-op on an empty query).
    pub fn backspace(&mut self) {
        if self.active {
            self.query.pop();
        }
    }

    /// Close the prompt and clear its query.
    pub fn cancel(&mut self) {
        self.active = false;
        self.kind = None;
        self.query.clear();
    }

    /// Number of chars in the query.
    pub fn len(&self) -> usize {
        self.query.len()
    }

    /// Whether the query is empty. Companion to [`len`](Self::len) (clippy).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// The `i`th query char as a codepoint, or `-1` out of range.
    pub fn char_at(&self, i: usize) -> i32 {
        match self.query.get(i) {
            Some(c) => *c as i32,
            None => -1,
        }
    }

    /// The query as a `String` (used as the find needle + for drawing).
    pub fn query_string(&self) -> String {
        self.query.iter().collect()
    }

    /// The full line to draw at the bottom: label + current query.
    pub fn display_line(&self) -> String {
        match self.kind {
            Some(k) => format!("{}{}", k.label(), self.query_string()),
            None => self.query_string(),
        }
    }

    /// Parse the goto query as a **1-based** line number. Returns the number on
    /// success, or `-1` when the query is empty, not all digits, or overflows
    /// `i32`. Leading/trailing ASCII whitespace is tolerated.
    pub fn goto_target(&self) -> i32 {
        let s: String = self.query_string();
        let t = s.trim();
        if t.is_empty() {
            return -1;
        }
        if !t.bytes().all(|b| b.is_ascii_digit()) {
            return -1;
        }
        match t.parse::<i32>() {
            Ok(n) if n >= 1 => n,
            _ => -1,
        }
    }
}

/// One find match, located in the streamed buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindMatch {
    /// Byte offset of the match start in the buffer.
    pub offset: usize,
    /// 0-based line of the match start.
    pub line: i32,
    /// 0-based column (bytes since the previous '\n') of the match start.
    pub col: i32,
}

/// Substring search over a buffer streamed in byte-by-byte from Mighty.
///
/// Usage: [`reset`](FindState::reset), then [`push_byte`](FindState::push_byte)
/// for each buffer byte, then [`run`](FindState::run) with the needle to compute
/// the match list. Matches are reported in buffer order with their `(offset,
/// line, col)`. The search is plain byte-substring (the editor buffer is a flat
/// byte stream), case-sensitive, and finds **overlapping**-free, left-to-right
/// non-overlapping matches (advance past each hit).
#[derive(Debug, Default)]
pub struct FindState {
    buf: Vec<u8>,
    matches: Vec<FindMatch>,
}

impl FindState {
    pub fn new() -> Self {
        FindState::default()
    }

    /// Clear the streamed buffer and any prior matches.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.matches.clear();
    }

    /// Append one byte to the search buffer (low 8 bits of `byte`).
    pub fn push_byte(&mut self, byte: u32) {
        self.buf.push((byte & 0xff) as u8);
    }

    /// Run the search for `needle` (UTF-8 bytes), storing matches. Returns the
    /// match count. An empty needle matches nothing (count 0). Matches do not
    /// overlap: after a hit at `i` the scan resumes at `i + needle.len()`.
    pub fn run(&mut self, needle: &str) -> i32 {
        self.matches.clear();
        let needle = needle.as_bytes();
        if needle.is_empty() || needle.len() > self.buf.len() {
            return 0;
        }
        let hay = &self.buf;
        let last = hay.len() - needle.len();
        let mut i = 0;
        while i <= last {
            if &hay[i..i + needle.len()] == needle {
                let (line, col) = self.line_col_at(i);
                self.matches.push(FindMatch {
                    offset: i,
                    line,
                    col,
                });
                i += needle.len();
            } else {
                i += 1;
            }
        }
        self.matches.len() as i32
    }

    /// 0-based (line, col) of byte offset `at` in the buffer.
    fn line_col_at(&self, at: usize) -> (i32, i32) {
        let mut line = 0i32;
        let mut col = 0i32;
        for &b in &self.buf[..at.min(self.buf.len())] {
            if b == b'\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    pub fn count(&self) -> i32 {
        self.matches.len() as i32
    }

    pub fn get(&self, i: usize) -> Option<FindMatch> {
        self.matches.get(i).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- PromptState ----

    #[test]
    fn prompt_open_push_backspace_cancel() {
        let mut p = PromptState::new();
        assert!(!p.is_active());
        p.open(PromptKind::Goto as i32);
        assert!(p.is_active());
        assert_eq!(p.kind(), Some(PromptKind::Goto));

        p.push(b'4' as u32);
        p.push(b'2' as u32);
        assert_eq!(p.len(), 2);
        assert_eq!(p.query_string(), "42");
        assert_eq!(p.char_at(0), b'4' as i32);
        assert_eq!(p.char_at(1), b'2' as i32);
        assert_eq!(p.char_at(2), -1);

        p.backspace();
        assert_eq!(p.query_string(), "4");
        p.backspace();
        p.backspace(); // no-op on empty
        assert!(p.is_empty());

        p.cancel();
        assert!(!p.is_active());
        assert_eq!(p.kind(), None);
        // push after cancel is ignored.
        p.push(b'9' as u32);
        assert!(p.is_empty());
    }

    #[test]
    fn prompt_open_clears_prior_query() {
        let mut p = PromptState::new();
        p.open(PromptKind::Find as i32);
        p.push(b'x' as u32);
        assert_eq!(p.query_string(), "x");
        p.open(PromptKind::Goto as i32);
        assert!(p.is_empty());
        assert_eq!(p.kind(), Some(PromptKind::Goto));
    }

    #[test]
    fn prompt_ignores_unknown_kind() {
        let mut p = PromptState::new();
        p.open(99);
        assert!(!p.is_active());
        p.open(0);
        assert!(!p.is_active());
    }

    #[test]
    fn goto_target_parses_valid_and_rejects_garbage() {
        let mut p = PromptState::new();
        p.open(PromptKind::Goto as i32);
        // empty -> -1
        assert_eq!(p.goto_target(), -1);
        for c in "42".bytes() {
            p.push(c as u32);
        }
        assert_eq!(p.goto_target(), 42);

        // non-digit -> -1
        p.open(PromptKind::Goto as i32);
        for c in "4a".bytes() {
            p.push(c as u32);
        }
        assert_eq!(p.goto_target(), -1);

        // leading zero ok (still a number)
        p.open(PromptKind::Goto as i32);
        for c in "007".bytes() {
            p.push(c as u32);
        }
        assert_eq!(p.goto_target(), 7);

        // huge overflow -> -1
        p.open(PromptKind::Goto as i32);
        for c in "99999999999999".bytes() {
            p.push(c as u32);
        }
        assert_eq!(p.goto_target(), -1);
    }

    #[test]
    fn display_line_includes_label_and_query() {
        let mut p = PromptState::new();
        p.open(PromptKind::Goto as i32);
        for c in "12".bytes() {
            p.push(c as u32);
        }
        assert_eq!(p.display_line(), "Go to line: 12");
        p.open(PromptKind::Find as i32);
        for c in "foo".bytes() {
            p.push(c as u32);
        }
        assert_eq!(p.display_line(), "Find: foo");
    }

    // ---- FindState ----

    /// Stream `text` into a fresh FindState (bytes), search `needle`, return it.
    fn search(text: &str, needle: &str) -> FindState {
        let mut f = FindState::new();
        f.reset();
        for b in text.as_bytes() {
            f.push_byte(*b as u32);
        }
        f.run(needle);
        f
    }

    #[test]
    fn find_single_match_offset_line_col() {
        // line 0: "hello world"
        // line 1: "find me here"
        let text = "hello world\nfind me here\n";
        let f = search(text, "me");
        assert_eq!(f.count(), 1);
        let m = f.get(0).unwrap();
        // "me" starts at index 5 of line 1 ("find me").
        assert_eq!(m.line, 1);
        assert_eq!(m.col, 5);
        // byte offset = len("hello world\n") + 5 = 12 + 5 = 17.
        assert_eq!(m.offset, 17);
    }

    #[test]
    fn find_multiple_matches_in_order() {
        let text = "abc abc\nxabcx";
        let f = search(text, "abc");
        assert_eq!(f.count(), 3);
        let m0 = f.get(0).unwrap();
        assert_eq!((m0.line, m0.col, m0.offset), (0, 0, 0));
        let m1 = f.get(1).unwrap();
        assert_eq!((m1.line, m1.col, m1.offset), (0, 4, 4));
        let m2 = f.get(2).unwrap();
        // line 1, after the leading 'x' -> col 1, offset 8 + 1 = 9
        assert_eq!((m2.line, m2.col, m2.offset), (1, 1, 9));
    }

    #[test]
    fn find_non_overlapping() {
        // "aaaa" with needle "aa" -> matches at 0 and 2 (non-overlapping).
        let f = search("aaaa", "aa");
        assert_eq!(f.count(), 2);
        assert_eq!(f.get(0).unwrap().offset, 0);
        assert_eq!(f.get(1).unwrap().offset, 2);
    }

    #[test]
    fn find_no_match_and_empty_needle() {
        let f = search("hello", "zzz");
        assert_eq!(f.count(), 0);
        assert!(f.get(0).is_none());

        let f2 = search("hello", "");
        assert_eq!(f2.count(), 0);
    }

    #[test]
    fn find_needle_longer_than_buffer() {
        let f = search("hi", "hello");
        assert_eq!(f.count(), 0);
    }

    #[test]
    fn find_reset_clears_prior_state() {
        let mut f = FindState::new();
        for b in "abcabc".as_bytes() {
            f.push_byte(*b as u32);
        }
        assert_eq!(f.run("abc"), 2);
        f.reset();
        // After reset the buffer is empty -> no matches.
        assert_eq!(f.run("abc"), 0);
        assert_eq!(f.count(), 0);
    }

    #[test]
    fn find_case_sensitive() {
        let f = search("Hello hello", "hello");
        // Only the lowercase one matches.
        assert_eq!(f.count(), 1);
        assert_eq!(f.get(0).unwrap().col, 6);
    }
}
