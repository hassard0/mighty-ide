//! On-save text transforms (trim trailing whitespace / ensure a final newline)
//! plus the auto-save debounce clock.
//!
//! These are pure functions over the file *text* so they're trivially testable
//! and reusable from the save path ([`crate::abi::mui_ed_save`]) and the
//! per-frame auto-save tick. Each transform is gated by a Settings toggle
//! ([`crate::settings`]); the save path reads the toggles and calls these.

use std::time::Instant;

/// Strip trailing spaces/tabs from every line, preserving line content and the
/// line structure (including a trailing blank line / final newline, which
/// [`ensure_final_newline`] owns). Operates on `\n`-separated text; a trailing
/// `\n` produces a final empty segment that stays empty. CRLF is preserved by
/// only trimming spaces/tabs (the `\r` is not whitespace we strip here... it is
/// `char::is_whitespace`, so we trim only ' ' and '\t' explicitly).
pub fn trim_trailing_ws(text: &str) -> String {
    // Split keeping the count of segments so we can rejoin with '\n'. The last
    // segment after a trailing '\n' is "" and stays "".
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        // Trim trailing spaces/tabs. Preserve a CRLF terminator: peel a trailing
        // '\r' first, trim the spaces/tabs before it, then re-append the '\r' so
        // we don't silently rewrite line endings.
        let (body, cr) = match line.strip_suffix('\r') {
            Some(b) => (b, true),
            None => (line, false),
        };
        out.push_str(body.trim_end_matches([' ', '\t']));
        if cr {
            out.push('\r');
        }
    }
    out
}

/// Ensure `text` ends with exactly one `\n` (adds one when missing; collapses a
/// run of trailing blank lines to a single newline is NOT done — we only add a
/// newline when the last line has content but no terminator). Empty text stays
/// empty (don't create a file that's just a newline).
pub fn ensure_final_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        return text.to_string();
    }
    let mut s = String::with_capacity(text.len() + 1);
    s.push_str(text);
    s.push('\n');
    s
}

/// Apply the enabled on-save transforms (trim then final-newline) to `text`.
pub fn apply(text: &str, trim: bool, final_nl: bool) -> String {
    let mut s = if trim {
        trim_trailing_ws(text)
    } else {
        text.to_string()
    };
    if final_nl {
        s = ensure_final_newline(&s);
    }
    s
}

/// The auto-save debounce window (ms): save after this much edit-idle.
pub const AUTOSAVE_IDLE_MS: u128 = 1200;

/// Debounce clock for auto-save. The editor calls [`AutoSave::touch`] on every
/// edit (resets the idle timer) and [`AutoSave::due`] each frame; `due` returns
/// `true` exactly once per idle window once the window has elapsed AND there is
/// a pending edit, then disarms until the next `touch`.
#[derive(Debug, Default)]
pub struct AutoSave {
    /// The instant of the most recent edit, or `None` when disarmed (already
    /// saved / never edited).
    last_edit: Option<Instant>,
}

impl AutoSave {
    pub fn new() -> Self {
        AutoSave { last_edit: None }
    }

    /// Record an edit — (re)arms the timer and resets the idle window.
    pub fn touch(&mut self) {
        self.last_edit = Some(Instant::now());
    }

    /// Disarm without firing (e.g. after a manual save).
    pub fn disarm(&mut self) {
        self.last_edit = None;
    }

    /// `true` if armed and the idle window has elapsed. Disarms on a `true`
    /// result so a single idle window fires at most one save.
    pub fn due(&mut self) -> bool {
        self.due_at(Instant::now(), AUTOSAVE_IDLE_MS)
    }

    /// Testable core: `now` is the reference instant, `idle_ms` the window.
    fn due_at(&mut self, now: Instant, idle_ms: u128) -> bool {
        match self.last_edit {
            Some(t) if now.duration_since(t).as_millis() >= idle_ms => {
                self.last_edit = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn trim_removes_trailing_spaces_and_tabs_multiline() {
        let input = "foo   \nbar\t\t\n  baz  \nqux";
        let out = trim_trailing_ws(input);
        assert_eq!(out, "foo\nbar\n  baz\nqux");
    }

    #[test]
    fn trim_preserves_content_and_leading_indent() {
        // Leading indentation + interior whitespace is untouched; only trailing.
        let input = "    indented line\n\ttab-indented\t \nplain";
        let out = trim_trailing_ws(input);
        assert_eq!(out, "    indented line\n\ttab-indented\nplain");
    }

    #[test]
    fn trim_keeps_blank_lines_and_trailing_newline() {
        let input = "a\n   \n\nb\n";
        // Blank/whitespace-only lines become empty; the trailing '\n' (empty
        // final segment) is preserved.
        assert_eq!(trim_trailing_ws(input), "a\n\n\nb\n");
    }

    #[test]
    fn trim_preserves_crlf_endings() {
        let input = "line  \r\nnext\t\r\n";
        // Trailing spaces/tabs before the CR are stripped; CRLF is kept.
        assert_eq!(trim_trailing_ws(input), "line\r\nnext\r\n");
    }

    #[test]
    fn final_newline_added_when_missing() {
        assert_eq!(ensure_final_newline("abc"), "abc\n");
    }

    #[test]
    fn final_newline_not_doubled() {
        assert_eq!(ensure_final_newline("abc\n"), "abc\n");
        // A pre-existing blank trailing line is left as-is (single add only).
        assert_eq!(ensure_final_newline("abc\n\n"), "abc\n\n");
    }

    #[test]
    fn final_newline_leaves_empty_text_empty() {
        assert_eq!(ensure_final_newline(""), "");
    }

    #[test]
    fn apply_composes_both_transforms() {
        let out = apply("a  \nb", true, true);
        assert_eq!(out, "a\nb\n");
        // Each independently gateable.
        assert_eq!(apply("a  \nb", true, false), "a\nb");
        assert_eq!(apply("a  \nb", false, true), "a  \nb\n");
        assert_eq!(apply("a  \nb", false, false), "a  \nb");
    }

    #[test]
    fn autosave_not_due_before_idle_window() {
        let mut a = AutoSave::new();
        a.touch();
        let now = Instant::now();
        // Only 100ms elapsed: not due yet.
        assert!(!a.due_at(now + Duration::from_millis(100), AUTOSAVE_IDLE_MS));
    }

    #[test]
    fn autosave_due_after_idle_window_and_fires_once() {
        let mut a = AutoSave::new();
        a.touch();
        let now = Instant::now();
        let later = now + Duration::from_millis(AUTOSAVE_IDLE_MS as u64 + 50);
        // First check after the window: due.
        assert!(a.due_at(later, AUTOSAVE_IDLE_MS));
        // Disarmed now — a second check does not fire again.
        assert!(!a.due_at(later + Duration::from_millis(10), AUTOSAVE_IDLE_MS));
    }

    #[test]
    fn autosave_not_due_when_never_touched() {
        let mut a = AutoSave::new();
        // Never edited: never due.
        assert!(!a.due_at(Instant::now() + Duration::from_secs(10), AUTOSAVE_IDLE_MS));
    }

    #[test]
    fn autosave_disarm_cancels_pending() {
        let mut a = AutoSave::new();
        a.touch();
        a.disarm();
        assert!(!a.due_at(Instant::now() + Duration::from_secs(5), AUTOSAVE_IDLE_MS));
    }

    #[test]
    fn autosave_touch_rearms_after_fire() {
        let mut a = AutoSave::new();
        a.touch();
        let t0 = Instant::now();
        assert!(a.due_at(t0 + Duration::from_millis(AUTOSAVE_IDLE_MS as u64 + 1), AUTOSAVE_IDLE_MS));
        // New edit re-arms; due again after a fresh window.
        a.touch();
        let t1 = Instant::now();
        assert!(!a.due_at(t1 + Duration::from_millis(50), AUTOSAVE_IDLE_MS));
        assert!(a.due_at(t1 + Duration::from_millis(AUTOSAVE_IDLE_MS as u64 + 1), AUTOSAVE_IDLE_MS));
    }
}
