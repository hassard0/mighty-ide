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
    /// Content fingerprint captured when the search results were produced.
    /// Replace-all skips the file if this no longer matches the bytes on disk.
    pub fingerprint: u64,
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

/// Case-insensitive substring search of `needle` in one file's `text`.
/// Appends matches (with the given `file` index) to `out`. Pure.
///
/// Columns are char offsets (so the highlight aligns with how the editor counts
/// columns). Matches within a line do not overlap.
pub fn search_text(text: &str, needle: &str, file: usize, out: &mut Vec<SearchMatch>) -> i32 {
    if needle.is_empty() {
        return 0;
    }
    let needle_folded = fold_needle(needle);
    if needle_folded.is_empty() {
        return 0;
    }
    let mut found = 0;
    for (line_idx, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let folded = fold_haystack(line);
        if needle_folded.len() > folded.len() {
            continue;
        }
        let mut idx = 0usize;
        let last = folded.len() - needle_folded.len();
        while idx <= last {
            if folded[idx..idx + needle_folded.len()]
                .iter()
                .map(|f| f.ch)
                .eq(needle_folded.iter().copied())
            {
                let col = folded[idx].char_idx as i32;
                out.push(SearchMatch {
                    file,
                    line: line_idx as i32,
                    col,
                    preview: line.to_string(),
                });
                found += 1;
                idx += needle_folded.len().max(1);
            } else {
                idx += 1;
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

pub(crate) fn content_fingerprint(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
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

    /// Clear derived results while preserving the user's query/replace draft.
    pub fn clear_results(&mut self) -> bool {
        let changed = !self.results.files.is_empty() || !self.results.matches.is_empty();
        self.results = SearchResults::default();
        changed
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
                    fingerprint: content_fingerprint(&bytes),
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
    #[allow(dead_code)]
    pub fn replace_all(&mut self, root: &Path) -> i32 {
        self.replace_all_with_changed_paths(root).0
    }

    /// Like [`Self::replace_all`], but also returns the paths that were written.
    /// The UI uses this to refresh clean open tabs after project-wide replaces.
    pub fn replace_all_with_changed_paths(&mut self, root: &Path) -> (i32, Vec<PathBuf>) {
        let (total, changed, _, _) =
            self.replace_all_with_changed_paths_skipping(root, |_| false);
        (total, changed)
    }

    /// Like [`Self::replace_all_with_changed_paths`], but lets the caller skip
    /// paths that are unsafe to rewrite, such as dirty open editor buffers.
    pub fn replace_all_with_changed_paths_skipping(
        &mut self,
        root: &Path,
        mut should_skip: impl FnMut(&Path) -> bool,
    ) -> (i32, Vec<PathBuf>, usize, usize) {
        let needle = self.query_string();
        if needle.trim().is_empty() {
            return (0, Vec::new(), 0, 0);
        }
        let replacement = self.replace_string();
        let mut total = 0;
        let mut changed = Vec::new();
        let mut skipped = 0;
        let mut stale = 0;
        let files: Vec<(PathBuf, u64)> = self
            .results
            .files
            .iter()
            .map(|f| (f.path.clone(), f.fingerprint))
            .collect();
        for (path, fingerprint) in files {
            if should_skip(&path) {
                skipped += 1;
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if looks_binary(&bytes) {
                continue;
            }
            if content_fingerprint(&bytes) != fingerprint {
                stale += 1;
                continue;
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let (rewritten, n) = replace_in_text(&text, &needle, &replacement);
            if n > 0 && std::fs::write(&path, rewritten.as_bytes()).is_ok() {
                total += n;
                changed.push(path);
            }
        }
        // Re-run so the panel reflects the post-replace state.
        self.run(root);
        (total, changed, skipped, stale)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FoldedChar {
    ch: char,
    byte_start: usize,
    byte_end: usize,
    char_idx: usize,
}

fn fold_needle(needle: &str) -> Vec<char> {
    needle.chars().flat_map(char::to_lowercase).collect()
}

fn fold_haystack(text: &str) -> Vec<FoldedChar> {
    let mut out = Vec::new();
    for (char_idx, (byte_start, ch)) in text.char_indices().enumerate() {
        let byte_end = byte_start + ch.len_utf8();
        for folded in ch.to_lowercase() {
            out.push(FoldedChar {
                ch: folded,
                byte_start,
                byte_end,
                char_idx,
            });
        }
    }
    out
}

fn folded_match_ranges(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle_folded = fold_needle(needle);
    if needle_folded.is_empty() {
        return Vec::new();
    }
    let folded = fold_haystack(text);
    if needle_folded.len() > folded.len() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut idx = 0usize;
    let last = folded.len() - needle_folded.len();
    while idx <= last {
        if folded[idx..idx + needle_folded.len()]
            .iter()
            .map(|f| f.ch)
            .eq(needle_folded.iter().copied())
        {
            let start = folded[idx].byte_start;
            let end = folded[idx + needle_folded.len() - 1].byte_end;
            if ranges.last().map_or(true, |&(_, prev_end)| start >= prev_end) {
                ranges.push((start, end));
            }
            idx += needle_folded.len().max(1);
        } else {
            idx += 1;
        }
    }
    ranges
}

/// Case-insensitive replace of every non-overlapping occurrence of `needle` in
/// `text` with `replacement`. Returns the new text + replacement count. Pure.
fn replace_in_text(text: &str, needle: &str, replacement: &str) -> (String, i32) {
    if needle.is_empty() {
        return (text.to_string(), 0);
    }
    let ranges = folded_match_ranges(text, needle);
    if ranges.is_empty() {
        return (text.to_string(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end) in &ranges {
        out.push_str(&text[cursor..*start]);
        out.push_str(replacement);
        cursor = *end;
    }
    out.push_str(&text[cursor..]);
    (out, ranges.len() as i32)
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
    fn unicode_case_insensitive_search_maps_to_original_columns() {
        let m = matches("α β CAFÉ café", "café");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].col, 4);
        assert_eq!(m[1].col, 9);
        assert_eq!(m[0].preview, "α β CAFÉ café");
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
    fn replace_non_ascii_matches_original_byte_ranges() {
        let (out, n) = replace_in_text("héllo HÉLLO héllo", "héllo", "x");
        assert_eq!(n, 3);
        assert_eq!(out, "x x x");
    }

    #[test]
    fn replace_non_ascii_preserves_surrounding_text() {
        let (out, n) = replace_in_text("pre café post CAFÉ", "café", "tea");
        assert_eq!(n, 2);
        assert_eq!(out, "pre tea post tea");
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

    #[test]
    fn search_clear_results_preserves_fields() {
        let mut state = SearchState::new();
        for ch in "needle".chars() {
            state.push_char(ch as u32);
        }
        state.replace_focus = true;
        for ch in "replacement".chars() {
            state.push_char(ch as u32);
        }
        state.results.files.push(SearchFile {
            path: PathBuf::from("hit.mty"),
            rel: "hit.mty".to_string(),
            match_count: 1,
            fingerprint: 0,
        });
        state.results.matches.push(SearchMatch {
            file: 0,
            line: 2,
            col: 4,
            preview: "let needle = 1".to_string(),
        });

        assert!(state.clear_results());
        assert_eq!(state.query_string(), "needle");
        assert_eq!(state.replace_string(), "replacement");
        assert!(state.replace_focus);
        assert_eq!(state.file_count(), 0);
        assert_eq!(state.match_count(), 0);
        assert!(!state.clear_results());
    }
}
