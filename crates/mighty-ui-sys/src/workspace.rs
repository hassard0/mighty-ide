//! Explicit workspace (open-folder) model — pure + unit-testable.
//!
//! Historically the "workspace root" was implicit (the opened file's parent dir
//! / git toplevel), derived ad-hoc by `panels::workspace_dir` and
//! `abi::quickopen_root`. This module makes it an EXPLICIT, settable concept:
//!
//!   * [`Workspace`] — the current `{ root, name }`. Everything that operates
//!     over the project (file tree, Quick-Open index, project Search, git,
//!     Agents discovery) reads its directory from here.
//!   * [`RecentWorkspaces`] — an MRU of recently-opened folders (cap
//!     [`RECENT_CAP`]), surfaced on the Welcome screen ("Recent Folders") and a
//!     "File: Open Recent" palette command, persisted to the config dir so it
//!     survives a restart.
//!
//! The actual re-rooting (rebuild the tree, invalidate the file index, re-run
//! git status, re-scan Agents) is wired in `crate::wsabi`; this module is just
//! the data + validation + persistence so it can be tested without a GPU/context.

use std::path::{Path, PathBuf};

/// Cap on the recent-workspaces MRU.
pub const RECENT_CAP: usize = 10;

/// The current explicit workspace: its root directory + a display name (the
/// root's basename, or `"workspace"` for a rootless / drive-root path).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    root: PathBuf,
    name: String,
}

impl Workspace {
    /// A workspace rooted at `root`, deriving the display name from its basename.
    pub fn new(root: PathBuf) -> Self {
        let name = derive_name(&root);
        Workspace { root, name }
    }

    /// The workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The workspace display name (root basename, else `"workspace"`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `true` if the root is empty (no explicit workspace yet).
    pub fn is_empty(&self) -> bool {
        self.root.as_os_str().is_empty()
    }

    /// Re-root this workspace at `root` (re-deriving the name). Returns `true`
    /// if the root actually changed (so callers can skip a redundant re-index).
    pub fn set_root(&mut self, root: PathBuf) -> bool {
        if self.root == root {
            return false;
        }
        self.name = derive_name(&root);
        self.root = root;
        true
    }
}

/// Derive a display name from a root path: the last path component, or
/// `"workspace"` when the path is empty / a bare drive/filesystem root.
fn derive_name(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workspace".to_string())
}

/// Validate + canonicalize a typed/picked folder path. Returns the absolute
/// directory `PathBuf` on success, or an error string suitable for a toast.
///
/// Rules: non-empty, exists, and is a directory. The path is made absolute
/// (joined onto the cwd when relative) and `canonicalize`d when possible so the
/// tree / index / git all agree on one concrete root. On Windows the verbatim
/// (`\\?\`) prefix that `canonicalize` adds is stripped for a friendlier name.
pub fn validate_folder(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("Enter a folder path".to_string());
    }
    let raw = PathBuf::from(trimmed);
    let abs = if raw.is_absolute() {
        raw
    } else {
        std::env::current_dir()
            .map(|c| c.join(&raw))
            .unwrap_or(raw)
    };
    if !abs.exists() {
        return Err(format!("No such folder: {}", abs.display()));
    }
    if !abs.is_dir() {
        return Err(format!("Not a folder: {}", abs.display()));
    }
    let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
    Ok(strip_verbatim(canon))
}

/// Strip the Windows verbatim `\\?\` prefix `canonicalize` adds, so the derived
/// workspace name + breadcrumbs read naturally. No-op on non-Windows / non-UNC.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// The recently-opened folders MRU (newest first, de-duplicated, capped).
#[derive(Debug, Clone, Default)]
pub struct RecentWorkspaces {
    paths: Vec<PathBuf>,
}

impl RecentWorkspaces {
    pub fn new() -> Self {
        RecentWorkspaces::default()
    }

    /// Record `path` as just-opened: move it to the front (de-duplicated),
    /// trimming to [`RECENT_CAP`].
    pub fn record(&mut self, path: PathBuf) {
        if path.as_os_str().is_empty() {
            return;
        }
        self.paths.retain(|p| p != &path);
        self.paths.insert(0, path);
        self.paths.truncate(RECENT_CAP);
    }

    /// The recents, newest first.
    pub fn entries(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&PathBuf> {
        self.paths.get(i)
    }

    /// Replace the list wholesale (used when loading from the persisted file),
    /// honoring the cap + de-dup.
    pub fn set_all(&mut self, paths: Vec<PathBuf>) {
        self.paths.clear();
        // Insert oldest-first via `record` so de-dup + cap apply, ending newest-first.
        for p in paths.into_iter().rev() {
            self.record(p);
        }
    }

    /// Serialize to a newline-joined blob (newest first), one absolute path per
    /// line. Round-trips through [`parse_blob`].
    pub fn to_blob(&self) -> String {
        let mut s = String::new();
        for p in &self.paths {
            s.push_str(&p.to_string_lossy());
            s.push('\n');
        }
        s
    }
}

/// Parse a recent-workspaces blob (one path per line; blanks / `#` comments
/// skipped) into newest-first paths.
pub fn parse_blob(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_name_from_basename() {
        let ws = Workspace::new(PathBuf::from("/proj/my-app"));
        assert_eq!(ws.name(), "my-app");
        assert_eq!(ws.root(), Path::new("/proj/my-app"));
        assert!(!ws.is_empty());
    }

    #[test]
    fn empty_workspace_is_empty() {
        let ws = Workspace::default();
        assert!(ws.is_empty());
        let ws2 = Workspace::new(PathBuf::new());
        assert_eq!(ws2.name(), "workspace");
    }

    #[test]
    fn set_root_reports_change_and_rederives_name() {
        let mut ws = Workspace::new(PathBuf::from("/a/one"));
        assert_eq!(ws.name(), "one");
        // Same root -> no change.
        assert!(!ws.set_root(PathBuf::from("/a/one")));
        // New root -> change + new name.
        assert!(ws.set_root(PathBuf::from("/b/two")));
        assert_eq!(ws.name(), "two");
        assert_eq!(ws.root(), Path::new("/b/two"));
    }

    #[test]
    fn validate_folder_rejects_blank_and_missing() {
        assert!(validate_folder("").is_err());
        assert!(validate_folder("   ").is_err());
        assert!(validate_folder("/this/does/not/exist/anywhere/12345").is_err());
    }

    #[test]
    fn validate_folder_rejects_a_file_accepts_a_dir() {
        let base = std::env::temp_dir().join(format!("mui_ws_validate_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        // A file path is rejected.
        assert!(validate_folder(&file.to_string_lossy()).is_err());
        // The directory is accepted (and made absolute).
        let ok = validate_folder(&base.to_string_lossy()).unwrap();
        assert!(ok.is_absolute());
        assert!(ok.is_dir());
        // Quoted paths are unwrapped.
        let quoted = format!("\"{}\"", base.to_string_lossy());
        assert!(validate_folder(&quoted).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn recents_mru_dedups_and_caps() {
        let mut r = RecentWorkspaces::new();
        for i in 0..(RECENT_CAP + 5) {
            r.record(PathBuf::from(format!("/p/{i}")));
        }
        assert_eq!(r.len(), RECENT_CAP);
        // Newest first.
        assert_eq!(r.get(0).unwrap(), &PathBuf::from(format!("/p/{}", RECENT_CAP + 4)));
        // Re-recording an existing path moves it to the front without growing.
        r.record(PathBuf::from("/p/7"));
        assert_eq!(r.len(), RECENT_CAP);
        assert_eq!(r.get(0).unwrap(), &PathBuf::from("/p/7"));
        // Empty paths are ignored.
        r.record(PathBuf::new());
        assert_eq!(r.len(), RECENT_CAP);
    }

    #[test]
    fn recents_blob_round_trips() {
        let mut r = RecentWorkspaces::new();
        r.record(PathBuf::from("/a/one"));
        r.record(PathBuf::from("/b/two"));
        r.record(PathBuf::from("/c/three")); // newest
        let blob = r.to_blob();
        let parsed = parse_blob(&blob);
        assert_eq!(parsed[0], PathBuf::from("/c/three"));
        let mut r2 = RecentWorkspaces::new();
        r2.set_all(parsed);
        assert_eq!(r2.entries(), r.entries());
    }

    #[test]
    fn parse_blob_skips_comments_and_blanks() {
        let parsed = parse_blob("# header\n/a/one\n\n  /b/two  \n");
        assert_eq!(parsed, vec![PathBuf::from("/a/one"), PathBuf::from("/b/two")]);
    }
}
