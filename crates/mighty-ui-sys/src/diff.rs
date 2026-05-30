//! Inline git-diff view: parse `git diff` unified hunks + render them read-only
//! in the editor area (the Source Control "diff" affordance).
//!
//! The shim shells `git -C <root> diff [--cached] -- <path>` (reusing
//! [`crate::scm::diff_path`]), parses the unified-diff body into a flat list of
//! display [`DiffLine`]s (hunk headers, added / removed / context lines, each
//! tagged with the old/new line numbers), and the IDE draws them in the editor
//! body via the scalar `mui_diff_*` ABI. v0.36 Mighty can't hold strings/Vecs
//! across FFI (L17/L21), so the parsed model + draw live here; Mighty just opens
//! / closes the view and calls `mui_diff_draw` each frame.
//!
//! The parser ([`parse_unified`]) is pure + exhaustively unit-tested: it handles
//! multi-hunk diffs, `@@ -a,b +c,d @@` headers (with the optional single-count
//! `@@ -a +c @@` form), `+`/`-`/` ` prefixes, the `\ No newline at end of file`
//! marker, and the leading `diff --git` / `index` / `---` / `+++` file headers
//! (skipped — we only render the hunks).

#![allow(dead_code)]

use std::process::Command;
use std::path::Path;

/// The kind of a parsed diff line (mirrors the scalar `mui_diff_line_kind` ABI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A `@@ -a,b +c,d @@` hunk header.
    Hunk = 0,
    /// An unchanged context line (` ` prefix).
    Context = 1,
    /// An added line (`+` prefix).
    Add = 2,
    /// A removed line (`-` prefix).
    Remove = 3,
    /// A `\ No newline at end of file` marker.
    Meta = 4,
}

/// One display line of a parsed unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line text (WITHOUT the leading `+`/`-`/` ` marker; hunk headers keep
    /// their `@@ ... @@` text).
    pub text: String,
    /// 1-based old-file line number, or `-1` for added / hunk / meta lines.
    pub old_no: i32,
    /// 1-based new-file line number, or `-1` for removed / hunk / meta lines.
    pub new_no: i32,
}

/// Parse a `git diff` blob (one file) into display lines. Pure; no IO. The
/// `diff --git` / `index` / `---` / `+++` file headers are skipped; only the
/// `@@` hunks and their bodies are emitted. Line numbers track the hunk header's
/// `-a,b +c,d` start positions.
pub fn parse_unified(blob: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut old_no = 0i32;
    let mut new_no = 0i32;
    let mut in_hunk = false;

    for raw in blob.lines() {
        if let Some(rest) = raw.strip_prefix("@@") {
            // Hunk header: "@@ -a,b +c,d @@ optional section heading".
            if let Some((a, c)) = parse_hunk_header(rest) {
                old_no = a;
                new_no = c;
            }
            in_hunk = true;
            out.push(DiffLine {
                kind: LineKind::Hunk,
                text: raw.to_string(),
                old_no: -1,
                new_no: -1,
            });
            continue;
        }
        if !in_hunk {
            // Pre-hunk file headers (diff --git / index / --- / +++) — skip.
            continue;
        }
        if let Some(meta) = raw.strip_prefix('\\') {
            // "\ No newline at end of file" — a meta marker, no line number.
            let _ = meta;
            out.push(DiffLine {
                kind: LineKind::Meta,
                text: raw.trim_start_matches('\\').trim().to_string(),
                old_no: -1,
                new_no: -1,
            });
            continue;
        }
        // Body line: first byte is the +/-/space marker.
        let (marker, body) = split_marker(raw);
        match marker {
            '+' => {
                out.push(DiffLine {
                    kind: LineKind::Add,
                    text: body,
                    old_no: -1,
                    new_no,
                });
                new_no += 1;
            }
            '-' => {
                out.push(DiffLine {
                    kind: LineKind::Remove,
                    text: body,
                    old_no,
                    new_no: -1,
                });
                old_no += 1;
            }
            _ => {
                // Context (space prefix) — advance both sides.
                out.push(DiffLine {
                    kind: LineKind::Context,
                    text: body,
                    old_no,
                    new_no,
                });
                old_no += 1;
                new_no += 1;
            }
        }
    }
    out
}

/// Split a diff body line into its marker char and the remaining text. An empty
/// line is treated as a blank context line (marker = space).
fn split_marker(line: &str) -> (char, String) {
    let mut chars = line.chars();
    match chars.next() {
        Some(c @ ('+' | '-' | ' ')) => (c, chars.collect()),
        Some(other) => {
            // No recognized marker (shouldn't happen mid-hunk): treat the whole
            // line as context text.
            let mut s = String::new();
            s.push(other);
            s.push_str(chars.as_str());
            (' ', s)
        }
        None => (' ', String::new()),
    }
}

/// Parse the part of a hunk header after the leading `@@`: ` -a,b +c,d @@ ...`.
/// Returns the 1-based `(old_start, new_start)`. Handles the single-count form
/// `-a +c` (count omitted means 1).
fn parse_hunk_header(rest: &str) -> Option<(i32, i32)> {
    // rest looks like " -a,b +c,d @@ heading"
    let inner = rest.trim_start();
    let mut parts = inner.split_whitespace();
    let minus = parts.next()?; // "-a,b"
    let plus = parts.next()?; // "+c,d"
    let old_start = minus.strip_prefix('-')?.split(',').next()?.parse().ok()?;
    let new_start = plus.strip_prefix('+')?.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

// ---------------------------------------------------------------------------
// shim-side view state (driven through the scalar ABI)
// ---------------------------------------------------------------------------

/// The inline diff view: the parsed lines for one file, plus the scroll offset
/// and which side (working tree vs staged) was diffed.
#[derive(Debug, Default)]
pub struct DiffView {
    /// `true` while the diff view is shown in the editor area.
    active: bool,
    /// Repo-relative path being diffed (for the header).
    path: String,
    /// `true` for the staged (`--cached`) diff, `false` for the worktree diff.
    staged: bool,
    /// Parsed display lines.
    lines: Vec<DiffLine>,
    /// Top visible line (scroll offset).
    first: usize,
}

impl DiffView {
    pub fn new() -> Self {
        DiffView::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn staged(&self) -> bool {
        self.staged
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn first(&self) -> usize {
        self.first
    }

    pub fn line(&self, i: usize) -> Option<&DiffLine> {
        self.lines.get(i)
    }

    /// Open the view with a parsed diff blob for `path` (`staged` side). Resets
    /// the scroll. Returns the number of parsed lines.
    pub fn open(&mut self, path: &str, staged: bool, blob: &str) -> usize {
        self.lines = parse_unified(blob);
        self.path = path.to_string();
        self.staged = staged;
        self.first = 0;
        self.active = true;
        self.lines.len()
    }

    pub fn close(&mut self) {
        self.active = false;
        self.lines.clear();
        self.first = 0;
    }

    /// Scroll by `delta` lines (clamped to range).
    pub fn scroll(&mut self, delta: i32) {
        let max = self.lines.len().saturating_sub(1) as i32;
        let mut f = self.first as i32 + delta;
        if f < 0 {
            f = 0;
        }
        if f > max {
            f = max;
        }
        self.first = f as usize;
    }

    /// Count of added / removed lines (for the header summary).
    pub fn add_count(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == LineKind::Add).count()
    }
    pub fn remove_count(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == LineKind::Remove).count()
    }
}

/// Run `git -C <root> diff [--cached] -- <path>` and return the raw blob. Thin
/// wrapper around [`crate::scm::diff_path`] kept here so the ABI layer can call
/// one function. Best-effort: returns "" on error.
pub fn run_diff(root: &Path, path: &str, staged: bool) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("diff");
    if staged {
        cmd.arg("--cached");
    }
    cmd.args(["--", path]);
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 83db48f..f735c2d 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    println!(\"added\");
 }
";

    #[test]
    fn parses_single_hunk_with_line_numbers() {
        let lines = parse_unified(SAMPLE);
        // hunk header + 1 context + 1 remove + 2 add + 1 context = 6
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].kind, LineKind::Hunk);
        assert_eq!(lines[1].kind, LineKind::Context);
        assert_eq!(lines[1].text, "fn main() {");
        assert_eq!(lines[1].old_no, 1);
        assert_eq!(lines[1].new_no, 1);
        assert_eq!(lines[2].kind, LineKind::Remove);
        assert_eq!(lines[2].text, "    println!(\"old\");");
        assert_eq!(lines[2].old_no, 2);
        assert_eq!(lines[2].new_no, -1);
        assert_eq!(lines[3].kind, LineKind::Add);
        assert_eq!(lines[3].new_no, 2);
        assert_eq!(lines[3].old_no, -1);
        assert_eq!(lines[4].kind, LineKind::Add);
        assert_eq!(lines[4].new_no, 3);
        // trailing context advances on both sides (old was 3, new was 4).
        assert_eq!(lines[5].kind, LineKind::Context);
        assert_eq!(lines[5].old_no, 3);
        assert_eq!(lines[5].new_no, 4);
    }

    #[test]
    fn parses_multiple_hunks() {
        let blob = "\
diff --git a/x b/x
--- a/x
+++ b/x
@@ -1,2 +1,2 @@
 a
-b
+B
@@ -10,2 +10,3 @@
 j
+K
 l
";
        let lines = parse_unified(blob);
        let hunks: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Hunk).collect();
        assert_eq!(hunks.len(), 2);
        // Second hunk's first context line should be old/new line 10.
        let second_hunk_pos = lines.iter().position(|l| l.kind == LineKind::Hunk && l.text.contains("-10")).unwrap();
        let first_ctx = &lines[second_hunk_pos + 1];
        assert_eq!(first_ctx.text, "j");
        assert_eq!(first_ctx.old_no, 10);
        assert_eq!(first_ctx.new_no, 10);
        // The added "K" line takes new line 11, no old number.
        let added = &lines[second_hunk_pos + 2];
        assert_eq!(added.kind, LineKind::Add);
        assert_eq!(added.new_no, 11);
        assert_eq!(added.old_no, -1);
    }

    #[test]
    fn no_newline_marker_is_meta() {
        let blob = "\
--- a/f
+++ b/f
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let lines = parse_unified(blob);
        let metas: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Meta).collect();
        assert_eq!(metas.len(), 2);
        assert!(metas[0].text.contains("No newline"));
    }

    #[test]
    fn single_count_hunk_header_form() {
        // "@@ -1 +1 @@" (count omitted) must still parse the start lines.
        let blob = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-x\n+y\n";
        let lines = parse_unified(blob);
        assert_eq!(lines[0].kind, LineKind::Hunk);
        assert_eq!(lines[1].kind, LineKind::Remove);
        assert_eq!(lines[1].old_no, 1);
        assert_eq!(lines[2].kind, LineKind::Add);
        assert_eq!(lines[2].new_no, 1);
    }

    #[test]
    fn pre_hunk_headers_are_skipped() {
        let lines = parse_unified(SAMPLE);
        // No display line should carry the "diff --git" / "index" / "---" text.
        assert!(lines.iter().all(|l| !l.text.starts_with("diff --git")));
        assert!(lines.iter().all(|l| !l.text.starts_with("index ")));
    }

    #[test]
    fn empty_blob_yields_no_lines() {
        assert!(parse_unified("").is_empty());
        // A blob with only file headers (no hunk) also yields nothing.
        assert!(parse_unified("diff --git a/x b/x\nindex 1..2\n--- a/x\n+++ b/x\n").is_empty());
    }

    #[test]
    fn view_open_close_scroll() {
        let mut v = DiffView::new();
        assert!(!v.is_active());
        let n = v.open("src/main.rs", false, SAMPLE);
        assert!(v.is_active());
        assert_eq!(n, 6);
        assert_eq!(v.add_count(), 2);
        assert_eq!(v.remove_count(), 1);
        v.scroll(3);
        assert_eq!(v.first(), 3);
        v.scroll(100);
        assert_eq!(v.first(), 5); // clamped to last index
        v.scroll(-100);
        assert_eq!(v.first(), 0);
        v.close();
        assert!(!v.is_active());
        assert_eq!(v.line_count(), 0);
    }
}
