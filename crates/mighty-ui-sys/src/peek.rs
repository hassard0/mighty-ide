//! Peek definition (shim-side): an inline framed card, rendered directly below
//! the current editor line, previewing the definition the cursor resolves to.
//!
//! ## Flow
//!
//! Mighty triggers "Peek Definition" (Alt+F12 / a command-palette entry) at the
//! cursor. The shim reuses the nav definition request ([`crate::nav`]) to resolve
//! the target file + 0-based line, then [`PeekState::open_at`] reads a small
//! window of source lines around that line (the definition's short body, up to
//! [`PEEK_MAX_LINES`]) — from the target file on disk, or, when the definition is
//! in the CURRENTLY-open buffer, from the live in-memory text passed in. Those
//! lines are stored + syntax-highlighted and drawn as a rounded, shadowed
//! Vivid-Modern card with a `file:line` header.
//!
//! Esc closes it; Enter navigates to the actual definition (the shim hands back
//! the target path + line; Mighty opens/jumps exactly as go-to-definition does).
//!
//! Like the rest of the nav surfaces this is pure-ish + scalar-ABI driven (L17):
//! the window extraction ([`extract_window`]) is a pure function and unit-tested.

use std::path::{Path, PathBuf};

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

/// How many source lines around the definition to preview (the short body or a
/// window). Capped so the inline card stays compact.
pub const PEEK_MAX_LINES: usize = 12;
/// How many lines ABOVE the definition line to include for context.
pub const PEEK_CONTEXT_BEFORE: usize = 1;

/// Extract a preview window from `source` around the 0-based `def_line`.
///
/// Returns `(first_line, lines)` where `first_line` is the 0-based index of the
/// first returned line (so callers can render true line numbers) and `lines` is
/// up to [`PEEK_MAX_LINES`] source lines beginning [`PEEK_CONTEXT_BEFORE`] above
/// `def_line` (clamped to the start of the file). Trailing `\r` is stripped so
/// CRLF files preview cleanly. An out-of-range `def_line` clamps to the last
/// line. Pure + unit-tested.
pub fn extract_window(source: &str, def_line: u32) -> (u32, Vec<String>) {
    let all: Vec<&str> = source.split('\n').collect();
    if all.is_empty() {
        return (0, Vec::new());
    }
    let last = all.len().saturating_sub(1);
    let def = (def_line as usize).min(last);
    let start = def.saturating_sub(PEEK_CONTEXT_BEFORE);
    let end = (start + PEEK_MAX_LINES).min(all.len());
    let lines: Vec<String> = all[start..end]
        .iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    (start as u32, lines)
}

/// Read a file from disk and extract the preview window (cross-file peek). Best
/// effort: returns an empty window if the file can't be read.
pub fn extract_window_from_file(path: &Path, def_line: u32) -> (u32, Vec<String>) {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            extract_window(&text, def_line)
        }
        Err(_) => (0, Vec::new()),
    }
}

/// One previewed line: its 0-based source line number + the text.
#[derive(Debug, Clone)]
struct PeekLine {
    line_no: u32,
    text: String,
}

/// Shim-owned peek state: the resolved target + the previewed window + the row
/// the card anchors below. Inactive (`active == false`) until [`open_at`].
#[derive(Debug)]
pub struct PeekState {
    active: bool,
    /// The target file (for the header + Enter navigation).
    path: Option<PathBuf>,
    /// The 0-based definition line (the Enter / "go to" target).
    def_line: u32,
    /// The 0-based definition column (for the navigation jump).
    def_col: u32,
    /// The previewed lines (header window).
    lines: Vec<PeekLine>,
    /// The language used to color the preview.
    lang: crate::langdetect::Language,
    /// The 0-based editor line the cursor was on when peek opened (the card is
    /// drawn directly below this line). Set by the ABI.
    anchor_line: u32,
    /// Scroll offset (rows) within the preview when the body exceeds the card.
    scroll: usize,
}

impl Default for PeekState {
    fn default() -> Self {
        PeekState {
            active: false,
            path: None,
            def_line: 0,
            def_col: 0,
            lines: Vec::new(),
            lang: crate::langdetect::Language::PlainText,
            anchor_line: 0,
            scroll: 0,
        }
    }
}

impl PeekState {
    pub fn new() -> Self {
        PeekState::default()
    }

    /// Open the peek card for a resolved definition: `path` + 0-based
    /// `(def_line, def_col)`, anchored below editor line `anchor_line`. The
    /// preview window is read from `live_source` when the definition is in the
    /// active buffer (so unsaved edits show), else from the file on disk. Returns
    /// `true` if any preview line resulted (always true for a resolvable target).
    pub fn open_at(
        &mut self,
        path: PathBuf,
        def_line: u32,
        def_col: u32,
        anchor_line: u32,
        lang: crate::langdetect::Language,
        live_source: Option<&str>,
    ) -> bool {
        let (first, win) = match live_source {
            Some(src) => extract_window(src, def_line),
            None => extract_window_from_file(&path, def_line),
        };
        self.lines = win
            .into_iter()
            .enumerate()
            .map(|(i, text)| PeekLine {
                line_no: first + i as u32,
                text,
            })
            .collect();
        self.path = Some(path);
        self.def_line = def_line;
        self.def_col = def_col;
        self.anchor_line = anchor_line;
        self.lang = lang;
        self.scroll = 0;
        self.active = !self.lines.is_empty();
        self.active
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn close(&mut self) {
        self.active = false;
        self.lines.clear();
        self.path = None;
        self.scroll = 0;
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The text of preview row `i` (empty out of range).
    pub fn line_text(&self, i: usize) -> &str {
        self.lines.get(i).map(|l| l.text.as_str()).unwrap_or("")
    }

    /// The 0-based source line number of preview row `i`, or `-1` out of range.
    pub fn line_no(&self, i: usize) -> i32 {
        self.lines.get(i).map(|l| l.line_no as i32).unwrap_or(-1)
    }

    /// The resolved target path's display string (for the header / tests).
    #[allow(dead_code)]
    pub fn path_display(&self) -> String {
        self.path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn target_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn target_line(&self) -> u32 {
        self.def_line
    }

    pub fn target_col(&self) -> u32 {
        self.def_col
    }

    /// Scroll the preview body by `delta` rows (clamped so at least one row stays
    /// visible). Only meaningful when the body exceeds the visible card rows.
    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.lines.len().saturating_sub(1);
        let next = (self.scroll as i32 + delta).clamp(0, max as i32);
        self.scroll = next as usize;
    }

    /// Draw the inline peek card directly below the anchored editor line. A
    /// rounded, shadowed Vivid-Modern card: a `file:line` header band + the
    /// syntax-highlighted definition lines, clipped to the card. `first_visible`
    /// is the editor's scroll offset, `total_lines` sizes the editor gutter, so
    /// the card aligns under the right row. No-op when inactive or the anchor row
    /// is off-screen.
    pub fn draw(&self, ctx: &mut crate::MuiContext, first_visible: u32, rows: u32, total_lines: u64) {
        if !self.active || self.lines.is_empty() {
            return;
        }
        // Only draw when the anchored editor line is on screen.
        if self.anchor_line < first_visible || self.anchor_line >= first_visible + rows {
            return;
        }
        let region = layout::region(ctx.sidebar_visible);
        let win_w = ctx.gpu.width as f32;
        let win_h = ctx.gpu.height as f32;
        let row_h = layout::LINE_H();
        let chrome = theme::CHROME_FONT_SIZE;

        let anchor_row = (self.anchor_line - first_visible) as i32;
        // The card sits in the row just below the anchored line.
        let card_x = region.left + 12.0;
        let mut card_y = layout::row_y_in(region, anchor_row) + row_h + 2.0;
        let card_w = (win_w - card_x - 24.0).max(120.0);

        let header_h = row_h + 4.0;
        let visible_rows = self.lines.len().min(PEEK_MAX_LINES);
        let body_h = visible_rows as f32 * row_h + 6.0;
        let card_h = header_h + body_h;

        // Flip the card ABOVE the anchored line if it would overflow the bottom.
        if card_y + card_h > win_h - 30.0 {
            let above = layout::row_y_in(region, anchor_row) - card_h - 2.0;
            if above > region.top {
                card_y = above;
            }
        }

        let radius = 10.0_f32;
        // Shadow + elevated card + border (matches the hover/completion chrome).
        ctx.dl_shadow(card_x, card_y + 6.0, card_w, card_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.55), 22.0);
        ctx.dl_grad_v(card_x, card_y, card_w, card_h, radius, theme::ELEVATED_2(), theme::ELEVATED());
        ctx.dl_stroke(card_x, card_y, card_w, card_h, radius, theme::BORDER_STRONG(), 1.0);

        // Header band: a peek glyph + `file:line` + an Esc/Enter affordance hint.
        ctx.dl_grad_h(card_x, card_y, card_w, header_h, radius, theme::accent_a(0.14), 0.85);
        ctx.dl_rect(card_x, card_y + header_h - 1.0, card_w, 1.0, theme::BORDER_SOFT());
        // A small "definition" eye-ish icon.
        ctx.dl_icon(
            card_x + 10.0,
            card_y + (header_h - 14.0) * 0.5,
            14.0,
            14.0,
            "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7z M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
            theme::ACCENT_BRIGHT(),
            1.5,
            false,
        );
        let fname = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "definition".to_string());
        let header = format!("{}:{}", fname, self.def_line + 1);
        ctx.text.queue_ui_sized(card_x + 32.0, card_y + 6.0, &header, theme::TEXT(), chrome, ctx.clip);
        // Right-aligned hint.
        let hint = "Enter: go  ·  Esc: close";
        let hint_w = hint.chars().count() as f32 * chrome * 0.5;
        ctx.text.queue_ui_sized(
            (card_x + card_w - hint_w - 12.0).max(card_x + 32.0),
            card_y + 6.0,
            hint,
            theme::TEXT_3(),
            chrome - 1.5,
            ctx.clip,
        );

        // Body: syntax-highlighted preview lines, clipped to the card body.
        let body_top = card_y + header_h + 3.0;
        let prev_clip = ctx.clip;
        let body_clip = (
            card_x as u32,
            body_top as u32,
            card_w as u32,
            body_h as u32,
        );
        ctx.clip = Some(body_clip);

        // Gutter for the preview's true line numbers, sized to the widest shown.
        let max_no = self.lines.iter().map(|l| l.line_no + 1).max().unwrap_or(1);
        let gutter_chars = layout::digit_count(max_no as u64) as f32;
        let num_col_w = gutter_chars * layout::CHAR_W() + 10.0;
        let text_x = card_x + 14.0 + num_col_w;

        let start = self.scroll.min(self.lines.len().saturating_sub(1));
        let end = (start + visible_rows).min(self.lines.len());
        for (vi, pl) in self.lines[start..end].iter().enumerate() {
            let y = body_top + vi as f32 * row_h;
            // The definition line itself gets a faint accent band.
            if pl.line_no == self.def_line {
                ctx.dl_grad_h(card_x + 2.0, y - 1.0, card_w - 4.0, row_h, 0.0, theme::accent_a(0.10), 0.7);
                ctx.dl_rect(card_x + 2.0, y - 1.0, 2.0, row_h, theme::ACCENT());
            }
            // Line number.
            let num = (pl.line_no + 1).to_string();
            let num_w = num.chars().count() as f32 * layout::CHAR_W();
            let gx = card_x + 14.0 + (num_col_w - 10.0 - num_w).max(0.0);
            ctx.text.queue_sized(gx, y + 3.0, &num, theme::GUTTER(), chrome, Some(body_clip));

            // Syntax-highlighted source.
            let spans = crate::abi::highlight_for(&pl.text, self.lang);
            let chars: Vec<char> = pl.text.chars().collect();
            if spans.is_empty() {
                ctx.text.queue(text_x, y + 3.0, &pl.text, theme::TEXT_1(), Some(body_clip));
            } else {
                for sp in spans {
                    let frag: String = chars.iter().skip(sp.start).take(sp.len).collect();
                    if frag.trim().is_empty() {
                        continue;
                    }
                    let x = text_x + sp.start as f32 * layout::CHAR_W();
                    ctx.text.queue(x, y + 3.0, &frag, sp.color, Some(body_clip));
                }
            }
        }
        let _ = total_lines;
        ctx.clip = prev_clip;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let r = add(1, 2)\n  print(r)\n}\n";

    #[test]
    fn window_centers_on_def_line_with_context() {
        // def at line 0 -> starts at 0 (clamped), includes the body.
        let (first, lines) = extract_window(SRC, 0);
        assert_eq!(first, 0);
        assert_eq!(lines[0], "fn add(a: I32, b: I32) -> I32 {");
        assert_eq!(lines[1], "  a + b");
        assert_eq!(lines[2], "}");
    }

    #[test]
    fn window_includes_one_line_before() {
        // def at line 4 (`fn main`) -> one line of context before (line 3, blank).
        let (first, lines) = extract_window(SRC, 4);
        assert_eq!(first, 3);
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], "fn main() {");
        assert_eq!(lines[2], "  let r = add(1, 2)");
    }

    #[test]
    fn window_caps_at_max_lines() {
        let big: String = (0..50).map(|i| format!("line{i}\n")).collect();
        let (_first, lines) = extract_window(&big, 20);
        assert_eq!(lines.len(), PEEK_MAX_LINES);
    }

    #[test]
    fn window_clamps_out_of_range_def_line() {
        let (first, lines) = extract_window(SRC, 9999);
        // Clamps to the last line (the trailing empty line after the final \n).
        assert!(!lines.is_empty());
        assert!(first as usize <= SRC.split('\n').count());
    }

    #[test]
    fn window_strips_cr_for_crlf() {
        let crlf = "a\r\nb\r\nc\r\n";
        let (_first, lines) = extract_window(crlf, 1);
        assert_eq!(lines[0], "a");
        assert_eq!(lines[1], "b");
        assert!(!lines.iter().any(|l| l.contains('\r')));
    }

    #[test]
    fn window_from_file_reads_cross_file() {
        let dir = std::env::temp_dir().join("mui_peek_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("other.mty");
        std::fs::write(&path, "fn helper() {\n  42\n}\n").unwrap();
        let (first, lines) = extract_window_from_file(&path, 0);
        assert_eq!(first, 0);
        assert_eq!(lines[0], "fn helper() {");
        assert_eq!(lines[1], "  42");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn window_from_missing_file_is_empty() {
        let (_first, lines) = extract_window_from_file(Path::new("/no/such/file.mty"), 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn open_at_uses_live_source_when_provided() {
        let mut st = PeekState::new();
        let live = "fn live() {\n  live_body\n}\n";
        let ok = st.open_at(
            PathBuf::from("a.mty"),
            0,
            3,
            5,
            crate::langdetect::Language::Mighty,
            Some(live),
        );
        assert!(ok);
        assert!(st.is_active());
        assert_eq!(st.target_line(), 0);
        assert_eq!(st.target_col(), 3);
        assert_eq!(st.line_text(0), "fn live() {");
        assert_eq!(st.line_no(0), 0);
        assert_eq!(st.path_display(), "a.mty");
        // Close clears.
        st.close();
        assert!(!st.is_active());
        assert_eq!(st.line_count(), 0);
    }

    #[test]
    fn scroll_clamps() {
        let mut st = PeekState::new();
        let big: String = (0..30).map(|i| format!("l{i}\n")).collect();
        st.open_at(PathBuf::from("a.mty"), 0, 0, 0, crate::langdetect::Language::Mighty, Some(&big));
        st.scroll_by(-5);
        assert_eq!(st.scroll, 0);
        st.scroll_by(999);
        assert_eq!(st.scroll, st.line_count() - 1);
    }
}
