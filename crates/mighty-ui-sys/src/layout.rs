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

/// X pixel where the text column starts (right edge of the gutter).
pub fn text_left(total_lines: u64) -> f32 {
    gutter_width(total_lines)
}

/// X pixel for `col` (0-based) within the text area.
pub fn text_x(total_lines: u64, col: i32) -> f32 {
    text_left(total_lines) + (col.max(0) as f32) * CHAR_W
}

/// Y pixel (top) for a screen row index `row` (0-based, relative to the first
/// visible line).
pub fn row_y(row: i32) -> f32 {
    PAD + (row.max(0) as f32) * LINE_H
}

/// How many whole text rows fit in a window `height` px tall.
pub fn visible_rows(height: u32) -> u32 {
    if (height as f32) <= PAD {
        return 1;
    }
    let usable = height as f32 - PAD;
    ((usable / LINE_H).floor() as u32).max(1)
}

/// Map a pixel `(x, y)` to a logical `(line, col)`.
///
/// * `first_line` is the buffer line currently drawn at the top of the view.
/// * `total_lines` sizes the gutter (so clicks left of the text column map to
///   col 0 of the row).
///
/// Returns absolute buffer `line` (>= `first_line`) and `col` (both clamped to
/// >= 0). Callers clamp `col` to the actual line length.
pub fn pixel_to_cell(x: f32, y: f32, first_line: u64, total_lines: u64) -> (u64, u64) {
    let row = if y <= PAD {
        0
    } else {
        ((y - PAD) / LINE_H).floor() as u64
    };
    let line = first_line + row;

    let left = text_left(total_lines);
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
}
