//! Pure editor-layout math (gutter width, pixel<->cell mapping, visible rows).
//!
//! All integer line/col -> pixel conversion lives here on the Rust side because
//! v0.36 Mighty has no int->float cast (docs/mighty-language-lessons.md L19).
//! Keeping it pure (no GPU/context) makes it unit-testable and lets the scalar
//! ABI and the render loop agree on a single set of metrics.

/// Left/top padding of the editor surface, in pixels (8px spacing rhythm).
pub const PAD: f32 = crate::theme::SPACE;
/// Vertical advance per text line, in pixels (≈1.5 line-height, from the theme).
pub const LINE_H: f32 = crate::theme::LINE_HEIGHT;
/// Horizontal advance per monospace cell, in pixels (must match the bundled
/// JetBrains Mono advance at the editor font size for cursor/click alignment).
pub const CHAR_W: f32 = crate::theme::CHAR_W;
/// Gap (px) between the line-number gutter and the text column.
pub const GUTTER_GAP: f32 = crate::theme::SPACE;
/// Base 8px spacing unit (re-exported from the theme for layout sites).
pub const SPACE: f32 = crate::theme::SPACE;

/// Height (px) of the top tab bar (matches the mockup's 38px tabs row).
pub const TAB_BAR_H: f32 = 38.0;
/// Height (px) of the breadcrumb bar at the top of the editor body.
pub const BREADCRUMB_H: f32 = 30.0;
/// Width (px) of the far-left activity rail (icons column).
pub const RAIL_W: f32 = 52.0;
/// Width (px) of one tab in the tab bar (fixed-width tabs keep click→index math
/// trivial: `idx = floor((x - RAIL_W) / TAB_W)`).
pub const TAB_W: f32 = 150.0;
/// Width (px) of the file-tree sidebar content (right of the rail) when shown.
pub const SIDEBAR_W: f32 = 196.0;
/// Pixels of indentation per tree depth level.
pub const TREE_INDENT: f32 = 14.0;

/// Fraction of the window height the integrated terminal panel occupies when
/// open (a "lower third").
pub const TERM_FRACTION: f32 = 0.33;
/// Minimum terminal panel height (px) so it stays usable in small windows.
pub const TERM_MIN_H: f32 = 4.0 * LINE_H;

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

/// Compute the editor body region given whether the sidebar is visible. The
/// body sits right of the activity rail (+ sidebar) and below the tab bar +
/// breadcrumb.
pub fn region(sidebar_visible: bool) -> Region {
    Region {
        top: TAB_BAR_H + BREADCRUMB_H,
        left: RAIL_W + if sidebar_visible { SIDEBAR_W } else { 0.0 },
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
/// at the top and two bands at the bottom (prompt + status), plus the terminal
/// panel when it is open.
pub fn visible_rows_in(region: Region, height: u32, term_open: bool) -> u32 {
    let reserved_bottom = 2.0 * LINE_H; // prompt band + status band
    let term = if term_open {
        term_panel_height(height)
    } else {
        0.0
    };
    let usable = height as f32 - region.top - PAD - reserved_bottom - term;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / LINE_H).floor() as u32).max(1)
}

// ---------------------------------------------------------------------------
// Integrated terminal panel
// ---------------------------------------------------------------------------

/// Height (px) of the terminal panel for a window `height` tall: a lower third,
/// clamped to a usable minimum and to not exceed the window.
pub fn term_panel_height(height: u32) -> f32 {
    let h = height as f32;
    let frac = (h * TERM_FRACTION).floor();
    frac.max(TERM_MIN_H).min((h - 2.0 * LINE_H).max(0.0))
}

/// Top y (px) of the terminal panel: it sits directly above the prompt + status
/// bands at the very bottom of the window.
pub fn term_panel_top(height: u32) -> f32 {
    let h = height as f32;
    let reserved_bottom = 2.0 * LINE_H; // prompt + status bands
    (h - reserved_bottom - term_panel_height(height)).max(0.0)
}

/// Left x (px) of the terminal panel: right of the sidebar (so it lines up with
/// the editor body), or 0 when the sidebar is hidden.
pub fn term_panel_left(region: Region) -> f32 {
    region.left
}

/// Height (px) of the terminal panel's "TERMINAL" header band (above the grid).
pub const TERM_HEADER_H: f32 = LINE_H;

/// Number of whole terminal rows that fit in the panel below the header (`>= 1`).
pub fn term_grid_rows(height: u32) -> usize {
    let usable = term_panel_height(height) - TERM_HEADER_H - PAD * 0.5;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / LINE_H).floor() as usize).max(1)
}

/// Number of whole terminal columns that fit in the panel for window width `w`
/// and the given region left offset (`>= 1`).
pub fn term_grid_cols(width: u32, region: Region) -> usize {
    let usable = width as f32 - region.left - 2.0 * PAD;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / CHAR_W).floor() as usize).max(1)
}

/// X pixel of terminal cell column `col` within the panel.
pub fn term_cell_x(region: Region, col: usize) -> f32 {
    region.left + PAD + (col as f32) * CHAR_W
}

/// Y pixel (top) of terminal cell row `row` within the panel for window `height`
/// — below the "TERMINAL" header band.
pub fn term_cell_y(height: u32, row: usize) -> f32 {
    term_panel_top(height) + TERM_HEADER_H + (row as f32) * LINE_H
}

/// Map the tab-bar pixel x to a tab index (`floor((x - RAIL_W) / TAB_W)`).
pub fn tab_index_at(x: f32) -> u32 {
    if x <= RAIL_W {
        0
    } else {
        ((x - RAIL_W) / TAB_W).floor() as u32
    }
}

/// Y pixel (top) of the first file row in the sidebar — below the tab bar and
/// the dim uppercase section header.
pub fn tree_rows_top() -> f32 {
    TAB_BAR_H + PAD + LINE_H
}

/// Map a sidebar pixel y to a tree row index. Rows start at [`tree_rows_top`]
/// and advance by LINE_H. Returns the row index (caller bounds-checks).
pub fn tree_row_at(y: f32) -> u32 {
    let top = tree_rows_top();
    if y <= top {
        0
    } else {
        ((y - top) / LINE_H).floor() as u32
    }
}

/// Y pixel (top) of tree sidebar row `i` (0-based, below the header). The
/// sidebar draw computes row y inline; this is retained as the click-mapping
/// inverse and for tests.
#[allow(dead_code)]
pub fn tree_row_y(i: i32) -> f32 {
    tree_rows_top() + (i.max(0) as f32) * LINE_H
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
        // 1 digit: PAD + 1*CHAR_W + GUTTER_GAP = 8 + 9 + 8 = 25
        assert_eq!(g1, PAD + CHAR_W + GUTTER_GAP);
        // 3 digits: 8 + 3*9 + 8 = 43
        assert_eq!(g3, PAD + 3.0 * CHAR_W + GUTTER_GAP);
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
        // (600 - PAD) / LINE_H, floored, at least 1.
        let expected = (((600.0 - PAD) / LINE_H).floor() as u32).max(1);
        assert_eq!(visible_rows(600), expected);
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
        assert_eq!(r.top, TAB_BAR_H + BREADCRUMB_H);
        assert_eq!(r.left, RAIL_W + SIDEBAR_W);

        // row_y_in is shifted down by the tab bar + breadcrumb; text_x_in right.
        assert_eq!(row_y_in(r, 0), TAB_BAR_H + BREADCRUMB_H + PAD);
        assert_eq!(row_y_in(r, 2), TAB_BAR_H + BREADCRUMB_H + PAD + 2.0 * LINE_H);
        assert!(text_x_in(r, 100, 0) > text_x(100, 0));
        assert_eq!(text_x_in(r, 100, 0), RAIL_W + SIDEBAR_W + text_left(100));

        // Sidebar hidden: left offset is just the rail, top still shifted.
        let r2 = region(false);
        assert_eq!(r2.left, RAIL_W);
        assert_eq!(text_left_in(r2, 100), RAIL_W + text_left(100));
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
        assert_eq!(tab_index_at(RAIL_W + TAB_W * 0.5), 0);
        assert_eq!(tab_index_at(RAIL_W + TAB_W + 1.0), 1);
        assert_eq!(tab_index_at(RAIL_W + TAB_W * 2.5), 2);
    }

    #[test]
    fn tree_row_mapping() {
        let top = tree_rows_top();
        // y above the first row -> row 0.
        assert_eq!(tree_row_at(top - 1.0), 0);
        // First row just below the header.
        assert_eq!(tree_row_at(top + 1.0), 0);
        assert_eq!(tree_row_at(top + LINE_H + 1.0), 1);
        assert_eq!(tree_row_at(top + 3.0 * LINE_H + 1.0), 3);
        // tree_row_y inverts.
        assert_eq!(tree_row_y(0), top);
        assert_eq!(tree_row_y(3), top + 3.0 * LINE_H);
    }

    #[test]
    fn visible_rows_in_reserves_bands() {
        let r = region(true);
        // Region-aware count is strictly less than the naive one (tab bar +
        // prompt + status reserved).
        let naive = visible_rows(600);
        let shifted = visible_rows_in(r, 600, false);
        assert!(shifted < naive, "shifted={shifted} naive={naive}");
        assert!(shifted >= 1);
    }

    #[test]
    fn terminal_open_shrinks_editor_rows() {
        let r = region(true);
        let without = visible_rows_in(r, 600, false);
        let with = visible_rows_in(r, 600, true);
        assert!(with < without, "with={with} without={without}");
        assert!(with >= 1);
    }

    #[test]
    fn terminal_panel_geometry() {
        // Lower third of a 600px window, clamped to >= TERM_MIN_H.
        let h = term_panel_height(600);
        assert!(h >= TERM_MIN_H);
        // The panel top + its height + the two bottom bands stay within height.
        let top = term_panel_top(600);
        assert!(top + h + 2.0 * LINE_H <= 600.0 + 0.5);
        // Grid dimensions are positive.
        assert!(term_grid_rows(600) >= 1);
        let r = region(true);
        assert!(term_grid_cols(900, r) >= 1);
        // Cols shrink when the sidebar pushes the panel right.
        assert!(term_grid_cols(900, region(false)) > term_grid_cols(900, r));
    }
}
