//! Integrated terminal: PTY-backed shell + minimal VT parser + character grid.
//!
//! Mighty (v0.36) can't hold strings/pointers/threads/Vecs of structs across
//! FFI (L17/L21), so the entire terminal lives here on the Rust side and is
//! driven through the scalar ABI in [`crate::abi`]. The three pieces:
//!
//! * [`Grid`] — a rows×cols matrix of [`Cell`]s (codepoint + fg color) plus a
//!   cursor; the only stateful UI surface, drawn shim-side.
//! * [`VtParser`] — a deliberately small VT/ANSI interpreter that feeds bytes
//!   into the grid: printable UTF-8, `\n`/`\r`/`\b`/`\t`, and SGR color escapes
//!   (`ESC [ … m`). Other CSI/OSC sequences are consumed (skipped) so they never
//!   corrupt the grid. This is NOT a full xterm — just enough to run a shell.
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
}

/// Sentinel `fg` meaning "default foreground" (SGR 0 / 39).
pub const DEFAULT_FG: u32 = 0xffff_ffff;
/// Sentinel `bg` meaning "transparent/default background" (SGR 0 / 49).
pub const DEFAULT_BG: u32 = 0xffff_fffe;
const TRUECOLOR_MASK: u32 = 0x0100_0000;

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
        }
    }
}

#[derive(Clone, Debug)]
struct ScreenSnapshot {
    rows: usize,
    cols: usize,
    cells: Vec<Cell>,
    cur_row: usize,
    cur_col: usize,
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

    /// Clear all cells to blanks and home the cursor.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
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
        for row in self.scroll_bottom - count + 1..=self.scroll_bottom {
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
        for row in self.scroll_bottom - count + 1..=self.scroll_bottom {
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

    /// Write a printable char at the cursor, honoring the current autowrap mode.
    /// Control chars are NOT handled here.
    fn put_char_autowrap(&mut self, ch: char, autowrap: bool) {
        if self.cur_col >= self.cols {
            if autowrap {
                // Wrap before writing.
                self.newline();
            } else {
                self.cur_col = self.cols - 1;
            }
        }
        let idx = self.cur_row * self.cols + self.cur_col;
        self.cells[idx] = Cell {
            ch,
            fg: self.cur_fg,
            bg: self.cur_bg,
        };
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

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        if top >= bottom || bottom >= self.rows {
            return;
        }
        self.scroll_top = top;
        self.scroll_bottom = bottom;
        self.cur_row = 0;
        self.cur_col = 0;
    }

    fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    fn move_cursor_1_based(&mut self, row: usize, col: usize) {
        self.cur_row = row.saturating_sub(1).min(self.rows - 1);
        self.cur_col = col.saturating_sub(1).min(self.cols - 1);
    }

    fn move_cursor_relative(&mut self, d_row: isize, d_col: isize) {
        let row = self.cur_row.saturating_add_signed(d_row).min(self.rows - 1);
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

    fn tab(&mut self) {
        // Advance to the next multiple-of-8 column (classic tab stops).
        let next = ((self.cur_col / 8) + 1) * 8;
        self.cur_col = next.min(self.cols);
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

/// Parser state machine. The VT parser is intentionally tiny: it recognizes a
/// handful of control bytes and the `ESC [ … m` (SGR) escape; every other
/// escape (CSI ending in a non-`m` final byte, or an OSC `ESC ] … BEL/ST`) is
/// consumed without touching the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Normal: bytes are decoded as UTF-8 and printed (or handled as controls).
    Ground,
    /// Saw `ESC`; waiting for the next byte to decide CSI / OSC / other.
    Escape,
    /// Inside a CSI (`ESC [`); collecting parameter/intermediate bytes until a
    /// final byte (0x40..=0x7e).
    Csi,
    /// Inside an OSC (`ESC ]`); consuming until BEL (0x07) or ST (`ESC \`).
    Osc,
    /// Inside OSC and just saw an `ESC`; an immediate `\` (0x5c) terminates (ST).
    OscEsc,
}

/// A minimal VT/ANSI parser that drives a [`Grid`].
#[derive(Debug)]
pub struct VtParser {
    state: State,
    /// Accumulated CSI parameter/intermediate bytes (between `ESC [` and final).
    csi: Vec<u8>,
    /// Partial UTF-8 sequence being decoded in Ground state.
    utf8: Vec<u8>,
    /// How many continuation bytes remain for the in-progress UTF-8 char.
    utf8_need: usize,
    /// Bytes the parser wants written BACK to the PTY (e.g. a Device Status
    /// Report reply to `ESC [ 6 n`). ConPTY blocks further output until the DSR
    /// it emits at startup is answered, so the terminal must drain + send these.
    reply: Vec<u8>,
    /// Saved cursor position used by DEC `ESC 7`/`ESC 8` and CSI `s`/`u`.
    saved_cursor: Option<(usize, usize)>,
    /// Whether the running app asked for bracketed paste (`CSI ?2004 h`).
    bracketed_paste: bool,
    /// Whether the terminal cursor should be drawn (`CSI ?25 h/l`).
    cursor_visible: bool,
    /// Shape requested by DECSCUSR (`CSI Ps SP q`).
    cursor_shape: CursorShape,
    /// Whether arrow keys should use application cursor-key sequences.
    application_cursor_keys: bool,
    /// Whether the running app asked for mouse button/drag/all-motion reports.
    mouse_reporting: bool,
    /// Whether mouse reports should use SGR extended coordinates (`CSI ?1006 h`).
    sgr_mouse: bool,
    /// Whether printable output should wrap after the right margin (`CSI ?7 h/l`).
    autowrap: bool,
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
            utf8: Vec::new(),
            utf8_need: 0,
            reply: Vec::new(),
            saved_cursor: None,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_shape: CursorShape::Block,
            application_cursor_keys: false,
            mouse_reporting: false,
            sgr_mouse: false,
            autowrap: true,
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

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    pub fn sgr_mouse_enabled(&self) -> bool {
        self.mouse_reporting && self.sgr_mouse
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    fn feed_byte(&mut self, grid: &mut Grid, b: u8) {
        match self.state {
            State::Ground => self.ground(grid, b),
            State::Escape => self.escape(grid, b),
            State::Csi => self.csi(grid, b),
            State::Osc => self.osc(b),
            State::OscEsc => self.osc_esc(b),
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
            b'\n' => grid.newline(),
            b'\r' => grid.carriage_return(),
            0x08 | 0x7f => grid.backspace(), // BS / DEL
            b'\t' => grid.tab(),
            0x07 => {} // BEL: ignore
            0x00..=0x06 | 0x0b..=0x1a | 0x1c..=0x1f => {} // other C0: ignore
            0x20..=0x7e => grid.put_char_autowrap(b as char, self.autowrap), // printable ASCII
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
                for ch in s.chars() {
                    grid.put_char_autowrap(ch, self.autowrap);
                }
            }
            Err(_) => grid.put_char_autowrap('\u{fffd}', self.autowrap), // replacement char
        }
        self.utf8.clear();
    }

    /// Just saw ESC: decide CSI / OSC / single-char escape.
    fn escape(&mut self, grid: &mut Grid, b: u8) {
        match b {
            b'[' => {
                self.csi.clear();
                self.state = State::Csi;
            }
            b']' => self.state = State::Osc,
            // `ESC c` full reset — clear the grid.
            b'c' => {
                grid.clear();
                self.reset_modes();
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
            // Other two-byte escapes (e.g. `ESC =`, `ESC >`, charset selects):
            // consume the single byte and return to ground.
            _ => self.state = State::Ground,
        }
    }

    /// Inside a CSI: accumulate until a final byte (0x40..=0x7e). Handles the
    /// core shell sequences we need (SGR, DSR, erase display/line, cursor
    /// movement); others are
    /// consumed harmlessly.
    fn csi(&mut self, grid: &mut Grid, b: u8) {
        match b {
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
                } else if b == b'@' {
                    self.insert_chars(grid);
                } else if b == b'J' {
                    self.erase_display(grid);
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
                } else if b == b'T' {
                    self.scroll_down(grid);
                } else if b == b'X' {
                    self.erase_chars(grid);
                } else if b == b'h' || b == b'l' {
                    self.set_mode(grid, b);
                } else if b == b'r' {
                    self.set_scroll_region(grid);
                } else if b == b'q' {
                    self.set_cursor_shape();
                } else if b == b'H' || b == b'f' {
                    self.cursor_position(grid);
                } else if matches!(b, b'A' | b'B' | b'C' | b'D') {
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

    /// Apply an `ESC [ … m` SGR sequence: parse `;`-separated numeric params and
    /// update the grid's current colors. Handles reset (0), the basic/bright
    /// ANSI colors, xterm 256-color `38;5;n` / `48;5;n`, truecolor
    /// `38;2;r;g;b` / `48;2;r;g;b`, and default fg/bg (39/49). Unknown params
    /// are ignored.
    fn apply_sgr(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        // A bare `ESC [ m` means reset.
        if params.is_empty() {
            grid.cur_fg = DEFAULT_FG;
            grid.cur_bg = DEFAULT_BG;
            return;
        }
        // `ESC [ ? … m` (private) — not a real SGR; ignore.
        if params.starts_with('?') {
            return;
        }
        let params: Vec<Option<i32>> = params
            .split(';')
            .map(|part| {
                if part.is_empty() {
                    Some(0)
                } else {
                    part.parse().ok()
                }
            })
            .collect();

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
                }
                30..=37 => grid.cur_fg = (n - 30) as u32, // basic 0..=7
                39 => grid.cur_fg = DEFAULT_FG,     // default fg
                40..=47 => grid.cur_bg = (n - 40) as u32, // basic bg 0..=7
                49 => grid.cur_bg = DEFAULT_BG,      // default bg
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
        grid.set_scroll_region(top.saturating_sub(1), bottom.saturating_sub(1));
    }

    fn set_mode(&mut self, grid: &mut Grid, final_byte: u8) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("").to_string();
        let Some(private) = params.strip_prefix('?') else {
            return;
        };

        let modes: Vec<&str> = private.split(';').collect();
        for mode in modes {
            match (mode, final_byte) {
                ("47" | "1047" | "1049", b'h') => grid.enter_alternate_screen(),
                ("47" | "1047" | "1049", b'l') => grid.exit_alternate_screen(),
                ("1", b'h') => self.application_cursor_keys = true,
                ("1", b'l') => self.application_cursor_keys = false,
                ("7", b'h') => self.autowrap = true,
                ("7", b'l') => self.autowrap = false,
                ("1048", b'h') => self.save_cursor(grid),
                ("1048", b'l') => self.restore_cursor(grid),
                ("25", b'h') => self.cursor_visible = true,
                ("25", b'l') => self.cursor_visible = false,
                ("2004", b'h') => self.bracketed_paste = true,
                ("2004", b'l') => self.bracketed_paste = false,
                ("1000" | "1002" | "1003", b'h') => self.mouse_reporting = true,
                ("1000" | "1002" | "1003", b'l') => self.mouse_reporting = false,
                ("1006", b'h') => self.sgr_mouse = true,
                ("1006", b'l') => self.sgr_mouse = false,
                _ => {}
            }
        }
    }

    fn reset_modes(&mut self) {
        self.saved_cursor = None;
        self.bracketed_paste = false;
        self.cursor_visible = true;
        self.cursor_shape = CursorShape::Block;
        self.application_cursor_keys = false;
        self.mouse_reporting = false;
        self.sgr_mouse = false;
        self.autowrap = true;
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
        self.cursor_shape = match shape {
            0 | 1 | 2 => CursorShape::Block,
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => self.cursor_shape,
        };
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
            2 | 3 => grid.clear(),
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
        grid.move_cursor_1_based(row, col);
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
            b'A' => grid.move_cursor_relative(-amount, 0),
            b'B' => grid.move_cursor_relative(amount, 0),
            b'C' => grid.move_cursor_relative(0, amount),
            b'D' => grid.move_cursor_relative(0, -amount),
            _ => {}
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
        grid.move_cursor_row_1_based(row);
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
        grid.move_cursor_relative(amount, 0);
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
            b'E' => grid.move_cursor_line_relative(amount),
            b'F' => grid.move_cursor_line_relative(-amount),
            _ => {}
        }
    }

    fn save_cursor(&mut self, grid: &Grid) {
        self.saved_cursor = Some(grid.cursor());
    }

    fn restore_cursor(&mut self, grid: &mut Grid) {
        if let Some((row, col)) = self.saved_cursor {
            grid.move_cursor_1_based(row + 1, col + 1);
        }
    }

    /// Answer a Device Status Report (`ESC [ Ps n`). `5n` -> "OK" (`ESC[0n`);
    /// `6n` -> cursor position report `ESC[<row>;<col>R` (1-based). Anything
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
            _ => {}
        }
    }

    /// Inside an OSC: consume until BEL or the start of an ST (`ESC \`).
    fn osc(&mut self, b: u8) {
        match b {
            0x07 => self.state = State::Ground, // BEL terminates
            0x1b => self.state = State::OscEsc, // maybe ST
            _ => {}                              // title text etc.: consume
        }
    }

    /// In OSC and saw ESC: a `\` completes ST; anything else re-enters OSC.
    fn osc_esc(&mut self, b: u8) {
        match b {
            b'\\' => self.state = State::Ground, // ST terminates
            0x07 => self.state = State::Ground,  // tolerate stray BEL
            _ => self.state = State::Osc,        // not ST; keep consuming
        }
    }
}

fn encode_truecolor(r: u8, g: u8, b: u8) -> u32 {
    TRUECOLOR_MASK | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
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

    pub fn cursor_shape(&self) -> CursorShape {
        self.parser.cursor_shape()
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
    }

    /// Write raw bytes to the PTY stdin (the shell's input).
    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
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

    /// Send a wheel gesture using mouse reporting when the running app requested
    /// it; otherwise fall back to repeated cursor movement for ordinary shells.
    pub fn send_scroll(&mut self, dir: i32) {
        if let Some(bytes) = scroll_to_bytes(dir, self.parser.sgr_mouse_enabled()) {
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
/// Backspace -> DEL (`\x7f`), Tab -> `\t`, Escape -> `\x1b`, arrows -> the usual
/// `ESC [ A/B/C/D`. Ctrl+letter (handled on the Char path) is mapped separately.
pub fn key_to_bytes(key: u32, _mods: u32, application_cursor_keys: bool) -> Option<Vec<u8>> {
    use crate::ffi::*;
    let bytes: Vec<u8> = match key {
        MUI_KEY_ENTER => vec![b'\r'],
        MUI_KEY_BACKSPACE => vec![0x7f],
        MUI_KEY_TAB => vec![b'\t'],
        MUI_KEY_ESCAPE => vec![0x1b],
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
        MUI_KEY_DELETE => vec![0x1b, b'[', b'3', b'~'],
        MUI_KEY_PAGE_UP => vec![0x1b, b'[', b'5', b'~'],
        MUI_KEY_PAGE_DOWN => vec![0x1b, b'[', b'6', b'~'],
        MUI_KEY_F1 => vec![0x1b, b'O', b'P'],
        MUI_KEY_F2 => vec![0x1b, b'[', b'1', b'2', b'~'],
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

/// Map a typed codepoint + modifier bits to terminal stdin bytes. With Ctrl held
/// and an ASCII letter, emit the corresponding control code (Ctrl+C -> 0x03,
/// etc.); otherwise emit the char's UTF-8 bytes.
pub fn codepoint_to_bytes(codepoint: u32, mods: u32) -> Option<Vec<u8>> {
    use crate::ffi::MUI_MOD_CTRL;
    let ch = char::from_u32(codepoint)?;
    if mods & MUI_MOD_CTRL != 0 {
        // Ctrl+@..Ctrl+_ -> 0x00..0x1f. Letters are case-insensitive.
        let upper = (ch as u32).to_ascii_uppercase_u32();
        if (0x40..=0x5f).contains(&upper) {
            return Some(vec![(upper - 0x40) as u8]);
        }
        // Ctrl+space -> NUL.
        if ch == ' ' {
            return Some(vec![0]);
        }
    }
    let mut buf = [0u8; 4];
    Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
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

pub fn scroll_to_bytes(dir: i32, sgr_mouse: bool) -> Option<Vec<u8>> {
    if sgr_mouse {
        return match dir {
            d if d > 0 => Some(b"\x1b[<64;1;1M".to_vec()),
            d if d < 0 => Some(b"\x1b[<65;1;1M".to_vec()),
            _ => None,
        };
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
    fn tab_advances_to_next_stop() {
        let g = grid_feed(2, 40, b"a\tb");
        // 'a' at col 0, tab -> col 8, 'b' at col 8.
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 8).ch, 'b');
    }

    #[test]
    fn wrap_at_right_edge() {
        let g = grid_feed(3, 3, b"abcd");
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(0, 2).ch, 'c');
        assert_eq!(g.cell(1, 0).ch, 'd');
        assert_eq!(g.cursor(), (1, 1));
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
        // Bold + fg color: "1;33" -> bold ignored, yellow (3) applied.
        let g = grid_feed(2, 10, b"\x1b[1;33mY");
        assert_eq!(g.cell(0, 0).ch, 'Y');
        assert_eq!(g.cell(0, 0).fg, 3);

        let g2 = grid_feed(1, 8, b"\x1b[1;33;45mZ");
        assert_eq!(g2.cell(0, 0).fg, 3);
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
    }

    #[test]
    fn scroll_region_linefeed_preserves_rows_outside_margins() {
        let g = grid_feed(
            4,
            4,
            b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[3;1HZZZZ\nYYYY",
        );
        assert_eq!(g.to_text(), "1111\nZZZZ\nYYYY\n4444");
        assert_eq!(g.cursor(), (2, 4));
        assert!(!g.contains("2;3r"));
    }

    #[test]
    fn scroll_region_limits_explicit_scroll_commands() {
        let g = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[S");
        assert_eq!(g.to_text(), "1111\n3333\n    \n4444");

        let g2 = grid_feed(4, 4, b"1111\n2222\n3333\n4444\x1b[2;3r\x1b[T");
        assert_eq!(g2.to_text(), "1111\n    \n2222\n4444");
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
    fn mouse_reporting_modes_track_private_csi() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert!(!p.sgr_mouse_enabled());

        p.feed(&mut g, b"\x1b[?1000h\x1b[?1006h");
        assert!(p.sgr_mouse_enabled());
        assert!(!g.contains("1000"));
        assert!(!g.contains("1006"));

        p.feed(&mut g, b"\x1b[?1006l");
        assert!(!p.sgr_mouse_enabled());

        p.feed(&mut g, b"\x1b[?1006h\x1b[?1000l");
        assert!(!p.sgr_mouse_enabled());
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
    fn cursor_shape_tracks_decscusr() {
        let mut g = Grid::new(1, 8);
        let mut p = VtParser::new();
        assert_eq!(p.cursor_shape(), CursorShape::Block);

        p.feed(&mut g, b"\x1b[4 q");
        assert_eq!(p.cursor_shape(), CursorShape::Underline);
        assert!(!g.contains("4 q"));

        p.feed(&mut g, b"\x1b[6 q");
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
        assert!(!g.contains("6 q"));

        p.feed(&mut g, b"\x1b[2 q");
        assert_eq!(p.cursor_shape(), CursorShape::Block);
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
            b"\x1b[?1h\x1b[?25l\x1b[6 q\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1bc",
        );
        assert!(!p.application_cursor_keys());
        assert!(p.cursor_visible());
        assert_eq!(p.cursor_shape(), CursorShape::Block);
        assert!(!p.bracketed_paste_enabled());
        assert!(!p.sgr_mouse_enabled());
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
    }

    #[test]
    fn osc_terminated_by_st() {
        // OSC terminated by ST (ESC \) instead of BEL.
        let g = grid_feed(2, 20, b"\x1b]2;t\x1b\\hi");
        assert!(g.contains("hi"));
        assert!(!g.contains("t"));
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
    }

    #[test]
    fn dsr_device_status_report_ok() {
        let mut g = Grid::new(2, 10);
        let mut p = VtParser::new();
        p.feed(&mut g, b"\x1b[5n");
        assert_eq!(p.take_reply(), b"\x1b[0n");
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
        assert_eq!(key_to_bytes(MUI_KEY_BACKSPACE, 0, false), Some(vec![0x7f]));
        assert_eq!(key_to_bytes(MUI_KEY_TAB, 0, false), Some(vec![b'\t']));
        assert_eq!(key_to_bytes(MUI_KEY_ESCAPE, 0, false), Some(vec![0x1b]));
        assert_eq!(key_to_bytes(MUI_KEY_UP, 0, false), Some(vec![0x1b, b'[', b'A']));
        assert_eq!(key_to_bytes(MUI_KEY_LEFT, 0, false), Some(vec![0x1b, b'[', b'D']));
        assert_eq!(key_to_bytes(MUI_KEY_HOME, 0, false), Some(vec![0x1b, b'[', b'H']));
        assert_eq!(key_to_bytes(MUI_KEY_END, 0, false), Some(vec![0x1b, b'[', b'F']));
        assert_eq!(key_to_bytes(MUI_KEY_DELETE, 0, false), Some(vec![0x1b, b'[', b'3', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_PAGE_UP, 0, false), Some(vec![0x1b, b'[', b'5', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_PAGE_DOWN, 0, false), Some(vec![0x1b, b'[', b'6', b'~']));
        assert_eq!(key_to_bytes(MUI_KEY_F1, 0, false), Some(vec![0x1b, b'O', b'P']));
        assert_eq!(key_to_bytes(MUI_KEY_F2, 0, false), Some(vec![0x1b, b'[', b'1', b'2', b'~']));
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
            scroll_to_bytes(1, false),
            Some(b"\x1b[A\x1b[A\x1b[A".to_vec())
        );
        assert_eq!(
            scroll_to_bytes(-1, false),
            Some(b"\x1b[B\x1b[B\x1b[B".to_vec())
        );
        assert_eq!(scroll_to_bytes(0, false), None);
    }

    #[test]
    fn scroll_bytes_send_sgr_mouse_wheel_when_enabled() {
        assert_eq!(scroll_to_bytes(1, true), Some(b"\x1b[<64;1;1M".to_vec()));
        assert_eq!(scroll_to_bytes(-1, true), Some(b"\x1b[<65;1;1M".to_vec()));
        assert_eq!(scroll_to_bytes(0, true), None);
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
