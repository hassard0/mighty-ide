//! Live **Markdown preview** — shim-owned state + a themed Vello renderer that
//! draws the rendered markdown of a source buffer into a pane column.
//!
//! The preview is a per-pane *mode*: a split pane (see [`crate::panes`]) can be
//! flipped into preview mode, in which case [`crate::abi::draw_editor_pane`] calls
//! [`MdPreview::draw`] for that pane instead of the editor body. The source is the
//! OTHER (editor) pane's `.md` buffer, re-parsed each frame from the live text
//! model (cheap for IDE-sized files), so the preview updates as you type.
//!
//! Styling pulls entirely from [`crate::theme`] so it works in all three themes:
//! scaled bold headings (h1/h2 get a bottom hairline), comfortable wrapped body
//! text, monospace code blocks in a tinted rounded card, inline `code` chips,
//! accent links, indented list markers, accent-bar blockquotes, and `---`
//! dividers. The pane scrolls independently (a pixel scroll offset).

#![allow(dead_code)]

use crate::layout::Region;
use crate::markdown::{self, Block, ListItem, Span};
use crate::theme;
use crate::MuiContext;

/// Shim-owned markdown-preview state. One per IDE (the preview targets whichever
/// pane is flagged for preview); holds only the pixel scroll offset + total
/// rendered content height so scrolling can be clamped. The block model itself is
/// re-parsed from the live buffer each frame, so nothing here caches stale text.
#[derive(Debug, Default)]
pub struct MdPreview {
    /// `true` while a preview pane is open.
    open: bool,
    /// Pixel scroll offset (top of content shifted up by this many px).
    scroll: f32,
    /// Total rendered content height in px (set by the last [`MdPreview::draw`]),
    /// so scrolling can clamp to `[0, content_h - viewport_h]`.
    content_h: f32,
    /// Last viewport height (px) used for scroll clamping.
    viewport_h: f32,
}

/// Comfortable inner margin (px) on the left/right of the preview content column.
const MARGIN_X: f32 = 28.0;
/// Top margin (px) below the pane's top edge.
const MARGIN_TOP: f32 = 20.0;

impl MdPreview {
    pub fn new() -> Self {
        MdPreview::default()
    }

    /// Open the preview (idempotent). Resets the scroll to the top.
    pub fn open(&mut self) {
        self.open = true;
        self.scroll = 0.0;
    }

    /// Close the preview.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// `true` while the preview pane is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Scroll by `delta` lines (each ~ a body line height), clamped to content.
    pub fn scroll_lines(&mut self, delta: i32) {
        let step = crate::layout::LINE_H();
        self.scroll = (self.scroll + delta as f32 * step).max(0.0);
        self.clamp_scroll();
    }

    /// Scroll by an explicit pixel `delta`, clamped to content.
    pub fn scroll_px(&mut self, delta: f32) {
        self.scroll = (self.scroll + delta).max(0.0);
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_h - self.viewport_h).max(0.0);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Render the markdown of `source` into the pane column `region`..`x_right`
    /// (clipped to the window), styled to the active theme. Sets `content_h` so a
    /// later scroll can clamp. `win_h` is the window height (the column runs from
    /// the pane top to just above the status bar).
    pub fn draw(&mut self, ctx: &mut MuiContext, source: &str, region: Region, x_right: f32, win_h: f32) {
        let blocks = markdown::parse(source);
        let left = region.left;
        let top = region.top;
        let field_h = (win_h - 30.0 - top).max(0.0); // 30 = status bar band
        self.viewport_h = field_h;

        // Opaque field background covering the column (so editor text never shows
        // through), then a header band labeling the preview.
        let col_w = (x_right - left).max(0.0);
        ctx.dl_rect(left, top, col_w, field_h, theme::BG_1());

        let head_h = 28.0_f32;
        ctx.dl_rect(left, top, col_w, head_h, theme::BG_2());
        ctx.dl_rect(left, top + head_h - 1.0, col_w, 1.0, theme::BORDER_SOFT());
        let chrome = theme::CHROME_FONT_SIZE;
        let hy = top + (head_h - chrome) * 0.5 - 1.0;
        ctx.dl_icon(left + 12.0, top + (head_h - 14.0) * 0.5, 14.0, 14.0, crate::icons::FILE_MD, theme::ACCENT_BRIGHT(), 1.5, false);
        ctx.text.queue_ui_sized(left + 34.0, hy, "Markdown Preview", theme::TEXT(), chrome, ctx.clip);

        // Clip rect for the scrolling body (below the header, above the status bar).
        let body_top = top + head_h;
        let clip = Some((
            left as u32,
            body_top as u32,
            col_w as u32,
            (field_h - head_h).max(0.0) as u32,
        ));

        // Content column geometry. We lay blocks out at a running y that starts
        // below the header minus the scroll offset; the clip hides off-screen rows.
        let content_left = left + MARGIN_X;
        let content_w = (col_w - 2.0 * MARGIN_X).max(40.0);
        let mut painter = Painter {
            ctx,
            clip,
            x0: content_left,
            width: content_w,
            y: body_top + MARGIN_TOP - self.scroll,
        };
        painter.blocks(&blocks, 0.0);
        // Total content height = where we ended (in content space) minus the start.
        let end_y = painter.y + self.scroll;
        self.content_h = (end_y - (body_top + MARGIN_TOP)) + MARGIN_TOP;
        self.clamp_scroll();
    }
}

/// A running-cursor painter over a content column. Holds the draw context, the
/// clip rect, the column geometry, and the current baseline-top `y`.
struct Painter<'a> {
    ctx: &'a mut MuiContext,
    clip: Option<(u32, u32, u32, u32)>,
    /// Left x of the content column (px).
    x0: f32,
    /// Content column width (px).
    width: f32,
    /// Current top y (px); advanced as blocks are emitted.
    y: f32,
}

/// Body text size (px) for the preview (a touch larger than chrome for reading).
const BODY_SIZE: f32 = 14.5;
/// Body line height (px) — comfortable leading.
const BODY_LH: f32 = 22.0;
/// Approximate proportional advance per char for the UI font at a given size,
/// used for word-wrap width estimation (Bricolage is ~0.52em average).
fn ui_advance(size: f32) -> f32 {
    size * 0.52
}

impl Painter<'_> {
    /// Emit a list of blocks at the given `indent` (px added to the left edge,
    /// used for blockquote / nested content).
    fn blocks(&mut self, blocks: &[Block], indent: f32) {
        for b in blocks {
            self.block(b, indent);
        }
    }

    fn block(&mut self, block: &Block, indent: f32) {
        match block {
            Block::Heading { level, spans } => self.heading(*level, spans, indent),
            Block::Paragraph(spans) => self.paragraph(spans, indent),
            Block::List { ordered, items } => self.list(*ordered, items, indent),
            Block::CodeBlock { lang, lines } => self.code_block(lang.as_deref(), lines, indent),
            Block::Quote(inner) => self.quote(inner, indent),
            Block::Rule => self.rule(indent),
            Block::Table { header, rows } => self.table(header, rows, indent),
        }
    }

    // ---- headings ----

    fn heading(&mut self, level: u8, spans: &[Span], indent: f32) {
        // Scale by level: h1 biggest down to h6 ~ body.
        let size = match level {
            1 => 26.0,
            2 => 21.0,
            3 => 18.0,
            4 => 16.0,
            5 => 14.5,
            _ => 13.0,
        };
        let lh = size * 1.45;
        self.y += if level <= 2 { 14.0 } else { 10.0 }; // space above
        let x = self.x0 + indent;
        let avail = self.width - indent;
        let lines = wrap_spans(spans, avail, size, true);
        for line in &lines {
            self.draw_span_line(line, x, self.y, size, theme::TEXT(), true);
            self.y += lh;
        }
        // h1 / h2 get a bottom hairline divider.
        if level <= 2 {
            self.y += 4.0;
            self.ctx.dl_rect(x, self.y, avail, 1.0, theme::BORDER());
            self.y += 8.0;
        } else {
            self.y += 4.0;
        }
    }

    // ---- paragraphs ----

    fn paragraph(&mut self, spans: &[Span], indent: f32) {
        let x = self.x0 + indent;
        let avail = self.width - indent;
        let lines = wrap_spans(spans, avail, BODY_SIZE, false);
        for line in &lines {
            self.draw_span_line(line, x, self.y, BODY_SIZE, theme::TEXT_1(), false);
            self.y += BODY_LH;
        }
        self.y += 8.0; // paragraph spacing
    }

    // ---- lists ----

    fn list(&mut self, ordered: bool, items: &[ListItem], indent: f32) {
        for item in items {
            let depth_px = indent + 16.0 + item.depth as f32 * 20.0;
            let marker_x = self.x0 + indent + item.depth as f32 * 20.0;
            // Marker (bullet or number).
            let marker = if ordered {
                format!("{}.", item.number.unwrap_or(0))
            } else {
                "\u{2022}".to_string() // •
            };
            // The item content wraps in the remaining width.
            let x = self.x0 + depth_px;
            let avail = (self.width - depth_px).max(40.0);
            let lines = wrap_spans(&item.spans, avail, BODY_SIZE, false);
            // Draw the marker aligned with the first wrapped line.
            self.ctx
                .text
                .queue_ui_sized(marker_x, self.y, &marker, theme::ACCENT(), BODY_SIZE, self.clip);
            for line in &lines {
                self.draw_span_line(line, x, self.y, BODY_SIZE, theme::TEXT_1(), false);
                self.y += BODY_LH;
            }
            if lines.is_empty() {
                self.y += BODY_LH;
            }
            self.y += 2.0;
        }
        self.y += 6.0;
    }

    // ---- fenced code ----

    fn code_block(&mut self, lang: Option<&str>, lines: &[String], indent: f32) {
        let size = BODY_SIZE - 1.0;
        let lh = crate::layout::LINE_H();
        let adv = crate::layout::CHAR_W();
        let x = self.x0 + indent;
        let avail = self.width - indent;
        let pad = 12.0;
        let n = lines.len().max(1);
        let card_h = n as f32 * lh + 2.0 * pad;
        self.y += 6.0;
        // Tinted rounded card.
        self.ctx.dl_round(x, self.y, avail, card_h, 8.0, theme::BG_EDIT());
        self.ctx.dl_stroke(x, self.y, avail, card_h, 8.0, theme::BORDER_SOFT(), 1.0);
        // Optional language tag, top-right.
        if let Some(l) = lang {
            let lw = l.chars().count() as f32 * ui_advance(size - 1.0) + 8.0;
            self.ctx.text.queue_ui_sized(
                (x + avail - lw - 8.0).max(x + pad),
                self.y + 5.0,
                l,
                theme::TEXT_3(),
                size - 1.0,
                self.clip,
            );
        }
        let tx = x + pad;
        let max_chars = (((avail - 2.0 * pad) / adv).floor() as usize).max(1);
        let mut cy = self.y + pad;
        for line in lines {
            // Monospace, syntax-default color; clip overlong lines.
            let mut shown = line.clone();
            if shown.chars().count() > max_chars {
                shown = shown.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "\u{2026}";
            }
            self.ctx
                .text
                .queue_sized(tx, cy, &shown, theme::SYN_DEFAULT(), size, self.clip);
            cy += lh;
        }
        self.y += card_h + 8.0;
    }

    // ---- blockquote ----

    fn quote(&mut self, inner: &[Block], indent: f32) {
        self.y += 4.0;
        let bar_x = self.x0 + indent;
        let start_y = self.y;
        // Render inner blocks indented past the bar, dimmed.
        // (We can't know the height ahead of time, so draw the bar after.)
        let bar_indent = indent + 14.0;
        // Temporarily render inner blocks dim by reusing paragraph/heading with a
        // dim color is complex; simplest: render inner with a left indent and then
        // overlay the accent bar over the spanned height.
        self.blocks_dim(inner, bar_indent);
        let end_y = self.y;
        let h = (end_y - start_y).max(BODY_LH);
        // Accent left bar over the spanned vertical range.
        self.ctx.dl_round(bar_x, start_y, 3.0, h - 6.0, 1.5, theme::ACCENT());
        self.y += 4.0;
    }

    /// Render inner blocks with the body color dimmed (used inside blockquotes).
    fn blocks_dim(&mut self, blocks: &[Block], indent: f32) {
        for b in blocks {
            match b {
                Block::Paragraph(spans) => {
                    let x = self.x0 + indent;
                    let avail = self.width - indent;
                    let lines = wrap_spans(spans, avail, BODY_SIZE, false);
                    for line in &lines {
                        self.draw_span_line(line, x, self.y, BODY_SIZE, theme::DIM(), false);
                        self.y += BODY_LH;
                    }
                    self.y += 6.0;
                }
                other => self.block(other, indent),
            }
        }
    }

    // ---- horizontal rule ----

    fn rule(&mut self, indent: f32) {
        self.y += 8.0;
        let x = self.x0 + indent;
        self.ctx.dl_rect(x, self.y, self.width - indent, 1.0, theme::BORDER_STRONG());
        self.y += 12.0;
    }

    // ---- table ----

    fn table(&mut self, header: &[Vec<Span>], rows: &[Vec<Vec<Span>>], indent: f32) {
        let cols = header.len().max(1);
        let x = self.x0 + indent;
        let avail = self.width - indent;
        let col_w = avail / cols as f32;
        let row_h = BODY_LH + 6.0;
        self.y += 6.0;
        // Header row (tinted).
        self.ctx.dl_rect(x, self.y, avail, row_h, theme::accent_a(0.10));
        for (c, cell) in header.iter().enumerate() {
            let cx = x + c as f32 * col_w + 8.0;
            let line = wrap_spans(cell, col_w - 16.0, BODY_SIZE, true)
                .into_iter()
                .next()
                .unwrap_or_default();
            self.draw_span_line(&line, cx, self.y + 4.0, BODY_SIZE, theme::TEXT(), true);
        }
        self.y += row_h;
        // Body rows.
        for row in rows {
            self.ctx.dl_rect(x, self.y + row_h - 1.0, avail, 1.0, theme::BORDER_SOFT());
            for c in 0..cols {
                let cx = x + c as f32 * col_w + 8.0;
                if let Some(cell) = row.get(c) {
                    let line = wrap_spans(cell, col_w - 16.0, BODY_SIZE, false)
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    self.draw_span_line(&line, cx, self.y + 4.0, BODY_SIZE, theme::TEXT_1(), false);
                }
            }
            self.y += row_h;
        }
        // Outer border.
        let table_h = row_h * (rows.len() + 1) as f32;
        self.ctx
            .dl_stroke(x, self.y - table_h, avail, table_h, 4.0, theme::BORDER(), 1.0);
        self.y += 10.0;
    }

    // ---- inline span line drawing ----

    /// Draw a single (already-wrapped) line of inline pieces at `(x, y)` baseline-
    /// top. `base_bold` forces the bold UI weight for the whole line (headings).
    fn draw_span_line(&mut self, line: &[Piece], x: f32, y: f32, size: f32, base: crate::ffi::MuiColor, base_bold: bool) {
        let mut px = x;
        for piece in line {
            let adv = piece.text.chars().count() as f32 * piece_advance(piece, size);
            match piece.kind {
                PieceKind::Code => {
                    // Inline code chip: a tinted rounded background + mono text.
                    let cadv = piece.text.chars().count() as f32 * crate::layout::CHAR_W() * (size / crate::theme::FONT_SIZE());
                    let chip_w = cadv + 8.0;
                    self.ctx
                        .dl_round(px, y - 1.0, chip_w, size + 6.0, 4.0, theme::accent_a(0.12));
                    self.ctx
                        .text
                        .queue_sized(px + 4.0, y, &piece.text, theme::ACCENT_BRIGHT(), size, self.clip);
                    px += chip_w + 1.0;
                    continue;
                }
                PieceKind::Link => {
                    self.ctx
                        .text
                        .queue_ui_sized(px, y, &piece.text, theme::ACCENT(), size, self.clip);
                    // Underline.
                    self.ctx.dl_rect(px, y + size + 1.0, adv, 1.0, theme::ACCENT_LINE());
                    px += adv;
                    continue;
                }
                _ => {}
            }
            let color = if matches!(piece.kind, PieceKind::Strike) {
                theme::DIM()
            } else {
                base
            };
            // Pick a TRUE face from the piece's emphasis (plus the line-level bold
            // for headings). `**bold**` / headings -> Bricolage Bold (a true bold
            // UI face). `*italic*` has no italic in the UI family, so it shapes in
            // the code family's TRUE italic face (a genuine slant, never faux).
            let bold = base_bold || matches!(piece.kind, PieceKind::Bold);
            let italic = matches!(piece.kind, PieceKind::Italic);
            if italic && !bold {
                self.ctx.text.queue_styled(
                    px,
                    y,
                    &piece.text,
                    color,
                    size,
                    crate::vello_ui::FontStyle::Italic,
                    self.clip,
                );
            } else {
                let style = crate::vello_ui::FontStyle::default().with(bold, italic);
                self.ctx
                    .text
                    .queue_ui_styled(px, y, &piece.text, color, size, style, self.clip);
            }
            if matches!(piece.kind, PieceKind::Strike) {
                self.ctx.dl_rect(px, y + size * 0.5, adv, 1.0, theme::DIM());
            }
            px += adv;
        }
    }
}

/// Per-char advance estimate for a piece at `size` (mono for code, proportional
/// otherwise).
fn piece_advance(piece: &Piece, size: f32) -> f32 {
    match piece.kind {
        PieceKind::Code => crate::layout::CHAR_W() * (size / crate::theme::FONT_SIZE()),
        _ => ui_advance(size),
    }
}

/// A flat drawable text piece produced from spans (kind drives styling).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    text: String,
    kind: PieceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PieceKind {
    Plain,
    Bold,
    Italic,
    Strike,
    Code,
    Link,
}

/// Flatten spans into styled word pieces (splitting plain/bold/italic/strike on
/// spaces so we can word-wrap; code/links stay atomic).
fn flatten_spans(spans: &[Span], kind: PieceKind, out: &mut Vec<Piece>) {
    for span in spans {
        match span {
            Span::Text(t) => push_words(t, kind, out),
            Span::Bold(inner) => flatten_spans(inner, PieceKind::Bold, out),
            Span::Italic(inner) => flatten_spans(inner, PieceKind::Italic, out),
            Span::Strike(inner) => flatten_spans(inner, PieceKind::Strike, out),
            Span::Code(t) => out.push(Piece { text: t.clone(), kind: PieceKind::Code }),
            Span::Link { text, .. } => {
                // Render the link's display text as one atomic accent piece.
                let label = markdown::spans_text(text);
                out.push(Piece { text: label, kind: PieceKind::Link });
            }
        }
    }
}

/// Split `t` on whitespace into word pieces (each carrying a trailing space so
/// wrapping re-joins naturally).
fn push_words(t: &str, kind: PieceKind, out: &mut Vec<Piece>) {
    let mut first = true;
    for word in t.split_whitespace() {
        let text = if first { word.to_string() } else { format!(" {word}") };
        first = false;
        out.push(Piece { text, kind });
    }
    // Preserve a leading/trailing space context loosely: if `t` ended with a
    // space and there were words, the next piece (different span) keeps reading
    // naturally because we prefix following words. Good enough for preview.
}

/// Word-wrap flattened spans to `width` px at `size`, returning lines of pieces.
fn wrap_spans(spans: &[Span], width: f32, size: f32, bold: bool) -> Vec<Vec<Piece>> {
    let mut pieces = Vec::new();
    let root_kind = if bold { PieceKind::Bold } else { PieceKind::Plain };
    flatten_spans(spans, root_kind, &mut pieces);
    let mut lines: Vec<Vec<Piece>> = Vec::new();
    let mut cur: Vec<Piece> = Vec::new();
    let mut cur_w = 0.0f32;
    for p in pieces {
        let w = p.text.chars().count() as f32 * piece_advance(&p, size);
        if cur_w + w > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0.0;
            // Drop a leading space on the wrapped line.
            let trimmed = p.text.trim_start().to_string();
            let w2 = trimmed.chars().count() as f32 * piece_advance(&p, size);
            cur.push(Piece { text: trimmed, kind: p.kind });
            cur_w += w2;
        } else {
            cur_w += w;
            cur.push(p);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_splits_words_and_keeps_code_atomic() {
        let spans = markdown::parse_inline("hello **bold world** `x = 1` end");
        let mut out = Vec::new();
        flatten_spans(&spans, PieceKind::Plain, &mut out);
        // "x = 1" stays as one code piece.
        assert!(out.iter().any(|p| p.kind == PieceKind::Code && p.text == "x = 1"));
        // Bold words are split.
        assert!(out.iter().any(|p| p.kind == PieceKind::Bold && p.text.trim() == "bold"));
        assert!(out.iter().any(|p| p.kind == PieceKind::Bold && p.text.trim() == "world"));
    }

    #[test]
    fn wrap_breaks_into_multiple_lines() {
        let spans = markdown::parse_inline(
            "one two three four five six seven eight nine ten eleven twelve",
        );
        // A narrow column forces several lines.
        let lines = wrap_spans(&spans, 80.0, BODY_SIZE, false);
        assert!(lines.len() > 1, "expected wrapping, got {} line(s)", lines.len());
        // A very wide column fits everything on one line.
        let wide = wrap_spans(&spans, 10_000.0, BODY_SIZE, false);
        assert_eq!(wide.len(), 1);
    }

    #[test]
    fn link_flattens_to_label_piece() {
        let spans = markdown::parse_inline("see [the docs](http://x)");
        let mut out = Vec::new();
        flatten_spans(&spans, PieceKind::Plain, &mut out);
        assert!(out.iter().any(|p| p.kind == PieceKind::Link && p.text == "the docs"));
    }

    #[test]
    fn preview_open_close_and_scroll_clamp() {
        let mut p = MdPreview::new();
        assert!(!p.is_open());
        p.open();
        assert!(p.is_open());
        // With no content height yet, scrolling clamps to 0.
        p.scroll_lines(5);
        assert_eq!(p.scroll, 0.0);
        p.close();
        assert!(!p.is_open());
    }
}
