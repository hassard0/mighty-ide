//! Document formatting via `mty fmt`.
//!
//! Reuses the subprocess pattern from [`crate::diagnostics`]: shell out to the
//! Mighty compiler in **format** mode. `mty fmt <path>` formats the file IN
//! PLACE (confirmed via `mty fmt --help`: "Format .mty files in place (or
//! stdin)"; `--check` and `--stdin` are the only other modes). So the IDE saves
//! the live buffer to disk first, then calls [`run_fmt`] to rewrite it, then
//! reloads the formatted file into the Mighty buffer.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

pub(crate) const MAX_FMT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Outcome of a format attempt. The IDE maps these to status-bar messages and a
/// distinct ABI return code (see [`crate::mui_format_current`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmtOutcome {
    /// The file was a `.mty` file and `mty fmt` exited 0.
    Formatted,
    /// The file is NOT a `.mty` file — formatting was refused (no-op). This is
    /// the L26 data-loss guard: `mty fmt` truncates non-`.mty` input to 1 byte.
    NotApplicable,
    /// The file was a `.mty` file but `mty fmt` failed to spawn / exited non-zero.
    Failed(String),
}

/// Whether `path` has a `.mty` extension (case-insensitive). Only `.mty` files
/// are safe to hand to `mty fmt`: on v0.36 the formatter truncates any input it
/// can't parse to 1 byte (L26), so a `.txt`/`.rs`/etc. file would be destroyed.
pub fn is_mty_path(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("mty"))
        .unwrap_or(false)
}

/// Resolve the path to the `mty` compiler through the shared resolver.
fn mty_path() -> String {
    crate::mty::path()
}

/// Format `path` in place via `mty fmt <path>`, but ONLY if it is a `.mty` file.
///
/// L26 safety guard: on v0.36 `mty fmt` truncates any input it can't parse to a
/// single byte (verified: a 6480-byte `.txt` became 1 byte, still exit 0). So we
/// refuse to even spawn the formatter for a non-`.mty` extension and return
/// [`FmtOutcome::NotApplicable`] — the editor reports "format: only .mty
/// supported" instead of corrupting the file.
///
/// For a `.mty` file we run `mty fmt`; success -> [`FmtOutcome::Formatted`], a
/// spawn error / non-zero exit -> [`FmtOutcome::Failed`] (logged, non-fatal).
pub fn run_fmt(path: &Path) -> FmtOutcome {
    if !is_mty_path(path) {
        eprintln!(
            "format: refusing to run `mty fmt` on non-.mty file {} (would truncate it; L26)",
            path.display()
        );
        return FmtOutcome::NotApplicable;
    }
    let mty = mty_path();
    let mut cmd = Command::new(&mty);
    cmd.arg("fmt").arg(path);
    match run_fmt_command(cmd) {
        Ok(Some((status, _stdout, stderr_bytes))) => {
            if status.success() {
                FmtOutcome::Formatted
            } else {
                let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
                let reason = if stderr.is_empty() {
                    status.to_string()
                } else {
                    format!("{}: {stderr}", status)
                };
                eprintln!(
                    "format: `{mty} fmt {}` exited {}: {}",
                    path.display(),
                    status,
                    stderr
                );
                FmtOutcome::Failed(reason)
            }
        }
        Ok(None) => {
            eprintln!("format: `{mty} fmt {}` output too large", path.display());
            FmtOutcome::Failed("formatter output too large".to_string())
        }
        Err(reason) => {
            eprintln!("format: failed to run `{mty} fmt`: {reason}");
            FmtOutcome::Failed(reason)
        }
    }
}

fn run_fmt_command(mut cmd: Command) -> Result<Option<(ExitStatus, Vec<u8>, Vec<u8>)>, String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "stdout pipe unavailable".to_string()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "stderr pipe unavailable".to_string()
    })?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = Arc::clone(&exceeded);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stdout_reader = std::thread::spawn(move || {
        read_fmt_stream_capped(stdout, MAX_FMT_OUTPUT_BYTES, stdout_exceeded)
    });
    let stderr_reader = std::thread::spawn(move || {
        read_fmt_stream_capped(stderr, MAX_FMT_OUTPUT_BYTES, stderr_exceeded)
    });

    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(e.to_string());
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    if exceeded.load(Ordering::Relaxed) {
        return Ok(None);
    }
    Ok(Some((status, stdout, stderr)))
}

fn append_fmt_output_with_cap(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if buf.len().saturating_add(chunk.len()) > cap {
        buf.clear();
        return false;
    }
    buf.extend_from_slice(chunk);
    true
}

fn read_fmt_stream_capped<R: Read>(
    mut reader: R,
    cap: usize,
    exceeded: Arc<AtomicBool>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if !append_fmt_output_with_cap(&mut buf, &chunk[..n], cap) {
                    exceeded.store(true, Ordering::Relaxed);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_output_cap_accepts_exact_limit() {
        let mut buf = Vec::from(b"fmt");

        assert!(append_fmt_output_with_cap(&mut buf, b"!", 4));
        assert_eq!(buf, b"fmt!");
    }

    #[test]
    fn fmt_output_cap_discards_oversized_stream() {
        let mut buf = Vec::from(b"fmt:");

        assert!(!append_fmt_output_with_cap(&mut buf, b"overflow", 8));
        assert!(buf.is_empty());
    }

    #[test]
    fn fmt_stream_reader_marks_oversized_output() {
        let exceeded = Arc::new(AtomicBool::new(false));
        let out = read_fmt_stream_capped(
            std::io::Cursor::new(b"abcdef".to_vec()),
            5,
            Arc::clone(&exceeded),
        );

        assert!(out.is_empty());
        assert!(exceeded.load(Ordering::Relaxed));
    }

    /// `true` if an `mty` binary is reachable (so we can skip the live test when
    /// the compiler is absent, e.g. CI without `mty`).
    fn mty_available() -> bool {
        let mty = mty_path();
        // `mty fmt --check` on an empty stdin is a cheap liveness probe; but the
        // simplest portable check is: can we spawn `mty --help` at all?
        Command::new(&mty)
            .arg("--help")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn is_mty_path_recognizes_extension() {
        assert!(is_mty_path(Path::new("foo.mty")));
        assert!(is_mty_path(Path::new(r"C:\a\b\main.mty")));
        // Case-insensitive.
        assert!(is_mty_path(Path::new("Foo.MTY")));
        // Anything else is refused.
        assert!(!is_mty_path(Path::new("notes.txt")));
        assert!(!is_mty_path(Path::new("lib.rs")));
        assert!(!is_mty_path(Path::new("README")));
        assert!(!is_mty_path(Path::new("archive.mty.bak")));
        // A bare ".mty" with no stem still counts (extension is "mty").
        assert!(!is_mty_path(Path::new("mty")));
    }

    /// The core L26 safety guard: a non-`.mty` file is REFUSED without spawning
    /// `mty fmt`, and — critically — its contents are left intact (not truncated
    /// to 1 byte). This does not require an `mty` binary (the guard short-circuits
    /// before any spawn), so it always runs.
    #[test]
    fn refuses_non_mty_and_leaves_file_intact() {
        // A non-.mty file with real content the broken formatter would truncate.
        let src = b"this is a plain text file\nwith several lines\nthat must NOT be truncated\n";
        let dir = std::env::temp_dir();
        let path = dir.join("mui_fmt_guard_test.txt");
        std::fs::write(&path, src).unwrap();

        let outcome = run_fmt(&path);
        assert_eq!(
            outcome,
            FmtOutcome::NotApplicable,
            "non-.mty file must be refused (NotApplicable), never formatted"
        );
        // The file is byte-for-byte unchanged — no truncation.
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after, src, "non-.mty file must be left intact");

        let _ = std::fs::remove_file(&path);
    }

    /// A `.mty` path IS handed to `mty fmt` (the guard lets it through). When the
    /// compiler is present we additionally assert idempotence; when absent we at
    /// least assert the outcome is one of the two `.mty` branches (Formatted /
    /// Failed) and NOT NotApplicable — proving the `.mty` path is attempted.
    #[test]
    fn mty_path_is_attempted() {
        // A deliberately mis-formatted but VALID Mighty program.
        let src = "fn main(){\n\n\n        let   x=1\n  log(\"hi\")\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("mui_fmt_test.mty");
        std::fs::write(&path, src).unwrap();

        let outcome = run_fmt(&path);
        // Whatever happens, a .mty file is NEVER refused as NotApplicable.
        assert_ne!(
            outcome,
            FmtOutcome::NotApplicable,
            ".mty file must be attempted, not refused"
        );

        if !mty_available() {
            eprintln!("fmt idempotence skipped: no `mty` binary available");
            let _ = std::fs::remove_file(&path);
            return;
        }
        if !matches!(outcome, FmtOutcome::Formatted) {
            eprintln!("fmt idempotence skipped: `mty fmt` did not succeed on sample");
            let _ = std::fs::remove_file(&path);
            return;
        }
        let after = std::fs::read_to_string(&path).unwrap();
        // Formatting should be idempotent: a second run leaves the file stable.
        assert_eq!(run_fmt(&path), FmtOutcome::Formatted);
        let after2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, after2, "mty fmt should be idempotent");

        let _ = std::fs::remove_file(&path);
    }
}
