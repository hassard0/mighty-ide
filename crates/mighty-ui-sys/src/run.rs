//! Run panel: run the active Mighty file via `mty run <path>` on a background
//! thread, stream stdout+stderr into a scrollable output view, and surface the
//! Mighty diagnostics in the output as clickable jump-to-file:line entries.
//!
//! Mirrors the integrated terminal's pump pattern (a reader thread drains the
//! child's combined output into a shared buffer; [`RunPanel::pump`] folds new
//! bytes into the line list each frame) so the UI never blocks on the process.
//! v0.36 Mighty can't run processes / hold strings across FFI (L17/L21), so the
//! whole thing lives shim-side and is driven through the scalar `mui_run_*` ABI.
//!
//! Diagnostic detection reuses [`crate::diagnostics::parse_header`]-style logic:
//! each output line is scanned for an `[MTxxxx] <Severity>: <msg>` header and an
//! accompanying `[<path>:<line>:<col>]` location (the ariadne report shape). A
//! line carrying a location becomes a CLICKABLE entry whose `(file, line, col)`
//! the IDE reads back to jump the editor there.

#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use crate::diagnostics;

/// One line of run output, optionally carrying a clickable file:line:col target
/// parsed out of a Mighty diagnostic location.
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub text: String,
    /// `true` when this line is a diagnostic with a resolved location (rendered
    /// as a clickable entry).
    pub clickable: bool,
    /// Absolute/relative path of the diagnostic target (empty if not clickable).
    pub file: String,
    /// 0-based line of the target (or -1).
    pub line: i32,
    /// 0-based column of the target (or -1).
    pub col: i32,
    /// `true` if the line looks like an error (red tint), `false` otherwise.
    pub is_error: bool,
}

impl OutputLine {
    fn plain(text: String) -> Self {
        OutputLine {
            text,
            clickable: false,
            file: String::new(),
            line: -1,
            col: -1,
            is_error: false,
        }
    }
}

/// Shim-owned Run panel state: the spawned child + reader thread, the parsed
/// output lines, scroll, status + timing, and the last clicked target.
#[derive(Default)]
pub struct RunPanel {
    /// `true` while the Run panel is shown.
    active: bool,
    /// Output lines (stdout+stderr interleaved, ANSI-stripped, diag-annotated).
    lines: Vec<OutputLine>,
    /// Top visible line (scroll offset).
    first: usize,
    /// `true` while the child process is still running.
    running: bool,
    /// Exit code once the process finished (`None` while running / never run).
    exit_code: Option<i32>,
    /// Wall-clock duration of the last/current run, in milliseconds.
    duration_ms: u128,
    /// The path that was run (for the header).
    path: String,
    /// Carry buffer for a partial last line (no trailing newline yet).
    partial: String,

    // ---- background process plumbing (lazily set on `start`) ----
    /// Combined stdout+stderr bytes the reader threads append; drained on pump.
    out: Option<Arc<Mutex<Vec<u8>>>>,
    /// Signals all reader threads finished (the child's pipes hit EOF).
    done: Option<Receiver<()>>,
    /// The spawned child (kept alive so `stop` can kill it).
    child: Option<std::process::Child>,
    /// When the current run started (for the duration line).
    started: Option<Instant>,
    /// The last clicked clickable line's target, read back by the IDE.
    click_target: Option<(String, i32, i32)>,
    /// Latch set by `pump` on the running→finished transition; read+cleared by
    /// [`take_just_finished`] so the IDE can fire a one-shot "Run finished" toast.
    just_finished: bool,
}

impl RunPanel {
    pub fn new() -> Self {
        RunPanel::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn open(&mut self) {
        self.active = true;
    }

    pub fn close(&mut self) {
        self.active = false;
    }

    pub fn toggle(&mut self) -> bool {
        self.active = !self.active;
        self.active
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Read+clear the running→finished latch (one-shot; for a "Run finished"
    /// toast). Returns `true` exactly once per completed run.
    pub fn take_just_finished(&mut self) -> bool {
        std::mem::take(&mut self.just_finished)
    }

    pub fn duration_ms(&self) -> u128 {
        self.duration_ms
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn first(&self) -> usize {
        self.first
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn line(&self, i: usize) -> Option<&OutputLine> {
        self.lines.get(i)
    }

    pub fn click_target(&self) -> Option<&(String, i32, i32)> {
        self.click_target.as_ref()
    }

    pub fn set_click_target(&mut self, t: Option<(String, i32, i32)>) {
        self.click_target = t;
    }

    /// Scroll by `delta` lines (clamped).
    pub fn scroll(&mut self, delta: i32) {
        let max = self.lines.len().saturating_sub(1) as i32;
        let mut f = self.first as i32 + delta;
        if f < 0 {
            f = 0;
        }
        if f > max.max(0) {
            f = max.max(0);
        }
        self.first = f as usize;
    }

    /// Auto-scroll so the tail is visible (best effort: pin to the last
    /// `visible_rows`). The IDE passes how many rows fit.
    pub fn scroll_to_end(&mut self, visible_rows: usize) {
        let n = self.lines.len();
        self.first = n.saturating_sub(visible_rows.max(1));
    }

    /// Resolve the path to the `mty` compiler (honors `MIGHTY_MTY`, else the dev
    /// build path, else bare `mty`). Shared shape with `diagnostics::mty_path`.
    fn mty_path() -> String {
        if let Ok(p) = std::env::var("MIGHTY_MTY") {
            if !p.trim().is_empty() {
                return p;
            }
        }
        const DEV: &str = r"C:\Users\ihass\stardust\target\debug\mty.exe";
        if Path::new(DEV).exists() {
            return DEV.to_string();
        }
        "mty".to_string()
    }

    /// Start `mty run <path>` on background threads. Kills any prior run first.
    /// Clears the output, resets status, and records the start time. Returns
    /// `true` if the process spawned.
    pub fn start(&mut self, path: &Path) -> bool {
        self.stop(); // kill a prior run if any
        self.lines.clear();
        self.partial.clear();
        self.first = 0;
        self.exit_code = None;
        self.duration_ms = 0;
        self.click_target = None;
        self.path = path.to_string_lossy().into_owned();
        self.active = true;

        let mty = Self::mty_path();
        let mut cmd = Command::new(&mty);
        cmd.arg("run").arg(path);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.lines.push(OutputLine {
                    text: format!("failed to run `{mty} run`: {e}"),
                    is_error: true,
                    ..OutputLine::plain(String::new())
                });
                self.running = false;
                self.exit_code = Some(-1);
                return false;
            }
        };

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (done_tx, done_rx) = mpsc::channel();

        // One reader thread per pipe, both appending into the shared buffer; a
        // shared completion sender fires once both finish.
        let spawn_reader = |mut pipe: Box<dyn Read + Send>, sink: Arc<Mutex<Vec<u8>>>| {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut g) = sink.lock() {
                                g.extend_from_slice(&buf[..n]);
                            }
                        }
                    }
                }
            })
        };

        let mut handles = Vec::new();
        if let Some(so) = child.stdout.take() {
            handles.push(spawn_reader(Box::new(so), Arc::clone(&out)));
        }
        if let Some(se) = child.stderr.take() {
            handles.push(spawn_reader(Box::new(se), Arc::clone(&out)));
        }
        // A joiner thread waits for both pipes to close, then signals done.
        std::thread::spawn(move || {
            for h in handles {
                let _ = h.join();
            }
            let _ = done_tx.send(());
        });

        self.out = Some(out);
        self.done = Some(done_rx);
        self.child = Some(child);
        self.started = Some(Instant::now());
        self.running = true;
        true
    }

    /// Stop the running process (best-effort kill + reap). No-op if not running.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.out = None;
        self.done = None;
        if self.running {
            self.running = false;
            if self.exit_code.is_none() {
                self.exit_code = Some(-1); // terminated
            }
            if let Some(s) = self.started.take() {
                self.duration_ms = s.elapsed().as_millis();
            }
        }
        self.started = None;
    }

    /// Drain pending output into the line list + detect completion. Returns
    /// `true` if anything changed this frame (so the IDE redraws). Call once per
    /// frame while the panel is open.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;

        // Drain any buffered bytes.
        if let Some(out) = &self.out {
            let chunk = match out.lock() {
                Ok(mut g) if !g.is_empty() => std::mem::take(&mut *g),
                _ => Vec::new(),
            };
            if !chunk.is_empty() {
                let text = String::from_utf8_lossy(&chunk);
                self.feed(&text);
                changed = true;
            }
        }

        // Completion check.
        if self.running {
            if let Some(done) = &self.done {
                match done.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => {
                        // Drain a final time in case bytes arrived just before EOF.
                        if let Some(out) = self.out.take() {
                            if let Ok(g) = out.lock() {
                                if !g.is_empty() {
                                    let text = String::from_utf8_lossy(&g);
                                    let owned = text.into_owned();
                                    self.feed(&owned);
                                }
                            }
                        }
                        self.flush_partial();
                        self.running = false;
                        if let Some(mut child) = self.child.take() {
                            let code = child.wait().ok().and_then(|s| s.code());
                            self.exit_code = Some(code.unwrap_or(-1));
                        } else {
                            self.exit_code = self.exit_code.or(Some(0));
                        }
                        if let Some(s) = self.started.take() {
                            self.duration_ms = s.elapsed().as_millis();
                        }
                        self.done = None;
                        self.just_finished = true;
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
        }

        changed
    }

    /// Append a chunk of (possibly partial) text: split on newlines, carrying an
    /// unterminated tail in `partial`. Each completed line is parsed for a
    /// diagnostic location → clickable entry.
    fn feed(&mut self, chunk: &str) {
        // ANSI-strip the whole chunk first (the compiler colors its report).
        let clean = strip_ansi(chunk);
        let mut buf = std::mem::take(&mut self.partial);
        buf.push_str(&clean);
        // Normalize CRLF.
        let buf = buf.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts: Vec<&str> = buf.split('\n').collect();
        // The last element is the (possibly empty) unterminated tail.
        let tail = parts.pop().unwrap_or("").to_string();
        for line in parts {
            self.push_line(line.to_string());
        }
        self.partial = tail;
    }

    /// Push the carried partial as a final line (on process completion).
    fn flush_partial(&mut self) {
        if !self.partial.is_empty() {
            let p = std::mem::take(&mut self.partial);
            self.push_line(p);
        }
    }

    /// Classify + push one complete output line. A line carrying a Mighty
    /// `[<path>:<line>:<col>]` location (from an ariadne report) becomes a
    /// clickable jump entry; a `[MTxxxx] Error:` header marks it as an error.
    fn push_line(&mut self, text: String) {
        let is_error = text.contains("] Error:") || text.contains("error:");
        let loc = parse_location(&text);
        let line = match loc {
            Some((file, l1, c1)) => OutputLine {
                text,
                clickable: true,
                file,
                line: (l1 as i32 - 1).max(0),
                col: (c1 as i32 - 1).max(0),
                is_error: true,
            },
            None => OutputLine {
                is_error,
                ..OutputLine::plain(text)
            },
        };
        self.lines.push(line);
    }

    /// Seed fake run output (used by the screenshot hook so the panel renders
    /// without spawning a real process). Includes a clickable diagnostic line.
    pub fn seed_demo(&mut self, path: &str) {
        self.path = path.to_string();
        self.active = true;
        self.running = false;
        self.exit_code = Some(1);
        self.duration_ms = 142;
        self.lines.clear();
        self.first = 0;
        let demo = [
            "Compiling demo.mty ...",
            "[MT2001] Error: expected `I32`, found `Str`",
            &format!("   \u{256d}\u{2500}[{path}:7:14]"),
            "   7 \u{2502}   let n: I32 = \"oops\"",
            "     \u{2502}                ^^^^^^ expected `I32`, found `Str`",
            "warning: 1 warning emitted",
            "error: could not compile `demo` due to previous error",
            "process exited with code 1",
        ];
        for d in demo {
            self.push_line(d.to_string());
        }
    }
}

/// Parse a trailing `[<path>:<line>:<col>]` location from a report line, the
/// same shape [`diagnostics`] recognizes. Returns `(path, line1, col1)`
/// (1-based). The path may itself contain `:` (`C:/...`), so we anchor on the
/// last two numeric colon-separated fields inside the final `[...]`.
fn parse_location(line: &str) -> Option<(String, u32, u32)> {
    let open = line.rfind('[')?;
    let close = line[open..].find(']')? + open;
    let inner = &line[open + 1..close];
    let parts: Vec<&str> = inner.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let col: u32 = parts[parts.len() - 1].trim().parse().ok()?;
    let line_no: u32 = parts[parts.len() - 2].trim().parse().ok()?;
    // The path is everything before those last two fields, rejoined on ':'.
    let path = parts[..parts.len() - 2].join(":");
    let path = path.trim().trim_start_matches(['\u{256d}', '\u{2500}', '-', ' ']).to_string();
    if path.is_empty() {
        return None;
    }
    Some((path, line_no, col))
}

/// Strip ANSI escape sequences (delegates to the diagnostics-style scanner).
fn strip_ansi(s: &str) -> String {
    diagnostics::strip_ansi_public(s)
}

/// Resolve a clicked line's target path against the workspace root if it is not
/// already absolute. Returns the path to open + the 0-based line/col.
pub fn resolve_target(root: &Path, file: &str, line: i32, col: i32) -> (PathBuf, i32, i32) {
    let p = PathBuf::from(file);
    let full = if p.is_absolute() { p } else { root.join(p) };
    (full, line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_splits_lines_and_carries_partial() {
        let mut r = RunPanel::new();
        r.feed("hello\nwor");
        assert_eq!(r.line_count(), 1);
        assert_eq!(r.line(0).unwrap().text, "hello");
        // "wor" is carried as partial, not yet a line.
        r.feed("ld\n");
        assert_eq!(r.line_count(), 2);
        assert_eq!(r.line(1).unwrap().text, "world");
    }

    #[test]
    fn diagnostic_location_becomes_clickable() {
        let mut r = RunPanel::new();
        r.feed("   \u{256d}\u{2500}[C:/proj/src/main.mty:7:14]\n");
        let l = r.line(0).unwrap();
        assert!(l.clickable);
        assert_eq!(l.file, "C:/proj/src/main.mty");
        // 1-based 7:14 -> 0-based 6:13.
        assert_eq!(l.line, 6);
        assert_eq!(l.col, 13);
        assert!(l.is_error);
    }

    #[test]
    fn unix_path_location_parses() {
        let mut r = RunPanel::new();
        r.feed("  --[/tmp/x.mty:3:5]\n");
        let l = r.line(0).unwrap();
        assert!(l.clickable);
        assert_eq!(l.file, "/tmp/x.mty");
        assert_eq!(l.line, 2);
        assert_eq!(l.col, 4);
    }

    #[test]
    fn plain_lines_are_not_clickable() {
        let mut r = RunPanel::new();
        r.feed("Compiling demo.mty ...\nHello, world!\n");
        assert!(!r.line(0).unwrap().clickable);
        assert!(!r.line(1).unwrap().clickable);
    }

    #[test]
    fn error_header_marks_error() {
        let mut r = RunPanel::new();
        r.feed("[MT2001] Error: expected `I32`, found `Str`\n");
        let l = r.line(0).unwrap();
        assert!(l.is_error);
        assert!(!l.clickable); // header has no location on this line
    }

    #[test]
    fn ansi_is_stripped_from_output() {
        let mut r = RunPanel::new();
        r.feed("\u{1b}[31mred\u{1b}[0m text\n");
        assert_eq!(r.line(0).unwrap().text, "red text");
    }

    #[test]
    fn scroll_clamps() {
        let mut r = RunPanel::new();
        r.feed("a\nb\nc\n");
        r.scroll(10);
        assert_eq!(r.first(), 2);
        r.scroll(-10);
        assert_eq!(r.first(), 0);
    }

    #[test]
    fn seed_demo_has_clickable_diagnostic() {
        let mut r = RunPanel::new();
        r.seed_demo("C:/proj/demo.mty");
        assert!(r.line_count() > 0);
        assert_eq!(r.exit_code(), Some(1));
        assert!(r.lines.iter().any(|l| l.clickable && l.file == "C:/proj/demo.mty"));
    }

    #[test]
    fn run_real_process_or_skip() {
        // Echo via the system shell would need a shell; instead exercise the
        // lifecycle with a guaranteed-present binary if mty is missing.
        let mty = RunPanel::mty_path();
        if mty == "mty" && which_mty_missing() {
            eprintln!("SKIP: mty not available");
            return;
        }
        // Run on a tiny temp .mty; we only assert the lifecycle terminates.
        let tmp = std::env::temp_dir().join(format!("mui-run-{}.mty", std::process::id()));
        let _ = std::fs::write(&tmp, b"fn main() { }\n");
        let mut r = RunPanel::new();
        if !r.start(&tmp) {
            eprintln!("SKIP: could not spawn mty run");
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        for _ in 0..200 {
            r.pump();
            if !r.is_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(!r.is_running(), "process should have terminated");
        assert!(r.exit_code().is_some());
        let _ = std::fs::remove_file(&tmp);
    }

    fn which_mty_missing() -> bool {
        Command::new("mty").arg("--version").output().is_err()
    }
}
