//! Pure editor-layout math (gutter width, pixel<->cell mapping, visible rows).
//!
//! All integer line/col -> pixel conversion lives here on the Rust side because
//! v0.36 Mighty has no int->float cast (docs/mighty-language-lessons.md L19).
//! Keeping it pure (no GPU/context) makes it unit-testable and lets the scalar
//! ABI and the render loop agree on a single set of metrics.

/// Left/top padding of the editor surface, in pixels.
pub const PAD: f32 = 8.0;
/// Vertical advance per text line, in pixels.
pub const LINE_H: f32 = 18.0;
/// Horizontal advance per monospace cell, in pixels (must match the font's
/// monospace metrics closely enough for cursor/click alignment).
pub const CHAR_W: f32 = 8.0;
/// Gap (px) between the line-number gutter and the text column.
pub const GUTTER_GAP: f32 = 8.0;

/// Height (px) of the top tab bar.
pub const TAB_BAR_H: f32 = 22.0;
/// Width (px) of one tab in the tab bar (fixed-width tabs keep click→index math
/// trivial: `idx = floor(x / TAB_W)`).
pub const TAB_W: f32 = 120.0;
/// Default width (px) of the file-tree sidebar when shown.
pub const SIDEBAR_W: f32 = 180.0;
/// Pixels of indentation per tree depth level.
pub const TREE_INDENT: f32 = 12.0;

/// The pixel offsets of the editable text region: the top edge (below the tab
/// bar) and the left edge (right of the sidebar, if shown). The gutter/text/
/// cursor math is all relative to these so the editor body can be shifted by the
/// tab bar and sidebar without touching the row/col arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    /// Top y (px) of the first text row — below the tab bar.
    pub top: f32,
    /// Left x (px) of the gutter — right of the sidebar (or 0 when hidden).
    pub left: f32,
}

/// Compute the editor body region given whether the sidebar is visible.
pub fn region(sidebar_visible: bool) -> Region {
    Region {
        top: TAB_BAR_H,
        left: if sidebar_visible { SIDEBAR_W } else { 0.0 },
    }
}

/// Number of decimal digits needed to print `n` (minimum 1).
pub fn digit_count(n: u64) -> u32 {
    let mut n = n;
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Width (px) reserved for the line-number gutter, sized to the widest line
/// number that can appear (`total_lines`), plus the gap to the text column.
/// `total_lines` is clamped to at least 1.
pub fn gutter_width(total_lines: u64) -> f32 {
    let digits = digit_count(total_lines.max(1));
    PAD + (digits as f32) * CHAR_W + GUTTER_GAP
}

/// X pixel where the text column starts (right edge of the gutter). Retained
/// as the no-offset base (used by the region-aware variants + unit tests).
#[allow(dead_code)]
pub fn text_left(total_lines: u64) -> f32 {
    gutter_width(total_lines)
}

/// Region-aware gutter left edge: shifted right past the sidebar.
pub fn text_left_in(region: Region, total_lines: u64) -> f32 {
    region.left + gutter_width(total_lines)
}

/// X pixel for `col` (0-based) within the text area (no offset; tests).
#[allow(dead_code)]
pub fn text_x(total_lines: u64, col: i32) -> f32 {
    text_left(total_lines) + (col.max(0) as f32) * CHAR_W
}

/// Region-aware column x: shifted right past the sidebar.
pub fn text_x_in(region: Region, total_lines: u64, col: i32) -> f32 {
    text_left_in(region, total_lines) + (col.max(0) as f32) * CHAR_W
}

/// Y pixel (top) for a screen row index `row` (0-based, relative to the first
/// visible line).
pub fn row_y(row: i32) -> f32 {
    PAD + (row.max(0) as f32) * LINE_H
}

/// Region-aware row y: shifted down below the tab bar.
pub fn row_y_in(region: Region, row: i32) -> f32 {
    region.top + PAD + (row.max(0) as f32) * LINE_H
}

/// How many whole text rows fit in a window `height` px tall (no offset; the
/// region-aware `visible_rows_in` is what the IDE uses — kept for tests).
#[allow(dead_code)]
pub fn visible_rows(height: u32) -> u32 {
    if (height as f32) <= PAD {
        return 1;
    }
    let usable = height as f32 - PAD;
    ((usable / LINE_H).floor() as u32).max(1)
}

/// Region-aware visible-row count: the usable height is reduced by the tab bar
/// at the top and two bands at the bottom (prompt + status).
pub fn visible_rows_in(region: Region, height: u32) -> u32 {
    let reserved_bottom = 2.0 * LINE_H; // prompt band + status band
    let usable = height as f32 - region.top - PAD - reserved_bottom;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / LINE_H).floor() as u32).max(1)
}

/// Map the tab-bar pixel x to a tab index (`floor(x / TAB_W)`).
pub fn tab_index_at(x: f32) -> u32 {
    if x <= 0.0 {
        0
    } else {
        (x / TAB_W).floor() as u32
    }
}

/// Map a sidebar pixel y to a tree row index. Rows start at the tab-bar bottom
/// and advance by LINE_H. Returns the row index (caller bounds-checks).
pub fn tree_row_at(y: f32) -> u32 {
    if y <= TAB_BAR_H {
        0
    } else {
        ((y - TAB_BAR_H) / LINE_H).floor() as u32
    }
}

/// Y pixel (top) of tree sidebar row `i` (0-based, below the tab bar).
pub fn tree_row_y(i: i32) -> f32 {
    TAB_BAR_H + (i.max(0) as f32) * LINE_H
}

/// Map a pixel `(x, y)` to a logical `(line, col)`.
///
/// * `first_line` is the buffer line currently drawn at the top of the view.
/// * `total_lines` sizes the gutter (so clicks left of the text column map to
///   col 0 of the row).
///
/// Returns absolute buffer `line` (>= `first_line`) and `col` (both clamped to
/// >= 0). Callers clamp `col` to the actual line length.
#[allow(dead_code)]
pub fn pixel_to_cell(x: f32, y: f32, first_line: u64, total_lines: u64) -> (u64, u64) {
    pixel_to_cell_in(Region { top: 0.0, left: 0.0 }, x, y, first_line, total_lines)
}

/// Region-aware pixel→cell: subtracts the tab-bar top + sidebar left before the
/// row/col math, so clicks in the shifted text area map correctly.
pub fn pixel_to_cell_in(
    region: Region,
    x: f32,
    y: f32,
    first_line: u64,
    total_lines: u64,
) -> (u64, u64) {
    let row_top = region.top + PAD;
    let row = if y <= row_top {
        0
    } else {
        ((y - row_top) / LINE_H).floor() as u64
    };
    let line = first_line + row;

    let left = text_left_in(region, total_lines);
    let col = if x <= left {
        0
    } else {
        ((x - left) / CHAR_W).floor().max(0.0) as u64
    };
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_count_boundaries() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
        assert_eq!(digit_count(1234), 4);
    }

    #[test]
    fn gutter_grows_with_line_count() {
        let g1 = gutter_width(9); // 1 digit
        let g2 = gutter_width(10); // 2 digits
        let g3 = gutter_width(100); // 3 digits
        assert!(g2 > g1);
        assert!(g3 > g2);
        // 1 digit: 8 + 1*8 + 8 = 24
        assert_eq!(g1, 24.0);
        // 3 digits: 8 + 3*8 + 8 = 40
        assert_eq!(g3, 40.0);
    }

    #[test]
    fn text_x_offsets_past_gutter() {
        let left = text_left(100);
        assert_eq!(text_x(100, 0), left);
        assert_eq!(text_x(100, 3), left + 3.0 * CHAR_W);
        // Negative col clamps to 0.
        assert_eq!(text_x(100, -5), left);
    }

    #[test]
    fn visible_rows_math() {
        // 600px tall: (600-8)/18 = 32.8 -> 32 rows.
        assert_eq!(visible_rows(600), 32);
        // Tiny window still yields at least one row.
        assert_eq!(visible_rows(1), 1);
        assert_eq!(visible_rows(0), 1);
    }

    #[test]
    fn pixel_to_cell_inverts_layout() {
        let total = 50;
        // Click at the top-left of line 0, col 0.
        let (l, c) = pixel_to_cell(PAD, PAD, 0, total);
        assert_eq!((l, c), (0, 0));

        // Click on the 3rd visible row when scrolled to first_line=10.
        let y = row_y(3) + 2.0; // a little into the row
        let (l, _) = pixel_to_cell(text_left(total), y, 10, total);
        assert_eq!(l, 13);

        // Click near the center of column 5 maps back to col 5.
        let x = text_x(total, 5) + CHAR_W * 0.5;
        let (_, c) = pixel_to_cell(x, PAD, 0, total);
        assert_eq!(c, 5);

        // Click left of the text column clamps to col 0.
        let (_, c0) = pixel_to_cell(2.0, PAD, 0, total);
        assert_eq!(c0, 0);
    }

    #[test]
    fn region_shifts_rows_and_columns() {
        let r = region(true); // sidebar visible
        assert_eq!(r.top, TAB_BAR_H);
        assert_eq!(r.left, SIDEBAR_W);

        // row_y_in is shifted down by the tab bar; text_x_in shifted right.
        assert_eq!(row_y_in(r, 0), TAB_BAR_H + PAD);
        assert_eq!(row_y_in(r, 2), TAB_BAR_H + PAD + 2.0 * LINE_H);
        assert!(text_x_in(r, 100, 0) > text_x(100, 0));
        assert_eq!(text_x_in(r, 100, 0), SIDEBAR_W + text_left(100));

        // Sidebar hidden: left offset is 0, top still shifted.
        let r2 = region(false);
        assert_eq!(r2.left, 0.0);
        assert_eq!(text_left_in(r2, 100), text_left(100));
    }

    #[test]
    fn region_pixel_to_cell_round_trips() {
        let r = region(true);
        let total = 50;
        // Click at the top-left text cell of the shifted region.
        let (l, c) = pixel_to_cell_in(r, text_left_in(r, total), row_y_in(r, 0) + 1.0, 0, total);
        assert_eq!((l, c), (0, 0));
        // Click on visible row 3 maps to line 3.
        let (l3, _) = pixel_to_cell_in(r, text_left_in(r, total), row_y_in(r, 3) + 2.0, 0, total);
        assert_eq!(l3, 3);
        // Click at column 5.
        let x = text_x_in(r, total, 5) + CHAR_W * 0.5;
        let (_, c5) = pixel_to_cell_in(r, x, row_y_in(r, 0) + 1.0, 0, total);
        assert_eq!(c5, 5);
    }

    #[test]
    fn tab_index_mapping() {
        assert_eq!(tab_index_at(0.0), 0);
        assert_eq!(tab_index_at(TAB_W * 0.5), 0);
        assert_eq!(tab_index_at(TAB_W + 1.0), 1);
        assert_eq!(tab_index_at(TAB_W * 2.5), 2);
    }

    #[test]
    fn tree_row_mapping() {
        // y within the tab bar -> row 0.
        assert_eq!(tree_row_at(TAB_BAR_H - 1.0), 0);
        // First row just below the tab bar.
        assert_eq!(tree_row_at(TAB_BAR_H + 1.0), 0);
        assert_eq!(tree_row_at(TAB_BAR_H + LINE_H + 1.0), 1);
        assert_eq!(tree_row_at(TAB_BAR_H + 3.0 * LINE_H + 1.0), 3);
        // tree_row_y inverts.
        assert_eq!(tree_row_y(0), TAB_BAR_H);
        assert_eq!(tree_row_y(3), TAB_BAR_H + 3.0 * LINE_H);
    }

    #[test]
    fn visible_rows_in_reserves_bands() {
        let r = region(true);
        // Region-aware count is strictly less than the naive one (tab bar +
        // prompt + status reserved).
        let naive = visible_rows(600);
        let shifted = visible_rows_in(r, 600);
        assert!(shifted < naive, "shifted={shifted} naive={naive}");
        assert!(shifted >= 1);
    }
}
