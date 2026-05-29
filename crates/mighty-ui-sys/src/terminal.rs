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

/// One terminal cell: a Unicode scalar value and an 8-bit-ish color index.
///
/// `fg` is a palette index 0..=15 (the 8 basic ANSI colors + 8 bright), or the
/// sentinel [`DEFAULT_FG`] for "use the default foreground". Keeping color as a
/// small index (not RGBA) means the draw path resolves it to a concrete color,
/// and the grid stays compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: u8,
}

/// Sentinel `fg` meaning "default foreground" (SGR 0 / 39).
pub const DEFAULT_FG: u8 = 0xff;

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: DEFAULT_FG,
        }
    }
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
    cur_fg: u8,
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
    }

    /// Clear all cells to blanks and home the cursor.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
        self.cur_row = 0;
        self.cur_col = 0;
    }

    /// Scroll the whole grid up one line: drop row 0, shift the rest up, blank
    /// the last row. Used when the cursor would advance past the last row.
    fn scroll_up(&mut self) {
        // Shift rows [1..rows) into [0..rows-1).
        self.cells.rotate_left(self.cols);
        // Blank the now-bottom row.
        let start = (self.rows - 1) * self.cols;
        for c in &mut self.cells[start..] {
            *c = Cell::default();
        }
    }

    /// Advance the cursor to the start of the next line, scrolling if needed.
    fn newline(&mut self) {
        self.cur_col = 0;
        if self.cur_row + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cur_row += 1;
        }
    }

    /// Write a printable char at the cursor, advancing it (wrapping at the right
    /// edge, scrolling at the bottom). Control chars are NOT handled here.
    pub fn put_char(&mut self, ch: char) {
        if self.cur_col >= self.cols {
            // Wrap before writing.
            self.newline();
        }
        let idx = self.cur_row * self.cols + self.cur_col;
        self.cells[idx] = Cell {
            ch,
            fg: self.cur_fg,
        };
        self.cur_col += 1;
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

    /// Whether any row contains `needle` (test helper).
    #[cfg(test)]
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
            0x20..=0x7e => grid.put_char(b as char), // printable ASCII
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
                    grid.put_char(ch);
                }
            }
            Err(_) => grid.put_char('\u{fffd}'), // replacement char
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
                self.state = State::Ground;
            }
            // Other two-byte escapes (e.g. `ESC =`, `ESC >`, charset selects):
            // consume the single byte and return to ground.
            _ => self.state = State::Ground,
        }
    }

    /// Inside a CSI: accumulate until a final byte (0x40..=0x7e). Only `m` (SGR)
    /// is acted on; every other final byte is consumed harmlessly.
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
                }
                // All other finals (J/K/H/A..D/etc.) are intentionally skipped.
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
    /// update the grid's current foreground. Handles reset (0), the 8 basic
    /// colors (30..=37), the 8 bright colors (90..=97), and default-fg (39).
    /// Unknown params (bold, bg, 256-color, etc.) are ignored.
    fn apply_sgr(&mut self, grid: &mut Grid) {
        let params = std::str::from_utf8(&self.csi).unwrap_or("");
        // A bare `ESC [ m` means reset.
        if params.is_empty() {
            grid.cur_fg = DEFAULT_FG;
            return;
        }
        // `ESC [ ? … m` (private) — not a real SGR; ignore.
        if params.starts_with('?') {
            return;
        }
        for part in params.split(';') {
            let n: i32 = if part.is_empty() {
                0
            } else {
                match part.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };
            match n {
                0 => grid.cur_fg = DEFAULT_FG,      // reset
                30..=37 => grid.cur_fg = (n - 30) as u8, // basic 0..=7
                39 => grid.cur_fg = DEFAULT_FG,     // default fg
                90..=97 => grid.cur_fg = (n - 90 + 8) as u8, // bright 8..=15
                _ => {}                              // ignore everything else
            }
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

/// Resolve a palette `fg` index to RGBA (0.0..=1.0). [`DEFAULT_FG`] -> a light
/// neutral. A small, readable ANSI-ish palette tuned for a dark background.
pub fn palette_rgba(fg: u8) -> (f32, f32, f32, f32) {
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
        _ => (0.82, 0.84, 0.88),  // DEFAULT_FG / unknown
    };
    (rgb.0, rgb.1, rgb.2, 1.0)
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
pub fn key_to_bytes(key: u32, _mods: u32) -> Option<Vec<u8>> {
    use crate::ffi::*;
    let bytes: Vec<u8> = match key {
        MUI_KEY_ENTER => vec![b'\r'],
        MUI_KEY_BACKSPACE => vec![0x7f],
        MUI_KEY_TAB => vec![b'\t'],
        MUI_KEY_ESCAPE => vec![0x1b],
        MUI_KEY_LEFT => vec![0x1b, b'[', b'D'],
        MUI_KEY_RIGHT => vec![0x1b, b'[', b'C'],
        MUI_KEY_UP => vec![0x1b, b'[', b'A'],
        MUI_KEY_DOWN => vec![0x1b, b'[', b'B'],
        MUI_KEY_HOME => vec![0x1b, b'[', b'H'],
        MUI_KEY_END => vec![0x1b, b'[', b'F'],
        MUI_KEY_DELETE => vec![0x1b, b'[', b'3', b'~'],
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
    fn sgr_compound_params_ignored_safely() {
        // Bold + fg color: "1;33" -> bold ignored, yellow (3) applied.
        let g = grid_feed(2, 10, b"\x1b[1;33mY");
        assert_eq!(g.cell(0, 0).ch, 'Y');
        assert_eq!(g.cell(0, 0).fg, 3);
    }

    #[test]
    fn unknown_csi_is_consumed_without_garbage() {
        // ESC[2J (clear screen) is NOT implemented but must be swallowed cleanly
        // with no stray glyphs landing in the grid.
        let g = grid_feed(2, 10, b"\x1b[2JOK");
        // The "2J" must not appear; only "OK" prints.
        assert_eq!(g.cell(0, 0).ch, 'O');
        assert_eq!(g.cell(0, 1).ch, 'K');
        assert!(!g.contains("2J"));
        assert!(!g.contains("["));
    }

    #[test]
    fn cursor_position_csi_is_skipped() {
        // ESC[5;10H (cursor move) skipped; text continues at the prior cursor.
        let g = grid_feed(2, 20, b"A\x1b[5;10HB");
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cell(0, 1).ch, 'B');
        assert!(!g.contains("5;10H"));
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
    }

    // ---- key/codepoint mapping ----

    #[test]
    fn key_mapping_enter_backspace_arrows() {
        use crate::ffi::*;
        assert_eq!(key_to_bytes(MUI_KEY_ENTER, 0), Some(vec![b'\r']));
        assert_eq!(key_to_bytes(MUI_KEY_BACKSPACE, 0), Some(vec![0x7f]));
        assert_eq!(key_to_bytes(MUI_KEY_TAB, 0), Some(vec![b'\t']));
        assert_eq!(key_to_bytes(MUI_KEY_ESCAPE, 0), Some(vec![0x1b]));
        assert_eq!(key_to_bytes(MUI_KEY_UP, 0), Some(vec![0x1b, b'[', b'A']));
        assert_eq!(key_to_bytes(MUI_KEY_LEFT, 0), Some(vec![0x1b, b'[', b'D']));
        assert_eq!(key_to_bytes(MUI_KEY_HOME, 0), Some(vec![0x1b, b'[', b'H']));
        assert_eq!(key_to_bytes(MUI_KEY_UNKNOWN, 0), None);
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
