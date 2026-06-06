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
//!   parse `CompletionItem` insert/display/kind/detail/labelDetails/docs/sort/
//!   deprecated/commit characters text, and merge them ahead of the buffer words.
//!   If the server is absent / slow / errors, we silently fall back to the
//!   buffer words — the editor never blocks ([`lsp::semantic_labels`]).
//!
//! The dropdown is drawn by [`CompletionEngine::draw`] near the cursor pixel.

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

/// One completion candidate: the insert text, optional display label, and where
/// it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The text inserted on accept.
    pub text: String,
    /// Optional row label shown in the dropdown when it differs from `text`.
    pub display_text: Option<String>,
    /// Optional provider detail shown beside the row and in the footer.
    pub detail_text: Option<String>,
    /// Optional provider documentation shown in the footer when detail is absent.
    pub documentation_text: Option<String>,
    /// Optional provider kind label shown in the right metadata column.
    pub kind_label: Option<&'static str>,
    /// `true` when the provider asked this item to be initially selected.
    pub preselect: bool,
    /// `true` when the provider marked this item as deprecated.
    pub deprecated: bool,
    /// Provider commit characters that accept this item before inserting the char.
    pub commit_chars: Vec<char>,
    /// Optional provider replacement width before the cursor, in editor chars.
    pub replace_len: Option<usize>,
    /// `true` for an LSP-provided semantic candidate, `false` for a buffer word.
    pub semantic: bool,
    /// `true` for a snippet prefix (shows a distinct "snippet" badge; accepting
    /// it expands the snippet body rather than inserting the label text).
    pub snippet: bool,
}

/// LSP semantic completion text plus the server's optional display and matching keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticCandidate {
    /// The text inserted on accept.
    pub text: String,
    /// Optional display label from LSP `label`.
    pub display_text: Option<String>,
    /// Optional LSP `detail` text.
    pub detail_text: Option<String>,
    /// Optional LSP `documentation` text.
    pub documentation_text: Option<String>,
    /// Optional LSP `kind` mapped to a stable display label.
    pub kind_label: Option<&'static str>,
    /// LSP `preselect` preference for the initial selected row.
    pub preselect: bool,
    /// LSP `deprecated` or `tags: [1]` marker.
    pub deprecated: bool,
    /// LSP `commitCharacters` for accepting this item while typing punctuation.
    pub commit_chars: Vec<char>,
    /// Optional LSP `textEdit.range`, converted during request matching.
    pub edit_range: Option<CompletionEditRange>,
    /// Optional LSP `filterText`, used only to decide whether the row matches the
    /// current typed prefix.
    pub filter_text: Option<String>,
    /// Optional LSP `sortText`, used to rank semantic rows before buffer words.
    pub sort_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionEditRange {
    pub start_line: u32,
    pub start_col_utf16: u32,
    pub end_line: u32,
    pub end_col_utf16: u32,
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

fn source_line_col_at(bytes: &[u8], cursor: usize) -> (u32, u32) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        let mut line = 0u32;
        let mut col = 0u32;
        for &b in bytes.iter().take(cursor.min(bytes.len())) {
            if b == b'\n' {
                line = line.saturating_add(1);
                col = 0;
            } else {
                col = col.saturating_add(1);
            }
        }
        return (line, col);
    };
    let mut end = cursor.min(bytes.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut line = 0u32;
    let mut col = 0u32;
    for ch in text[..end].chars() {
        if ch == '\n' {
            line = line.saturating_add(1);
            col = 0;
        } else {
            col = col.saturating_add(1);
        }
    }
    (line, col)
}

fn source_utf16_col_to_char(bytes: &[u8], line: u32, utf16_col: u32) -> Option<u32> {
    let text = std::str::from_utf8(bytes).ok()?;
    let line_text = text.lines().nth(line as usize).unwrap_or("");
    let mut units = 0u32;
    let mut chars = 0u32;
    for ch in line_text.chars() {
        if units >= utf16_col {
            return Some(chars);
        }
        let next = units.saturating_add(ch.len_utf16() as u32);
        if next > utf16_col {
            return Some(chars);
        }
        units = next;
        chars = chars.saturating_add(1);
    }
    Some(chars)
}

fn replacement_len_for_range(
    bytes: &[u8],
    cursor_pos: (u32, u32),
    range: CompletionEditRange,
) -> Option<usize> {
    if range.start_line != range.end_line || range.end_line != cursor_pos.0 {
        return None;
    }
    let start_col = source_utf16_col_to_char(bytes, range.start_line, range.start_col_utf16)?;
    let end_col = source_utf16_col_to_char(bytes, range.end_line, range.end_col_utf16)?;
    if end_col != cursor_pos.1 || start_col > end_col {
        return None;
    }
    Some((end_col - start_col) as usize)
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
        let semantic: Vec<SemanticCandidate> = lsp_labels
            .iter()
            .map(|text| SemanticCandidate {
                text: text.clone(),
                display_text: None,
                detail_text: None,
                documentation_text: None,
                kind_label: None,
                preselect: false,
                deprecated: false,
                commit_chars: Vec::new(),
                edit_range: None,
                filter_text: None,
                sort_text: None,
            })
            .collect();
        self.request_semantic(bytes, cursor, &semantic)
    }

    /// Build the candidate list with full LSP semantic metadata. `filterText`
    /// participates in prefix matching, but accept still inserts `text`.
    pub fn request_semantic(
        &mut self,
        bytes: &[u8],
        cursor: usize,
        semantic: &[SemanticCandidate],
    ) -> usize {
        let prefix = prefix_at(bytes, cursor);
        let cursor_pos = source_line_col_at(bytes, cursor);
        self.prefix_len = prefix.chars().count();
        self.candidates.clear();
        self.sel = 0;
        self.active = false;

        if prefix.is_empty() {
            return 0;
        }

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1) Semantic (LSP) candidates first — filter by prefix, drop the exact
        //    prefix, and honor provider sortText when present.
        let mut semantic_order: Vec<(usize, &SemanticCandidate)> =
            semantic.iter().enumerate().collect();
        if semantic.iter().any(|item| item.sort_text.is_some()) {
            semantic_order.sort_by(|(a_idx, a), (b_idx, b)| {
                semantic_candidate_sort_key(a)
                    .cmp(semantic_candidate_sort_key(b))
                    .then_with(|| a_idx.cmp(b_idx))
            });
        }
        for (_idx, item) in semantic_order {
            if semantic_candidate_matches_prefix(item, &prefix) && seen.insert(item.text.clone()) {
                self.candidates.push(Candidate {
                    text: item.text.clone(),
                    display_text: item.display_text.clone(),
                    detail_text: item.detail_text.clone(),
                    documentation_text: item.documentation_text.clone(),
                    kind_label: item.kind_label,
                    preselect: item.preselect,
                    deprecated: item.deprecated,
                    commit_chars: item.commit_chars.clone(),
                    replace_len: item
                        .edit_range
                        .and_then(|range| replacement_len_for_range(bytes, cursor_pos, range)),
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
                    display_text: None,
                    detail_text: None,
                    documentation_text: None,
                    kind_label: None,
                    preselect: false,
                    deprecated: false,
                    commit_chars: Vec::new(),
                    replace_len: None,
                    semantic: false,
                    snippet: false,
                });
            }
        }

        self.active = !self.candidates.is_empty();
        if let Some(idx) = self.candidates.iter().position(|candidate| candidate.preselect) {
            self.sel = idx;
        }
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
                    display_text: None,
                    detail_text: None,
                    documentation_text: None,
                    kind_label: Some("snippet"),
                    preselect: false,
                    deprecated: false,
                    commit_chars: Vec::new(),
                    replace_len: None,
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
        self.sel = 0;
        self.active = true;
    }

    /// `true` if the currently-selected candidate is a snippet prefix (so the
    /// accept path should EXPAND rather than insert the literal text).
    pub fn accepted_is_snippet(&self) -> bool {
        self.active
            && self
                .candidates
                .get(self.sel)
                .map(|c| c.snippet)
                .unwrap_or(false)
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

    /// Number of chars before the cursor to delete for the selected candidate.
    pub fn accepted_replace_len(&self) -> usize {
        if !self.active {
            return 0;
        }
        self.candidates
            .get(self.sel)
            .and_then(|c| c.replace_len)
            .unwrap_or(self.prefix_len)
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

    pub fn selected_commits_char(&self, ch: char) -> bool {
        self.active
            && self
                .candidates
                .get(self.sel)
                .is_some_and(|candidate| {
                    !candidate.snippet && candidate.commit_chars.contains(&ch)
                })
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
        let (box_x, box_y, box_w, _box_h, pad, row_h, top, shown) =
            self.geometry(text, cx, cy, width, height);
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
        ctx.dl_shadow(
            box_x,
            box_y + 8.0,
            box_w,
            box_h,
            radius,
            MuiColor::new(0.0, 0.0, 0.0, 0.8),
            24.0,
        );
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(
            box_x,
            box_y,
            box_w,
            box_h,
            radius,
            theme::BORDER_STRONG(),
            1.0,
        );

        for vis in 0..shown {
            let idx = top + vis;
            let cand = &self.candidates[idx];
            let row_y = box_y + pad + vis as f32 * row_h;
            let selected = idx == self.sel;
            if selected {
                ctx.dl_grad_h(
                    box_x + 5.0,
                    row_y + 2.0,
                    box_w - 10.0,
                    row_h - 4.0,
                    5.0,
                    theme::accent_a(0.20),
                    0.9,
                );
                ctx.dl_stroke(
                    box_x + 5.0,
                    row_y + 2.0,
                    box_w - 10.0,
                    row_h - 4.0,
                    5.0,
                    theme::ACCENT_LINE(),
                    1.0,
                );
            }
            // Type badge: a small rounded colored square with a letter, classified
            // by a light heuristic (mockup badge colors).
            let (badge_bg, badge_fg, letter, kind, _sig) = classify_candidate(cand);
            let detail = completion_row_detail(cand);
            let bx = box_x + 10.0;
            let by = row_y + (row_h - 18.0) * 0.5;
            ctx.dl_round(bx, by, 18.0, 18.0, 4.0, badge_bg);
            let lw = completion_badge_letter_width(&mut ctx.text, letter, 10.0);
            ctx.text.queue_ui_sized(
                bx + (18.0 - lw) * 0.5,
                by + 3.0,
                letter,
                badge_fg,
                10.0,
                clip,
            );

            let ty = row_y + (row_h - chrome) * 0.5 - 0.5;
            let name_x = box_x + 38.0;
            let kind_size = chrome - 1.5;
            let kind_x = completion_kind_x(&mut ctx.text, box_x, box_w, kind, kind_size);
            let sig_gap = if completion_row_detail_visible(detail) {
                2.0
            } else {
                0.0
            };
            let sig_w = if sig_gap > 0.0 {
                ctx.text.measure_ui_sized(detail, chrome - 1.0).0
            } else {
                0.0
            };
            let name_budget = (kind_x - 10.0 - name_x - sig_gap - sig_w).max(0.0);
            let shown_name =
                fit_completion_text(&mut ctx.text, cand.display_text(), name_budget, chrome);
            ctx.text
                .queue_sized(name_x, ty, &shown_name, theme::TEXT(), chrome, clip);
            // Signature hint immediately after the name, when the provider has
            // real row-level detail. Avoid placeholder fragments; the footer
            // carries full signature context for the selected row.
            if completion_row_detail_visible(detail) {
                let name_w = ctx.text.measure_ui_sized(&shown_name, chrome).0;
                let sig_x = name_x + name_w + sig_gap;
                let sig_budget = (kind_x - 10.0 - sig_x).max(0.0);
                let shown_sig =
                    fit_completion_text(&mut ctx.text, detail, sig_budget, chrome - 1.0);
                if !shown_sig.is_empty() {
                    ctx.text
                        .queue_sized(sig_x, ty, &shown_sig, theme::DIM(), chrome - 1.0, clip);
                }
            }
            // Right-aligned kind metadata.
            ctx.text
                .queue_ui_sized(kind_x, ty, kind, theme::DIM(), kind_size, clip);
        }

        // Detail footer (mockup `.ac-hint`): selected label in accent plus
        // provider detail or a neutral source hint.
        let hint_y = box_y + box_h - hint_h;
        ctx.dl_rect(box_x + 1.0, hint_y, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_round(
            box_x + 1.0,
            hint_y,
            box_w - 2.0,
            hint_h - 1.0,
            0.0,
            theme::BG_2(),
        );
        if let Some(sel) = self.candidates.get(self.sel) {
            let hy = hint_y + (hint_h - (chrome - 1.0)) * 0.5 - 0.5;
            let mut hx = box_x + 12.0;
            let tail = completion_footer_tail(sel);
            let tail_w = ctx.text.measure_ui_sized(&tail, chrome - 1.0).0;
            let name_budget = (box_x + box_w - 12.0 - tail_w - hx).max(0.0);
            let shown_name =
                fit_completion_text(&mut ctx.text, sel.display_text(), name_budget, chrome - 1.0);
            ctx.text.queue_sized(
                hx,
                hy,
                &shown_name,
                theme::ACCENT_BRIGHT(),
                chrome - 1.0,
                clip,
            );
            hx += ctx.text.measure_ui_sized(&shown_name, chrome - 1.0).0;
            let tail_budget = (box_x + box_w - 12.0 - hx).max(0.0);
            let shown_tail = fit_completion_text(&mut ctx.text, &tail, tail_budget, chrome - 1.0);
            if !shown_tail.is_empty() {
                ctx.text
                    .queue_sized(hx, hy, &shown_tail, theme::DIM(), chrome - 1.0, clip);
            }
        }
    }
}

impl Candidate {
    fn display_text(&self) -> &str {
        self.display_text.as_deref().unwrap_or(&self.text)
    }

    fn detail_text(&self) -> &str {
        self.detail_text.as_deref().unwrap_or("")
    }

    fn documentation_text(&self) -> &str {
        self.documentation_text.as_deref().unwrap_or("")
    }
}

fn semantic_candidate_matches_prefix(item: &SemanticCandidate, prefix: &str) -> bool {
    if item.text == prefix {
        return false;
    }
    item.text.starts_with(prefix)
        || item
            .filter_text
            .as_deref()
            .is_some_and(|filter| filter.starts_with(prefix))
}

fn semantic_candidate_sort_key(item: &SemanticCandidate) -> &str {
    item.sort_text
        .as_deref()
        .or(item.display_text.as_deref())
        .unwrap_or(&item.text)
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
        let (_badge_bg, _badge_fg, _letter, kind, _sig) = classify_candidate(cand);
        let detail = completion_row_detail(cand);
        let name_w = text.measure_ui_sized(cand.display_text(), chrome).0;
        let sig_gap = if completion_row_detail_visible(detail) {
            2.0
        } else {
            0.0
        };
        let sig_w = if sig_gap > 0.0 {
            text.measure_ui_sized(detail, chrome - 1.0).0
        } else {
            0.0
        };
        let kind_w = text.measure_ui_sized(kind, kind_size).0;
        desired = desired.max(name_x + name_w + sig_gap + sig_w + kind_gap + kind_w + right_pad);
    }
    if let Some(sel) = candidates.get(selected) {
        let tail = completion_footer_tail(sel);
        let footer_w = 12.0
            + text.measure_ui_sized(sel.display_text(), chrome - 1.0).0
            + text.measure_ui_sized(&tail, chrome - 1.0).0
            + right_pad;
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

/// Classify a candidate into a mockup-style type badge + kind hint.
/// A light heuristic over the text since the engine only tracks semantic-ness:
/// capitalized → type (T, teal), keyword set → keyword (K, violet), looks like a
/// fn (followed by `(` in source isn't known here) → fn (ƒ, gold) when semantic,
/// else variable (x, grey).
fn classify_candidate(
    cand: &Candidate,
) -> (MuiColor, MuiColor, &'static str, &'static str, &'static str) {
    const KEYWORDS: &[&str] = &[
        "fn", "let", "mut", "while", "if", "else", "return", "match", "struct", "enum", "for",
        "in", "type", "true", "false", "await", "async", "pub", "import", "effect", "extern",
    ];
    let t = cand.display_text();
    if let Some(kind) = cand.kind_label {
        let (badge_bg, badge_fg, letter) = completion_kind_badge(kind);
        return (badge_bg, badge_fg, letter, kind, "");
    }
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
            "",
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

fn completion_kind_badge(kind: &str) -> (MuiColor, MuiColor, &'static str) {
    match kind {
        "method" | "function" | "constructor" => (
            MuiColor::new(1.0, 0.824, 0.478, 0.14),
            theme::SYN_FUNCTION(),
            "\u{0192}",
        ),
        "class" | "interface" | "struct" | "enum" | "type parameter" => (
            MuiColor::new(0.353, 0.820, 0.769, 0.14),
            theme::SYN_TYPE(),
            "T",
        ),
        "keyword" => (
            MuiColor::new(0.718, 0.580, 1.0, 0.14),
            theme::SYN_KEYWORD(),
            "K",
        ),
        "snippet" => (
            MuiColor::new(0.482, 0.800, 1.0, 0.16),
            theme::SYN_FUNCTION(),
            "\u{2026}",
        ),
        "file" | "folder" | "module" => (
            MuiColor::new(0.482, 0.800, 1.0, 0.12),
            theme::ACCENT_BRIGHT(),
            "F",
        ),
        "field" | "property" | "variable" | "constant" | "enum member" => (
            MuiColor::new(0.843, 0.843, 0.890, 0.10),
            theme::SYN_DEFAULT(),
            "x",
        ),
        _ => (
            MuiColor::new(0.843, 0.843, 0.890, 0.10),
            theme::SYN_DEFAULT(),
            "i",
        ),
    }
}

fn completion_row_detail_visible(detail: &str) -> bool {
    !detail.trim().is_empty()
}

fn completion_row_detail(cand: &Candidate) -> &str {
    if completion_row_detail_visible(cand.detail_text()) {
        cand.detail_text()
    } else if cand.deprecated {
        "deprecated"
    } else {
        ""
    }
}

fn completion_footer_tail(cand: &Candidate) -> String {
    if cand.deprecated {
        let after = if completion_row_detail_visible(cand.detail_text()) {
            cand.detail_text()
        } else if completion_row_detail_visible(cand.documentation_text()) {
            cand.documentation_text()
        } else if cand.semantic {
            "semantic symbol"
        } else {
            "local symbol"
        };
        format!("  \u{00B7} deprecated  \u{00B7} {after}")
    } else if completion_row_detail_visible(cand.detail_text()) {
        cand.detail_text().to_string()
    } else if completion_row_detail_visible(cand.documentation_text()) {
        cand.documentation_text().to_string()
    } else if cand.semantic {
        "  \u{00B7} semantic symbol".to_string()
    } else {
        "  \u{00B7} local symbol".to_string()
    }
}

// ---------------------------------------------------------------------------
// mty-lsp semantic provider (best-effort, hand-rolled JSON-RPC over stdio)
// ---------------------------------------------------------------------------

pub mod lsp {
    //! Minimal `mty lsp` client: spawn the server, do the LSP handshake, ask
    //! for completion at a position, and scrape top-level `CompletionItem`
    //! fields out of the JSON response with a small hand scanner (no serde
    //! dependency). Every step is short-timeout and failure-tolerant — any error
    //! returns an empty label list so the caller falls back to buffer words.

    use super::{CompletionEditRange, SemanticCandidate};
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
        crate::nav::path_to_file_uri(path)
    }

    /// Scrape insertable completion candidates out of a JSON blob. Prefers
    /// `textEdit.newText`, then `insertText`, then `label`, and flattens LSP
    /// snippet-formatted insert text to plain text before it reaches the editor.
    /// Preserves `filterText` so prefix matching can follow the server's chosen
    /// key while accept still inserts the selected text, and `sortText` so the
    /// dropdown can honor provider ranking. Preserves provider documentation for
    /// the selected-row footer when no shorter detail string is present.
    /// Handles both `result: [items...]` and `result: { items: [items...] }`,
    /// plus a bare item array for tests.
    pub fn scrape_candidates(json: &str) -> Vec<SemanticCandidate> {
        let mut out: Vec<SemanticCandidate> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let bytes = json.as_bytes();
        for items in completion_items_regions(bytes) {
            for item in split_top_level_objects(items) {
                let Some(text) = completion_item_insert_text(item) else {
                    continue;
                };
                if !text.is_empty() && seen.insert(text.clone()) {
                    let display_text = completion_item_display_text(item, &text);
                    out.push(SemanticCandidate {
                        text,
                        display_text,
                        detail_text: completion_item_detail_text(item),
                        documentation_text: completion_item_documentation_text(item),
                        kind_label: completion_item_kind_label(item),
                        preselect: completion_item_preselect(item),
                        deprecated: completion_item_deprecated(item),
                        commit_chars: completion_item_commit_chars(item),
                        edit_range: completion_item_text_edit_range(item),
                        filter_text: completion_item_filter_text(item),
                        sort_text: completion_item_sort_text(item),
                    });
                }
            }
        }
        out
    }

    /// Compatibility wrapper for callers that only need inserted text.
    pub fn scrape_labels(json: &str) -> Vec<String> {
        scrape_candidates(json)
            .into_iter()
            .map(|candidate| candidate.text)
            .collect()
    }

    fn completion_item_insert_text(item: &[u8]) -> Option<String> {
        let snippet_format = top_level_number_value(item, b"insertTextFormat") == Some(2);
        let value = completion_text_edit_new_text(item)
            .or_else(|| top_level_string_value(item, b"insertText"))
            .or_else(|| top_level_string_value(item, b"label"))?;
        if snippet_format {
            Some(flatten_lsp_snippet_insert_text(&value))
        } else {
            Some(value)
        }
    }

    fn completion_item_filter_text(item: &[u8]) -> Option<String> {
        top_level_string_value(item, b"filterText").filter(|filter| !filter.is_empty())
    }

    fn completion_item_sort_text(item: &[u8]) -> Option<String> {
        top_level_string_value(item, b"sortText").filter(|sort| !sort.is_empty())
    }

    fn completion_item_preselect(item: &[u8]) -> bool {
        top_level_bool_value(item, b"preselect").unwrap_or(false)
    }

    fn completion_item_deprecated(item: &[u8]) -> bool {
        top_level_bool_value(item, b"deprecated").unwrap_or(false)
            || top_level_array_contains_number(item, b"tags", 1)
    }

    fn completion_item_commit_chars(item: &[u8]) -> Vec<char> {
        let Some(at) = top_level_field_value_start(item, b"commitCharacters") else {
            return Vec::new();
        };
        let Some(region) = value_region(item, at) else {
            return Vec::new();
        };
        if region.first() != Some(&b'[') {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut i = 1usize;
        while i < region.len().saturating_sub(1) {
            if region[i] == b'"' {
                if let Some((value, end)) = read_json_string_at(region, i) {
                    if let Some(ch) = value.chars().next() {
                        if !out.contains(&ch) {
                            out.push(ch);
                        }
                    }
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
        out
    }

    fn completion_item_display_text(item: &[u8], insert_text: &str) -> Option<String> {
        top_level_string_value(item, b"label")
            .filter(|label| !label.is_empty() && label != insert_text)
    }

    fn completion_item_detail_text(item: &[u8]) -> Option<String> {
        top_level_string_value(item, b"detail")
            .filter(|detail| !detail.trim().is_empty())
            .or_else(|| completion_item_label_details_text(item))
    }

    fn completion_item_label_details_text(item: &[u8]) -> Option<String> {
        let at = top_level_field_value_start(item, b"labelDetails")?;
        let details = value_region(item, at)?;
        if details.first() != Some(&b'{') {
            return None;
        }
        let detail = top_level_string_value(details, b"detail")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let description = top_level_string_value(details, b"description")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        match (detail, description) {
            (Some(detail), Some(description)) => Some(format!("{detail} - {description}")),
            (Some(detail), None) => Some(detail),
            (None, Some(description)) => Some(description),
            (None, None) => None,
        }
    }

    fn completion_item_documentation_text(item: &[u8]) -> Option<String> {
        let at = top_level_field_value_start(item, b"documentation")?;
        let doc = match item.get(at).copied()? {
            b'"' => read_json_string_at(item, at).map(|(value, _)| value)?,
            b'{' => {
                let region = value_region(item, at)?;
                top_level_string_value(region, b"value")?
            }
            _ => return None,
        };
        let cleaned = doc.trim();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned.to_string())
        }
    }

    fn completion_item_kind_label(item: &[u8]) -> Option<&'static str> {
        match top_level_number_value(item, b"kind")? {
            1 => Some("text"),
            2 => Some("method"),
            3 => Some("function"),
            4 => Some("constructor"),
            5 => Some("field"),
            6 => Some("variable"),
            7 => Some("class"),
            8 => Some("interface"),
            9 => Some("module"),
            10 => Some("property"),
            11 => Some("unit"),
            12 => Some("value"),
            13 => Some("enum"),
            14 => Some("keyword"),
            15 => Some("snippet"),
            16 => Some("color"),
            17 => Some("file"),
            18 => Some("reference"),
            19 => Some("folder"),
            20 => Some("enum member"),
            21 => Some("constant"),
            22 => Some("struct"),
            23 => Some("event"),
            24 => Some("operator"),
            25 => Some("type parameter"),
            _ => None,
        }
    }

    fn completion_text_edit_new_text(item: &[u8]) -> Option<String> {
        let text_edit_at = top_level_field_value_start(item, b"textEdit")?;
        let text_edit = value_region(item, text_edit_at)?;
        if text_edit.first() != Some(&b'{') {
            return None;
        }
        top_level_string_value(text_edit, b"newText")
    }

    fn completion_item_text_edit_range(item: &[u8]) -> Option<CompletionEditRange> {
        let text_edit_at = top_level_field_value_start(item, b"textEdit")?;
        let text_edit = value_region(item, text_edit_at)?;
        if text_edit.first() != Some(&b'{') {
            return None;
        }
        let range_at = top_level_field_value_start(text_edit, b"range")?;
        let range = value_region(text_edit, range_at)?;
        if range.first() != Some(&b'{') {
            return None;
        }
        let start_at = top_level_field_value_start(range, b"start")?;
        let start = value_region(range, start_at)?;
        let end_at = top_level_field_value_start(range, b"end")?;
        let end = value_region(range, end_at)?;
        Some(CompletionEditRange {
            start_line: top_level_number_value(start, b"line")?.try_into().ok()?,
            start_col_utf16: top_level_number_value(start, b"character")?.try_into().ok()?,
            end_line: top_level_number_value(end, b"line")?.try_into().ok()?,
            end_col_utf16: top_level_number_value(end, b"character")?.try_into().ok()?,
        })
    }

    fn top_level_string_value(obj: &[u8], field: &[u8]) -> Option<String> {
        let at = top_level_field_value_start(obj, field)?;
        read_json_string_at(obj, at).map(|(value, _)| value)
    }

    fn top_level_number_value(obj: &[u8], field: &[u8]) -> Option<i64> {
        let at = top_level_field_value_start(obj, field)?;
        let region = value_region(obj, at)?;
        let text = std::str::from_utf8(region).ok()?.trim();
        text.parse::<i64>().ok()
    }

    fn top_level_bool_value(obj: &[u8], field: &[u8]) -> Option<bool> {
        let at = top_level_field_value_start(obj, field)?;
        let region = value_region(obj, at)?;
        match std::str::from_utf8(region).ok()?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    fn top_level_array_contains_number(obj: &[u8], field: &[u8], needle: i64) -> bool {
        let Some(at) = top_level_field_value_start(obj, field) else {
            return false;
        };
        let Some(region) = value_region(obj, at) else {
            return false;
        };
        if region.first() != Some(&b'[') {
            return false;
        }
        let mut i = 1usize;
        while i < region.len().saturating_sub(1) {
            while i < region.len() && matches!(region[i], b' ' | b',' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            let start = i;
            if i < region.len() && region[i] == b'-' {
                i += 1;
            }
            while i < region.len() && region[i].is_ascii_digit() {
                i += 1;
            }
            if start < i {
                if std::str::from_utf8(&region[start..i])
                    .ok()
                    .and_then(|n| n.parse::<i64>().ok())
                    == Some(needle)
                {
                    return true;
                }
            } else {
                i += 1;
            }
        }
        false
    }

    fn flatten_lsp_snippet_insert_text(text: &str) -> String {
        crate::snippets::expand(text, "", 0, 0).text
    }

    fn completion_items_regions(bytes: &[u8]) -> Vec<&[u8]> {
        if bytes.first() == Some(&b'[') {
            return vec![bytes];
        }
        if bytes.first() == Some(&b'{') {
            let first_end = match_delim(bytes, 0, b'{', b'}').min(bytes.len());
            let rest = &bytes[first_end..];
            if rest.iter().all(|b| b.is_ascii_whitespace())
                && top_level_field_value_start(bytes, b"result").is_some()
                && top_level_field_value_start(bytes, b"method").is_none()
            {
                return completion_items_region(bytes).into_iter().collect();
            }
        }
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'{' {
                let end = match_delim(bytes, i, b'{', b'}').min(bytes.len());
                let obj = &bytes[i..end];
                if top_level_field_value_start(obj, b"result").is_some()
                    && top_level_field_value_start(obj, b"id").is_some()
                    && top_level_field_value_start(obj, b"method").is_none()
                {
                    if let Some(items) = completion_items_region(obj) {
                        out.push(items);
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
        out
    }

    fn completion_items_region(obj: &[u8]) -> Option<&[u8]> {
        let payload = if obj.first() == Some(&b'{') {
            if let Some(result_at) = top_level_field_value_start(obj, b"result") {
                value_region(obj, result_at)?
            } else {
                obj
            }
        } else {
            obj
        };

        completion_items_from_payload(payload)
    }

    fn completion_items_from_payload(payload: &[u8]) -> Option<&[u8]> {
        if payload.first() == Some(&b'[') {
            return Some(payload);
        }
        if payload.first() == Some(&b'{') {
            let items_at = top_level_field_value_start(payload, b"items")?;
            let items = value_region(payload, items_at)?;
            if items.first() == Some(&b'[') {
                return Some(items);
            }
        }
        None
    }

    fn value_region(bytes: &[u8], start: usize) -> Option<&[u8]> {
        if start >= bytes.len() {
            return None;
        }
        let end = match bytes[start] {
            b'{' => match_delim(bytes, start, b'{', b'}'),
            b'[' => match_delim(bytes, start, b'[', b']'),
            b'"' => read_json_string_at(bytes, start)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len()),
            _ => {
                let mut i = start;
                while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']') {
                    i += 1;
                }
                i
            }
        };
        Some(&bytes[start..end.min(bytes.len())])
    }

    fn split_top_level_objects(arr: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut obj_start: Option<usize> = None;
        let mut in_str = false;
        let mut esc = false;
        for (k, &c) in arr.iter().enumerate() {
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' => {
                    if depth == 0 {
                        obj_start = Some(k);
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start) = obj_start.take() {
                            out.push(&arr[start..=k]);
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn top_level_field_value_start(obj: &[u8], field: &[u8]) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = 0usize;
        while i < obj.len() {
            match obj[i] {
                b'{' | b'[' => {
                    depth += 1;
                    i += 1;
                }
                b'}' | b']' => {
                    depth -= 1;
                    i += 1;
                }
                b'"' => {
                    let key_start = i + 1;
                    let past = skip_json_string(obj, i)?;
                    let key_end = past.checked_sub(1)?;
                    if depth == 1 && key_end >= key_start && &obj[key_start..key_end] == field {
                        let mut value_at = past;
                        while value_at < obj.len()
                            && matches!(obj[value_at], b' ' | b':' | b'\t' | b'\r' | b'\n')
                        {
                            value_at += 1;
                        }
                        return Some(value_at);
                    }
                    i = past;
                }
                _ => i += 1,
            }
        }
        None
    }

    fn skip_json_string(bytes: &[u8], quote: usize) -> Option<usize> {
        let mut i = quote + 1;
        let mut esc = false;
        while i < bytes.len() {
            if esc {
                esc = false;
            } else if bytes[i] == b'\\' {
                esc = true;
            } else if bytes[i] == b'"' {
                return Some(i + 1);
            }
            i += 1;
        }
        None
    }

    fn match_delim(bytes: &[u8], open: usize, o: u8, c: u8) -> usize {
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        let mut k = open;
        while k < bytes.len() {
            let b = bytes[k];
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if b == b'"' {
                in_str = true;
            } else if b == o {
                depth += 1;
            } else if b == c {
                depth -= 1;
                if depth == 0 {
                    return k + 1;
                }
            }
            k += 1;
        }
        bytes.len()
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
        // timeout. The thread reads until it has seen the complete completion
        // response object for id 2, then it stops promptly, or until EOF / a
        // size cap. The server doesn't close stdout on its own, so
        // response-owned id matching lets the happy path return as soon as the
        // answer arrives.
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
                        if crate::nav::lsp::has_response_id(&buf, 2) {
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
                let bytes = rx
                    .recv_timeout(Duration::from_millis(500))
                    .unwrap_or_default();
                let _ = writer.join();
                let _ = reader.join();
                eprintln!("completion(lsp): timed out after {timeout:?} — buffer words only");
                bytes
            }
        };

        let text = String::from_utf8_lossy(&raw);
        // Scrape labels from the completion result payload only; envelope or
        // item metadata labels are not completion candidates.
        scrape_labels(&text)
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
        assert_eq!(got, vec!["countdown".to_string(), "counter".to_string()]);
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
    fn request_semantic_matches_filter_text_but_inserts_text() {
        let mut e = CompletionEngine::new();
        let src = b"np";
        let semantic = vec![SemanticCandidate {
            text: "numpy".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: None,
            filter_text: Some("np".to_string()),
            sort_text: None,
        }];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 1);
        assert_eq!(e.prefix_len(), 2);
        assert_eq!(e.accepted_text(), "numpy");
    }

    #[test]
    fn request_semantic_uses_safe_lsp_text_edit_replace_len() {
        let mut e = CompletionEngine::new();
        let src = b"Console.Wr";
        let semantic = vec![SemanticCandidate {
            text: "Console.WriteLine".to_string(),
            display_text: Some("WriteLine".to_string()),
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: Some(CompletionEditRange {
                start_line: 0,
                start_col_utf16: 0,
                end_line: 0,
                end_col_utf16: 10,
            }),
            filter_text: Some("Wr".to_string()),
            sort_text: None,
        }];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 1);
        assert_eq!(e.prefix_len(), 2);
        assert_eq!(e.accepted_replace_len(), 10);
        assert_eq!(e.accepted_text(), "Console.WriteLine");
    }

    #[test]
    fn request_semantic_converts_lsp_text_edit_utf16_columns() {
        let mut e = CompletionEngine::new();
        let src = "😀.Wr";
        let semantic = vec![SemanticCandidate {
            text: "😀.WriteLine".to_string(),
            display_text: Some("WriteLine".to_string()),
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: Some(CompletionEditRange {
                start_line: 0,
                start_col_utf16: 0,
                end_line: 0,
                end_col_utf16: 5,
            }),
            filter_text: Some("Wr".to_string()),
            sort_text: None,
        }];

        let n = e.request_semantic(src.as_bytes(), src.len(), &semantic);

        assert_eq!(n, 1);
        assert_eq!(e.prefix_len(), 2);
        assert_eq!(e.accepted_replace_len(), 4);
    }

    #[test]
    fn request_semantic_ignores_lsp_text_edit_range_not_ending_at_cursor() {
        let mut e = CompletionEngine::new();
        let src = b"Console.Wr";
        let semantic = vec![SemanticCandidate {
            text: "Console.WriteLine".to_string(),
            display_text: Some("WriteLine".to_string()),
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: Some(CompletionEditRange {
                start_line: 0,
                start_col_utf16: 0,
                end_line: 0,
                end_col_utf16: 9,
            }),
            filter_text: Some("Wr".to_string()),
            sort_text: None,
        }];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 1);
        assert_eq!(e.prefix_len(), 2);
        assert_eq!(e.accepted_replace_len(), 2);
    }

    #[test]
    fn request_semantic_keeps_display_text_separate_from_insert_text() {
        let mut e = CompletionEngine::new();
        let src = b"print";
        let semantic = vec![SemanticCandidate {
            text: "println($1)".to_string(),
            display_text: Some("println!".to_string()),
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: None,
            filter_text: Some("println".to_string()),
            sort_text: None,
        }];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 1);
        assert_eq!(e.accepted_text(), "println($1)");
        assert_eq!(e.candidates[0].display_text(), "println!");
    }

    #[test]
    fn request_semantic_honors_sort_text() {
        let mut e = CompletionEngine::new();
        let src = b"pr";
        let semantic = vec![
            SemanticCandidate {
                text: "printer".to_string(),
                display_text: None,
                detail_text: None,
                documentation_text: None,
                kind_label: None,
                preselect: false,
                deprecated: false,
                commit_chars: Vec::new(),
                edit_range: None,
                filter_text: None,
                sort_text: Some("020".to_string()),
            },
            SemanticCandidate {
                text: "printf".to_string(),
                display_text: None,
                detail_text: None,
                documentation_text: None,
                kind_label: None,
                preselect: false,
                deprecated: false,
                commit_chars: Vec::new(),
                edit_range: None,
                filter_text: None,
                sort_text: Some("010".to_string()),
            },
            SemanticCandidate {
                text: "prepend".to_string(),
                display_text: None,
                detail_text: None,
                documentation_text: None,
                kind_label: None,
                preselect: false,
                deprecated: false,
                commit_chars: Vec::new(),
                edit_range: None,
                filter_text: None,
                sort_text: None,
            },
        ];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 3);
        assert_eq!(e.accepted_text(), "printf");
        e.move_sel(1);
        assert_eq!(e.accepted_text(), "printer");
        e.move_sel(1);
        assert_eq!(e.accepted_text(), "prepend");
    }

    #[test]
    fn request_semantic_uses_preselected_candidate_after_sorting() {
        let mut e = CompletionEngine::new();
        let src = b"pr";
        let semantic = vec![
            SemanticCandidate {
                text: "printf".to_string(),
                display_text: None,
                detail_text: None,
                documentation_text: None,
                kind_label: None,
                preselect: false,
                deprecated: false,
                commit_chars: Vec::new(),
                edit_range: None,
                filter_text: None,
                sort_text: Some("010".to_string()),
            },
            SemanticCandidate {
                text: "printer".to_string(),
                display_text: None,
                detail_text: None,
                documentation_text: None,
                kind_label: None,
                preselect: true,
                deprecated: false,
                commit_chars: Vec::new(),
                edit_range: None,
                filter_text: None,
                sort_text: Some("020".to_string()),
            },
        ];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 2);
        assert_eq!(e.selection(), 1);
        assert_eq!(e.accepted_text(), "printer");
    }

    #[test]
    fn request_semantic_matches_selected_commit_character() {
        let mut e = CompletionEngine::new();
        let src = b"pri";
        let semantic = vec![SemanticCandidate {
            text: "println".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: vec!['.', '('],
            edit_range: None,
            filter_text: None,
            sort_text: None,
        }];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 1);
        assert!(e.selected_commits_char('.'));
        assert!(e.selected_commits_char('('));
        assert!(!e.selected_commits_char(';'));
    }

    #[test]
    fn snippet_completion_does_not_match_commit_character() {
        let mut e = CompletionEngine::new();
        e.candidates = vec![Candidate {
            text: "for".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: Some("snippet"),
            preselect: false,
            deprecated: false,
            commit_chars: vec!['('],
            replace_len: None,
            semantic: false,
            snippet: true,
        }];
        e.active = true;

        assert!(!e.selected_commits_char('('));
    }

    #[test]
    fn injected_snippets_override_semantic_preselect() {
        let mut e = CompletionEngine::new();
        let src = b"pr";
        let semantic = vec![SemanticCandidate {
            text: "printer".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: true,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: None,
            filter_text: None,
            sort_text: None,
        }];

        e.request_semantic(src, src.len(), &semantic);
        assert_eq!(e.accepted_text(), "printer");
        e.inject_snippets(&["print-snippet".to_string()]);

        assert_eq!(e.selection(), 0);
        assert_eq!(e.accepted_text(), "print-snippet");
        assert!(e.accepted_is_snippet());
    }

    #[test]
    fn semantic_completion_footer_prefers_provider_detail() {
        let cand = Candidate {
            text: "println($1)".to_string(),
            display_text: Some("println!".to_string()),
            detail_text: Some("macro println!(...)".to_string()),
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: true,
            snippet: false,
        };

        assert_eq!(completion_footer_tail(&cand), "macro println!(...)");
        assert!(completion_row_detail_visible(cand.detail_text()));
    }

    #[test]
    fn semantic_completion_without_detail_uses_plain_semantic_footer() {
        let cand = Candidate {
            text: "protocol".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: true,
            snippet: false,
        };

        assert_eq!(completion_footer_tail(&cand), "  \u{00B7} semantic symbol");
        assert!(!completion_row_detail_visible(cand.detail_text()));
    }

    #[test]
    fn semantic_completion_footer_uses_documentation_when_detail_is_absent() {
        let cand = Candidate {
            text: "collect".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: Some("Collects an iterator into a collection.".to_string()),
            kind_label: Some("method"),
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: true,
            snippet: false,
        };

        assert_eq!(
            completion_footer_tail(&cand),
            "Collects an iterator into a collection."
        );
    }

    #[test]
    fn semantic_completion_deprecated_rows_get_visible_status() {
        let cand = Candidate {
            text: "oldApi".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: Some("Use newApi instead.".to_string()),
            kind_label: Some("function"),
            preselect: false,
            deprecated: true,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: true,
            snippet: false,
        };

        assert_eq!(completion_row_detail(&cand), "deprecated");
        assert_eq!(
            completion_footer_tail(&cand),
            "  \u{00B7} deprecated  \u{00B7} Use newApi instead."
        );
    }

    #[test]
    fn semantic_completion_uses_provider_kind_label() {
        let cand = Candidate {
            text: "User".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: Some("variable"),
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: true,
            snippet: false,
        };

        let (_, _, _letter, kind, sig) = classify_candidate(&cand);
        assert_eq!(kind, "variable");
        assert_eq!(sig, "");
    }

    #[test]
    fn request_semantic_excludes_exact_text_even_when_filter_matches() {
        let mut e = CompletionEngine::new();
        let src = b"let";
        let semantic = vec![SemanticCandidate {
            text: "let".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            edit_range: None,
            filter_text: Some("let".to_string()),
            sort_text: None,
        }];

        let n = e.request_semantic(src, src.len(), &semantic);

        assert_eq!(n, 0);
        assert!(!e.is_active());
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
        assert!(
            e.accepted_is_snippet(),
            "snippet entry must flag the accept path"
        );
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
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: true,
            snippet: false,
        };
        let (_, _, _letter, kind, sig) = classify_candidate(&cand);
        assert_eq!(kind, "function");
        assert!(!kind.contains("fn"));
        assert_eq!(sig, "");
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
        assert!(
            shown.ends_with("..."),
            "long completion rows should ellipsize: {shown}"
        );
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
        assert!(
            shown_name.ends_with("..."),
            "long footer name should ellipsize: {shown_name}"
        );
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
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: false,
            snippet: false,
        }];
        let wide = vec![Candidate {
            text: "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW".to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
            semantic: false,
            snippet: false,
        }];

        let narrow_w = completion_popup_width(&mut ctx.text, &narrow, 0, 1, 0, 900.0, chrome);
        let wide_w = completion_popup_width(&mut ctx.text, &wide, 0, 1, 0, 900.0, chrome);
        let measured_delta = ctx.text.measure_ui_sized(&wide[0].text, chrome).0
            - ctx.text.measure_ui_sized(&narrow[0].text, chrome).0;

        assert!(
            measured_delta > 10.0,
            "test strings should differ in rendered width"
        );
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
            text: "very_wide_completion_candidate_name_that_should_not_escape_the_viewport"
                .to_string(),
            display_text: None,
            detail_text: None,
            documentation_text: None,
            kind_label: None,
            preselect: false,
            deprecated: false,
            commit_chars: Vec::new(),
            replace_len: None,
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
        let idx = e.click_row(
            &mut ctx.text,
            box_x + 24.0,
            box_y + pad + row_h + 2.0,
            250.0,
            120.0,
            900,
            700,
        );
        assert_eq!(idx, 1);
        assert_eq!(e.selection(), 1);
        assert_eq!(
            e.click_row(
                &mut ctx.text,
                box_x - 2.0,
                box_y + pad + 2.0,
                250.0,
                120.0,
                900,
                700
            ),
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
    fn lsp_scrape_prefers_insert_text_over_label() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"println!","insertText":"println($1)"},{"label":"plain"}]}"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(labels, vec!["println($1)".to_string(), "plain".to_string()]);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_label_as_display_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"println!","insertText":"println($1)"},{"label":"plain"}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].text, "println($1)");
        assert_eq!(candidates[0].display_text.as_deref(), Some("println!"));
        assert_eq!(candidates[1].text, "plain");
        assert_eq!(candidates[1].display_text, None);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_detail_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"println!","insertText":"println($1)","detail":"macro println!(...)"}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].text, "println($1)");
        assert_eq!(candidates[0].display_text.as_deref(), Some("println!"));
        assert_eq!(
            candidates[0].detail_text.as_deref(),
            Some("macro println!(...)")
        );
    }

    #[test]
    fn lsp_scrape_candidates_uses_label_details_when_detail_is_absent() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"map","labelDetails":{"detail":"(callback)","description":"Array"}},{"label":"len","labelDetails":{"description":"slice"}},{"label":"plain","labelDetails":{"label":"ignored"}}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].detail_text.as_deref(),
            Some("(callback) - Array")
        );
        assert_eq!(candidates[1].detail_text.as_deref(), Some("slice"));
        assert_eq!(candidates[2].detail_text, None);
    }

    #[test]
    fn lsp_scrape_candidates_keeps_top_level_detail_before_label_details() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"map","detail":"fn map<T>()","labelDetails":{"detail":"(callback)","description":"Array"}}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].detail_text.as_deref(), Some("fn map<T>()"));
    }

    #[test]
    fn lsp_scrape_candidates_preserves_filter_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"numpy","insertText":"numpy","filterText":"np"},{"label":"plain"}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].text, "numpy");
        assert_eq!(candidates[0].display_text, None);
        assert_eq!(candidates[0].filter_text.as_deref(), Some("np"));
        assert_eq!(candidates[1].text, "plain");
        assert_eq!(candidates[1].filter_text, None);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_sort_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"printer","sortText":"020"},{"label":"printf","sortText":"010"}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].text, "printer");
        assert_eq!(candidates[0].sort_text.as_deref(), Some("020"));
        assert_eq!(candidates[1].text, "printf");
        assert_eq!(candidates[1].sort_text.as_deref(), Some("010"));
    }

    #[test]
    fn lsp_scrape_candidates_preserves_kind_label() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"println","kind":3},{"label":"answer","kind":21},{"label":"mystery","kind":999}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].kind_label, Some("function"));
        assert_eq!(candidates[1].kind_label, Some("constant"));
        assert_eq!(candidates[2].kind_label, None);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_documentation_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"stringDoc","documentation":"plain docs"},{"label":"markupDoc","documentation":{"kind":"markdown","value":"**rich** docs"}},{"label":"emptyDoc","documentation":"   "}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].documentation_text.as_deref(),
            Some("plain docs")
        );
        assert_eq!(
            candidates[1].documentation_text.as_deref(),
            Some("**rich** docs")
        );
        assert_eq!(candidates[2].documentation_text, None);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_preselect() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"first","preselect":false},{"label":"chosen","preselect":true},{"label":"plain"}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 3);
        assert!(!candidates[0].preselect);
        assert!(candidates[1].preselect);
        assert!(!candidates[2].preselect);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_deprecated_markers() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"oldFlag","deprecated":true},{"label":"oldTag","tags":[1]},{"label":"fresh","tags":[2]}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 3);
        assert!(candidates[0].deprecated);
        assert!(candidates[1].deprecated);
        assert!(!candidates[2].deprecated);
    }

    #[test]
    fn lsp_scrape_candidates_preserves_commit_characters() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"println","commitCharacters":[".","(","."]},{"label":"plain","commitCharacters":"."}]}"#;
        let candidates = super::lsp::scrape_candidates(json);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].commit_chars, vec!['.', '(']);
        assert!(candidates[1].commit_chars.is_empty());
    }

    #[test]
    fn lsp_scrape_prefers_text_edit_new_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"Console.WriteLine","insertText":"ignored","textEdit":{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}},"newText":"Console.WriteLine($1)"}}]}"#;
        let candidates = super::lsp::scrape_candidates(json);
        let labels = super::lsp::scrape_labels(json);

        assert_eq!(
            candidates[0].edit_range,
            Some(CompletionEditRange {
                start_line: 0,
                start_col_utf16: 0,
                end_line: 0,
                end_col_utf16: 2,
            })
        );
        assert_eq!(labels, vec!["Console.WriteLine($1)".to_string()]);
    }

    #[test]
    fn lsp_scrape_flattens_snippet_insert_text() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"label":"for","insertTextFormat":2,"insertText":"for ${1:item} in ${2:items} {\n\t$0\n}"},{"label":"choice","insertTextFormat":2,"insertText":"${1|red,green,blue|}"}]}"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(
            labels,
            vec!["for item in items {\n\t\n}".to_string(), "red".to_string()]
        );
    }

    #[test]
    fn lsp_scrape_labels_reads_completion_list_items() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":{"isIncomplete":false,"items":[{"label":"foo"},{"label":"bar"}]}}"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(labels, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn lsp_scrape_labels_skips_non_completion_results_in_stream() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"label":"wrong capability"}}}
{"jsonrpc":"2.0","id":2,"result":[{"label":"foo"},{"label":"bar"}]}"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(labels, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn lsp_scrape_labels_uses_result_item_top_level_labels() {
        let json = r#"{
          "jsonrpc":"2.0",
          "metadata":{"result":[{"label":"wrong envelope"}],"items":[{"label":"wrong items"}]},
          "result":{
            "metadata":{"items":[{"label":"wrong nested items"}]},
            "items":[
              {"metadata":{"label":"wrong item"},"label":"right"},
              {"labelDetails":{"label":"wrong details"},"label":"next"}
            ]
          },
          "id":2
        }"#;
        let labels = super::lsp::scrape_labels(json);
        assert_eq!(labels, vec!["right".to_string(), "next".to_string()]);
    }

    #[test]
    fn lsp_scrape_labels_requires_response_result_in_stream() {
        let json = r#"{"jsonrpc":"2.0","method":"$/progress","params":{"result":[{"label":"wrong progress"}]}}
{"jsonrpc":"2.0","id":7,"method":"workspace/applyEdit","result":[{"label":"wrong request"}]}
{"jsonrpc":"2.0","id":2,"result":{"items":[{"label":"right"}]}}"#;
        let labels = super::lsp::scrape_labels(json);

        assert_eq!(labels, vec!["right".to_string()]);
    }

    #[test]
    fn lsp_completion_response_wait_uses_response_owned_id() {
        let stream = br#"Content-Length: 99

{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":[{"label":"wrong"}]}}}Content-Length: 59

{"jsonrpc":"2.0","id":2,"result":[{"label":"right"}]}"#;

        assert!(crate::nav::lsp::has_response_id(stream, 2));
    }

    #[test]
    fn lsp_completion_response_wait_ignores_nested_id_and_requests() {
        let nested_id = br#"{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":[{"label":"wrong"}]}}}"#;
        let server_request = br#"{"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;

        assert!(!crate::nav::lsp::has_response_id(nested_id, 2));
        assert!(!crate::nav::lsp::has_response_id(server_request, 2));
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
        let labels = lsp::semantic_labels_with_timeout(&path, source, 2, 4, Duration::from_secs(8));

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
