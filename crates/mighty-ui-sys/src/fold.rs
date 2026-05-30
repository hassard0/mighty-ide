//! Code folding (shim-side, per tab).
//!
//! ## Why shim-side (L17 / L21 / L28)
//!
//! Like the editable [`crate::editor::TextModel`] and the [`crate::outline`]
//! scanner, folding lives here on the Rust side: the foldable-range computation,
//! the folded-state set, and the visible↔source line mapping are all pure +
//! GPU-free so they are exhaustively unit-testable, and the scalar `mui_fold_*`
//! ABI drives them from the Mighty side (which can't hold a `Vec`, L21, nor pass
//! a buffer, L17).
//!
//! ## What is foldable
//!
//! A foldable region is a `(start, end)` line pair with `end > start`:
//!
//! * **Brace blocks** — a `{` that opens on some line and its matching `}` on a
//!   LATER line. The region runs from the opening line to the closing line. This
//!   reuses the outline scanner's string/comment-aware brace counting so a `{`
//!   inside a string / `//` comment never opens a phantom region.
//! * **Indentation blocks** — a non-blank line whose following non-blank lines
//!   are MORE indented forms a region from that header to the last such line.
//!   This captures Python-ish / YAML-ish blocks that have no braces.
//!
//! Regions can nest (an outer brace block contains inner ones). Each distinct
//! `start` line keeps only its WIDEST region (so folding line N hides the most).
//!
//! ## Folded state + the visible↔source mapping
//!
//! [`FoldState`] holds the computed ranges plus a set of folded START lines.
//! When a region starting at line `s` (ending at `e`) is folded, the INNER lines
//! `s+1 ..= e` are hidden; the header line `s` stays visible (with a "⋯ N lines"
//! indicator drawn at its end). Nested folds compose: a line is hidden if it is
//! strictly inside ANY folded region. [`FoldState::visible_to_source`] /
//! [`FoldState::source_to_visible`] convert between on-screen rows and buffer
//! lines so the body draw, the cursor, and clicks all agree.

#![allow(dead_code)]

/// A foldable region: the inclusive line span `[start, end]` with `end > start`.
/// `start` is the header line (kept visible when folded); `start+1 ..= end` are
/// the inner lines that hide when the region is folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRange {
    pub start: usize,
    pub end: usize,
}

/// Per-tab fold state: the foldable ranges of the current buffer + the set of
/// folded header lines. Recomputed (`recompute`) on edit; the folded set is
/// preserved across a recompute where the same header still opens a region.
#[derive(Debug, Default, Clone)]
pub struct FoldState {
    /// Foldable ranges, sorted by `start` then by descending `end` (widest
    /// first), with at most one range per `start` line (the widest).
    ranges: Vec<FoldRange>,
    /// Header lines whose region is currently folded.
    folded: Vec<usize>,
}

impl FoldState {
    pub fn new() -> Self {
        FoldState::default()
    }

    /// Replace the foldable ranges from a fresh scan of `lines`, preserving any
    /// folded header line that still opens a region (so an edit elsewhere keeps
    /// folds where it can). Folded headers that no longer open a region drop.
    pub fn recompute(&mut self, lines: &[&str]) {
        self.ranges = compute_ranges(lines);
        let starts: Vec<usize> = self.ranges.iter().map(|r| r.start).collect();
        self.folded.retain(|f| starts.contains(f));
        self.folded.sort_unstable();
        self.folded.dedup();
    }

    /// Replace ranges from owned line strings (convenience for the ABI, which
    /// snapshots the buffer into a `Vec<String>`).
    pub fn recompute_owned(&mut self, lines: &[String]) {
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        self.recompute(&refs);
    }

    /// The foldable ranges (read-only; for the gutter chevron draw + tests).
    pub fn ranges(&self) -> &[FoldRange] {
        &self.ranges
    }

    /// `true` if `line` is the START of a foldable region (a chevron is drawn).
    pub fn is_foldable_start(&self, line: usize) -> bool {
        self.ranges.iter().any(|r| r.start == line)
    }

    /// The region that STARTS at `line`, if any.
    pub fn region_at(&self, line: usize) -> Option<FoldRange> {
        self.ranges.iter().copied().find(|r| r.start == line)
    }

    /// `true` if the region starting at `line` is currently folded.
    pub fn is_folded(&self, line: usize) -> bool {
        self.folded.contains(&line)
    }

    /// The number of INNER (hidden-when-folded) lines of the region starting at
    /// `line`, i.e. `end - start`, or 0 if `line` starts no region.
    pub fn hidden_count_at(&self, line: usize) -> usize {
        self.region_at(line).map(|r| r.end - r.start).unwrap_or(0)
    }

    /// `true` if `line` is hidden by SOME folded region (strictly inside it:
    /// `start < line <= end`). Header lines are never hidden by their own fold.
    pub fn is_hidden(&self, line: usize) -> bool {
        self.folded.iter().any(|&s| {
            self.region_at(s)
                .map(|r| line > r.start && line <= r.end)
                .unwrap_or(false)
        })
    }

    /// Toggle the fold of the region whose header is `line`. Folds when open,
    /// unfolds when folded. No-op (returns `false`) if `line` starts no region.
    /// Returns `true` if a region was toggled.
    pub fn toggle(&mut self, line: usize) -> bool {
        if self.region_at(line).is_none() {
            return false;
        }
        if let Some(pos) = self.folded.iter().position(|&f| f == line) {
            self.folded.remove(pos);
        } else {
            self.folded.push(line);
            self.folded.sort_unstable();
        }
        true
    }

    /// Fold the INNERMOST region that contains `line` (so toggling "at the
    /// cursor" works even when the cursor is on a body line, not the header).
    /// Picks the region with the LARGEST `start` that satisfies
    /// `start <= line <= end`. Returns the folded header line, or `None`.
    pub fn toggle_at_cursor(&mut self, line: usize) -> Option<usize> {
        let header = self.enclosing_start(line)?;
        self.toggle(header);
        Some(header)
    }

    /// The header line of the innermost foldable region containing `line`
    /// (`start <= line <= end`), or `None`. "Innermost" = largest `start`.
    pub fn enclosing_start(&self, line: usize) -> Option<usize> {
        self.ranges
            .iter()
            .filter(|r| r.start <= line && line <= r.end)
            .map(|r| r.start)
            .max()
    }

    /// Fold EVERY foldable region (Fold All).
    pub fn fold_all(&mut self) {
        self.folded = self.ranges.iter().map(|r| r.start).collect();
        self.folded.sort_unstable();
        self.folded.dedup();
    }

    /// Unfold every region (Unfold All).
    pub fn unfold_all(&mut self) {
        self.folded.clear();
    }

    /// The number of VISIBLE lines given `total` buffer lines: `total` minus the
    /// count of lines hidden by a folded region. (Used to size the scrollbar /
    /// clamp scroll.)
    pub fn visible_count(&self, total: usize) -> usize {
        (0..total).filter(|&l| !self.is_hidden(l)).count()
    }

    /// Map a 0-based VISIBLE row to the buffer line it shows, walking from line 0
    /// and skipping hidden lines. Returns the source line, or the last line when
    /// `row` is past the end.
    pub fn visible_to_source(&self, row: usize, total: usize) -> usize {
        if total == 0 {
            return 0;
        }
        let mut seen = 0usize;
        for l in 0..total {
            if self.is_hidden(l) {
                continue;
            }
            if seen == row {
                return l;
            }
            seen += 1;
        }
        total - 1
    }

    /// Map a buffer `line` to its VISIBLE row index (how many non-hidden lines
    /// precede it). A hidden line maps to the row of its visible header (the
    /// nearest non-hidden line at or before it).
    pub fn source_to_visible(&self, line: usize, total: usize) -> usize {
        if total == 0 {
            return 0;
        }
        let line = line.min(total - 1);
        // The visible "anchor" for a hidden line is the nearest non-hidden line
        // at or before it (its enclosing fold header).
        let anchor = (0..=line).rev().find(|&l| !self.is_hidden(l)).unwrap_or(0);
        (0..anchor).filter(|&l| !self.is_hidden(l)).count()
    }

    /// The list of buffer lines that are visible starting from source line
    /// `first` for up to `rows` rows (skipping hidden lines). The body draw uses
    /// this to lay out exactly the rows it paints.
    pub fn visible_lines_from(&self, first: usize, rows: usize, total: usize) -> Vec<usize> {
        let mut out = Vec::with_capacity(rows);
        let mut l = first;
        while l < total && out.len() < rows {
            if !self.is_hidden(l) {
                out.push(l);
            }
            l += 1;
        }
        out
    }
}

/// Compute the foldable ranges of `lines`: brace-delimited blocks (string/comment
/// aware) plus indentation blocks, merged so each `start` keeps only its widest
/// region. Result is sorted by `start` ascending.
pub fn compute_ranges(lines: &[&str]) -> Vec<FoldRange> {
    let mut ranges: Vec<FoldRange> = Vec::new();
    brace_ranges(lines, &mut ranges);
    indent_ranges(lines, &mut ranges);
    dedup_widest(&mut ranges)
}

/// Brace-balance ranges: each `{` is matched to its `}` (string/comment masked).
/// A pair on the SAME line is not foldable (`end > start` required). Reuses the
/// same noise-stripping rules as the outline scanner.
fn brace_ranges(lines: &[&str], out: &mut Vec<FoldRange>) {
    // Stack of opening-brace line numbers.
    let mut stack: Vec<usize> = Vec::new();
    for (lineno, raw) in lines.iter().enumerate() {
        let code = strip_line_noise(raw);
        for b in code.bytes() {
            match b {
                b'{' => stack.push(lineno),
                b'}' => {
                    if let Some(open) = stack.pop() {
                        if lineno > open {
                            out.push(FoldRange { start: open, end: lineno });
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Indentation-block ranges: a non-blank line followed by a run of more-indented
/// (or blank, while still inside the run) lines, ending at the last line whose
/// indent stays strictly greater than the header's. Blank lines do not end a
/// block (they belong to whichever block surrounds them) but a trailing run of
/// blanks is trimmed off the region end.
fn indent_ranges(lines: &[&str], out: &mut Vec<FoldRange>) {
    let n = lines.len();
    for i in 0..n {
        if is_blank(lines[i]) {
            continue;
        }
        let base = indent_of(lines[i]);
        // Find the extent of the block: subsequent lines that are blank or more
        // indented than `base`.
        let mut j = i + 1;
        let mut last_nonblank = i;
        let mut saw_deeper = false;
        while j < n {
            if is_blank(lines[j]) {
                j += 1;
                continue;
            }
            if indent_of(lines[j]) > base {
                saw_deeper = true;
                last_nonblank = j;
                j += 1;
            } else {
                break;
            }
        }
        if saw_deeper && last_nonblank > i {
            out.push(FoldRange { start: i, end: last_nonblank });
        }
    }
}

/// Keep only the WIDEST range per `start` line; sort by `start` ascending. Two
/// ranges sharing a `start` (a brace block and an indent block both opening on
/// the same header) collapse to the one that hides the most lines.
fn dedup_widest(ranges: &mut [FoldRange]) -> Vec<FoldRange> {
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<FoldRange> = Vec::new();
    for r in ranges.iter().copied() {
        match out.last() {
            Some(last) if last.start == r.start => {
                // Same header: keep the wider (already first due to the sort).
            }
            _ => out.push(r),
        }
    }
    out
}

/// Leading-whitespace width of `line` in columns (tabs count as 1 here; the
/// folding heuristic only needs relative ordering, not exact tab expansion).
fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// `true` if `line` is empty or all-whitespace.
fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Blank out string contents + drop a trailing line comment so braces inside a
/// string / `//` comment don't open phantom fold regions. Mirrors the outline
/// scanner's `strip_line_noise` (kept local so folding doesn't couple to it).
fn strip_line_noise(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_str = false;
    let mut str_ch = b'"';
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == str_ch {
                in_str = false;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = true;
                str_ch = b;
                out.push(' ');
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => break,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => break,
            _ => out.push(b as char),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(src: &str) -> Vec<FoldRange> {
        let lines: Vec<&str> = src.lines().collect();
        compute_ranges(&lines)
    }

    fn state(src: &str) -> (FoldState, usize) {
        let lines: Vec<&str> = src.lines().collect();
        let total = lines.len();
        let mut f = FoldState::new();
        f.recompute(&lines);
        (f, total)
    }

    #[test]
    fn brace_block_is_foldable() {
        let r = ranges("fn main() {\n  body\n}\n");
        assert!(r.contains(&FoldRange { start: 0, end: 2 }), "{r:?}");
    }

    #[test]
    fn same_line_braces_not_foldable() {
        // `{}` on one line has end == start -> not a region.
        let r = ranges("fn f() {}\nfn g() {}\n");
        assert!(r.iter().all(|x| x.end > x.start));
        assert!(r.iter().all(|x| x.start != x.end));
        // No brace region opens (both pairs close on their own line).
        assert!(!r.iter().any(|x| matches!((x.start, x.end), (0, 0) | (1, 1))));
    }

    #[test]
    fn nested_braces_give_nested_ranges() {
        let src = "outer {\n  inner {\n    x\n  }\n}\n";
        let r = ranges(src);
        assert!(r.contains(&FoldRange { start: 0, end: 4 }), "outer {r:?}");
        assert!(r.contains(&FoldRange { start: 1, end: 3 }), "inner {r:?}");
    }

    #[test]
    fn braces_in_strings_and_comments_ignored() {
        // The `{` lives in a string and a comment; no phantom region opens, and
        // the real fn body region still spans 0..2.
        let src = "fn a() {\n  let s = \"x { y\"  // trailing { comment\n}\n";
        let r = ranges(src);
        assert!(r.contains(&FoldRange { start: 0, end: 2 }), "{r:?}");
        // No region opens on the string/comment line.
        assert!(!r.iter().any(|x| x.start == 1));
    }

    #[test]
    fn indent_block_is_foldable() {
        let src = "header:\n    a\n    b\nnext:\n    c\n";
        let r = ranges(src);
        // header (0) over the more-indented a,b (1,2).
        assert!(r.contains(&FoldRange { start: 0, end: 2 }), "{r:?}");
        // next (3) over c (4).
        assert!(r.contains(&FoldRange { start: 3, end: 4 }), "{r:?}");
    }

    #[test]
    fn indent_block_trims_trailing_blanks() {
        let src = "h:\n    x\n\n\ny\n";
        let r = ranges(src);
        // Region ends at the last MORE-indented line (1), not the trailing blanks.
        assert!(r.contains(&FoldRange { start: 0, end: 1 }), "{r:?}");
    }

    #[test]
    fn widest_range_per_start_kept() {
        // A header that opens both a brace block and an indent block keeps one.
        let src = "blk {\n    a\n    b\n}\n";
        let r = ranges(src);
        let at0: Vec<_> = r.iter().filter(|x| x.start == 0).collect();
        assert_eq!(at0.len(), 1, "one region per start: {r:?}");
        // Widest is the brace block (0..3).
        assert_eq!(*at0[0], FoldRange { start: 0, end: 3 });
    }

    #[test]
    fn mapping_is_identity_when_nothing_folded() {
        // With no folds active the visible↔source mapping is the identity and
        // every line is visible — the editor must behave exactly as before.
        let src = "o {\n  i {\n    x\n  }\n  y\n}\nz\n"; // foldable, but nothing folded
        let (f, total) = state(src);
        assert_eq!(total, 7);
        assert_eq!(f.visible_count(total), total);
        for l in 0..total {
            assert!(!f.is_hidden(l), "line {l} hidden with no folds");
            assert_eq!(f.visible_to_source(l, total), l);
            assert_eq!(f.source_to_visible(l, total), l);
        }
        assert_eq!(
            f.visible_lines_from(0, total, total),
            (0..total).collect::<Vec<_>>()
        );
    }

    #[test]
    fn visible_mapping_single_fold() {
        let src = "a {\n  b\n  c\n}\nd\n"; // lines 0..4, region 0..3
        let (mut f, total) = state(src);
        assert_eq!(total, 5);
        f.toggle(0);
        // Folded: lines 1,2,3 hidden. Visible: 0, 4.
        assert!(f.is_folded(0));
        assert!(f.is_hidden(1) && f.is_hidden(2) && f.is_hidden(3));
        assert!(!f.is_hidden(0) && !f.is_hidden(4));
        assert_eq!(f.visible_count(total), 2);
        // visible row 0 -> source 0; row 1 -> source 4.
        assert_eq!(f.visible_to_source(0, total), 0);
        assert_eq!(f.visible_to_source(1, total), 4);
        // source -> visible: 0->0; 4->1; a hidden line maps to its header's row.
        assert_eq!(f.source_to_visible(0, total), 0);
        assert_eq!(f.source_to_visible(4, total), 1);
        assert_eq!(f.source_to_visible(2, total), 0); // hidden -> header row 0
    }

    #[test]
    fn visible_mapping_nested_folds() {
        let src = "o {\n  i {\n    x\n  }\n  y\n}\nz\n"; // 0..6
        // regions: outer 0..5, inner 1..3
        let (mut f, total) = state(src);
        assert_eq!(total, 7);
        // Fold only the inner region -> hides 2,3.
        f.toggle(1);
        assert_eq!(f.visible_count(total), 5); // 0,1,4,5,6
        assert_eq!(f.visible_to_source(2, total), 4);
        // Now also fold the outer -> hides 1..5 (incl. the inner header).
        f.toggle(0);
        assert_eq!(f.visible_count(total), 2); // 0, 6
        assert_eq!(f.visible_to_source(0, total), 0);
        assert_eq!(f.visible_to_source(1, total), 6);
    }

    #[test]
    fn fold_all_and_unfold_all() {
        let src = "o {\n  i {\n    x\n  }\n}\n";
        let (mut f, total) = state(src);
        f.fold_all();
        // Both headers folded; everything inside the OUTER is hidden.
        assert!(f.is_folded(0) && f.is_folded(1));
        assert_eq!(f.visible_count(total), 1); // only line 0 visible
        f.unfold_all();
        assert_eq!(f.visible_count(total), total);
        assert!(!f.is_folded(0) && !f.is_folded(1));
    }

    #[test]
    fn toggle_at_cursor_uses_innermost_enclosing() {
        let src = "o {\n  i {\n    x\n  }\n}\n"; // outer 0..4, inner 1..3
        let (mut f, _total) = state(src);
        // Cursor on line 2 (inside inner) folds the INNER region (start 1).
        assert_eq!(f.toggle_at_cursor(2), Some(1));
        assert!(f.is_folded(1));
        assert!(!f.is_folded(0));
        // Cursor on the outer header line 0 folds the outer.
        assert_eq!(f.toggle_at_cursor(0), Some(0));
        assert!(f.is_folded(0));
    }

    #[test]
    fn toggle_non_region_line_is_noop() {
        let src = "a\nb\nc\n";
        let (mut f, _total) = state(src);
        assert!(!f.toggle(1));
        assert!(f.folded.is_empty());
    }

    #[test]
    fn recompute_preserves_folds_where_header_survives() {
        let src = "a {\n  b\n}\nc {\n  d\n}\n"; // regions 0..2 and 3..5
        let (mut f, _total) = state(src);
        f.toggle(0);
        f.toggle(3);
        assert!(f.is_folded(0) && f.is_folded(3));
        // Recompute identical buffer: both headers survive, folds preserved.
        let lines: Vec<&str> = src.lines().collect();
        f.recompute(&lines);
        assert!(f.is_folded(0) && f.is_folded(3));
        // Recompute a buffer where the 2nd region is gone: that fold drops.
        let src2 = "a {\n  b\n}\nc\n";
        let lines2: Vec<&str> = src2.lines().collect();
        f.recompute(&lines2);
        assert!(f.is_folded(0));
        assert!(!f.is_folded(3));
    }

    #[test]
    fn folding_does_not_lose_lines_round_trip() {
        // The fold state never mutates the buffer; visible_to_source over the
        // full visible range, plus the hidden lines, reconstructs every line.
        let src = "o {\n  i {\n    x\n  }\n  y\n}\nz\n";
        let (mut f, total) = state(src);
        f.toggle(1);
        let vis = f.visible_count(total);
        let mut covered: Vec<usize> = (0..vis).map(|r| f.visible_to_source(r, total)).collect();
        for l in 0..total {
            if f.is_hidden(l) {
                covered.push(l);
            }
        }
        covered.sort_unstable();
        covered.dedup();
        assert_eq!(covered, (0..total).collect::<Vec<_>>());
    }
}
