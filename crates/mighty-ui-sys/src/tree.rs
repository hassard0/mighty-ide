//! File-tree sidebar model (pure, unit-testable).
//!
//! Scans a root directory and produces a flat list of visible rows (depth-first,
//! directories before files, alphabetical within a kind). Directories are
//! lazily expandable: a collapsed dir contributes one row; expanding it splices
//! its children in beneath it. Mighty drives this through scalar getters
//! (`count`, `is_dir`, `depth`) and a draw call that renders the row's name
//! shim-side (L17 — Mighty can't hold the name string).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One visible row in the flattened tree.
#[derive(Debug, Clone)]
pub struct TreeRow {
    /// Full path of this entry.
    pub path: PathBuf,
    /// True for a directory, false for a file.
    pub is_dir: bool,
    /// Indentation depth (0 = direct child of root).
    pub depth: u32,
    /// For directories: whether currently expanded.
    pub expanded: bool,
}

impl TreeRow {
    /// Basename to draw (with a trailing `/` for directories).
    pub fn display_name(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned());
        if self.is_dir {
            format!("{name}/")
        } else {
            name
        }
    }
}

/// The file-tree model: a root + the set of expanded directory paths + the
/// current flattened row list (rebuilt on every refresh/toggle).
#[derive(Debug, Default)]
pub struct FileTree {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    rows: Vec<TreeRow>,
}

impl FileTree {
    pub fn new() -> Self {
        FileTree::default()
    }

    /// Set the root directory (the directory containing the initial file, or
    /// cwd) and rebuild the row list. Any previously-expanded dirs that still
    /// exist remain expanded.
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root;
        self.rebuild();
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, i: usize) -> Option<&TreeRow> {
        self.rows.get(i)
    }

    /// Re-scan from the root, honoring the current expanded set.
    pub fn refresh(&mut self) {
        self.rebuild();
    }

    /// Toggle expand/collapse of the directory at row `i`. No-op for files or
    /// out-of-range rows. Returns true if it toggled a directory.
    /// Collapse every expanded directory (the "collapse all" header action).
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild();
    }

    pub fn toggle(&mut self, i: usize) -> bool {
        let Some(row) = self.rows.get(i) else {
            return false;
        };
        if !row.is_dir {
            return false;
        }
        let path = row.path.clone();
        if self.expanded.contains(&path) {
            // Collapse: also drop any expanded descendants so re-expanding a
            // parent doesn't auto-explode the whole subtree.
            self.expanded.retain(|p| !p.starts_with(&path) || p == &path);
            self.expanded.remove(&path);
        } else {
            self.expanded.insert(path);
        }
        self.rebuild();
        true
    }

    /// Rebuild the flat row list by a depth-first walk from the root, descending
    /// only into expanded directories.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        if self.root.as_os_str().is_empty() {
            self.rows = rows;
            return;
        }
        Self::walk(&self.root, 0, &self.expanded, &mut rows);
        self.rows = rows;
    }

    fn walk(dir: &Path, depth: u32, expanded: &BTreeSet<PathBuf>, out: &mut Vec<TreeRow>) {
        let entries = match Self::read_sorted(dir) {
            Some(e) => e,
            None => return,
        };
        for (path, is_dir) in entries {
            let is_expanded = is_dir && expanded.contains(&path);
            out.push(TreeRow {
                path: path.clone(),
                is_dir,
                depth,
                expanded: is_expanded,
            });
            if is_expanded {
                Self::walk(&path, depth + 1, expanded, out);
            }
        }
    }

    /// Read a directory's immediate children, dirs first then files, each group
    /// sorted case-insensitively by name. Returns None on read error.
    fn read_sorted(dir: &Path) -> Option<Vec<(PathBuf, bool)>> {
        let rd = std::fs::read_dir(dir).ok()?;
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();
        for ent in rd.flatten() {
            let path = ent.path();
            // Skip hidden entries (leading dot) to keep the tree tidy.
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
        let key = |p: &PathBuf| {
            p.file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        dirs.sort_by_key(key);
        files.sort_by_key(key);
        let mut out: Vec<(PathBuf, bool)> = Vec::with_capacity(dirs.len() + files.len());
        out.extend(dirs.into_iter().map(|p| (p, true)));
        out.extend(files.into_iter().map(|p| (p, false)));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp dir tree:
    ///   root/
    ///     sub/
    ///       deep.txt
    ///     a.txt
    ///     b.txt
    fn make_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mui_tree_{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("deep.txt"), b"deep").unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("b.txt"), b"b").unwrap();
        // a hidden file that should be skipped
        std::fs::write(root.join(".hidden"), b"x").unwrap();
        root
    }

    #[test]
    fn scan_lists_dirs_first_then_files_collapsed() {
        let root = make_tree("scan");
        let mut t = FileTree::new();
        t.set_root(root.clone());

        // Collapsed: 3 rows -> sub/ (dir), a.txt, b.txt. Hidden skipped.
        assert_eq!(t.count(), 3);
        let r0 = t.get(0).unwrap();
        assert!(r0.is_dir);
        assert_eq!(r0.depth, 0);
        assert_eq!(r0.display_name(), "sub/");

        let r1 = t.get(1).unwrap();
        assert!(!r1.is_dir);
        assert_eq!(r1.display_name(), "a.txt");

        let r2 = t.get(2).unwrap();
        assert_eq!(r2.display_name(), "b.txt");
    }

    #[test]
    fn expand_and_collapse_splices_children() {
        let root = make_tree("expand");
        let mut t = FileTree::new();
        t.set_root(root);
        assert_eq!(t.count(), 3);

        // Expand row 0 (sub/). Its child deep.txt splices in at depth 1.
        assert!(t.toggle(0));
        assert_eq!(t.count(), 4);
        assert!(t.get(0).unwrap().expanded);
        let child = t.get(1).unwrap();
        assert_eq!(child.depth, 1);
        assert_eq!(child.display_name(), "deep.txt");
        assert!(!child.is_dir);

        // Collapse again.
        assert!(t.toggle(0));
        assert_eq!(t.count(), 3);
        assert!(!t.get(0).unwrap().expanded);
    }

    #[test]
    fn toggle_on_file_is_noop() {
        let root = make_tree("toggle_file");
        let mut t = FileTree::new();
        t.set_root(root);
        // Row 1 is a.txt (a file).
        assert!(!t.toggle(1));
        assert_eq!(t.count(), 3);
    }

    #[test]
    fn empty_root_yields_no_rows() {
        let mut t = FileTree::new();
        assert_eq!(t.count(), 0);
        t.set_root(PathBuf::new());
        assert_eq!(t.count(), 0);
    }
}
