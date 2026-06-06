//! Git blame gutter (shim-side, scalar-driven from Mighty).
//!
//! Toggled by the "Git: Toggle Blame" palette command + a `mui_chord` key, the
//! blame gutter runs `git -C <root> blame --porcelain <file>` and parses the
//! per-line author + short-date + short-sha, then the IDE renders it dimly in/
//! next to the editor gutter. Per L21 the entire parsed model + cache live here;
//! Mighty only toggles it, queries `mui_blame_active`, and reads per-line fields.
//!
//! The `--porcelain` parser ([`parse_porcelain`]) is pure + unit-tested. It
//! handles git's INCREMENTAL commit headers: a commit's metadata (author /
//! author-time / summary / …) is emitted only the FIRST time that commit appears
//! in the output; later lines for the same commit carry just the
//! `<sha> <orig> <final> [<count>]` group line, so we cache commit metadata by
//! sha and back-fill each line from the cache.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

pub(crate) const MAX_BLAME_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// One blamed line: the commit short-sha, author, and a short date (YYYY-MM-DD).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlameLine {
    /// Abbreviated commit sha (first 8 chars), or "" for an uncommitted line.
    pub sha: String,
    /// Commit author name.
    pub author: String,
    /// Short date `YYYY-MM-DD` (from author-time + author-tz, UTC-ish).
    pub date: String,
    /// `true` for the "Not Committed Yet" zero-sha (local, unsaved) lines.
    pub uncommitted: bool,
}

/// Commit metadata accumulated while parsing porcelain headers.
#[derive(Debug, Clone, Default)]
struct Commit {
    author: String,
    author_time: Option<i64>,
    author_tz_seconds: Option<i64>,
}

/// Parse `git blame --porcelain` output into one [`BlameLine`] per source line,
/// in final-file order. Pure; no IO.
pub fn parse_porcelain(out: &str) -> Vec<BlameLine> {
    let mut commits: HashMap<String, Commit> = HashMap::new();
    let mut lines: Vec<BlameLine> = Vec::new();
    let mut cur_sha = String::new();
    let mut cur = Commit::default();
    // Whether we're between a group header and its terminating `\t<content>` line
    // (so header keys like `author` apply to `cur` / `cur_sha`).
    let mut in_entry = false;

    for raw in out.lines() {
        if let Some(content) = raw.strip_prefix('\t') {
            // The literal source line: this terminates the current entry. Emit a
            // BlameLine for it, looking up (and caching) the commit metadata.
            let _ = content;
            let commit = commits
                .entry(cur_sha.clone())
                .or_insert_with(|| cur.clone());
            // If this commit's metadata was just seen (header lines present),
            // refresh the cached entry with it.
            if !cur.author.is_empty() {
                commit.author = cur.author.clone();
            }
            if let Some(author_time) = cur.author_time {
                commit.author_time = Some(author_time);
            }
            if let Some(author_tz_seconds) = cur.author_tz_seconds {
                commit.author_tz_seconds = Some(author_tz_seconds);
            }
            let uncommitted = cur_sha.chars().all(|c| c == '0') && !cur_sha.is_empty();
            let sha = if uncommitted {
                String::new()
            } else {
                cur_sha.chars().take(8).collect()
            };
            let author = if uncommitted && commit.author.is_empty() {
                "You".to_string()
            } else {
                commit.author.clone()
            };
            lines.push(BlameLine {
                sha,
                author,
                date: commit
                    .author_time
                    .map(|unix| short_date(unix, commit.author_tz_seconds.unwrap_or(0)))
                    .unwrap_or_default(),
                uncommitted,
            });
            in_entry = false;
            cur = Commit::default();
            continue;
        }

        // A group header line begins a new entry: "<sha> <orig> <final> [<n>]".
        // Detect it as: 40-hex (or all-zero) sha followed by a space + digit.
        if !in_entry || raw.split(' ').next().map(is_sha_token).unwrap_or(false) {
            let mut it = raw.split(' ');
            if let Some(tok) = it.next() {
                if is_sha_token(tok) {
                    cur_sha = tok.to_string();
                    in_entry = true;
                    // Seed `cur` from cache (if known) so a repeat commit's lines
                    // back-fill author/date without re-emitting headers.
                    if let Some(c) = commits.get(&cur_sha) {
                        cur = c.clone();
                    } else {
                        cur = Commit::default();
                    }
                    continue;
                }
            }
        }

        // Header key/value lines within an entry.
        if let Some(v) = raw.strip_prefix("author ") {
            cur.author = v.trim().to_string();
        } else if let Some(v) = raw.strip_prefix("author-time ") {
            cur.author_time = parse_author_time(v);
        } else if let Some(v) = raw.strip_prefix("author-tz ") {
            cur.author_tz_seconds = parse_tz_seconds(v);
        }
        // Other headers (committer*, summary, filename, previous, boundary) ignored.
    }
    lines
}

/// Is `tok` a git blame sha token (40 hex chars, or all-zero uncommitted)?
fn is_sha_token(tok: &str) -> bool {
    tok.len() == 40 && tok.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_author_time(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

/// Format a Unix `author-time` plus a parsed tz offset into a short `YYYY-MM-DD`
/// date. Self-contained civil-date conversion (no chrono dep). Applies the tz
/// offset so the date matches the author's local day.
fn short_date(unix: i64, offset: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let local = unix + offset;
    let days = local.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parse a git tz offset "+HHMM" / "-HHMM" into seconds.
fn parse_tz_seconds(tz: &str) -> Option<i64> {
    let t = tz.trim();
    if t.len() != 5 {
        return None;
    }
    let sign = if t.starts_with('-') {
        -1
    } else if t.starts_with('+') {
        1
    } else {
        return None;
    };
    let digits = &t[1..];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hh: i64 = digits.get(0..2).and_then(|s| s.parse().ok())?;
    let mm: i64 = digits.get(2..4).and_then(|s| s.parse().ok())?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

/// Convert a count of days since the Unix epoch (1970-01-01) into a civil
/// (year, month, day). Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Run `git -C <root> blame --porcelain -- <relpath>` and return the raw blob.
/// Best-effort: "" on error / not tracked.
fn run_blame(root: &Path, relpath: &str) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["blame", "--porcelain", "--", relpath]);
    let out = run_blame_command_capped(cmd);
    match out {
        Ok(Some((status, stdout, _))) if status.success() => {
            String::from_utf8_lossy(&stdout).into_owned()
        }
        _ => String::new(),
    }
}

fn run_blame_command_capped(
    mut cmd: Command,
) -> Result<Option<(ExitStatus, Vec<u8>, Vec<u8>)>, String> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("failed to start: {e}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "git blame stdout pipe unavailable".to_string()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "git blame stderr pipe unavailable".to_string()
    })?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = Arc::clone(&exceeded);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stdout_reader = std::thread::spawn(move || {
        read_blame_stream_capped(stdout, MAX_BLAME_OUTPUT_BYTES, stdout_exceeded)
    });
    let stderr_reader = std::thread::spawn(move || {
        read_blame_stream_capped(stderr, MAX_BLAME_OUTPUT_BYTES, stderr_exceeded)
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
        Ok(None)
    } else {
        Ok(Some((status, stdout, stderr)))
    }
}

fn read_blame_stream_capped<R: Read>(
    mut reader: R,
    cap: usize,
    exceeded: Arc<AtomicBool>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if !append_blame_output_with_cap(&mut out, &chunk[..n], cap) {
                    exceeded.store(true, Ordering::Relaxed);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out
}

fn append_blame_output_with_cap(out: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if out.len().saturating_add(chunk.len()) > cap {
        out.clear();
        return false;
    }
    out.extend_from_slice(chunk);
    true
}

/// The blame-gutter state: a toggle + a per-file cache of parsed blame lines.
#[derive(Debug, Default)]
pub struct BlameState {
    /// `true` while the blame gutter is shown.
    active: bool,
    /// The file (repo-relative path) the current `lines` were blamed from.
    file: String,
    /// Parsed blame lines for `file`, indexed by 0-based final-file line.
    lines: Vec<BlameLine>,
    /// Cache: repo-relative path -> parsed blame lines (refreshed on save).
    cache: HashMap<String, Vec<BlameLine>>,
}

impl BlameState {
    pub fn new() -> Self {
        BlameState::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> Option<&BlameLine> {
        self.lines.get(i)
    }

    /// Toggle the gutter for `relpath` under `root`. When turning ON, (re)loads
    /// blame (from cache if present). Returns `true` if now active.
    pub fn toggle(&mut self, root: &Path, relpath: &str) -> bool {
        if self.active && self.file == relpath {
            self.active = false;
            return false;
        }
        self.load(root, relpath);
        self.active = true;
        true
    }

    /// (Re)load blame for `relpath`, using the cache when available. Used on
    /// toggle-on and after a save (call [`Self::invalidate`] first to force).
    pub fn load(&mut self, root: &Path, relpath: &str) {
        if let Some(cached) = self.cache.get(relpath) {
            self.lines = cached.clone();
            self.file = relpath.to_string();
            return;
        }
        let blob = run_blame(root, relpath);
        let parsed = parse_porcelain(&blob);
        self.cache.insert(relpath.to_string(), parsed.clone());
        self.lines = parsed;
        self.file = relpath.to_string();
    }

    /// Drop the cached blame for `relpath` (call on save), and — if the gutter is
    /// active for that file — reload it from git.
    pub fn invalidate(&mut self, root: &Path, relpath: &str) {
        self.cache.remove(relpath);
        if self.active && self.file == relpath {
            self.load(root, relpath);
        }
    }

    /// Switch the active file (e.g. tab switch) while the gutter stays on.
    pub fn set_file(&mut self, root: &Path, relpath: &str) {
        if self.active && self.file != relpath {
            self.load(root, relpath);
        }
    }

    pub fn close(&mut self) {
        self.active = false;
    }

    /// Screenshot-only: seed the gutter from a raw porcelain `blob` and activate
    /// it (no git call). Returns the parsed line count.
    pub fn seed_demo(&mut self, blob: &str) -> usize {
        self.lines = parse_porcelain(blob);
        self.file = "src/main.mty".to_string();
        self.active = true;
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A two-commit porcelain blob: commit B appears once with full headers, then
    // a second line references commit A (which appeared earlier) by sha only.
    const SAMPLE: &str = "\
1111111111111111111111111111111111111111 1 1 2
author Ada Lovelace
author-mail <ada@example.com>
author-time 1136239445
author-tz +0000
committer Ada Lovelace
committer-time 1136239445
committer-tz +0000
summary first commit
filename src/main.rs
\tfn main() {
1111111111111111111111111111111111111111 2 2
\t    let x = 1;
2222222222222222222222222222222222222222 3 3 1
author Grace Hopper
author-time 1700000000
author-tz +0000
summary second commit
filename src/main.rs
\t    let y = 2;
";

    #[test]
    fn parses_author_date_sha_per_line() {
        let lines = parse_porcelain(SAMPLE);
        assert_eq!(lines.len(), 3);
        // Line 1 + 2 share commit 1111... (line 2 references it by sha only).
        assert_eq!(lines[0].sha, "11111111");
        assert_eq!(lines[0].author, "Ada Lovelace");
        assert_eq!(lines[0].date, "2006-01-02"); // 1136239445 = 2006-01-02 UTC
        assert_eq!(lines[1].sha, "11111111");
        assert_eq!(
            lines[1].author, "Ada Lovelace",
            "repeat commit back-fills author"
        );
        assert_eq!(lines[1].date, "2006-01-02");
        // Line 3 is the second commit.
        assert_eq!(lines[2].sha, "22222222");
        assert_eq!(lines[2].author, "Grace Hopper");
        assert_eq!(lines[2].date, "2023-11-14"); // 1700000000 = 2023-11-14 UTC
    }

    #[test]
    fn uncommitted_zero_sha_line() {
        let blob = "\
0000000000000000000000000000000000000000 1 1 1
author Not Committed Yet
author-time 1700000000
author-tz +0000
summary Version of ... (Not Committed Yet)
filename src/main.rs
\tlet draft = true;
";
        let lines = parse_porcelain(blob);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].uncommitted);
        assert_eq!(lines[0].sha, "");
    }

    #[test]
    fn tz_offset_shifts_local_day() {
        // 1136239445 = 2006-01-02 21:24:05 UTC; +0500 pushes past midnight? no,
        // still the 3rd at +0500 (02:24). Use a near-midnight time.
        // 1136246400 = 2006-01-03 00:00:00 UTC.
        let utc = short_date(1136246400, 0);
        assert_eq!(utc, "2006-01-03");
        // Same instant at -0100 is still 2006-01-02 (23:00 prev day).
        let minus = short_date(1136246400, -3600);
        assert_eq!(minus, "2006-01-02");
    }

    #[test]
    fn malformed_repeat_metadata_preserves_cached_date() {
        let blob = "\
3333333333333333333333333333333333333333 1 1 1
author Katherine Johnson
author-time 1700000000
author-tz +0000
summary first sighting
filename src/main.rs
\tlet first = true;
3333333333333333333333333333333333333333 2 2 1
author Katherine Johnson
author-time not-a-timestamp
author-tz +9999
summary malformed repeat
filename src/main.rs
\tlet second = true;
3333333333333333333333333333333333333333 3 3
\tlet third = true;
";
        let lines = parse_porcelain(blob);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].date, "2023-11-14");
        assert_eq!(
            lines[1].date, "2023-11-14",
            "malformed repeat headers must not clear cached commit dates"
        );
        assert_eq!(lines[2].date, "2023-11-14");
    }

    #[test]
    fn author_time_tokens_reject_signs_suffixes_and_overflow() {
        assert_eq!(parse_author_time("1700000000"), Some(1_700_000_000));
        assert_eq!(parse_author_time(&i64::MAX.to_string()), Some(i64::MAX));
        assert_eq!(parse_author_time("+1700000000"), None);
        assert_eq!(parse_author_time("-1700000000"), None);
        assert_eq!(parse_author_time("1700000000s"), None);
        assert_eq!(parse_author_time("9223372036854775808"), None);
    }

    #[test]
    fn author_timezone_tokens_reject_embedded_signs_and_bad_ranges() {
        assert_eq!(parse_tz_seconds("+0000"), Some(0));
        assert_eq!(parse_tz_seconds("-0130"), Some(-5400));
        assert_eq!(parse_tz_seconds("++000"), None);
        assert_eq!(parse_tz_seconds("-+000"), None);
        assert_eq!(parse_tz_seconds("+00+0"), None);
        assert_eq!(parse_tz_seconds("+2360"), None);
        assert_eq!(parse_tz_seconds("+2400"), None);
        assert_eq!(parse_tz_seconds("+0000x"), None);
    }

    #[test]
    fn blame_output_cap_accepts_exact_limit() {
        let mut out = Vec::from(b"git");

        assert!(append_blame_output_with_cap(&mut out, b"!", 4));
        assert_eq!(out, b"git!");
    }

    #[test]
    fn blame_output_cap_discards_oversized_stream() {
        let mut out = Vec::from(b"blame:");

        assert!(!append_blame_output_with_cap(&mut out, b"overflow", 8));
        assert!(out.is_empty());
    }

    #[test]
    fn blame_stream_reader_marks_oversized_output() {
        let exceeded = Arc::new(AtomicBool::new(false));
        let out = read_blame_stream_capped(
            std::io::Cursor::new(b"abcdef".to_vec()),
            5,
            Arc::clone(&exceeded),
        );

        assert!(out.is_empty());
        assert!(exceeded.load(Ordering::Relaxed));
    }

    #[test]
    fn malformed_author_time_does_not_replace_cached_date() {
        let blob = "\
4444444444444444444444444444444444444444 1 1 1
author Emmy Noether
author-time 1700000000
author-tz +0000
summary first sighting
filename src/main.rs
\tlet first = true;
4444444444444444444444444444444444444444 2 2 1
author Emmy Noether
author-time +1700000001
author-tz +0000
summary plus-prefixed repeat
filename src/main.rs
\tlet second = true;
4444444444444444444444444444444444444444 3 3 1
author Emmy Noether
author-time 9223372036854775808
author-tz +0000
summary overflowing repeat
filename src/main.rs
\tlet third = true;
";
        let lines = parse_porcelain(blob);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].date, "2023-11-14");
        assert_eq!(lines[1].date, "2023-11-14");
        assert_eq!(lines[2].date, "2023-11-14");
    }

    #[test]
    fn malformed_author_timezone_does_not_replace_cached_date() {
        let blob = "\
5555555555555555555555555555555555555555 1 1 1
author Mary Jackson
author-time 1700000000
author-tz +0000
summary first sighting
filename src/main.rs
\tlet first = true;
5555555555555555555555555555555555555555 2 2 1
author Mary Jackson
author-time 1700000000
author-tz ++500
summary malformed repeat
filename src/main.rs
\tlet second = true;
5555555555555555555555555555555555555555 3 3 1
author Mary Jackson
author-time 1700000000
author-tz +05+0
summary malformed minute repeat
filename src/main.rs
\tlet third = true;
";
        let lines = parse_porcelain(blob);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].date, "2023-11-14");
        assert_eq!(lines[1].date, "2023-11-14");
        assert_eq!(lines[2].date, "2023-11-14");
    }

    #[test]
    fn empty_blob_yields_no_lines() {
        assert!(parse_porcelain("").is_empty());
    }

    #[test]
    fn civil_from_days_epoch() {
        // Day 0 = 1970-01-01.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }
}
