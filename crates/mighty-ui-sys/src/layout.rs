//! Pure editor-layout math (gutter width, pixel<->cell mapping, visible rows).
//!
//! All integer line/col -> pixel conversion lives here on the Rust side because
//! v0.36 Mighty has no int->float cast (docs/mighty-language-lessons.md L19).
//! Keeping it pure (no GPU/context) makes it unit-testable and lets the scalar
//! ABI and the render loop agree on a single set of metrics.

/// Left/top padding of the editor surface, in pixels (8px spacing rhythm).
pub const PAD: f32 = crate::theme::SPACE;
/// Vertical advance per text line, in pixels — LIVE from the active settings
/// (editor font size; see [`crate::settings`]). A function (not a const) so
/// changing the font size in the Settings panel re-flows the editor next frame.
#[inline]
#[allow(non_snake_case)]
pub fn LINE_H() -> f32 {
    crate::theme::LINE_HEIGHT()
}
/// Horizontal advance per monospace cell, in pixels — LIVE from the active
/// settings (must match the bundled JetBrains Mono advance at the editor font
/// size for cursor/click alignment).
#[inline]
#[allow(non_snake_case)]
pub fn CHAR_W() -> f32 {
    crate::theme::CHAR_W()
}
/// Gap (px) between the line-number gutter and the text column.
pub const GUTTER_GAP: f32 = crate::theme::SPACE;
/// Base 8px spacing unit (re-exported from the theme for layout sites).
#[allow(dead_code)]
pub const SPACE: f32 = crate::theme::SPACE;

/// Height (px) of the top tab bar (matches the mockup's 40px tabs row).
pub const TAB_BAR_H: f32 = 40.0;
/// Height (px) of the breadcrumb bar at the top of the editor body.
pub const BREADCRUMB_H: f32 = 30.0;
/// Width (px) of the far-left activity rail (icons column) — mockup `52px`.
pub const RAIL_W: f32 = 52.0;
/// Width (px) of one tab in the tab bar (fixed-width tabs keep click→index math
/// trivial: `idx = floor((x - RAIL_W) / TAB_W)`).
pub const TAB_W: f32 = 160.0;
/// Width (px) of the file-tree sidebar content (right of the rail) when shown.
/// Mockup sidebar column is `248px` total; rail is separate, so the panel is
/// `248 - 52 = 196`… but the mockup's body grid is `52px 248px 1fr`, meaning the
/// sidebar panel itself is 248. Match that.
pub const SIDEBAR_W: f32 = 248.0;
/// Pixels of indentation per tree depth level (mockup `.indent` = 16px).
pub const TREE_INDENT: f32 = 16.0;

/// Fraction of the window height the integrated terminal panel occupies when
/// open (a "lower third").
pub const TERM_FRACTION: f32 = 0.33;
/// Minimum terminal panel height (px) so it stays usable in small windows.
/// A function (depends on the live line height).
#[inline]
pub fn term_min_h() -> f32 {
    4.0 * LINE_H()
}

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

/// Comfortable left/top margin for the centered Zen editor (px). The editor
/// content column is inset by this on both sides so a full-window distraction-
/// free editor isn't flush to the glass edges.
pub const ZEN_MARGIN_X: f32 = 64.0;
pub const ZEN_MARGIN_TOP: f32 = 28.0;

/// Authoritative **Zen / focus mode** flag, mirrored from the IDE's toggle.
///
/// Zen mode is a global (mirroring the [`crate::settings`] / [`crate::theme`]
/// active-value pattern) so [`region`] — called from ~40 draw/click sites — can
/// be zen-aware WITHOUT threading a flag through every one. When set, the rail /
/// sidebar / tab bar / breadcrumb / status bar are hidden and the editor body
/// fills the window with a comfortable margin.
static ZEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Read the active Zen flag.
#[inline]
pub fn zen_active() -> bool {
    ZEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the active Zen flag (effective next frame: the region recomputes).
#[inline]
pub fn set_zen(on: bool) {
    ZEN.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Pure region math for an explicit chrome state. In **Zen mode** the rail /
/// sidebar / tab bar / breadcrumb are hidden and the body fills the window with
/// a comfortable margin; otherwise the body sits right of the rail (+ sidebar)
/// and below the tab bar + breadcrumb. Pure (no global) so it is unit-testable
/// without racing the [`ZEN`] global.
pub fn region_chrome(sidebar_visible: bool, zen: bool) -> Region {
    if zen {
        return Region {
            top: ZEN_MARGIN_TOP,
            left: ZEN_MARGIN_X,
        };
    }
    Region {
        top: TAB_BAR_H + BREADCRUMB_H,
        left: RAIL_W + if sidebar_visible { SIDEBAR_W } else { 0.0 },
    }
}

/// Compute the editor body region given whether the sidebar is visible. Reads
/// the active [`zen_active`] flag so EVERY draw/click site is zen-aware without
/// threading the flag through. See [`region_chrome`] for the pure form.
pub fn region(sidebar_visible: bool) -> Region {
    region_chrome(sidebar_visible, zen_active())
}

/// Width (px) of the vertical divider between split editor panes.
pub const PANE_DIVIDER_W: f32 = 1.0;

/// Pixel column bounds `[left, right)` of pane `i` of `count` panes, given the
/// editor body's `region` left edge and the window width `win_w`. The body
/// `[region.left, win_w)` is divided into `count` equal columns separated by a
/// [`PANE_DIVIDER_W`]-px divider. With `count == 1` this returns the full body
/// span (`region.left .. win_w`) so the unsplit path is identical to today.
///
/// Returns `(left, right)`; callers derive a per-pane [`Region`] (same `top`,
/// `left = left`) and clip drawing to `right`.
pub fn pane_bounds(region: Region, win_w: f32, count: usize, i: usize) -> (f32, f32) {
    let count = count.max(1);
    let body_left = region.left;
    let total = (win_w - body_left).max(0.0);
    if count == 1 {
        return (body_left, win_w);
    }
    let dividers = (count - 1) as f32 * PANE_DIVIDER_W;
    let col_w = ((total - dividers) / count as f32).max(0.0);
    let i = i.min(count - 1);
    let left = body_left + (i as f32) * (col_w + PANE_DIVIDER_W);
    let right = left + col_w;
    (left, right)
}

/// The per-pane editor [`Region`] for pane `i` of `count`: the same top as the
/// shared editor body, but its left edge shifted to the pane's column start.
pub fn pane_region(region: Region, win_w: f32, count: usize, i: usize) -> Region {
    let (left, _right) = pane_bounds(region, win_w, count, i);
    Region {
        top: region.top,
        left,
    }
}

/// X pixel of the divider drawn to the RIGHT of pane `i` (only meaningful for
/// `i < count - 1`). The divider occupies `[x, x + PANE_DIVIDER_W)`.
pub fn pane_divider_x(region: Region, win_w: f32, count: usize, i: usize) -> f32 {
    let (_left, right) = pane_bounds(region, win_w, count, i);
    right
}

/// Map a pixel `x` to the pane index it falls in (for click→focus). Clamps to
/// `[0, count-1]`. With `count == 1` always returns 0.
pub fn pane_at_x(region: Region, win_w: f32, count: usize, x: f32) -> usize {
    let count = count.max(1);
    if count == 1 {
        return 0;
    }
    for i in 0..count {
        let (_left, right) = pane_bounds(region, win_w, count, i);
        if x < right + PANE_DIVIDER_W {
            return i;
        }
    }
    count - 1
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
    PAD + (digits as f32) * CHAR_W() + GUTTER_GAP
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
    text_left(total_lines) + (col.max(0) as f32) * CHAR_W()
}

/// Region-aware column x: shifted right past the sidebar.
pub fn text_x_in(region: Region, total_lines: u64, col: i32) -> f32 {
    text_left_in(region, total_lines) + (col.max(0) as f32) * CHAR_W()
}

/// Y pixel (top) for a screen row index `row` (0-based, relative to the first
/// visible line).
pub fn row_y(row: i32) -> f32 {
    PAD + (row.max(0) as f32) * LINE_H()
}

/// Region-aware row y: shifted down below the tab bar.
pub fn row_y_in(region: Region, row: i32) -> f32 {
    region.top + PAD + (row.max(0) as f32) * LINE_H()
}

/// How many whole text rows fit in a window `height` px tall (no offset; the
/// region-aware `visible_rows_in` is what the IDE uses — kept for tests).
#[allow(dead_code)]
pub fn visible_rows(height: u32) -> u32 {
    if (height as f32) <= PAD {
        return 1;
    }
    let usable = height as f32 - PAD;
    ((usable / LINE_H()).floor() as u32).max(1)
}

/// Region-aware visible-row count: the usable height is reduced by the tab bar
/// at the top and two bands at the bottom (prompt + status), plus the terminal
/// panel when it is open.
pub fn visible_rows_in(region: Region, height: u32, term_open: bool) -> u32 {
    let reserved_bottom = 2.0 * LINE_H(); // prompt band + status band
    let term = if term_open {
        term_panel_height(height)
    } else {
        0.0
    };
    let usable = height as f32 - region.top - PAD - reserved_bottom - term;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / LINE_H()).floor() as u32).max(1)
}

// ---------------------------------------------------------------------------
// Integrated terminal panel
// ---------------------------------------------------------------------------

/// Height (px) of the terminal panel for a window `height` tall: a lower third,
/// clamped to a usable minimum and to not exceed the window.
pub fn term_panel_height(height: u32) -> f32 {
    let h = height as f32;
    let frac = (h * TERM_FRACTION).floor();
    frac.max(term_min_h()).min((h - 2.0 * LINE_H()).max(0.0))
}

/// Top y (px) of the terminal panel: it sits directly above the prompt + status
/// bands at the very bottom of the window.
pub fn term_panel_top(height: u32) -> f32 {
    let h = height as f32;
    let reserved_bottom = 2.0 * LINE_H(); // prompt + status bands
    (h - reserved_bottom - term_panel_height(height)).max(0.0)
}

/// Left x (px) of the terminal panel: right of the sidebar (so it lines up with
/// the editor body), or 0 when the sidebar is hidden.
pub fn term_panel_left(region: Region) -> f32 {
    region.left
}

/// Height (px) of the terminal panel's "TERMINAL" header band (above the grid).
#[inline]
pub fn term_header_h() -> f32 {
    LINE_H()
}

/// Number of whole terminal rows that fit in the panel below the header (`>= 1`).
pub fn term_grid_rows(height: u32) -> usize {
    let usable = term_panel_height(height) - term_header_h() - PAD * 0.5;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / LINE_H()).floor() as usize).max(1)
}

/// Number of whole terminal columns that fit in the panel for window width `w`
/// and the given region left offset (`>= 1`).
pub fn term_grid_cols(width: u32, region: Region) -> usize {
    let usable = width as f32 - region.left - 2.0 * PAD;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / CHAR_W()).floor() as usize).max(1)
}

/// X pixel of terminal cell column `col` within the panel.
pub fn term_cell_x(region: Region, col: usize) -> f32 {
    region.left + PAD + (col as f32) * CHAR_W()
}

/// Y pixel (top) of terminal cell row `row` within the panel for window `height`
/// — below the "TERMINAL" header band.
pub fn term_cell_y(height: u32, row: usize) -> f32 {
    term_panel_top(height) + term_header_h() + (row as f32) * LINE_H()
}

/// Map the tab-bar pixel x to a tab index (`floor((x - RAIL_W) / TAB_W)`).
/// Retained for the unit test + as the no-sidebar inverse; the live click
/// handler ([`crate::mui_tab_index_at_click`]) offsets by the sidebar width.
#[allow(dead_code)]
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
    // Sidebar header band (40px) + 6px gap, matching `mui_sidebar_draw`.
    40.0 + 6.0
}

/// Map a sidebar pixel y to a tree row index. Rows start at [`tree_rows_top`]
/// and advance by LINE_H(). Returns the row index (caller bounds-checks).
pub fn tree_row_at(y: f32) -> u32 {
    let top = tree_rows_top();
    if y <= top {
        0
    } else {
        ((y - top) / LINE_H()).floor() as u32
    }
}

/// Y pixel (top) of tree sidebar row `i` (0-based, below the header). The
/// sidebar draw computes row y inline; this is retained as the click-mapping
/// inverse and for tests.
#[allow(dead_code)]
pub fn tree_row_y(i: i32) -> f32 {
    tree_rows_top() + (i.max(0) as f32) * LINE_H()
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
        ((y - row_top) / LINE_H()).floor() as u64
    };
    let line = first_line + row;

    let left = text_left_in(region, total_lines);
    let col = if x <= left {
        0
    } else {
        ((x - left) / CHAR_W()).floor().max(0.0) as u64
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
        // 1 digit: PAD + 1*CHAR_W() + GUTTER_GAP = 8 + 9 + 8 = 25
        assert_eq!(g1, PAD + CHAR_W() + GUTTER_GAP);
        // 3 digits: 8 + 3*9 + 8 = 43
        assert_eq!(g3, PAD + 3.0 * CHAR_W() + GUTTER_GAP);
    }

    #[test]
    fn text_x_offsets_past_gutter() {
        let left = text_left(100);
        assert_eq!(text_x(100, 0), left);
        assert_eq!(text_x(100, 3), left + 3.0 * CHAR_W());
        // Negative col clamps to 0.
        assert_eq!(text_x(100, -5), left);
    }

    #[test]
    fn visible_rows_math() {
        // (600 - PAD) / LINE_H(), floored, at least 1.
        let expected = (((600.0 - PAD) / LINE_H()).floor() as u32).max(1);
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
        let x = text_x(total, 5) + CHAR_W() * 0.5;
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
        assert_eq!(row_y_in(r, 2), TAB_BAR_H + BREADCRUMB_H + PAD + 2.0 * LINE_H());
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
        let x = text_x_in(r, total, 5) + CHAR_W() * 0.5;
        let (_, c5) = pixel_to_cell_in(r, x, row_y_in(r, 0) + 1.0, 0, total);
        assert_eq!(c5, 5);
    }

    #[test]
    fn zen_region_hides_chrome_and_centers() {
        // Pure form (no global): non-zen is the normal chrome-offset region.
        let normal = region_chrome(true, false);
        assert_eq!(normal.top, TAB_BAR_H + BREADCRUMB_H);
        assert_eq!(normal.left, RAIL_W + SIDEBAR_W);
        // Zen drops the rail/sidebar/tab-bar/breadcrumb offsets to a small margin.
        let zen = region_chrome(true, true);
        assert_eq!(zen.top, ZEN_MARGIN_TOP);
        assert_eq!(zen.left, ZEN_MARGIN_X);
        assert!(zen.left < normal.left, "zen body starts further left");
        assert!(zen.top < normal.top, "zen body starts higher");
        // Sidebar visibility is irrelevant in zen (chrome is hidden either way).
        assert_eq!(region_chrome(false, true), region_chrome(true, true));
    }

    #[test]
    fn zen_global_drives_region() {
        // Mutates the process-global ZEN flag; restore it after so the other
        // (zen-off) region tests aren't affected. Serialized by acquiring the
        // crate test lock.
        let _g = crate::settings::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let before = zen_active();
        set_zen(false);
        assert_eq!(region(true), region_chrome(true, false));
        set_zen(true);
        assert_eq!(region(true), region_chrome(true, true));
        assert!(zen_active());
        set_zen(before);
    }

    #[test]
    fn pane_bounds_single_pane_is_full_body() {
        let r = region(true);
        let win_w = 1320.0;
        // count == 1: the full body span, identical to the unsplit editor.
        let (l, rgt) = pane_bounds(r, win_w, 1, 0);
        assert_eq!(l, r.left);
        assert_eq!(rgt, win_w);
        // pane_region for a single pane == the shared region.
        assert_eq!(pane_region(r, win_w, 1, 0), r);
        // pane_at_x always 0.
        assert_eq!(pane_at_x(r, win_w, 1, 5.0), 0);
        assert_eq!(pane_at_x(r, win_w, 1, win_w - 1.0), 0);
    }

    #[test]
    fn pane_bounds_two_columns_with_divider() {
        let r = region(true);
        let win_w = 1320.0;
        let (l0, r0) = pane_bounds(r, win_w, 2, 0);
        let (l1, r1) = pane_bounds(r, win_w, 2, 1);
        // Pane 0 starts at the body left; pane 1 ends at the window right.
        assert_eq!(l0, r.left);
        assert!((r1 - win_w).abs() < 0.001, "r1={r1}");
        // Equal column widths.
        let w0 = r0 - l0;
        let w1 = r1 - l1;
        assert!((w0 - w1).abs() < 0.001, "w0={w0} w1={w1}");
        // The divider sits between the two columns: pane 1's left = pane 0's
        // right + divider width.
        assert!((l1 - (r0 + PANE_DIVIDER_W)).abs() < 0.001, "l1={l1} r0={r0}");
        // The two columns + the divider fill the whole body span.
        let total = win_w - r.left;
        assert!((w0 + w1 + PANE_DIVIDER_W - total).abs() < 0.001);
        // Divider x for pane 0 == pane 0's right edge.
        assert_eq!(pane_divider_x(r, win_w, 2, 0), r0);
    }

    #[test]
    fn pane_region_shifts_left_for_right_pane() {
        let r = region(true);
        let win_w = 1320.0;
        let pr0 = pane_region(r, win_w, 2, 0);
        let pr1 = pane_region(r, win_w, 2, 1);
        assert_eq!(pr0.top, r.top);
        assert_eq!(pr1.top, r.top);
        assert_eq!(pr0.left, r.left);
        assert!(pr1.left > pr0.left, "right pane starts further right");
    }

    #[test]
    fn pane_at_x_maps_click_to_column() {
        let r = region(true);
        let win_w = 1320.0;
        let (l0, r0) = pane_bounds(r, win_w, 2, 0);
        let (l1, _r1) = pane_bounds(r, win_w, 2, 1);
        // A click in the middle of the left column -> pane 0.
        assert_eq!(pane_at_x(r, win_w, 2, (l0 + r0) * 0.5), 0);
        // A click well inside the right column -> pane 1.
        assert_eq!(pane_at_x(r, win_w, 2, l1 + 20.0), 1);
        // A click past the right edge clamps to the last pane.
        assert_eq!(pane_at_x(r, win_w, 2, win_w + 100.0), 1);
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
        assert_eq!(tree_row_at(top + LINE_H() + 1.0), 1);
        assert_eq!(tree_row_at(top + 3.0 * LINE_H() + 1.0), 3);
        // tree_row_y inverts.
        assert_eq!(tree_row_y(0), top);
        assert_eq!(tree_row_y(3), top + 3.0 * LINE_H());
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
        // Lower third of a 600px window, clamped to >= term_min_h().
        let h = term_panel_height(600);
        assert!(h >= term_min_h());
        // The panel top + its height + the two bottom bands stay within height.
        let top = term_panel_top(600);
        assert!(top + h + 2.0 * LINE_H() <= 600.0 + 0.5);
        // Grid dimensions are positive.
        assert!(term_grid_rows(600) >= 1);
        let r = region(true);
        assert!(term_grid_cols(900, r) >= 1);
        // Cols shrink when the sidebar pushes the panel right.
        assert!(term_grid_cols(900, region(false)) > term_grid_cols(900, r));
    }
}
