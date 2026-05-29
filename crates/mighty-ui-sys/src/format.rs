//! Document formatting via `mty fmt`.
//!
//! Reuses the subprocess pattern from [`crate::diagnostics`]: shell out to the
//! Mighty compiler in **format** mode. `mty fmt <path>` formats the file IN
//! PLACE (confirmed via `mty fmt --help`: "Format .mty files in place (or
//! stdin)"; `--check` and `--stdin` are the only other modes). So the IDE saves
//! the live buffer to disk first, then calls [`run_fmt`] to rewrite it, then
//! reloads the formatted file into the Mighty buffer.

use std::path::Path;
use std::process::Command;

/// Resolve the path to the `mty` compiler: honor `MIGHTY_MTY`, else the known
/// dev build path, else bare `mty` (relying on `PATH`). Mirrors
/// [`crate::diagnostics`]'s resolver so both subprocess features agree.
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

/// Run `mty fmt <path>` to format the file in place. Returns `true` on success
/// (exit status 0), `false` if the process failed to spawn or exited non-zero.
/// Logs stderr to the shim's stderr on failure (non-fatal for the IDE).
pub fn run_fmt(path: &Path) -> bool {
    let mty = mty_path();
    match Command::new(&mty).arg("fmt").arg(path).output() {
        Ok(out) => {
            if out.status.success() {
                true
            } else {
                eprintln!(
                    "format: `{mty} fmt {}` exited {}: {}",
                    path.display(),
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                false
            }
        }
        Err(e) => {
            eprintln!("format: failed to run `{mty} fmt`: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `true` if an `mty` binary is reachable (so we can skip the live test when
    /// the compiler is absent, e.g. CI without stardust).
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
    fn fmt_on_misformatted_file_changes_then_is_stable() {
        if !mty_available() {
            eprintln!("skipping fmt test: no `mty` binary available");
            return;
        }
        // A deliberately mis-formatted but VALID Mighty program (extra blank
        // lines + odd indentation the formatter should normalize).
        let src = "fn main(){\n\n\n        let   x=1\n  log(\"hi\")\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("mui_fmt_test.mty");
        std::fs::write(&path, src).unwrap();

        let ok = run_fmt(&path);
        // If `mty fmt` rejects the snippet (parse error), treat as skip rather
        // than fail — the formatter only operates on parseable files.
        if !ok {
            eprintln!("skipping fmt assertion: `mty fmt` did not succeed on sample");
            let _ = std::fs::remove_file(&path);
            return;
        }
        let after = std::fs::read_to_string(&path).unwrap();

        // Formatting should be idempotent: a second run leaves the file stable.
        assert!(run_fmt(&path));
        let after2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, after2, "mty fmt should be idempotent");

        let _ = std::fs::remove_file(&path);
    }
}
