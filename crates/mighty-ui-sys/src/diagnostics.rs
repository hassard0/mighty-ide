//! Live Mighty diagnostics engine.
//!
//! Runs the Mighty compiler in **check** mode (`mty.exe check <path>`) as a
//! subprocess, captures stdout+stderr, and parses the human-readable diagnostic
//! report into a structured [`Diag`] list the IDE can render.
//!
//! ## Discovered diagnostic format (mty v0.36)
//!
//! `mty check <file>` prints an ariadne-style report with ANSI color codes. A
//! clean file prints a single line `ok: <path>` and exits 0. Each diagnostic is
//! a block of the shape (ANSI stripped):
//!
//! ```text
//! [MT2001] Error: expected `I32`, found `Str`
//!    ╭─[C:/path/to/file.mty:1:1]
//!    │
//!  1 │ fn main() {
//!    │ │
//!    │ ╰─ expected `I32`, found `Str`
//! ───╯
//! ```
//!
//! So each diagnostic has:
//!   * a **header line** `[MT<digits>] <Severity>: <message>` where severity is
//!     `Error` or `Warning`;
//!   * a following **location line** containing `[<path>:<line>:<col>]` with
//!     **1-based** line and column.
//!
//! Multiple diagnostics are separated by blank lines. v0.36 frequently resolves
//! type-error spans to the enclosing function start (`1:1`), and reports only a
//! start column (no end column) — we record `col_end = col_start + 1` so the
//! IDE can draw a minimal one-cell underline. The parser is tolerant: a header
//! with no following location line still yields a [`Diag`] at line/col 0.

use std::path::Path;
use std::process::Command;

/// Severity of a diagnostic. Mirrors the scalar values exposed over the C ABI
/// (`mui_diag_severity`): 0 = error, 1 = warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error = 0,
    Warning = 1,
}

/// One parsed diagnostic. Line/column are **0-based** (converted from the
/// compiler's 1-based report) so they line up with the editor's buffer model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    /// 0-based line index.
    pub line: i32,
    /// 0-based start column (inclusive).
    pub col_start: i32,
    /// 0-based end column (exclusive).
    pub col_end: i32,
    pub severity: Severity,
    /// Diagnostic code, e.g. `MT2001`.
    pub code: String,
    pub message: String,
}

/// Strip ANSI escape sequences (`ESC [ ... m` and friends) from `s`.
///
/// The compiler does not honor `NO_COLOR`, so we always sanitize before
/// parsing. Handles the CSI form `ESC[ ... <final-byte>` which covers the SGR
/// color codes used by the report.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC. Skip an optional '[' then everything up to a byte in 0x40..=0x7e.
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // consume the final byte
                }
            }
        } else {
            // Copy one UTF-8 char starting at i.
            let ch_len = utf8_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            if let Ok(chunk) = std::str::from_utf8(&bytes[i..end]) {
                out.push_str(chunk);
            }
            i = end;
        }
    }
    out
}

/// Public re-export of [`strip_ansi`] so other shim modules (the Run panel) can
/// sanitize compiler output the same way.
pub fn strip_ansi_public(s: &str) -> String {
    strip_ansi(s)
}

/// Number of bytes in a UTF-8 sequence given its first byte.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Parse a diagnostic header line of the form `[MT<digits>] <Severity>: <msg>`.
/// Returns `(code, severity, message)` on a match.
fn parse_header(line: &str) -> Option<(String, Severity, String)> {
    let line = line.trim_start();
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let code = &rest[..close];
    // Code must look like MT followed by digits.
    if !(code.starts_with("MT") && code.len() > 2 && code[2..].chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let after = rest[close + 1..].trim_start();
    let colon = after.find(':')?;
    let sev_word = after[..colon].trim();
    let severity = match sev_word {
        "Error" => Severity::Error,
        "Warning" => Severity::Warning,
        _ => return None,
    };
    let message = after[colon + 1..].trim().to_string();
    Some((code.to_string(), severity, message))
}

/// Parse the `[<path>:<line>:<col>]` location embedded in a report line.
/// Returns **1-based** `(line, col)`. The path may itself contain `:` (e.g.
/// `C:/...`), so we anchor on the LAST two colon-separated numeric fields
/// before the closing `]`.
fn parse_location(line: &str) -> Option<(u32, u32)> {
    let open = line.rfind('[')?;
    let close = line[open..].find(']')? + open;
    let inner = &line[open + 1..close];
    // Split on ':' and take the last two fields as line:col.
    let parts: Vec<&str> = inner.split(':').collect();
    if parts.len() < 2 {
        return None;
    }
    let col: u32 = parts[parts.len() - 1].trim().parse().ok()?;
    let line_no: u32 = parts[parts.len() - 2].trim().parse().ok()?;
    Some((line_no, col))
}

/// Parse the full ANSI-or-plain `mty check` output into a list of [`Diag`].
///
/// Strategy: strip ANSI, then scan lines. Each header line opens a new diag;
/// the next location line (`╭─[...:L:C]`) fills in its position. A clean run
/// (`ok: ...` / no headers) yields an empty list.
pub fn parse_check_output(raw: &str) -> Vec<Diag> {
    let clean = strip_ansi(raw);
    let mut diags: Vec<Diag> = Vec::new();
    let mut pending: Option<Diag> = None;

    for line in clean.lines() {
        if let Some((code, severity, message)) = parse_header(line) {
            // Flush any header that never found a location.
            if let Some(d) = pending.take() {
                diags.push(d);
            }
            pending = Some(Diag {
                line: 0,
                col_start: 0,
                col_end: 1,
                severity,
                code,
                message,
            });
            continue;
        }
        if let Some(d) = pending.as_mut() {
            if let Some((l1, c1)) = parse_location(line) {
                // 1-based -> 0-based (clamp so a reported 0 stays 0).
                d.line = l1.saturating_sub(1) as i32;
                d.col_start = c1.saturating_sub(1) as i32;
                d.col_end = d.col_start + 1;
                diags.push(pending.take().unwrap());
            }
        }
    }
    if let Some(d) = pending.take() {
        diags.push(d);
    }
    diags
}

/// Resolve the path to the `mty` compiler: honor `MIGHTY_MTY`, else fall back to
/// the known dev build path, else bare `mty` (relying on `PATH`).
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

/// Run `mty check <path>` and parse the result. Returns the parsed diagnostics.
/// On spawn failure, logs to stderr and returns an empty list (the IDE simply
/// shows "no diagnostics" rather than crashing).
pub fn run_check(path: &Path) -> Vec<Diag> {
    let mty = mty_path();
    let output = Command::new(&mty).arg("check").arg(path).output();
    match output {
        Ok(out) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            parse_check_output(&combined)
        }
        Err(e) => {
            eprintln!("diagnostics: failed to run `{mty} check`: {e}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_output_yields_no_diags() {
        let raw = "ok: C:/Users/ihass/AppData/Local/Temp/clean.mty\n";
        assert!(parse_check_output(raw).is_empty());
    }

    #[test]
    fn single_error_parsed() {
        // Real ANSI-stripped shape from `mty check` on a type-mismatch file.
        let raw = "\
[MT2001] Error: expected `I32`, found `Str`
   ╭─[C:/Users/ihass/AppData/Local/Temp/with_error.mty:1:1]
   │
 1 │ fn main() {
   │ │
   │ ╰─ expected `I32`, found `Str`
───╯
";
        let diags = parse_check_output(raw);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.code, "MT2001");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "expected `I32`, found `Str`");
        // 1:1 (1-based) -> 0:0 (0-based).
        assert_eq!(d.line, 0);
        assert_eq!(d.col_start, 0);
        assert_eq!(d.col_end, 1);
    }

    #[test]
    fn multiple_errors_parsed() {
        let raw = "\
[MT2001] Error: expected `I32`, found `Str`
   ╭─[C:/Users/ihass/AppData/Local/Temp/multi.mty:2:7]
   │
 2 │   let x: I32 = \"a\"
   │ ╰─ expected `I32`, found `Str`
───╯

[MT2019] Error: function returns `I32`, body produces `Bool`
   ╭─[C:/Users/ihass/AppData/Local/Temp/multi.mty:5:3]
   │
 5 │   let y: Bool = 5
   │ ╰─ function returns `I32`, body produces `Bool`
───╯
";
        let diags = parse_check_output(raw);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].code, "MT2001");
        assert_eq!(diags[0].line, 1); // line 2 -> 0-based 1
        assert_eq!(diags[0].col_start, 6); // col 7 -> 0-based 6
        assert_eq!(diags[0].col_end, 7);
        assert_eq!(diags[1].code, "MT2019");
        assert_eq!(diags[1].severity, Severity::Error);
        assert_eq!(diags[1].line, 4);
        assert_eq!(diags[1].col_start, 2);
    }

    #[test]
    fn warning_severity_parsed() {
        let raw = "\
[MT3001] Warning: unused variable `x`
   ╭─[/tmp/warn.mty:3:5]
   │
───╯
";
        let diags = parse_check_output(raw);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].code, "MT3001");
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].col_start, 4);
    }

    #[test]
    fn ansi_colored_output_is_stripped() {
        // The actual compiler output is heavily ANSI-colored.
        let raw = "\u{1b}[31m[MT2001] Error:\u{1b}[0m expected `I32`, found `Str`\n   \u{1b}[38;5;246m╭\u{1b}[0m\u{1b}[38;5;246m─\u{1b}[0m\u{1b}[38;5;246m[\u{1b}[0mC:/Users/ihass/AppData/Local/Temp/with_error.mty:1:1\u{1b}[38;5;246m]\u{1b}[0m\n";
        let diags = parse_check_output(raw);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "MT2001");
        assert_eq!(diags[0].message, "expected `I32`, found `Str`");
        assert_eq!(diags[0].line, 0);
        assert_eq!(diags[0].col_start, 0);
    }

    #[test]
    fn header_without_location_still_yields_diag() {
        let raw = "[MT9999] Error: mystery error with no location\n";
        let diags = parse_check_output(raw);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 0);
        assert_eq!(diags[0].col_start, 0);
        assert_eq!(diags[0].message, "mystery error with no location");
    }

    #[test]
    fn non_diagnostic_noise_ignored() {
        let raw = "\
compiling...
[MT2001] Error: boom
   ╭─[/x.mty:10:4]
some trailing note
done
";
        let diags = parse_check_output(raw);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 9);
        assert_eq!(diags[0].col_start, 3);
    }
}
