//! Test panel: run `mty test` over the active file's package on a background
//! thread, stream its cargo-test-style report, and fold it into a results tree
//! (one row per `test NAME ... ok|FAILED`, plus a running/finished summary).
//!
//! Reuses the exact spawn/pump shape of [`crate::run::RunPanel`] (a reader
//! thread per pipe draining into a shared buffer; [`TestPanel::pump`] folds new
//! bytes into rows each frame) so the UI never blocks. v0.36 Mighty can't run
//! processes / hold strings across FFI (L17/L21), so the whole thing lives
//! shim-side and is driven through the scalar `mui_test_*` ABI.
//!
//! ## `mty test` output (v0.36, confirmed)
//! ```text
//! test sample.test::test_adds ... ok
//! test sample.test::test_fails ... FAILED
//!   reason: trap MT5001: boom
//! test sample.test::test_passes_again ... ok
//!
//! test result: 2 passed; 1 failed; 3 total
//! ```
//! There is NO `running N tests` header and NO duration in the summary line
//! (we time the run ourselves). `mty test` takes no test-name filter, so
//! "Run Test at Cursor" re-runs the whole package (the ABI still records the
//! cursor test name so the UI can highlight it). Discovery is `tests/*.test.mty`
//! under `--manifest-dir <pkg>`.

#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A test's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Discovered but not yet resolved (streamed but no `... ok/FAILED` yet).
    Pending,
    Passed,
    Failed,
}

impl Status {
    /// Scalar code for the ABI: 0 pending, 1 passed, 2 failed.
    pub fn as_i32(self) -> i32 {
        match self {
            Status::Pending => 0,
            Status::Passed => 1,
            Status::Failed => 2,
        }
    }
}

/// One row in the results tree: a test result with its display name + optional
/// failure detail + a resolved click target.
#[derive(Debug, Clone)]
pub struct TestRow {
    /// Full reported name, e.g. `sample.test::test_adds`.
    pub full_name: String,
    /// The short test fn name (after the last `::`), e.g. `test_adds`.
    pub short_name: String,
    /// The file stem before `::` (the test suite), e.g. `sample.test`.
    pub suite: String,
    pub status: Status,
    /// Failure message (the `reason:` line), empty when passing.
    pub message: String,
}

impl TestRow {
    fn new(full_name: String, status: Status) -> Self {
        let short_name = full_name
            .rsplit("::")
            .next()
            .unwrap_or(&full_name)
            .to_string();
        let suite = full_name
            .rsplit_once("::")
            .map(|(s, _)| s.to_string())
            .unwrap_or_default();
        TestRow {
            full_name,
            short_name,
            suite,
            status,
            message: String::new(),
        }
    }
}

/// Shim-owned Test panel state: the spawned `mty test` child + reader thread,
/// the parsed rows, the running/finished summary, scroll, and a click target.
#[derive(Default)]
pub struct TestPanel {
    /// `true` while the Test panel is the active sidebar panel.
    active: bool,
    /// Result rows, in report order.
    rows: Vec<TestRow>,
    /// Top visible row (scroll offset within the rows list).
    first: usize,
    /// `true` while `mty test` is still running.
    running: bool,
    /// Counts parsed so far (live during the run, final at the summary).
    passed: usize,
    failed: usize,
    /// Total from the summary line (`0` until the summary parses; otherwise the
    /// authoritative count). While running, callers use [`Self::count`].
    total: usize,
    /// Wall-clock duration of the last/current run, ms.
    duration_ms: u128,
    /// The package dir we ran `mty test --manifest-dir` against (for the header).
    pkg: String,
    /// The cursor test name "Run Test at Cursor" recorded (highlight target).
    focus_test: String,
    /// Carry buffer for a partial last line (no trailing newline yet).
    partial: String,
    /// The last row whose `... FAILED` we saw, so a following `reason:` line
    /// attaches to it.
    last_failed: Option<usize>,

    // ---- background process plumbing (lazily set on `start`) ----
    out: Option<Arc<Mutex<Vec<u8>>>>,
    done: Option<Receiver<()>>,
    child: Option<std::process::Child>,
    started: Option<Instant>,
    /// The resolved click target (path, 0-based line, 0-based col) from the last
    /// row click, read back by the IDE to jump the editor.
    click_target: Option<(String, i32, i32)>,
}

impl TestPanel {
    pub fn new() -> Self {
        TestPanel::default()
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
    pub fn passed(&self) -> usize {
        self.passed
    }
    pub fn failed(&self) -> usize {
        self.failed
    }
    /// Total tests: the summary's total once parsed, else the live row count.
    pub fn total(&self) -> usize {
        if self.total > 0 {
            self.total
        } else {
            self.rows.len()
        }
    }
    pub fn duration_ms(&self) -> u128 {
        self.duration_ms
    }
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
    pub fn first(&self) -> usize {
        self.first
    }
    pub fn pkg(&self) -> &str {
        &self.pkg
    }
    pub fn focus_test(&self) -> &str {
        &self.focus_test
    }
    pub fn row(&self, i: usize) -> Option<&TestRow> {
        self.rows.get(i)
    }
    pub fn click_target(&self) -> Option<&(String, i32, i32)> {
        self.click_target.as_ref()
    }
    pub fn set_click_target(&mut self, t: Option<(String, i32, i32)>) {
        self.click_target = t;
    }

    /// Scroll by `delta` rows (clamped).
    pub fn scroll(&mut self, delta: i32) {
        let max = self.rows.len().saturating_sub(1) as i32;
        let mut f = self.first as i32 + delta;
        if f < 0 {
            f = 0;
        }
        if f > max.max(0) {
            f = max.max(0);
        }
        self.first = f as usize;
    }

    /// Resolve `mty`. Mirrors [`crate::run`]'s resolver (honors `MIGHTY_MTY`,
    /// else the dev build, else bare `mty`).
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

    /// The package directory for `file`: the nearest ancestor that contains a
    /// `mighty.toml`, else the file's parent directory.
    pub fn package_dir(file: &Path) -> PathBuf {
        let start = file.parent().unwrap_or(file);
        let mut cur = Some(start);
        while let Some(dir) = cur {
            if dir.join("mighty.toml").exists() {
                return dir.to_path_buf();
            }
            cur = dir.parent();
        }
        start.to_path_buf()
    }

    /// Start `mty test --manifest-dir <pkg>` for the package owning `file`, on
    /// background reader threads. Kills any prior run, clears rows, resets
    /// counts + timing. `focus` is the optional cursor test name to highlight
    /// (re-runs all the same — `mty test` has no name filter). Returns `true` if
    /// the process spawned.
    pub fn start(&mut self, file: &Path, focus: Option<String>) -> bool {
        use std::process::{Command, Stdio};
        self.stop();
        self.rows.clear();
        self.partial.clear();
        self.first = 0;
        self.passed = 0;
        self.failed = 0;
        self.total = 0;
        self.duration_ms = 0;
        self.last_failed = None;
        self.click_target = None;
        self.focus_test = focus.unwrap_or_default();
        self.active = true;

        let pkg = Self::package_dir(file);
        self.pkg = pkg.to_string_lossy().into_owned();

        let mty = Self::mty_path();
        let mut cmd = Command::new(&mty);
        cmd.arg("test").arg("--manifest-dir").arg(&pkg);
        cmd.current_dir(&pkg);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // Surface the spawn failure as a synthetic failed row.
                let mut r = TestRow::new("<spawn>".to_string(), Status::Failed);
                r.message = format!("failed to run `{mty} test`: {e}");
                self.rows.push(r);
                self.failed = 1;
                self.running = false;
                return false;
            }
        };

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (done_tx, done_rx) = mpsc::channel();
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

    /// Stop the running `mty test` (best-effort kill + reap). No-op if idle.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.out = None;
        self.done = None;
        if self.running {
            self.running = false;
            if let Some(s) = self.started.take() {
                self.duration_ms = s.elapsed().as_millis();
            }
        }
        self.started = None;
    }

    /// Drain pending output into the rows + detect completion. Returns `true` if
    /// anything changed this frame (so the IDE redraws). Call once per frame.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        if let Some(out) = &self.out {
            let chunk = match out.lock() {
                Ok(mut g) if !g.is_empty() => std::mem::take(&mut *g),
                _ => Vec::new(),
            };
            if !chunk.is_empty() {
                let text = String::from_utf8_lossy(&chunk).into_owned();
                self.feed(&text);
                changed = true;
            }
        }
        if self.running {
            if let Some(done) = &self.done {
                match done.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => {
                        if let Some(out) = self.out.take() {
                            if let Ok(g) = out.lock() {
                                if !g.is_empty() {
                                    let text = String::from_utf8_lossy(&g).into_owned();
                                    self.feed(&text);
                                }
                            }
                        }
                        self.flush_partial();
                        self.running = false;
                        if let Some(mut child) = self.child.take() {
                            let _ = child.wait();
                        }
                        if let Some(s) = self.started.take() {
                            self.duration_ms = s.elapsed().as_millis();
                        }
                        self.done = None;
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
        }
        changed
    }

    /// Append a chunk (possibly partial): split on newlines, carry an
    /// unterminated tail, parse each completed line.
    fn feed(&mut self, chunk: &str) {
        let clean = strip_ansi(chunk);
        let mut buf = std::mem::take(&mut self.partial);
        buf.push_str(&clean);
        let buf = buf.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts: Vec<&str> = buf.split('\n').collect();
        let tail = parts.pop().unwrap_or("").to_string();
        for line in parts {
            self.parse_line(line);
        }
        self.partial = tail;
    }

    fn flush_partial(&mut self) {
        if !self.partial.is_empty() {
            let p = std::mem::take(&mut self.partial);
            self.parse_line(&p);
        }
    }

    /// Parse one complete report line into the model:
    ///   * `test NAME ... ok|FAILED`  -> a result row;
    ///   * `  reason: <msg>`          -> the message for the last failed row;
    ///   * `test result: X passed; Y failed; Z total` -> the summary.
    fn parse_line(&mut self, line: &str) {
        let trimmed = line.trim_end();
        // Summary line.
        if let Some(rest) = trimmed.trim_start().strip_prefix("test result:") {
            if let Some((p, f, t)) = parse_summary(rest) {
                self.passed = p;
                self.failed = f;
                self.total = t;
            }
            return;
        }
        // Per-test result: `test <name> ... ok` / `... FAILED`.
        if let Some(rest) = trimmed.strip_prefix("test ") {
            if let Some((name, ok)) = parse_result(rest) {
                let status = if ok { Status::Passed } else { Status::Failed };
                let row = TestRow::new(name, status);
                let idx = self.rows.len();
                self.rows.push(row);
                match status {
                    Status::Passed => self.passed += 1,
                    Status::Failed => {
                        self.failed += 1;
                        self.last_failed = Some(idx);
                    }
                    Status::Pending => {}
                }
                return;
            }
        }
        // A `reason:` detail line attaches to the most recent failed row.
        let reason = trimmed.trim_start();
        if let Some(msg) = reason.strip_prefix("reason:") {
            if let Some(idx) = self.last_failed {
                if let Some(r) = self.rows.get_mut(idx) {
                    r.message = msg.trim().to_string();
                }
            }
        }
    }

    /// Resolve a clicked row to a jump target: the test's `fn NAME` declaration
    /// inside its package's `tests/` dir. Returns `(abs_path, line0, col0)` if
    /// found. Used for click-to-jump (the report has no per-test location, so we
    /// locate the declaration by scanning the package's test files).
    pub fn resolve_row_target(&self, i: usize) -> Option<(PathBuf, i32, i32)> {
        let row = self.rows.get(i)?;
        if self.pkg.is_empty() {
            return None;
        }
        let tests_dir = PathBuf::from(&self.pkg).join("tests");
        locate_test_fn(&tests_dir, &row.short_name)
    }

    /// Seed sample results for a headless screenshot (a mix of pass/fail + a
    /// failure message + the summary), without spawning a process.
    pub fn seed_demo(&mut self, pkg: &str) {
        self.pkg = pkg.to_string();
        self.active = true;
        self.running = false;
        self.rows.clear();
        self.first = 0;
        self.last_failed = None;
        let seed = [
            ("arith.test::test_addition", true, ""),
            ("arith.test::test_subtraction", true, ""),
            ("arith.test::test_overflow_wraps", true, ""),
            (
                "parser.test::test_rejects_empty",
                false,
                "trap MT5001: assertion failed: tokens.len() > 0",
            ),
            ("parser.test::test_parses_let", true, ""),
            (
                "parser.test::test_nested_blocks",
                false,
                "trap MT0901: index out of bounds: len 3, idx 5",
            ),
            ("vm.test::test_push_pop", true, ""),
            ("vm.test::test_call_returns", true, ""),
        ];
        for (name, ok, msg) in seed {
            let status = if ok { Status::Passed } else { Status::Failed };
            let mut r = TestRow::new(name.to_string(), status);
            r.message = msg.to_string();
            let idx = self.rows.len();
            self.rows.push(r);
            if ok {
                self.passed += 1;
            } else {
                self.failed += 1;
                self.last_failed = Some(idx);
            }
        }
        self.total = self.rows.len();
        self.duration_ms = 218;
    }
}

/// Parse the per-test result body (everything after `test `): split on the
/// ` ... ` separator into the name + outcome word. Returns `(name, passed?)`.
fn parse_result(rest: &str) -> Option<(String, bool)> {
    let (name, outcome) = rest.rsplit_once(" ... ")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let outcome = outcome.trim();
    if outcome.eq_ignore_ascii_case("ok") {
        Some((name.to_string(), true))
    } else if outcome.eq_ignore_ascii_case("FAILED") || outcome.eq_ignore_ascii_case("fail") {
        Some((name.to_string(), false))
    } else {
        None
    }
}

/// Parse `" X passed; Y failed; Z total"` (after the `test result:` prefix) into
/// `(passed, failed, total)`. Tolerant of extra whitespace; total may be absent
/// (then derived as passed+failed).
fn parse_summary(rest: &str) -> Option<(usize, usize, usize)> {
    let mut passed = None;
    let mut failed = None;
    let mut total = None;
    for seg in rest.split(';') {
        let seg = seg.trim().trim_end_matches('.');
        let mut it = seg.split_whitespace();
        let (Some(num), Some(word)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(n) = num.parse::<usize>() else { continue };
        match word {
            "passed" => passed = Some(n),
            "failed" => failed = Some(n),
            "total" => total = Some(n),
            _ => {}
        }
    }
    let p = passed?;
    let f = failed.unwrap_or(0);
    let t = total.unwrap_or(p + f);
    Some((p, f, t))
}

/// Scan `dir` recursively for a top-level `fn <name>(` declaration; return its
/// `(file, line0, col0)`. The col points at the `fn` keyword's `name` start.
fn locate_test_fn(dir: &Path, name: &str) -> Option<(PathBuf, i32, i32)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let needle = format!("fn {name}");
    for ent in entries.flatten() {
        let path = ent.path();
        if path.is_dir() {
            if let Some(hit) = locate_test_fn(&path, name) {
                return Some(hit);
            }
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (lineno, line) in text.lines().enumerate() {
            if let Some(pos) = line.find(&needle) {
                // Verify it's `fn name` followed by `(` or `<` or whitespace
                // (avoid `fn test_adds_more` matching `test_adds`).
                let after = &line[pos + needle.len()..];
                let next = after.chars().next();
                if matches!(next, Some('(') | Some('<') | Some(' ') | None) {
                    let col = pos + 3; // skip "fn "
                    return Some((path.clone(), lineno as i32, col as i32));
                }
            }
        }
    }
    None
}

/// Strip ANSI escape sequences (reuse the diagnostics scanner).
fn strip_ansi(s: &str) -> String {
    crate::diagnostics::strip_ansi_public(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_passing() {
        let mut t = TestPanel::new();
        t.feed("test sample.test::test_a ... ok\ntest sample.test::test_b ... ok\n");
        assert_eq!(t.row_count(), 2);
        assert_eq!(t.row(0).unwrap().short_name, "test_a");
        assert_eq!(t.row(0).unwrap().suite, "sample.test");
        assert_eq!(t.row(0).unwrap().status, Status::Passed);
        assert_eq!(t.row(1).unwrap().status, Status::Passed);
    }

    #[test]
    fn parses_mixed_pass_fail() {
        let mut t = TestPanel::new();
        t.feed("test s::test_a ... ok\ntest s::test_b ... FAILED\ntest s::test_c ... ok\n");
        assert_eq!(t.row_count(), 3);
        assert_eq!(t.row(0).unwrap().status, Status::Passed);
        assert_eq!(t.row(1).unwrap().status, Status::Failed);
        assert_eq!(t.row(2).unwrap().status, Status::Passed);
        // Live counts track before any summary line.
        assert_eq!(t.passed(), 2);
        assert_eq!(t.failed(), 1);
    }

    #[test]
    fn parses_summary_line() {
        let mut t = TestPanel::new();
        t.feed("\ntest result: 2 passed; 1 failed; 3 total\n");
        assert_eq!(t.passed(), 2);
        assert_eq!(t.failed(), 1);
        assert_eq!(t.total(), 3);
    }

    #[test]
    fn summary_without_total_derives_it() {
        let mut t = TestPanel::new();
        t.feed("test result: 4 passed; 2 failed\n");
        assert_eq!(t.total(), 6);
    }

    #[test]
    fn failure_message_attaches_to_failed_row() {
        let mut t = TestPanel::new();
        t.feed("test s::test_x ... FAILED\n  reason: trap MT5001: boom\n");
        let r = t.row(0).unwrap();
        assert_eq!(r.status, Status::Failed);
        assert_eq!(r.message, "trap MT5001: boom");
    }

    #[test]
    fn reason_only_attaches_to_the_most_recent_failure() {
        let mut t = TestPanel::new();
        t.feed("test s::test_a ... ok\n  reason: stray line\n");
        // No failed row yet -> the stray reason is dropped, test_a stays clean.
        assert_eq!(t.row(0).unwrap().message, "");
    }

    #[test]
    fn streamed_in_partial_chunks_parses_the_same() {
        let mut t = TestPanel::new();
        // Split mid-line across feeds; the carry buffer must stitch it back.
        t.feed("test s::test_a ... o");
        assert_eq!(t.row_count(), 0); // line not yet complete
        t.feed("k\ntest s::test_");
        assert_eq!(t.row_count(), 1);
        t.feed("b ... FAILED\n  reason: nope\n");
        assert_eq!(t.row_count(), 2);
        assert_eq!(t.row(1).unwrap().status, Status::Failed);
        assert_eq!(t.row(1).unwrap().message, "nope");
    }

    #[test]
    fn ansi_is_stripped() {
        let mut t = TestPanel::new();
        t.feed("test s::test_a ... \u{1b}[32mok\u{1b}[0m\n");
        assert_eq!(t.row(0).unwrap().status, Status::Passed);
    }

    #[test]
    fn full_report_end_to_end() {
        let mut t = TestPanel::new();
        let report = "\
test sample.test::test_adds ... ok
test sample.test::test_fails ... FAILED
  reason: trap MT5001: boom: expected 2 got 3
test sample.test::test_passes_again ... ok

test result: 2 passed; 1 failed; 3 total
";
        t.feed(report);
        assert_eq!(t.row_count(), 3);
        assert_eq!(t.passed(), 2);
        assert_eq!(t.failed(), 1);
        assert_eq!(t.total(), 3);
        assert_eq!(t.row(1).unwrap().short_name, "test_fails");
        assert_eq!(t.row(1).unwrap().message, "trap MT5001: boom: expected 2 got 3");
    }

    #[test]
    fn scroll_clamps() {
        let mut t = TestPanel::new();
        t.feed("test s::a ... ok\ntest s::b ... ok\ntest s::c ... ok\n");
        t.scroll(10);
        assert_eq!(t.first(), 2);
        t.scroll(-10);
        assert_eq!(t.first(), 0);
    }

    #[test]
    fn seed_demo_has_mixed_results_and_summary() {
        let mut t = TestPanel::new();
        t.seed_demo("C:/proj/demo");
        assert!(t.row_count() > 0);
        assert!(t.passed() > 0 && t.failed() > 0);
        assert_eq!(t.total(), t.row_count());
        assert!(t.rows.iter().any(|r| r.status == Status::Failed && !r.message.is_empty()));
    }

    #[test]
    fn package_dir_finds_nearest_manifest() {
        let root = std::env::temp_dir().join(format!("mui-pkgdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("mighty.toml"), b"[package]\nname=\"x\"\n").unwrap();
        let f = root.join("tests").join("a.test.mty");
        std::fs::write(&f, b"fn test_a() {}\n").unwrap();
        assert_eq!(TestPanel::package_dir(&f), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn locate_test_fn_finds_declaration_and_skips_prefix_collisions() {
        let root = std::env::temp_dir().join(format!("mui-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let body = "fn test_adds_more() {}\nfn helper() {}\nfn test_adds() {}\n";
        std::fs::write(root.join("a.test.mty"), body).unwrap();
        // `test_adds` must match line 2 (0-based), not `test_adds_more` on line 0.
        let (path, line, col) = locate_test_fn(&root, "test_adds").unwrap();
        assert_eq!(line, 2);
        assert_eq!(col, 3);
        assert!(path.ends_with("a.test.mty"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_real_mty_test_or_skip() {
        // Guarded integration test: build a tiny package with a passing + a
        // failing test, run REAL `mty test`, assert the parsed counts. Skips if
        // `mty` is not resolvable.
        let mty = TestPanel::mty_path();
        if mty == "mty" && std::process::Command::new("mty").arg("--version").output().is_err() {
            eprintln!("SKIP: mty not available");
            return;
        }
        let root = std::env::temp_dir().join(format!("mui-mtytest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("mighty.toml"), b"[package]\nname = \"t\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(
            root.join("tests").join("sample.test.mty"),
            b"fn test_pass_one() {\n  let x = 1 + 1\n}\n\nfn test_fail_one() {\n  panic(\"boom\")\n}\n\nfn test_pass_two() {\n  let y = 2 + 2\n}\n",
        )
        .unwrap();
        let target = root.join("tests").join("sample.test.mty");
        let mut t = TestPanel::new();
        if !t.start(&target, None) {
            eprintln!("SKIP: could not spawn mty test");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        for _ in 0..400 {
            t.pump();
            if !t.is_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(!t.is_running(), "mty test should have terminated");
        assert_eq!(t.passed(), 2, "expected 2 passing tests, rows={:?}", t.rows);
        assert_eq!(t.failed(), 1, "expected 1 failing test, rows={:?}", t.rows);
        assert_eq!(t.total(), 3);
        // The failing row should carry a reason message.
        let failed = t.rows.iter().find(|r| r.status == Status::Failed).unwrap();
        assert!(!failed.message.is_empty(), "failed test should have a reason");
        // Click-to-jump: locate the failing test's declaration.
        let fail_idx = t.rows.iter().position(|r| r.status == Status::Failed).unwrap();
        let target = t.resolve_row_target(fail_idx);
        assert!(target.is_some(), "should locate the failing test fn declaration");
        let _ = std::fs::remove_dir_all(&root);
    }
}
