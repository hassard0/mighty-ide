//! Pure code-reading visual helpers: bracket-pair depth colorization, indent-
//! guide depth per line, and minimap click→line mapping.
//!
//! Everything here is intentionally **pure** (no GPU/context) so the editor
//! body draw + the minimap click router share one tested set of math, exactly
//! mirroring the `crate::layout` discipline. Colors are derived from the active
//! `crate::theme` so the rainbow + guides fit Vivid / Aurora / Warm alike.

use crate::ffi::MuiColor;

// ---------------------------------------------------------------------------
// Feature 1 — bracket-pair colorization
// ---------------------------------------------------------------------------

/// One bracket character on a visible line, tagged with its rainbow color
/// index (by nesting depth) or flagged as an error (unmatched / extra closer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketTag {
    /// Char column (0-based) of the bracket within its line.
    pub col: usize,
    /// Buffer line the bracket sits on.
    pub line: usize,
    /// Rainbow palette index by depth (`depth % palette_len`). Ignored when
    /// `error` is set.
    pub color_index: usize,
    /// `true` for an unmatched/extra bracket (a closer with no opener, or — when
    /// the scan finishes — an opener with no closer).
    pub error: bool,
}

/// Is `c` an opening bracket?
#[inline]
pub fn is_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{')
}
/// Is `c` a closing bracket?
#[inline]
pub fn is_close(c: char) -> bool {
    matches!(c, ')' | ']' | '}')
}
/// The closer that matches opener `c` (or `c` itself if not an opener).
#[inline]
fn closer_of(c: char) -> char {
    match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        other => other,
    }
}

/// Map a nesting `depth` (0-based) to a rainbow palette index in `0..palette_len`.
/// `palette_len` is clamped to at least 1 so the result is always valid.
#[inline]
pub fn depth_color_index(depth: usize, palette_len: usize) -> usize {
    let n = palette_len.max(1);
    depth % n
}

/// Mask of which char positions are *inside* a string/comment span and so must
/// NOT be treated as brackets. Built from the syntax spans for that line: any
/// span colored as a string or comment masks its chars. We pass the masked set
/// as a slice of `(start, len)` ranges (char offsets).
///
/// Returns `true` if char index `col` is masked.
#[inline]
fn masked(col: usize, mask: &[(usize, usize)]) -> bool {
    mask.iter().any(|&(s, l)| col >= s && col < s + l)
}

/// Assign rainbow color indices (by nesting depth) to every bracket across a
/// block of visible `lines` (each `(line_number, line_text)`), skipping any
/// bracket char whose column falls inside that line's `mask` ranges
/// (string/comment spans). Depth is tracked CONTINUOUSLY across the block so a
/// `{` on one line and its `}` on a later line share the same depth color.
///
/// * An opener increases depth; its color is `depth_color_index(depth, n)` at
///   the depth it OPENS (so the outermost pair is index 0).
/// * A matching closer gets the SAME index as its opener (depth before the pop).
/// * A closer with no open bracket on the stack is an `error` (extra closer).
/// * Any openers still on the stack at the end of the block are `error`
///   (unclosed within the visible region) — they still render, flagged.
///
/// `mask_for(line_index)` yields the masked char ranges for that line.
pub fn colorize_brackets<'a>(
    lines: impl IntoIterator<Item = (usize, &'a str)>,
    palette_len: usize,
    mut mask_for: impl FnMut(usize) -> Vec<(usize, usize)>,
) -> Vec<BracketTag> {
    let n = palette_len.max(1);
    let mut out = Vec::new();
    // Stack of (palette_index, out_position) so we can flag unclosed openers.
    let mut stack: Vec<(char, usize)> = Vec::new();
    for (line_no, text) in lines {
        let mask = mask_for(line_no);
        for (col, ch) in text.chars().enumerate() {
            if !is_open(ch) && !is_close(ch) {
                continue;
            }
            if masked(col, &mask) {
                continue;
            }
            if is_open(ch) {
                let depth = stack.len();
                let idx = depth_color_index(depth, n);
                out.push(BracketTag {
                    col,
                    line: line_no,
                    color_index: idx,
                    error: false,
                });
                stack.push((ch, out.len() - 1));
            } else {
                // Closer: must match the top opener's species.
                match stack.last().copied() {
                    Some((open_ch, _)) if closer_of(open_ch) == ch => {
                        let (_open, _pos) = stack.pop().unwrap();
                        let depth = stack.len(); // depth the pair lives at
                        out.push(BracketTag {
                            col,
                            line: line_no,
                            color_index: depth_color_index(depth, n),
                            error: false,
                        });
                    }
                    _ => {
                        // Extra / mismatched closer.
                        out.push(BracketTag {
                            col,
                            line: line_no,
                            color_index: 0,
                            error: true,
                        });
                    }
                }
            }
        }
    }
    // Any openers never closed within the block → flag them as errors.
    for (_ch, pos) in stack {
        if let Some(tag) = out.get_mut(pos) {
            tag.error = true;
        }
    }
    out
}

/// A theme-derived rainbow palette for bracket depths. Six hues chosen from the
/// active theme's syntax colors + accent so they harmonize across Vivid /
/// Aurora / Warm rather than clashing. Always non-empty.
pub fn bracket_palette() -> Vec<MuiColor> {
    let t = crate::theme::active();
    vec![
        t.syn_function,
        t.syn_type,
        t.syn_attr,
        t.syn_number,
        t.accent_bright,
        t.syn_string,
    ]
}

/// The error color for an unmatched/extra bracket (active theme's error hue).
#[inline]
pub fn bracket_error_color() -> MuiColor {
    crate::theme::active().error
}

// ---------------------------------------------------------------------------
// Feature 2 — indent guides
// ---------------------------------------------------------------------------

/// Count the leading-whitespace columns of `line`, expanding a literal tab to
/// the next `tab_width` stop. A blank line (only whitespace) returns `None` so
/// the caller can carry the guide depth from neighbors.
pub fn leading_indent_cols(line: &str, tab_width: usize) -> Option<usize> {
    let tw = tab_width.max(1);
    let mut cols = 0usize;
    let mut any = false;
    for ch in line.chars() {
        match ch {
            ' ' => cols += 1,
            '\t' => cols += tw - (cols % tw),
            _ => {
                any = true;
                break;
            }
        }
    }
    if any {
        Some(cols)
    } else {
        None // blank / whitespace-only
    }
}

/// Number of indent GUIDE levels to draw for an indent of `cols` columns at
/// `tab_width` spaces per level: a guide sits at the START of each level, i.e.
/// at columns `0, tab_width, 2*tab_width, …` strictly LESS than `cols`. So an
/// indent of exactly `tab_width` draws ONE guide (at column 0's child level).
#[inline]
pub fn guide_levels(cols: usize, tab_width: usize) -> usize {
    let tw = tab_width.max(1);
    // Guides at columns tw, 2*tw, … up to (but not exceeding) cols. A line
    // indented `cols` belongs to a block whose guides sit at every level strictly
    // inside it. We draw a guide at each multiple of tw in 1..=cols/tw, but a
    // guide AT `cols` itself only when the line has deeper content — to keep it
    // simple and VS-Code-like we draw `cols / tw` guides (one per completed level).
    cols / tw
}

/// Compute, per line in a block, the indent depth (in columns) used to place
/// guides — carrying the depth across BLANK lines from the nearest non-blank
/// neighbor (the MAX of the preceding and following non-blank indents, so a
/// blank line inside a nested block keeps its guides). Returns one entry per
/// input line.
///
/// `lines` is the full set of lines in scan order (typically the whole buffer
/// or the visible window plus a little context).
pub fn indent_depths(lines: &[&str], tab_width: usize) -> Vec<usize> {
    let n = lines.len();
    let raw: Vec<Option<usize>> = lines
        .iter()
        .map(|l| leading_indent_cols(l, tab_width))
        .collect();
    // prev[i] = indent of nearest non-blank at or before i.
    let mut prev = vec![0usize; n];
    let mut last = 0usize;
    for i in 0..n {
        if let Some(c) = raw[i] {
            last = c;
        }
        prev[i] = last;
    }
    // next[i] = indent of nearest non-blank at or after i.
    let mut next = vec![0usize; n];
    let mut fut = 0usize;
    for i in (0..n).rev() {
        if let Some(c) = raw[i] {
            fut = c;
        }
        next[i] = fut;
    }
    (0..n)
        .map(|i| match raw[i] {
            Some(c) => c,
            None => prev[i].min(next[i]), // blank: the SHARED depth of the block
        })
        .collect()
}

/// The active indent LEVEL (0-based) for the cursor: the indent level of the
/// block containing the cursor line. We use the cursor line's own indent (in
/// levels), but if the cursor sits ON an opening line (its indent is shallower
/// than the next line's body) the highlighted guide is the cursor line's level.
/// Returns the level index, or `None` when the cursor line is at column 0
/// (no active guide).
pub fn active_indent_level(
    lines: &[&str],
    cursor_line: usize,
    tab_width: usize,
) -> Option<usize> {
    let tw = tab_width.max(1);
    let depths = indent_depths(lines, tab_width);
    let here = *depths.get(cursor_line)?;
    // The active guide is the deepest one the cursor sits inside: cols/tw, minus
    // one to point at the guide that brackets the current block (its left rail).
    let levels = here / tw;
    if levels == 0 {
        None
    } else {
        Some(levels - 1)
    }
}

// ---------------------------------------------------------------------------
// Feature 3 — interactive minimap: click → source line
// ---------------------------------------------------------------------------

/// Geometry of the rendered minimap strip (pure, matches the draw in `abi.rs`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimapGeom {
    /// Left x of the strip.
    pub x: f32,
    /// Strip width.
    pub w: f32,
    /// Top y of the first minimap line bar.
    pub top: f32,
    /// Per-line vertical advance.
    pub line_h: f32,
    /// How many buffer lines the strip can show (the bars actually drawn).
    pub shown_lines: usize,
    /// Total buffer line count.
    pub total: usize,
}

impl MinimapGeom {
    /// Map a pixel `y` within the strip to the buffer line it represents,
    /// clamped to `0..total`. Lines beyond `shown_lines` are compressed: the
    /// strip's full height maps proportionally across ALL `total` lines so a
    /// click near the bottom of a tall file lands near EOF (not just the last
    /// drawn bar).
    pub fn line_at_y(&self, y: f32) -> usize {
        if self.total == 0 {
            return 0;
        }
        let span = (self.shown_lines.max(1) as f32) * self.line_h;
        let rel = ((y - self.top) / span).clamp(0.0, 1.0);
        let line = (rel * (self.total as f32)).floor() as usize;
        line.min(self.total.saturating_sub(1))
    }

    /// `true` if pixel `(x, y)` is inside the strip's horizontal band (callers
    /// pair this with the vertical field bounds).
    pub fn contains_x(&self, x: f32) -> bool {
        x >= self.x && x <= self.x + self.w
    }

    /// Given a target line, the `first_visible` scroll that CENTERS it in a
    /// viewport `rows` tall, clamped so we never scroll past EOF or below 0.
    pub fn scroll_to_center(&self, line: usize, rows: usize) -> usize {
        let half = rows / 2;
        let first = line.saturating_sub(half);
        let max_first = self.total.saturating_sub(rows);
        first.min(max_first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mask(_l: usize) -> Vec<(usize, usize)> {
        Vec::new()
    }

    // ---- bracket depth assignment ----

    #[test]
    fn nested_brackets_get_increasing_then_decreasing_depth() {
        // ( [ { } ] )  → depths 0,1,2,2,1,0
        let tags = colorize_brackets([(0usize, "([{}])")], 6, no_mask);
        let idx: Vec<usize> = tags.iter().map(|t| t.color_index).collect();
        assert_eq!(idx, vec![0, 1, 2, 2, 1, 0]);
        assert!(tags.iter().all(|t| !t.error));
    }

    #[test]
    fn depth_wraps_modulo_palette_len() {
        // Three palette colors, four levels deep → 0,1,2,0 on the openers.
        let tags = colorize_brackets([(0usize, "((((")], 3, no_mask);
        let opener_idx: Vec<usize> = tags.iter().map(|t| t.color_index).collect();
        assert_eq!(opener_idx, vec![0, 1, 2, 0]);
        // All four are unclosed → errors.
        assert!(tags.iter().all(|t| t.error));
    }

    #[test]
    fn extra_closer_is_flagged_error() {
        let tags = colorize_brackets([(0usize, ")(")], 6, no_mask);
        assert!(tags[0].error, "leading ')' is an extra closer");
        // The trailing '(' is unclosed → also error.
        assert!(tags[1].error);
    }

    #[test]
    fn mismatched_species_is_error() {
        // ( ] → the ']' mismatches and is flagged; the '(' never matches a closer
        // so it's flagged as unclosed at the end. Both are errors.
        let tags = colorize_brackets([(0usize, "(]")], 6, no_mask);
        assert!(tags[1].error, "']' mismatches '('");
        assert!(tags[0].error, "'(' is left unclosed → flagged");
    }

    #[test]
    fn well_matched_species_no_error() {
        let tags = colorize_brackets([(0usize, "()")], 6, no_mask);
        assert!(tags.iter().all(|t| !t.error));
        assert_eq!(tags[0].color_index, 0);
        assert_eq!(tags[1].color_index, 0);
    }

    #[test]
    fn depth_carries_across_lines() {
        // Opener on line 0, body on line 1, closer on line 2 — all share depth 0.
        let tags = colorize_brackets(
            [(0usize, "fn f() {"), (1, "  x"), (2, "}")],
            6,
            no_mask,
        );
        // The () pair on line 0 is depth 0; the {…} pair spanning lines is depth 0.
        let line0: Vec<_> = tags.iter().filter(|t| t.line == 0).collect();
        assert_eq!(line0.len(), 3); // ( ) {
        assert_eq!(line0[0].color_index, 0); // (
        assert_eq!(line0[1].color_index, 0); // )
        assert_eq!(line0[2].color_index, 0); // {  (opens at depth 0)
        let closer = tags.iter().find(|t| t.line == 2).unwrap();
        assert_eq!(closer.color_index, 0);
        assert!(tags.iter().all(|t| !t.error));
    }

    #[test]
    fn masked_brackets_in_strings_are_ignored() {
        // The '(' at col 4 is inside a masked span [4,3) → skipped.
        let line = "abc(x)"; // bracket at 3 and 5
        let mask = |_l: usize| vec![(3usize, 3usize)]; // mask cols 3,4,5
        let tags = colorize_brackets([(0usize, line)], 6, mask);
        assert!(tags.is_empty(), "all brackets masked → none tagged");
    }

    #[test]
    fn depth_color_index_wraps() {
        assert_eq!(depth_color_index(0, 3), 0);
        assert_eq!(depth_color_index(3, 3), 0);
        assert_eq!(depth_color_index(4, 3), 1);
        // Zero palette len is treated as 1 (no panic).
        assert_eq!(depth_color_index(5, 0), 0);
    }

    #[test]
    fn palette_is_nonempty_for_each_theme() {
        use crate::theme::{self, ThemeId};
        let _g = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for id in ThemeId::ALL {
            theme::set_active(id);
            assert!(!bracket_palette().is_empty());
            assert!(bracket_palette().len() >= 3);
        }
        theme::set_active(ThemeId::Vivid);
    }

    // ---- indent guides ----

    #[test]
    fn leading_indent_counts_spaces_and_tabs() {
        assert_eq!(leading_indent_cols("hi", 2), Some(0));
        assert_eq!(leading_indent_cols("  hi", 2), Some(2));
        assert_eq!(leading_indent_cols("    hi", 2), Some(4));
        // A tab expands to the next stop.
        assert_eq!(leading_indent_cols("\thi", 4), Some(4));
        assert_eq!(leading_indent_cols("  \thi", 4), Some(4)); // 2 spaces → tab to 4
        // Blank / whitespace-only → None (carry from neighbors).
        assert_eq!(leading_indent_cols("", 2), None);
        assert_eq!(leading_indent_cols("    ", 2), None);
    }

    #[test]
    fn guide_levels_one_per_completed_indent() {
        assert_eq!(guide_levels(0, 2), 0);
        assert_eq!(guide_levels(2, 2), 1);
        assert_eq!(guide_levels(4, 2), 2);
        assert_eq!(guide_levels(6, 2), 3);
        // Odd indent (3 cols, tw=2) → 1 completed level.
        assert_eq!(guide_levels(3, 2), 1);
    }

    #[test]
    fn indent_depths_carry_through_blank_lines() {
        // A blank line nested inside a 4-col block keeps the block's depth.
        let lines = ["fn f() {", "    a", "", "    b", "}"];
        let v: Vec<&str> = lines.to_vec();
        let d = indent_depths(&v, 2);
        assert_eq!(d[0], 0); // fn …
        assert_eq!(d[1], 4); // a
        assert_eq!(d[2], 4); // blank inside the block → carried (min(4,4))
        assert_eq!(d[3], 4); // b
        assert_eq!(d[4], 0); // }
    }

    #[test]
    fn blank_between_blocks_takes_shallower_depth() {
        // Blank line between a deep block and the top level → the SHALLOWER depth
        // (so guides don't bleed past the block boundary).
        let lines = ["        deep", "", "top"];
        let v: Vec<&str> = lines.to_vec();
        let d = indent_depths(&v, 4);
        assert_eq!(d[0], 8);
        assert_eq!(d[1], 0, "blank before a col-0 line drops to 0");
        assert_eq!(d[2], 0);
    }

    #[test]
    fn active_level_from_cursor() {
        let lines = ["fn f() {", "    a", "        b", "    c", "}"];
        let v: Vec<&str> = lines.to_vec();
        // Cursor on the col-0 line → no active guide.
        assert_eq!(active_indent_level(&v, 0, 2), None);
        // Cursor on the 4-col line (tw=2 → 2 levels) → active guide index 1
        // (the rail that brackets the cursor's block).
        assert_eq!(active_indent_level(&v, 1, 2), Some(1));
        // Cursor on the 8-col line (4 levels) → active guide index 3.
        assert_eq!(active_indent_level(&v, 2, 2), Some(3));
        // With tw=4: 4-col line is 1 level → index 0; 8-col is 2 levels → index 1.
        assert_eq!(active_indent_level(&v, 1, 4), Some(0));
        assert_eq!(active_indent_level(&v, 2, 4), Some(1));
    }

    // ---- minimap click → line ----

    fn geom(total: usize, shown: usize) -> MinimapGeom {
        MinimapGeom {
            x: 1250.0,
            w: 70.0,
            top: 100.0,
            line_h: 4.0,
            shown_lines: shown,
            total,
        }
    }

    #[test]
    fn minimap_click_top_maps_to_first_line() {
        let g = geom(1000, 180);
        assert_eq!(g.line_at_y(g.top), 0);
        assert_eq!(g.line_at_y(g.top - 50.0), 0, "above the strip clamps to 0");
    }

    #[test]
    fn minimap_click_bottom_maps_near_eof() {
        let g = geom(1000, 180);
        let bottom = g.top + (g.shown_lines as f32) * g.line_h;
        let line = g.line_at_y(bottom);
        assert_eq!(line, g.total - 1, "click at the strip bottom → last line");
        // Well past the bottom still clamps to EOF.
        assert_eq!(g.line_at_y(bottom + 999.0), g.total - 1);
    }

    #[test]
    fn minimap_click_middle_maps_to_middle() {
        let g = geom(1000, 180);
        let mid = g.top + (g.shown_lines as f32) * g.line_h * 0.5;
        let line = g.line_at_y(mid);
        // Half-way down the strip → roughly the middle of a 1000-line file.
        assert!((line as i64 - 500).abs() <= 5, "mid click line={line}");
    }

    #[test]
    fn minimap_short_file_maps_directly() {
        // A file shorter than the strip: every line drawn, click maps 1:1-ish.
        let g = geom(40, 40);
        assert_eq!(g.line_at_y(g.top), 0);
        let last = g.top + (g.shown_lines as f32) * g.line_h;
        assert_eq!(g.line_at_y(last), 39);
    }

    #[test]
    fn scroll_to_center_clamps() {
        let g = geom(1000, 180);
        // Centering line 500 in a 50-row view → first ≈ 475.
        assert_eq!(g.scroll_to_center(500, 50), 475);
        // Near the top can't go negative.
        assert_eq!(g.scroll_to_center(3, 50), 0);
        // Near EOF clamps so first + rows <= total.
        assert_eq!(g.scroll_to_center(999, 50), 950);
    }

    #[test]
    fn contains_x_band() {
        let g = geom(100, 100);
        assert!(g.contains_x(1250.0));
        assert!(g.contains_x(1300.0));
        assert!(!g.contains_x(1200.0));
        assert!(!g.contains_x(1400.0));
    }
}
