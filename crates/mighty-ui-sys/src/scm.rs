//! Source-control (git) model for the Source Control activity panel.
//!
//! The shim shells out to `git` in the workspace root and parses its
//! `--porcelain=v1 -b` output into a flat list of changed entries grouped by
//! staged / unstaged, plus the branch + ahead/behind. Mighty (v0.36, L17) can't
//! hold strings or run processes from FFI, so all of this lives shim-side and is
//! exposed through the scalar `mui_scm_*` ABI in [`crate::abi`].
//!
//! The porcelain parser ([`parse_status`]) is pure + unit-tested. The git shell
//! calls ([`discover_root`], [`status`], [`stage`], [`unstage`], [`commit`],
//! [`diff`]) are thin wrappers around `std::process::Command`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A single changed path in the working tree / index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScmEntry {
    /// Repo-relative path (the new path for renames).
    pub path: String,
    /// `true` if the change is staged (in the index).
    pub staged: bool,
    /// One-letter status: M(odified) A(dded) D(eleted) R(enamed) U(ntracked)
    /// C(onflicted). Uppercase, single char.
    pub status: char,
}

impl ScmEntry {
    /// Basename (file-name component) for display.
    pub fn name(&self) -> &str {
        let p = self.path.as_str();
        match p.rfind(['/', '\\']) {
            Some(i) => &p[i + 1..],
            None => p,
        }
    }

    /// Directory portion (everything before the basename), or "" at the root.
    pub fn dir(&self) -> &str {
        let p = self.path.as_str();
        match p.rfind(['/', '\\']) {
            Some(i) => &p[..i],
            None => "",
        }
    }
}

/// Parsed `git status --porcelain=v1 -b` result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScmStatus {
    /// Current branch (e.g. `main`), or `(detached)` / "" when unknown.
    pub branch: String,
    /// Commits ahead of upstream (0 if no upstream).
    pub ahead: i32,
    /// Commits behind upstream (0 if no upstream).
    pub behind: i32,
    /// Changed entries (staged ones first, then unstaged), in git's order.
    pub entries: Vec<ScmEntry>,
}

impl ScmStatus {
    #[allow(dead_code)]
    pub fn staged_count(&self) -> usize {
        self.entries.iter().filter(|e| e.staged).count()
    }
    #[allow(dead_code)]
    pub fn unstaged_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.staged).count()
    }
}

/// Map a porcelain XY status pair to a one-letter display status for one side.
/// `code` is the index char (X) or worktree char (Y).
fn status_letter(code: u8) -> Option<char> {
    match code {
        b'M' => Some('M'),
        b'A' => Some('A'),
        b'D' => Some('D'),
        b'R' => Some('R'),
        b'C' => Some('C'),
        b'U' => Some('U'), // unmerged / conflicted
        b'T' => Some('M'), // type-change -> treat as modified
        b'?' => Some('U'), // untracked -> show as U
        b'!' => None,      // ignored
        b' ' => None,
        _ => None,
    }
}

/// Parse `git status --porcelain=v1 -b` (or `-z`-free newline form) into a
/// [`ScmStatus`]. Pure; no IO. Handles the `## branch...upstream [ahead N, behind M]`
/// header line and each `XY <path>` (or `XY <old> -> <new>`) entry.
pub fn parse_status(out: &str) -> ScmStatus {
    let mut status = ScmStatus::default();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            parse_branch_line(rest, &mut status);
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0];
        let y = bytes[1];
        // The path starts at column 3 (after "XY ").
        let mut path = line[3..].to_string();
        // Rename/copy entries are "old -> new"; keep the new path.
        if let Some(idx) = path.find(" -> ") {
            path = path[idx + 4..].to_string();
        }
        let path = unquote(&path);

        if x == b'?' && y == b'?' {
            // Untracked: a single unstaged "U" entry.
            status.entries.push(ScmEntry {
                path,
                staged: false,
                status: 'U',
            });
            continue;
        }
        if x == b'U' || y == b'U' || (x == b'D' && y == b'D') || (x == b'A' && y == b'A') {
            // Unmerged / conflicted: a single "C" entry (shown under Changes).
            status.entries.push(ScmEntry {
                path,
                staged: false,
                status: 'C',
            });
            continue;
        }
        // Staged side (index, X).
        if let Some(letter) = status_letter(x) {
            status.entries.push(ScmEntry {
                path: path.clone(),
                staged: true,
                status: letter,
            });
        }
        // Unstaged side (worktree, Y).
        if let Some(letter) = status_letter(y) {
            status.entries.push(ScmEntry {
                path,
                staged: false,
                status: letter,
            });
        }
    }
    status
}

/// Parse the `## ...` branch header (without the leading "## ").
fn parse_branch_line(rest: &str, status: &mut ScmStatus) {
    // Forms:
    //   "main"
    //   "main...origin/main"
    //   "main...origin/main [ahead 2]"
    //   "main...origin/main [ahead 2, behind 1]"
    //   "No commits yet on main"
    //   "HEAD (no branch)"
    let head = rest.split("...").next().unwrap_or(rest);
    // Strip a trailing " [ahead ...]" from the head if there was no upstream.
    let branch_part = head.split(" [").next().unwrap_or(head).trim();
    if let Some(b) = branch_part.strip_prefix("No commits yet on ") {
        status.branch = b.trim().to_string();
    } else {
        status.branch = branch_part.to_string();
    }
    if let Some(start) = rest.find('[') {
        if let Some(end) = rest[start..].find(']') {
            let inner = &rest[start + 1..start + end];
            for part in inner.split(',') {
                let part = part.trim();
                if let Some(n) = part.strip_prefix("ahead ") {
                    status.ahead = n.trim().parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix("behind ") {
                    status.behind = n.trim().parse().unwrap_or(0);
                }
            }
        }
    }
}

/// Remove the surrounding quotes git adds for paths with special chars (best
/// effort; we don't decode octal escapes, just strip the quotes).
fn unquote(path: &str) -> String {
    let t = path.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

// ---------------------------------------------------------------------------
// git shell wrappers
// ---------------------------------------------------------------------------

/// Find the enclosing git repo root for `dir` via
/// `git -C <dir> rev-parse --show-toplevel`. Returns `None` if not a repo.
pub fn discover_root(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(PathBuf::from(line))
    }
}

/// Run `git -C <root> status --porcelain=v1 -b` and parse the result.
pub fn status(root: &Path) -> ScmStatus {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-b"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_status(&String::from_utf8_lossy(&o.stdout)),
        _ => ScmStatus::default(),
    }
}

/// Stage one path (`git add -- <path>`). Returns true on success.
pub fn stage(root: &Path, path: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "--", path])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Unstage one path (`git restore --staged -- <path>`). Returns true on success.
pub fn unstage(root: &Path, path: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["restore", "--staged", "--", path])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Commit staged changes with `message` (`git commit -m <message>`). Returns
/// true on success (false if nothing staged / message empty / git error).
pub fn commit(root: &Path, message: &str) -> bool {
    if message.trim().is_empty() {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-m", message])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Unified diff for one path (`git -C <root> diff -- <path>`), staged side
/// included via a second call when the worktree diff is empty. Best-effort;
/// returns "" on error. (Optional inline-diff feature.)
#[allow(dead_code)]
pub fn diff(root: &Path, path: &str) -> String {
    let run = |staged: bool| -> String {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(root).arg("diff");
        if staged {
            cmd.arg("--staged");
        }
        cmd.args(["--", path]);
        cmd.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let worktree = run(false);
    if worktree.trim().is_empty() {
        run(true)
    } else {
        worktree
    }
}

// ---------------------------------------------------------------------------
// shim-side panel state (driven through the scalar ABI)
// ---------------------------------------------------------------------------

/// Source-control panel state: discovered repo root, last parsed status, and the
/// commit-message input buffer (shim-owned, L17).
#[derive(Debug, Default)]
pub struct ScmState {
    /// The repo root (discovered from the workspace dir), or `None` if not a repo.
    pub root: Option<PathBuf>,
    /// The most recently parsed status.
    pub status: ScmStatus,
    /// The commit-message input buffer.
    pub message: Vec<char>,
}

impl ScmState {
    pub fn new() -> Self {
        ScmState::default()
    }

    /// (Re)discover the repo root from `dir` and refresh status. Returns the
    /// number of changed entries (0 if not a repo).
    pub fn refresh(&mut self, dir: &Path) -> i32 {
        if self.root.is_none() {
            self.root = discover_root(dir);
        }
        match &self.root {
            Some(root) => {
                self.status = status(root);
                self.status.entries.len() as i32
            }
            None => {
                self.status = ScmStatus::default();
                0
            }
        }
    }

    pub fn count(&self) -> i32 {
        self.status.entries.len() as i32
    }

    pub fn get(&self, i: usize) -> Option<&ScmEntry> {
        self.status.entries.get(i)
    }

    /// Stage or unstage entry `i` based on its current staged flag, then refresh.
    /// Returns true if a git command ran and succeeded.
    pub fn toggle_stage(&mut self, i: usize, dir: &Path) -> bool {
        let (path, staged) = match self.status.entries.get(i) {
            Some(e) => (e.path.clone(), e.staged),
            None => return false,
        };
        let Some(root) = self.root.clone() else {
            return false;
        };
        let ok = if staged {
            unstage(&root, &path)
        } else {
            stage(&root, &path)
        };
        if ok {
            self.refresh(dir);
        }
        ok
    }

    /// Commit with the current message buffer, then clear it + refresh.
    pub fn commit_message(&mut self, dir: &Path) -> bool {
        let Some(root) = self.root.clone() else {
            return false;
        };
        let msg: String = self.message.iter().collect();
        let ok = commit(&root, &msg);
        if ok {
            self.message.clear();
            self.refresh(dir);
        }
        ok
    }

    pub fn message_string(&self) -> String {
        self.message.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_branch_with_ahead_behind() {
        let out = "## main...origin/main [ahead 2, behind 1]\n M src/main.rs\n";
        let s = parse_status(out);
        assert_eq!(s.branch, "main");
        assert_eq!(s.ahead, 2);
        assert_eq!(s.behind, 1);
    }

    #[test]
    fn parse_branch_ahead_only() {
        let s = parse_status("## feature/x...origin/feature/x [ahead 3]\n");
        assert_eq!(s.branch, "feature/x");
        assert_eq!(s.ahead, 3);
        assert_eq!(s.behind, 0);
    }

    #[test]
    fn parse_branch_no_upstream() {
        let s = parse_status("## main\n");
        assert_eq!(s.branch, "main");
        assert_eq!(s.ahead, 0);
        assert_eq!(s.behind, 0);
    }

    #[test]
    fn parse_no_commits_yet() {
        let s = parse_status("## No commits yet on main\n?? new.txt\n");
        assert_eq!(s.branch, "main");
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].status, 'U');
    }

    #[test]
    fn modified_unstaged() {
        let s = parse_status("## main\n M src/lib.rs\n");
        assert_eq!(s.entries.len(), 1);
        let e = &s.entries[0];
        assert_eq!(e.status, 'M');
        assert!(!e.staged);
        assert_eq!(e.path, "src/lib.rs");
        assert_eq!(e.name(), "lib.rs");
        assert_eq!(e.dir(), "src");
    }

    #[test]
    fn staged_added() {
        let s = parse_status("## main\nA  src/new.rs\n");
        assert_eq!(s.entries.len(), 1);
        let e = &s.entries[0];
        assert_eq!(e.status, 'A');
        assert!(e.staged);
    }

    #[test]
    fn both_staged_and_unstaged_modified() {
        // "MM" = modified in index AND in worktree -> two entries.
        let s = parse_status("## main\nMM src/x.rs\n");
        assert_eq!(s.entries.len(), 2);
        assert!(s.entries[0].staged && s.entries[0].status == 'M');
        assert!(!s.entries[1].staged && s.entries[1].status == 'M');
        assert_eq!(s.staged_count(), 1);
        assert_eq!(s.unstaged_count(), 1);
    }

    #[test]
    fn deleted_and_untracked() {
        let s = parse_status("## main\n D gone.txt\n?? fresh.txt\n");
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].status, 'D');
        assert!(!s.entries[0].staged);
        assert_eq!(s.entries[1].status, 'U');
        assert!(!s.entries[1].staged);
    }

    #[test]
    fn rename_keeps_new_path() {
        let s = parse_status("## main\nR  old.rs -> new.rs\n");
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].status, 'R');
        assert_eq!(s.entries[0].path, "new.rs");
        assert!(s.entries[0].staged);
    }

    #[test]
    fn conflict_is_single_entry() {
        let s = parse_status("## main\nUU both.rs\n");
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].status, 'C');
        assert!(!s.entries[0].staged);
    }

    #[test]
    fn ignored_lines_skipped() {
        let s = parse_status("## main\n!! ignored.log\n M real.rs\n");
        // The "!!" ignored line produces no entry.
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].path, "real.rs");
    }

    #[test]
    fn quoted_path_unquoted() {
        let s = parse_status("## main\n M \"with space.rs\"\n");
        assert_eq!(s.entries[0].path, "with space.rs");
    }
}
