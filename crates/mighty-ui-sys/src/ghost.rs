//! Inline AI ghost-text completions (Copilot-style) — shim-side engine.
//!
//! Like every other capability in this IDE, ALL the logic lives shim-side and is
//! exposed to Mighty via the scalar `mui_ghost_*` ABI (see [`crate::ghostabi`]).
//! The editor text model ([`crate::editor::TextModel`]) is the source of truth;
//! Mighty drives this engine each frame.
//!
//! ## How it fires (cost discipline)
//!
//! Inline completions are EXPENSIVE, so they are gated hard:
//!   * **Debounced.** After an edit, Mighty calls [`GhostState::arm`] to start a
//!     ~450ms idle timer. Only [`GhostState::tick`] firing AFTER the timer
//!     elapses (with no intervening edit) sends a request. Typing/moving re-arms
//!     it, so a fast typist never fires.
//!   * **One in flight.** A request only starts when none is outstanding (a
//!     `running` flag), so a burst can never stack requests.
//!   * **Capped.** `max_tokens` is small ([`MAX_TOKENS`]) and the prompt windows
//!     the prefix/suffix to a bounded number of lines.
//!   * **Cancelled aggressively.** Every request carries a monotonically
//!     increasing **generation id**. Any edit / cursor move / dismiss bumps the
//!     generation; a background response whose id no longer matches is dropped on
//!     the floor ([`GhostState::poll`]). So a stale completion never appears.
//!   * **Disabled without a key.** With no `ANTHROPIC_API_KEY`, the engine never
//!     fires and never errors (the `inline_ai` setting is effectively off).
//!
//! ## Fill-in-the-middle prompt
//!
//! [`build_fim_prompt`] sends the text BEFORE the cursor (prefix, capped lines)
//! plus the text AFTER (suffix, capped lines), the language, and the filename,
//! with a system instruction telling the model to return ONLY the insertion. The
//! response is run through [`strip_fences`] so a stray ```` ``` ```` fence the
//! model adds anyway is removed.
//!
//! The request itself runs on a background thread (single non-streaming call) so
//! the UI never blocks; the result lands in a shared slot the UI polls each
//! frame, exactly like the chat copilot's pump discipline.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ai::{api_key, MODEL};

/// Conservative output cap for an inline completion (cost discipline).
pub const MAX_TOKENS: u32 = 120;

/// Idle time after the last edit before a debounced request fires.
pub const DEBOUNCE: Duration = Duration::from_millis(450);

/// How many lines of context before the cursor to send (prefix window).
pub const PREFIX_LINES: usize = 80;
/// How many lines of context after the cursor to send (suffix window).
pub const SUFFIX_LINES: usize = 30;

/// Anthropic Messages endpoint (same as the chat copilot).
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The system instruction that turns the model into a pure completion engine.
pub const SYSTEM: &str = "You are a code completion engine. Return ONLY the text \
    to insert at the cursor to continue the code — no explanation, no markdown \
    fences, no surrounding prose. Continue exactly from the cursor position.";

// ===========================================================================
// Cursor context + prompt building (pure + testable, no network)
// ===========================================================================

/// A snapshot of the editor needed to build a completion request: the full text,
/// the 0-based cursor `(line, col)`, the language display name, and the filename.
#[derive(Debug, Clone)]
pub struct Context {
    pub text: String,
    pub cur_line: usize,
    pub cur_col: usize,
    pub language: String,
    pub file_name: String,
}

/// Split a document into `(prefix, suffix)` around the cursor, where `prefix` is
/// everything from the start up to and including the cursor column on its line,
/// and `suffix` is the rest. Columns are char offsets. Pure + testable.
pub fn split_at_cursor(text: &str, cur_line: usize, cur_col: usize) -> (String, String) {
    let lines: Vec<&str> = text.split('\n').collect();
    let cur_line = cur_line.min(lines.len().saturating_sub(1));
    let mut prefix = String::new();
    let mut suffix = String::new();
    for (i, line) in lines.iter().enumerate() {
        use std::cmp::Ordering as Ord;
        match i.cmp(&cur_line) {
            Ord::Less => {
                prefix.push_str(line);
                prefix.push('\n');
            }
            Ord::Equal => {
                let chars: Vec<char> = line.chars().collect();
                let col = cur_col.min(chars.len());
                prefix.extend(chars[..col].iter());
                suffix.extend(chars[col..].iter());
            }
            Ord::Greater => {
                suffix.push('\n');
                suffix.push_str(line);
            }
        }
    }
    (prefix, suffix)
}

/// Keep at most the LAST `n` lines of `s` (the lines nearest the cursor).
fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.split('\n').collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Keep at most the FIRST `n` lines of `s` (the lines nearest the cursor).
fn first_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.split('\n').collect();
    let end = lines.len().min(n);
    lines[..end].join("\n")
}

/// Build the single user-message string for a fill-in-the-middle completion: the
/// windowed prefix + suffix wrapped in explicit markers, plus the language +
/// filename, so the model knows exactly where to continue. Pure + testable.
pub fn build_fim_prompt(ctx: &Context) -> String {
    let (prefix, suffix) = split_at_cursor(&ctx.text, ctx.cur_line, ctx.cur_col);
    let prefix = last_lines(&prefix, PREFIX_LINES);
    let suffix = first_lines(&suffix, SUFFIX_LINES);
    format!(
        "Language: {lang}\nFile: {file}\n\n\
         Complete the code at the cursor (<CURSOR>). Return only the text to \
         insert there.\n\n\
         <PREFIX>\n{prefix}<CURSOR>{suffix}\n</SUFFIX>",
        lang = ctx.language,
        file = ctx.file_name,
    )
}

/// Serialize a non-streaming Anthropic Messages body for a completion. Pure +
/// testable (no network).
pub fn anthropic_body(user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM,
        "stream": false,
        "messages": [ { "role": "user", "content": user } ],
    })
}

/// Strip a stray Markdown code fence the model may have wrapped the completion in
/// despite the instruction. Removes a leading ```` ```lang ```` line and a
/// trailing ```` ``` ```` line; otherwise returns the text unchanged. Also trims
/// a single leading newline introduced by the opening fence. Pure + testable.
pub fn strip_fences(s: &str) -> String {
    let trimmed = s.trim_matches('\n');
    // Whole thing fenced: ```...\n<body>\n```
    if trimmed.starts_with("```") {
        // Drop the first line (the ```lang opener) and a trailing ``` line.
        let mut lines: Vec<&str> = trimmed.split('\n').collect();
        if !lines.is_empty() {
            lines.remove(0);
        }
        if lines.last().map(|l| l.trim_end()) == Some("```") {
            lines.pop();
        }
        return lines.join("\n");
    }
    s.to_string()
}

// ===========================================================================
// Background request slot (thread → UI)
// ===========================================================================

/// The shared result slot a background request fills. The generation `gen` is
/// captured at request time; [`GhostState::poll`] ignores a result whose `gen`
/// no longer matches the live generation (a stale/cancelled response).
#[derive(Default)]
struct ResultSlot {
    /// The completion text (fences stripped), once it arrives.
    text: Option<String>,
    /// The generation this result was requested under.
    gen: u64,
    /// Set true once the thread finished (success or error). Errors are silent —
    /// inline completion never surfaces an error to the user.
    done: bool,
}

#[derive(Clone, Default)]
struct Shared {
    inner: Arc<Mutex<ResultSlot>>,
    running: Arc<AtomicBool>,
}

// ===========================================================================
// GhostState — the engine the ABI drives
// ===========================================================================

/// The inline ghost-text engine for the active editor.
pub struct GhostState {
    /// The pending suggestion to render (None when no ghost is shown).
    suggestion: Option<String>,
    /// Anchor `(line, col)` where the ghost is shown — the cursor at accept time.
    anchor: (usize, usize),
    /// Monotonic generation id. Bumped on every edit / move / dismiss so a stale
    /// background response is dropped.
    generation: u64,
    /// When `Some(t)`, a debounced request is scheduled to fire once `t` elapses
    /// with no further edit. Cleared when it fires or is cancelled.
    armed_at: Option<Instant>,
    /// The active background request slot, if one is in flight.
    shared: Option<Shared>,
    /// The generation of the in-flight request (so poll can match).
    inflight_gen: u64,
    /// Screenshot/demo hook: a seeded fake ghost so a headless capture renders
    /// the dim overlay without a live call.
    pub force_demo: bool,
}

impl Default for GhostState {
    fn default() -> Self {
        Self::new()
    }
}

impl GhostState {
    pub fn new() -> Self {
        GhostState {
            suggestion: None,
            anchor: (0, 0),
            generation: 0,
            armed_at: None,
            shared: None,
            inflight_gen: 0,
            force_demo: false,
        }
    }

    /// `true` when inline AI is enabled: the `inline_ai` setting is on AND an API
    /// key is present. With no key it is effectively off (never fires, no error).
    pub fn enabled() -> bool {
        crate::settings::inline_ai() && api_key().is_some()
    }

    /// `true` if a ghost suggestion is currently being shown.
    pub fn has_ghost(&self) -> bool {
        self.suggestion.is_some()
    }

    /// The current suggestion text, if any.
    pub fn suggestion(&self) -> Option<&str> {
        self.suggestion.as_deref()
    }

    /// The ghost anchor `(line, col)`.
    pub fn anchor(&self) -> (usize, usize) {
        self.anchor
    }

    /// `true` if a background request is in flight.
    pub fn is_inflight(&self) -> bool {
        self.shared
            .as_ref()
            .map(|s| s.running.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Bump the generation id, invalidating any in-flight request's result.
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Schedule a debounced request `DEBOUNCE` from now. Called by Mighty after an
    /// edit when inline AI is enabled. Clears any shown ghost (it's now stale) and
    /// bumps the generation so an outstanding request can't land late. No-op when
    /// disabled (no key / setting off).
    pub fn arm(&mut self) {
        if !Self::enabled() {
            self.dismiss();
            return;
        }
        self.suggestion = None;
        self.bump_generation();
        self.armed_at = Some(Instant::now() + DEBOUNCE);
    }

    /// Like [`arm`] but with an explicit deadline — used by tests to avoid real
    /// sleeps. Production calls [`arm`].
    #[cfg(test)]
    pub fn arm_at(&mut self, deadline: Instant) {
        self.suggestion = None;
        self.bump_generation();
        self.armed_at = Some(deadline);
    }

    /// Dismiss the current ghost + cancel any pending/scheduled request by bumping
    /// the generation. Called on Escape / any edit / cursor move.
    pub fn dismiss(&mut self) {
        self.suggestion = None;
        self.armed_at = None;
        self.bump_generation();
    }

    /// Force an immediate request (the Alt+\ explicit trigger), bypassing the
    /// debounce. No-op when disabled or a request is already in flight.
    pub fn force(&mut self, ctx: Context) -> bool {
        if !Self::enabled() || self.is_inflight() {
            return false;
        }
        self.suggestion = None;
        self.armed_at = None;
        self.bump_generation();
        self.start_request(ctx)
    }

    /// Called each frame. If a debounced request is due (the deadline elapsed) and
    /// no request is in flight, fire it (using `ctx_fn` to snapshot the editor
    /// lazily, only when actually firing). Returns `true` if a request was
    /// started this tick.
    pub fn tick(&mut self, now: Instant, ctx_fn: impl FnOnce() -> Context) -> bool {
        let Some(deadline) = self.armed_at else {
            return false;
        };
        if now < deadline {
            return false;
        }
        // Deadline reached: clear the timer regardless of outcome.
        self.armed_at = None;
        if !Self::enabled() || self.is_inflight() {
            return false;
        }
        self.start_request(ctx_fn())
    }

    /// Spawn the background request for `ctx` under the current generation.
    fn start_request(&mut self, ctx: Context) -> bool {
        let Some(key) = api_key() else {
            return false;
        };
        let gen = self.generation;
        let shared = Shared::default();
        shared.running.store(true, Ordering::SeqCst);
        self.shared = Some(shared.clone());
        self.inflight_gen = gen;

        let user = build_fim_prompt(&ctx);
        std::thread::spawn(move || {
            let text = run_completion(&key, &user);
            if let Ok(mut g) = shared.inner.lock() {
                g.text = text.map(|t| strip_fences(&t));
                g.gen = gen;
                g.done = true;
            }
            shared.running.store(false, Ordering::SeqCst);
        });
        true
    }

    /// Drain a finished background result into the ghost suggestion. Returns `1`
    /// (as a bool) only when a fresh, non-empty, non-stale suggestion became
    /// available this frame. A result whose generation no longer matches the live
    /// generation (the user edited/moved since) is dropped silently.
    ///
    /// `anchor` is the CURRENT cursor `(line, col)`; we only adopt the suggestion
    /// if the cursor hasn't moved off the request position class (the generation
    /// check already guards this, but we record the anchor for rendering).
    pub fn poll(&mut self, anchor: (usize, usize)) -> bool {
        let Some(shared) = self.shared.clone() else {
            return false;
        };
        let (text, gen, done) = {
            let Ok(mut g) = shared.inner.lock() else {
                return false;
            };
            if !g.done {
                return false;
            }
            (g.text.take(), g.gen, g.done)
        };
        if done {
            self.shared = None;
        }
        // Stale? The generation moved on (edit/move/dismiss since the request).
        if gen != self.generation {
            return false;
        }
        match text {
            Some(t) if !t.is_empty() => {
                self.suggestion = Some(t);
                self.anchor = anchor;
                true
            }
            _ => false,
        }
    }

    /// Accept the FULL suggestion: returns the text to insert at the cursor and
    /// clears the ghost. The caller (ABI) inserts it via the editor path. Returns
    /// `None` when there is no ghost.
    pub fn accept(&mut self) -> Option<String> {
        let s = self.suggestion.take()?;
        self.bump_generation();
        self.armed_at = None;
        Some(s)
    }

    /// Partial accept: take ONE word (a run of whitespace + the next word) off the
    /// front of the suggestion, returning that fragment to insert. The remaining
    /// suggestion stays as the ghost (re-anchored by the caller). Returns `None`
    /// when there is no ghost.
    pub fn accept_word(&mut self) -> Option<String> {
        let s = self.suggestion.take()?;
        let (word, rest) = split_first_word(&s);
        if rest.is_empty() {
            // Last word — accepting it clears the ghost.
            self.bump_generation();
            self.armed_at = None;
        } else {
            // Keep the remainder as the live ghost.
            self.suggestion = Some(rest);
        }
        Some(word)
    }

    /// Advance the anchor by the inserted `text` (used after a partial accept so
    /// the remaining ghost renders at the new cursor). The caller passes the post
    /// insert cursor.
    pub fn set_anchor(&mut self, anchor: (usize, usize)) {
        self.anchor = anchor;
    }

    /// Seed a fake ghost for a headless screenshot (`MUI_GHOST_AUTOOPEN`). Bypasses
    /// the network + the enabled gate so the dim overlay renders for capture.
    pub fn seed_demo(&mut self, suggestion: &str, anchor: (usize, usize)) {
        self.suggestion = Some(suggestion.to_string());
        self.anchor = anchor;
        self.force_demo = true;
    }
}

/// Split off the leading word (any leading whitespace + the following run of
/// non-whitespace) from `s`, returning `(word, rest)`. A multi-line suggestion's
/// first "word" never crosses a newline: if the leading whitespace contains a
/// `\n`, only up to and including the newline is taken.
fn split_first_word(s: &str) -> (String, String) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    // Leading whitespace. Stop right after a newline (a partial accept of a
    // blank-leading multi-line suggestion advances one line at a time).
    while i < chars.len() && chars[i].is_whitespace() {
        let nl = chars[i] == '\n';
        i += 1;
        if nl {
            break;
        }
    }
    // The word run (non-whitespace).
    if i < chars.len() && !chars[i].is_whitespace() {
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
    }
    let word: String = chars[..i].iter().collect();
    let rest: String = chars[i..].iter().collect();
    (word, rest)
}

/// Run a single non-streaming completion request. Returns the model's text on a
/// 2xx, or `None` on any error (no-key, non-2xx, network, parse) — inline
/// completion is best-effort and silent on failure.
fn run_completion(key: &str, user: &str) -> Option<String> {
    let body = anthropic_body(user);
    let resp = ureq::post(ENDPOINT)
        .set("x-api-key", key)
        .set("anthropic-version", ANTHROPIC_VERSION)
        .set("content-type", "application/json")
        .send_json(body)
        .ok()?;
    let v: serde_json::Value = resp.into_json().ok()?;
    // content: [ { type: "text", text: "..." }, ... ]
    let text = v
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(text: &str, line: usize, col: usize) -> Context {
        Context {
            text: text.to_string(),
            cur_line: line,
            cur_col: col,
            language: "Mighty".to_string(),
            file_name: "main.mty".to_string(),
        }
    }

    // ---- FIM prefix/suffix windowing ----

    #[test]
    fn split_at_cursor_basic() {
        let (p, s) = split_at_cursor("ab\ncd\nef", 1, 1);
        assert_eq!(p, "ab\nc");
        assert_eq!(s, "d\nef");
    }

    #[test]
    fn split_at_cursor_end_of_doc() {
        let (p, s) = split_at_cursor("foo\nbar", 1, 3);
        assert_eq!(p, "foo\nbar");
        assert_eq!(s, "");
    }

    #[test]
    fn split_at_cursor_start_of_doc() {
        let (p, s) = split_at_cursor("foo\nbar", 0, 0);
        assert_eq!(p, "");
        assert_eq!(s, "foo\nbar");
    }

    #[test]
    fn split_at_cursor_unicode_cols() {
        // é is one char; cursor after it.
        let (p, s) = split_at_cursor("café", 0, 4);
        assert_eq!(p, "café");
        assert_eq!(s, "");
        let (p2, s2) = split_at_cursor("café", 0, 3);
        assert_eq!(p2, "caf");
        assert_eq!(s2, "é");
    }

    #[test]
    fn fim_prompt_windows_prefix_and_suffix() {
        // Build a doc with more than PREFIX_LINES lines before the cursor.
        let mut doc = String::new();
        for i in 0..200 {
            doc.push_str(&format!("line{i}\n"));
        }
        doc.push_str("HERE");
        let total_lines = doc.split('\n').count();
        let c = ctx(&doc, total_lines - 1, 4);
        let prompt = build_fim_prompt(&c);
        assert!(prompt.contains("Language: Mighty"));
        assert!(prompt.contains("File: main.mty"));
        assert!(prompt.contains("<CURSOR>"));
        // The prefix is windowed to the LAST PREFIX_LINES lines — far-earlier
        // lines must be dropped.
        assert!(!prompt.contains("line0\n"), "early lines should be windowed out");
        assert!(prompt.contains("line199"), "recent lines kept");
        assert!(prompt.contains("HERE<CURSOR>"));
    }

    #[test]
    fn fim_suffix_windowed() {
        let mut doc = String::from("X");
        for i in 0..100 {
            doc.push_str(&format!("\ntail{i}"));
        }
        let c = ctx(&doc, 0, 1); // cursor right after "X" on line 0
        let prompt = build_fim_prompt(&c);
        // Only the first SUFFIX_LINES suffix lines are kept.
        assert!(prompt.contains("tail0"));
        assert!(!prompt.contains("tail99"), "far suffix windowed out");
    }

    // ---- fence stripping ----

    #[test]
    fn strip_fences_removes_wrapping_fence() {
        let r = strip_fences("```rust\nlet x = 1;\n```");
        assert_eq!(r, "let x = 1;");
    }

    #[test]
    fn strip_fences_no_fence_unchanged() {
        assert_eq!(strip_fences("let x = 1;"), "let x = 1;");
        assert_eq!(strip_fences(".push(x)"), ".push(x)");
    }

    #[test]
    fn strip_fences_bare_open_no_lang() {
        let r = strip_fences("```\nfoo()\n```");
        assert_eq!(r, "foo()");
    }

    #[test]
    fn strip_fences_keeps_inline_backticks() {
        // Backticks NOT at the very start are left alone.
        assert_eq!(strip_fences("a `b` c"), "a `b` c");
    }

    // ---- debounce + generation-id cancel ----

    #[test]
    fn arm_then_tick_before_deadline_does_not_fire() {
        let _g = settings_guard_inline_on();
        let mut gh = GhostState::new();
        let now = Instant::now();
        gh.arm_at(now + Duration::from_millis(450));
        // Tick before the deadline: no fire (no key here anyway, but the timer
        // gate is what we assert — armed_at stays set).
        let fired = gh.tick(now, || unreachable!("ctx_fn must not be called early"));
        assert!(!fired);
        assert!(gh.armed_at.is_some(), "still armed before the deadline");
    }

    #[test]
    fn tick_after_deadline_clears_timer() {
        let _g = settings_guard_inline_on();
        let mut gh = GhostState::new();
        let now = Instant::now();
        gh.arm_at(now);
        // At/after the deadline the timer is consumed even if no key (no request).
        let _ = gh.tick(now + Duration::from_millis(1), || ctx("x", 0, 1));
        assert!(gh.armed_at.is_none(), "timer cleared once the deadline passes");
    }

    #[test]
    fn stale_response_is_ignored() {
        let mut gh = GhostState::new();
        // Simulate an in-flight request at generation G.
        let shared = Shared::default();
        shared.running.store(false, Ordering::SeqCst);
        let req_gen = gh.generation;
        {
            let mut slot = shared.inner.lock().unwrap();
            slot.text = Some("completion".to_string());
            slot.gen = req_gen;
            slot.done = true;
        }
        gh.shared = Some(shared);
        // The user edited since: generation advanced.
        gh.bump_generation();
        // Poll: the result's gen (req_gen) != current generation -> dropped.
        let got = gh.poll((0, 0));
        assert!(!got, "stale-generation result must be ignored");
        assert!(!gh.has_ghost());
    }

    #[test]
    fn fresh_response_becomes_ghost() {
        let mut gh = GhostState::new();
        let shared = Shared::default();
        shared.running.store(false, Ordering::SeqCst);
        let gen = gh.generation;
        {
            let mut slot = shared.inner.lock().unwrap();
            slot.text = Some(".push(x)".to_string());
            slot.gen = gen;
            slot.done = true;
        }
        gh.shared = Some(shared);
        let got = gh.poll((2, 5));
        assert!(got);
        assert!(gh.has_ghost());
        assert_eq!(gh.suggestion(), Some(".push(x)"));
        assert_eq!(gh.anchor(), (2, 5));
    }

    #[test]
    fn dismiss_bumps_generation_and_clears() {
        let mut gh = GhostState::new();
        gh.suggestion = Some("x".to_string());
        let before = gh.generation;
        gh.dismiss();
        assert!(!gh.has_ghost());
        assert_ne!(gh.generation, before);
    }

    // ---- accept (full + word) ----

    #[test]
    fn accept_full_returns_text_and_clears() {
        let mut gh = GhostState::new();
        gh.suggestion = Some(".push(x)".to_string());
        let got = gh.accept();
        assert_eq!(got, Some(".push(x)".to_string()));
        assert!(!gh.has_ghost());
        // Accepting again returns None.
        assert_eq!(gh.accept(), None);
    }

    #[test]
    fn accept_word_takes_one_word_then_remainder_stays() {
        let mut gh = GhostState::new();
        gh.suggestion = Some("foo bar baz".to_string());
        let w1 = gh.accept_word();
        assert_eq!(w1, Some("foo".to_string()));
        assert!(gh.has_ghost());
        assert_eq!(gh.suggestion(), Some(" bar baz"));
        let w2 = gh.accept_word();
        assert_eq!(w2, Some(" bar".to_string()));
        let w3 = gh.accept_word();
        assert_eq!(w3, Some(" baz".to_string()));
        assert!(!gh.has_ghost(), "last word clears the ghost");
    }

    #[test]
    fn accept_word_multiline_advances_one_line() {
        let mut gh = GhostState::new();
        gh.suggestion = Some("end\n  next()".to_string());
        let w1 = gh.accept_word();
        assert_eq!(w1, Some("end".to_string()));
        let w2 = gh.accept_word();
        // The leading newline is consumed (and stops there), so the next accept is
        // just the newline + indent up to the word boundary.
        assert_eq!(w2, Some("\n".to_string()));
        assert_eq!(gh.suggestion(), Some("  next()"));
    }

    // ---- no-key no-op path ----

    #[test]
    fn no_key_arm_is_noop() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("CLAUDE_API_KEY");
        assert!(!GhostState::enabled(), "no key -> disabled");
        let mut gh = GhostState::new();
        gh.arm();
        assert!(gh.armed_at.is_none(), "arm() must not schedule without a key");
        assert!(!gh.has_ghost());
    }

    #[test]
    fn no_key_force_is_noop() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("CLAUDE_API_KEY");
        let mut gh = GhostState::new();
        assert!(!gh.force(ctx("x", 0, 1)), "force() must no-op without a key");
        assert!(!gh.is_inflight());
    }

    #[test]
    fn body_shape_is_nonstreaming_capped() {
        let body = anthropic_body("hi");
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_tokens"], MAX_TOKENS);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    /// Turn the `inline_ai` setting ON under the shared settings test lock so the
    /// debounce-gate tests see `inline_ai == true` (the key is still absent, so no
    /// real request fires — these tests assert only the timer logic).
    fn settings_guard_inline_on() -> std::sync::MutexGuard<'static, ()> {
        let g = crate::settings::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::settings::update(|s| s.inline_ai = true);
        g
    }
}
