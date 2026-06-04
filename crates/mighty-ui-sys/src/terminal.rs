//! Integrated terminal: PTY-backed shell + minimal VT parser + character grid.
//!
//! Mighty (v0.36) can't hold strings/pointers/threads/Vecs of structs across
//! FFI (L17/L21), so the entire terminal lives here on the Rust side and is
//! driven through the scalar ABI in [`crate::abi`]. The three pieces:
//!
//! * [`Grid`] — a rows×cols matrix of [`Cell`]s (codepoint + fg color) plus a
//!   cursor; the only stateful UI surface, drawn shim-side.
//! * [`VtParser`] — a deliberately small VT/ANSI interpreter that feeds bytes
//!   into the grid: printable UTF-8, common C0/C1 controls, SGR colors, cursor
//!   movement, erase/scroll/editing CSI sequences, and terminal query replies.
//!   Unsupported CSI/OSC/string sequences are consumed so they never corrupt the
//!   grid. This is NOT a full xterm — just enough to run a shell.
//! * [`Terminal`] — spawns a real shell with `portable-pty` (ConPTY on Windows),
//!   pumps its output on a background thread into a shared byte buffer, and on
//!   [`Terminal::pump`] drains that buffer through the parser into the grid.
//!   Keystrokes are mapped to bytes and written to the PTY stdin.
//!
//! Scrollback is intentionally NOT retained beyond the visible grid: when the
//! cursor advances past the last row the grid scrolls up one line (oldest row
//! dropped). This keeps the model a fixed rows×cols matrix that Mighty never has
//! to touch (it just calls `mui_term_draw`).

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{mpsc, Arc, Mutex};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};

/// One terminal cell: a Unicode scalar value and palette color indices.
///
/// `fg`/`bg` are color codes: xterm palette indices 0..=255, encoded RGB
/// truecolor values, or sentinels for "use the default". Keeping colors encoded
/// (not expanded RGBA) means the draw path resolves them to concrete colors,
/// and the grid stays compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    pub underline: bool,
    pub strikethrough: bool,
    pub italic: bool,
    pub faint: bool,
    pub overline: bool,
    pub conceal: bool,
    pub blink: bool,
}

/// Sentinel `fg` meaning "default foreground" (SGR 0 / 39).
pub const DEFAULT_FG: u32 = 0xffff_ffff;
/// Sentinel `bg` meaning "transparent/default background" (SGR 0 / 49).
pub const DEFAULT_BG: u32 = 0xffff_fffe;
const TRUECOLOR_MASK: u32 = 0x0100_0000;
const DEFAULT_FG_RGB: (u8, u8, u8) = (0xd1, 0xd6, 0xe0);
const DEFAULT_BG_RGB: (u8, u8, u8) = (0x14, 0x14, 0x1c);
const DEFAULT_CURSOR_RGB: (u8, u8, u8) = (0x7c, 0x5c, 0xff);
const MAX_OSC_BYTES: usize = 8192;
const MAX_OSC_52_DECODED_BYTES: usize = 6144;
const MAX_OSC_52_TEXT_CHARS: usize = 4096;
const MOUSE_MODE_BUTTON: u8 = 1 << 0; // DECSET ?1000
const MOUSE_MODE_DRAG: u8 = 1 << 1; // DECSET ?1002
const MOUSE_MODE_ANY: u8 = 1 << 2; // DECSET ?1003

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            underline: false,
            strikethrough: false,
            italic: false,
            faint: false,
            overline: false,
            conceal: false,
            blink: false,
        }
    }
}

fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols).map(|col| col > 0 && col % 8 == 0).collect()
}

#[derive(Clone, Debug)]
struct ScreenSnapshot {
    rows: usize,
    cols: usize,
    cells: Vec<Cell>,
    cur_row: usize,
    cur_col: usize,
    cur_fg: u32,
    cur_bg: u32,
    scroll_top: usize,
    scroll_bottom: usize,
}

/// A fixed-size character grid with a cursor. Rows are stored top-to-bottom; the
/// cursor is a (row, col) within bounds. Writing past the last column wraps to
/// the next row; writing past the last row scrolls the whole grid up one line.
#[derive(Debug)]
pub struct Grid {
    rows: usize,
    cols: usize,
    /// `rows * cols` cells in row-major order.
    cells: Vec<Cell>,
    cur_row: usize,
    cur_col: usize,
    /// Current SGR foreground applied to newly-written cells.
    cur_fg: u32,
    /// Current SGR background applied to newly-written cells.
    cur_bg: u32,
    /// Inclusive scroll-region top row. Defaults to the full grid.
    scroll_top: usize,
    /// Inclusive scroll-region bottom row. Defaults to the full grid.
    scroll_bottom: usize,
    /// Horizontal tab stops. Defaults to every 8 columns, excluding column 0.
    tab_stops: Vec<bool>,
    primary_screen: Option<ScreenSnapshot>,
}

impl Grid {
    pub fn new(rows: usize, cols: usize) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Grid {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            cur_row: 0,
            cur_col: 0,
            cur_fg: DEFAULT_FG,
            cur_bg: DEFAULT_BG,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            tab_stops: default_tab_stops(cols),
            primary_screen: None,
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cur_row, self.cur_col.min(self.cols - 1))
    }

    fn raw_cursor(&self) -> (usize, usize) {
        (self.cur_row, self.cur_col)
    }

    /// Cell at (row, col), or a default cell if out of range.
    pub fn cell(&self, row: usize, col: usize) -> Cell {
        if row < self.rows && col < self.cols {
            self.cells[row * self.cols + col]
        } else {
            Cell::default()
        }
    }

    /// Resize the grid to `rows`×`cols`, preserving the top-left overlap of the
    /// old contents and clamping the cursor. A no-op if the size is unchanged.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        let was_full_scroll_region = self.scroll_top == 0 && self.scroll_bottom == self.rows - 1;
        let mut next = vec![Cell::default(); rows * cols];
        let copy_rows = rows.min(self.rows);
        let copy_cols = cols.min(self.cols);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                next[r * cols + c] = self.cells[r * self.cols + c];
            }
        }
        self.cells = next;
        self.rows = rows;
        self.cols = cols;
        self.cur_row = self.cur_row.min(rows - 1);
        self.cur_col = self.cur_col.min(cols);
        let mut tab_stops = vec![false; cols];
        for (idx, stop) in self.tab_stops.iter().copied().enumerate().take(cols) {
            tab_stops[idx] = stop;
        }
        self.tab_stops = tab_stops;
        if was_full_scroll_region {
            self.reset_scroll_region();
        } else {
            self.scroll_top = self.scroll_top.min(rows - 1);
            self.scroll_bottom = self.scroll_bottom.min(rows - 1).max(self.scroll_top);
        }
    }

    fn snapshot(&self) -> ScreenSnapshot {
        ScreenSnapshot {
            rows: self.rows,
            cols: self.cols,
            cells: self.cells.clone(),
            cur_row: self.cur_row,
            cur_col: self.cur_col,
            cur_fg: self.cur_fg,
            cur_bg: self.cur_bg,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
        }
    }

    fn restore_snapshot(&mut self, snapshot: ScreenSnapshot) {
        self.cells.fill(Cell::default());
        let copy_rows = self.rows.min(snapshot.rows);
        let copy_cols = self.cols.min(snapshot.cols);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                self.cells[r * self.cols + c] = snapshot.cells[r * snapshot.cols + c];
            }
        }
        self.cur_row = snapshot.cur_row.min(self.rows - 1);
        self.cur_col = snapshot.cur_col.min(self.cols);
        self.cur_fg = snapshot.cur_fg;
        self.cur_bg = snapshot.cur_bg;
        self.scroll_top = snapshot.scroll_top.min(self.rows - 1);
        self.scroll_bottom = snapshot.scroll_bottom.min(self.rows - 1).max(self.scroll_top);
    }

    fn enter_alternate_screen(&mut self) {
        if self.primary_screen.is_none() {
            self.primary_screen = Some(self.snapshot());
        }
        self.cells.fill(Cell::default());
        self.cur_row = 0;
        self.cur_col = 0;
        self.reset_scroll_region();
    }

    fn exit_alternate_screen(&mut self) {
        if let Some(snapshot) = self.primary_screen.take() {
            self.restore_snapshot(snapshot);
        }
    }

    fn alternate_screen_active(&self) -> bool {
        self.primary_screen.is_some()
    }

    /// Clear all cells to blanks and home the cursor.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
        self.primary_screen = None;
        self.cur_row = 0;
        self.cur_col = 0;
        self.reset_scroll_region();
    }

    /// Whether the visible grid differs from a freshly-cleared terminal grid.
    pub fn has_visible_content(&self) -> bool {
        self.cur_row != 0
            || self.cur_col != 0
            || self.scroll_top != 0
            || self.scroll_bottom != self.rows.saturating_sub(1)
            || self.cells.iter().any(|c| *c != Cell::default())
    }

    fn clear_from_cursor_to_end(&mut self) {
        let start = self.cur_row * self.cols + self.cur_col.min(self.cols - 1);
        for c in &mut self.cells[start..] {
            *c = Cell::default();
        }
    }

    fn clear_from_start_to_cursor(&mut self) {
        let end = self.cur_row * self.cols + self.cur_col.min(self.cols - 1);
        for c in &mut self.cells[..=end] {
            *c = Cell::default();
        }
    }

    fn clear_line_from_cursor_to_end(&mut self) {
        let row_start = self.cur_row * self.cols;
        let start = row_start + self.cur_col.min(self.cols - 1);
        let end = row_start + self.cols;
        for c in &mut self.cells[start..end] {
            *c = Cell::default();
        }
    }

    fn clear_line_from_start_to_cursor(&mut self) {
        let row_start = self.cur_row * self.cols;
        let end = row_start + self.cur_col.min(self.cols - 1);
        for c in &mut self.cells[row_start..=end] {
            *c = Cell::default();
        }
    }

    fn clear_line(&mut self) {
        let row_start = self.cur_row * self.cols;
        let end = row_start + self.cols;
        for c in &mut self.cells[row_start..end] {
            *c = Cell::default();
        }
    }

    fn insert_blank_chars(&mut self, count: usize) {
        let col = self.cur_col.min(self.cols - 1);
        let count = count.max(1).min(self.cols - col);
        let row_start = self.cur_row * self.cols;
        for c in (col..self.cols - count).rev() {
            self.cells[row_start + c + count] = self.cells[row_start + c];
        }
        for c in col..col + count {
            self.cells[row_start + c] = Cell::default();
        }
    }

    fn prepare_insert(&mut self, autowrap: bool) {
        if self.cur_col >= self.cols {
            if autowrap {
                self.newline();
            } else {
                self.cur_col = self.cols - 1;
            }
        }
        self.insert_blank_chars(1);
    }

    fn delete_chars(&mut self, count: usize) {
        let col = self.cur_col.min(self.cols - 1);
        let count = count.max(1).min(self.cols - col);
        let row_start = self.cur_row * self.cols;
        for c in col..self.cols - count {
            self.cells[row_start + c] = self.cells[row_start + c + count];
        }
        for c in self.cols - count..self.cols {
            self.cells[row_start + c] = Cell::default();
        }
    }

    fn erase_chars(&mut self, count: usize) {
        let col = self.cur_col.min(self.cols - 1);
        let count = count.max(1).min(self.cols - col);
        let row_start = self.cur_row * self.cols;
        for c in col..col + count {
            self.cells[row_start + c] = Cell::default();
        }
    }

    fn insert_blank_lines(&mut self, count: usize) {
        if self.cur_row < self.scroll_top || self.cur_row > self.scroll_bottom {
            return;
        }
        let count = count.max(1).min(self.scroll_bottom - self.cur_row + 1);
        if count <= self.scroll_bottom - self.cur_row {
            for row in (self.cur_row..=self.scroll_bottom - count).rev() {
                for col in 0..self.cols {
                    self.cells[(row + count) * self.cols + col] =
                        self.cells[row * self.cols + col];
                }
            }
        }
        for row in self.cur_row..self.cur_row + count {
            self.clear_row(row);
        }
    }

    fn delete_lines(&mut self, count: usize) {
        if self.cur_row < self.scroll_top || self.cur_row > self.scroll_bottom {
            return;
        }
        let count = count.max(1).min(self.scroll_bottom - self.cur_row + 1);
        if count <= self.scroll_bottom - self.cur_row {
            for row in self.cur_row..=self.scroll_bottom - count {
                for col in 0..self.cols {
                    self.cells[row * self.cols + col] =
                        self.cells[(row + count) * self.cols + col];
                }
            }
        }
        for row in self.scroll_bottom + 1 - count..=self.scroll_bottom {
            self.clear_row(row);
        }
    }

    fn clear_row(&mut self, row: usize) {
        let start = row * self.cols;
        for cell in &mut self.cells[start..start + self.cols] {
            *cell = Cell::default();
        }
    }

    /// Scroll the active region up `count` lines: drop top rows, shift the rest
    /// up, blank the bottom rows. Used by newline-at-bottom and `CSI S`.
    fn scroll_up(&mut self, count: usize) {
        let height = self.scroll_bottom - self.scroll_top + 1;
        let count = count.max(1).min(height);
        if count < height {
            for row in self.scroll_top..=self.scroll_bottom - count {
                for col in 0..self.cols {
                    self.cells[row * self.cols + col] =
                        self.cells[(row + count) * self.cols + col];
                }
            }
        }
        for row in self.scroll_bottom + 1 - count..=self.scroll_bottom {
            self.clear_row(row);
        }
    }

    /// Scroll the active region down `count` lines: drop bottom rows, shift the
    /// rest down, blank the top rows. Used by `CSI T`.
    fn scroll_down(&mut self, count: usize) {
        let height = self.scroll_bottom - self.scroll_top + 1;
        let count = count.max(1).min(height);
        if count < height {
            for row in (self.scroll_top + count..=self.scroll_bottom).rev() {
                for col in 0..self.cols {
                    self.cells[row * self.cols + col] =
                        self.cells[(row - count) * self.cols + col];
                }
            }
        }
        for row in self.scroll_top..self.scroll_top + count {
            self.clear_row(row);
        }
    }

    /// Advance the cursor to the start of the next line, scrolling if needed.
    fn newline(&mut self) {
        self.cur_col = 0;
        if self.cur_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cur_row + 1 < self.rows {
            self.cur_row += 1;
        }
    }

    /// VT Index (IND): move down one row without changing the column, scrolling
    /// the active region when already at the bottom margin.
    fn index(&mut self) {
        if self.cur_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cur_row + 1 < self.rows {
            self.cur_row += 1;
        }
    }

    /// VT Reverse Index (RI): move up one row without changing the column,
    /// scrolling the active region down when already at the top margin.
    fn reverse_index(&mut self) {
        if self.cur_row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cur_row > 0 {
            self.cur_row -= 1;
        }
    }

    fn put_cell_autowrap(&mut self, cell: Cell, autowrap: bool) {
        if self.cur_col >= self.cols {
            if autowrap {
                // Wrap before writing.
                self.newline();
            } else {
                self.cur_col = self.cols - 1;
            }
        }
        let idx = self.cur_row * self.cols + self.cur_col;
        self.cells[idx] = cell;
        if autowrap {
            self.cur_col += 1;
        } else {
            self.cur_col = (self.cur_col + 1).min(self.cols - 1);
        }
    }

    fn backspace(&mut self) {
        if self.cur_col > 0 {
            self.cur_col -= 1;
        } else if self.cur_row > 0 {
            self.cur_row -= 1;
            self.cur_col = self.cols - 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cur_col = 0;
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) -> bool {
        if top >= bottom || bottom >= self.rows {
            return false;
        }
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        self.cur_row = 0;
        self.cur_col = 0;
        true
    }

    fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    fn reset_tab_stops(&mut self) {
        self.tab_stops = default_tab_stops(self.cols);
    }

    fn move_cursor_1_based(&mut self, row: usize, col: usize) {
        self.cur_row = row.saturating_sub(1).min(self.rows - 1);
        self.cur_col = col.saturating_sub(1).min(self.cols - 1);
    }

    fn move_cursor_origin_1_based(&mut self, row: usize, col: usize) {
        self.cur_row = (self.scroll_top + row.saturating_sub(1)).min(self.scroll_bottom);
        self.cur_col = col.saturating_sub(1).min(self.cols - 1);
    }

    fn move_cursor_row_origin_1_based(&mut self, row: usize) {
        self.cur_row = (self.scroll_top + row.saturating_sub(1)).min(self.scroll_bottom);
    }

    fn move_cursor_relative(&mut self, d_row: isize, d_col: isize) {
        let row = self.cur_row.saturating_add_signed(d_row).min(self.rows - 1);
        let col = self.cur_col.saturating_add_signed(d_col).min(self.cols - 1);
        self.cur_row = row;
        self.cur_col = col;
    }

    fn move_cursor_relative_origin(&mut self, d_row: isize, d_col: isize) {
        let row = self
            .cur_row
            .saturating_add_signed(d_row)
            .clamp(self.scroll_top, self.scroll_bottom);
        let col = self.cur_col.saturating_add_signed(d_col).min(self.cols - 1);
        self.cur_row = row;
        self.cur_col = col;
    }

    fn move_cursor_col_1_based(&mut self, col: usize) {
        self.cur_col = col.saturating_sub(1).min(self.cols - 1);
    }

    fn move_cursor_row_1_based(&mut self, row: usize) {
        self.cur_row = row.saturating_sub(1).min(self.rows - 1);
    }

    fn move_cursor_line_relative(&mut self, d_row: isize) {
        let row = self.cur_row.saturating_add_signed(d_row).min(self.rows - 1);
        self.cur_row = row;
        self.cur_col = 0;
    }

    fn move_cursor_line_relative_origin(&mut self, d_row: isize) {
        let row = self
            .cur_row
            .saturating_add_signed(d_row)
            .clamp(self.scroll_top, self.scroll_bottom);
        self.cur_row = row;
        self.cur_col = 0;
    }

    fn tab(&mut self) {
        self.tab_forward(1);
    }

    fn tab_forward(&mut self, count: usize) {
        let count = count.max(1);
        for _ in 0..count {
            self.cur_col = self.next_tab_stop();
        }
    }

    fn tab_backward(&mut self, count: usize) {
        let count = count.max(1);
        for _ in 0..count {
            self.cur_col = self.previous_tab_stop();
        }
    }

    fn next_tab_stop(&self) -> usize {
        let next = self
            .tab_stops
            .iter()
            .enumerate()
            .skip(self.cur_col.saturating_add(1))
            .find_map(|(col, stop)| stop.then_some(col))
            .unwrap_or(self.cols);
        next.min(self.cols)
    }

    fn previous_tab_stop(&self) -> usize {
        self.tab_stops
            .iter()
            .enumerate()
            .take(self.cur_col.min(self.cols))
            .rev()
            .find_map(|(col, stop)| stop.then_some(col))
            .unwrap_or(0)
    }

    fn set_tab_stop(&mut self) {
        if self.cur_col < self.cols {
            self.tab_stops[self.cur_col] = true;
        }
    }

    fn clear_tab_stop(&mut self) {
        if self.cur_col < self.cols {
            self.tab_stops[self.cur_col] = false;
        }
    }

    fn clear_all_tab_stops(&mut self) {
        self.tab_stops.fill(false);
    }

    fn screen_alignment_test(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell {
                ch: 'E',
                fg: self.cur_fg,
                bg: self.cur_bg,
                underline: false,
                strikethrough: false,
                italic: false,
                faint: false,
                overline: false,
                conceal: false,
                blink: false,
            };
        }
        self.cur_row = 0;
        self.cur_col = 0;
    }

    /// All visible cells as text rows joined by '\n' (test/debug helper).
    #[cfg(test)]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows {
            for c in 0..self.cols {
                out.push(self.cell(r, c).ch);
            }
            if r + 1 < self.rows {
                out.push('\n');
            }
        }
        out
    }

    /// Whether any row contains `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        for r in 0..self.rows {
            let row: String = (0..self.cols).map(|c| self.cell(r, c).ch).collect();
            if row.contains(needle) {
                return true;
            }
        }
        false
    }
}

/// Parser state machine for the shell-focused subset of VT/xterm control bytes
/// this terminal supports. Unknown CSI/OSC/string sequences are consumed without
/// touching the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Normal: bytes are decoded as UTF-8 and printed (or handled as controls).
    Ground,
    /// Saw `ESC`; waiting for the next byte to decide CSI / OSC / other.
    Escape,
    /// Saw `ESC` plus an intermediate byte like `(`, `)`, or `#`; waiting for
    /// the final byte to consume charset selects and similar two-byte escapes.
    EscapeIntermediate,
    /// Inside a CSI (`ESC [`); collecting parameter/intermediate bytes until a
    /// final byte (0x40..=0x7e).
    Csi,
    /// Inside an OSC (`ESC ]`); consuming until BEL (0x07) or ST (`ESC \`).
    Osc,
    /// Inside OSC and just saw an `ESC`; an immediate `\` (0x5c) terminates (ST).
    OscEsc,
    /// Inside a non-OSC escape string (DCS/PM/APC/SOS); consuming until ST.
    String,
    /// Inside a non-OSC string and just saw ESC; an immediate `\` terminates.
    StringEsc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCursor {
    row: usize,
    col: usize,
    fg: u32,
    bg: u32,
    g0_charset: Charset,
    g1_charset: Charset,
    active_charset: CharsetSlot,
    bold: bool,
    inverse: bool,
    underline: bool,
    strikethrough: bool,
    italic: bool,
    faint: bool,
    overline: bool,
    conceal: bool,
    blink: bool,
    autowrap: bool,
    origin_mode: bool,
    insert_mode: bool,
    newline_mode: bool,
    cursor_visible: bool,
    cursor_blinking: bool,
    cursor_shape: CursorShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    Ascii,
    DecSpecialGraphics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharsetSlot {
    G0,
    G1,
}

impl Charset {
    fn from_designator(b: u8) -> Self {
        match b {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        }
    }

    fn map_ascii(self, b: u8) -> char {
        if self != Charset::DecSpecialGraphics {
            return b as char;
        }
        match b {
            b'`' => '◆',
            b'a' => '▒',
            b'b' => '␉',
            b'c' => '␌',
            b'd' => '␍',
            b'e' => '␊',
            b'f' => '°',
            b'g' => '±',
            b'h' => '␤',
            b'i' => '␋',
            b'j' => '┘',
            b'k' => '┐',
            b'l' => '┌',
            b'm' => '└',
            b'n' => '┼',
            b'o' => '⎺',
            b'p' => '⎻',
            b'q' => '─',
            b'r' => '⎼',
            b's' => '⎽',
            b't' => '├',
            b'u' => '┤',
            b'v' => '┴',
            b'w' => '┬',
            b'x' => '│',
            b'y' => '≤',
            b'z' => '≥',
            b'{' => 'π',
            b'|' => '≠',
            b'}' => '£',
            b'~' => '·',
            _ => b as char,
        }
    }
}

/// A minimal VT/ANSI parser that drives a [`Grid`].
#[derive(Debug)]
pub struct VtParser {
    state: State,
    /// Accumulated CSI parameter/intermediate bytes (between `ESC [` and final).
    csi: Vec<u8>,
    /// Accumulated OSC payload bytes (between `ESC ]` and BEL/ST), capped.
    osc: Vec<u8>,
    /// ESC intermediate byte for non-CSI escape sequences such as `ESC ( B`.
    esc_intermediate: u8,
    /// Partial UTF-8 sequence being decoded in Ground state.
    utf8: Vec<u8>,
    /// How many continuation bytes remain for the in-progress UTF-8 char.
    utf8_need: usize,
    /// Bytes the parser wants written BACK to the PTY (e.g. a Device Status
    /// Report reply to `ESC [ 6 n`). ConPTY blocks further output until the DSR
    /// it emits at startup is answered, so the terminal must drain + send these.
    reply: Vec<u8>,
    /// Saved cursor state used by DEC `ESC 7`/`ESC 8` and CSI `s`/`u`.
    saved_cursor: Option<SavedCursor>,
    /// Cursor state saved by `CSI ?1049 h`; kept separate from app-visible
    /// cursor save/restore sequences so alternate-screen entry cannot clobber them.
    alternate_cursor: Option<SavedCursor>,
    /// G0/G1 charset designations used by legacy TUIs for box drawing.
    g0_charset: Charset,
    g1_charset: Charset,
    /// Charset currently mapped into GL by SI/SO. Defaults to G0.
    active_charset: CharsetSlot,
    /// Whether SGR bold/intense is active for subsequently-written cells.
    bold: bool,
    /// Whether SGR reverse-video swaps foreground/background for new cells.
    inverse: bool,
    /// Whether SGR underline is active for subsequently-written cells.
    underline: bool,
    /// Whether SGR strikethrough is active for subsequently-written cells.
    strikethrough: bool,
    /// Whether SGR italic is active for subsequently-written cells.
    italic: bool,
    /// Whether SGR faint/dim intensity is active for subsequently-written cells.
    faint: bool,
    /// Whether SGR overline is active for subsequently-written cells.
    overline: bool,
    /// Whether SGR conceal/hidden text is active for subsequently-written cells.
    conceal: bool,
    /// Whether SGR blink is active for subsequently-written cells.
    blink: bool,
    /// Last graphic cell written by printable output, used by REP (`CSI Ps b`).
    last_graphic: Option<Cell>,
    /// Whether the running app asked for bracketed paste (`CSI ?2004 h`).
    bracketed_paste: bool,
    /// Whether the running app asked for focus in/out reports (`CSI ?1004 h`).
    focus_reporting: bool,
    /// Whether the terminal cursor should be drawn (`CSI ?25 h/l`).
    cursor_visible: bool,
    /// Whether the running app requested cursor blinking (`CSI ?12 h/l`).
    cursor_blinking: bool,
    /// Shape requested by DECSCUSR (`CSI Ps SP q`).
    cursor_shape: CursorShape,
    /// Whether arrow keys should use application cursor-key sequences.
    application_cursor_keys: bool,
    /// Enabled xterm mouse event modes (`?1000`, `?1002`, `?1003`).
    mouse_modes: u8,
    /// Whether mouse reports should use SGR extended coordinates (`CSI ?1006 h`).
    sgr_mouse: bool,
    /// Whether printable output inserts at the cursor before writing (`CSI 4 h/l`).
    insert_mode: bool,
    /// Whether LF also returns to column 0 (`CSI 20 h/l`).
    newline_mode: bool,
    /// Whether printable output should wrap after the right margin (`CSI ?7 h/l`).
    autowrap: bool,
    /// Whether CUP/HVP row coordinates are relative to the scroll-region top (`CSI ?6 h/l`).
    origin_mode: bool,
    /// Default foreground color reported by OSC 10 color queries.
    default_fg_rgb: (u8, u8, u8),
    /// Default background color reported by OSC 11 color queries.
    default_bg_rgb: (u8, u8, u8),
    /// Cursor color reported by OSC 12 color queries.
    cursor_rgb: (u8, u8, u8),
    /// Mutable palette entries reported by OSC 4 color queries.
    palette_rgb: [(u8, u8, u8); 256],
    /// Last window/icon title reported by OSC 0/1/2.
    title: String,
    /// Last decoded OSC 52 clipboard write request, drained by the UI bridge.
    clipboard_write: Option<String>,
}

impl Default for VtParser {
    fn default() -> Self {
        VtParser::new()
    }
}

impl VtParser {
    pub fn new() -> Self {
        VtParser {
            state: State::Ground,
            csi: Vec::new(),
            osc: Vec::new(),
            esc_intermediate: 0,
            utf8: Vec::new(),
            utf8_need: 0,
            reply: Vec::new(),
            saved_cursor: None,
            alternate_cursor: None,
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: CharsetSlot::G0,
            bold: false,
            inverse: false,
            underline: false,
            strikethrough: false,
            italic: false,
            faint: false,
            overline: false,
            conceal: false,
            blink: false,
            last_graphic: None,
            bracketed_paste: false,
            focus_reporting: false,
            cursor_visible: true,
            cursor_blinking: false,
            cursor_shape: CursorShape::Block,
            application_cursor_keys: false,
            mouse_modes: 0,
            sgr_mouse: false,
            insert_mode: false,
            newline_mode: true,
            autowrap: true,
            origin_mode: false,
            default_fg_rgb: DEFAULT_FG_RGB,
            default_bg_rgb: DEFAULT_BG_RGB,
            cursor_rgb: DEFAULT_CURSOR_RGB,
            palette_rgb: std::array::from_fn(|index| palette_rgb8(index as u32)),
            title: String::new(),
            clipboard_write: None,
        }
    }

    /// Feed a slice of PTY output bytes, mutating `grid`.
    pub fn feed(&mut self, grid: &mut Grid, bytes: &[u8]) {
        for &b in bytes {
            self.feed_byte(grid, b);
        }
    }

    /// Take any pending reply bytes the parser wants written back to the PTY
    /// (DSR responses). Empties the internal buffer.
    pub fn take_reply(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply)
    }

    pub fn take_clipboard_write(&mut self) -> Option<String> {
        self.clipboard_write.take()
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    pub fn focus_reporting_enabled(&self) -> bool {
        self.focus_reporting
    }

    pub fn sgr_mouse_enabled(&self) -> bool {
        self.mouse_reporting_enabled() && self.sgr_mouse
    }

    pub fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_modes != 0
    }

    pub fn mouse_drag_reporting_enabled(&self) -> bool {
        self.mouse_modes & (MOUSE_MODE_DRAG | MOUSE_MODE_ANY) != 0
    }

    pub fn mouse_any_reporting_enabled(&self) -> bool {
        self.mouse_modes & MOUSE_MODE_ANY != 0
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn cursor_blinking(&self) -> bool {
        self.cursor_blinking
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    fn foreground_rgba(&self, fg: u32) -> (f32, f32, f32, f32) {
        if fg == DEFAULT_FG {
            return rgb8_rgba(self.default_fg_rgb, 1.0);
        }
        if fg == DEFAULT_BG {
            return rgb8_rgba(self.default_bg_rgb, 1.0);
        }

        if fg & TRUECOLOR_MASK != 0 {
            let r = ((fg >> 16) & 0xff) as u8;
            let g = ((fg >> 8) & 0xff) as u8;
            let b = (fg & 0xff) as u8;
            return rgb8_rgba((r, g, b), 1.0);
        }

        if fg <= 255 {
            return rgb8_rgba(self.palette_rgb[fg as usize], 1.0);
        }

        rgb8_rgba(self.default_fg_rgb, 1.0)
    }

    fn background_rgba(&self, bg: u32) -> Option<(f32, f32, f32, f32)> {
        if bg == DEFAULT_BG {
            return if self.default_bg_rgb == DEFAULT_BG_RGB {
                None
            } else {
                Some(rgb8_rgba(self.default_bg_rgb, 0.72))
            };
        }

        let (r, g, b, _) = self.foreground_rgba(bg);
        Some((r, g, b, 0.72))
    }

    fn cursor_rgba(&self) -> (f32, f32, f32, f32) {
        rgb8_rgba(self.cursor_rgb, 0.6)
    }

    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    fn feed_byte(&mut self, grid: &mut Grid, b: u8) {
        match self.state {
            State::Ground => self.ground(grid, b),
            State::Escape => self.escape(grid, b),
            State::EscapeIntermediate => self.escape_intermediate(grid, b),
            State::Csi => self.csi(grid, b),
            State::Osc => self.osc(b),
            State::OscEsc => self.osc_esc(b),
            State::String => self.string(b),
            State::StringEsc => self.string_esc(b),
        }
    }

    /// Ground state: handle control bytes, decode UTF-8, print printables.
    fn ground(&mut self, grid: &mut Grid, b: u8) {
        // A continuation byte expected for a multi-byte UTF-8 char?
        if self.utf8_need > 0 {
            if b & 0xc0 == 0x80 {
                self.utf8.push(b);
                self.utf8_need -= 1;
                if self.utf8_need == 0 {
                    self.flush_utf8(grid);
                }
                return;
            }
            // Malformed sequence: drop what we had and reprocess `b` fresh.
            self.utf8.clear();
            self.utf8_need = 0;
        }

        match b {
            0x1b => self.state = State::Escape, // ESC
            0x84 => grid.index(),               // IND
            0x85 => grid.newline(),             // NEL
            0x88 => grid.set_tab_stop(),        // HTS
            0x8d => grid.reverse_index(),       // RI
            0x90 | 0x98 | 0x9e | 0x9f => self.state = State::String, // DCS/SOS/PM/APC
            0x9b => {
                self.csi.clear();
                self.state = State::Csi;
            }
            0x9d => self.enter_osc(),
            b'\n' | 0x0b | 0x0c => self.linefeed(grid),
            b'\r' => grid.carriage_return(),
            0x08 => grid.backspace(), // BS
            b'\t' => grid.tab(),
            0x0e => self.active_charset = CharsetSlot::G1, // SO / LS1
            0x0f => self.active_charset = CharsetSlot::G0, // SI / LS0
            0x07 => {} // BEL: ignore
            0x7f => {} // DEL: ignored on the display side
            0x00..=0x06 | 0x10..=0x1a | 0x1c..=0x1f => {} // other C0: ignore
            0x20..=0x7e => self.print_ascii_byte(grid, b), // printable ASCII
            0xc0..=0xdf => {
                self.utf8.clear();
                self.utf8.push(b);
                self.utf8_need = 1;
            }
            0xe0..=0xef => {
                self.utf8.clear();
                self.utf8.push(b);
                self.utf8_need = 2;
            }
            0xf0..=0xf7 => {
                self.utf8.clear();
                self.utf8.push(b);
                self.utf8_need = 3;
            }
            // Stray continuation / invalid lead byte: ignore.
            _ => {}
        }
    }

    fn flush_utf8(&mut self, grid: &mut Grid) {
        match std::str::from_utf8(&self.utf8) {
            Ok(s) => {
                let chars: Vec<char> = s.chars().collect();
                for ch in chars {
                    self.print_char(grid, ch);
                }
            }
            Err(_) => self.print_char(grid, '\u{fffd}'), // replacement char
        }
        self.utf8.clear();
    }

    fn print_char(&mut self, grid: &mut Grid, ch: char) {
        let (fg, bg) = effective_sgr_cell_colors(grid.cur_fg, grid.cur_bg, self.bold, self.inverse);
        let cell = Cell {
            ch,
            fg,
            bg,
            underline: self.underline,
            strikethrough: self.strikethrough,
            italic: self.italic,
            faint: self.faint,
            overline: self.overline,
            conceal: self.conceal,
            blink: self.blink,
        };
        if self.insert_mode {
            grid.prepare_insert(self.autowrap);
        }
        grid.put_cell_autowrap(cell, self.autowrap);
        self.last_graphic = Some(cell);
    }

    fn print_ascii_byte(&mut self, grid: &mut Grid, b: u8) {
        let charset = match self.active_charset {
            CharsetSlot::G0 => self.g0_charset,
            CharsetSlot::G1 => self.g1_charset,
        };
        self.print_char(grid, charset.map_ascii(b));
    }

    fn linefeed(&self, grid: &mut Grid) {
        if self.newline_mode {
            grid.newline();
        } else {
            grid.index();
        }
    }

    /// Just saw ESC: decide CSI / OSC / single-char escape.
    fn escape(&mut self, grid: &mut Grid, b: u8) {
        match b {
            b'[' => {
                self.csi.clear();
                self.state = State::Csi;
            }
            b']' => self.enter_osc(),
            b'P' | b'X' | b'^' | b'_' => self.state = State::String,
            0x20..=0x2f => {
                self.esc_intermediate = b;
                self.state = State::EscapeIntermediate;
            }
            // `ESC c` full reset restores modes, grid, and terminal identity.
            b'c' => {
                self.full_reset(grid);
                self.state = State::Ground;
            }
            b'H' => {
                grid.set_tab_stop();
                self.state = State::Ground;
            }
            b'D' => {
                grid.index();
                self.state = State::Ground;
            }
            b'E' => {
                grid.newline();
                self.state = State::Ground;
            }
            b'M' => {
                grid.reverse_index();
                self.state = State::Ground;
            }
            b'7' => {
                self.save_cursor(grid);
                self.state = State::Ground;
            }
            b'8' => {
                self.restore_cursor(grid);
                self.state = State::Ground;
            }
            // Other two-byte escapes (e.g. `ESC =`, `ESC >`): consume the
            // single byte and return to ground.
            _ => self.state = State::Ground,
        }
    }

    fn escape_intermediate(&mut self, grid: &mut Grid, b: u8) {
        match b {
            0x18 | 0x1a => self.state = State::Ground,
            0x1b => self.state = State::Escape,
            0x20..=0x2f => {
                self.esc_intermediate = b;
            }
            0x30..=0x7e => {
                if self.esc_intermediate == b'#' && b == b'8' {
                    grid.screen_alignment_test();
                } else if self.esc_intermediate == b'(' || self.esc_intermediate == b')' {
                    self.designate_charset(self.esc_intermediate, b);
                }
                self.esc_intermediate = 0;
                self.state = State::Ground;
            }
            _ => {
                self.esc_intermediate = 0;
                self.state = State::Ground;
            }
        }
    }

    fn designate_charset(&mut self, slot: u8, designator: u8) {
        let charset = Charset::from_designator(designator);
        match slot {
            b'(' => self.g0_charset = charset,
            b')' => self.g1_charset = charset,
            _ => {}
        }
    }

    /// Inside a CSI: accumulate until a final byte (0x40..=0x7e). Handles the
    /// core shell sequences we need (SGR, DSR, erase display/line, cursor
    /// movement); others are
    /// consumed harmlessly.
    fn csi(&mut self, grid: &mut Grid, b: u8) {
        match b {
            0x18 | 0x1a => {
                self.csi.clear();
                self.state = State::Ground;
            }
            0x1b => {
                self.csi.clear();
                self.state = State::Escape;
            }
            0x90 | 0x98 | 0x9e | 0x9f => {
                self.csi.clear();
                self.state = State::String;
            }
            0x9b => {
                self.csi.clear();
                self.state = State::Csi;
            }
            0x9d => {
                self.csi.clear();
                self.enter_osc();
            }
            0x08 => grid.backspace(),
            b'\t' => grid.tab(),
            b'\n' | 0x0b | 0x0c => self.linefeed(grid),
            b'\r' => grid.carriage_return(),
            0x0e => self.active_charset = CharsetSlot::G1,
            0x0f => self.active_charset = CharsetSlot::G0,
            0x07 | 0x7f => {},
            0x00..=0x06 | 0x10..=0x17 | 0x19 | 0x1c..=0x1f => {}
            // Parameter bytes (0x30..=0x3f) and intermediates (0x20..=0x2f).
            0x20..=0x3f => self.csi.push(b),
            // Final byte: dispatch and return to ground.
            0x40..=0x7e => {
                if b == b'm' {
                    self.apply_sgr(grid);
                } else if b == b'n' {
                    // Device Status Report. ConPTY emits `ESC[6n` at startup and
                    // blocks until answered, so we must reply.
                    self.handle_dsr(grid);
                } else if b == b'c' {
                    self.handle_device_attributes();
                } else if b == b'@' {
                    self.insert_chars(grid);
                } else if b == b'J' {
                    self.erase_display(grid);
                } else if b == b'I' {
                    self.cursor_forward_tab(grid);
                } else if b == b'K' {
                    self.erase_line(grid);
                } else if b == b'L' {
                    self.insert_lines(grid);
                } else if b == b'M' {
                    self.delete_lines(grid);
                } else if b == b'P' {
                    self.delete_chars(grid);
                } else if b == b'S' {
                    self.scroll_up(grid);
                } else if b == b'T' || b == b'^' {
                    self.scroll_down(grid);
                } else if b == b'X' {
                    self.erase_chars(grid);
                } else if b == b'Z' {
                    self.cursor_backward_tab(grid);
                } else if b == b'b' {
                    self.repeat_previous_graphic(grid);
                } else if b == b'g' {
                    self.clear_tab_stop(grid);
                } else if b == b'W' {
                    self.cursor_tab_control(grid);
                } else if b == b'h' || b == b'l' {
                    self.set_mode(grid, b);
                } else if b == b'r' {
                    self.set_scroll_region(grid);
                } else if b == b'q' {
                    self.set_cursor_shape();
                } else if b == b'H' || b == b'f' {
                    self.cursor_position(grid);
                } else if matches!(b, b'A' | b'B' | b'C' | b'D' | b'a' | b'j' | b'k') {
                    self.cursor_relative(grid, b);
                } else if b == b'G' {
                    self.cursor_column(grid);
                } else if b == b'`' {
                    self.cursor_column(grid);
                } else if b == b'd' {
                    self.cursor_row(grid);
                } else if b == b'e' {
                    self.cursor_row_relative(grid);
                } else if b == b'E' || b == b'F' {
                    self.cursor_line_relative(grid, b);
                } else if b == b's' {
                    self.save_cursor(grid);
                } else if b == b'u' {
                    self.restore_cursor(grid);
                } else if b == b'p' {
                    if self.is_soft_reset() {
                        self.soft_reset(grid);
                    } else {
                        self.handle_mode_status_query(grid);
                    }
                } else if b == b't' {
                    self.handle_window_ops(grid);
                }
                // All other finals are intentionally skipped.
                self.csi.clear();
                self.state = State::Ground;
            }
            // Anything unexpected: bail back to ground without corrupting output.
            _ => {
                self.csi.clear();
                self.state = State::Ground;
            }
        }
    }

    /// Apply an `ESC [ … m` SGR sequence: parse numeric params and update the
    /// grid's current colors. Handles reset (0), the basic/bright ANSI colors,
    /// xterm 256-color `38;5;n` / `48;5;n`, truecolor `38;2;r;g;b` /
    /// `48;2;r;g;b`, colon forms like `38:2::r:g:b`, and default fg/bg (39/49).
    /// Unknown params are ignored.
    fn apply_sgr(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        // A bare `ESC [ m` means reset.
        if params.is_empty() {
            grid.cur_fg = DEFAULT_FG;
            grid.cur_bg = DEFAULT_BG;
            self.bold = false;
            self.inverse = false;
            self.underline = false;
            self.strikethrough = false;
            self.italic = false;
            self.faint = false;
            self.overline = false;
            self.conceal = false;
            self.blink = false;
            return;
        }
        // `ESC [ ? … m` (private) — not a real SGR; ignore.
        if params.starts_with('?') {
            return;
        }
        let params = parse_sgr_params(params);

        let mut i = 0usize;
        while i < params.len() {
            let Some(n) = params[i] else {
                i += 1;
                continue;
            };
            match n {
                0 => {
                    grid.cur_fg = DEFAULT_FG;
                    grid.cur_bg = DEFAULT_BG;
                    self.bold = false;
                    self.inverse = false;
                    self.underline = false;
                    self.strikethrough = false;
                    self.italic = false;
                    self.faint = false;
                    self.overline = false;
                    self.conceal = false;
                    self.blink = false;
                }
                1 => self.bold = true,
                2 => self.faint = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 | 6 => self.blink = true,
                7 => self.inverse = true,
                8 => self.conceal = true,
                9 => self.strikethrough = true,
                22 => {
                    self.bold = false;
                    self.faint = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.conceal = false,
                29 => self.strikethrough = false,
                30..=37 => grid.cur_fg = (n - 30) as u32, // basic 0..=7
                39 => grid.cur_fg = DEFAULT_FG,     // default fg
                40..=47 => grid.cur_bg = (n - 40) as u32, // basic bg 0..=7
                49 => grid.cur_bg = DEFAULT_BG,      // default bg
                53 => self.overline = true,
                55 => self.overline = false,
                90..=97 => grid.cur_fg = (n - 90 + 8) as u32, // bright 8..=15
                100..=107 => grid.cur_bg = (n - 100 + 8) as u32, // bright bg
                38 | 48 => {
                    let is_fg = n == 38;
                    match params.get(i + 1).and_then(|value| *value) {
                        Some(5) => {
                            if let Some(idx @ 0..=255) =
                                params.get(i + 2).and_then(|value| *value)
                            {
                                if is_fg {
                                    grid.cur_fg = idx as u32;
                                } else {
                                    grid.cur_bg = idx as u32;
                                }
                            }
                            i += 2;
                        }
                        Some(2) => {
                            if let (Some(r @ 0..=255), Some(g @ 0..=255), Some(b @ 0..=255)) = (
                                params.get(i + 2).and_then(|value| *value),
                                params.get(i + 3).and_then(|value| *value),
                                params.get(i + 4).and_then(|value| *value),
                            ) {
                                let color = encode_truecolor(r as u8, g as u8, b as u8);
                                if is_fg {
                                    grid.cur_fg = color;
                                } else {
                                    grid.cur_bg = color;
                                }
                            }
                            i += 4;
                        }
                        _ => {}
                    }
                }
                _ => {}                              // ignore everything else
            }
            i += 1;
        }
    }

    fn first_count_param(&self) -> Option<usize> {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return None;
        }
        Some(
            params
                .split(';')
                .next()
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1),
        )
    }

    fn insert_chars(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.insert_blank_chars(count);
        }
    }

    fn delete_chars(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.delete_chars(count);
        }
    }

    fn erase_chars(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.erase_chars(count);
        }
    }

    fn cursor_forward_tab(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.tab_forward(count);
        }
    }

    fn cursor_backward_tab(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.tab_backward(count);
        }
    }

    fn insert_lines(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.insert_blank_lines(count);
        }
    }

    fn delete_lines(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.delete_lines(count);
        }
    }

    fn scroll_up(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.scroll_up(count);
        }
    }

    fn scroll_down(&mut self, grid: &mut Grid) {
        if let Some(count) = self.first_count_param() {
            grid.scroll_down(count);
        }
    }

    fn set_scroll_region(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let mut parts = params.split(';');
        let top = parts
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        let bottom = parts
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(grid.rows());
        if grid.set_scroll_region(top.saturating_sub(1), bottom.saturating_sub(1)) {
            if self.origin_mode {
                grid.move_cursor_origin_1_based(1, 1);
            } else {
                grid.move_cursor_1_based(1, 1);
            }
        }
    }

    fn clear_tab_stop(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let mode = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        match mode {
            0 => grid.clear_tab_stop(),
            3 => grid.clear_all_tab_stops(),
            _ => {}
        }
    }

    fn cursor_tab_control(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let mode = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        match mode {
            0 => grid.set_tab_stop(),
            2 => grid.clear_tab_stop(),
            5 => grid.clear_all_tab_stops(),
            _ => {}
        }
    }

    fn set_mode(&mut self, grid: &mut Grid, final_byte: u8) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("").to_string();
        if !params.starts_with('?') {
            let modes: Vec<&str> = params.split(';').collect();
            for mode in modes {
                match (mode, final_byte) {
                    ("4", b'h') => self.insert_mode = true,
                    ("4", b'l') => self.insert_mode = false,
                    ("20", b'h') => self.newline_mode = true,
                    ("20", b'l') => self.newline_mode = false,
                    _ => {}
                }
            }
            return;
        }
        let Some(private) = params.strip_prefix('?') else {
            return;
        };

        let modes: Vec<&str> = private.split(';').collect();
        for mode in modes {
            match (mode, final_byte) {
                ("1049", b'h') => self.enter_alternate_screen_with_cursor_save(grid),
                ("1049", b'l') => self.exit_alternate_screen_with_cursor_restore(grid),
                ("47" | "1047", b'h') => grid.enter_alternate_screen(),
                ("47" | "1047", b'l') => grid.exit_alternate_screen(),
                ("1", b'h') => self.application_cursor_keys = true,
                ("1", b'l') => self.application_cursor_keys = false,
                ("6", b'h') => {
                    self.origin_mode = true;
                    grid.move_cursor_origin_1_based(1, 1);
                }
                ("6", b'l') => {
                    self.origin_mode = false;
                    grid.move_cursor_1_based(1, 1);
                }
                ("7", b'h') => self.autowrap = true,
                ("7", b'l') => self.autowrap = false,
                ("1048", b'h') => self.save_cursor(grid),
                ("1048", b'l') => self.restore_cursor(grid),
                ("12", b'h') => self.cursor_blinking = true,
                ("12", b'l') => self.cursor_blinking = false,
                ("25", b'h') => self.cursor_visible = true,
                ("25", b'l') => self.cursor_visible = false,
                ("1004", b'h') => self.focus_reporting = true,
                ("1004", b'l') => self.focus_reporting = false,
                ("2004", b'h') => self.bracketed_paste = true,
                ("2004", b'l') => self.bracketed_paste = false,
                ("1000", b'h') => self.mouse_modes |= MOUSE_MODE_BUTTON,
                ("1000", b'l') => self.mouse_modes &= !MOUSE_MODE_BUTTON,
                ("1002", b'h') => self.mouse_modes |= MOUSE_MODE_DRAG,
                ("1002", b'l') => self.mouse_modes &= !MOUSE_MODE_DRAG,
                ("1003", b'h') => self.mouse_modes |= MOUSE_MODE_ANY,
                ("1003", b'l') => self.mouse_modes &= !MOUSE_MODE_ANY,
                ("1006", b'h') => self.sgr_mouse = true,
                ("1006", b'l') => self.sgr_mouse = false,
                _ => {}
            }
        }
    }

    fn reset_modes(&mut self) {
        self.saved_cursor = None;
        self.alternate_cursor = None;
        self.last_graphic = None;
        self.g0_charset = Charset::Ascii;
        self.g1_charset = Charset::Ascii;
        self.active_charset = CharsetSlot::G0;
        self.bold = false;
        self.inverse = false;
        self.underline = false;
        self.strikethrough = false;
        self.italic = false;
        self.faint = false;
        self.overline = false;
        self.conceal = false;
        self.blink = false;
        self.bracketed_paste = false;
        self.focus_reporting = false;
        self.cursor_visible = true;
        self.cursor_blinking = false;
        self.cursor_shape = CursorShape::Block;
        self.application_cursor_keys = false;
        self.mouse_modes = 0;
        self.sgr_mouse = false;
        self.insert_mode = false;
        self.newline_mode = true;
        self.autowrap = true;
        self.origin_mode = false;
    }

    fn reset_color_state(&mut self) {
        self.default_fg_rgb = DEFAULT_FG_RGB;
        self.default_bg_rgb = DEFAULT_BG_RGB;
        self.cursor_rgb = DEFAULT_CURSOR_RGB;
        self.palette_rgb = std::array::from_fn(|index| palette_rgb8(index as u32));
    }

    fn full_reset(&mut self, grid: &mut Grid) {
        grid.clear();
        grid.reset_tab_stops();
        grid.cur_fg = DEFAULT_FG;
        grid.cur_bg = DEFAULT_BG;
        self.reset_modes();
        self.reset_color_state();
        self.title.clear();
        self.osc.clear();
        self.csi.clear();
        self.utf8.clear();
        self.utf8_need = 0;
    }

    fn is_soft_reset(&self) -> bool {
        self.csi.as_slice() == b"!"
    }

    fn soft_reset(&mut self, grid: &mut Grid) {
        self.reset_modes();
        grid.cur_fg = DEFAULT_FG;
        grid.cur_bg = DEFAULT_BG;
        grid.reset_scroll_region();
        grid.move_cursor_1_based(1, 1);
    }

    fn set_cursor_shape(&mut self) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        let Some(params) = params.strip_suffix(' ') else {
            return;
        };
        if params.starts_with('?') {
            return;
        }
        let shape = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        match shape {
            0 => self.cursor_shape = CursorShape::Block,
            1 => {
                self.cursor_shape = CursorShape::Block;
                self.cursor_blinking = true;
            }
            2 => {
                self.cursor_shape = CursorShape::Block;
                self.cursor_blinking = false;
            }
            3 => {
                self.cursor_shape = CursorShape::Underline;
                self.cursor_blinking = true;
            }
            4 => {
                self.cursor_shape = CursorShape::Underline;
                self.cursor_blinking = false;
            }
            5 => {
                self.cursor_shape = CursorShape::Bar;
                self.cursor_blinking = true;
            }
            6 => {
                self.cursor_shape = CursorShape::Bar;
                self.cursor_blinking = false;
            }
            _ => {}
        }
    }

    fn erase_display(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let mode = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        match mode {
            0 => grid.clear_from_cursor_to_end(),
            1 => grid.clear_from_start_to_cursor(),
            2 => grid.clear(),
            3 => {}
            _ => {}
        }
    }

    fn erase_line(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let mode = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        match mode {
            0 => grid.clear_line_from_cursor_to_end(),
            1 => grid.clear_line_from_start_to_cursor(),
            2 => grid.clear_line(),
            _ => {}
        }
    }

    fn cursor_position(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let mut parts = params.split(';');
        let row = parts
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        let col = parts
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        if self.origin_mode {
            grid.move_cursor_origin_1_based(row, col);
        } else {
            grid.move_cursor_1_based(row, col);
        }
    }

    fn cursor_relative(&mut self, grid: &mut Grid, final_byte: u8) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let amount = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1) as isize;
        match final_byte {
            b'A' | b'k' if self.origin_mode => grid.move_cursor_relative_origin(-amount, 0),
            b'A' | b'k' => grid.move_cursor_relative(-amount, 0),
            b'B' if self.origin_mode => grid.move_cursor_relative_origin(amount, 0),
            b'B' => grid.move_cursor_relative(amount, 0),
            b'C' | b'a' => grid.move_cursor_relative(0, amount),
            b'D' | b'j' => grid.move_cursor_relative(0, -amount),
            _ => {}
        }
    }

    fn repeat_previous_graphic(&mut self, grid: &mut Grid) {
        let Some(cell) = self.last_graphic else {
            return;
        };
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let count = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        for _ in 0..count {
            if self.insert_mode {
                grid.prepare_insert(self.autowrap);
            }
            grid.put_cell_autowrap(cell, self.autowrap);
        }
    }

    fn cursor_column(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let col = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        grid.move_cursor_col_1_based(col);
    }

    fn cursor_row(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let row = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        if self.origin_mode {
            grid.move_cursor_row_origin_1_based(row);
        } else {
            grid.move_cursor_row_1_based(row);
        }
    }

    fn cursor_row_relative(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let amount = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1) as isize;
        if self.origin_mode {
            grid.move_cursor_relative_origin(amount, 0);
        } else {
            grid.move_cursor_relative(amount, 0);
        }
    }

    fn cursor_line_relative(&mut self, grid: &mut Grid, final_byte: u8) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        if params.starts_with('?') {
            return;
        }
        let amount = params
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1) as isize;
        match final_byte {
            b'E' if self.origin_mode => grid.move_cursor_line_relative_origin(amount),
            b'E' => grid.move_cursor_line_relative(amount),
            b'F' if self.origin_mode => grid.move_cursor_line_relative_origin(-amount),
            b'F' => grid.move_cursor_line_relative(-amount),
            _ => {}
        }
    }

    fn cursor_snapshot(&self, grid: &Grid) -> SavedCursor {
        let (row, col) = grid.raw_cursor();
        SavedCursor {
            row,
            col,
            fg: grid.cur_fg,
            bg: grid.cur_bg,
            g0_charset: self.g0_charset,
            g1_charset: self.g1_charset,
            active_charset: self.active_charset,
            bold: self.bold,
            inverse: self.inverse,
            underline: self.underline,
            strikethrough: self.strikethrough,
            italic: self.italic,
            faint: self.faint,
            overline: self.overline,
            conceal: self.conceal,
            blink: self.blink,
            autowrap: self.autowrap,
            origin_mode: self.origin_mode,
            insert_mode: self.insert_mode,
            newline_mode: self.newline_mode,
            cursor_visible: self.cursor_visible,
            cursor_blinking: self.cursor_blinking,
            cursor_shape: self.cursor_shape,
        }
    }

    fn restore_cursor_snapshot(&mut self, grid: &mut Grid, saved: SavedCursor) {
        grid.cur_row = saved.row.min(grid.rows - 1);
        grid.cur_col = saved.col.min(grid.cols);
        grid.cur_fg = saved.fg;
        grid.cur_bg = saved.bg;
        self.g0_charset = saved.g0_charset;
        self.g1_charset = saved.g1_charset;
        self.active_charset = saved.active_charset;
        self.bold = saved.bold;
        self.inverse = saved.inverse;
        self.underline = saved.underline;
        self.strikethrough = saved.strikethrough;
        self.italic = saved.italic;
        self.faint = saved.faint;
        self.overline = saved.overline;
        self.conceal = saved.conceal;
        self.blink = saved.blink;
        self.autowrap = saved.autowrap;
        self.origin_mode = saved.origin_mode;
        self.insert_mode = saved.insert_mode;
        self.newline_mode = saved.newline_mode;
        self.cursor_visible = saved.cursor_visible;
        self.cursor_blinking = saved.cursor_blinking;
        self.cursor_shape = saved.cursor_shape;
    }

    fn enter_alternate_screen_with_cursor_save(&mut self, grid: &mut Grid) {
        if !grid.alternate_screen_active() {
            self.alternate_cursor = Some(self.cursor_snapshot(grid));
        }
        grid.enter_alternate_screen();
    }

    fn exit_alternate_screen_with_cursor_restore(&mut self, grid: &mut Grid) {
        grid.exit_alternate_screen();
        if let Some(saved) = self.alternate_cursor.take() {
            self.restore_cursor_snapshot(grid, saved);
        }
    }

    fn save_cursor(&mut self, grid: &Grid) {
        self.saved_cursor = Some(self.cursor_snapshot(grid));
    }

    fn restore_cursor(&mut self, grid: &mut Grid) {
        if let Some(saved) = self.saved_cursor {
            self.restore_cursor_snapshot(grid, saved);
        }
    }

    /// Answer a Device Status Report (`ESC [ Ps n`). `5n` -> "OK" (`ESC[0n`);
    /// `6n` -> cursor position report `ESC[<row>;<col>R`; private `?6n` ->
    /// DEC cursor position report `ESC[?<row>;<col>R` (all 1-based). Anything
    /// else is ignored. The reply is queued for the terminal to write back.
    fn handle_dsr(&mut self, grid: &Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        match params {
            "5" => self.reply.extend_from_slice(b"\x1b[0n"),
            "6" => {
                let (r, c) = grid.cursor();
                let report = format!("\x1b[{};{}R", r + 1, c + 1);
                self.reply.extend_from_slice(report.as_bytes());
            }
            "?6" => {
                let (r, c) = grid.cursor();
                let report = format!("\x1b[?{};{}R", r + 1, c + 1);
                self.reply.extend_from_slice(report.as_bytes());
            }
            _ => {}
        }
    }

    /// Answer Device Attributes queries (`ESC [ c` / `ESC [ > c`) with minimal
    /// VT-compatible identity replies so probing terminal apps do not wait.
    fn handle_device_attributes(&mut self) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        match params {
            "" | "0" => self.reply.extend_from_slice(b"\x1b[?1;2c"),
            ">" | ">0" => self.reply.extend_from_slice(b"\x1b[>0;0;0c"),
            _ => {}
        }
    }

    /// Answer ANSI/DEC mode status queries (`CSI Ps $ p` / `CSI ? Ps $ p`).
    /// The reply uses xterm's `CSI Ps ; Pm $ y` form, preserving the private
    /// `?` marker when present. `1` means set, `2` reset, and `0` not recognized.
    fn handle_mode_status_query(&mut self, grid: &Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        let Some(query) = params.strip_suffix('$') else {
            return;
        };
        let (private, mode) = if let Some(mode) = query.strip_prefix('?') {
            (true, mode)
        } else {
            (false, query)
        };
        if mode.is_empty() || mode.contains(';') {
            return;
        }

        let status = if private {
            match mode {
                "1" => Some(self.application_cursor_keys),
                "6" => Some(self.origin_mode),
                "7" => Some(self.autowrap),
                "12" => Some(self.cursor_blinking),
                "25" => Some(self.cursor_visible),
                "47" | "1047" | "1049" => Some(grid.alternate_screen_active()),
                "1000" => Some(self.mouse_modes & MOUSE_MODE_BUTTON != 0),
                "1002" => Some(self.mouse_modes & MOUSE_MODE_DRAG != 0),
                "1003" => Some(self.mouse_modes & MOUSE_MODE_ANY != 0),
                "1004" => Some(self.focus_reporting),
                "1006" => Some(self.sgr_mouse),
                "2004" => Some(self.bracketed_paste),
                _ => None,
            }
        } else {
            match mode {
                "4" => Some(self.insert_mode),
                "20" => Some(self.newline_mode),
                _ => None,
            }
        };
        let status_code = match status {
            Some(true) => 1,
            Some(false) => 2,
            None => 0,
        };
        let private_marker = if private { "?" } else { "" };
        let report = format!("\x1b[{private_marker}{mode};{status_code}$y");
        self.reply.extend_from_slice(report.as_bytes());
    }

    /// Answer xterm window-operation size queries. `18t` asks for the terminal
    /// text area in characters; `19t` asks for the screen size in characters.
    /// Mighty has one visible grid, so both report the current grid dimensions.
    fn handle_window_ops(&mut self, grid: &Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        match params {
            "18" | "19" => {
                let report = format!("\x1b[8;{};{}t", grid.rows(), grid.cols());
                self.reply.extend_from_slice(report.as_bytes());
            }
            _ => {}
        }
    }

    /// Inside an OSC: consume until BEL or the start of an ST (`ESC \`).
    fn osc(&mut self, b: u8) {
        match b {
            0x07 => self.finish_osc(), // BEL terminates
            0x18 | 0x1a => self.abort_osc(), // CAN/SUB abort
            0x1b => self.state = State::OscEsc, // maybe ST
            0x9c => self.finish_osc(), // 8-bit ST terminates
            _ => self.push_osc_byte(b),
        }
    }

    /// In OSC and saw ESC: a `\` completes ST; anything else re-enters OSC.
    fn osc_esc(&mut self, b: u8) {
        match b {
            b'\\' => self.finish_osc(), // ST terminates
            0x18 | 0x1a => self.abort_osc(), // CAN/SUB abort
            0x07 => self.finish_osc(),  // tolerate stray BEL
            0x9c => self.finish_osc(),  // 8-bit ST terminates
            _ => {
                self.push_osc_byte(0x1b);
                self.push_osc_byte(b);
                self.state = State::Osc;
            }
        }
    }

    fn enter_osc(&mut self) {
        self.osc.clear();
        self.state = State::Osc;
    }

    fn abort_osc(&mut self) {
        self.osc.clear();
        self.state = State::Ground;
    }

    fn push_osc_byte(&mut self, b: u8) {
        if self.osc.len() < MAX_OSC_BYTES {
            self.osc.push(b);
        }
    }

    fn finish_osc(&mut self) {
        self.capture_osc_title();
        self.capture_osc_clipboard();
        self.capture_osc_color_set();
        self.capture_osc_color_reset();
        self.capture_osc_palette_set();
        self.capture_osc_palette_reset();
        self.reply_osc_color_query();
        self.reply_osc_palette_query();
        self.osc.clear();
        self.state = State::Ground;
    }

    fn reply_osc_color_query(&mut self) {
        let (kind, color) = match self.osc.as_slice() {
            b"10;?" => ("10", self.default_fg_rgb),
            b"11;?" => ("11", self.default_bg_rgb),
            b"12;?" => ("12", self.cursor_rgb),
            _ => return,
        };
        self.push_osc_color_reply(kind, color);
    }

    fn capture_osc_color_set(&mut self) {
        let payload = String::from_utf8_lossy(&self.osc);
        let Some((kind, value)) = payload.split_once(';') else {
            return;
        };
        if value == "?" {
            return;
        }
        let Some(color) = parse_osc_rgb(value) else {
            return;
        };
        match kind {
            "10" => self.default_fg_rgb = color,
            "11" => self.default_bg_rgb = color,
            "12" => self.cursor_rgb = color,
            _ => {}
        }
    }

    fn capture_osc_color_reset(&mut self) {
        match self.osc.as_slice() {
            b"110" => self.default_fg_rgb = DEFAULT_FG_RGB,
            b"111" => self.default_bg_rgb = DEFAULT_BG_RGB,
            b"112" => self.cursor_rgb = DEFAULT_CURSOR_RGB,
            _ => {}
        }
    }

    fn reply_osc_palette_query(&mut self) {
        let payload = String::from_utf8_lossy(&self.osc).into_owned();
        let mut parts = payload.split(';');
        if parts.next() != Some("4") {
            return;
        }

        while let Some(index) = parts.next() {
            let Some(value) = parts.next() else {
                break;
            };
            if value != "?" {
                continue;
            }
            let Some(index) = index.parse::<u32>().ok().filter(|n| *n <= 255) else {
                continue;
            };
            let color = self.palette_rgb[index as usize];
            self.push_osc_color_reply(&format!("4;{index}"), color);
        }
    }

    fn capture_osc_palette_set(&mut self) {
        let payload = String::from_utf8_lossy(&self.osc);
        let mut parts = payload.split(';');
        if parts.next() != Some("4") {
            return;
        }

        while let Some(index) = parts.next() {
            let Some(value) = parts.next() else {
                break;
            };
            if value == "?" {
                continue;
            }
            let Some(index) = index.parse::<usize>().ok().filter(|n| *n <= 255) else {
                continue;
            };
            let Some(color) = parse_osc_rgb(value) else {
                continue;
            };
            self.palette_rgb[index] = color;
        }
    }

    fn capture_osc_palette_reset(&mut self) {
        let payload = String::from_utf8_lossy(&self.osc);
        let mut parts = payload.split(';');
        if parts.next() != Some("104") {
            return;
        }

        let mut saw_index = false;
        for index in parts {
            saw_index = true;
            let Some(index) = index.parse::<usize>().ok().filter(|n| *n <= 255) else {
                continue;
            };
            self.palette_rgb[index] = palette_rgb8(index as u32);
        }
        if !saw_index {
            self.palette_rgb = std::array::from_fn(|index| palette_rgb8(index as u32));
        }
    }

    fn push_osc_color_reply(&mut self, kind: &str, (r, g, b): (u8, u8, u8)) {
        let reply = format!(
            "\x1b]{kind};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\"
        );
        self.reply.extend_from_slice(reply.as_bytes());
    }

    fn capture_osc_title(&mut self) {
        let Some(sep) = self.osc.iter().position(|b| *b == b';') else {
            return;
        };
        let kind = &self.osc[..sep];
        if kind != b"0" && kind != b"1" && kind != b"2" {
            return;
        }
        let raw = String::from_utf8_lossy(&self.osc[sep + 1..]);
        let title: String = raw
            .chars()
            .filter(|ch| !ch.is_control())
            .take(160)
            .collect::<String>()
            .trim()
            .to_string();
        if !title.is_empty() {
            self.title = title;
        }
    }

    fn capture_osc_clipboard(&mut self) {
        let mut parts = self.osc.splitn(3, |b| *b == b';');
        if parts.next() != Some(b"52".as_slice()) {
            return;
        }
        let selector = parts.next().unwrap_or_default();
        let payload = parts.next().unwrap_or_default();
        if payload.is_empty() || payload == b"?" {
            return;
        }
        if !selector.is_empty() && !selector.contains(&b'c') {
            return;
        }

        let Some(decoded) = decode_osc52_base64(payload) else {
            return;
        };
        let Ok(text) = String::from_utf8(decoded) else {
            return;
        };
        let text: String = text
            .chars()
            .filter(|ch| *ch != '\0')
            .take(MAX_OSC_52_TEXT_CHARS)
            .collect();
        if !text.is_empty() {
            self.clipboard_write = Some(text);
        }
    }

    fn string(&mut self, b: u8) {
        match b {
            0x18 | 0x1a => self.state = State::Ground, // CAN/SUB abort
            0x1b => self.state = State::StringEsc, // maybe ST
            0x9c => self.state = State::Ground,    // 8-bit ST terminates
            _ => {}                                // payload: consume
        }
    }

    fn string_esc(&mut self, b: u8) {
        match b {
            b'\\' => self.state = State::Ground, // ST terminates
            0x18 | 0x1a => self.state = State::Ground, // CAN/SUB abort
            0x9c => self.state = State::Ground,  // 8-bit ST terminates
            _ => self.state = State::String,     // not ST; keep consuming
        }
    }
}

fn encode_truecolor(r: u8, g: u8, b: u8) -> u32 {
    TRUECOLOR_MASK | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

fn decode_osc52_base64(input: &[u8]) -> Option<Vec<u8>> {
    fn value(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(64),
            b'\t' | b'\n' | b'\r' | b' ' => Some(65),
            _ => None,
        }
    }

    fn push_quartet(out: &mut Vec<u8>, q: [u8; 4]) -> Option<bool> {
        if q[0] >= 64 || q[1] >= 64 || (q[2] == 64 && q[3] != 64) {
            return None;
        }
        out.push((q[0] << 2) | (q[1] >> 4));
        if q[2] != 64 {
            out.push((q[1] << 4) | (q[2] >> 2));
        }
        if q[3] != 64 {
            out.push((q[2] << 6) | q[3]);
        }
        if out.len() > MAX_OSC_52_DECODED_BYTES {
            return None;
        }
        Some(q[2] == 64 || q[3] == 64)
    }

    let mut out = Vec::with_capacity(input.len().saturating_mul(3) / 4);
    let mut q = [0u8; 4];
    let mut q_len = 0;
    let mut padded = false;

    for &b in input {
        let v = value(b)?;
        if v == 65 {
            continue;
        }
        if padded {
            return None;
        }
        q[q_len] = v;
        q_len += 1;
        if q_len == 4 {
            padded = push_quartet(&mut out, q)?;
            q_len = 0;
        }
    }

    match q_len {
        0 => {}
        2 => {
            if q[0] >= 64 || q[1] >= 64 {
                return None;
            }
            out.push((q[0] << 2) | (q[1] >> 4));
        }
        3 => {
            if q[0] >= 64 || q[1] >= 64 || q[2] >= 64 {
                return None;
            }
            out.push((q[0] << 2) | (q[1] >> 4));
            out.push((q[1] << 4) | (q[2] >> 2));
        }
        _ => return None,
    }
    if out.len() > MAX_OSC_52_DECODED_BYTES {
        return None;
    }
    Some(out)
}

fn palette_rgb8(index: u32) -> (u8, u8, u8) {
    let (r, g, b, _) = palette_rgba(index);
    (unit_to_byte(r), unit_to_byte(g), unit_to_byte(b))
}

fn parse_osc_rgb(value: &str) -> Option<(u8, u8, u8)> {
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex_rgb(hex);
    }
    let components = value.strip_prefix("rgb:")?;
    let mut parts = components.split('/');
    let r = parse_osc_rgb_component(parts.next()?)?;
    let g = parse_osc_rgb_component(parts.next()?)?;
    let b = parse_osc_rgb_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((r, g, b))
}

fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

fn parse_osc_rgb_component(component: &str) -> Option<u8> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    let value = u16::from_str_radix(component, 16).ok()?;
    match component.len() {
        1 => Some((value as u8) * 17),
        2 => Some(value as u8),
        3 => Some((value >> 4) as u8),
        4 => Some((value >> 8) as u8),
        _ => None,
    }
}

fn unit_to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgb8_rgba((r, g, b): (u8, u8, u8), alpha: f32) -> (f32, f32, f32, f32) {
    (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, alpha)
}

fn effective_sgr_fg(fg: u32, bold: bool) -> u32 {
    if bold && fg <= 7 {
        fg + 8
    } else {
        fg
    }
}

fn effective_sgr_cell_colors(fg: u32, bg: u32, bold: bool, inverse: bool) -> (u32, u32) {
    let fg = effective_sgr_fg(fg, bold);
    if inverse {
        (bg, fg)
    } else {
        (fg, bg)
    }
}

fn parse_sgr_params(params: &str) -> Vec<Option<i32>> {
    let mut out = Vec::new();
    for part in params.split(';') {
        if part.contains(':') {
            append_colon_sgr_param(part, &mut out);
        } else if part.is_empty() {
            out.push(Some(0));
        } else {
            out.push(part.parse().ok());
        }
    }
    out
}

fn append_colon_sgr_param(part: &str, out: &mut Vec<Option<i32>>) {
    let pieces: Vec<&str> = part.split(':').collect();
    let Some(head) = pieces.first().and_then(|s| s.parse::<i32>().ok()) else {
        out.push(None);
        return;
    };
    let Some(mode) = pieces.get(1).and_then(|s| s.parse::<i32>().ok()) else {
        out.push(Some(head));
        return;
    };

    match (head, mode) {
        (38 | 48, 5) => {
            out.push(Some(head));
            out.push(Some(5));
            out.push(pieces.get(2).and_then(|s| s.parse::<i32>().ok()));
        }
        (38 | 48, 2) => {
            let rgb: Vec<i32> = pieces
                .iter()
                .skip(2)
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<i32>().ok())
                .collect();
            out.push(Some(head));
            out.push(Some(2));
            if rgb.len() >= 3 {
                for value in &rgb[rgb.len() - 3..] {
                    out.push(Some(*value));
                }
            } else {
                out.extend([None, None, None]);
            }
        }
        _ => {
            for piece in pieces {
                if piece.is_empty() {
                    out.push(Some(0));
                } else {
                    out.push(piece.parse().ok());
                }
            }
        }
    }
}

/// Resolve a terminal color code to RGBA (0.0..=1.0). [`DEFAULT_FG`] -> a light
/// neutral. The first 16 palette entries are a readable ANSI-ish palette tuned
/// for a dark background; 16..=255 follow the standard xterm 256-color palette.
/// Encoded RGB truecolor values resolve exactly.
pub fn palette_rgba(fg: u32) -> (f32, f32, f32, f32) {
    if fg == DEFAULT_FG {
        return (0.82, 0.84, 0.88, 1.0);
    }

    if fg & TRUECOLOR_MASK != 0 {
        let r = ((fg >> 16) & 0xff) as f32 / 255.0;
        let g = ((fg >> 8) & 0xff) as f32 / 255.0;
        let b = (fg & 0xff) as f32 / 255.0;
        return (r, g, b, 1.0);
    }

    let rgb = match fg {
        0 => (0.20, 0.20, 0.22),  // black (dim, visible on dark bg)
        1 => (0.80, 0.25, 0.25),  // red
        2 => (0.30, 0.72, 0.35),  // green
        3 => (0.80, 0.68, 0.25),  // yellow
        4 => (0.35, 0.55, 0.90),  // blue
        5 => (0.75, 0.40, 0.80),  // magenta
        6 => (0.30, 0.72, 0.78),  // cyan
        7 => (0.80, 0.82, 0.86),  // white
        8 => (0.45, 0.45, 0.48),  // bright black (gray)
        9 => (0.95, 0.45, 0.45),  // bright red
        10 => (0.50, 0.90, 0.55), // bright green
        11 => (0.95, 0.85, 0.45), // bright yellow
        12 => (0.55, 0.72, 1.0),  // bright blue
        13 => (0.90, 0.60, 0.95), // bright magenta
        14 => (0.50, 0.90, 0.95), // bright cyan
        15 => (0.96, 0.97, 1.0),  // bright white
        16..=231 => {
            let idx = fg - 16;
            let levels = [
                0.0,
                95.0 / 255.0,
                135.0 / 255.0,
                175.0 / 255.0,
                215.0 / 255.0,
                1.0,
            ];
            (
                levels[(idx / 36) as usize],
                levels[((idx / 6) % 6) as usize],
                levels[(idx % 6) as usize],
            )
        }
        232..=255 => {
            let level = (8 + (fg - 232) * 10) as f32 / 255.0;
            (level, level, level)
        }
        _ => (0.82, 0.84, 0.88),  // unknown
    };
    (rgb.0, rgb.1, rgb.2, 1.0)
}

/// Resolve a terminal background color code to RGBA. [`DEFAULT_BG`] is
/// transparent so the terminal panel background shows through when no SGR
/// background is active.
#[cfg(test)]
pub fn background_rgba(bg: u32) -> Option<(f32, f32, f32, f32)> {
    if bg == DEFAULT_BG {
        return None;
    }
    let (r, g, b, _) = palette_rgba(bg);
    Some((r, g, b, 0.72))
}

/// A live PTY-backed terminal: a spawned shell, a reader thread draining its
/// output into a shared buffer, the parser, and the grid.
pub struct Terminal {
    grid: Grid,
    parser: VtParser,
    /// Current IDE keyboard-focus state for xterm focus-in/focus-out reports.
    focused: bool,
    /// Last focus state reported to the PTY while focus reporting was enabled.
    reported_focus: Option<bool>,
    /// Currently pressed mouse button for drag/any-motion reporting.
    mouse_button_down: Option<u32>,
    /// PTY master half — used to write stdin and resize.
    master: Box<dyn MasterPty + Send>,
    /// Writer to the PTY (the child's stdin).
    writer: Box<dyn Write + Send>,
    /// The spawned child (kept alive; killed on drop).
    child: Box<dyn Child + Send + Sync>,
    /// Output bytes accumulated by the reader thread, drained on `pump`.
    out: Arc<Mutex<Vec<u8>>>,
    /// Signals the reader thread reached EOF (shell exited).
    eof: Receiver<()>,
}

impl Terminal {
    /// Spawn a shell in a new PTY sized `rows`×`cols`. On Windows this uses the
    /// default ConPTY backend and runs `cmd.exe`; elsewhere `$SHELL` or `/bin/sh`.
    /// Returns an error string on failure (caller decides whether to surface it).
    pub fn spawn(rows: usize, cols: usize) -> Result<Self, String> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e}"))?;

        let cmd = default_shell_command();
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn shell failed: {e}"))?;
        // The slave handle is owned by the child now; drop our copy so EOF is
        // observed when the child exits.
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("pty take_writer failed: {e}"))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("pty clone_reader failed: {e}"))?;

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (eof_tx, eof_rx) = mpsc::channel();
        let out_thread = Arc::clone(&out);
        std::thread::Builder::new()
            .name("mui-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: shell exited
                        Ok(n) => {
                            if let Ok(mut g) = out_thread.lock() {
                                g.extend_from_slice(&buf[..n]);
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = eof_tx.send(());
            })
            .map_err(|e| format!("spawn reader thread failed: {e}"))?;

        Ok(Terminal {
            grid: Grid::new(rows, cols),
            parser: VtParser::new(),
            focused: false,
            reported_focus: None,
            mouse_button_down: None,
            master: pair.master,
            writer,
            child,
            out,
            eof: eof_rx,
        })
    }

    pub fn rows(&self) -> usize {
        self.grid.rows()
    }

    pub fn cols(&self) -> usize {
        self.grid.cols()
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    pub fn visible_contains(&self, needle: &str) -> bool {
        self.grid.contains(needle)
    }

    pub fn clear_buffer(&mut self) -> bool {
        let had_content = self.grid.has_visible_content();
        self.grid.clear();
        had_content
    }

    pub fn cursor_visible(&self) -> bool {
        self.parser.cursor_visible()
    }

    pub fn cursor_blinking(&self) -> bool {
        self.parser.cursor_blinking()
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.parser.cursor_shape()
    }

    pub fn foreground_rgba(&self, fg: u32) -> (f32, f32, f32, f32) {
        self.parser.foreground_rgba(fg)
    }

    pub fn background_rgba(&self, bg: u32) -> Option<(f32, f32, f32, f32)> {
        self.parser.background_rgba(bg)
    }

    pub fn cursor_rgba(&self) -> (f32, f32, f32, f32) {
        self.parser.cursor_rgba()
    }

    pub fn title(&self) -> &str {
        self.parser.title()
    }

    pub fn take_clipboard_write(&mut self) -> Option<String> {
        self.parser.take_clipboard_write()
    }

    /// Drain any pending PTY output through the parser into the grid. Cheap when
    /// there is nothing buffered. Call once per frame.
    pub fn pump(&mut self) {
        let chunk = {
            match self.out.lock() {
                Ok(mut g) => {
                    if g.is_empty() {
                        return;
                    }
                    std::mem::take(&mut *g)
                }
                Err(_) => return,
            }
        };
        self.parser.feed(&mut self.grid, &chunk);
        // Answer any DSR queries the parser collected (ConPTY blocks on these).
        let reply = self.parser.take_reply();
        if !reply.is_empty() {
            self.send(&reply);
        }
        self.report_focus_if_needed();
    }

    /// Write raw bytes to the PTY stdin (the shell's input).
    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Update IDE keyboard focus and send xterm focus in/out reports when the
    /// running app enabled `CSI ?1004 h`.
    pub fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.report_focus_if_needed();
    }

    fn report_focus_if_needed(&mut self) {
        if !self.parser.focus_reporting_enabled() {
            self.reported_focus = None;
            return;
        }
        if self.reported_focus == Some(self.focused) {
            return;
        }
        self.reported_focus = Some(self.focused);
        self.send(focus_report_to_bytes(self.focused));
    }

    /// Send a named key, honoring terminal modes that alter key encoding.
    pub fn send_key(&mut self, key: u32, mods: u32) {
        if let Some(bytes) = key_to_bytes(key, mods, self.parser.application_cursor_keys()) {
            self.send(&bytes);
        }
    }

    /// Send pasted text, wrapping it when the shell/app has enabled bracketed
    /// paste so pasted newlines are not interpreted as typed Enter presses.
    pub fn send_paste(&mut self, text: &str) {
        let bytes = paste_to_bytes(text, self.parser.bracketed_paste_enabled());
        self.send(&bytes);
    }

    /// Send a wheel gesture at a 1-based terminal cell coordinate.
    pub fn send_scroll_at(&mut self, dir: i32, row: usize, col: usize, mods: u32) {
        if let Some(bytes) = scroll_to_bytes(
            dir,
            self.parser.mouse_reporting_enabled(),
            self.parser.sgr_mouse_enabled(),
            row,
            col,
            mods,
        ) {
            self.send(&bytes);
        }
    }

    /// Send a mouse button press/release at a 1-based terminal cell coordinate.
    pub fn send_mouse_button_at(
        &mut self,
        pressed: bool,
        button: u32,
        row: usize,
        col: usize,
        mods: u32,
    ) {
        let known_button = mouse_button_code(button).is_some();
        if known_button || !pressed {
            self.mouse_button_down = if pressed { Some(button) } else { None };
        }
        if let Some(bytes) = mouse_button_to_bytes(
            pressed,
            button,
            self.parser.mouse_reporting_enabled(),
            self.parser.sgr_mouse_enabled(),
            row,
            col,
            mods,
        ) {
            self.send(&bytes);
        }
    }

    pub fn clear_mouse_button_state(&mut self) {
        self.mouse_button_down = None;
    }

    pub fn mouse_motion_reporting_enabled(&self) -> bool {
        self.parser.mouse_any_reporting_enabled()
            || (self.parser.mouse_drag_reporting_enabled() && self.mouse_button_down.is_some())
    }

    /// Send a mouse motion event at a 1-based terminal cell coordinate.
    pub fn mouse_reporting_enabled(&self) -> bool {
        self.parser.mouse_reporting_enabled()
    }

    pub fn send_mouse_motion_at(&mut self, row: usize, col: usize, mods: u32) {
        let button = if self.parser.mouse_any_reporting_enabled() {
            self.mouse_button_down
        } else if self.parser.mouse_drag_reporting_enabled() {
            match self.mouse_button_down {
                Some(button) => Some(button),
                None => return,
            }
        } else {
            return;
        };
        if let Some(bytes) = mouse_motion_to_bytes(
            button,
            self.parser.mouse_reporting_enabled(),
            self.parser.sgr_mouse_enabled(),
            row,
            col,
            mods,
        ) {
            self.send(&bytes);
        }
    }

    /// Resize the PTY and the grid to `rows`×`cols`.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.grid.rows() && cols == self.grid.cols() {
            return;
        }
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.grid.resize(rows, cols);
    }

    /// Whether the shell child is still running (false once it exits / EOF).
    pub fn is_alive(&mut self) -> bool {
        // EOF from the reader thread is the authoritative "exited" signal.
        match self.eof.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => return false,
            Err(TryRecvError::Empty) => {}
        }
        // Also poll the child directly (non-blocking).
        match self.child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(_) => false,
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Best-effort: kill the shell so we don't leak a process.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Build the shell command to spawn: `cmd.exe` on Windows, `$SHELL`/`/bin/sh`
/// elsewhere. Inherits the current working directory.
fn default_shell_command() -> CommandBuilder {
    #[cfg(windows)]
    {
        // ComSpec is `C:\Windows\system32\cmd.exe` on a normal install.
        let shell = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut cmd = CommandBuilder::new(shell);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        cmd
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut cmd = CommandBuilder::new(shell);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        cmd
    }
}

// ---------------------------------------------------------------------------
// key -> bytes mapping (shim-side, given a MUI_KEY_* code + mods)
// ---------------------------------------------------------------------------

/// Map a named key code (`MUI_KEY_*`) + modifier bits to the bytes a terminal
/// expects, or `None` for keys with no terminal meaning. Enter -> CR (`\r`),
/// Alt+Enter -> Meta-CR (`ESC CR`), Backspace -> DEL (`\x7f`), Alt+Backspace ->
/// Meta-DEL (`ESC DEL`), Tab -> `\t`, Alt+Tab -> Meta-HT (`ESC TAB`),
/// Shift+Tab -> `ESC [ Z`, Alt+Shift+Tab -> Meta-Shift+Tab (`ESC ESC [ Z`),
/// Escape -> `\x1b`, Alt+Escape -> Meta-Escape (`ESC ESC`), arrows -> the usual
/// `ESC [ A/B/C/D`, Insert -> `ESC [ 2 ~`.
/// Ctrl+letter (handled on the Char path) is mapped separately.
pub fn key_to_bytes(key: u32, mods: u32, application_cursor_keys: bool) -> Option<Vec<u8>> {
    use crate::ffi::*;
    let modifier = terminal_modifier_param(mods);
    let bytes: Vec<u8> = match key {
        MUI_KEY_ENTER if mods & MUI_MOD_ALT != 0 => vec![0x1b, b'\r'],
        MUI_KEY_ENTER => vec![b'\r'],
        MUI_KEY_BACKSPACE if mods & MUI_MOD_ALT != 0 => vec![0x1b, 0x7f],
        MUI_KEY_BACKSPACE => vec![0x7f],
        MUI_KEY_TAB if mods & MUI_MOD_ALT != 0 && mods & MUI_MOD_SHIFT != 0 => {
            vec![0x1b, 0x1b, b'[', b'Z']
        }
        MUI_KEY_TAB if mods & MUI_MOD_SHIFT != 0 => vec![0x1b, b'[', b'Z'],
        MUI_KEY_TAB if mods & MUI_MOD_ALT != 0 => vec![0x1b, b'\t'],
        MUI_KEY_TAB => vec![b'\t'],
        MUI_KEY_ESCAPE if mods & MUI_MOD_ALT != 0 => vec![0x1b, 0x1b],
        MUI_KEY_ESCAPE => vec![0x1b],
        MUI_KEY_LEFT if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'D'),
        MUI_KEY_RIGHT if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'C'),
        MUI_KEY_UP if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'A'),
        MUI_KEY_DOWN if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'B'),
        MUI_KEY_HOME if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'H'),
        MUI_KEY_END if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'F'),
        MUI_KEY_INSERT if modifier.is_some() => modified_csi_tilde(2, modifier.unwrap()),
        MUI_KEY_DELETE if modifier.is_some() => modified_csi_tilde(3, modifier.unwrap()),
        MUI_KEY_PAGE_UP if modifier.is_some() => modified_csi_tilde(5, modifier.unwrap()),
        MUI_KEY_PAGE_DOWN if modifier.is_some() => modified_csi_tilde(6, modifier.unwrap()),
        MUI_KEY_F1 if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'P'),
        MUI_KEY_F2 if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'Q'),
        MUI_KEY_F3 if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'R'),
        MUI_KEY_F4 if modifier.is_some() => modified_csi_1(modifier.unwrap(), b'S'),
        MUI_KEY_F5 if modifier.is_some() => modified_csi_tilde(15, modifier.unwrap()),
        MUI_KEY_F6 if modifier.is_some() => modified_csi_tilde(17, modifier.unwrap()),
        MUI_KEY_F7 if modifier.is_some() => modified_csi_tilde(18, modifier.unwrap()),
        MUI_KEY_F8 if modifier.is_some() => modified_csi_tilde(19, modifier.unwrap()),
        MUI_KEY_F9 if modifier.is_some() => modified_csi_tilde(20, modifier.unwrap()),
        MUI_KEY_F10 if modifier.is_some() => modified_csi_tilde(21, modifier.unwrap()),
        MUI_KEY_F11 if modifier.is_some() => modified_csi_tilde(23, modifier.unwrap()),
        MUI_KEY_F12 if modifier.is_some() => modified_csi_tilde(24, modifier.unwrap()),
        MUI_KEY_LEFT if application_cursor_keys => vec![0x1b, b'O', b'D'],
        MUI_KEY_RIGHT if application_cursor_keys => vec![0x1b, b'O', b'C'],
        MUI_KEY_UP if application_cursor_keys => vec![0x1b, b'O', b'A'],
        MUI_KEY_DOWN if application_cursor_keys => vec![0x1b, b'O', b'B'],
        MUI_KEY_LEFT => vec![0x1b, b'[', b'D'],
        MUI_KEY_RIGHT => vec![0x1b, b'[', b'C'],
        MUI_KEY_UP => vec![0x1b, b'[', b'A'],
        MUI_KEY_DOWN => vec![0x1b, b'[', b'B'],
        MUI_KEY_HOME => vec![0x1b, b'[', b'H'],
        MUI_KEY_END => vec![0x1b, b'[', b'F'],
        MUI_KEY_INSERT => vec![0x1b, b'[', b'2', b'~'],
        MUI_KEY_DELETE => vec![0x1b, b'[', b'3', b'~'],
        MUI_KEY_PAGE_UP => vec![0x1b, b'[', b'5', b'~'],
        MUI_KEY_PAGE_DOWN => vec![0x1b, b'[', b'6', b'~'],
        MUI_KEY_F1 => vec![0x1b, b'O', b'P'],
        MUI_KEY_F2 => vec![0x1b, b'O', b'Q'],
        MUI_KEY_F3 => vec![0x1b, b'O', b'R'],
        MUI_KEY_F4 => vec![0x1b, b'O', b'S'],
        MUI_KEY_F5 => vec![0x1b, b'[', b'1', b'5', b'~'],
        MUI_KEY_F6 => vec![0x1b, b'[', b'1', b'7', b'~'],
        MUI_KEY_F7 => vec![0x1b, b'[', b'1', b'8', b'~'],
        MUI_KEY_F8 => vec![0x1b, b'[', b'1', b'9', b'~'],
        MUI_KEY_F9 => vec![0x1b, b'[', b'2', b'0', b'~'],
        MUI_KEY_F10 => vec![0x1b, b'[', b'2', b'1', b'~'],
        MUI_KEY_F11 => vec![0x1b, b'[', b'2', b'3', b'~'],
        MUI_KEY_F12 => vec![0x1b, b'[', b'2', b'4', b'~'],
        _ => return None,
    };
    Some(bytes)
}

fn terminal_modifier_param(mods: u32) -> Option<u8> {
    use crate::ffi::{MUI_MOD_ALT, MUI_MOD_CTRL, MUI_MOD_SHIFT};
    let mut value = 1_u8;
    if mods & MUI_MOD_SHIFT != 0 {
        value += 1;
    }
    if mods & MUI_MOD_ALT != 0 {
        value += 2;
    }
    if mods & MUI_MOD_CTRL != 0 {
        value += 4;
    }
    (value > 1).then_some(value)
}

fn modified_csi_1(modifier: u8, final_byte: u8) -> Vec<u8> {
    format!("\x1b[1;{}{}", modifier, final_byte as char).into_bytes()
}

fn modified_csi_tilde(code: u8, modifier: u8) -> Vec<u8> {
    format!("\x1b[{};{}~", code, modifier).into_bytes()
}

/// Map a typed codepoint + modifier bits to terminal stdin bytes. With Ctrl held
/// and an ASCII letter, emit the corresponding control code (Ctrl+C -> 0x03,
/// etc.); otherwise emit the char's UTF-8 bytes. With Alt held, prefix the
/// resulting payload with ESC for Meta input.
pub fn codepoint_to_bytes(codepoint: u32, mods: u32) -> Option<Vec<u8>> {
    use crate::ffi::{MUI_MOD_ALT, MUI_MOD_CTRL};
    let ch = char::from_u32(codepoint)?;
    let mut bytes = None;
    if mods & MUI_MOD_CTRL != 0 {
        // Ctrl+@..Ctrl+_ -> 0x00..0x1f. Letters are case-insensitive.
        let upper = (ch as u32).to_ascii_uppercase_u32();
        if (0x40..=0x5f).contains(&upper) {
            bytes = Some(vec![(upper - 0x40) as u8]);
        }
        // Ctrl+space -> NUL.
        if bytes.is_none() && ch == ' ' {
            bytes = Some(vec![0]);
        }
    }
    let mut bytes = bytes.unwrap_or_else(|| {
        let mut buf = [0u8; 4];
        ch.encode_utf8(&mut buf).as_bytes().to_vec()
    });
    if mods & MUI_MOD_ALT != 0 {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

pub fn paste_to_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + if bracketed { 12 } else { 0 });
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(text.as_bytes());
    if bracketed {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    bytes
}

pub fn scroll_to_bytes(
    dir: i32,
    mouse_reporting: bool,
    sgr_mouse: bool,
    row: usize,
    col: usize,
    mods: u32,
) -> Option<Vec<u8>> {
    let row = row.max(1);
    let col = col.max(1);
    if sgr_mouse {
        let modifier = mouse_modifier_code(mods);
        return match dir {
            d if d > 0 => Some(format!("\x1b[<{};{col};{row}M", 64 + modifier).into_bytes()),
            d if d < 0 => Some(format!("\x1b[<{};{col};{row}M", 65 + modifier).into_bytes()),
            _ => None,
        };
    }

    if mouse_reporting {
        let button = if dir > 0 {
            64
        } else if dir < 0 {
            65
        } else {
            return None;
        };
        let x = (col.min(223) as u8) + 32;
        let y = (row.min(223) as u8) + 32;
        return Some(vec![0x1b, b'[', b'M', 32 + button + mouse_modifier_code(mods), x, y]);
    }

    let key = if dir > 0 {
        b'A'
    } else if dir < 0 {
        b'B'
    } else {
        return None;
    };

    let mut bytes = Vec::with_capacity(9);
    for _ in 0..3 {
        bytes.extend_from_slice(&[0x1b, b'[', key]);
    }
    Some(bytes)
}

pub fn mouse_button_to_bytes(
    pressed: bool,
    button: u32,
    mouse_reporting: bool,
    sgr_mouse: bool,
    row: usize,
    col: usize,
    mods: u32,
) -> Option<Vec<u8>> {
    if !mouse_reporting {
        return None;
    }

    let row = row.max(1);
    let col = col.max(1);
    let modifier = mouse_modifier_code(mods);
    let code = mouse_button_code(button)? + modifier;

    if sgr_mouse {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{col};{row}{suffix}").into_bytes());
    }

    let event_code = if pressed { code } else { 3 + modifier };
    let x = (col.min(223) as u8) + 32;
    let y = (row.min(223) as u8) + 32;
    Some(vec![0x1b, b'[', b'M', event_code + 32, x, y])
}

fn mouse_button_code(button: u32) -> Option<u8> {
    match button {
        crate::ffi::MUI_MOUSE_LEFT => Some(0),
        crate::ffi::MUI_MOUSE_MIDDLE => Some(1),
        crate::ffi::MUI_MOUSE_RIGHT => Some(2),
        _ => None,
    }
}

pub fn mouse_motion_to_bytes(
    button: Option<u32>,
    mouse_reporting: bool,
    sgr_mouse: bool,
    row: usize,
    col: usize,
    mods: u32,
) -> Option<Vec<u8>> {
    if !mouse_reporting {
        return None;
    }

    let row = row.max(1);
    let col = col.max(1);
    let code = match button {
        Some(button) => mouse_button_code(button)? + 32,
        None => 35,
    } + mouse_modifier_code(mods);

    if sgr_mouse {
        return Some(format!("\x1b[<{code};{col};{row}M").into_bytes());
    }

    let x = (col.min(223) as u8) + 32;
    let y = (row.min(223) as u8) + 32;
    Some(vec![0x1b, b'[', b'M', code + 32, x, y])
}

fn mouse_modifier_code(mods: u32) -> u8 {
    use crate::ffi::{MUI_MOD_ALT, MUI_MOD_CTRL, MUI_MOD_SHIFT};
    let mut code = 0;
    if mods & MUI_MOD_SHIFT != 0 {
        code += 4;
    }
    if mods & MUI_MOD_ALT != 0 {
        code += 8;
    }
    if mods & MUI_MOD_CTRL != 0 {
        code += 16;
    }
    code
}

pub fn focus_report_to_bytes(focused: bool) -> &'static [u8] {
    if focused {
        b"\x1b[I"
    } else {
        b"\x1b[O"
    }
}

/// Tiny extension so `codepoint_to_bytes` can uppercase a raw u32 codepoint
/// without an intermediate `char` round-trip for the ASCII range.
trait AsciiUpperU32 {
    fn to_ascii_uppercase_u32(self) -> u32;
}
impl AsciiUpperU32 for u32 {
    fn to_ascii_uppercase_u32(self) -> u32 {
        if (0x61..=0x7a).contains(&self) {
            self - 0x20
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_feed(rows: usize, cols: usize, bytes: &[u8]) -> Grid {
        let mut g = Grid::new(rows, cols);
        let mut p = VtParser::new();
        p.feed(&mut g, bytes);
        g
    }

    #[test]
    fn plain_text_fills_first_row() {
        let g = grid_feed(4, 10, b"hello");
        assert_eq!(g.cell(0, 0).ch, 'h');
        assert_eq!(g.cell(0, 4).ch, 'o');
        assert_eq!(g.cursor(), (0, 5));
        assert!(g.contains("hello"));
    }

    #[test]
    fn clear_buffer_empties_visible_grid_and_reports_content() {
        let mut g = grid_feed(2, 8, b"prompt");
        assert!(g.has_visible_content());
        g.clear();
        assert!(!g.has_visible_content());
        assert_eq!(g.to_text(), "        \n        ");
        assert_eq!(g.cursor(), (0, 0));
    }

    #[test]
    fn newline_and_carriage_return_move_cursor() {
        let g = grid_feed(4, 10, b"ab\ncd");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'b');
        assert_eq!(g.cell(1, 0).ch, 'c');
        assert_eq!(g.cell(1, 1).ch, 'd');
        assert_eq!(g.cursor(), (1, 2));

        // CR returns to column 0 of the same row; subsequent text overwrites.
        let g2 = grid_feed(2, 10, b"abc\rX");
        assert_eq!(g2.cell(0, 0).ch, 'X');
        assert_eq!(g2.cell(0, 1).ch, 'b');

        let g3 = grid_feed(4, 10, b"ab\x0bcd\x0cef");
        assert_eq!(g3.cell(0, 0).ch, 'a');
        assert_eq!(g3.cell(1, 0).ch, 'c');
        assert_eq!(g3.cell(2, 0).ch, 'e');
        assert_eq!(g3.cursor(), (2, 2));
    }

    #[test]
    fn backspace_moves_cursor_left() {
        // Shells echo backspace as BS, space, BS; emulate the cursor motion.
        let g = grid_feed(2, 10, b"abc\x08");
        assert_eq!(g.cursor(), (0, 2));
        // Writing now overwrites the 'c'.
        let g2 = grid_feed(2, 10, b"abc\x08X");
        assert_eq!(g2.cell(0, 2).ch, 'X');
    }

    #[test]
    fn del_output_byte_is_ignored() {
        let g = grid_feed(2, 10, b"abc\x7fX");
        assert_eq!(g.to_text(), "abcX      \n          ");
        assert_eq!(g.cursor(), (0, 4));
    }

    #[test]
    fn ansi_newline_mode_controls_linefeed_column() {
        let g = grid_feed(3, 8, b"ab\x1b[20lcd\x1b[20h\nef");
        assert_eq!(g.to_text(), "abcd    \nef      \n        ");
        assert_eq!(g.cursor(), (1, 2));
        assert!(!g.contains("20l"));
        assert!(!g.contains("20h"));

        let g2 = grid_feed(3, 8, b"ab\x1b[20l\ncd\rEF");
        assert_eq!(g2.to_text(), "ab      \nEFcd    \n        ");

        let g3 = grid_feed(3, 8, b"ab\x1b[20l\ncd\x1b[!p\nef");
        assert_eq!(g3.to_text(), "ab      \nefcd    \n        ");
        assert!(!g3.contains("!p"));

        let g4 = grid_feed(3, 8, b"ab\x1b[20l\x0bcd\rEF");
        assert_eq!(g4.to_text(), "ab      \nEFcd    \n        ");
    }

    #[test]
    fn tab_advances_to_next_stop() {
        let g = grid_feed(2, 40, b"a\tb");
        // 'a' at col 0, tab -> col 8, 'b' at col 8.
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 8).ch, 'b');
    }

    #[test]
    fn custom_tab_stops_are_set_and_cleared() {
        let g = grid_feed(1, 16, b"\x1b[1;5H\x1bH\x1b[1;1Ha\tb");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 4).ch, 'b');
        assert!(!g.contains("[H"));

        let g2 = grid_feed(1, 16, b"\x1b[1;5H\x1bH\x1b[g\x1b[1;1Ha\tb");
        assert_eq!(g2.cell(0, 0).ch, 'a');
        assert_eq!(g2.cell(0, 8).ch, 'b');
        assert!(!g2.contains("[g"));
    }

    #[test]
    fn csi_cursor_tab_control_sets_and_clears_tab_stops() {
        let g = grid_feed(1, 20, b"\x1b[1;5H\x1b[W\x1b[1;1Ha\tb");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 4).ch, 'b');
        assert!(!g.contains("[W"));

        let g2 = grid_feed(1, 20, b"\x1b[1;5H\x1b[W\x1b[2W\x1b[1;1Ha\tb");
        assert_eq!(g2.cell(0, 0).ch, 'a');
        assert_eq!(g2.cell(0, 8).ch, 'b');
        assert!(!g2.contains("2W"));

        let g3 = grid_feed(1, 12, b"\x1b[1;5H\x1b[W\x1b[5W\x1b[1;1Ha\t");
        assert_eq!(g3.cell(0, 0).ch, 'a');
        assert_eq!(g3.cursor(), (0, 11));
        assert!(!g3.contains("5W"));
    }

    #[test]
    fn clearing_all_tab_stops_pins_tab_at_right_edge_until_reset() {
        let g = grid_feed(1, 12, b"\x1b[3ga\t");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cursor(), (0, 11));

        let g2 = grid_feed(1, 12, b"\x1b[3g\x1bca\tb");
        assert_eq!(g2.cell(0, 0).ch, 'a');
        assert_eq!(g2.cell(0, 8).ch, 'b');
    }

    #[test]
    fn csi_forward_and_backward_tab_use_tab_stops() {
        let g = grid_feed(1, 32, b"a\x1b[2Ib");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 16).ch, 'b');
        assert!(!g.contains("2I"));

        let g2 = grid_feed(1, 32, b"\x1b[1;20H\x1b[Zx\x1b[2Zy");
        assert_eq!(g2.cell(0, 16).ch, 'x');
        assert_eq!(g2.cell(0, 8).ch, 'y');
        assert!(!g2.contains("[Z"));
    }

    #[test]
    fn csi_tabs_honor_custom_tab_stops() {
        let g = grid_feed(1, 20, b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;12H\x1bH\x1b[1;1Ha\x1b[Ib\x1b[Ic");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 4).ch, 'b');
        assert_eq!(g.cell(0, 11).ch, 'c');

        let g2 = grid_feed(1, 20, b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;12H\x1bH\x1b[1;18H\x1b[2Zd");
        assert_eq!(g2.cell(0, 4).ch, 'd');
    }

    #[test]
    fn wrap_at_right_edge() {
        let g = grid_feed(3, 3, b"abcd");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 2).ch, 'c');
        assert_eq!(g.cell(1, 0).ch, 'd');
        assert_eq!(g.cursor(), (1, 1));

        let g2 = grid_feed(3, 3, b"abc");
        assert_eq!(g2.to_text(), "abc\n   \n   ");
        assert_eq!(g2.cursor(), (0, 2));
    }

    #[test]
    fn dec_autowrap_mode_controls_right_margin_behavior() {
        let g = grid_feed(2, 3, b"\x1b[?7labcd");
        assert_eq!(g.to_text(), "abd\n   ");
        assert_eq!(g.cursor(), (0, 2));
        assert!(!g.contains("?7l"));

        let g2 = grid_feed(2, 3, b"\x1b[?7labcd\x1b[1;1H\x1b[?7hWXYZ");
        assert_eq!(g2.to_text(), "WXY\nZ  ");
        assert_eq!(g2.cursor(), (1, 1));
        assert!(!g2.contains("?7h"));
    }

    #[test]
    fn esc_c_resets_autowrap_mode() {
        let g = grid_feed(2, 3, b"\x1b[?7l\x1bcabcd");
        assert_eq!(g.to_text(), "abc\nd  ");
        assert_eq!(g.cursor(), (1, 1));
    }

    #[test]
    fn scroll_up_when_past_last_row() {
        // 2 rows: fill row 0, newline to row 1, newline scrolls.
        let g = grid_feed(2, 4, b"AA\nBB\nCC");
        // After the second newline the grid scrolled: row 0 = "BB", row 1 = "CC".
        assert_eq!(g.cell(0, 0).ch, 'B');
        assert_eq!(g.cell(1, 0).ch, 'C');
    }

    #[test]
    fn sgr_sets_foreground_color() {
        // ESC[31m -> red (index 1), then 'X'.
        let g = grid_feed(2, 10, b"\x1b[31mX");
        let cell = g.cell(0, 0);
        assert_eq!(cell.ch, 'X');
        assert_eq!(cell.fg, 1, "expected red fg index");

        // ESC[0m resets to default.
        let g2 = grid_feed(2, 10, b"\x1b[32mA\x1b[0mB");
        assert_eq!(g2.cell(0, 0).fg, 2); // green
        assert_eq!(g2.cell(0, 1).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_bright_colors() {
        // ESC[91m -> bright red (index 9).
        let g = grid_feed(2, 10, b"\x1b[91mZ");
        assert_eq!(g.cell(0, 0).fg, 9);
    }

    #[test]
    fn sgr_bold_maps_basic_foreground_to_bright_for_later_cells() {
        let g = grid_feed(1, 12, b"\x1b[31mA\x1b[1mB\x1b[22mC\x1b[1;32mD\x1b[0mE");
        assert_eq!(g.cell(0, 0).fg, 1);
        assert_eq!(g.cell(0, 1).fg, 9);
        assert_eq!(g.cell(0, 2).fg, 1);
        assert_eq!(g.cell(0, 3).fg, 10);
        assert_eq!(g.cell(0, 4).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_bold_does_not_rewrite_non_basic_foregrounds() {
        let g = grid_feed(
            1,
            12,
            b"\x1b[38;5;196m\x1b[1mA\x1b[38;2;1;2;3mB\x1b[39mC",
        );
        assert_eq!(g.cell(0, 0).fg, 196);
        assert_eq!(g.cell(0, 1).fg, encode_truecolor(1, 2, 3));
        assert_eq!(g.cell(0, 2).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_inverse_swaps_effective_foreground_and_background() {
        let g = grid_feed(1, 12, b"\x1b[31;44mA\x1b[7mB\x1b[27mC\x1b[1;32;47;7mD\x1b[0mE");
        assert_eq!(g.cell(0, 0).fg, 1);
        assert_eq!(g.cell(0, 0).bg, 4);
        assert_eq!(g.cell(0, 1).fg, 4);
        assert_eq!(g.cell(0, 1).bg, 1);
        assert_eq!(g.cell(0, 2).fg, 1);
        assert_eq!(g.cell(0, 2).bg, 4);
        assert_eq!(g.cell(0, 3).fg, 7);
        assert_eq!(g.cell(0, 3).bg, 10);
        assert_eq!(g.cell(0, 4).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 4).bg, DEFAULT_BG);
    }

    #[test]
    fn sgr_inverse_handles_default_colors_for_drawing() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[7mA\x1b[27mB");

        assert_eq!(g.cell(0, 0).fg, DEFAULT_BG);
        assert_eq!(g.cell(0, 0).bg, DEFAULT_FG);
        assert_eq!(g.cell(0, 1).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 1).bg, DEFAULT_BG);
        assert_eq!(p.foreground_rgba(DEFAULT_BG), rgb8_rgba(DEFAULT_BG_RGB, 1.0));
    }

    #[test]
    fn sgr_underline_marks_later_cells_until_reset() {
        let g = grid_feed(1, 12, b"A\x1b[4mBC\x1b[24mD\x1b[4;31mE\x1b[0mF");
        assert!(!g.cell(0, 0).underline);
        assert!(g.cell(0, 1).underline);
        assert!(g.cell(0, 2).underline);
        assert!(!g.cell(0, 3).underline);
        assert!(g.cell(0, 4).underline);
        assert_eq!(g.cell(0, 4).fg, 1);
        assert!(!g.cell(0, 5).underline);
        assert_eq!(g.cell(0, 5).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_strikethrough_marks_later_cells_until_reset() {
        let g = grid_feed(1, 12, b"A\x1b[9mBC\x1b[29mD\x1b[9;31mE\x1b[0mF");
        assert!(!g.cell(0, 0).strikethrough);
        assert!(g.cell(0, 1).strikethrough);
        assert!(g.cell(0, 2).strikethrough);
        assert!(!g.cell(0, 3).strikethrough);
        assert!(g.cell(0, 4).strikethrough);
        assert_eq!(g.cell(0, 4).fg, 1);
        assert!(!g.cell(0, 5).strikethrough);
        assert_eq!(g.cell(0, 5).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_italic_marks_later_cells_until_reset() {
        let g = grid_feed(1, 12, b"A\x1b[3mBC\x1b[23mD\x1b[3;31mE\x1b[0mF");
        assert!(!g.cell(0, 0).italic);
        assert!(g.cell(0, 1).italic);
        assert!(g.cell(0, 2).italic);
        assert!(!g.cell(0, 3).italic);
        assert!(g.cell(0, 4).italic);
        assert_eq!(g.cell(0, 4).fg, 1);
        assert!(!g.cell(0, 5).italic);
        assert_eq!(g.cell(0, 5).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_faint_marks_later_cells_until_intensity_reset() {
        let g = grid_feed(1, 12, b"A\x1b[2mBC\x1b[22mD\x1b[2;31mE\x1b[0mF");
        assert!(!g.cell(0, 0).faint);
        assert!(g.cell(0, 1).faint);
        assert!(g.cell(0, 2).faint);
        assert!(!g.cell(0, 3).faint);
        assert!(g.cell(0, 4).faint);
        assert_eq!(g.cell(0, 4).fg, 1);
        assert!(!g.cell(0, 5).faint);
        assert_eq!(g.cell(0, 5).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_overline_marks_later_cells_until_reset() {
        let g = grid_feed(1, 12, b"A\x1b[53mBC\x1b[55mD\x1b[53;31mE\x1b[0mF");
        assert!(!g.cell(0, 0).overline);
        assert!(g.cell(0, 1).overline);
        assert!(g.cell(0, 2).overline);
        assert!(!g.cell(0, 3).overline);
        assert!(g.cell(0, 4).overline);
        assert_eq!(g.cell(0, 4).fg, 1);
        assert!(!g.cell(0, 5).overline);
        assert_eq!(g.cell(0, 5).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_conceal_marks_later_cells_until_reset() {
        let g = grid_feed(1, 16, b"A\x1b[8mSECRET\x1b[28mB\x1b[8;31mC\x1b[0mD");
        assert_eq!(g.to_text(), "ASECRETBCD      ");
        assert!(!g.cell(0, 0).conceal);
        for col in 1..7 {
            assert!(g.cell(0, col).conceal);
        }
        assert!(!g.cell(0, 7).conceal);
        assert!(g.cell(0, 8).conceal);
        assert_eq!(g.cell(0, 8).fg, 1);
        assert!(!g.cell(0, 9).conceal);
        assert_eq!(g.cell(0, 9).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_blink_marks_later_cells_until_reset() {
        let g = grid_feed(1, 12, b"A\x1b[5mBC\x1b[25mD\x1b[6;31mE\x1b[0mF");
        assert!(!g.cell(0, 0).blink);
        assert!(g.cell(0, 1).blink);
        assert!(g.cell(0, 2).blink);
        assert!(!g.cell(0, 3).blink);
        assert!(g.cell(0, 4).blink);
        assert_eq!(g.cell(0, 4).fg, 1);
        assert!(!g.cell(0, 5).blink);
        assert_eq!(g.cell(0, 5).fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_intensity_reset_clears_bold_and_faint() {
        let g = grid_feed(1, 8, b"\x1b[31;1;2mA\x1b[22mB");
        assert_eq!(g.cell(0, 0).fg, 9);
        assert!(g.cell(0, 0).faint);
        assert_eq!(g.cell(0, 1).fg, 1);
        assert!(!g.cell(0, 1).faint);
    }

    #[test]
    fn sgr_background_colors_and_resets() {
        let g = grid_feed(2, 10, b"\x1b[44mA\x1b[49mB\x1b[104mC\x1b[0mD");
        assert_eq!(g.cell(0, 0).bg, 4);
        assert_eq!(g.cell(0, 1).bg, DEFAULT_BG);
        assert_eq!(g.cell(0, 2).bg, 12);
        assert_eq!(g.cell(0, 3).bg, DEFAULT_BG);

        let g2 = grid_feed(1, 8, b"\x1b[31;42mX\x1b[0mY");
        assert_eq!(g2.cell(0, 0).fg, 1);
        assert_eq!(g2.cell(0, 0).bg, 2);
        assert_eq!(g2.cell(0, 1).fg, DEFAULT_FG);
        assert_eq!(g2.cell(0, 1).bg, DEFAULT_BG);
    }

    #[test]
    fn csi_rep_repeats_previous_graphic_cell() {
        let g = grid_feed(1, 10, b"\x1b[31mA\x1b[32m\x1b[3bZ");
        assert_eq!(g.to_text(), "AAAAZ     ");
        for col in 0..4 {
            assert_eq!(g.cell(0, col).fg, 1, "REP should preserve previous cell fg");
        }
        assert_eq!(g.cell(0, 4).fg, 2, "current SGR should still affect later text");
        assert!(!g.contains("3b"));

        let g2 = grid_feed(1, 8, b"Q\x1b[b\x1b[0b!");
        assert_eq!(g2.to_text(), "QQQ!    ");

        let g3 = grid_feed(1, 8, b"\x1b[4bX");
        assert_eq!(g3.to_text(), "X       ");
        assert!(!g3.contains("4b"));

        let g4 = grid_feed(1, 8, b"A\x1bc\x1b[2bZ");
        assert_eq!(g4.to_text(), "Z       ");
        assert!(!g4.contains("2b"));

        let g5 = grid_feed(1, 8, b"abcdef\x1b[1;3HA\x1b[4h\x1b[2b");
        assert_eq!(g5.to_text(), "abAAAdef");
        for col in 2..5 {
            assert_eq!(g5.cell(0, col).fg, DEFAULT_FG);
        }

        let g6 = grid_feed(2, 3, b"\x1b[4habc\x1b[b");
        assert_eq!(g6.to_text(), "abc\nc  ");

        let g7 = grid_feed(1, 8, b"\x1b[4mA\x1b[2b\x1b[24mZ");
        assert_eq!(g7.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g7.cell(0, col).underline);
        }
        assert!(!g7.cell(0, 3).underline);

        let g8 = grid_feed(1, 8, b"\x1b[9mA\x1b[2b\x1b[29mZ");
        assert_eq!(g8.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g8.cell(0, col).strikethrough);
        }
        assert!(!g8.cell(0, 3).strikethrough);

        let g9 = grid_feed(1, 8, b"\x1b[3mA\x1b[2b\x1b[23mZ");
        assert_eq!(g9.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g9.cell(0, col).italic);
        }
        assert!(!g9.cell(0, 3).italic);

        let g10 = grid_feed(1, 8, b"\x1b[2mA\x1b[2b\x1b[22mZ");
        assert_eq!(g10.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g10.cell(0, col).faint);
        }
        assert!(!g10.cell(0, 3).faint);

        let g11 = grid_feed(1, 8, b"\x1b[53mA\x1b[2b\x1b[55mZ");
        assert_eq!(g11.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g11.cell(0, col).overline);
        }
        assert!(!g11.cell(0, 3).overline);

        let g12 = grid_feed(1, 8, b"\x1b[8mA\x1b[2b\x1b[28mZ");
        assert_eq!(g12.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g12.cell(0, col).conceal);
        }
        assert!(!g12.cell(0, 3).conceal);

        let g13 = grid_feed(1, 8, b"\x1b[5mA\x1b[2b\x1b[25mZ");
        assert_eq!(g13.to_text(), "AAAZ    ");
        for col in 0..3 {
            assert!(g13.cell(0, col).blink);
        }
        assert!(!g13.cell(0, 3).blink);
    }

    #[test]
    fn sgr_256_color_foreground_and_background() {
        let g = grid_feed(1, 8, b"\x1b[38;5;196mR\x1b[48;5;22mB\x1b[0mZ");
        assert_eq!(g.cell(0, 0).fg, 196);
        assert_eq!(g.cell(0, 0).bg, DEFAULT_BG);
        assert_eq!(g.cell(0, 1).fg, 196);
        assert_eq!(g.cell(0, 1).bg, 22);
        assert_eq!(g.cell(0, 2).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 2).bg, DEFAULT_BG);
    }

    #[test]
    fn sgr_colon_256_color_foreground_and_background() {
        let g = grid_feed(1, 8, b"\x1b[38:5:196mR\x1b[48:5:22mB\x1b[0mZ");
        assert_eq!(g.cell(0, 0).fg, 196);
        assert_eq!(g.cell(0, 0).bg, DEFAULT_BG);
        assert_eq!(g.cell(0, 1).fg, 196);
        assert_eq!(g.cell(0, 1).bg, 22);
        assert_eq!(g.cell(0, 2).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 2).bg, DEFAULT_BG);
    }

    #[test]
    fn sgr_256_color_index_255_is_not_a_sentinel() {
        let g = grid_feed(1, 4, b"\x1b[38;5;255mX\x1b[48;5;255mY");
        assert_eq!(g.cell(0, 0).fg, 255);
        assert_eq!(g.cell(0, 1).bg, 255);
        assert!(background_rgba(255).is_some());
        assert_ne!(DEFAULT_FG, 255);
        assert_ne!(DEFAULT_BG, 255);
    }

    #[test]
    fn sgr_truecolor_foreground_and_background() {
        let g = grid_feed(
            1,
            8,
            b"\x1b[31mA\x1b[38;2;1;2;3mB\x1b[48;2;4;5;6mC",
        );
        assert_eq!(g.cell(0, 0).fg, 1);
        assert_eq!(g.cell(0, 1).fg, encode_truecolor(1, 2, 3));
        assert_eq!(g.cell(0, 1).bg, DEFAULT_BG);
        assert_eq!(g.cell(0, 2).fg, encode_truecolor(1, 2, 3));
        assert_eq!(g.cell(0, 2).bg, encode_truecolor(4, 5, 6));
    }

    #[test]
    fn sgr_colon_truecolor_foreground_and_background() {
        let g = grid_feed(
            1,
            8,
            b"\x1b[38:2::1:2:3mA\x1b[48:2:4:5:6mB\x1b[0mC",
        );
        assert_eq!(g.cell(0, 0).fg, encode_truecolor(1, 2, 3));
        assert_eq!(g.cell(0, 0).bg, DEFAULT_BG);
        assert_eq!(g.cell(0, 1).fg, encode_truecolor(1, 2, 3));
        assert_eq!(g.cell(0, 1).bg, encode_truecolor(4, 5, 6));
        assert_eq!(g.cell(0, 2).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 2).bg, DEFAULT_BG);

        let g2 = grid_feed(1, 8, b"\x1b[38:2:1:7:8:9mX");
        assert_eq!(g2.cell(0, 0).fg, encode_truecolor(7, 8, 9));
    }

    #[test]
    fn invalid_truecolor_is_consumed_without_side_effects() {
        let g = grid_feed(1, 8, b"\x1b[31mA\x1b[38;2;1;2;300mB");
        assert_eq!(g.cell(0, 0).fg, 1);
        assert_eq!(g.cell(0, 1).fg, 1);
        assert_eq!(g.cell(0, 1).bg, DEFAULT_BG);
    }

    #[test]
    fn terminal_colors_resolve_palette_cube_grayscale_and_truecolor() {
        assert_eq!(palette_rgba(DEFAULT_FG), (0.82, 0.84, 0.88, 1.0));
        assert_eq!(palette_rgba(196), (1.0, 0.0, 0.0, 1.0));
        let gray = 238.0 / 255.0;
        assert_eq!(palette_rgba(255), (gray, gray, gray, 1.0));
        assert_eq!(
            palette_rgba(encode_truecolor(12, 34, 56)),
            (12.0 / 255.0, 34.0 / 255.0, 56.0 / 255.0, 1.0)
        );
    }

    #[test]
    fn sgr_compound_params_ignored_safely() {
        // Bold + fg color: "1;33" -> bright yellow (11) applied.
        let g = grid_feed(2, 10, b"\x1b[1;33mY");
        assert_eq!(g.cell(0, 0).ch, 'Y');
        assert_eq!(g.cell(0, 0).fg, 11);

        let g2 = grid_feed(1, 8, b"\x1b[1;33;45mZ");
        assert_eq!(g2.cell(0, 0).fg, 11);
        assert_eq!(g2.cell(0, 0).bg, 5);
    }

    #[test]
    fn erase_display_clears_screen_without_garbage() {
        let g = grid_feed(2, 10, b"junk\x1b[2JOK");
        // The old content and "2J" escape bytes must not appear; only OK prints.
        assert_eq!(g.cell(0, 0).ch, 'O');
        assert_eq!(g.cell(0, 1).ch, 'K');
        assert!(!g.contains("junk"));
        assert!(!g.contains("2J"));
        assert!(!g.contains("["));
    }

    #[test]
    fn erase_display_default_clears_from_cursor_to_end() {
        let g = grid_feed(2, 10, b"abc\x1b[JZ");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'b');
        assert_eq!(g.cell(0, 2).ch, 'c');
        assert_eq!(g.cell(0, 3).ch, 'Z');
        assert_eq!(g.cell(0, 4).ch, ' ');
    }

    #[test]
    fn erase_display_scrollback_mode_does_not_clear_visible_grid() {
        let g = grid_feed(2, 10, b"abc\x1b[3JZ");
        assert_eq!(g.to_text(), "abcZ      \n          ");
        assert!(!g.contains("3J"));
    }

    #[test]
    fn erase_line_modes_are_row_local() {
        let g = grid_feed(2, 8, b"abcdef\nQRSTUV\x1b[1;4H\x1b[KZ");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'b');
        assert_eq!(g.cell(0, 2).ch, 'c');
        assert_eq!(g.cell(0, 3).ch, 'Z');
        assert_eq!(g.cell(0, 4).ch, ' ');
        assert_eq!(g.cell(1, 0).ch, 'Q', "row below should not be erased");

        let g2 = grid_feed(1, 8, b"abcdef\x1b[1;4H\x1b[1KZ");
        assert_eq!(g2.cell(0, 0).ch, ' ');
        assert_eq!(g2.cell(0, 1).ch, ' ');
        assert_eq!(g2.cell(0, 2).ch, ' ');
        assert_eq!(g2.cell(0, 3).ch, 'Z');
        assert_eq!(g2.cell(0, 4).ch, 'e');

        let g3 = grid_feed(1, 8, b"abcdef\x1b[1;4H\x1b[2KZ");
        assert_eq!(g3.cell(0, 0).ch, ' ');
        assert_eq!(g3.cell(0, 3).ch, 'Z');
        assert_eq!(g3.cell(0, 4).ch, ' ');
    }

    #[test]
    fn insert_chars_csi_shifts_current_row_right() {
        let g = grid_feed(2, 8, b"abcdef\nQRSTUV\x1b[1;3H\x1b[2@XY");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'b');
        assert_eq!(g.cell(0, 2).ch, 'X');
        assert_eq!(g.cell(0, 3).ch, 'Y');
        assert_eq!(g.cell(0, 4).ch, 'c');
        assert_eq!(g.cell(0, 7).ch, 'f');
        assert_eq!(g.cell(1, 0).ch, 'Q', "row below should not shift");
        assert!(!g.contains("2@"));

        let g2 = grid_feed(1, 6, b"abcd\x1b[1;3H\x1b[@Z");
        assert_eq!(g2.to_text(), "abZcd ");
    }

    #[test]
    fn insert_mode_shifts_printable_output_until_reset() {
        let g = grid_feed(2, 8, b"abcdef\nQRSTUV\x1b[1;3H\x1b[4hXY\x1b[4lZ");
        assert_eq!(g.to_text(), "abXYZdef\nQRSTUV  ");
        assert_eq!(g.cursor(), (0, 5));
        assert_eq!(g.cell(1, 0).ch, 'Q', "row below should not shift");
        assert!(!g.contains("[4h"));
        assert!(!g.contains("[4l"));

        let g2 = grid_feed(1, 8, b"abcd\x1b[1;3H\x1b[4h\xc3\xa9!");
        assert_eq!(g2.to_text(), "abé!cd  ");

        let g3 = grid_feed(1, 8, b"abcdef\x1b[1;3H\x1b[4hX\x1b[!p\x1b[1;4HY");
        assert_eq!(g3.to_text(), "abXYdef ");
        assert!(!g3.contains("!p"));

        let g4 = grid_feed(2, 3, b"\x1b[4habcX");
        assert_eq!(g4.to_text(), "abc\nX  ");
        assert_eq!(g4.cursor(), (1, 1));
    }

    #[test]
    fn delete_chars_csi_shifts_current_row_left() {
        let g = grid_feed(2, 8, b"abcdef\nQRSTUV\x1b[1;3H\x1b[2P");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'b');
        assert_eq!(g.cell(0, 2).ch, 'e');
        assert_eq!(g.cell(0, 3).ch, 'f');
        assert_eq!(g.cell(0, 4).ch, ' ');
        assert_eq!(g.cell(1, 0).ch, 'Q', "row below should not shift");
        assert!(!g.contains("2P"));

        let g2 = grid_feed(1, 8, b"abcdef\x1b[1;5H\x1b[99P");
        assert_eq!(g2.to_text(), "abcd    ");
    }

    #[test]
    fn erase_chars_csi_blanks_without_shifting_row() {
        let g = grid_feed(2, 8, b"abcdef\nQRSTUV\x1b[1;3H\x1b[2X");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'b');
        assert_eq!(g.cell(0, 2).ch, ' ');
        assert_eq!(g.cell(0, 3).ch, ' ');
        assert_eq!(g.cell(0, 4).ch, 'e', "tail should not shift left");
        assert_eq!(g.cell(1, 0).ch, 'Q', "row below should not be erased");
        assert!(!g.contains("2X"));

        let g2 = grid_feed(1, 8, b"abcdef\x1b[1;5H\x1b[99X");
        assert_eq!(g2.to_text(), "abcd    ");
    }

    #[test]
    fn insert_lines_csi_shifts_rows_down_from_cursor() {
        let g = grid_feed(4, 4, b"aaaa\nbbbb\ncccc\ndddd\x1b[2;1H\x1b[2L");
        assert_eq!(g.to_text(), "aaaa\n    \n    \nbbbb");
        assert!(!g.contains("2L"));

        let g2 = grid_feed(3, 4, b"aaaa\nbbbb\ncccc\x1b[2;1H\x1b[LZ");
        assert_eq!(g2.cell(0, 0).ch, 'a');
        assert_eq!(g2.cell(1, 0).ch, 'Z');
        assert_eq!(g2.cell(2, 0).ch, 'b');
    }

    #[test]
    fn delete_lines_csi_shifts_rows_up_from_cursor() {
        let g = grid_feed(4, 4, b"aaaa\nbbbb\ncccc\ndddd\x1b[2;1H\x1b[2M");
        assert_eq!(g.to_text(), "aaaa\ndddd\n    \n    ");
        assert!(!g.contains("2M"));

        let g2 = grid_feed(3, 4, b"aaaa\nbbbb\ncccc\x1b[2;1H\x1b[99M");
        assert_eq!(g2.to_text(), "aaaa\n    \n    ");
    }

    #[test]
    fn scroll_up_csi_shifts_viewport_and_blanks_bottom() {
        let g = grid_feed(4, 4, b"aaaa\nbbbb\ncccc\ndddd\x1b[2S");
        assert_eq!(g.to_text(), "cccc\ndddd\n    \n    ");
        assert!(!g.contains("2S"));

        let g2 = grid_feed(3, 4, b"aaaa\nbbbb\ncccc\x1b[S");
        assert_eq!(g2.to_text(), "bbbb\ncccc\n    ");
    }

    #[test]
    fn scroll_down_csi_shifts_viewport_and_blanks_top() {
        let g = grid_feed(4, 4, b"aaaa\nbbbb\ncccc\ndddd\x1b[2T");
        assert_eq!(g.to_text(), "    \n    \naaaa\nbbbb");
        assert!(!g.contains("2T"));

        let g2 = grid_feed(3, 4, b"aaaa\nbbbb\ncccc\x1b[99T");
        assert_eq!(g2.to_text(), "    \n    \n    ");

        let g3 = grid_feed(4, 4, b"aaaa\nbbbb\ncccc\ndddd\x1b[2^");
        assert_eq!(g3.to_text(), "    \n    \naaaa\nbbbb");
        assert!(!g3.contains("2^"));
    }

    #[test]
    fn scroll_region_linefeed_preserves_rows_outside_margins() {
        let g = grid_feed(
            4,
            4,
            b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[3;1HZZZZ\nYYYY",
        );
        assert_eq!(g.to_text(), "1111\nZZZZ\nYYYY\n4444");
        assert_eq!(g.cursor(), (2, 3));
        assert!(!g.contains("2;3r"));
    }

    #[test]
    fn scroll_region_limits_explicit_scroll_commands() {
        let g = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[S");
        assert_eq!(g.to_text(), "1111\n3333\n    \n4444");

        let g2 = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[T");
        assert_eq!(g2.to_text(), "1111\n    \n2222\n4444");

        let g3 = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[^");
        assert_eq!(g3.to_text(), "1111\n    \n2222\n4444");
    }

    #[test]
    fn origin_mode_makes_cursor_position_relative_to_scroll_region() {
        let g = grid_feed(5, 6, b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[1;3HX");
        assert_eq!(g.cell(0, 2).ch, 'a', "row above margin is preserved");
        assert_eq!(g.cell(1, 2).ch, 'X');
        assert_eq!(g.cursor(), (1, 3));
        assert!(!g.contains("?6h"));

        let g2 = grid_feed(5, 6, b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[99;2HY");
        assert_eq!(g2.cell(3, 1).ch, 'Y', "origin-mode rows clamp to bottom margin");
    }

    #[test]
    fn origin_mode_scroll_region_changes_home_to_top_margin() {
        let g = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[?6h\x1b[2;4rX",
        );
        assert_eq!(g.cell(1, 0).ch, 'X');
        assert_eq!(g.cursor(), (1, 1));
        assert!(!g.contains("2;4r"));
    }

    #[test]
    fn invalid_scroll_region_does_not_rehome_origin_mode_cursor() {
        let g = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[2;3HX\x1b[4;2rY",
        );
        assert_eq!(g.cell(2, 2).ch, 'X');
        assert_eq!(g.cell(2, 3).ch, 'Y');
        assert_eq!(g.cursor(), (2, 4));
        assert!(!g.contains("4;2r"));
    }

    #[test]
    fn origin_mode_makes_vpa_relative_to_scroll_region() {
        let g = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[3;3HX\x1b[1dY\x1b[99dZ",
        );
        assert_eq!(g.cell(1, 3).ch, 'Y', "VPA row 1 maps to top margin");
        assert_eq!(g.cell(3, 4).ch, 'Z', "VPA clamps to bottom margin");
        assert!(!g.contains("99d"));
    }

    #[test]
    fn resetting_origin_mode_returns_cursor_position_to_absolute_rows() {
        let g = grid_feed(5, 6, b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[?6l\x1b[1;3HZ");
        assert_eq!(g.cell(0, 2).ch, 'Z');
        assert_eq!(g.cell(1, 2).ch, 'b');
        assert_eq!(g.cursor(), (0, 3));
        assert!(!g.contains("?6l"));
    }

    #[test]
    fn origin_mode_clamps_relative_vertical_moves_to_scroll_region() {
        let g = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[1;1HX\x1b[99AY\x1b[99BZ",
        );
        assert_eq!(g.cell(1, 1).ch, 'Y', "CUU clamps to the top margin");
        assert_eq!(g.cell(3, 2).ch, 'Z', "CUD clamps to the bottom margin");
        assert!(!g.contains("99A"));
        assert!(!g.contains("99B"));

        let g2 = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[2;4HX\x1b[99eY",
        );
        assert_eq!(g2.cell(3, 4).ch, 'Y', "VPR clamps to the bottom margin");
        assert!(!g2.contains("99e"));
    }

    #[test]
    fn origin_mode_clamps_relative_line_moves_to_scroll_region() {
        let g = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b[2;4HX\x1b[99EY\x1b[99FZ",
        );
        assert_eq!(g.cell(3, 0).ch, 'Y', "CNL clamps to the bottom margin");
        assert_eq!(g.cell(1, 0).ch, 'Z', "CPL clamps to the top margin");
        assert!(!g.contains("99E"));
        assert!(!g.contains("99F"));
    }

    #[test]
    fn single_escape_index_sequences_move_cursor_without_garbage() {
        let g = grid_feed(3, 6, b"aa\nbb\ncc\x1b[1;3H\x1bDID");
        assert_eq!(g.to_text(), "aa    \nbbID  \ncc    ");
        assert!(!g.contains("[D"));

        let g2 = grid_feed(3, 6, b"aa\nbb\ncc\x1b[1;5H\x1bEN");
        assert_eq!(g2.to_text(), "aa    \nNb    \ncc    ");

        let g3 = grid_feed(3, 6, b"aa\nbb\ncc\x1b[2;3H\x1bMR");
        assert_eq!(g3.to_text(), "aaR   \nbb    \ncc    ");
    }

    #[test]
    fn esc_intermediate_charset_selects_do_not_leak_final_bytes() {
        let g = grid_feed(1, 8, b"A\x1b(BZ");
        assert_eq!(g.to_text(), "AZ      ");
        assert!(!g.contains("(B"));
    }

    #[test]
    fn dec_special_graphics_charset_draws_tui_borders() {
        let g = grid_feed(2, 12, b"\x1b(0lqk\x1b(B abc");
        assert_eq!(g.cell(0, 0).ch, '┌');
        assert_eq!(g.cell(0, 1).ch, '─');
        assert_eq!(g.cell(0, 2).ch, '┐');
        assert_eq!(g.cell(0, 4).ch, 'a');
        assert!(!g.contains("(0"));

        let g2 = grid_feed(1, 12, b"\x1b)0\x0elqk\x0flqk");
        assert_eq!(g2.cell(0, 0).ch, '┌');
        assert_eq!(g2.cell(0, 1).ch, '─');
        assert_eq!(g2.cell(0, 2).ch, '┐');
        assert_eq!(g2.cell(0, 3).ch, 'l');
        assert_eq!(g2.cell(0, 4).ch, 'q');
        assert_eq!(g2.cell(0, 5).ch, 'k');
    }

    #[test]
    fn charset_state_is_saved_restored_and_reset() {
        let g = grid_feed(1, 12, b"\x1b(0\x1b7\x1b(B\x1b8q");
        assert_eq!(g.cell(0, 0).ch, '─');

        let g2 = grid_feed(1, 12, b"\x1b(0\x1b[!pq");
        assert_eq!(g2.cell(0, 0).ch, 'q');
        assert!(!g2.contains("!p"));

        let g3 = grid_feed(1, 12, b"\x1b(0\x1bcq");
        assert_eq!(g3.cell(0, 0).ch, 'q');
    }

    #[test]
    fn esc_screen_alignment_fills_grid_without_leaking_sequence() {
        let g = grid_feed(2, 4, b"abc\x1b#8");
        assert_eq!(g.to_text(), "EEEE\nEEEE");
        assert!(!g.contains("#8"));
    }

    #[test]
    fn single_escape_index_sequences_respect_scroll_region_margins() {
        let g = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[2;1H\x1bMZ");
        assert_eq!(g.to_text(), "1111\nZ   \n2222\n4444");

        let g2 = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[3;2H\x1bDq");
        assert_eq!(g2.to_text(), "1111\n3333\n q  \n4444");
    }

    #[test]
    fn scroll_region_limits_insert_and_delete_lines() {
        let g = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[2;1H\x1b[L");
        assert_eq!(g.to_text(), "1111\n    \n2222\n4444");

        let g2 = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[2;1H\x1b[M");
        assert_eq!(g2.to_text(), "1111\n3333\n    \n4444");

        let g3 = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[4;1H\x1b[L");
        assert_eq!(g3.to_text(), "1111\n2222\n3333\n4444");
    }

    #[test]
    fn scroll_region_resets_to_full_grid_with_bare_csi_r() {
        let g = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[r\x1b[S");
        assert_eq!(g.to_text(), "2222\n3333\n4444\n    ");
        assert_eq!(g.cursor(), (0, 0));
    }

    #[test]
    fn valid_scroll_region_homes_cursor_in_absolute_mode() {
        let g = grid_feed(4, 6, b"aaaaaa\nbbbbbb\ncccccc\ndddddd\x1b[4;6H@\x1b[2;3rX");
        assert_eq!(g.cell(0, 0).ch, 'X');
        assert_eq!(g.cell(3, 5).ch, '@');
        assert_eq!(g.cursor(), (0, 1));
        assert!(!g.contains("2;3r"));
    }

    #[test]
    fn cursor_position_csi_moves_and_clamps() {
        // ESC[5;10H moves to a 1-based row/col and clamps to the visible grid.
        let g = grid_feed(2, 20, b"A\x1b[5;10HB");
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cell(1, 9).ch, 'B');
        assert!(!g.contains("5;10H"));

        // Bare H homes the cursor; empty row defaults to 1.
        let g2 = grid_feed(2, 20, b"abc\x1b[HXY");
        assert_eq!(g2.cell(0, 0).ch, 'X');
        assert_eq!(g2.cell(0, 1).ch, 'Y');

        let g3 = grid_feed(3, 20, b"\x1b[;4fZ");
        assert_eq!(g3.cell(0, 3).ch, 'Z');
    }

    #[test]
    fn cursor_relative_csi_moves_and_clamps() {
        let g = grid_feed(3, 8, b"\x1b[2;3H@\x1b[AU\x1b[2BZ\x1b[2D<\x1b[20C>");
        assert_eq!(g.cell(1, 2).ch, '@');
        assert_eq!(g.cell(0, 3).ch, 'U');
        assert_eq!(g.cell(2, 4).ch, 'Z');
        assert_eq!(g.cell(2, 3).ch, '<');
        assert_eq!(g.cell(2, 7).ch, '>');
        assert!(!g.contains("20C"));

        let g2 = grid_feed(2, 8, b"\x1b[2;2HX\x1b[D<\x1b[99D[\x1b[99A^");
        assert_eq!(g2.cell(1, 1).ch, '<');
        assert_eq!(g2.cell(1, 0).ch, '[');
        assert_eq!(g2.cell(0, 1).ch, '^');

        let g3 = grid_feed(1, 8, b"A\x1b[3aB\x1b[20aC");
        assert_eq!(g3.cell(0, 0).ch, 'A');
        assert_eq!(g3.cell(0, 4).ch, 'B');
        assert_eq!(g3.cell(0, 7).ch, 'C');
        assert!(!g3.contains("3a"));

        let g4 = grid_feed(3, 8, b"\x1b[3;6H@\x1b[2jL\x1b[2kU");
        assert_eq!(g4.cell(2, 5).ch, '@');
        assert_eq!(g4.cell(2, 4).ch, 'L');
        assert_eq!(g4.cell(0, 5).ch, 'U');
        assert!(!g4.contains("2j"));
        assert!(!g4.contains("2k"));
    }

    #[test]
    fn cursor_save_restore_sequences_return_to_saved_cell() {
        let g = grid_feed(2, 8, b"A\x1b7BC\x1b8ZD");
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cell(0, 1).ch, 'Z');
        assert_eq!(g.cell(0, 2).ch, 'D');
        assert!(!g.contains("7"));
        assert!(!g.contains("8"));

        let g2 = grid_feed(3, 8, b"\x1b[2;4H@\x1b[sab\x1b[uX");
        assert_eq!(g2.cell(1, 3).ch, '@');
        assert_eq!(g2.cell(1, 4).ch, 'X');
        assert_eq!(g2.cell(1, 5).ch, 'b');
        assert!(!g2.contains("[s"));
        assert!(!g2.contains("[u"));
    }

    #[test]
    fn cursor_save_restore_sequences_restore_sgr_colors() {
        let g = grid_feed(1, 8, b"\x1b[31mA\x1b7\x1b[32mB\x1b8C");
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cell(0, 0).fg, 1);
        assert_eq!(g.cell(0, 1).ch, 'C');
        assert_eq!(g.cell(0, 1).fg, 1);

        let g2 = grid_feed(1, 8, b"\x1b[44mA\x1b[s\x1b[45mB\x1b[uC");
        assert_eq!(g2.cell(0, 0).ch, 'A');
        assert_eq!(g2.cell(0, 0).bg, 4);
        assert_eq!(g2.cell(0, 1).ch, 'C');
        assert_eq!(g2.cell(0, 1).bg, 4);

        let g3 = grid_feed(
            1,
            8,
            b"\x1b[33;46mA\x1b[?1048h\x1b[31;44mB\x1b[?1048lC",
        );
        assert_eq!(g3.cell(0, 0).ch, 'A');
        assert_eq!(g3.cell(0, 0).fg, 3);
        assert_eq!(g3.cell(0, 0).bg, 6);
        assert_eq!(g3.cell(0, 1).ch, 'C');
        assert_eq!(g3.cell(0, 1).fg, 3);
        assert_eq!(g3.cell(0, 1).bg, 6);
        assert!(!g3.contains("1048"));
    }

    #[test]
    fn cursor_save_restore_sequences_restore_terminal_modes() {
        let g = grid_feed(2, 3, b"\x1b[?7l\x1b7\x1b[?7h\x1b8abcd");
        assert_eq!(g.to_text(), "abd\n   ");
        assert_eq!(g.cursor(), (0, 2));
        assert!(!g.contains("?7"));

        let g2 = grid_feed(
            5,
            6,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\neeeeee\x1b[2;4r\x1b[?6h\x1b7\x1b[?6l\x1b8\x1b[1;3HX",
        );
        assert_eq!(g2.cell(0, 2).ch, 'a', "absolute row 1 should be untouched");
        assert_eq!(g2.cell(1, 2).ch, 'X', "CUP should be relative after restore");
        assert_eq!(g2.cursor(), (1, 3));
        assert!(!g2.contains("?6"));

        let g3 = grid_feed(1, 8, b"abcdef\x1b[1;3H\x1b[4h\x1b7\x1b[4l\x1b8X");
        assert_eq!(g3.to_text(), "abXcdef ");
        assert!(!g3.contains("[4"));

        let g4 = grid_feed(1, 8, b"abcdef\x1b[1;3H\x1b[4l\x1b[s\x1b[4h\x1b[uX");
        assert_eq!(g4.to_text(), "abXdef  ");
        assert!(!g4.contains("[4"));

        let g5 = grid_feed(3, 8, b"ab\x1b[20l\x1b7\x1b[20h\x1b8\ncd");
        assert_eq!(g5.to_text(), "ab      \n  cd    \n        ");
        assert!(!g5.contains("20"));

        let g6 = grid_feed(1, 8, b"\x1b[31;1m\x1b7\x1b[22m\x1b8X");
        assert_eq!(g6.cell(0, 0).fg, 9);
        assert!(!g6.contains("[22"));

        let g7 = grid_feed(1, 8, b"\x1b[31;44;7m\x1b7\x1b[27m\x1b8X");
        assert_eq!(g7.cell(0, 0).fg, 4);
        assert_eq!(g7.cell(0, 0).bg, 1);
        assert!(!g7.contains("[27"));

        let g8 = grid_feed(1, 8, b"\x1b[4m\x1b7\x1b[24m\x1b8X");
        assert!(g8.cell(0, 0).underline);
        assert!(!g8.contains("[24"));

        let g9 = grid_feed(1, 8, b"\x1b[9m\x1b7\x1b[29m\x1b8X");
        assert!(g9.cell(0, 0).strikethrough);
        assert!(!g9.contains("[29"));

        let g10 = grid_feed(1, 8, b"\x1b[3m\x1b7\x1b[23m\x1b8X");
        assert!(g10.cell(0, 0).italic);
        assert!(!g10.contains("[23"));

        let g11 = grid_feed(1, 8, b"\x1b[2m\x1b7\x1b[22m\x1b8X");
        assert!(g11.cell(0, 0).faint);
        assert!(!g11.contains("[22"));

        let g12 = grid_feed(1, 8, b"\x1b[53m\x1b7\x1b[55m\x1b8X");
        assert!(g12.cell(0, 0).overline);
        assert!(!g12.contains("[55"));

        let g13 = grid_feed(1, 8, b"\x1b[8m\x1b7\x1b[28m\x1b8X");
        assert!(g13.cell(0, 0).conceal);
        assert!(!g13.contains("[28"));

        let g14 = grid_feed(1, 8, b"\x1b[5m\x1b7\x1b[25m\x1b8X");
        assert!(g14.cell(0, 0).blink);
        assert!(!g14.contains("[25"));
    }

    #[test]
    fn cursor_save_restore_sequences_restore_cursor_attributes() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();

        p.feed(&mut g, b"\x1b[1 q\x1b7\x1b[?12l\x1b[?25l\x1b[6 q\x1b8");
        assert!(p.cursor_blinking());
        assert!(p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Block);

        p.feed(&mut g, b"\x1b[?12l\x1b[?25l\x1b[6 q\x1b[s\x1b[?12h\x1b[?25h\x1b[4 q\x1b[u");
        assert!(!p.cursor_blinking());
        assert!(!p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
        assert!(!g.contains("?25"));
    }

    #[test]
    fn alternate_screen_restores_primary_grid_and_cursor() {
        let g = grid_feed(2, 10, b"prompt\x1b[?1049hALT\x1b[?1049l!");
        assert_eq!(g.cell(0, 0).ch, 'p');
        assert_eq!(g.cell(0, 5).ch, 't');
        assert_eq!(g.cell(0, 6).ch, '!');
        assert_eq!(g.cursor(), (0, 7));
        assert!(!g.contains("ALT"));
        assert!(!g.contains("1049"));
    }

    #[test]
    fn alternate_screen_restores_primary_sgr_colors() {
        let g = grid_feed(1, 12, b"\x1b[31;44mP\x1b[?1049h\x1b[32;45mA\x1b[?1049lX");
        assert_eq!(g.cell(0, 0).ch, 'P');
        assert_eq!(g.cell(0, 0).fg, 1);
        assert_eq!(g.cell(0, 0).bg, 4);
        assert_eq!(g.cell(0, 1).ch, 'X');
        assert_eq!(g.cell(0, 1).fg, 1);
        assert_eq!(g.cell(0, 1).bg, 4);
        assert!(!g.contains("A"));
        assert!(!g.contains("1049"));
    }

    #[test]
    fn alternate_screen_1049_restores_cursor_attributes() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();

        p.feed(&mut g, b"\x1b[?25h\x1b[1 q\x1b[?1049h\x1b[?12l\x1b[?25l\x1b[6 q\x1b[?1049l");
        assert!(p.cursor_blinking());
        assert!(p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(!g.contains("1049"));

        p.feed(&mut g, b"\x1b[?12l\x1b[?25l\x1b[6 q\x1b7\x1b[?25h\x1b[1 q\x1b[?1049h\x1b[?12l\x1b[?25l\x1b[6 q\x1b[?1049l");
        assert!(p.cursor_blinking());
        assert!(p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Block);

        p.feed(&mut g, b"\x1b8");
        assert!(!p.cursor_blinking());
        assert!(!p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
    }

    #[test]
    fn alternate_screen_1049_restores_insert_mode() {
        let g = grid_feed(1, 8, b"abcdef\x1b[1;3H\x1b[?1049h\x1b[4h\x1b[?1049lX");
        assert_eq!(g.to_text(), "abXdef  ");
        assert!(!g.contains("1049"));
        assert!(!g.contains("[4h"));

        let g2 = grid_feed(1, 8, b"abcdef\x1b[1;3H\x1b[4h\x1b[?1049h\x1b[4l\x1b[?1049lX");
        assert_eq!(g2.to_text(), "abXcdef ");
    }

    #[test]
    fn alternate_screen_restore_survives_resize() {
        let mut g = Grid::new(2, 6);
        let mut p = VtParser::new();
        p.feed(&mut g, b"ABC\nDEF\x1b[?1047hALT");
        g.resize(3, 8);
        p.feed(&mut g, b"\x1b[?1047l!");

        assert_eq!(g.to_text(), "ABC     \nDEF!    \n        ");
        assert_eq!(g.cursor(), (1, 4));
        assert!(!g.contains("ALT"));
    }

    #[test]
    fn esc_c_inside_alternate_screen_discards_primary_snapshot() {
        let g = grid_feed(2, 10, b"prompt\x1b[?1049hALT\x1bc\x1b[?1049lZ");
        assert_eq!(g.cell(0, 0).ch, 'Z');
        assert!(!g.contains("prompt"));
        assert!(!g.contains("ALT"));
        assert!(!g.contains("1049"));
    }

    #[test]
    fn private_cursor_save_restore_mode_does_not_switch_screens() {
        let g = grid_feed(1, 8, b"A\x1b[?1048hBC\x1b[?1048lZ");
        assert_eq!(g.to_text(), "AZC     ");
        assert!(!g.contains("1048"));
    }

    #[test]
    fn bracketed_paste_mode_tracks_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(!p.bracketed_paste_enabled());

        p.feed(&mut g, b"\x1b[?2004h");
        assert!(p.bracketed_paste_enabled());
        assert!(!g.contains("2004"));

        p.feed(&mut g, b"\x1b[?2004l");
        assert!(!p.bracketed_paste_enabled());
        assert!(!g.contains("2004"));
    }

    #[test]
    fn focus_reporting_mode_tracks_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(!p.focus_reporting_enabled());

        p.feed(&mut g, b"\x1b[?1004h");
        assert!(p.focus_reporting_enabled());
        assert!(!g.contains("1004"));

        p.feed(&mut g, b"\x1b[?1004l");
        assert!(!p.focus_reporting_enabled());
        assert!(!g.contains("1004"));
    }

    #[test]
    fn mouse_reporting_modes_track_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(!p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());

        p.feed(&mut g, b"\x1b[?1000h");
        assert!(p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());
        assert!(!g.contains("1000"));

        p.feed(&mut g, b"\x1b[?1006h");
        assert!(p.sgr_mouse_enabled());
        assert!(!g.contains("1006"));

        p.feed(&mut g, b"\x1b[?1006l");
        assert!(p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());

        p.feed(&mut g, b"\x1b[?1006h\x1b[?1000l");
        assert!(!p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());
    }

    #[test]
    fn mouse_reporting_modes_are_tracked_independently() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();

        p.feed(&mut g, b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        assert!(p.mouse_reporting_enabled());
        assert!(p.sgr_mouse_enabled());
        assert!(p.mouse_drag_reporting_enabled());
        assert!(!p.mouse_any_reporting_enabled());

        p.feed(&mut g, b"\x1b[?1000l");
        assert!(p.mouse_reporting_enabled());
        assert!(p.sgr_mouse_enabled());
        assert!(p.mouse_drag_reporting_enabled());

        p.feed(&mut g, b"\x1b[?1002l");
        assert!(!p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());
        assert!(!p.mouse_drag_reporting_enabled());

        p.feed(&mut g, b"\x1b[?1003h\x1b[?1006l");
        assert!(p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());
        assert!(p.mouse_drag_reporting_enabled());
        assert!(p.mouse_any_reporting_enabled());
        assert!(!g.contains("1003"));
    }

    #[test]
    fn application_cursor_key_mode_tracks_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(!p.application_cursor_keys());

        p.feed(&mut g, b"\x1b[?1h");
        assert!(p.application_cursor_keys());
        assert!(!g.contains("?1h"));

        p.feed(&mut g, b"\x1b[?1l");
        assert!(!p.application_cursor_keys());
        assert!(!g.contains("?1l"));
    }

    #[test]
    fn cursor_visibility_mode_tracks_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(p.cursor_visible());

        p.feed(&mut g, b"\x1b[?25l");
        assert!(!p.cursor_visible());
        assert!(!g.contains("25"));

        p.feed(&mut g, b"\x1b[?25h");
        assert!(p.cursor_visible());
        assert!(!g.contains("25"));
    }

    #[test]
    fn cursor_blink_mode_tracks_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(!p.cursor_blinking());

        p.feed(&mut g, b"\x1b[?12h");
        assert!(p.cursor_blinking());
        assert!(!g.contains("12"));

        p.feed(&mut g, b"\x1b[?12l");
        assert!(!p.cursor_blinking());
        assert!(!g.contains("12"));
    }

    #[test]
    fn cursor_shape_tracks_decscusr() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(!p.cursor_blinking());

        p.feed(&mut g, b"\x1b[3 q");
        assert_eq!(p.cursor_shape(), CursorShape::Underline);
        assert!(p.cursor_blinking());
        assert!(!g.contains("3 q"));

        p.feed(&mut g, b"\x1b[4 q");
        assert_eq!(p.cursor_shape(), CursorShape::Underline);
        assert!(!p.cursor_blinking());
        assert!(!g.contains("4 q"));

        p.feed(&mut g, b"\x1b[5 q");
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
        assert!(p.cursor_blinking());
        assert!(!g.contains("5 q"));

        p.feed(&mut g, b"\x1b[6 q");
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
        assert!(!p.cursor_blinking());
        assert!(!g.contains("6 q"));

        p.feed(&mut g, b"\x1b[1 q");
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(p.cursor_blinking());
        assert!(!g.contains("1 q"));

        p.feed(&mut g, b"\x1b[2 q");
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(!p.cursor_blinking());
        assert!(!g.contains("2 q"));
    }

    #[test]
    fn cursor_shape_ignores_non_decscusr_q_sequences() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[6 q");
        assert_eq!(p.cursor_shape(), CursorShape::Bar);

        p.feed(&mut g, b"\x1b[4q");
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
    }

    #[test]
    fn esc_c_resets_terminal_modes() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b[?1h\x1b[?12h\x1b[?25l\x1b[6 q\x1b[?1004h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1bc",
        );
        assert!(!p.application_cursor_keys());
        assert!(!p.cursor_blinking());
        assert!(p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(!p.focus_reporting_enabled());
        assert!(!p.bracketed_paste_enabled());
        assert!(!p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());
    }

    #[test]
    fn esc_c_resets_sgr_attributes_for_later_text() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[31;44;3;4;5;8;9;53mA\x1bcZ");

        assert_eq!(g.cell(0, 0).ch, 'Z');
        assert_eq!(g.cell(0, 0).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 0).bg, DEFAULT_BG);
        assert!(!g.cell(0, 0).italic);
        assert!(!g.cell(0, 0).underline);
        assert!(!g.cell(0, 0).strikethrough);
        assert!(!g.cell(0, 0).faint);
        assert!(!g.cell(0, 0).overline);
        assert!(!g.cell(0, 0).conceal);
        assert!(!g.cell(0, 0).blink);
        assert!(!g.contains("A"));
    }

    #[test]
    fn esc_c_resets_terminal_identity_state() {
        let mut g = Grid::new(1, 40);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]0;custom title\x07\
              \x1b]10;#010203\x07\
              \x1b]11;#040506\x07\
              \x1b]12;#070809\x07\
              \x1b]4;1;#0a0b0c\x07\
              \x1bc\
              \x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07\x1b]4;1;?\x07Z",
        );

        assert_eq!(p.title(), "");
        assert_eq!(
            p.take_reply(),
            b"\x1b]10;rgb:d1d1/d6d6/e0e0\x1b\\\
              \x1b]11;rgb:1414/1414/1c1c\x1b\\\
              \x1b]12;rgb:7c7c/5c5c/ffff\x1b\\\
              \x1b]4;1;rgb:cccc/4040/4040\x1b\\"
                .to_vec()
        );
        assert_eq!(p.foreground_rgba(DEFAULT_FG), rgb8_rgba(DEFAULT_FG_RGB, 1.0));
        assert_eq!(p.background_rgba(DEFAULT_BG), None);
        assert_eq!(p.cursor_rgba(), rgb8_rgba(DEFAULT_CURSOR_RGB, 0.6));
        assert_eq!(p.foreground_rgba(1), rgb8_rgba((0xcc, 0x40, 0x40), 1.0));
        assert!(g.contains("Z"));
        assert!(!g.contains("custom title"));
    }

    #[test]
    fn decstr_soft_reset_restores_modes_without_clearing_grid() {
        let mut g = Grid::new(4, 6);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"aaaaaa\nbbbbbb\ncccccc\ndddddd\x1b[2;3r\x1b[?6h\x1b[?7l\x1b[31;44m\x1b[?12h\x1b[6 q\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[!pX",
        );

        assert_eq!(g.cell(0, 0).ch, 'X');
        assert_eq!(g.cell(0, 1).ch, 'a', "soft reset must not clear visible cells");
        assert_eq!(g.cell(0, 0).fg, DEFAULT_FG);
        assert_eq!(g.cell(0, 0).bg, DEFAULT_BG);
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(p.cursor_visible());
        assert!(!p.cursor_blinking());
        assert!(!p.application_cursor_keys());
        assert!(!p.bracketed_paste_enabled());
        assert!(!p.mouse_reporting_enabled());
        assert!(!p.sgr_mouse_enabled());
        assert!(!g.contains("!p"));

        p.feed(&mut g, b"\x1b[4;1HZ\x1b[S");
        assert_eq!(g.to_text(), "bbbbbb\ncccccc\nZddddd\n      ");
    }

    #[test]
    fn cursor_column_and_line_csi_moves_and_clamps() {
        let g = grid_feed(3, 8, b"abcd\x1b[2GZ\x1b[20GX");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 1).ch, 'Z');
        assert_eq!(g.cell(0, 7).ch, 'X');
        assert!(!g.contains("2G"));
        assert!(!g.contains("20G"));

        let g2 = grid_feed(3, 8, b"\x1b[2;4H@\x1b[EZ\x1b[2FY\x1b[20E\x1b[2GX");
        assert_eq!(g2.cell(1, 3).ch, '@');
        assert_eq!(g2.cell(2, 0).ch, 'Z');
        assert_eq!(g2.cell(0, 0).ch, 'Y');
        assert_eq!(g2.cell(2, 1).ch, 'X');
        assert!(!g2.contains("[E"));
        assert!(!g2.contains("[2F"));
    }

    #[test]
    fn cursor_hpa_vpa_and_vpr_csi_moves_and_clamps() {
        let g = grid_feed(3, 8, b"abcd\x1b[3`Z\x1b[20`X");
        assert_eq!(g.cell(0, 2).ch, 'Z');
        assert_eq!(g.cell(0, 7).ch, 'X');
        assert!(!g.contains("3`"));
        assert!(!g.contains("20`"));

        let g2 = grid_feed(3, 8, b"\x1b[1;4H@\x1b[3dZ\x1b[1dA\x1b[20dB");
        assert_eq!(g2.cell(0, 5).ch, 'A');
        assert_eq!(g2.cell(2, 6).ch, 'B');
        assert!(!g2.contains("[3d"));
        assert!(!g2.contains("[20d"));

        let g3 = grid_feed(3, 8, b"\x1b[1;4H@\x1b[eA\x1b[20eB");
        assert_eq!(g3.cell(1, 4).ch, 'A');
        assert_eq!(g3.cell(2, 5).ch, 'B');
        assert!(!g3.contains("[e"));
        assert!(!g3.contains("[20e"));
    }

    #[test]
    fn osc_title_is_consumed() {
        // ESC]0;my title BEL  then text. The title bytes must not corrupt grid.
        let g = grid_feed(2, 20, b"\x1b]0;my title\x07done");
        assert!(g.contains("done"));
        assert!(!g.contains("my title"));
        assert!(!g.contains("0;"));

        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]0;my title\x07done");
        assert_eq!(p.title(), "my title");
    }

    #[test]
    fn osc_terminated_by_st() {
        // OSC terminated by ST (ESC \) instead of BEL.
        let g = grid_feed(2, 20, b"\x1b]2;t\x1b\\hi");
        assert!(g.contains("hi"));
        assert!(!g.contains("t"));

        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]2;project shell\x1b\\hi");
        assert_eq!(p.title(), "project shell");
    }

    #[test]
    fn osc_title_accepts_c1_st_and_ignores_unknown_kinds() {
        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x9d2;c1 title\x9cok");
        assert_eq!(p.title(), "c1 title");
        assert!(g.contains("ok"));
        assert!(!g.contains("c1 title"));

        p.feed(&mut g, b"\x1b]9;ignored\x07");
        assert_eq!(p.title(), "c1 title");
    }

    #[test]
    fn osc_title_is_sanitized_and_bounded() {
        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        let long = "x".repeat(200);
        let seq = format!("\x1b]0;\t  a\nb\r{long}\x07");
        p.feed(&mut g, seq.as_bytes());
        assert!(p.title().starts_with("ab"));
        assert!(p.title().chars().count() <= 160);
        assert!(!p.title().contains('\n'));
    }

    #[test]
    fn osc_52_clipboard_write_is_captured() {
        let mut g = Grid::new(1, 24);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]52;c;aGVsbG8gd29ybGQ=\x07ok");

        assert_eq!(p.take_clipboard_write(), Some("hello world".to_string()));
        assert!(p.take_clipboard_write().is_none());
        assert!(g.contains("ok"));
        assert!(!g.contains("aGVsbG8"));
        assert!(!g.contains("52;c"));
    }

    #[test]
    fn osc_52_ignores_queries_invalid_payloads_and_non_clipboard_targets() {
        let mut g = Grid::new(1, 40);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]52;c;?\x07\x1b]52;c;###\x07\x1b]52;p;aGVsbG8=\x07done",
        );

        assert!(p.take_clipboard_write().is_none());
        assert!(g.contains("done"));
        assert!(!g.contains("###"));
        assert!(!g.contains("aGVsbG8"));
    }

    #[test]
    fn osc_52_accepts_st_and_unpadded_base64() {
        let mut g = Grid::new(1, 24);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]52;;b2s\x1b\\done");

        assert_eq!(p.take_clipboard_write(), Some("ok".to_string()));
        assert!(g.contains("done"));
        assert!(!g.contains("b2s"));
    }

    #[test]
    fn osc_color_queries_are_answered() {
        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]10;?\x07\x1b]11;?\x1b\\\x9d12;?\x9cok",
        );
        assert_eq!(
            p.take_reply(),
            b"\x1b]10;rgb:d1d1/d6d6/e0e0\x1b\\\
              \x1b]11;rgb:1414/1414/1c1c\x1b\\\
              \x1b]12;rgb:7c7c/5c5c/ffff\x1b\\"
                .to_vec()
        );
        assert!(p.take_reply().is_empty());
        assert!(g.contains("ok"));
        assert!(!g.contains("10;?"));
        assert!(!g.contains("11;?"));
        assert!(!g.contains("12;?"));
    }

    #[test]
    fn osc_color_setters_update_query_replies() {
        let mut g = Grid::new(1, 30);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]10;#010203\x07\
              \x1b]11;rgb:ffff/8000/0000\x1b\\\
              \x1b]12;rgb:7c/5c/ff\x07\
              \x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07done",
        );
        assert_eq!(
            p.take_reply(),
            b"\x1b]10;rgb:0101/0202/0303\x1b\\\
              \x1b]11;rgb:ffff/8080/0000\x1b\\\
              \x1b]12;rgb:7c7c/5c5c/ffff\x1b\\"
                .to_vec()
        );
        assert!(g.contains("done"));
        assert!(!g.contains("#010203"));
        assert!(!g.contains("rgb:ffff"));
        assert!(!g.contains("10;?"));
    }

    #[test]
    fn osc_color_resets_restore_default_query_replies() {
        let mut g = Grid::new(1, 40);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]10;#010203\x07\x1b]11;#040506\x07\x1b]12;#070809\x07\
              \x1b]110\x07\x1b]111\x1b\\\x9d112\x9c\
              \x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07done",
        );
        assert_eq!(
            p.take_reply(),
            b"\x1b]10;rgb:d1d1/d6d6/e0e0\x1b\\\
              \x1b]11;rgb:1414/1414/1c1c\x1b\\\
              \x1b]12;rgb:7c7c/5c5c/ffff\x1b\\"
                .to_vec()
        );
        assert!(g.contains("done"));
        assert!(!g.contains("#010203"));
        assert!(!g.contains("110"));
    }

    #[test]
    fn osc_colors_resolve_for_terminal_drawing() {
        let mut g = Grid::new(1, 40);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]10;#010203\x07\
              \x1b]11;#040506\x07\
              \x1b]12;#070809\x07\
              \x1b]4;1;#0a0b0c\x07",
        );

        assert_eq!(p.foreground_rgba(DEFAULT_FG), rgb8_rgba((1, 2, 3), 1.0));
        assert_eq!(
            p.background_rgba(DEFAULT_BG),
            Some(rgb8_rgba((4, 5, 6), 0.72))
        );
        assert_eq!(p.cursor_rgba(), rgb8_rgba((7, 8, 9), 0.6));
        assert_eq!(p.foreground_rgba(1), rgb8_rgba((10, 11, 12), 1.0));
        assert_eq!(
            p.foreground_rgba(encode_truecolor(13, 14, 15)),
            rgb8_rgba((13, 14, 15), 1.0)
        );
    }

    #[test]
    fn osc_color_resets_restore_drawing_defaults() {
        let mut g = Grid::new(1, 40);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]10;#010203\x07\x1b]11;#040506\x07\x1b]12;#070809\x07\
              \x1b]110\x07\x1b]111\x07\x1b]112\x07",
        );

        assert_eq!(p.foreground_rgba(DEFAULT_FG), rgb8_rgba(DEFAULT_FG_RGB, 1.0));
        assert_eq!(p.background_rgba(DEFAULT_BG), None);
        assert_eq!(p.cursor_rgba(), rgb8_rgba(DEFAULT_CURSOR_RGB, 0.6));
    }

    #[test]
    fn osc_color_invalid_setters_and_unknown_queries_are_only_consumed() {
        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]10;not-a-color\x07\x1b]13;?\x07\x1b]10;?\x07done");
        assert_eq!(p.take_reply(), b"\x1b]10;rgb:d1d1/d6d6/e0e0\x1b\\");
        assert!(g.contains("done"));
        assert!(!g.contains("not-a-color"));
        assert!(!g.contains("13;?"));
    }

    #[test]
    fn osc_palette_queries_are_answered() {
        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]4;1;?\x07ok");
        assert_eq!(p.take_reply(), b"\x1b]4;1;rgb:cccc/4040/4040\x1b\\");
        assert!(g.contains("ok"));
        assert!(!g.contains("4;1;?"));
    }

    #[test]
    fn osc_palette_queries_support_multiple_pairs() {
        let mut g = Grid::new(1, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]4;196;?;7;?\x1b\\done");
        assert_eq!(
            p.take_reply(),
            b"\x1b]4;196;rgb:ffff/0000/0000\x1b\\\
              \x1b]4;7;rgb:cccc/d1d1/dbdb\x1b\\"
                .to_vec()
        );
        assert!(g.contains("done"));
        assert!(!g.contains("196;?"));
    }

    #[test]
    fn osc_palette_setters_update_query_replies() {
        let mut g = Grid::new(1, 40);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]4;1;#010203;2;rgb:ffff/8000/0000\x07\
              \x1b]4;1;?;2;?\x1b\\done",
        );
        assert_eq!(
            p.take_reply(),
            b"\x1b]4;1;rgb:0101/0202/0303\x1b\\\
              \x1b]4;2;rgb:ffff/8080/0000\x1b\\"
                .to_vec()
        );
        assert!(g.contains("done"));
        assert!(!g.contains("#010203"));
        assert!(!g.contains("rgb:ffff"));
    }

    #[test]
    fn osc_palette_resets_restore_default_query_replies() {
        let mut g = Grid::new(1, 50);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b]4;1;#010203;2;#040506\x07\
              \x1b]104;invalid\x07\
              \x1b]4;2;?\x07\
              \x1b]104;1\x07\
              \x1b]4;1;?;2;?\x07\
              \x1b]104\x07\
              \x1b]4;2;?\x1b\\done",
        );
        assert_eq!(
            p.take_reply(),
            b"\x1b]4;2;rgb:0404/0505/0606\x1b\\\
              \x1b]4;1;rgb:cccc/4040/4040\x1b\\\
              \x1b]4;2;rgb:0404/0505/0606\x1b\\\
              \x1b]4;2;rgb:4d4d/b8b8/5959\x1b\\"
                .to_vec()
        );
        assert!(g.contains("done"));
        assert!(!g.contains("104"));
    }

    #[test]
    fn osc_palette_invalid_queries_and_setters_are_only_consumed() {
        let mut g = Grid::new(1, 30);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b]4;256;?\x07\x1b]4;1;not-a-color\x07done");
        assert!(p.take_reply().is_empty());
        assert!(g.contains("done"));
        assert!(!g.contains("256;?"));
        assert!(!g.contains("not-a-color"));
    }

    #[test]
    fn non_osc_escape_strings_are_consumed_until_st() {
        let g = grid_feed(
            2,
            40,
            b"\x1bP1+rpayload\x1b\\ok\x1b_hidden\x1b\\done",
        );
        assert!(g.contains("ok"));
        assert!(g.contains("done"));
        assert!(!g.contains("payload"));
        assert!(!g.contains("hidden"));
        assert!(!g.contains("1+r"));

        let g2 = grid_feed(2, 40, b"\x1b^privacy\x1b\\x\x1bXguard\x1b\\y");
        assert!(g2.contains("xy"));
        assert!(!g2.contains("privacy"));
        assert!(!g2.contains("guard"));
    }

    #[test]
    fn escape_strings_abort_on_can_or_sub() {
        let g = grid_feed(
            2,
            40,
            b"\x1b]0;title\x18after\x1b]1;icon\x1asub\x1bPpayload\x18dcs",
        );
        assert!(g.contains("after"));
        assert!(g.contains("sub"));
        assert!(g.contains("dcs"));
        assert!(!g.contains("title"));
        assert!(!g.contains("icon"));
        assert!(!g.contains("payload"));

        let g2 = grid_feed(2, 40, b"\x1b^privacy\x1aguard\x1bXhidden\x18safe");
        assert!(g2.contains("guard"));
        assert!(g2.contains("safe"));
        assert!(!g2.contains("privacy"));
        assert!(!g2.contains("hidden"));
    }

    #[test]
    fn escape_string_esc_substates_accept_eight_bit_st() {
        let g = grid_feed(2, 40, b"\x1b]0;title\x1b\x9cafter");
        assert!(g.contains("after"));
        assert!(!g.contains("title"));

        let g2 = grid_feed(2, 40, b"\x1bPpayload\x1b\x9cdcs\x1b_hidden\x1b\x9capc");
        assert!(g2.contains("dcs"));
        assert!(g2.contains("apc"));
        assert!(!g2.contains("payload"));
        assert!(!g2.contains("hidden"));
    }

    #[test]
    fn eight_bit_c1_controls_match_esc_prefixed_forms() {
        let g = grid_feed(2, 40, b"abcd\x9b2GZ\x9d0;title\x9cok");
        assert_eq!(g.cell(0, 1).ch, 'Z');
        assert!(g.contains("ok"));
        assert!(!g.contains("2G"));
        assert!(!g.contains("title"));

        let g2 = grid_feed(
            2,
            40,
            b"\x90dcspayload\x9cD\x98sospayload\x9cS\x9epmpayload\x9cP\x9fapcpayload\x9cA",
        );
        assert!(g2.contains("DSPA"));
        assert!(!g2.contains("payload"));
        assert!(!g2.contains("dcs"));
        assert!(!g2.contains("sos"));
        assert!(!g2.contains("pm"));
        assert!(!g2.contains("apc"));
    }

    #[test]
    fn eight_bit_c1_movement_controls_match_single_escape_forms() {
        let g = grid_feed(3, 8, b"AA\x85BB\x84C");
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cell(1, 0).ch, 'B');
        assert_eq!(g.cell(2, 2).ch, 'C');

        let g2 = grid_feed(3, 8, b"\x1b[2;3H@\x8dR");
        assert_eq!(g2.cell(0, 3).ch, 'R');
        assert_eq!(g2.cell(1, 2).ch, '@');

        let g3 = grid_feed(1, 12, b"\x1b[3g\x1b[1;7H\x88\x1b[1;1H\tZ");
        assert_eq!(g3.cell(0, 6).ch, 'Z');
    }

    #[test]
    fn csi_can_be_interrupted_by_new_escape_controls() {
        let g = grid_feed(2, 20, b"abcd\x1b[31\x1b[2GZ");
        assert_eq!(g.cell(0, 1).ch, 'Z');
        assert!(!g.contains("[2G"));
        assert!(!g.contains("31"));

        let g2 = grid_feed(2, 20, b"abcd\x9b31\x9b3GX");
        assert_eq!(g2.cell(0, 2).ch, 'X');
        assert!(!g2.contains("31"));
        assert!(!g2.contains("3G"));

        let g3 = grid_feed(2, 30, b"\x1b[999\x9d0;title\x9cok");
        assert!(g3.contains("ok"));
        assert!(!g3.contains("999"));
        assert!(!g3.contains("title"));
    }

    #[test]
    fn csi_tolerates_embedded_c0_controls_without_leaking_final_bytes() {
        let g = grid_feed(2, 20, b"abcd\x1b[2\x07GZ");
        assert_eq!(g.cell(0, 1).ch, 'Z');
        assert!(!g.contains("GZ"));
        assert!(!g.contains("2G"));

        let g2 = grid_feed(2, 20, b"abcd\x1b[3\rGQ");
        assert_eq!(g2.cell(0, 2).ch, 'Q');
        assert!(!g2.contains("GQ"));
        assert!(!g2.contains("3G"));

        let g3 = grid_feed(2, 20, b"abcd\x1b[2\nGZ");
        assert_eq!(g3.cell(0, 0).ch, 'a');
        assert_eq!(g3.cell(0, 3).ch, 'd');
        assert_eq!(g3.cell(1, 1).ch, 'Z');
        assert!(!g3.contains("2G"));

        let g4 = grid_feed(3, 20, b"abcd\x1b[2\x0cGZ\x1b[4\x0bGX");
        assert_eq!(g4.cell(1, 1).ch, 'Z');
        assert_eq!(g4.cell(2, 3).ch, 'X');
        assert!(!g4.contains("2G"));
        assert!(!g4.contains("4G"));
    }

    #[test]
    fn utf8_multibyte_decodes() {
        // "é" is 0xC3 0xA9; "→" is 0xE2 0x86 0x92.
        let g = grid_feed(2, 10, "café→".as_bytes());
        assert_eq!(g.cell(0, 3).ch, 'é');
        assert_eq!(g.cell(0, 4).ch, '→');
    }

    #[test]
    fn dsr_cursor_position_report_is_queued() {
        // ESC[6n after writing "abc" -> cursor at row 1, col 4 (1-based).
        let mut g = Grid::new(4, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"abc\x1b[6n");
        let reply = p.take_reply();
        assert_eq!(reply, b"\x1b[1;4R");
        // The query itself left no garbage in the grid.
        assert!(!g.contains("6n"));
        // A second take yields nothing (buffer drained).
        assert!(p.take_reply().is_empty());

        p.feed(&mut g, b"\x1b[1;1Habcdefghij\x1b[6n");
        assert_eq!(p.take_reply(), b"\x1b[1;10R");
    }

    #[test]
    fn private_dsr_cursor_position_report_is_queued() {
        let mut g = Grid::new(4, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[3;5HX\x1b[?6n");
        assert_eq!(p.take_reply(), b"\x1b[?3;6R");
        assert!(!g.contains("?6n"));
        assert_eq!(g.cell(2, 4).ch, 'X');
    }

    #[test]
    fn dsr_device_status_report_ok() {
        let mut g = Grid::new(2, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[5n");
        assert_eq!(p.take_reply(), b"\x1b[0n");
    }

    #[test]
    fn device_attributes_queries_are_answered() {
        let mut g = Grid::new(2, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[c");
        assert_eq!(p.take_reply(), b"\x1b[?1;2c");
        assert!(!g.contains("[c"));

        p.feed(&mut g, b"\x1b[0c");
        assert_eq!(p.take_reply(), b"\x1b[?1;2c");

        p.feed(&mut g, b"\x1b[>c");
        assert_eq!(p.take_reply(), b"\x1b[>0;0;0c");

        p.feed(&mut g, b"\x1b[>0c");
        assert_eq!(p.take_reply(), b"\x1b[>0;0;0c");
    }

    #[test]
    fn private_mode_status_queries_report_tracked_state() {
        let mut g = Grid::new(2, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[?2004$p");
        assert_eq!(p.take_reply(), b"\x1b[?2004;2$y");

        p.feed(&mut g, b"\x1b[?2004h\x1b[?2004$p");
        assert_eq!(p.take_reply(), b"\x1b[?2004;1$y");
        assert!(p.bracketed_paste_enabled());

        p.feed(&mut g, b"\x1b[?12$p\x1b[?12h\x1b[?12$p");
        assert_eq!(p.take_reply(), b"\x1b[?12;2$y\x1b[?12;1$y");
        assert!(p.cursor_blinking());

        p.feed(&mut g, b"\x1b[?1004h\x1b[?1004$p\x1b[?25l\x1b[?25$p");
        assert_eq!(p.take_reply(), b"\x1b[?1004;1$y\x1b[?25;2$y");
        assert!(p.focus_reporting_enabled());
        assert!(!p.cursor_visible());
        assert!(!g.contains("2004"));
        assert!(!g.contains("12"));
        assert!(!g.contains("1004"));
        assert!(!g.contains("25"));
    }

    #[test]
    fn ansi_mode_status_queries_report_insert_mode() {
        let mut g = Grid::new(2, 16);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[4$p");
        assert_eq!(p.take_reply(), b"\x1b[4;2$y");

        p.feed(&mut g, b"\x1b[4h\x1b[4$p\x1b[4l\x1b[4$p\x1b[20$p\x1b[20l\x1b[20$p");
        assert_eq!(p.take_reply(), b"\x1b[4;1$y\x1b[4;2$y\x1b[20;1$y\x1b[20;2$y");
        assert!(!g.contains("4$p"));
        assert!(!g.contains("[4h"));
        assert!(!g.contains("20$p"));

        p.feed(&mut g, b"\x1b[9999$px\x1b[1;2$py");
        assert_eq!(p.take_reply(), b"\x1b[9999;0$y");
        assert!(g.contains("xy"));
        assert!(!g.contains("9999"));
        assert!(!g.contains("1;2"));
    }

    #[test]
    fn private_mode_status_queries_report_mouse_and_alternate_modes() {
        let mut g = Grid::new(2, 10);
        let mut p = VtParser::new();
        p.feed(
            &mut g,
            b"\x1b[?1000h\x1b[?1006h\x1b[?1049h\x1b[?1000$p\x1b[?1006$p\x1b[?1049$p",
        );
        assert_eq!(p.take_reply(), b"\x1b[?1000;1$y\x1b[?1006;1$y\x1b[?1049;1$y");

        p.feed(&mut g, b"\x1b[?1000l\x1b[?1049l\x1b[?1000$p\x1b[?1049$p");
        assert_eq!(p.take_reply(), b"\x1b[?1000;2$y\x1b[?1049;2$y");
    }

    #[test]
    fn private_mode_status_unknown_and_malformed_queries_are_safe() {
        let mut g = Grid::new(2, 20);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[?9999$pok");
        assert_eq!(p.take_reply(), b"\x1b[?9999;0$y");
        assert!(g.contains("ok"));
        assert!(!g.contains("9999"));

        p.feed(&mut g, b"\x1b[?1;2$pdone");
        assert!(p.take_reply().is_empty());
        assert!(g.contains("done"));
        assert!(!g.contains("1;2"));
    }

    #[test]
    fn window_size_character_queries_are_answered() {
        let mut g = Grid::new(7, 33);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[18t");
        assert_eq!(p.take_reply(), b"\x1b[8;7;33t");
        assert!(!g.contains("18t"));

        g.resize(12, 80);
        p.feed(&mut g, b"\x1b[19t");
        assert_eq!(p.take_reply(), b"\x1b[8;12;80t");
        assert!(!g.contains("19t"));
    }

    #[test]
    fn unsupported_window_ops_are_only_consumed() {
        let mut g = Grid::new(2, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[14tok");
        assert!(p.take_reply().is_empty());
        assert!(g.contains("ok"));
        assert!(!g.contains("14t"));
    }

    #[test]
    fn esc_c_resets_grid() {
        let g = grid_feed(2, 10, b"junk\x1bcOK");
        assert_eq!(g.cell(0, 0).ch, 'O');
        assert!(!g.contains("junk"));
    }

    #[test]
    fn grid_resize_preserves_overlap() {
        let mut g = Grid::new(2, 4);
        let mut p = VtParser::new();
        p.feed(&mut g, b"AB\nCD");
        g.resize(3, 6);
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cell(1, 1).ch, 'D');
        assert_eq!(g.rows(), 3);
        assert_eq!(g.cols(), 6);
        assert_eq!(g.scroll_top, 0);
        assert_eq!(g.scroll_bottom, 2);
    }

    // ---- key/codepoint mapping ----

    #[test]
    fn key_mapping_enter_backspace_arrows() {
        use crate::ffi::*;
        assert_eq!(key_to_bytes(MUI_KEY_ENTER, 0, false), Some(vec![b'\r']));
        assert_eq!(
            key_to_bytes(MUI_KEY_ENTER, MUI_MOD_ALT, false),
            Some(vec![0x1b, b'\r'])
        );
        assert_eq!(key_to_bytes(MUI_KEY_BACKSPACE, 0, false), Some(vec![0x7f]));
        assert_eq!(
            key_to_bytes(MUI_KEY_BACKSPACE, MUI_MOD_ALT, false),
            Some(vec![0x1b, 0x7f])
        );
        assert_eq!(key_to_bytes(MUI_KEY_TAB, 0, false), Some(vec![b'\t']));
        assert_eq!(
            key_to_bytes(MUI_KEY_TAB, MUI_MOD_ALT, false),
            Some(vec![0x1b, b'\t'])
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_TAB, MUI_MOD_SHIFT, false),
            Some(vec![0x1b, b'[', b'Z'])
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_TAB, MUI_MOD_ALT | MUI_MOD_SHIFT, false),
            Some(vec![0x1b, 0x1b, b'[', b'Z'])
        );
        assert_eq!(key_to_bytes(MUI_KEY_ESCAPE, 0, false), Some(vec![0x1b]));
        assert_eq!(
            key_to_bytes(MUI_KEY_ESCAPE, MUI_MOD_ALT, false),
            Some(vec![0x1b, 0x1b])
        );
        assert_eq!(key_to_bytes(MUI_KEY_UP, 0, false), Some(vec![0x1b, b'[', b'A']));
        assert_eq!(key_to_bytes(MUI_KEY_LEFT, 0, false), Some(vec![0x1b, b'[', b'D']));
        assert_eq!(key_to_bytes(MUI_KEY_HOME, 0, false), Some(vec![0x1b, b'[', b'H']));
        assert_eq!(key_to_bytes(MUI_KEY_END, 0, false), Some(vec![0x1b, b'[', b'F']));
        assert_eq!(key_to_bytes(MUI_KEY_INSERT, 0, false), Some(vec![0x1b, b'[', b'2', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_DELETE, 0, false), Some(vec![0x1b, b'[', b'3', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_PAGE_UP, 0, false), Some(vec![0x1b, b'[', b'5', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_PAGE_DOWN, 0, false), Some(vec![0x1b, b'[', b'6', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F1, 0, false), Some(vec![0x1b, b'O', b'P']));
        assert_eq!(key_to_bytes(MUI_KEY_F2, 0, false), Some(vec![0x1b, b'O', b'Q']));
        assert_eq!(key_to_bytes(MUI_KEY_F3, 0, false), Some(vec![0x1b, b'O', b'R']));
        assert_eq!(key_to_bytes(MUI_KEY_F4, 0, false), Some(vec![0x1b, b'O', b'S']));
        assert_eq!(key_to_bytes(MUI_KEY_F5, 0, false), Some(vec![0x1b, b'[', b'1', b'5', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F6, 0, false), Some(vec![0x1b, b'[', b'1', b'7', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F7, 0, false), Some(vec![0x1b, b'[', b'1', b'8', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F8, 0, false), Some(vec![0x1b, b'[', b'1', b'9', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F9, 0, false), Some(vec![0x1b, b'[', b'2', b'0', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F10, 0, false), Some(vec![0x1b, b'[', b'2', b'1', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F11, 0, false), Some(vec![0x1b, b'[', b'2', b'3', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F12, 0, false), Some(vec![0x1b, b'[', b'2', b'4', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_UNKNOWN, 0, false), None);
    }

    #[test]
    fn key_mapping_honors_application_cursor_mode() {
        use crate::ffi::*;
        assert_eq!(key_to_bytes(MUI_KEY_UP, 0, true), Some(vec![0x1b, b'O', b'A']));
        assert_eq!(key_to_bytes(MUI_KEY_DOWN, 0, true), Some(vec![0x1b, b'O', b'B']));
        assert_eq!(key_to_bytes(MUI_KEY_RIGHT, 0, true), Some(vec![0x1b, b'O', b'C']));
        assert_eq!(key_to_bytes(MUI_KEY_LEFT, 0, true), Some(vec![0x1b, b'O', b'D']));
        assert_eq!(key_to_bytes(MUI_KEY_HOME, 0, true), Some(vec![0x1b, b'[', b'H']));
    }

    #[test]
    fn key_mapping_honors_navigation_modifiers() {
        use crate::ffi::*;
        assert_eq!(
            key_to_bytes(MUI_KEY_UP, MUI_MOD_SHIFT, false),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_LEFT, MUI_MOD_CTRL, false),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_RIGHT, MUI_MOD_ALT | MUI_MOD_CTRL, false),
            Some(b"\x1b[1;7C".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_HOME, MUI_MOD_SHIFT | MUI_MOD_CTRL, false),
            Some(b"\x1b[1;6H".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_END, MUI_MOD_ALT, false),
            Some(b"\x1b[1;3F".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_INSERT, MUI_MOD_SHIFT, false),
            Some(b"\x1b[2;2~".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_DELETE, MUI_MOD_CTRL, false),
            Some(b"\x1b[3;5~".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_PAGE_UP, MUI_MOD_SHIFT, false),
            Some(b"\x1b[5;2~".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_PAGE_DOWN, MUI_MOD_ALT | MUI_MOD_SHIFT, false),
            Some(b"\x1b[6;4~".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_UP, MUI_MOD_CTRL, true),
            Some(b"\x1b[1;5A".to_vec())
        );
    }

    #[test]
    fn key_mapping_honors_function_key_modifiers() {
        use crate::ffi::*;
        assert_eq!(
            key_to_bytes(MUI_KEY_F1, MUI_MOD_SHIFT, false),
            Some(b"\x1b[1;2P".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_F2, MUI_MOD_ALT, false),
            Some(b"\x1b[1;3Q".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_F4, MUI_MOD_CTRL, false),
            Some(b"\x1b[1;5S".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_F5, MUI_MOD_SHIFT, false),
            Some(b"\x1b[15;2~".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_F10, MUI_MOD_ALT | MUI_MOD_CTRL, false),
            Some(b"\x1b[21;7~".to_vec())
        );
        assert_eq!(
            key_to_bytes(MUI_KEY_F12, MUI_MOD_SHIFT | MUI_MOD_CTRL, false),
            Some(b"\x1b[24;6~".to_vec())
        );
    }

    #[test]
    fn codepoint_mapping_plain_and_ctrl() {
        use crate::ffi::MUI_MOD_CTRL;
        // Plain 'a' -> "a".
        assert_eq!(codepoint_to_bytes(b'a' as u32, 0), Some(vec![b'a']));
        // Ctrl+C -> 0x03.
        assert_eq!(codepoint_to_bytes(b'c' as u32, MUI_MOD_CTRL), Some(vec![0x03]));
        // Ctrl+uppercase C -> also 0x03.
        assert_eq!(codepoint_to_bytes(b'C' as u32, MUI_MOD_CTRL), Some(vec![0x03]));
        // Ctrl+space -> NUL.
        assert_eq!(codepoint_to_bytes(b' ' as u32, MUI_MOD_CTRL), Some(vec![0]));
        // Multibyte char -> UTF-8 bytes.
        assert_eq!(codepoint_to_bytes('é' as u32, 0), Some(vec![0xc3, 0xa9]));
    }

    #[test]
    fn codepoint_mapping_alt_prefixes_meta_escape() {
        use crate::ffi::{MUI_MOD_ALT, MUI_MOD_CTRL};
        assert_eq!(
            codepoint_to_bytes(b'x' as u32, MUI_MOD_ALT),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            codepoint_to_bytes(b'c' as u32, MUI_MOD_ALT | MUI_MOD_CTRL),
            Some(vec![0x1b, 0x03])
        );
        assert_eq!(
            codepoint_to_bytes('é' as u32, MUI_MOD_ALT),
            Some(vec![0x1b, 0xc3, 0xa9])
        );
    }

    #[test]
    fn paste_bytes_wrap_only_when_bracketed() {
        assert_eq!(paste_to_bytes("a\nb", false), b"a\nb".to_vec());
        assert_eq!(
            paste_to_bytes("a\nb", true),
            b"\x1b[200~a\nb\x1b[201~".to_vec()
        );
    }

    #[test]
    fn scroll_bytes_send_repeated_cursor_moves() {
        assert_eq!(
            scroll_to_bytes(1, false, false, 1, 1, 0),
            Some(b"\x1b[A\x1b[A\x1b[A".to_vec())
        );
        assert_eq!(
            scroll_to_bytes(-1, false, false, 1, 1, 0),
            Some(b"\x1b[B\x1b[B\x1b[B".to_vec())
        );
        assert_eq!(scroll_to_bytes(0, false, false, 1, 1, 0), None);
    }

    #[test]
    fn scroll_bytes_send_legacy_mouse_wheel_when_reporting_enabled() {
        assert_eq!(
            scroll_to_bytes(1, true, false, 1, 1, 0),
            Some(vec![0x1b, b'[', b'M', 96, 33, 33])
        );
        assert_eq!(
            scroll_to_bytes(-1, true, false, 3, 7, 0),
            Some(vec![0x1b, b'[', b'M', 97, 39, 35])
        );
        assert_eq!(
            scroll_to_bytes(1, true, false, 999, 999, 0),
            Some(vec![0x1b, b'[', b'M', 96, 255, 255])
        );
        assert_eq!(scroll_to_bytes(0, true, false, 1, 1, 0), None);
    }

    #[test]
    fn scroll_bytes_send_sgr_mouse_wheel_at_event_cell_when_enabled() {
        assert_eq!(
            scroll_to_bytes(1, true, true, 1, 1, 0),
            Some(b"\x1b[<64;1;1M".to_vec())
        );
        assert_eq!(
            scroll_to_bytes(-1, true, true, 3, 7, 0),
            Some(b"\x1b[<65;7;3M".to_vec())
        );
        assert_eq!(scroll_to_bytes(0, true, true, 1, 1, 0), None);
    }

    #[test]
    fn scroll_bytes_can_still_encode_origin_cell() {
        assert_eq!(
            scroll_to_bytes(-1, true, false, 1, 1, 0),
            Some(vec![0x1b, b'[', b'M', 97, 33, 33])
        );
        assert_eq!(
            scroll_to_bytes(1, true, true, 1, 1, 0),
            Some(b"\x1b[<64;1;1M".to_vec())
        );
    }

    #[test]
    fn mouse_button_bytes_send_legacy_press_and_release() {
        assert_eq!(
            mouse_button_to_bytes(true, crate::ffi::MUI_MOUSE_LEFT, true, false, 3, 7, 0),
            Some(vec![0x1b, b'[', b'M', 32, 39, 35])
        );
        assert_eq!(
            mouse_button_to_bytes(false, crate::ffi::MUI_MOUSE_LEFT, true, false, 3, 7, 0),
            Some(vec![0x1b, b'[', b'M', 35, 39, 35])
        );
        assert_eq!(
            mouse_button_to_bytes(
                true,
                crate::ffi::MUI_MOUSE_RIGHT,
                true,
                false,
                999,
                999,
                0,
            ),
            Some(vec![0x1b, b'[', b'M', 34, 255, 255])
        );
        assert_eq!(
            mouse_button_to_bytes(true, crate::ffi::MUI_MOUSE_LEFT, false, false, 3, 7, 0),
            None
        );
    }

    #[test]
    fn mouse_button_bytes_send_sgr_press_and_release() {
        assert_eq!(
            mouse_button_to_bytes(true, crate::ffi::MUI_MOUSE_LEFT, true, true, 3, 7, 0),
            Some(b"\x1b[<0;7;3M".to_vec())
        );
        assert_eq!(
            mouse_button_to_bytes(false, crate::ffi::MUI_MOUSE_LEFT, true, true, 3, 7, 0),
            Some(b"\x1b[<0;7;3m".to_vec())
        );
        assert_eq!(
            mouse_button_to_bytes(true, crate::ffi::MUI_MOUSE_MIDDLE, true, true, 1, 2, 0),
            Some(b"\x1b[<1;2;1M".to_vec())
        );
        assert_eq!(
            mouse_button_to_bytes(true, crate::ffi::MUI_MOUSE_OTHER, true, true, 3, 7, 0),
            None
        );
    }

    #[test]
    fn mouse_motion_bytes_send_legacy_drag_and_any_motion() {
        assert_eq!(
            mouse_motion_to_bytes(Some(crate::ffi::MUI_MOUSE_LEFT), true, false, 3, 7, 0),
            Some(vec![0x1b, b'[', b'M', 64, 39, 35])
        );
        assert_eq!(
            mouse_motion_to_bytes(Some(crate::ffi::MUI_MOUSE_RIGHT), true, false, 999, 999, 0),
            Some(vec![0x1b, b'[', b'M', 66, 255, 255])
        );
        assert_eq!(
            mouse_motion_to_bytes(None, true, false, 3, 7, 0),
            Some(vec![0x1b, b'[', b'M', 67, 39, 35])
        );
        assert_eq!(
            mouse_motion_to_bytes(Some(crate::ffi::MUI_MOUSE_OTHER), true, false, 3, 7, 0),
            None
        );
        assert_eq!(
            mouse_motion_to_bytes(Some(crate::ffi::MUI_MOUSE_LEFT), false, false, 3, 7, 0),
            None
        );
    }

    #[test]
    fn mouse_motion_bytes_send_sgr_drag_and_any_motion() {
        assert_eq!(
            mouse_motion_to_bytes(Some(crate::ffi::MUI_MOUSE_LEFT), true, true, 3, 7, 0),
            Some(b"\x1b[<32;7;3M".to_vec())
        );
        assert_eq!(
            mouse_motion_to_bytes(Some(crate::ffi::MUI_MOUSE_MIDDLE), true, true, 1, 2, 0),
            Some(b"\x1b[<33;2;1M".to_vec())
        );
        assert_eq!(
            mouse_motion_to_bytes(None, true, true, 3, 7, 0),
            Some(b"\x1b[<35;7;3M".to_vec())
        );
    }

    #[test]
    fn mouse_bytes_include_xterm_modifier_bits() {
        use crate::ffi::{MUI_MOD_ALT, MUI_MOD_CTRL, MUI_MOD_SHIFT};
        assert_eq!(
            scroll_to_bytes(1, true, true, 3, 7, MUI_MOD_SHIFT | MUI_MOD_CTRL),
            Some(b"\x1b[<84;7;3M".to_vec())
        );
        assert_eq!(
            scroll_to_bytes(-1, true, false, 3, 7, MUI_MOD_ALT),
            Some(vec![0x1b, b'[', b'M', 105, 39, 35])
        );
        assert_eq!(
            mouse_button_to_bytes(
                true,
                crate::ffi::MUI_MOUSE_LEFT,
                true,
                true,
                3,
                7,
                MUI_MOD_SHIFT | MUI_MOD_ALT,
            ),
            Some(b"\x1b[<12;7;3M".to_vec())
        );
        assert_eq!(
            mouse_button_to_bytes(
                false,
                crate::ffi::MUI_MOUSE_LEFT,
                true,
                false,
                3,
                7,
                MUI_MOD_CTRL,
            ),
            Some(vec![0x1b, b'[', b'M', 51, 39, 35])
        );
        assert_eq!(
            mouse_motion_to_bytes(
                Some(crate::ffi::MUI_MOUSE_LEFT),
                true,
                true,
                3,
                7,
                MUI_MOD_CTRL,
            ),
            Some(b"\x1b[<48;7;3M".to_vec())
        );
        assert_eq!(
            mouse_motion_to_bytes(None, true, false, 3, 7, MUI_MOD_SHIFT),
            Some(vec![0x1b, b'[', b'M', 71, 39, 35])
        );
    }

    #[test]
    fn focus_report_bytes_match_xterm_focus_events() {
        assert_eq!(focus_report_to_bytes(true), b"\x1b[I");
        assert_eq!(focus_report_to_bytes(false), b"\x1b[O");
    }

    // ---- PTY integration (skips gracefully if spawn fails) ----

    #[test]
    fn pty_echo_roundtrip_or_skip() {
        let mut term = match Terminal::spawn(24, 80) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("SKIP: PTY spawn failed in this environment: {e}");
                return;
            }
        };
        // Ask the shell to echo a unique marker. `echo` works in both cmd.exe
        // and POSIX shells.
        term.send(b"echo mui_marker_123\r");
        // Give the shell time to start + respond, pumping output as it arrives.
        let mut found = false;
        for _ in 0..100 {
            term.pump();
            if term.grid().contains("mui_marker_123") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            found,
            "expected echoed marker in grid; got:\n{}",
            term.grid().to_text()
        );
    }
}
