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

/// Result of a network git action (push / pull / fetch): success flag + the
/// combined stdout+stderr git emitted (surfaced to the user as a toast). Never
/// force-pushes; the caller passes only safe argument sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitResult {
    pub ok: bool,
    pub message: String,
}

/// Run `git -C <root> <args...>`, capturing combined stdout+stderr. Pure-ish
/// wrapper: returns `(success, trimmed combined output)`. Used by push/pull/
/// fetch/branch ops so the exact git message can be toasted.
fn run_git(root: &Path, args: &[&str]) -> GitResult {
    let out = Command::new("git").arg("-C").arg(root).args(args).output();
    match out {
        Ok(o) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&o.stdout));
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                if !combined.trim().is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&err);
            }
            GitResult {
                ok: o.status.success(),
                message: summarize(combined.trim()),
            }
        }
        Err(e) => GitResult {
            ok: false,
            message: format!("git failed to start: {e}"),
        },
    }
}

/// Collapse multi-line git output into a single short line for a toast: take the
/// last non-empty line (git's most relevant message is usually last), trimmed.
fn summarize(out: &str) -> String {
    let line = out
        .lines()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        "done".to_string()
    } else {
        line.to_string()
    }
}

/// Push the current branch (`git push`). Never force-pushes. Returns the git
/// result (success + message) for a toast.
pub fn push(root: &Path) -> GitResult {
    run_git(root, &["push"])
}

/// Pull with fast-forward only (`git pull --ff-only`) so a non-trivial merge
/// never silently happens. Returns the git result for a toast.
pub fn pull(root: &Path) -> GitResult {
    run_git(root, &["pull", "--ff-only"])
}

/// Fetch all remotes (`git fetch`). Returns the git result for a toast.
pub fn fetch(root: &Path) -> GitResult {
    run_git(root, &["fetch"])
}

/// Checkout / switch to an existing local or remote branch (`git switch`,
/// falling back to `git checkout`). Returns the git result.
pub fn checkout(root: &Path, name: &str) -> GitResult {
    let r = run_git(root, &["switch", name]);
    if r.ok {
        return r;
    }
    // Fall back to `git checkout` (older git / detached situations / remote ref).
    run_git(root, &["checkout", name])
}

/// Create + switch to a new branch (`git switch -c <name>`). Returns the result.
pub fn create_branch(root: &Path, name: &str) -> GitResult {
    run_git(root, &["switch", "-c", name])
}

/// Run `git -C <root> branch --all` and parse it into a [`BranchList`].
pub fn branch_list(root: &Path) -> BranchList {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--all", "--no-color"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_branches(&String::from_utf8_lossy(&o.stdout)),
        _ => BranchList::default(),
    }
}

/// A single branch row from `git branch --all`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    /// The ref name to check out (e.g. `main`, `feature/x`, `origin/main`).
    pub name: String,
    /// `true` for the current branch (the `* ` marker).
    pub current: bool,
    /// `true` for a remote-tracking branch (`remotes/...`).
    pub remote: bool,
}

/// Parsed `git branch --all` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchList {
    pub entries: Vec<BranchEntry>,
}

#[allow(dead_code)]
impl BranchList {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, i: usize) -> Option<&BranchEntry> {
        self.entries.get(i)
    }
    /// Index of the current branch, if present.
    pub fn current_index(&self) -> Option<usize> {
        self.entries.iter().position(|e| e.current)
    }
}

/// Parse `git branch --all` output into a [`BranchList`]. Pure; no IO.
///
/// Handles:
///   * the `* ` current-branch marker (and the worktree `+ ` marker);
///   * indented local + `remotes/<remote>/<name>` rows;
///   * the `remotes/origin/HEAD -> origin/main` symbolic-ref row (skipped — it's
///     an alias, not a checkout target);
///   * a `(HEAD detached at <sha>)` row (skipped).
pub fn parse_branches(out: &str) -> BranchList {
    let mut list = BranchList::default();
    for raw in out.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        // Marker column: "* " current, "+ " worktree, "  " plain.
        let (current, rest) = if let Some(r) = line.strip_prefix("* ") {
            (true, r)
        } else if let Some(r) = line.strip_prefix("+ ") {
            (false, r)
        } else {
            (false, line.trim_start())
        };
        let rest = rest.trim();
        if rest.is_empty() {
            continue;
        }
        // Skip a detached-HEAD pseudo-row "(HEAD detached at abc1234)".
        if rest.starts_with('(') {
            continue;
        }
        // The first whitespace-delimited token is the ref name; anything after
        // (e.g. "-> origin/main") makes this a symbolic alias we skip.
        let name = rest.split_whitespace().next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if rest.contains("->") {
            // remotes/origin/HEAD -> origin/main — alias, not a target.
            continue;
        }
        let remote = name.starts_with("remotes/");
        let name = name.strip_prefix("remotes/").unwrap_or(name).to_string();
        list.entries.push(BranchEntry {
            name,
            current,
            remote,
        });
    }
    list
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
    /// The most recently fetched branch list (for the branch picker).
    pub branches: BranchList,
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

    /// Run a network/branch git action and refresh status afterwards. Returns the
    /// [`GitResult`] (success + git message) for the caller to toast. No-op (with
    /// an error result) if there is no repo root.
    pub fn run_action(&mut self, action: GitAction, dir: &Path) -> GitResult {
        let Some(root) = self.root.clone() else {
            return GitResult {
                ok: false,
                message: "Not a git repository".to_string(),
            };
        };
        let res = match action {
            GitAction::Push => push(&root),
            GitAction::Pull => pull(&root),
            GitAction::Fetch => fetch(&root),
        };
        // Always refresh so ahead/behind + changes reflect the new state.
        self.refresh(dir);
        res
    }

    /// Refresh the branch list from git (for the branch picker).
    pub fn refresh_branches(&mut self) -> i32 {
        match &self.root {
            Some(root) => {
                self.branches = branch_list(root);
                self.branches.len() as i32
            }
            None => {
                self.branches = BranchList::default();
                0
            }
        }
    }

    /// Checkout branch `name`, then refresh status + branches. Returns the result.
    pub fn checkout_branch(&mut self, name: &str, dir: &Path) -> GitResult {
        let Some(root) = self.root.clone() else {
            return GitResult {
                ok: false,
                message: "Not a git repository".to_string(),
            };
        };
        let res = checkout(&root, name);
        if res.ok {
            self.refresh(dir);
            self.refresh_branches();
        }
        res
    }

    /// Create + switch to a new branch `name`, then refresh. Returns the result.
    pub fn create_and_switch(&mut self, name: &str, dir: &Path) -> GitResult {
        let Some(root) = self.root.clone() else {
            return GitResult {
                ok: false,
                message: "Not a git repository".to_string(),
            };
        };
        let res = create_branch(&root, name);
        if res.ok {
            self.refresh(dir);
            self.refresh_branches();
        }
        res
    }
}

/// A network git action selected from a palette command / SCM button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitAction {
    Push,
    Pull,
    Fetch,
}

/// The branch-switcher overlay (shim-owned, scalar-driven). Lists local + remote
/// branches with a fuzzy filter; Up/Down move, Enter checks out the selection,
/// and a "Create branch…" mode lets the user type a new branch name. Mirrors the
/// command palette / theme picker pattern.
#[derive(Debug, Default)]
pub struct BranchPicker {
    active: bool,
    /// The full branch list captured when the picker opened.
    branches: Vec<BranchEntry>,
    /// The typed filter / new-branch-name buffer (chars).
    query: Vec<char>,
    /// Filtered indices into `branches` for the current query.
    filtered: Vec<usize>,
    /// Selected row into `filtered` (0-based). When in create mode, unused.
    sel: usize,
    /// `true` while the user is typing a NEW branch name (the "Create branch…"
    /// row was chosen); Enter then creates it.
    creating: bool,
}

impl BranchPicker {
    pub fn new() -> Self {
        BranchPicker::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_creating(&self) -> bool {
        self.creating
    }

    /// Open the picker over `list`, selecting the current branch's row.
    pub fn open(&mut self, list: &BranchList) {
        self.active = true;
        self.creating = false;
        self.query.clear();
        self.branches = list.entries.clone();
        self.refilter();
        // Start the highlight on the current branch if it survived the filter.
        if let Some(cur) = self.branches.iter().position(|e| e.current) {
            if let Some(p) = self.filtered.iter().position(|&i| i == cur) {
                self.sel = p;
            }
        }
    }

    fn refilter(&mut self) {
        let q: String = self.query.iter().collect::<String>().to_ascii_lowercase();
        self.filtered = self
            .branches
            .iter()
            .enumerate()
            .filter(|(_, e)| q.is_empty() || e.name.to_ascii_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        if self.sel >= self.filtered.len() {
            self.sel = self.filtered.len().saturating_sub(1);
        }
    }

    /// Number of rows: filtered branches + one trailing "Create branch…" row when
    /// not already in create mode.
    pub fn count(&self) -> usize {
        if self.creating {
            0
        } else {
            self.filtered.len() + 1
        }
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    pub fn query_string(&self) -> String {
        self.query.iter().collect()
    }

    pub fn query_len(&self) -> usize {
        self.query.len()
    }

    /// The branch name at filtered row `i`, or `None` for the "Create branch…"
    /// row / out of range.
    pub fn name_at(&self, i: usize) -> Option<&str> {
        self.filtered.get(i).and_then(|&bi| self.branches.get(bi)).map(|e| e.name.as_str())
    }

    pub fn entry_at(&self, i: usize) -> Option<&BranchEntry> {
        self.filtered.get(i).and_then(|&bi| self.branches.get(bi))
    }

    /// `true` if filtered row `i` is the trailing "Create branch…" row.
    pub fn is_create_row(&self, i: usize) -> bool {
        !self.creating && i == self.filtered.len()
    }

    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        if !self.creating {
            self.sel = 0;
            self.refilter();
        }
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        if !self.creating {
            self.sel = 0;
            self.refilter();
        }
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.count();
        if n == 0 {
            return;
        }
        let n_i = n as i32;
        let mut s = self.sel as i32 + delta;
        s %= n_i;
        if s < 0 {
            s += n_i;
        }
        self.sel = s as usize;
    }

    /// Switch into "Create branch…" mode (clears the query for the new name).
    pub fn enter_create_mode(&mut self) {
        self.creating = true;
        self.query.clear();
    }

    /// `true` if the current selection is the "Create branch…" row.
    pub fn selection_is_create(&self) -> bool {
        self.is_create_row(self.sel)
    }

    /// The selected branch name (when a branch row is highlighted).
    pub fn selected_name(&self) -> Option<String> {
        self.name_at(self.sel).map(|s| s.to_string())
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.creating = false;
        self.query.clear();
        self.branches.clear();
        self.filtered.clear();
        self.sel = 0;
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

    // ---- branch-list parsing ----

    #[test]
    fn branches_basic_current_and_remote() {
        let out = "\
* main
  develop
  remotes/origin/HEAD -> origin/main
  remotes/origin/main
  remotes/origin/develop
";
        let bl = parse_branches(out);
        // The "-> origin/main" alias row is skipped.
        assert_eq!(bl.len(), 4);
        assert_eq!(bl.entries[0].name, "main");
        assert!(bl.entries[0].current);
        assert!(!bl.entries[0].remote);
        assert_eq!(bl.entries[1].name, "develop");
        assert!(!bl.entries[1].current);
        // Remotes have the "remotes/" prefix stripped + remote flag set.
        assert_eq!(bl.entries[2].name, "origin/main");
        assert!(bl.entries[2].remote);
        assert_eq!(bl.entries[3].name, "origin/develop");
        assert!(bl.entries[3].remote);
        assert_eq!(bl.current_index(), Some(0));
    }

    #[test]
    fn branches_worktree_marker_and_detached_skipped() {
        let out = "\
  feature/a
+ feature/b
* (HEAD detached at 1a2b3c4)
  main
";
        let bl = parse_branches(out);
        // The detached pseudo-row is dropped; the "+ " worktree marker is parsed
        // as a normal (non-current) branch.
        assert_eq!(bl.len(), 3);
        assert_eq!(bl.entries[0].name, "feature/a");
        assert_eq!(bl.entries[1].name, "feature/b");
        assert!(!bl.entries[1].current);
        assert_eq!(bl.entries[2].name, "main");
        assert_eq!(bl.current_index(), None);
    }

    #[test]
    fn branches_empty_input() {
        assert!(parse_branches("").is_empty());
        assert!(parse_branches("\n\n  \n").is_empty());
    }

    // ---- git-output summarize (toast text) ----

    #[test]
    fn summarize_takes_last_nonempty_line() {
        let out = "remote: Enumerating objects\nTo github.com:me/repo.git\n   abc..def  main -> main";
        assert_eq!(summarize(out), "abc..def  main -> main");
    }

    #[test]
    fn summarize_empty_is_done() {
        assert_eq!(summarize("   \n\n"), "done");
        assert_eq!(summarize(""), "done");
    }

    // ---- branch picker ----

    #[test]
    fn branch_picker_open_selects_current_and_filters() {
        let bl = parse_branches("* main\n  develop\n  feature/login\n");
        let mut p = BranchPicker::new();
        assert!(!p.is_active());
        p.open(&bl);
        assert!(p.is_active());
        // count = 3 branches + 1 "Create branch…" row.
        assert_eq!(p.count(), 4);
        // Current branch (main) is selected.
        assert_eq!(p.selected_name(), Some("main".to_string()));
        // Filter to "fea" -> only feature/login (+ create row).
        p.push_char('f');
        p.push_char('e');
        p.push_char('a');
        assert_eq!(p.count(), 2);
        assert_eq!(p.name_at(0), Some("feature/login"));
        assert!(p.is_create_row(1));
    }

    #[test]
    fn branch_picker_create_mode() {
        let bl = parse_branches("* main\n");
        let mut p = BranchPicker::new();
        p.open(&bl);
        // Move to the create row (index 1) and enter create mode.
        p.move_sel(1);
        assert!(p.selection_is_create());
        p.enter_create_mode();
        assert!(p.is_creating());
        for ch in "feat/x".chars() {
            p.push_char(ch);
        }
        assert_eq!(p.query_string(), "feat/x");
        p.cancel();
        assert!(!p.is_active());
        assert!(!p.is_creating());
    }
}
