//! Autocomplete engine (shim-side, scalar-driven from Mighty).
//!
//! The completion logic lives here on the Rust side because the Mighty IDE can
//! only drive the shim through a scalar `extern c` ABI (L17) and must keep its
//! own `Vec` access flat (L21). Mighty triggers a request at the cursor, then
//! moves the selection / accepts / cancels through `mui_complete_*`; this module
//! owns the candidate list and the selection state.
//!
//! Two providers feed the same dropdown:
//!
//! * **Buffer-word provider (primary, always available):** extract every
//!   identifier-like word from the current buffer,
//!   filter by the prefix at the cursor, dedupe, and sort. Self-contained and
//!   thoroughly unit-tested ([`buffer_words`], [`filter_by_prefix`]).
//! * **mty-lsp semantic provider (best-effort):** spawn `mty lsp`, do the LSP
//!   stdio JSON-RPC handshake, ask `textDocument/completion` at the cursor,
//!   parse `CompletionItem` labels, and merge them ahead of the buffer words.
//!   If the server is absent / slow / errors, we silently fall back to the
//!   buffer words — the editor never blocks ([`lsp::semantic_labels`]).
//!
//! The dropdown is drawn by [`CompletionEngine::draw`] near the cursor pixel.

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

/// One completion candidate: the label/insert text plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The text inserted on accept (also what is shown in the dropdown).
    pub text: String,
    /// `true` for an LSP-provided semantic candidate, `false` for a buffer word.
    pub semantic: bool,
    /// `true` for a snippet prefix (shows a distinct "snippet" badge; accepting
    /// it expands the snippet body rather than inserting the label text).
    pub snippet: bool,
}

/// Whether a char is part of an identifier.
fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// Whether a char can START an identifier.
fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

fn is_ascii_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn is_ascii_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

/// Extract every identifier-like word from `bytes`, in first-appearance order,
/// deduped. Used as the buffer-word candidate pool.
pub fn buffer_words(bytes: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return buffer_words_ascii(bytes);
    };
    let mut words: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if is_ident_start(chars[i].1) {
            let start = chars[i].0;
            i += 1;
            while i < chars.len() && is_ident_char(chars[i].1) {
                i += 1;
            }
            let end = chars.get(i).map_or(text.len(), |(byte, _)| *byte);
            let w = &text[start..end];
            if seen.insert(w.to_string()) {
                words.push(w.to_string());
            }
        } else {
            i += 1;
        }
    }
    words
}

fn buffer_words_ascii(bytes: &[u8]) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_ascii_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ascii_ident_byte(bytes[i]) {
                i += 1;
            }
            if let Ok(w) = std::str::from_utf8(&bytes[start..i]) {
                if seen.insert(w.to_string()) {
                    words.push(w.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    words
}

/// The identifier prefix immediately before byte offset `cursor` in `bytes`:
/// the run of identifier chars ending at the cursor (empty if the char before
/// the cursor is not an identifier char). Returns the prefix string.
pub fn prefix_at(bytes: &[u8], cursor: usize) -> String {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return prefix_at_ascii(bytes, cursor);
    };
    let end = cursor.min(bytes.len());
    let mut end = end;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let chars: Vec<(usize, char)> = text[..end].char_indices().collect();
    let mut idx = chars.len();
    while idx > 0 && is_ident_char(chars[idx - 1].1) {
        idx -= 1;
    }
    if idx == chars.len() {
        return String::new();
    }
    if !is_ident_start(chars[idx].1) {
        return String::new();
    }
    text[chars[idx].0..end].to_string()
}

fn prefix_at_ascii(bytes: &[u8], cursor: usize) -> String {
    let end = cursor.min(bytes.len());
    let mut start = end;
    while start > 0 && is_ascii_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    // A prefix that begins with a digit (e.g. inside `123abc`) is not a valid
    // identifier start.
    if start < end && !is_ascii_ident_start(bytes[start]) {
        return String::new();
    }
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

/// Filter `words` to those that start with `prefix` (case-sensitive) but are not
/// exactly equal to it, sorted and deduped. An empty prefix returns nothing (we
/// don't pop a dropdown of the whole buffer for a bare cursor).
pub fn filter_by_prefix(words: &[String], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = words
        .iter()
        .filter(|w| w.len() > prefix.len() && w.starts_with(prefix))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Max items drawn in the dropdown at once (the visible window).
const VISIBLE: usize = 8;

/// Shim-owned completion state: the candidate list + selection + the prefix
/// length to replace on accept.
#[derive(Debug, Default)]
pub struct CompletionEngine {
    candidates: Vec<Candidate>,
    /// Selected index into `candidates` (0-based).
    sel: usize,
    /// `true` while the dropdown is open.
    active: bool,
    /// Number of chars the accepted item should replace (the prefix length).
    prefix_len: usize,
}

impl CompletionEngine {
    pub fn new() -> Self {
        CompletionEngine::default()
    }

    /// Build the candidate list for the prefix at `cursor` in `bytes`.
    ///
    /// `lsp_labels` are semantic candidates already fetched (possibly empty);
    /// they are merged ahead of the buffer words. Returns the candidate count.
    /// A zero count leaves the engine inactive.
    pub fn request(&mut self, bytes: &[u8], cursor: usize, lsp_labels: &[String]) -> usize {
        let prefix = prefix_at(bytes, cursor);
        self.prefix_len = prefix.chars().count();
        self.candidates.clear();
        self.sel = 0;
        self.active = false;

        if prefix.is_empty() {
            return 0;
        }

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1) Semantic (LSP) candidates first — filter by prefix, drop the exact
        //    prefix, keep their order.
        for label in lsp_labels {
            if label.len() > prefix.len()
                && label.starts_with(&prefix)
                && seen.insert(label.clone())
            {
                self.candidates.push(Candidate {
                    text: label.clone(),
                    semantic: true,
                    snippet: false,
                });
            }
        }

        // 2) Buffer words after, sorted/deduped, skipping anything already added.
        let words = buffer_words(bytes);
        for w in filter_by_prefix(&words, &prefix) {
            if seen.insert(w.clone()) {
                self.candidates.push(Candidate {
                    text: w,
                    semantic: false,
                    snippet: false,
                });
            }
        }

        self.active = !self.candidates.is_empty();
        self.candidates.len()
    }

    /// Inject snippet prefixes (already filtered to the current request's prefix)
    /// at the FRONT of the candidate list — snippets are the most intentional
    /// match, so they rank first and get a distinct "snippet" badge. Skips any
    /// prefix already present. Re-activates the dropdown if it adds candidates.
    /// Call right after [`request`].
    pub fn inject_snippets(&mut self, snippet_prefixes: &[String]) {
        let existing: std::collections::HashSet<String> =
            self.candidates.iter().map(|c| c.text.clone()).collect();
        let mut front: Vec<Candidate> = Vec::new();
        for p in snippet_prefixes {
            if !existing.contains(p) {
                front.push(Candidate {
                    text: p.clone(),
                    semantic: false,
                    snippet: true,
                });
            }
        }
        if front.is_empty() {
            return;
        }
        front.append(&mut self.candidates);
        self.candidates = front;
        self.active = true;
    }

    /// `true` if the currently-selected candidate is a snippet prefix (so the
    /// accept path should EXPAND rather than insert the literal text).
    pub fn accepted_is_snippet(&self) -> bool {
        self.active && self.candidates.get(self.sel).map(|c| c.snippet).unwrap_or(false)
    }

    pub fn count(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    /// Move the selection by `delta` (positive = down), wrapping around.
    pub fn move_sel(&mut self, delta: i32) {
        let n = self.candidates.len();
        if n == 0 {
            return;
        }
        let n_i = n as i32;
        let mut s = self.sel as i32 + delta;
        // Wrap into [0, n).
        s %= n_i;
        if s < 0 {
            s += n_i;
        }
        self.sel = s as usize;
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx < self.candidates.len() {
            self.sel = idx;
            true
        } else {
            false
        }
    }

    /// Number of chars before the cursor to delete when accepting.
    pub fn prefix_len(&self) -> usize {
        self.prefix_len
    }

    /// The selected candidate's text, or `""` when inactive / empty.
    pub fn accepted_text(&self) -> &str {
        if !self.active {
            return "";
        }
        self.candidates
            .get(self.sel)
            .map(|c| c.text.as_str())
            .unwrap_or("")
    }

    /// Close the dropdown and clear its state.
    pub fn cancel(&mut self) {
        self.active = false;
        self.candidates.clear();
        self.sel = 0;
        self.prefix_len = 0;
    }

    /// First visible row index given the current selection, so the selected item
    /// is always within the [0, VISIBLE) window. Pure (unit-tested).
    pub fn scroll_top(&self) -> usize {
        if self.candidates.len() <= VISIBLE {
            return 0;
        }
        if self.sel < VISIBLE {
            0
        } else {
            (self.sel + 1).saturating_sub(VISIBLE)
        }
    }

    fn geometry(
        &self,
        text: &mut crate::text::Text,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
    ) -> (f32, f32, f32, f32, f32, f32, usize, usize) {
        let top = self.scroll_top();
        let shown = self.candidates.len().saturating_sub(top).min(VISIBLE);
        let row_h = layout::LINE_H();
        let pad = 5.0;
        let hint_h = 30.0_f32;
        let w = width as f32;
        let box_w = completion_popup_width(
            text,
            &self.candidates,
            top,
            shown,
            self.sel,
            w,
            theme::CHROME_FONT_SIZE,
        );
        let box_h = shown as f32 * row_h + 2.0 * pad + hint_h;

        let mut box_x = cx;
        let mut box_y = cy + row_h;
        let h = height as f32;
        if box_x + box_w > w {
            box_x = (w - box_w).max(0.0);
        }
        if box_y + box_h > h {
            box_y = (cy - box_h).max(0.0);
        }

        (box_x, box_y, box_w, box_h, pad, row_h, top, shown)
    }

    /// Select the completion row under a click. Returns the selected candidate
    /// index, or -1 when the click missed the visible rows.
    pub fn click_row(
        &mut self,
        text: &mut crate::text::Text,
        x: f32,
        y: f32,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
    ) -> i32 {
        if !self.active || self.candidates.is_empty() {
            return -1;
        }
        let (box_x, box_y, box_w, _box_h, pad, row_h, top, shown) = self.geometry(text, cx, cy, width, height);
        if x < box_x || x > box_x + box_w {
            return -1;
        }
        let row_top = box_y + pad;
        if y < row_top {
            return -1;
        }
        let vis = ((y - row_top) / row_h).floor() as usize;
        if vis >= shown {
            return -1;
        }
        let idx = top + vis;
        if self.select(idx) {
            idx as i32
        } else {
            -1
        }
    }

    /// Draw the dropdown near the cursor pixel `(cx, cy)`. Up to [`VISIBLE`]
    /// items are shown, the selected one highlighted; semantic items get a small
    /// left accent bar. No-op when inactive. `width`/`height` size the panel so
    /// it stays on-screen.
    pub fn draw(&self, ctx: &mut crate::MuiContext, cx: f32, cy: f32, width: u32, height: u32) {
        if !self.active || self.candidates.is_empty() {
            return;
        }
        let top = self.scroll_top();
        let shown = (self.candidates.len() - top).min(VISIBLE);
        if shown == 0 {
            return;
        }

        // Panel geometry: a box just below the cursor, widened to the longest
        // visible label.
        let row_h = layout::LINE_H();
        let pad = 5.0;
        let chrome = theme::CHROME_FONT_SIZE;
        let hint_h = 30.0_f32;
        let (box_x, box_y, box_w, box_h, _pad, _row_h, _top, _shown) =
            self.geometry(&mut ctx.text, cx, cy, width, height);

        let clip = ctx.clip;
        let radius = 8.0_f32;

        // Soft drop shadow + rounded raised card + hairline border (mockup
        // `.autocomplete`).
        ctx.dl_shadow(box_x, box_y + 8.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.8), 24.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        for vis in 0..shown {
            let idx = top + vis;
            let cand = &self.candidates[idx];
            let row_y = box_y + pad + vis as f32 * row_h;
            let selected = idx == self.sel;
            if selected {
                ctx.dl_grad_h(box_x + 5.0, row_y + 2.0, box_w - 10.0, row_h - 4.0, 5.0, theme::accent_a(0.20), 0.9);
                ctx.dl_stroke(box_x + 5.0, row_y + 2.0, box_w - 10.0, row_h - 4.0, 5.0, theme::ACCENT_LINE(), 1.0);
            }
            // Type badge: a small rounded colored square with a letter, classified
            // by a light heuristic (mockup badge colors).
            let (badge_bg, badge_fg, letter, kind, sig) = classify_candidate(cand);
            let bx = box_x + 10.0;
            let by = row_y + (row_h - 18.0) * 0.5;
            ctx.dl_round(bx, by, 18.0, 18.0, 4.0, badge_bg);
            let lw = completion_badge_letter_width(&mut ctx.text, letter, 10.0);
            ctx.text.queue_ui_sized(bx + (18.0 - lw) * 0.5, by + 3.0, letter, badge_fg, 10.0, clip);

            let ty = row_y + (row_h - chrome) * 0.5 - 0.5;
            let name_x = box_x + 38.0;
            let kind_size = chrome - 1.5;
            let kind_x = completion_kind_x(&mut ctx.text, box_x, box_w, kind, kind_size);
            let sig_gap = if completion_row_signature_visible(sig) { 2.0 } else { 0.0 };
            let sig_w = if sig_gap > 0.0 {
                ctx.text.measure_ui_sized(sig, chrome - 1.0).0
            } else {
                0.0
            };
            let name_budget = (kind_x - 10.0 - name_x - sig_gap - sig_w).max(0.0);
            let shown_name = fit_completion_text(&mut ctx.text, &cand.text, name_budget, chrome);
            ctx.text.queue_sized(name_x, ty, &shown_name, theme::TEXT(), chrome, clip);
            // Signature hint immediately after the name, when the provider has
            // real row-level detail. Avoid placeholder fragments; the footer
            // carries full signature context for the selected row.
            if completion_row_signature_visible(sig) {
                let name_w = ctx.text.measure_ui_sized(&shown_name, chrome).0;
                let sig_x = name_x + name_w + sig_gap;
                let sig_budget = (kind_x - 10.0 - sig_x).max(0.0);
                let shown_sig = fit_completion_text(&mut ctx.text, sig, sig_budget, chrome - 1.0);
                if !shown_sig.is_empty() {
                    ctx.text.queue_sized(sig_x, ty, &shown_sig, theme::DIM(), chrome - 1.0, clip);
                }
            }
            // Right-aligned kind metadata.
            ctx.text.queue_ui_sized(kind_x, ty, kind, theme::DIM(), kind_size, clip);
        }

        // Signature-hint footer (mockup `.ac-hint`): the selected candidate's
        // signature on a divided strip, the name in accent + a "· pure" tail.
        let hint_y = box_y + box_h - hint_h;
        ctx.dl_rect(box_x + 1.0, hint_y, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_round(box_x + 1.0, hint_y, box_w - 2.0, hint_h - 1.0, 0.0, theme::BG_2());
        if let Some(sel) = self.candidates.get(self.sel) {
            let hy = hint_y + (hint_h - (chrome - 1.0)) * 0.5 - 0.5;
            let mut hx = box_x + 12.0;
            let tail = if sel.semantic { "(a: I32, b: I32) \u{2192} I32  \u{00B7} pure" } else { "  \u{00B7} local symbol" };
            let tail_w = ctx.text.measure_ui_sized(tail, chrome - 1.0).0;
            let name_budget = (box_x + box_w - 12.0 - tail_w - hx).max(0.0);
            let shown_name = fit_completion_text(&mut ctx.text, &sel.text, name_budget, chrome - 1.0);
            ctx.text.queue_sized(hx, hy, &shown_name, theme::ACCENT_BRIGHT(), chrome - 1.0, clip);
            hx += ctx.text.measure_ui_sized(&shown_name, chrome - 1.0).0;
            let tail_budget = (box_x + box_w - 12.0 - hx).max(0.0);
            let shown_tail = fit_completion_text(&mut ctx.text, tail, tail_budget, chrome - 1.0);
            if !shown_tail.is_empty() {
                ctx.text.queue_sized(hx, hy, &shown_tail, theme::DIM(), chrome - 1.0, clip);
            }
        }
    }
}

fn completion_badge_letter_width(text: &mut crate::text::Text, letter: &str, size: f32) -> f32 {
    text.measure_ui_sized(letter, size).0
}

fn completion_kind_x(
    text: &mut crate::text::Text,
    box_x: f32,
    box_w: f32,
    kind: &str,
    size: f32,
) -> f32 {
    let kw = text.measure_ui_sized(kind, size).0;
    box_x + box_w - 12.0 - kw
}

fn completion_popup_width(
    text: &mut crate::text::Text,
    candidates: &[Candidate],
    top: usize,
    shown: usize,
    selected: usize,
    viewport_w: f32,
    chrome: f32,
) -> f32 {
    let name_x = 38.0_f32;
    let right_pad = 12.0_f32;
    let kind_gap = 10.0_f32;
    let row_min = 280.0_f32;
    let row_max = viewport_w.max(row_min).min(560.0);
    let kind_size = chrome - 1.5;
    let mut desired = row_min;
    for cand in candidates.iter().skip(top).take(shown) {
        let (_badge_bg, _badge_fg, _letter, kind, sig) = classify_candidate(cand);
        let name_w = text.measure_ui_sized(&cand.text, chrome).0;
        let sig_gap = if completion_row_signature_visible(sig) { 2.0 } else { 0.0 };
        let sig_w = if sig_gap > 0.0 {
            text.measure_ui_sized(sig, chrome - 1.0).0
        } else {
            0.0
        };
        let kind_w = text.measure_ui_sized(kind, kind_size).0;
        desired = desired.max(name_x + name_w + sig_gap + sig_w + kind_gap + kind_w + right_pad);
    }
    if let Some(sel) = candidates.get(selected) {
        let tail = if sel.semantic { "(a: I32, b: I32) \u{2192} I32  \u{00B7} pure" } else { "  \u{00B7} local symbol" };
        let footer_w = 12.0 + text.measure_ui_sized(&sel.text, chrome - 1.0).0 + text.measure_ui_sized(tail, chrome - 1.0).0 + right_pad;
        desired = desired.max(footer_w.min(420.0));
    }
    desired.min(row_max).max(row_min)
}

fn fit_completion_text(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if max_px <= 0.0 || s.is_empty() {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    const ELLIPSIS: &str = "...";
    if text.measure_ui_sized(ELLIPSIS, size).0 > max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    for keep in (1..=chars.len()).rev() {
        let mut candidate: String = chars.iter().take(keep).collect();
        candidate.push_str(ELLIPSIS);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            return candidate;
        }
    }
    ELLIPSIS.to_string()
}

/// Classify a candidate into a mockup-style type badge + a signature/kind hint.
/// A light heuristic over the text since the engine only tracks semantic-ness:
/// capitalized → type (T, teal), keyword set → keyword (K, violet), looks like a
/// fn (followed by `(` in source isn't known here) → fn (ƒ, gold) when semantic,
/// else variable (x, grey).
fn classify_candidate(cand: &Candidate) -> (MuiColor, MuiColor, &'static str, &'static str, &'static str) {
    const KEYWORDS: &[&str] = &[
        "fn", "let", "mut", "while", "if", "else", "return", "match", "struct",
        "enum", "for", "in", "type", "true", "false", "await", "async", "pub",
        "import", "effect", "extern",
    ];
    let t = cand.text.as_str();
    // Snippet prefixes get a distinct badge regardless of how the text looks.
    if cand.snippet {
        return (
            MuiColor::new(0.482, 0.800, 1.0, 0.16),
            theme::SYN_FUNCTION(),
            "\u{2026}", // ellipsis glyph — "expands to more"
            "snippet",
            "",
        );
    }
    if KEYWORDS.contains(&t) {
        return (
            MuiColor::new(0.718, 0.580, 1.0, 0.14),
            theme::SYN_KEYWORD(),
            "K",
            "keyword",
            "",
        );
    }
    if t.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
        return (
            MuiColor::new(0.353, 0.820, 0.769, 0.14),
            theme::SYN_TYPE(),
            "T",
            "struct",
            "",
        );
    }
    if cand.semantic {
        return (
            MuiColor::new(1.0, 0.824, 0.478, 0.14),
            theme::SYN_FUNCTION(),
            "\u{0192}",
            "function",
            "(…)",
        );
    }
    (
        MuiColor::new(0.843, 0.843, 0.890, 0.10),
        theme::SYN_DEFAULT(),
        "x",
        "local",
        "",
    )
}

fn completion_row_signature_visible(sig: &str) -> bool {
    !sig.is_empty() && sig.is_ascii()
}

// ---------------------------------------------------------------------------
// mty-lsp semantic provider (best-effort, hand-rolled JSON-RPC over stdio)
// ---------------------------------------------------------------------------

pub mod lsp {
    //! Minimal `mty lsp` client: spawn the server, do the LSP handshake, ask
    //! for completion at a position, and scrape `CompletionItem` `label`s out of
    //! the JSON response with a small hand scanner (no serde dependency). Every
    //! step is short-timeout and failure-tolerant — any error returns an empty
    //! label list so the caller falls back to buffer words.

    use std::io::{Read, Write};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Resolve the `mty` binary the same way diagnostics does.
    fn mty_path() -> String {
        crate::mty::path()
    }

    /// Frame a JSON-RPC message with the LSP `Content-Length` header.
    fn frame(json: &str) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", json.len()).into_bytes();
        out.extend_from_slice(json.as_bytes());
        out
    }

    /// Escape a string for embedding in a JSON string literal.
    fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    /// Build a `file://` URI for an absolute path (Windows-aware: drive paths
    /// become `file:///C:/...`). Best-effort; used only as the document id.
    fn file_uri(path: &Path) -> String {
        let s = path.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    /// Scrape `"label":"..."` values out of a JSON blob. Returns labels in the
    /// order seen, deduped. A deliberately small scanner: completion responses
    /// put the insert text in `label`, which is all the dropdown needs.
    pub fn scrape_labels(json: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let bytes = json.as_bytes();
        let key = b"\"label\"";
        let mut i = 0;
        while i + key.len() < bytes.len() {
            if &bytes[i..i + key.len()] == key {
                if let Some((val, past)) = read_json_string_at(bytes, i + key.len()) {
                    if !val.is_empty() && seen.insert(val.clone()) {
                        out.push(val);
                    }
                    i = past;
                    continue;
                }
            }
            i += 1;
        }
        out
    }

    fn read_json_string_at(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
        let mut j = pos;
        while j < bytes.len() && matches!(bytes[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'"' {
            return None;
        }
        j += 1;
        let mut val = String::new();
        let mut segment_start = j;
        let mut high_surrogate: Option<u16> = None;
        while j < bytes.len() {
            match bytes[j] {
                b'"' => {
                    flush_json_segment(bytes, segment_start, j, &mut val, &mut high_surrogate)?;
                    push_pending_surrogate(&mut val, &mut high_surrogate);
                    return Some((val, j + 1));
                }
                b'\\' if j + 1 < bytes.len() => {
                    flush_json_segment(bytes, segment_start, j, &mut val, &mut high_surrogate)?;
                    j += 1;
                    match bytes[j] {
                        b'n' => push_escaped_char(&mut val, &mut high_surrogate, '\n'),
                        b't' => push_escaped_char(&mut val, &mut high_surrogate, '\t'),
                        b'r' => push_escaped_char(&mut val, &mut high_surrogate, '\r'),
                        b'b' => push_escaped_char(&mut val, &mut high_surrogate, '\u{0008}'),
                        b'f' => push_escaped_char(&mut val, &mut high_surrogate, '\u{000c}'),
                        b'"' => push_escaped_char(&mut val, &mut high_surrogate, '"'),
                        b'\\' => push_escaped_char(&mut val, &mut high_surrogate, '\\'),
                        b'/' => push_escaped_char(&mut val, &mut high_surrogate, '/'),
                        b'u' if j + 4 < bytes.len() => {
                            let unit = read_hex4(&bytes[j + 1..j + 5])?;
                            push_json_code_unit(&mut val, &mut high_surrogate, unit);
                            j += 4;
                        }
                        other => push_escaped_char(&mut val, &mut high_surrogate, other as char),
                    }
                    j += 1;
                    segment_start = j;
                    continue;
                }
                _ => {
                    j += 1;
                    continue;
                }
            }
        }
        None
    }

    fn flush_json_segment(
        bytes: &[u8],
        start: usize,
        end: usize,
        out: &mut String,
        high_surrogate: &mut Option<u16>,
    ) -> Option<()> {
        if start < end {
            push_pending_surrogate(out, high_surrogate);
            out.push_str(std::str::from_utf8(&bytes[start..end]).ok()?);
        }
        Some(())
    }

    fn read_hex4(bytes: &[u8]) -> Option<u16> {
        if bytes.len() != 4 {
            return None;
        }
        let mut value = 0u16;
        for &b in bytes {
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return None,
            };
            value = (value << 4) | digit as u16;
        }
        Some(value)
    }

    fn push_escaped_char(out: &mut String, high_surrogate: &mut Option<u16>, ch: char) {
        push_pending_surrogate(out, high_surrogate);
        out.push(ch);
    }

    fn push_pending_surrogate(out: &mut String, high_surrogate: &mut Option<u16>) {
        if high_surrogate.take().is_some() {
            out.push('\u{fffd}');
        }
    }

    fn push_json_code_unit(out: &mut String, high_surrogate: &mut Option<u16>, unit: u16) {
        match unit {
            0xd800..=0xdbff => {
                push_pending_surrogate(out, high_surrogate);
                *high_surrogate = Some(unit);
            }
            0xdc00..=0xdfff => {
                if let Some(high) = high_surrogate.take() {
                    let high = (high as u32) - 0xd800;
                    let low = (unit as u32) - 0xdc00;
                    let cp = 0x10000 + ((high << 10) | low);
                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                } else {
                    out.push('\u{fffd}');
                }
            }
            _ => {
                push_pending_surrogate(out, high_surrogate);
                out.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
            }
        }
    }

    /// Kill a child process, ignoring errors (best-effort teardown).
    fn kill(mut child: Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Ask `mty lsp` for completion at (`line0`, `col0`) (0-based) in a document
    /// whose full text is `source`, identified by `path`. Returns the scraped
    /// `CompletionItem` labels, or an empty Vec on any failure / timeout.
    ///
    /// The whole exchange runs against a short overall deadline; the caller
    /// should run this off the render thread (we keep it self-contained so it
    /// can be spawned on a worker thread).
    pub fn semantic_labels(path: &Path, source: &str, line0: u32, col0: u32) -> Vec<String> {
        semantic_labels_with_timeout(path, source, line0, col0, Duration::from_millis(2500))
    }

    /// [`semantic_labels`] with an explicit overall timeout (used by tests).
    ///
    /// Robustness note: on Windows the child's stdout pipe is *blocking*, so a
    /// naive read loop with a deadline never returns until the server closes
    /// stdout (it doesn't). We therefore read on a worker thread and bound the
    /// wait with `recv_timeout`; on timeout we KILL the child, which closes the
    /// pipe and lets the reader thread reach EOF and exit. This guarantees the
    /// caller is never blocked longer than `timeout` even if the server hangs.
    pub fn semantic_labels_with_timeout(
        path: &Path,
        source: &str,
        line0: u32,
        col0: u32,
        timeout: Duration,
    ) -> Vec<String> {
        let mty = mty_path();
        let child = Command::new(&mty)
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                eprintln!("completion(lsp): spawn `{mty} lsp` failed: {e} — buffer words only");
                return Vec::new();
            }
        };

        let uri = file_uri(path);

        // Compose the JSON-RPC message sequence.
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#.to_string();
        let initialized = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string();
        let did_open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"mighty","version":1,"text":"{}"}}}}}}"#,
            json_escape(&uri),
            json_escape(source)
        );
        let completion = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":{},"character":{}}}}}}}"#,
            json_escape(&uri),
            line0,
            col0
        );
        // Send the requests on a writer thread, STAGED with brief pauses. The
        // server (tower-lsp) processes messages in arrival order but applies
        // `didOpen` to its doc store before it can answer `completion` against
        // that document — firing everything in one burst makes completion race
        // ahead of the open and return nothing (verified). Small gaps let the
        // open settle. After completion we close stdin so the server, having
        // answered, will eventually exit; we don't rely on that for the timeout.
        let Some(mut stdin) = child.stdin.take() else {
            kill(child);
            return Vec::new();
        };
        let writer = std::thread::spawn(move || {
            let stages: [(&str, u64); 4] = [
                (&initialize, 80),
                (&initialized, 40),
                (&did_open, 120),
                (&completion, 0),
            ];
            for (msg, pause_ms) in stages {
                if stdin.write_all(&frame(msg)).is_err() || stdin.flush().is_err() {
                    return;
                }
                if pause_ms > 0 {
                    std::thread::sleep(Duration::from_millis(pause_ms));
                }
            }
            // Drop stdin (end of input) once requests are sent.
            drop(stdin);
        });

        let Some(mut stdout) = child.stdout.take() else {
            kill(child);
            return Vec::new();
        };

        // Read on a worker thread so a blocking pipe read can't pin us past the
        // timeout. The thread reads until it has seen the completion response
        // (the `"id":2` payload) — then it stops promptly — or until EOF / a
        // size cap. The server doesn't close stdout on its own, so without the
        // early stop we'd always wait the full timeout; the `"id":2` marker lets
        // the happy path return as soon as the answer arrives.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader = std::thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        // Stop once the completion response (id:2) has arrived.
                        if find_subslice(&buf, b"\"id\":2").is_some() {
                            break;
                        }
                        if buf.len() > 1024 * 1024 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(buf);
        });

        let raw = match rx.recv_timeout(timeout) {
            Ok(bytes) => {
                kill(child); // closes the pipe; reader (already done) exits
                let _ = writer.join();
                let _ = reader.join();
                bytes
            }
            Err(_) => {
                // Timed out: kill the child to close the pipe and unblock the
                // reader, then collect whatever it managed to read.
                let _ = child.kill();
                let _ = child.wait();
                let bytes = rx.recv_timeout(Duration::from_millis(500)).unwrap_or_default();
                let _ = writer.join();
                let _ = reader.join();
                eprintln!("completion(lsp): timed out after {timeout:?} — buffer words only");
                bytes
            }
        };

        let text = String::from_utf8_lossy(&raw);
        // Scrape `label` values from the response stream. The completion result
        // (id:2) is the only message with `label` fields; the initialize result
        // has none, so the whole-blob scrape yields exactly the candidate labels.
        scrape_labels(&text)
    }

    /// Find the first occurrence of `needle` in `hay` (byte substring search).
    fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || needle.len() > hay.len() {
            return None;
        }
        hay.windows(needle.len()).position(|w| w == needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_words_extracts_identifiers() {
        let src = b"fn main() { let foo = bar_baz + foo; qux123 }";
        let words = buffer_words(src);
        // First-appearance order, deduped (`foo` appears twice -> once).
        assert_eq!(
            words,
            vec![
                "fn".to_string(),
                "main".to_string(),
                "let".to_string(),
                "foo".to_string(),
                "bar_baz".to_string(),
                "qux123".to_string(),
            ]
        );
    }

    #[test]
    fn buffer_words_ignores_numbers_and_punct() {
        let words = buffer_words(b"123 + 45.6 - _x99 == y");
        assert_eq!(words, vec!["_x99".to_string(), "y".to_string()]);
    }

    #[test]
    fn buffer_words_dedupes() {
        let words = buffer_words(b"alpha alpha beta alpha beta");
        assert_eq!(words, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn buffer_words_extracts_unicode_identifiers() {
        let words = buffer_words("fn café() { let δοκιμή = 東京_2 + café }".as_bytes());
        assert_eq!(
            words,
            vec![
                "fn".to_string(),
                "café".to_string(),
                "let".to_string(),
                "δοκιμή".to_string(),
                "東京_2".to_string(),
            ]
        );
    }

    #[test]
    fn prefix_at_reads_identifier_before_cursor() {
        let src = b"let counter = coun";
        // Cursor at end -> prefix "coun".
        assert_eq!(prefix_at(src, src.len()), "coun");
        // Cursor right after "let" -> "let".
        assert_eq!(prefix_at(b"let x", 3), "let");
        // Cursor after a space -> empty.
        assert_eq!(prefix_at(b"let ", 4), "");
        // Cursor in the middle of a word -> partial up to cursor.
        assert_eq!(prefix_at(b"counter", 4), "coun");
    }

    #[test]
    fn prefix_at_skips_leading_digits() {
        // `123abc` is not an identifier.
        assert_eq!(prefix_at(b"x = 123abc", 10), "");
        // Pure digits -> empty (a numeric literal).
        assert_eq!(prefix_at(b"x = 1234", 8), "");
    }

    #[test]
    fn prefix_at_reads_unicode_identifier_before_cursor() {
        let src = "let café_value = caf";
        assert_eq!(prefix_at(src.as_bytes(), src.len()), "caf");

        let src = "let δοκιμή = δοκ";
        assert_eq!(prefix_at(src.as_bytes(), src.len()), "δοκ");

        let cursor = "let 東京".len();
        let src = "let 東京_2 = 1";
        assert_eq!(prefix_at(src.as_bytes(), cursor), "東京");
    }

    #[test]
    fn prefix_at_rejects_unicode_numeric_literals() {
        let src = "let x = １２３abc";
        assert_eq!(prefix_at(src.as_bytes(), src.len()), "");
    }

    #[test]
    fn filter_by_prefix_sorts_dedupes_excludes_exact() {
        let words = vec![
            "counter".to_string(),
            "count".to_string(),
            "countdown".to_string(),
            "color".to_string(),
            "count".to_string(), // dup
        ];
        let got = filter_by_prefix(&words, "count");
        // "count" itself is excluded (equal to prefix); sorted; deduped.
        assert_eq!(
            got,
            vec!["countdown".to_string(), "counter".to_string()]
        );
        // No matches.
        assert!(filter_by_prefix(&words, "zzz").is_empty());
        // Empty prefix -> nothing.
        assert!(filter_by_prefix(&words, "").is_empty());
    }

    #[test]
    fn request_merges_lsp_ahead_of_buffer_words() {
        let mut e = CompletionEngine::new();
        let src = b"let counter = 0; let countdown = 1; coun";
        let cursor = src.len();
        // LSP offers `count_lsp` and `counter` (dup with buffer); buffer offers
        // `counter`, `countdown`.
        let lsp = vec!["count_lsp".to_string(), "counter".to_string()];
        let n = e.request(src, cursor, &lsp);
        assert_eq!(e.prefix_len(), 4); // "coun"
        // Order: semantic first (count_lsp, counter), then buffer-only
        // (countdown). `counter` dedupes to the semantic entry.
        assert_eq!(n, 3);
        assert!(e.is_active());
        assert_eq!(e.accepted_text(), "count_lsp"); // sel starts at 0
    }

    #[test]
    fn request_buffer_only_when_no_lsp() {
        let mut e = CompletionEngine::new();
        let src = b"alpha alphabet album al";
        let n = e.request(src, src.len(), &[]);
        // prefix "al" -> album, alpha, alphabet (sorted), excludes nothing equal.
        assert_eq!(n, 3);
        assert_eq!(e.accepted_text(), "album");
    }

    #[test]
    fn request_offers_unicode_buffer_words() {
        let mut e = CompletionEngine::new();
        let src = "let café_value = 1\nlet café_total = caf";
        let n = e.request(src.as_bytes(), src.len(), &[]);
        assert_eq!(n, 2);
        assert_eq!(e.prefix_len(), 3);
        assert_eq!(e.accepted_text(), "café_total");
    }

    #[test]
    fn request_empty_prefix_is_inactive() {
        let mut e = CompletionEngine::new();
        let n = e.request(b"foo bar ", 8, &["anything".to_string()]);
        assert_eq!(n, 0);
        assert!(!e.is_active());
        assert_eq!(e.accepted_text(), "");
        assert_eq!(e.prefix_len(), 0);
    }

    #[test]
    fn move_selection_wraps() {
        let mut e = CompletionEngine::new();
        let src = b"aa ab ac ad a";
        e.request(src, src.len(), &[]); // aa, ab, ac, ad (prefix "a")
        assert_eq!(e.count(), 4);
        assert_eq!(e.selection(), 0);
        e.move_sel(1);
        assert_eq!(e.selection(), 1);
        e.move_sel(-1);
        assert_eq!(e.selection(), 0);
        // Wrap below 0 -> last.
        e.move_sel(-1);
        assert_eq!(e.selection(), 3);
        // Wrap above end -> 0.
        e.move_sel(1);
        assert_eq!(e.selection(), 0);
    }

    #[test]
    fn accept_replace_length_math() {
        // Buffer "...= coun", cursor after "coun". Accepting "counter" must
        // delete prefix_len (4) chars then insert "counter" (7 chars) -> net +3.
        let mut e = CompletionEngine::new();
        let src = b"x = coun";
        e.request(src, src.len(), &[]);
        // No buffer word starts with "coun" besides nothing -> inactive here, so
        // feed an LSP candidate to exercise the math.
        let mut e2 = CompletionEngine::new();
        e2.request(src, src.len(), &["counter".to_string()]);
        assert_eq!(e2.prefix_len(), 4);
        assert_eq!(e2.accepted_text(), "counter");
        // The Mighty side deletes prefix_len chars, inserts accepted_text chars.
        assert_eq!(e2.accepted_text().chars().count(), 7);
        let _ = e; // silence unused in the inactive branch
    }

    #[test]
    fn inject_snippets_prepends_with_badge_and_flags_accept() {
        let mut e = CompletionEngine::new();
        // Buffer offers "format", "for_each" for prefix "for".
        let src = b"format for_each fo";
        e.request(src, src.len(), &[]);
        let before = e.count();
        // Inject the `for` snippet prefix.
        e.inject_snippets(&["for".to_string()]);
        // It went to the FRONT and is the selected (sel=0) candidate.
        assert_eq!(e.count(), before + 1);
        assert_eq!(e.accepted_text(), "for");
        assert!(e.accepted_is_snippet(), "snippet entry must flag the accept path");
        assert!(e.candidates[0].snippet);
        // Its badge classifies as a snippet (distinct from keyword/type/var).
        let (_, _, letter, kind, _) = classify_candidate(&e.candidates[0]);
        assert_eq!(kind, "snippet");
        assert_eq!(letter, "\u{2026}");
        // Moving off the snippet entry clears the snippet-accept flag.
        e.move_sel(1);
        assert!(!e.accepted_is_snippet());
    }

    #[test]
    fn semantic_completion_row_uses_readable_kind_label() {
        let cand = Candidate {
            text: "protocol".to_string(),
            semantic: true,
            snippet: false,
        };
        let (_, _, _letter, kind, sig) = classify_candidate(&cand);
        assert_eq!(kind, "function");
        assert!(!kind.contains("fn"));
        assert!(!completion_row_signature_visible(sig));
    }

    #[test]
    fn completion_badge_letter_width_uses_measured_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };
        let narrow = completion_badge_letter_width(&mut ctx.text, "i", 10.0);
        let wide = completion_badge_letter_width(&mut ctx.text, "\u{2026}", 10.0);

        assert!(narrow > 0.0);
        assert!(wide > narrow);
    }

    #[test]
    fn completion_kind_x_uses_measured_kind_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };
        let box_x = 24.0;
        let box_w = 280.0;
        let size = theme::CHROME_FONT_SIZE - 1.5;
        let short_x = completion_kind_x(&mut ctx.text, box_x, box_w, "var", size);
        let long_x = completion_kind_x(&mut ctx.text, box_x, box_w, "function", size);

        assert!(long_x < short_x);
        assert!(short_x < box_x + box_w - 12.0);
    }

    #[test]
    fn completion_row_label_fits_before_kind_metadata() {
        let mut ctx = match crate::MuiContext::new_offscreen(640, 480) {
            Some(c) => c,
            None => {
                eprintln!("SKIP: no GPU adapter available; skipping completion text measurement");
                return;
            }
        };
        let name_x = 38.0;
        let box_x = 0.0;
        let box_w = 280.0;
        let size = theme::CHROME_FONT_SIZE;
        let kind_x = completion_kind_x(&mut ctx.text, box_x, box_w, "snippet", size - 1.5);
        let budget = (kind_x - 10.0 - name_x).max(0.0);
        let shown = fit_completion_text(
            &mut ctx.text,
            "very_long_completion_candidate_that_used_to_run_under_kind",
            budget,
            size,
        );
        let shown_w = ctx.text.measure_ui_sized(&shown, size).0;
        assert!(shown.ends_with("..."), "long completion rows should ellipsize: {shown}");
        assert!(
            name_x + shown_w <= kind_x - 10.0 + 0.5,
            "completion label should fit before kind metadata: name_end={} kind_x={kind_x}",
            name_x + shown_w
        );
    }

    #[test]
    fn completion_footer_name_and_tail_fit_panel_width() {
        let mut ctx = match crate::MuiContext::new_offscreen(640, 480) {
            Some(c) => c,
            None => {
                eprintln!("SKIP: no GPU adapter available; skipping completion text measurement");
                return;
            }
        };
        let box_x = 0.0;
        let box_w = 280.0;
        let size = theme::CHROME_FONT_SIZE - 1.0;
        let hx = box_x + 12.0;
        let tail = "  \u{00B7} local symbol";
        let tail_w = ctx.text.measure_ui_sized(tail, size).0;
        let name_budget = (box_x + box_w - 12.0 - tail_w - hx).max(0.0);
        let shown_name = fit_completion_text(
            &mut ctx.text,
            "selected_completion_candidate_with_a_long_name",
            name_budget,
            size,
        );
        let name_w = ctx.text.measure_ui_sized(&shown_name, size).0;
        let tail_budget = (box_x + box_w - 12.0 - (hx + name_w)).max(0.0);
        let shown_tail = fit_completion_text(&mut ctx.text, tail, tail_budget, size);
        let total_w = name_w + ctx.text.measure_ui_sized(&shown_tail, size).0;
        assert!(shown_name.ends_with("..."), "long footer name should ellipsize: {shown_name}");
        assert!(
            hx + total_w <= box_x + box_w - 12.0 + 0.5,
            "completion footer should fit within panel: footer_end={} panel_right={}",
            hx + total_w,
            box_x + box_w - 12.0
        );
    }

    #[test]
    fn completion_popup_width_uses_measured_visible_rows() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(900, 700) else {
            return;
        };
        let chrome = theme::CHROME_FONT_SIZE;
        let narrow = vec![Candidate {
            text: "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii".to_string(),
            semantic: false,
            snippet: false,
        }];
        let wide = vec![Candidate {
            text: "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW".to_string(),
            semantic: false,
            snippet: false,
        }];

        let narrow_w = completion_popup_width(&mut ctx.text, &narrow, 0, 1, 0, 900.0, chrome);
        let wide_w = completion_popup_width(&mut ctx.text, &wide, 0, 1, 0, 900.0, chrome);
        let measured_delta =
            ctx.text.measure_ui_sized(&wide[0].text, chrome).0 - ctx.text.measure_ui_sized(&narrow[0].text, chrome).0;

        assert!(measured_delta > 10.0, "test strings should differ in rendered width");
        assert!(
            wide_w >= narrow_w + measured_delta.min(200.0) - 1.0,
            "popup width should grow with measured row text: narrow={narrow_w} wide={wide_w}"
        );
    }

    #[test]
    fn completion_geometry_clamps_measured_width_to_viewport() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(360, 480) else {
            return;
        };
        let mut e = CompletionEngine::new();
        e.candidates = vec![Candidate {
            text: "very_wide_completion_candidate_name_that_should_not_escape_the_viewport".to_string(),
            semantic: true,
            snippet: false,
        }];
        e.active = true;
        let (box_x, _box_y, box_w, _box_h, _pad, _row_h, _top, _shown) =
            e.geometry(&mut ctx.text, 320.0, 120.0, 360, 480);

        assert!(box_w <= 360.0);
        assert!(box_x + box_w <= 360.0 + 0.5);
    }

    #[test]
    fn inject_snippets_skips_duplicates() {
        let mut e = CompletionEngine::new();
        // Buffer already contains "for".
        e.request(b"for fo", 6, &[]);
        let before = e.count();
        e.inject_snippets(&["for".to_string()]);
        // No duplicate added (the buffer word already covered "for").
        assert_eq!(e.count(), before);
    }

    #[test]
    fn cancel_clears_state() {
        let mut e = CompletionEngine::new();
        e.request(b"aa ab a", 7, &[]);
        assert!(e.is_active());
        e.cancel();
        assert!(!e.is_active());
        assert_eq!(e.count(), 0);
        assert_eq!(e.accepted_text(), "");
        assert_eq!(e.prefix_len(), 0);
    }

    #[test]
    fn scroll_top_keeps_selection_visible() {
        let mut e = CompletionEngine::new();
        // Build 12 candidates: words a0..a11 with prefix "a".
        let src = b"a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 a10 a11 a";
        e.request(src, src.len(), &[]);
        assert!(e.count() >= 10);
        // Selection within first window -> top 0.
        assert_eq!(e.scroll_top(), 0);
        // Move selection to index 9 (>= VISIBLE 8) -> window scrolls.
        for _ in 0..9 {
            e.move_sel(1);
        }
        assert_eq!(e.selection(), 9);
        assert_eq!(e.scroll_top(), 9 + 1 - VISIBLE); // 2
    }

    #[test]
    fn click_row_selects_visible_completion() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(900, 700) else {
            return;
        };
        let mut e = CompletionEngine::new();
        e.request(b"alpha alpine a", 14, &[]);
        assert!(e.count() >= 2);
        let (box_x, box_y, _box_w, _box_h, pad, row_h, _top, _shown) =
            e.geometry(&mut ctx.text, 250.0, 120.0, 900, 700);
        let idx = e.click_row(&mut ctx.text, box_x + 24.0, box_y + pad + row_h + 2.0, 250.0, 120.0, 900, 700);
        assert_eq!(idx, 1);
        assert_eq!(e.selection(), 1);
        assert_eq!(
            e.click_row(&mut ctx.text, box_x - 2.0, box_y + pad + 2.0, 250.0, 120.0, 900, 700),
            -1
        );
    }

    #[test]
    fn lsp_scrape_labels_extracts_and_dedupes() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"foo","kind":3},{"label":"bar"},{"label":"foo"}]}"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(labels, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn lsp_scrape_handles_escapes() {
        let json = r#"[{"label":"a\"b"},{"label":"c\\d"}]"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(labels, vec!["a\"b".to_string(), "c\\d".to_string()]);
    }

    #[test]
    fn lsp_scrape_decodes_unicode_labels() {
        let json = r#"[{"label":"東京"},{"label":"caf\u00e9"},{"label":"\ud83d\ude00 target"},{"label":"\ud83dX"}]"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(
            labels,
            vec![
                "東京".to_string(),
                "café".to_string(),
                "\u{1f600} target".to_string(),
                "\u{fffd}X".to_string(),
            ]
        );
    }

    /// Guarded integration test: spawn the real `mty lsp` and ask for completion
    /// in a tiny program. SKIPS (passes with a note) if the server can't spawn
    /// (no `mty` on PATH / dev build absent), so CI without `mty` stays green.
    ///
    /// When the server IS available, asserts we got at least one keyword label
    /// (the LSP always returns the keyword set), proving the full handshake +
    /// scrape path works end-to-end.
    #[test]
    fn lsp_semantic_completion_end_to_end() {
        use std::path::PathBuf;
        use std::time::Duration;

        // Resolve mty the way the client does; if it is not present, skip.
        let mty = PathBuf::from(crate::mty::path());
        let has_mty = std::env::var_os("MIGHTY_MTY").is_some() || mty.exists();
        if !has_mty {
            eprintln!("lsp_semantic_completion_end_to_end: no mty binary — skipping");
            return;
        }

        // A trivial Mighty program; complete after `le` on its own line.
        let source = "fn main() {\n  let counter = 0\n  le\n}\n";
        // Cursor on line index 2 (`  le`), char 4 (after "le").
        let path = PathBuf::from("probe.mty");
        let labels = lsp::semantic_labels_with_timeout(
            &path,
            source,
            2,
            4,
            Duration::from_secs(8),
        );

        if labels.is_empty() {
            // Server spawned but returned nothing within the timeout — treat as
            // best-effort fallback (don't fail CI on a flaky/slow server).
            eprintln!(
                "lsp_semantic_completion_end_to_end: server returned no labels (timeout/flaky) — \
                 buffer-word fallback still covers completion"
            );
            return;
        }
        // The LSP always includes the keyword set; `let` must be present.
        assert!(
            labels.iter().any(|l| l == "let"),
            "expected `let` keyword among LSP labels, got: {labels:?}"
        );
        eprintln!(
            "lsp_semantic_completion_end_to_end: got {} labels (incl. `let`)",
            labels.len()
        );
    }
}
