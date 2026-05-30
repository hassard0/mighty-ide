//! Universal Quick-Open (Ctrl+P) — the defining modern-editor DX feature.
//!
//! A single fast fuzzy finder whose **mode** switches on the first character of
//! the query:
//!
//! * (no prefix) = **files** — fuzzy-find across all workspace files. The shim
//!   walks the workspace root once + caches the result; binaries and the usual
//!   noise dirs (`.git` / `target` / `node_modules`) are skipped. An empty query
//!   shows the **Recently Opened (MRU)** list.
//! * `>` = **commands** — routes to the existing command palette ([`crate::palette`]).
//! * `@` = **symbols in the current file** — uses the Outline symbol provider
//!   ([`crate::outline`]).
//! * `:` = **go to line** — parses the number after the colon.
//!
//! Like the palette/outline, all state lives shim-side and Mighty drives it
//! through a scalar-only ABI (L17): open, feed chars/backspace, move the
//! selection, read back the chosen row's path/line/symbol, and dispatch.
//!
//! The fuzzy matcher ([`fuzzy_match`]) returns the matched character indices so
//! the renderer can highlight them in the accent color, and a score that favors
//! basename matches, word/path-boundary hits, and contiguous runs.

use std::path::{Path, PathBuf};

use crate::ffi::MuiColor;
use crate::outline::SymKind;
use crate::theme;

// ===========================================================================
// Mode
// ===========================================================================

/// Which finder mode the current query selects (by its first char).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No prefix: fuzzy file search (empty query → MRU recents).
    Files = 0,
    /// `>` prefix: command palette.
    Commands = 1,
    /// `@` prefix: symbols in the active file.
    Symbols = 2,
    /// `:` prefix: go to line.
    GotoLine = 3,
}

impl Mode {
    /// Classify a raw query string by its leading character.
    pub fn of(query: &str) -> Mode {
        match query.chars().next() {
            Some('>') => Mode::Commands,
            Some('@') => Mode::Symbols,
            Some(':') => Mode::GotoLine,
            _ => Mode::Files,
        }
    }

    /// The query with the mode-prefix char stripped (the part that filters).
    pub fn strip(self, query: &str) -> &str {
        match self {
            Mode::Files => query,
            _ => {
                let mut it = query.chars();
                it.next();
                it.as_str()
            }
        }
    }

    /// Scalar value exposed over the ABI (`mui_qo_mode`).
    pub fn scalar(self) -> i32 {
        self as i32
    }
}

// ===========================================================================
// Fuzzy matcher (with matched-index output for highlighting)
// ===========================================================================

/// The result of a successful fuzzy match: a score (higher = better) plus the
/// byte... no — the *char* indices (into the matched haystack) that the query
/// matched, in order, for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    /// Char indices in the haystack that were matched (ascending).
    pub indices: Vec<usize>,
}

/// Case-insensitive subsequence fuzzy match of `query` against `hay`.
///
/// Returns `None` when `query` is not a subsequence of `hay`. An empty query
/// matches everything with score 0 and no highlighted indices.
///
/// Scoring (higher is better) rewards the qualities that make a fuzzy finder
/// feel "smart":
/// * **contiguous runs** — adjacent matched chars score a growing bonus;
/// * **word / path boundaries** — a match right after `/`, `\`, `_`, `-`, `.`,
///   a space, or a lower→Upper camelCase hump scores a boundary bonus;
/// * **start of string** — the very first char matching scores highest;
/// * **gaps** — unmatched chars between matches apply a small penalty.
///
/// This is the generic core; [`score_path`] layers basename preference on top.
pub fn fuzzy_match(hay: &str, query: &str) -> Option<Match> {
    if query.is_empty() {
        return Some(Match { score: 0, indices: Vec::new() });
    }
    let hay_chars: Vec<char> = hay.chars().collect();
    let q_chars: Vec<char> = query.chars().collect();

    let mut indices = Vec::with_capacity(q_chars.len());
    let mut score: i32 = 0;
    let mut qi = 0usize;
    let mut prev_match: Option<usize> = None;
    // Length of the current contiguous run of matched chars (0 outside a run).
    let mut run: i32 = 0;

    for (hi, &hc) in hay_chars.iter().enumerate() {
        if qi >= q_chars.len() {
            break;
        }
        if hc.eq_ignore_ascii_case(&q_chars[qi]) {
            // Base reward for any match.
            let mut s = 1;
            // Boundary / start bonus.
            if hi == 0 {
                s += 12;
            } else if is_boundary(&hay_chars, hi) {
                s += 8;
            }
            // Contiguity: this match directly follows the previous one. The run
            // bonus GROWS with the run length so a long contiguous span (e.g.
            // "main" in "main.rs") decisively outscores the same chars scattered
            // across boundaries (e.g. "m_a_i_n.rs"), where every char restarts
            // the run at the flat +8 boundary bonus.
            match prev_match {
                Some(p) if p + 1 == hi => {
                    run += 1;
                    s += 6 + 4 * run;
                }
                Some(p) => {
                    run = 0;
                    // Gap penalty grows with distance, capped so far matches
                    // still beat no match.
                    let gap = (hi - p - 1) as i32;
                    s -= gap.min(4);
                }
                None => {}
            }
            score += s;
            indices.push(hi);
            prev_match = Some(hi);
            qi += 1;
        }
    }

    if qi == q_chars.len() {
        Some(Match { score, indices })
    } else {
        None
    }
}

/// `true` if `hay[i]` sits on a word/path boundary (the char before it is a
/// separator, or it begins a camelCase hump).
fn is_boundary(hay: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = hay[i - 1];
    if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ') {
        return true;
    }
    // lower/digit -> Upper camelCase hump.
    hay[i].is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit())
}

/// Score a workspace-relative `path` against `query`, preferring basename hits.
///
/// We match against the basename first (so typing `main` ranks `src/main.rs`
/// highly) and, if that matches, add a strong basename bonus and translate the
/// matched indices back into the full relative path for highlighting. If the
/// basename alone doesn't match, we fall back to matching the whole relative
/// path. Returns `None` if neither matches.
pub fn score_path(rel: &str, query: &str) -> Option<Match> {
    if query.is_empty() {
        return Some(Match { score: 0, indices: Vec::new() });
    }
    let base_start = rel
        .rfind(['/', '\\'])
        .map(|p| p + 1)
        .unwrap_or(0);
    let base = &rel[base_start..];
    let base_char_off = rel[..base_start].chars().count();

    if let Some(m) = fuzzy_match(base, query) {
        // Shift the basename indices into full-path char space.
        let indices = m.indices.iter().map(|i| i + base_char_off).collect();
        // Strong basename-match bonus + a small bonus for a short path (shallow
        // files rank above deep ones for equal matches).
        let depth = rel.matches(['/', '\\']).count() as i32;
        let score = m.score + 30 - depth;
        return Some(Match { score, indices });
    }
    // Fall back to the whole relative path.
    fuzzy_match(rel, query)
}

// ===========================================================================
// Workspace file index (cached)
// ===========================================================================

/// One indexed workspace file: its absolute `path` and the workspace-relative,
/// forward-slashed display string used for matching + rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub path: PathBuf,
    /// Workspace-relative path with `/` separators (matched + displayed).
    pub rel: String,
}

impl IndexedFile {
    /// The basename (display, bold) portion of `rel`.
    pub fn basename(&self) -> &str {
        let start = self.rel.rfind('/').map(|p| p + 1).unwrap_or(0);
        &self.rel[start..]
    }

    /// The directory (dim) portion of `rel`, or "" for a root file.
    pub fn dir(&self) -> &str {
        match self.rel.rfind('/') {
            Some(p) => &self.rel[..p],
            None => "",
        }
    }
}

/// Directory names skipped entirely during the walk.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".hg", ".svn", "dist", ".cache"];

/// File extensions treated as binary / non-text and skipped.
const SKIP_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "svg", "pdf", "zip", "gz", "tar", "7z",
    "rar", "exe", "dll", "so", "dylib", "lib", "a", "o", "obj", "bin", "class", "jar", "wasm",
    "ttf", "otf", "woff", "woff2", "mp3", "mp4", "wav", "mov", "avi", "lock",
];

/// `true` if `name` is a directory we should not descend into.
fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name) || (name.starts_with('.') && name != ".")
}

/// `true` if `path` looks like a binary / non-text file we should not index.
fn skip_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => SKIP_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Cap on indexed files so a huge tree never stalls the finder.
const MAX_INDEXED: usize = 20_000;

/// Walk `root` recursively, returning the indexed text files (skipping the noise
/// dirs + binaries). Deterministic order: directories sorted, files sorted, so
/// the cache + tests are stable. Pure (no shim state); unit-tested.
pub fn walk_workspace(root: &Path) -> Vec<IndexedFile> {
    let mut out = Vec::new();
    walk_into(root, root, &mut out);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn walk_into(root: &Path, dir: &Path, out: &mut Vec<IndexedFile>) {
    if out.len() >= MAX_INDEXED {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if out.len() >= MAX_INDEXED {
            return;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            if skip_dir(name) {
                continue;
            }
            walk_into(root, &p, out);
        } else if p.is_file() {
            if skip_file(&p) {
                continue;
            }
            let rel = rel_display(root, &p);
            out.push(IndexedFile { path: p, rel });
        }
    }
}

/// The workspace-relative, forward-slashed display string for `path` under
/// `root` (falls back to the basename if `path` is not under `root`).
fn rel_display(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let s = rel.to_string_lossy();
    s.replace('\\', "/")
}

// ===========================================================================
// MRU (most-recently-used) tracking
// ===========================================================================

/// Cap on remembered recents.
const MRU_CAP: usize = 20;

/// A bounded most-recently-used list of opened file paths (front = newest).
#[derive(Debug, Default, Clone)]
pub struct Mru {
    paths: Vec<PathBuf>,
}

#[allow(dead_code)]
impl Mru {
    pub fn new() -> Self {
        Mru::default()
    }

    /// Record `path` as just-opened: move it to the front (de-duplicated),
    /// trimming to [`MRU_CAP`].
    pub fn record(&mut self, path: PathBuf) {
        self.paths.retain(|p| p != &path);
        self.paths.insert(0, path);
        self.paths.truncate(MRU_CAP);
    }

    /// The recents, newest first.
    pub fn entries(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

// ===========================================================================
// A finder row (the unified shape the ABI streams + the renderer draws)
// ===========================================================================

/// One row in the finder list. `kind` drives the icon; `name`/`dir` are the
/// display strings; `indices` are the matched char positions in `name` (for
/// highlighting); `target` carries the mode-specific payload index (a file
/// index, a symbol index, or unused for go-to-line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Icon/kind discriminant (see [`Row::ICON_*`] / [`row_icon`]).
    pub icon_kind: i32,
    /// Bold display name (basename / symbol name).
    pub name: String,
    /// Dim secondary text (dir / kind label).
    pub dir: String,
    /// Matched char indices into `name` for accent highlighting.
    pub indices: Vec<usize>,
    /// Mode payload: file index (Files/MRU) or symbol index (Symbols).
    pub target: i32,
}

/// Icon-kind discriminants for [`Row::icon_kind`]. Files reuse the language
/// file glyphs; symbols map to [`SymKind`]; recents get a clock.
impl Row {
    pub const ICON_FILE_MTY: i32 = 100;
    pub const ICON_FILE_TOML: i32 = 101;
    pub const ICON_FILE_MD: i32 = 102;
    pub const ICON_FILE_TXT: i32 = 103;
    pub const ICON_RECENT: i32 = 104;
    pub const ICON_LINE: i32 = 105;
}

/// The icon-kind for a file by basename.
fn file_icon_kind(base: &str) -> i32 {
    if base.ends_with(".mty") {
        Row::ICON_FILE_MTY
    } else if base.ends_with(".toml") {
        Row::ICON_FILE_TOML
    } else if base.ends_with(".md") {
        Row::ICON_FILE_MD
    } else {
        Row::ICON_FILE_TXT
    }
}

/// Map an `icon_kind` to a vector icon path + color for drawing.
fn row_icon(kind: i32) -> (&'static str, MuiColor) {
    use crate::icons;
    match kind {
        Row::ICON_FILE_MTY => (icons::FILE_MTY, theme::SYN_TYPE()),
        Row::ICON_FILE_TOML => (icons::FILE_TOML, theme::WARNING()),
        Row::ICON_FILE_MD => (icons::FILE_MD, theme::INFO()),
        Row::ICON_FILE_TXT => (icons::FILE_TXT, theme::TEXT_3()),
        Row::ICON_RECENT => (icons::REFRESH, theme::ACCENT_BRIGHT()),
        Row::ICON_LINE => (icons::CHEVRON, theme::ACCENT_BRIGHT()),
        k if (0..100).contains(&k) => {
            // A SymKind scalar.
            let sk = sym_kind_from_scalar(k);
            (sk.icon(), sk.color())
        }
        _ => (icons::FILE_TXT, theme::TEXT_3()),
    }
}

/// Reconstruct a [`SymKind`] from its scalar (mirrors the enum's `as i32`).
fn sym_kind_from_scalar(k: i32) -> SymKind {
    match k {
        0 => SymKind::Function,
        1 => SymKind::Struct,
        2 => SymKind::Enum,
        3 => SymKind::Agent,
        4 => SymKind::Protocol,
        5 => SymKind::TypeAlias,
        6 => SymKind::Impl,
        7 => SymKind::Field,
        8 => SymKind::Variant,
        _ => SymKind::Const,
    }
}

// ===========================================================================
// The Quick-Open engine
// ===========================================================================

/// Max rows drawn at once (the visible window).
const VISIBLE: usize = 12;

/// Shim-owned Quick-Open state: the typed query, the cached workspace index, the
/// MRU, the computed rows for the current query, and the selection. The Symbols
/// and Commands modes read their data from the outer [`crate::MuiContext`] when
/// building rows, so this engine holds only the file index and MRU itself.
#[derive(Debug, Default)]
pub struct QuickOpen {
    active: bool,
    query: String,
    /// Cached workspace file index; rebuilt when the root changes / on refresh.
    index: Vec<IndexedFile>,
    /// The root the index was built for (rebuild when it differs).
    index_root: Option<PathBuf>,
    mru: Mru,
    /// Computed rows for the current query (rank order).
    rows: Vec<Row>,
    sel: usize,
}

impl QuickOpen {
    pub fn new() -> Self {
        QuickOpen::default()
    }

    /// Ensure the file index is built for `root` (rebuild if the root changed or
    /// `force`). Returns the indexed file count.
    pub fn ensure_index(&mut self, root: &Path, force: bool) -> usize {
        let stale = self.index_root.as_deref() != Some(root);
        if force || stale || self.index.is_empty() {
            self.index = walk_workspace(root);
            self.index_root = Some(root.to_path_buf());
        }
        self.index.len()
    }

    /// Record `path` as recently opened (called whenever any file opens).
    pub fn record_mru(&mut self, path: PathBuf) {
        self.mru.record(path);
    }

    #[allow(dead_code)]
    pub fn mru_len(&self) -> usize {
        self.mru.len()
    }

    /// The recently-opened paths, newest first (reused by the Welcome screen's
    /// "Recently Opened" column).
    pub fn recent_paths(&self) -> Vec<PathBuf> {
        self.mru.entries().to_vec()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn mode(&self) -> Mode {
        Mode::of(&self.query)
    }

    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    #[allow(dead_code)]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn row(&self, i: usize) -> Option<&Row> {
        self.rows.get(i)
    }

    /// The parsed line number for go-to-line mode (`:NN`), or `-1` if not in
    /// that mode / not a number. 1-based, as the user types it.
    pub fn goto_line(&self) -> i32 {
        if self.mode() != Mode::GotoLine {
            return -1;
        }
        let n = Mode::strip(Mode::GotoLine, &self.query).trim();
        n.parse::<i32>().ok().filter(|v| *v >= 1).unwrap_or(-1)
    }

    /// Open the finder: clear the query, build the file rows (MRU when empty).
    /// The caller seeds the symbol provider via [`set_symbol_rows`] when needed.
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.sel = 0;
        self.rebuild_files();
    }

    /// Close + clear all transient state (keeps the cached index + MRU).
    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
        self.rows.clear();
        self.sel = 0;
    }

    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.sel = 0;
        self.recompute_self_modes();
    }

    /// Backspace one char. Returns `true` if a char was removed (so the caller,
    /// which owns the symbol/command providers, can re-seed those modes after a
    /// prefix is deleted back into Files mode).
    pub fn backspace(&mut self) -> bool {
        let had = self.query.pop().is_some();
        self.sel = 0;
        self.recompute_self_modes();
        had
    }

    /// Recompute rows for the modes this engine owns by itself (Files / MRU /
    /// GotoLine). Symbols + Commands are re-seeded by the caller (they need
    /// outer state), so for those modes we leave `rows` for the caller to fill.
    fn recompute_self_modes(&mut self) {
        match self.mode() {
            Mode::Files => self.rebuild_files(),
            Mode::GotoLine => self.rebuild_goto(),
            // Symbols / Commands rows are owned by the caller (set_*_rows).
            Mode::Symbols | Mode::Commands => {}
        }
    }

    /// Build the Files-mode rows: when the query (sans prefix) is empty, the MRU
    /// recents; otherwise the fuzzy-ranked workspace files.
    fn rebuild_files(&mut self) {
        let q = Mode::strip(Mode::Files, &self.query);
        if q.is_empty() {
            self.rows = self.mru_rows();
            self.clamp_sel();
            return;
        }
        let mut scored: Vec<(i32, usize, Row)> = Vec::new();
        for (i, f) in self.index.iter().enumerate() {
            if let Some(m) = score_path(&f.rel, q) {
                // The highlight indices are over the FULL rel path, but the row
                // shows the basename bold + dir dim; translate into basename
                // char space (drop dir-side indices).
                let base = f.basename();
                let dir = f.dir();
                let dir_chars = dir.chars().count();
                // +1 for the '/' separator when there is a dir.
                let base_off = if dir.is_empty() { 0 } else { dir_chars + 1 };
                let indices: Vec<usize> = m
                    .indices
                    .iter()
                    .filter(|&&ix| ix >= base_off)
                    .map(|&ix| ix - base_off)
                    .collect();
                scored.push((
                    m.score,
                    i,
                    Row {
                        icon_kind: file_icon_kind(base),
                        name: base.to_string(),
                        dir: dir.to_string(),
                        indices,
                        target: i as i32,
                    },
                ));
            }
        }
        // Higher score first; ties broken by index order (stable, deterministic).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(200);
        self.rows = scored.into_iter().map(|(_, _, r)| r).collect();
        self.clamp_sel();
    }

    /// MRU rows (newest first), resolving each path's basename/dir against the
    /// index root for a clean relative display.
    fn mru_rows(&self) -> Vec<Row> {
        let root = self.index_root.as_deref();
        self.mru
            .entries()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let rel = match root {
                    Some(r) => rel_display(r, p),
                    None => p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                };
                let base_start = rel.rfind('/').map(|x| x + 1).unwrap_or(0);
                let base = rel[base_start..].to_string();
                let dir = if base_start > 0 { rel[..base_start - 1].to_string() } else { String::new() };
                Row {
                    icon_kind: Row::ICON_RECENT,
                    name: base,
                    dir,
                    indices: Vec::new(),
                    // Negative target encodes "MRU index i" (vs. a file-index target).
                    target: -(i as i32) - 1,
                }
            })
            .collect()
    }

    /// A single synthetic row for go-to-line mode previewing the target.
    fn rebuild_goto(&mut self) {
        let n = self.goto_line();
        let (name, dir) = if n >= 1 {
            (format!("Go to line {n}"), "Enter to jump".to_string())
        } else {
            ("Go to line\u{2026}".to_string(), "type a line number".to_string())
        };
        self.rows = vec![Row {
            icon_kind: Row::ICON_LINE,
            name,
            dir,
            indices: Vec::new(),
            target: n,
        }];
        self.sel = 0;
    }

    /// Replace the rows with caller-built symbol rows (Symbols mode). `provider`
    /// yields `(name, sym_kind_scalar, sym_index)` for every symbol; we fuzzy
    /// filter by the query sans the `@` prefix.
    pub fn set_symbol_rows(&mut self, syms: &[(String, i32, i32)]) {
        let q = Mode::strip(Mode::Symbols, &self.query);
        let mut scored: Vec<(i32, usize, Row)> = Vec::new();
        for (i, (name, kind, sym_idx)) in syms.iter().enumerate() {
            if let Some(m) = fuzzy_match(name, q) {
                scored.push((
                    m.score,
                    i,
                    Row {
                        icon_kind: *kind,
                        name: name.clone(),
                        dir: String::new(),
                        indices: m.indices,
                        target: *sym_idx,
                    },
                ));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.rows = scored.into_iter().map(|(_, _, r)| r).collect();
        self.clamp_sel();
    }

    /// Replace the rows with caller-built command rows (Commands mode). The
    /// `cmds` are `(label, command_id)` pairs already filtered/ranked by the
    /// caller (it reuses the palette's `filter_commands`); we just map them to
    /// rows, fuzzy-highlighting the label against the query sans the `>` prefix.
    pub fn set_command_rows(&mut self, cmds: &[(String, i32)]) {
        let q = Mode::strip(Mode::Commands, &self.query);
        self.rows = cmds
            .iter()
            .map(|(label, id)| {
                let indices = fuzzy_match(label, q).map(|m| m.indices).unwrap_or_default();
                Row {
                    icon_kind: Row::ICON_LINE,
                    name: label.clone(),
                    dir: String::new(),
                    indices,
                    target: *id,
                }
            })
            .collect();
        self.clamp_sel();
    }

    /// Resolve the selected (or row `i`, `-1` = current) file target to a path:
    /// either a workspace-index file or an MRU recent. `None` for non-file rows.
    pub fn accept_file_path(&self, i: i32) -> Option<PathBuf> {
        let idx = if i < 0 { self.sel } else { i as usize };
        let row = self.rows.get(idx)?;
        if row.target < 0 {
            // MRU row: -(i)-1 -> i.
            let mru_i = (-row.target - 1) as usize;
            self.mru.entries().get(mru_i).cloned()
        } else {
            self.index.get(row.target as usize).map(|f| f.path.clone())
        }
    }

    /// The symbol index of the selected (or row `i`) Symbols row, or `-1`.
    pub fn accept_symbol(&self, i: i32) -> i32 {
        let idx = if i < 0 { self.sel } else { i as usize };
        self.rows.get(idx).map(|r| r.target).unwrap_or(-1)
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.rows.len();
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

    fn clamp_sel(&mut self) {
        if self.sel >= self.rows.len() {
            self.sel = self.rows.len().saturating_sub(1);
        }
    }

    /// First visible row index so the selected item stays within the window.
    fn scroll_top(&self) -> usize {
        if self.rows.len() <= VISIBLE {
            return 0;
        }
        if self.sel < VISIBLE {
            0
        } else {
            (self.sel + 1).saturating_sub(VISIBLE)
        }
    }

    // -----------------------------------------------------------------------
    // Draw — mirrors the command palette's Vivid-Modern card.
    // -----------------------------------------------------------------------

    /// Draw the Quick-Open overlay: a dim scrim, a rounded indigo-glow card with
    /// a search field, a mode hint, and the result rows (icon + bold name with
    /// matched chars highlighted in the accent + dim secondary text), with the
    /// selected row indigo-tinted. No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.active {
            return;
        }
        use crate::icons;
        let w = width as f32;
        let h = height as f32;
        let chrome = theme::CHROME_FONT_SIZE;
        let clip = ctx.clip;

        let top = self.scroll_top();
        let shown = self.rows.len().saturating_sub(top).min(VISIBLE).clamp(1, 8);

        let box_w = 620.0_f32.min(w - 80.0);
        let search_h = 56.0;
        let cat_h = 25.0;
        let row_h = 46.0;
        let foot_h = 37.0;
        let box_h = search_h + cat_h + shown as f32 * row_h + 10.0 + foot_h;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = 96.0_f32.min((h - box_h).max(0.0));
        let radius = 12.0_f32;

        // Scrim + glow + card.
        ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, 0.55));
        ctx.dl_grad_v(0.0, 0.0, w, h * 0.5, 0.0, theme::accent_a(0.05), theme::accent_a(0.0));
        ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.85), 40.0);
        ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW(), 40.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        // ---- search field ----
        ctx.dl_rect(box_x + 1.0, box_y + search_h - 1.0, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_icon(box_x + 18.0, box_y + (search_h - 20.0) * 0.5, 20.0, 20.0, icons::SEARCH, theme::DIM(), 1.7, false);
        let q_text_x = box_x + 50.0;
        let qy = box_y + (search_h - 16.0) * 0.5 - 1.0;
        let placeholder = "Search files by name\u{2026}  (\u{203A} commands  @ symbols  : line)";
        let (q_str, q_color): (&str, _) = if self.query.is_empty() {
            (placeholder, theme::TEXT_3())
        } else {
            (self.query.as_str(), theme::TEXT())
        };
        ctx.text.queue_ui_sized(q_text_x, qy, q_str, q_color, 16.0, clip);
        let qadv = 16.0 * 0.52;
        let caret_x = q_text_x + self.query.chars().count() as f32 * qadv + 1.0;
        ctx.dl_round(caret_x, box_y + (search_h - 18.0) * 0.5, 2.0, 18.0, 1.0, theme::ACCENT_BRIGHT());
        // Mode pill (right): the current mode label.
        let mode_txt = match self.mode() {
            Mode::Files => "FILES",
            Mode::Commands => "CMDS",
            Mode::Symbols => "SYMS",
            Mode::GotoLine => "LINE",
        };
        let pill_w = (mode_txt.chars().count() as f32 * 6.2 + 16.0).max(44.0);
        let pill_x = box_x + box_w - pill_w - 18.0;
        let pill_y = box_y + (search_h - 22.0) * 0.5;
        ctx.dl_round(pill_x, pill_y, pill_w, 22.0, 5.0, theme::ACCENT_FAINT());
        ctx.dl_stroke(pill_x, pill_y, pill_w, 22.0, 5.0, theme::ACCENT_LINE(), 1.0);
        let mode_lbl_w = mode_txt.chars().count() as f32 * 6.2;
        ctx.text.queue_ui_sized(pill_x + (pill_w - mode_lbl_w) * 0.5, pill_y + 5.5, mode_txt, theme::ACCENT_BRIGHT(), 10.5, clip);

        // ---- category label ----
        let cat_y = box_y + search_h + 9.0;
        let cat_str = match self.mode() {
            Mode::Files => {
                if Mode::strip(Mode::Files, &self.query).is_empty() {
                    "RECENTLY OPENED"
                } else {
                    "WORKSPACE FILES"
                }
            }
            Mode::Commands => "COMMANDS",
            Mode::Symbols => "SYMBOLS",
            Mode::GotoLine => "GO TO LINE",
        };
        let cat: String = cat_str.chars().flat_map(|c| [c, '\u{2009}']).collect();
        ctx.text.queue_ui_sized(box_x + 18.0, cat_y, &cat, theme::TEXT_3(), chrome - 2.5, clip);
        let cnt = self.rows.len().to_string();
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - cnt.chars().count() as f32 * 6.0, cat_y, &cnt, theme::TEXT_3(), chrome - 2.5, clip);

        // ---- rows ----
        let list_top = box_y + search_h + cat_h;
        let name_adv = 13.5 * 0.55;
        for vis in 0..shown {
            let idx = top + vis;
            let Some(row) = self.rows.get(idx) else { break };
            let ry = list_top + vis as f32 * row_h;
            let selected = idx == self.sel;
            if selected {
                ctx.dl_grad_h(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::accent_a(0.22), 0.9);
                ctx.dl_stroke(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_LINE(), 1.0);
                ctx.dl_shadow(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_GLOW(), 16.0);
            }
            // Leading icon tile.
            let tile = 28.0;
            let tile_x = box_x + 18.0;
            let tile_y = ry + (row_h - tile) * 0.5;
            if selected {
                ctx.dl_round(tile_x, tile_y, tile, tile, 7.0, theme::accent_a(0.10));
                ctx.dl_stroke(tile_x, tile_y, tile, tile, 7.0, theme::ACCENT_LINE(), 1.0);
            } else {
                ctx.dl_round(tile_x, tile_y, tile, tile, 7.0, theme::BG_2());
                ctx.dl_stroke(tile_x, tile_y, tile, tile, 7.0, theme::BORDER(), 1.0);
            }
            let (icon, icon_col) = row_icon(row.icon_kind);
            ctx.dl_icon(tile_x + 6.0, tile_y + 6.0, 16.0, 16.0, icon, icon_col, 1.6, false);

            // Bold name with matched chars highlighted in the accent.
            let txt_x = box_x + 58.0;
            let name_y = ry + (row_h - 13.5) * 0.5 - if row.dir.is_empty() { 0.0 } else { 8.0 };
            self.draw_highlighted(ctx, txt_x, name_y, &row.name, &row.indices, name_adv, selected, clip);

            // Dim secondary (dir / kind) under the name.
            if !row.dir.is_empty() {
                ctx.text.queue_ui_sized(txt_x, ry + (row_h - 13.5) * 0.5 + 9.0, &row.dir, theme::TEXT_3(), 11.0, clip);
            }
        }

        // ---- footer hint line ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_round(box_x + 1.0, foot_y, box_w - 2.0, foot_h - 1.0, 0.0, theme::BG_2());
        let fty = foot_y + (foot_h - chrome + 1.0) * 0.5 - 1.0;
        let mut fx = box_x + 18.0;
        let foot_seg = |ctx: &mut crate::MuiContext, key: &str, label: &str, fx: &mut f32| {
            let kw = (key.chars().count() as f32 * 6.0 + 10.0).max(20.0);
            ctx.dl_round(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BG_1());
            ctx.dl_stroke(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BORDER_STRONG(), 1.0);
            ctx.text.queue_ui_sized(*fx + 5.0, foot_y + (foot_h - 10.0) * 0.5, key, theme::TEXT_1(), 10.0, clip);
            *fx += kw + 6.0;
            ctx.text.queue_ui_sized(*fx, fty, label, theme::TEXT_3(), 11.0, clip);
            *fx += label.chars().count() as f32 * 6.0 + 16.0;
        };
        foot_seg(ctx, "\u{2191}\u{2193}", "navigate", &mut fx);
        foot_seg(ctx, "\u{21B5}", "open", &mut fx);
        foot_seg(ctx, "esc", "dismiss", &mut fx);
        let tag = "Quick Open";
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - tag.chars().count() as f32 * 6.3, fty, tag, theme::ACCENT_BRIGHT(), 11.0, clip);
    }

    /// Draw `name` at (`x`,`y`), drawing the chars at `indices` in the accent
    /// color (highlighted) and the rest in the normal text color. Monospaced
    /// advance `adv` keeps the two passes aligned.
    #[allow(clippy::too_many_arguments)]
    fn draw_highlighted(
        &self,
        ctx: &mut crate::MuiContext,
        x: f32,
        y: f32,
        name: &str,
        indices: &[usize],
        adv: f32,
        selected: bool,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let base_col = if selected { theme::TEXT() } else { theme::TEXT_1() };
        let hi_col = theme::ACCENT_BRIGHT();
        let mut cx = x;
        for (i, ch) in name.chars().enumerate() {
            let col = if indices.contains(&i) { hi_col } else { base_col };
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            ctx.text.queue_ui_sized(cx, y, s, col, 13.5, clip);
            cx += adv;
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing() {
        assert_eq!(Mode::of(""), Mode::Files);
        assert_eq!(Mode::of("main"), Mode::Files);
        assert_eq!(Mode::of(">save"), Mode::Commands);
        assert_eq!(Mode::of("@fn"), Mode::Symbols);
        assert_eq!(Mode::of(":42"), Mode::GotoLine);
        assert_eq!(Mode::strip(Mode::Commands, ">save"), "save");
        assert_eq!(Mode::strip(Mode::Symbols, "@foo"), "foo");
        assert_eq!(Mode::strip(Mode::GotoLine, ":42"), "42");
        assert_eq!(Mode::strip(Mode::Files, "main"), "main");
    }

    #[test]
    fn mode_scalars_match_abi() {
        assert_eq!(Mode::Files.scalar(), 0);
        assert_eq!(Mode::Commands.scalar(), 1);
        assert_eq!(Mode::Symbols.scalar(), 2);
        assert_eq!(Mode::GotoLine.scalar(), 3);
    }

    #[test]
    fn fuzzy_empty_query_matches_all() {
        let m = fuzzy_match("anything", "").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.indices.is_empty());
    }

    #[test]
    fn fuzzy_no_match_returns_none() {
        assert!(fuzzy_match("abc", "xyz").is_none());
        assert!(fuzzy_match("main", "mn x").is_none());
    }

    #[test]
    fn fuzzy_returns_matched_indices() {
        // "mn" matches m(0) and n(3) in "m-a-i-n".
        let m = fuzzy_match("main", "mn").unwrap();
        assert_eq!(m.indices, vec![0, 3]);
    }

    #[test]
    fn fuzzy_is_case_insensitive() {
        let lo = fuzzy_match("MainWindow", "mw").unwrap();
        let hi = fuzzy_match("MainWindow", "MW").unwrap();
        assert_eq!(lo.indices, hi.indices);
        // M(0) and W(4) — both on boundaries (start + camelCase hump).
        assert_eq!(lo.indices, vec![0, 4]);
    }

    #[test]
    fn fuzzy_contiguous_beats_scattered() {
        // "main" exact-contiguous should outscore the scattered subsequence in
        // a longer string.
        let contig = fuzzy_match("main.rs", "main").unwrap();
        let scattered = fuzzy_match("m_a_i_n.rs", "main").unwrap();
        assert!(
            contig.score > scattered.score,
            "contiguous {} should beat scattered {}",
            contig.score,
            scattered.score
        );
    }

    #[test]
    fn fuzzy_boundary_bonus() {
        // The boundary match (after '/') should outscore a mid-word match.
        let boundary = fuzzy_match("src/main.rs", "m").unwrap();
        let midword = fuzzy_match("xmlfile.rs", "m").unwrap();
        assert!(boundary.score >= midword.score);
    }

    #[test]
    fn score_path_prefers_basename() {
        // Query "main": "src/main.rs" (basename match) should beat
        // "main/zzz.rs" (dir match only).
        let base_hit = score_path("src/main.rs", "main").unwrap();
        let dir_hit = score_path("main/zzz.rs", "main").unwrap();
        assert!(
            base_hit.score > dir_hit.score,
            "basename hit {} should beat dir-only hit {}",
            base_hit.score,
            dir_hit.score
        );
    }

    #[test]
    fn score_path_indices_in_full_path_space() {
        // "main" in "src/main.rs": indices point at the basename chars, offset
        // by the "src/" prefix (4 chars).
        let m = score_path("src/main.rs", "main").unwrap();
        assert_eq!(m.indices, vec![4, 5, 6, 7]);
    }

    #[test]
    fn mru_orders_newest_first_and_dedups() {
        let mut mru = Mru::new();
        mru.record(PathBuf::from("/a"));
        mru.record(PathBuf::from("/b"));
        mru.record(PathBuf::from("/c"));
        assert_eq!(
            mru.entries(),
            &[PathBuf::from("/c"), PathBuf::from("/b"), PathBuf::from("/a")]
        );
        // Re-opening /a moves it to front, no dup.
        mru.record(PathBuf::from("/a"));
        assert_eq!(
            mru.entries(),
            &[PathBuf::from("/a"), PathBuf::from("/c"), PathBuf::from("/b")]
        );
        assert_eq!(mru.len(), 3);
    }

    #[test]
    fn mru_caps_at_20() {
        let mut mru = Mru::new();
        for i in 0..30 {
            mru.record(PathBuf::from(format!("/f{i}")));
        }
        assert_eq!(mru.len(), MRU_CAP);
        // Newest is /f29.
        assert_eq!(mru.entries()[0], PathBuf::from("/f29"));
    }

    #[test]
    fn walk_finds_files_and_skips_ignored_dirs() {
        let root = std::env::temp_dir().join(format!("mui_qo_walk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("README.md"), b"# hi").unwrap();
        std::fs::write(root.join("src/main.mty"), b"fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.mty"), b"fn lib() {}").unwrap();
        std::fs::write(root.join("logo.png"), b"\x89PNG").unwrap(); // binary, skipped
        std::fs::write(root.join(".git/config"), b"x").unwrap(); // in .git, skipped
        std::fs::write(root.join("target/debug/x.o"), b"x").unwrap(); // in target, skipped
        std::fs::write(root.join("node_modules/pkg/index.js"), b"x").unwrap(); // skipped

        let files = walk_workspace(&root);
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert!(rels.contains(&"README.md"), "got {rels:?}");
        assert!(rels.contains(&"src/main.mty"), "got {rels:?}");
        assert!(rels.contains(&"src/lib.mty"), "got {rels:?}");
        assert!(!rels.iter().any(|r| r.contains("logo.png")), "binary skipped: {rels:?}");
        assert!(!rels.iter().any(|r| r.contains(".git")), ".git skipped: {rels:?}");
        assert!(!rels.iter().any(|r| r.contains("target")), "target skipped: {rels:?}");
        assert!(!rels.iter().any(|r| r.contains("node_modules")), "node_modules skipped: {rels:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn indexed_file_basename_and_dir() {
        let f = IndexedFile { path: PathBuf::from("/x/src/main.mty"), rel: "src/main.mty".to_string() };
        assert_eq!(f.basename(), "main.mty");
        assert_eq!(f.dir(), "src");
        let g = IndexedFile { path: PathBuf::from("/x/top.mty"), rel: "top.mty".to_string() };
        assert_eq!(g.basename(), "top.mty");
        assert_eq!(g.dir(), "");
    }

    #[test]
    fn goto_line_parsing() {
        let mut qo = QuickOpen::new();
        qo.open();
        for ch in ":42".chars() {
            qo.push_char(ch);
        }
        assert_eq!(qo.mode(), Mode::GotoLine);
        assert_eq!(qo.goto_line(), 42);
        // One row previewing the jump.
        assert_eq!(qo.count(), 1);
        // Non-numeric -> -1.
        qo.cancel();
        qo.open();
        for ch in ":abc".chars() {
            qo.push_char(ch);
        }
        assert_eq!(qo.goto_line(), -1);
    }

    #[test]
    fn empty_query_shows_mru_recents() {
        let mut qo = QuickOpen::new();
        qo.record_mru(PathBuf::from("/ws/a.mty"));
        qo.record_mru(PathBuf::from("/ws/b.mty"));
        qo.open();
        // Empty query in Files mode -> MRU rows (newest first).
        assert_eq!(qo.mode(), Mode::Files);
        assert_eq!(qo.count(), 2);
        assert_eq!(qo.row(0).unwrap().name, "b.mty");
        assert_eq!(qo.row(1).unwrap().name, "a.mty");
        // Recents use a negative target encoding.
        assert!(qo.row(0).unwrap().target < 0);
        // accept resolves back to the path.
        assert_eq!(qo.accept_file_path(-1), Some(PathBuf::from("/ws/b.mty")));
    }

    #[test]
    fn file_query_filters_and_highlights() {
        let root = std::env::temp_dir().join(format!("mui_qo_filter_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.mty"), b"x").unwrap();
        std::fs::write(root.join("src/parser.mty"), b"x").unwrap();
        std::fs::write(root.join("notes.txt"), b"x").unwrap();

        let mut qo = QuickOpen::new();
        qo.ensure_index(&root, true);
        qo.open();
        for ch in "main".chars() {
            qo.push_char(ch);
        }
        assert!(qo.count() >= 1);
        // Top hit is main.mty with all 4 chars highlighted (contiguous basename).
        let top = qo.row(0).unwrap();
        assert_eq!(top.name, "main.mty");
        assert_eq!(top.indices, vec![0, 1, 2, 3]);
        // Accept resolves to the real path.
        let p = qo.accept_file_path(-1).unwrap();
        assert!(p.ends_with("main.mty"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symbol_mode_filters_by_name() {
        let mut qo = QuickOpen::new();
        qo.open();
        qo.push_char('@');
        let syms = vec![
            ("main".to_string(), SymKind::Function as i32, 0),
            ("Parser".to_string(), SymKind::Struct as i32, 1),
            ("parse".to_string(), SymKind::Function as i32, 2),
        ];
        qo.set_symbol_rows(&syms);
        assert_eq!(qo.count(), 3); // empty filter -> all
        // Now filter to "par".
        qo.push_char('p');
        qo.set_symbol_rows(&syms);
        qo.push_char('a');
        qo.set_symbol_rows(&syms);
        qo.push_char('r');
        qo.set_symbol_rows(&syms);
        // "Parser" and "parse" match "par"; "main" does not.
        let names: Vec<&str> = qo.rows().iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"Parser"));
        assert!(names.contains(&"parse"));
        assert!(!names.contains(&"main"));
        assert_eq!(qo.accept_symbol(-1), qo.row(0).unwrap().target);
    }

    #[test]
    fn move_sel_wraps() {
        let mut qo = QuickOpen::new();
        qo.record_mru(PathBuf::from("/a"));
        qo.record_mru(PathBuf::from("/b"));
        qo.open();
        let n = qo.count();
        assert_eq!(n, 2);
        qo.move_sel(-1);
        assert_eq!(qo.selection(), n - 1);
        qo.move_sel(1);
        assert_eq!(qo.selection(), 0);
    }

    #[test]
    fn backspace_past_prefix_returns_to_files() {
        let mut qo = QuickOpen::new();
        qo.open();
        qo.push_char('@');
        assert_eq!(qo.mode(), Mode::Symbols);
        let removed = qo.backspace();
        assert!(removed);
        assert_eq!(qo.mode(), Mode::Files);
        assert_eq!(qo.query(), "");
    }

    #[test]
    fn ensure_index_caches_until_root_changes() {
        let root = std::env::temp_dir().join(format!("mui_qo_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.mty"), b"x").unwrap();
        let mut qo = QuickOpen::new();
        let n1 = qo.ensure_index(&root, false);
        assert_eq!(n1, 1);
        // Add a file; without force + same root, the cache is reused.
        std::fs::write(root.join("b.mty"), b"x").unwrap();
        let n2 = qo.ensure_index(&root, false);
        assert_eq!(n2, 1, "cached index reused");
        // Force rebuild picks up the new file.
        let n3 = qo.ensure_index(&root, true);
        assert_eq!(n3, 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
