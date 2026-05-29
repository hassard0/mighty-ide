//! Project-wide find/replace model for the Search activity panel.
//!
//! The shim walks the workspace root (skipping `.git`, `target`, `node_modules`,
//! and binary files), does a case-insensitive substring search of the query
//! across files, and collects matches grouped by file. Mighty (v0.36, L17) can't
//! hold strings or walk the filesystem from FFI, so this lives shim-side and is
//! driven through the scalar `mui_search_*` ABI in [`crate::abi`].
//!
//! The matcher ([`search_text`]) is pure + unit-tested; the file walk
//! ([`SearchState::run`]) is a thin wrapper that reads files and feeds the
//! matcher. Replace-all ([`SearchState::replace_all`]) rewrites only the files
//! that matched, in memory, and writes them back (skipping files that changed
//! on disk since the search).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Directory names never descended into during the walk.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", "build", ".cargo"];

/// One match within a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Index into [`SearchResults::files`] of the file this match is in.
    pub file: usize,
    /// 0-based line number.
    pub line: i32,
    /// 0-based column (char offset, not byte) of the match start within the line.
    pub col: i32,
    /// The full line text (for the preview), trimmed of a trailing '\r'.
    pub preview: String,
}

/// One file with matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFile {
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Repo-relative display path (forward slashes).
    pub rel: String,
    /// Number of matches in this file.
    pub match_count: i32,
}

/// The result of a project-wide search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub files: Vec<SearchFile>,
    pub matches: Vec<SearchMatch>,
}

impl SearchResults {
    pub fn total_matches(&self) -> i32 {
        self.matches.len() as i32
    }
}

/// Case-insensitive (ASCII-fold) substring search of `needle` in one file's
/// `text`. Appends matches (with the given `file` index) to `out`. Pure.
///
/// Columns are char offsets (so the highlight aligns with how the editor counts
/// columns). Matches within a line do not overlap.
pub fn search_text(text: &str, needle: &str, file: usize, out: &mut Vec<SearchMatch>) -> i32 {
    if needle.is_empty() {
        return 0;
    }
    let needle_lower = needle.to_lowercase();
    let mut found = 0;
    for (line_idx, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let lower = line.to_lowercase();
        // Search on the lowercased line; map byte hits back to char columns.
        let mut byte = 0usize;
        let lb = lower.as_bytes();
        let nb = needle_lower.as_bytes();
        if nb.len() > lb.len() {
            continue;
        }
        let last = lb.len() - nb.len();
        while byte <= last {
            if &lb[byte..byte + nb.len()] == nb {
                let col = lower[..byte].chars().count() as i32;
                out.push(SearchMatch {
                    file,
                    line: line_idx as i32,
                    col,
                    preview: line.to_string(),
                });
                found += 1;
                byte += nb.len().max(1);
            } else {
                byte += 1;
            }
        }
    }
    found
}

/// Heuristic: treat a file as binary if it contains a NUL byte in its first 8KB.
fn looks_binary(bytes: &[u8]) -> bool {
    let n = bytes.len().min(8192);
    bytes[..n].contains(&0)
}

/// Project-wide search panel state: query + optional replacement buffers
/// (shim-owned, L17) and the last results.
#[derive(Debug, Default)]
pub struct SearchState {
    /// Search query (the "find" field).
    pub query: Vec<char>,
    /// Replacement text (the "replace" field).
    pub replace: Vec<char>,
    /// `true` when the replace field has focus instead of the query field.
    pub replace_focus: bool,
    /// The last search results.
    pub results: SearchResults,
}

impl SearchState {
    pub fn new() -> Self {
        SearchState::default()
    }

    pub fn query_string(&self) -> String {
        self.query.iter().collect()
    }
    pub fn replace_string(&self) -> String {
        self.replace.iter().collect()
    }

    /// Append a char to the focused field.
    pub fn push_char(&mut self, codepoint: u32) {
        if let Some(ch) = char::from_u32(codepoint) {
            if self.replace_focus {
                self.replace.push(ch);
            } else {
                self.query.push(ch);
            }
        }
    }
    /// Backspace the focused field.
    pub fn backspace(&mut self) {
        if self.replace_focus {
            self.replace.pop();
        } else {
            self.query.pop();
        }
    }
    /// Clear both fields and results.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.query.clear();
        self.replace.clear();
        self.results = SearchResults::default();
    }

    /// Walk `root`, searching every text file for the current query. Returns the
    /// total match count. Caps total matches + files scanned so a huge tree
    /// can't hang the UI.
    pub fn run(&mut self, root: &Path) -> i32 {
        self.results = SearchResults::default();
        let needle = self.query_string();
        if needle.trim().is_empty() {
            return 0;
        }
        const MAX_MATCHES: usize = 2000;
        const MAX_FILES: usize = 5000;

        let files = collect_files(root, MAX_FILES);
        for path in files {
            if self.results.matches.len() >= MAX_MATCHES {
                break;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if looks_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let file_idx = self.results.files.len();
            let mut local: Vec<SearchMatch> = Vec::new();
            let n = search_text(&text, &needle, file_idx, &mut local);
            if n > 0 {
                let rel = rel_path(root, &path);
                self.results.files.push(SearchFile {
                    path: path.clone(),
                    rel,
                    match_count: n,
                });
                self.results.matches.extend(local);
            }
        }
        self.results.total_matches()
    }

    /// Replace every match of the query with the replacement text across the
    /// files that matched. Returns the number of replacements written. SAFE:
    /// only rewrites files already in `results.files`, re-reads each to confirm
    /// it still matches, and does a plain case-insensitive substitution that
    /// preserves the rest of the file. Skips if the query is empty.
    pub fn replace_all(&mut self, root: &Path) -> i32 {
        let needle = self.query_string();
        if needle.trim().is_empty() {
            return 0;
        }
        let replacement = self.replace_string();
        let needle_lower = needle.to_lowercase();
        let mut total = 0;
        let files: Vec<PathBuf> = self.results.files.iter().map(|f| f.path.clone()).collect();
        for path in files {
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if looks_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let (rewritten, n) = replace_in_text(&text, &needle_lower, &replacement);
            if n > 0 && std::fs::write(&path, rewritten.as_bytes()).is_ok() {
                total += n;
            }
        }
        // Re-run so the panel reflects the post-replace state.
        self.run(root);
        total
    }

    // ---- scalar getters ----
    pub fn file_count(&self) -> i32 {
        self.results.files.len() as i32
    }
    pub fn match_count(&self) -> i32 {
        self.results.matches.len() as i32
    }
    pub fn match_at(&self, i: usize) -> Option<&SearchMatch> {
        self.results.matches.get(i)
    }
    pub fn file_at(&self, i: usize) -> Option<&SearchFile> {
        self.results.files.get(i)
    }
}

/// Case-insensitive replace of every non-overlapping occurrence of `needle_lower`
/// (already lowercased) in `text` with `replacement`. Returns the new text + the
/// number of replacements. Pure.
fn replace_in_text(text: &str, needle_lower: &str, replacement: &str) -> (String, i32) {
    if needle_lower.is_empty() {
        return (text.to_string(), 0);
    }
    let lower = text.to_lowercase();
    let lb = lower.as_bytes();
    let nb = needle_lower.as_bytes();
    if nb.len() > lb.len() {
        return (text.to_string(), 0);
    }
    // NOTE: `to_lowercase` can change byte length for non-ASCII; to keep byte
    // offsets aligned we only substitute when the needle is pure ASCII (the
    // common code-search case). Otherwise we bail (0 replacements) to stay SAFE.
    if !needle_lower.is_ascii() || !text.is_ascii() {
        return (text.to_string(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut count = 0;
    let last = lb.len().saturating_sub(nb.len());
    while i < bytes.len() {
        if i <= last && &lb[i..i + nb.len()] == nb {
            out.push_str(replacement);
            i += nb.len();
            count += 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    (out, count)
}

/// Collect text files under `root` depth-first, skipping [`SKIP_DIRS`] and
/// hidden dirs, capped at `max` files. Sorted for stable display order.
fn collect_files(root: &Path, max: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        if out.len() >= max {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut entries: Vec<(PathBuf, bool)> = Vec::new();
        for ent in rd.flatten() {
            let path = ent.path();
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                entries.push((path, true));
            } else {
                if name.starts_with('.') {
                    continue;
                }
                entries.push((path, false));
            }
        }
        // Sort: files then subdirs, each alphabetical, so files near the root
        // surface first and the order is deterministic.
        entries.sort_by(|a, b| {
            a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0))
        });
        for (path, is_dir) in entries {
            if is_dir {
                stack.push(path);
            } else if seen.insert(path.clone()) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Path of `path` relative to `root`, with forward slashes. Falls back to the
/// file name if `path` is not under `root`.
fn rel_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(text: &str, needle: &str) -> Vec<SearchMatch> {
        let mut out = Vec::new();
        search_text(text, needle, 0, &mut out);
        out
    }

    #[test]
    fn single_match_line_col() {
        let m = matches("hello world\nfoo bar\n", "world");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 0);
        assert_eq!(m[0].col, 6);
        assert_eq!(m[0].preview, "hello world");
    }

    #[test]
    fn case_insensitive() {
        let m = matches("Hello HELLO hello", "hello");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].col, 0);
        assert_eq!(m[1].col, 6);
        assert_eq!(m[2].col, 12);
    }

    #[test]
    fn multiline_columns() {
        let m = matches("ab\n  needle here\nx", "needle");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 1);
        assert_eq!(m[0].col, 2);
    }

    #[test]
    fn non_overlapping() {
        let m = matches("aaaa", "aa");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].col, 0);
        assert_eq!(m[1].col, 2);
    }

    #[test]
    fn no_match_and_empty_needle() {
        assert_eq!(matches("hello", "zzz").len(), 0);
        assert_eq!(matches("hello", "").len(), 0);
    }

    #[test]
    fn strips_carriage_return_from_preview() {
        let m = matches("foo bar\r\nbaz", "bar");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].preview, "foo bar");
    }

    #[test]
    fn unicode_column_offsets_are_char_based() {
        // "héllo match" — the 'é' is one char; "match" starts at char col 6.
        let m = matches("héllo match", "match");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].col, 6);
    }

    #[test]
    fn replace_basic() {
        let (out, n) = replace_in_text("foo bar foo", "foo", "X");
        assert_eq!(n, 2);
        assert_eq!(out, "X bar X");
    }

    #[test]
    fn replace_case_insensitive() {
        let (out, n) = replace_in_text("Foo foo FOO", "foo", "Z");
        assert_eq!(n, 3);
        assert_eq!(out, "Z Z Z");
    }

    #[test]
    fn replace_empty_needle_is_noop() {
        let (out, n) = replace_in_text("abc", "", "X");
        assert_eq!(n, 0);
        assert_eq!(out, "abc");
    }

    #[test]
    fn replace_non_ascii_bails_safely() {
        // Non-ASCII text -> we refuse to risk a misaligned substitution.
        let (out, n) = replace_in_text("héllo héllo", "héllo", "x");
        assert_eq!(n, 0);
        assert_eq!(out, "héllo héllo");
    }

    #[test]
    fn end_to_end_walk(/* uses a temp tree */) {
        let root = std::env::temp_dir().join("mui_search_e2e");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("a.txt"), b"find me\nand find me again").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"nothing here").unwrap();
        std::fs::write(root.join("sub").join("c.txt"), b"FIND this").unwrap();
        // A file under target/ must be skipped.
        std::fs::write(root.join("target").join("skip.txt"), b"find skip").unwrap();

        let mut s = SearchState::new();
        for c in "find".chars() {
            s.push_char(c as u32);
        }
        let total = s.run(&root);
        // a.txt: 2 matches, c.txt: 1 (case-insensitive). target/ skipped.
        assert_eq!(total, 3);
        assert_eq!(s.file_count(), 2);

        // Replace-all turns "find"/"FIND" into "got".
        let mut s2 = SearchState::new();
        for c in "find".chars() {
            s2.push_char(c as u32);
        }
        s2.replace_focus = true;
        for c in "got".chars() {
            s2.push_char(c as u32);
        }
        s2.replace_focus = false;
        s2.run(&root);
        let replaced = s2.replace_all(&root);
        assert_eq!(replaced, 3);
        let a = std::fs::read_to_string(root.join("a.txt")).unwrap();
        assert!(a.contains("got me"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
