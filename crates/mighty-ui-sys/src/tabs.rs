//! Multi-file tab store (pure, unit-testable).
//!
//! The Mighty side keeps exactly ONE live edit buffer (`Vec[I32]` of byte
//! values). The shim owns the *other* tabs' contents + per-tab cursor/scroll
//! state here. Tab switching is a byte-swap: Mighty serializes its current
//! buffer into the active slot (`store_*`), then pulls the target slot's bytes
//! back (`load`) and restores its cursor/scroll.
//!
//! v0.36 Mighty can't pass strings/buffers across FFI (L17), so paths and bytes
//! live shim-side; Mighty drives everything through scalar getters/setters.

use std::path::{Path, PathBuf};

use crate::editor::TextModel;
use crate::fold::FoldState;

const CLOSED_CAP: usize = 20;
const BINARY_SCAN_LIMIT: usize = 8192;
pub(crate) const MAX_OPEN_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Snapshot a model's lines into owned strings (for the fold scanner, which is
/// pure over `&[String]`). The model stores newlines as line boundaries, so this
/// is one `String` per buffer line.
fn model_lines(model: &TextModel) -> Vec<String> {
    (0..model.line_count())
        .map(|i| model.line(i).to_string())
        .collect()
}

/// Conservative binary-file detection for the text editor path. NUL bytes are
/// the strongest signal; invalid UTF-8 covers most image/font/archive payloads
/// while still allowing normal UTF-8 source files through.
pub fn is_probably_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let sample_len = bytes.len().min(BINARY_SCAN_LIMIT);
    let sample = &bytes[..sample_len];
    sample.iter().any(|b| *b == 0) || std::str::from_utf8(sample).is_err()
}

fn binary_placeholder(path: Option<&Path>, bytes_len: usize) -> String {
    let name = path
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "This file".to_string());
    format!(
        "Binary file preview\n\n{name} appears to be a binary file ({bytes_len} bytes).\nMighty IDE opened a read-only text preview instead of corrupting the editor buffer.\nUse an external asset or binary editor to modify this file.\n"
    )
}

fn oversized_placeholder(path: Option<&Path>, bytes_len: u64) -> String {
    let name = path
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "This file".to_string());
    format!(
        "Large file preview\n\n{name} is {bytes_len} bytes, which exceeds Mighty IDE's {MAX_OPEN_FILE_BYTES} byte text-editing limit.\nMighty IDE opened a read-only preview instead of loading the full file into the editor buffer.\nUse an external large-file viewer or split the file before editing.\n"
    )
}

fn model_for_bytes(path: Option<&Path>, bytes: &[u8]) -> (TextModel, FoldState, bool) {
    let read_only = is_probably_binary(bytes);
    let model_bytes;
    let model = if read_only {
        model_bytes = binary_placeholder(path, bytes.len()).into_bytes();
        TextModel::from_bytes(&model_bytes)
    } else {
        TextModel::from_bytes(bytes)
    };
    let mut fold = FoldState::new();
    fold.recompute_owned(&model_lines(&model));
    (model, fold, read_only)
}

fn model_for_oversized_file(
    path: Option<&Path>,
    bytes_len: u64,
) -> (TextModel, FoldState, Vec<u8>) {
    let model_bytes = oversized_placeholder(path, bytes_len).into_bytes();
    let model = TextModel::from_bytes(&model_bytes);
    let mut fold = FoldState::new();
    fold.recompute_owned(&model_lines(&model));
    (model, fold, model_bytes)
}

fn read_open_file_bytes(path: &Path) -> Result<Vec<u8>, u64> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() && meta.len() > MAX_OPEN_FILE_BYTES {
            return Err(meta.len());
        }
    }
    Ok(std::fs::read(path).unwrap_or_default())
}

fn tab_paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
        if ca == cb {
            return true;
        }
        #[cfg(windows)]
        {
            return normalize_windows_path(&ca) == normalize_windows_path(&cb);
        }
    }
    #[cfg(windows)]
    {
        return normalize_windows_path(a) == normalize_windows_path(b);
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn normalize_windows_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// One open file tab. Since the L28 codegen bug forced the editable buffer
/// shim-side, each tab now owns an authoritative [`TextModel`] (lines, cursor,
/// scroll, dirty). The legacy `bytes`/cursor/scroll fields are retained only for
/// the byte-swap ABI still referenced by older tests; the model is the source of
/// truth for the active tab's editing.
#[derive(Debug, Clone, Default)]
pub struct Tab {
    /// Absolute or relative path of the file (None for an unsaved scratch tab).
    pub path: Option<PathBuf>,
    /// File content as raw bytes (legacy byte-swap path; kept in sync on store).
    pub bytes: Vec<u8>,
    /// The authoritative editable text model for this tab.
    pub model: TextModel,
    /// Per-tab code-folding state (foldable ranges + folded headers). Recomputed
    /// from the model on edit / load; folded headers preserved where they survive.
    pub fold: FoldState,
    /// 0-based cursor line saved when this tab was last active (legacy).
    pub cursor_line: i32,
    /// 0-based cursor column saved when this tab was last active (legacy).
    pub cursor_col: i32,
    /// Top visible line (scroll offset) saved when this tab was last active.
    pub scroll_first: i32,
    /// True if the buffer has unsaved edits relative to disk.
    pub dirty: bool,
    /// True when the tab represents non-text bytes and must never be saved from
    /// the text editor model.
    pub read_only: bool,
    /// Per-tab undo snapshots for the authoritative text model.
    pub undo: Vec<TextModel>,
    /// Per-tab redo snapshots for the authoritative text model.
    pub redo: Vec<TextModel>,
}

impl Tab {
    /// Basename (file-name component) for the tab bar, or `(scratch)`.
    pub fn basename(&self) -> String {
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            None => "(scratch)".to_string(),
        }
    }

    /// True when either the tab chrome flag or the authoritative model says the
    /// buffer has unsaved edits.
    pub fn is_dirty(&self) -> bool {
        if self.read_only {
            return false;
        }
        self.dirty || self.model.dirty()
    }
}

/// The ordered set of open tabs plus the active index. Always holds at least one
/// tab (closing the last tab leaves an empty scratch tab).
#[derive(Debug, Default)]
pub struct TabStore {
    tabs: Vec<Tab>,
    active: usize,
    closed: Vec<Tab>,
}

/// Result of a bulk tab close that compacts the tab list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabCompaction {
    pub removed: usize,
    pub removed_empty_scratch: usize,
    /// Old-index -> new-index for kept tabs; `None` for tabs that were closed.
    pub old_to_new: Vec<Option<usize>>,
}

impl TabStore {
    pub fn new() -> Self {
        TabStore {
            tabs: Vec::new(),
            active: 0,
            closed: Vec::new(),
        }
    }

    pub fn count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn get(&self, i: usize) -> Option<&Tab> {
        self.tabs.get(i)
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(i)
    }

    /// Find an already-open tab whose path matches `path`. Existing files are
    /// compared by canonical path; Windows also falls back to loose slash/case
    /// matching so alternate spellings do not create duplicate tabs.
    pub fn find_by_path(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.path.as_deref().is_some_and(|p| tab_paths_equal(p, path)))
    }

    /// Open `path` as a new tab (reading its bytes from disk), or switch to the
    /// existing tab if already open. Returns the tab index. If the file can't be
    /// read it is opened empty (so a brand-new file path still gets a tab).
    pub fn open_path(&mut self, path: PathBuf) -> usize {
        if let Some(i) = self.find_by_path(&path) {
            self.active = i;
            return i;
        }
        let (model, fold, read_only, saved_bytes) = match read_open_file_bytes(&path) {
            Ok(bytes) => {
                let (model, fold, read_only) = model_for_bytes(Some(&path), &bytes);
                let saved_bytes = if read_only { bytes } else { model.to_bytes() };
                (model, fold, read_only, saved_bytes)
            }
            Err(bytes_len) => {
                let (model, fold, saved_bytes) = model_for_oversized_file(Some(&path), bytes_len);
                (model, fold, true, saved_bytes)
            }
        };
        self.tabs.push(Tab {
            path: Some(path),
            bytes: saved_bytes,
            model,
            fold,
            cursor_line: 0,
            cursor_col: 0,
            scroll_first: 0,
            dirty: false,
            read_only,
            undo: Vec::new(),
            redo: Vec::new(),
        });
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Open a fresh, empty, untitled tab and make it active (the New File action).
    /// Returns the new tab's index.
    pub fn new_untitled(&mut self) -> usize {
        self.tabs.push(Tab::default());
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Duplicate the active tab next to itself and make the duplicate active.
    /// This intentionally clones the live model, cursor, fold, dirty, and path
    /// state instead of re-reading from disk.
    pub fn duplicate_active(&mut self) -> usize {
        self.ensure_scratch();
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let mut tab = self.tabs[active].clone();
        tab.undo.clear();
        tab.redo.clear();
        let insert_at = (active + 1).min(self.tabs.len());
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        self.active
    }

    /// Move the active tab one slot to the left. Returns the new active index,
    /// or `None` when the active tab is already first / no move is possible.
    pub fn move_active_left(&mut self) -> Option<usize> {
        if self.tabs.len() <= 1 {
            self.ensure_scratch();
            return None;
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        if active == 0 {
            self.active = active;
            return None;
        }
        self.tabs.swap(active - 1, active);
        self.active = active - 1;
        Some(self.active)
    }

    /// `true` when the active tab cannot move farther left.
    pub fn active_tab_is_first(&self) -> bool {
        self.tabs.len() <= 1 || self.active.min(self.tabs.len().saturating_sub(1)) == 0
    }

    /// Move the active tab one slot to the right. Returns the new active index,
    /// or `None` when the active tab is already last / no move is possible.
    pub fn move_active_right(&mut self) -> Option<usize> {
        if self.tabs.len() <= 1 {
            self.ensure_scratch();
            return None;
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        if active + 1 >= self.tabs.len() {
            self.active = active;
            return None;
        }
        self.tabs.swap(active, active + 1);
        self.active = active + 1;
        Some(self.active)
    }

    /// `true` when the active tab cannot move farther right.
    pub fn active_tab_is_last(&self) -> bool {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs.len() <= 1 || active + 1 >= self.tabs.len()
    }

    /// Sort open tabs by display name, preserving the active logical document.
    /// Returns an old-index -> new-index remap when order changed.
    pub fn sort_by_name(&mut self) -> Option<Vec<usize>> {
        if self.tabs.len() <= 1 {
            self.ensure_scratch();
            return None;
        }
        let old_active = self.active.min(self.tabs.len().saturating_sub(1));
        let len = self.tabs.len();
        let mut indexed: Vec<(usize, Tab)> = self.tabs.drain(..).enumerate().collect();
        indexed.sort_by(|(ia, a), (ib, b)| {
            a.basename()
                .to_ascii_lowercase()
                .cmp(&b.basename().to_ascii_lowercase())
                .then_with(|| ia.cmp(ib))
        });
        let mut old_to_new = vec![0; len];
        let mut changed = false;
        for (new_idx, (old_idx, _)) in indexed.iter().enumerate() {
            old_to_new[*old_idx] = new_idx;
            if *old_idx != new_idx {
                changed = true;
            }
        }
        self.tabs = indexed.into_iter().map(|(_, tab)| tab).collect();
        self.active = old_to_new[old_active];
        if changed {
            Some(old_to_new)
        } else {
            None
        }
    }

    /// `true` when sorting tabs by display name would leave order unchanged.
    pub fn tabs_already_sorted_by_name(&self) -> bool {
        if self.tabs.len() <= 1 {
            return true;
        }
        self.tabs.windows(2).all(|pair| {
            pair[0].basename().to_ascii_lowercase() <= pair[1].basename().to_ascii_lowercase()
        })
    }

    /// Close clean duplicate file-backed tabs, preserving dirty duplicates and
    /// preferring the active tab when it is one of the duplicates.
    pub fn close_duplicate_file_tabs(&mut self) -> Option<TabCompaction> {
        if self.tabs.len() <= 1 {
            self.ensure_scratch();
            return None;
        }
        let old_active = self.active.min(self.tabs.len().saturating_sub(1));
        let mut keep_for_path: Vec<(PathBuf, usize)> = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            let Some(path) = tab.path.clone() else {
                continue;
            };
            if tab.is_dirty() {
                continue;
            }
            if let Some((_, keep_idx)) = keep_for_path
                .iter_mut()
                .find(|(p, _)| tab_paths_equal(p, &path))
            {
                if idx == old_active {
                    *keep_idx = idx;
                }
            } else {
                keep_for_path.push((path, idx));
            }
        }

        let len = self.tabs.len();
        let mut old_to_new = vec![None; len];
        let mut kept: Vec<Tab> = Vec::new();
        let mut removed_tabs: Vec<Tab> = Vec::new();
        for (idx, tab) in self.tabs.drain(..).enumerate() {
            let keep = if tab.is_dirty() {
                true
            } else if let Some(path) = tab.path.as_ref() {
                keep_for_path
                    .iter()
                    .find(|(p, _)| tab_paths_equal(p, path))
                    .map(|(_, keep_idx)| *keep_idx == idx)
                    .unwrap_or(true)
            } else {
                true
            };
            if keep {
                old_to_new[idx] = Some(kept.len());
                kept.push(tab);
            } else {
                removed_tabs.push(tab);
            }
        }

        let removed = removed_tabs.len();
        if removed == 0 {
            self.tabs = kept;
            return None;
        }
        self.tabs = kept;
        self.active = old_to_new
            .get(old_active)
            .and_then(|v| *v)
            .unwrap_or_else(|| old_to_new.iter().flatten().copied().next().unwrap_or(0));
        self.ensure_scratch();
        let removed_empty_scratch = removed_tabs
            .iter()
            .filter(|tab| Self::is_empty_scratch(tab))
            .count();
        self.remember_closed_many(removed_tabs);
        Some(TabCompaction {
            removed,
            removed_empty_scratch,
            old_to_new,
        })
    }

    /// `true` when a clean file-backed duplicate tab can be closed.
    pub fn has_clean_duplicate_file_tabs(&self) -> bool {
        let mut paths: Vec<&Path> = Vec::new();
        for tab in &self.tabs {
            if tab.is_dirty() {
                continue;
            }
            let Some(path) = tab.path.as_deref() else {
                continue;
            };
            if paths.iter().any(|existing| tab_paths_equal(existing, path)) {
                return true;
            }
            paths.push(path);
        }
        false
    }

    /// True when any open tab for `path` has unsaved edits.
    pub fn any_dirty_path(&self, path: &Path) -> bool {
        self.tabs.iter().any(|tab| {
            tab.path
                .as_deref()
                .is_some_and(|p| tab_paths_equal(p, path) && tab.is_dirty())
        })
    }

    /// True when any open tab for `path`, except `except_idx`, has unsaved edits.
    pub fn any_dirty_path_except(&self, path: &Path, except_idx: usize) -> bool {
        self.tabs.iter().enumerate().any(|(idx, tab)| {
            idx != except_idx
                && tab
                    .path
                    .as_deref()
                    .is_some_and(|p| tab_paths_equal(p, path) && tab.is_dirty())
        })
    }

    /// Close all clean tabs pointing at `path` without adding them to
    /// reopen-closed history. Used after the backing file itself was deleted.
    /// Dirty matching tabs are preserved; callers should usually preflight with
    /// [`Self::any_dirty_path`].
    pub fn close_clean_path_forget(&mut self, path: &Path) -> Option<TabCompaction> {
        if self.tabs.is_empty() {
            self.ensure_scratch();
            return None;
        }
        let old_active = self.active.min(self.tabs.len().saturating_sub(1));
        let len = self.tabs.len();
        let mut old_to_new = vec![None; len];
        let mut kept: Vec<(usize, Tab)> = Vec::new();
        let mut removed = 0;
        for (idx, tab) in self.tabs.drain(..).enumerate() {
            let matches_path = tab
                .path
                .as_deref()
                .is_some_and(|p| tab_paths_equal(p, path));
            if matches_path && !tab.is_dirty() {
                removed += 1;
            } else {
                old_to_new[idx] = Some(kept.len());
                kept.push((idx, tab));
            }
        }

        if removed == 0 {
            self.tabs = kept.into_iter().map(|(_, tab)| tab).collect();
            return None;
        }
        if kept.is_empty() {
            self.tabs.push(Tab::default());
            self.active = 0;
        } else {
            let new_active = old_to_new
                .get(old_active)
                .and_then(|v| *v)
                .or_else(|| kept.iter().position(|(idx, _)| *idx > old_active))
                .unwrap_or_else(|| kept.len().saturating_sub(1));
            self.tabs = kept.into_iter().map(|(_, tab)| tab).collect();
            self.active = new_active;
        }
        Some(TabCompaction {
            removed,
            removed_empty_scratch: 0,
            old_to_new,
        })
    }

    /// Set the active tab's file path (Save As on an untitled buffer binds it to a
    /// real path so subsequent saves write there).
    pub fn set_active_path(&mut self, path: PathBuf) {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs[i].path = Some(path);
        self.tabs[i].read_only = false;
    }

    /// Rebind all open tabs that point at `old_path` to `new_path`. Used after a
    /// filesystem rename so duplicate views do not keep stale old paths.
    pub fn rebind_path(&mut self, old_path: &Path, new_path: PathBuf) -> usize {
        let mut changed = 0;
        for tab in &mut self.tabs {
            if tab
                .path
                .as_deref()
                .is_some_and(|p| tab_paths_equal(p, old_path))
            {
                tab.path = Some(new_path.clone());
                changed += 1;
            }
        }
        changed
    }

    /// `true` when the active tab is backed by a file path (vs an untitled buffer).
    pub fn active_has_path(&self) -> bool {
        self.tabs
            .get(self.active.min(self.tabs.len().saturating_sub(1)))
            .map(|t| t.path.is_some())
            .unwrap_or(false)
    }

    /// `true` when the active tab is a read-only binary preview.
    pub fn active_read_only(&self) -> bool {
        self.tabs
            .get(self.active.min(self.tabs.len().saturating_sub(1)))
            .map(|t| t.read_only)
            .unwrap_or(false)
    }

    /// Tab `i`'s editable model (shared ref), or `None` out of range. Used by
    /// the split-pane draw to render an UNFOCUSED pane's tab (the focused pane's
    /// tab is the active one, read via [`Self::active_model`]).
    pub fn model_at(&self, i: usize) -> Option<&TextModel> {
        self.tabs.get(i).map(|t| &t.model)
    }

    /// The active tab's authoritative editable model (shared ref).
    pub fn active_model(&self) -> &TextModel {
        // Always at least one tab exists.
        &self.tabs[self.active.min(self.tabs.len().saturating_sub(1))].model
    }

    /// The active tab's authoritative editable model (mutable).
    pub fn active_model_mut(&mut self) -> &mut TextModel {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        &mut self.tabs[i].model
    }

    /// The active tab's code-fold state (shared ref).
    pub fn active_fold(&self) -> &FoldState {
        &self.tabs[self.active.min(self.tabs.len().saturating_sub(1))].fold
    }

    /// The active tab's code-fold state (mutable).
    pub fn active_fold_mut(&mut self) -> &mut FoldState {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        &mut self.tabs[i].fold
    }

    /// Tab `i`'s fold state (shared ref), or `None` out of range (for the
    /// split-pane draw of an UNFOCUSED tab).
    pub fn fold_at(&self, i: usize) -> Option<&FoldState> {
        self.tabs.get(i).map(|t| &t.fold)
    }

    /// Recompute the active tab's foldable ranges from its current model lines
    /// (preserving folded headers that still open a region). Called after edits.
    pub fn recompute_active_fold(&mut self) {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        let lines = model_lines(&self.tabs[i].model);
        self.tabs[i].fold.recompute_owned(&lines);
    }

    /// Active tab's path, if any.
    pub fn active_path(&self) -> Option<PathBuf> {
        self.path(self.active)
    }

    /// Replace the active tab's model from raw bytes (load / reload from disk).
    pub fn reload_active(&mut self, bytes: &[u8]) {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        self.reload_index(i, bytes, false);
    }

    /// Replace the active tab with an oversized-file placeholder.
    pub fn reload_active_oversized(&mut self, bytes_len: u64) {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        self.reload_index_oversized(i, bytes_len, false);
    }

    /// Replace the active tab from disk while keeping its undo checkpoint stack.
    /// Used for formatter-style transformations where the reload is the edit.
    pub fn reload_active_preserving_history(&mut self, bytes: &[u8]) {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        self.reload_index(i, bytes, true);
    }

    /// Replace every clean open file-backed tab matching `path` from disk.
    /// Returns `(refreshed, dirty_skipped)`.
    pub fn reload_all_clean_path(&mut self, path: &Path, bytes: &[u8]) -> (usize, usize) {
        self.reload_all_clean_path_except(path, bytes, usize::MAX)
    }

    /// Replace every clean open file-backed tab matching `path` from disk,
    /// excluding `except_idx`. Returns `(refreshed, dirty_skipped)`.
    pub fn reload_all_clean_path_except(
        &mut self,
        path: &Path,
        bytes: &[u8],
        except_idx: usize,
    ) -> (usize, usize) {
        let mut refreshed = 0usize;
        let mut dirty_skipped = 0usize;
        let matching: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(idx, tab)| {
                (idx != except_idx)
                    .then(|| {
                        tab.path
                            .as_deref()
                            .is_some_and(|p| tab_paths_equal(p, path))
                            .then_some(idx)
                    })
                    .flatten()
            })
            .collect();
        for idx in matching {
            if self.is_dirty(idx) {
                dirty_skipped += 1;
            } else {
                self.reload_index(idx, bytes, false);
                refreshed += 1;
            }
        }
        (refreshed, dirty_skipped)
    }

    /// Replace every clean open file-backed tab matching `path` with an
    /// oversized-file placeholder, excluding `except_idx`.
    pub fn reload_all_clean_path_oversized_except(
        &mut self,
        path: &Path,
        bytes_len: u64,
        except_idx: usize,
    ) -> (usize, usize) {
        let mut refreshed = 0usize;
        let mut dirty_skipped = 0usize;
        let matching: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(idx, tab)| {
                (idx != except_idx)
                    .then(|| {
                        tab.path
                            .as_deref()
                            .is_some_and(|p| tab_paths_equal(p, path))
                            .then_some(idx)
                    })
                    .flatten()
            })
            .collect();
        for idx in matching {
            if self.is_dirty(idx) {
                dirty_skipped += 1;
            } else {
                self.reload_index_oversized(idx, bytes_len, false);
                refreshed += 1;
            }
        }
        (refreshed, dirty_skipped)
    }

    fn reload_index(&mut self, i: usize, bytes: &[u8], preserve_history: bool) {
        let (model, fold, read_only) = model_for_bytes(self.tabs[i].path.as_deref(), bytes);
        self.tabs[i].model = model;
        self.tabs[i].bytes = if read_only {
            bytes.to_vec()
        } else {
            self.tabs[i].model.to_bytes()
        };
        self.tabs[i].dirty = false;
        self.tabs[i].read_only = read_only;
        if !preserve_history {
            self.tabs[i].undo.clear();
        }
        self.tabs[i].redo.clear();
        // A fresh buffer: recompute folds and drop any stale folded state.
        self.tabs[i].fold = fold;
    }

    fn reload_index_oversized(&mut self, i: usize, bytes_len: u64, preserve_history: bool) {
        let (model, fold, bytes) =
            model_for_oversized_file(self.tabs[i].path.as_deref(), bytes_len);
        self.tabs[i].model = model;
        self.tabs[i].bytes = bytes;
        self.tabs[i].dirty = false;
        self.tabs[i].read_only = true;
        if !preserve_history {
            self.tabs[i].undo.clear();
        }
        self.tabs[i].redo.clear();
        self.tabs[i].fold = fold;
    }

    /// Ensure at least one tab exists. Used at startup if no file opened and on
    /// close-to-empty.
    pub fn ensure_scratch(&mut self) {
        if self.tabs.is_empty() {
            self.tabs.push(Tab::default());
            self.active = 0;
        }
    }

    /// Switch the active tab to `idx` (clamped/ignored if out of range). Returns
    /// the resulting active index.
    pub fn switch(&mut self, idx: usize) -> usize {
        if idx < self.tabs.len() {
            self.active = idx;
        }
        self.active
    }

    /// Next tab (wraps).
    pub fn next(&mut self) -> usize {
        if self.tabs.is_empty() {
            return 0;
        }
        self.active = (self.active + 1) % self.tabs.len();
        self.active
    }

    /// Previous tab (wraps).
    pub fn prev(&mut self) -> usize {
        if self.tabs.is_empty() {
            return 0;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        self.active
    }

    /// Close tab `idx`. Keeps at least one tab: closing the last remaining tab
    /// replaces it with an empty scratch tab. The active index is adjusted to
    /// stay in range and follow a sensible neighbor. Returns the new active idx.
    pub fn close(&mut self, idx: usize) -> usize {
        self.close_inner(idx, true)
    }

    /// Close tab `idx` without making it recoverable through reopen-closed.
    /// Kept for the targeted reopen-history regression test.
    #[cfg(test)]
    pub fn close_forget(&mut self, idx: usize) -> usize {
        self.close_inner(idx, false)
    }

    fn close_inner(&mut self, idx: usize, remember: bool) -> usize {
        if idx >= self.tabs.len() {
            return self.active;
        }
        let closed = self.tabs.remove(idx);
        if remember {
            self.remember_closed(closed);
        }
        if self.tabs.is_empty() {
            self.tabs.push(Tab::default());
            self.active = 0;
            return 0;
        }
        // Keep the active pointing at the same logical neighbor.
        if self.active > idx {
            self.active -= 1;
        } else if self.active == idx && self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.active
    }

    /// Reopen the most recently closed tab. Returns the new active index, or
    /// `None` if there is no recoverable closed tab.
    pub fn reopen_closed(&mut self) -> Option<usize> {
        while let Some(tab) = self.closed.pop() {
            if let Some(path) = tab.path.as_deref() {
                if let Some(existing) = self.find_by_path(path) {
                    self.active = existing;
                    return Some(existing);
                }
            }
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
            return Some(self.active);
        }
        None
    }

    /// Whether there is a tab currently recoverable through reopen-closed-tab.
    pub fn has_closed_tabs(&self) -> bool {
        !self.closed.is_empty()
    }

    /// Number of tabs currently recoverable through reopen-closed-tab.
    #[cfg(test)]
    pub fn closed_count(&self) -> usize {
        self.closed.len()
    }

    fn remember_closed(&mut self, tab: Tab) {
        if !Self::is_reopenable(&tab) {
            return;
        }
        if tab
            .path
            .as_deref()
            .is_some_and(|path| self.find_by_path(path).is_some())
        {
            return;
        }
        self.closed.push(tab);
        if self.closed.len() > CLOSED_CAP {
            let overflow = self.closed.len() - CLOSED_CAP;
            self.closed.drain(0..overflow);
        }
    }

    fn remember_closed_many(&mut self, tabs: Vec<Tab>) {
        for tab in tabs {
            self.remember_closed(tab);
        }
    }

    fn is_reopenable(tab: &Tab) -> bool {
        tab.path.is_some()
            || tab.is_dirty()
            || !tab.model.to_bytes().is_empty()
            || !tab.bytes.is_empty()
    }

    fn is_empty_scratch(tab: &Tab) -> bool {
        tab.path.is_none()
            && !tab.is_dirty()
            && tab.model.to_bytes().is_empty()
            && tab.bytes.is_empty()
    }

    /// Close every clean tab, preserving all dirty tabs. Returns compaction
    /// metadata when tabs were removed. If every tab is clean, leaves a single
    /// empty scratch tab.
    pub fn close_saved(&mut self) -> Option<TabCompaction> {
        if self.tabs.is_empty() {
            self.ensure_scratch();
            return None;
        }
        if self.tabs.len() == 1 && !self.tabs[0].is_dirty() && self.tabs[0].path.is_none() {
            return None;
        }
        let old_active = self.active.min(self.tabs.len().saturating_sub(1));
        let len = self.tabs.len();
        let mut old_to_new = vec![None; len];
        let mut kept: Vec<(usize, Tab)> = Vec::new();
        let mut removed_tabs: Vec<Tab> = Vec::new();
        for (idx, tab) in self.tabs.drain(..).enumerate() {
            if tab.is_dirty() {
                old_to_new[idx] = Some(kept.len());
                kept.push((idx, tab));
            } else {
                removed_tabs.push(tab);
            }
        }
        let removed = removed_tabs.len();
        if kept.is_empty() {
            self.tabs.push(Tab::default());
            self.active = 0;
            let removed_empty_scratch = removed_tabs
                .iter()
                .filter(|tab| Self::is_empty_scratch(tab))
                .count();
            self.remember_closed_many(removed_tabs);
            return Some(TabCompaction {
                removed,
                removed_empty_scratch,
                old_to_new,
            });
        }
        let new_active = kept
            .iter()
            .position(|(idx, _)| *idx == old_active)
            .or_else(|| kept.iter().position(|(idx, _)| *idx > old_active))
            .unwrap_or_else(|| kept.len().saturating_sub(1));
        self.tabs = kept.drain(..).map(|(_, tab)| tab).collect();
        self.active = new_active;
        let removed_empty_scratch = removed_tabs
            .iter()
            .filter(|tab| Self::is_empty_scratch(tab))
            .count();
        self.remember_closed_many(removed_tabs);
        Some(TabCompaction {
            removed,
            removed_empty_scratch,
            old_to_new,
        })
    }

    /// `true` when close-saved-tabs would remove at least one tab.
    pub fn has_saved_tabs_to_close(&self) -> bool {
        if self.tabs.len() == 1 && !self.tabs[0].is_dirty() && self.tabs[0].path.is_none() {
            return false;
        }
        self.tabs.iter().any(|tab| !tab.is_dirty())
    }

    /// Close every clean tab except the active tab, preserving all dirty tabs.
    /// Returns compaction metadata when tabs were removed.
    pub fn close_other_saved(&mut self) -> Option<TabCompaction> {
        if self.tabs.is_empty() {
            self.ensure_scratch();
            return None;
        }
        let old_active = self.active.min(self.tabs.len().saturating_sub(1));
        let len = self.tabs.len();
        let mut old_to_new = vec![None; len];
        let mut kept: Vec<(usize, Tab)> = Vec::new();
        let mut removed_tabs: Vec<Tab> = Vec::new();
        for (idx, tab) in self.tabs.drain(..).enumerate() {
            if idx == old_active || tab.is_dirty() {
                old_to_new[idx] = Some(kept.len());
                kept.push((idx, tab));
            } else {
                removed_tabs.push(tab);
            }
        }
        let removed = removed_tabs.len();
        if removed == 0 {
            self.tabs = kept.into_iter().map(|(_, tab)| tab).collect();
            self.active = old_to_new[old_active].unwrap_or(0);
            self.ensure_scratch();
            return None;
        }
        let new_active = kept
            .iter()
            .position(|(idx, _)| *idx == old_active)
            .unwrap_or(0);
        self.tabs = kept.drain(..).map(|(_, tab)| tab).collect();
        self.active = new_active;
        self.ensure_scratch();
        let removed_empty_scratch = removed_tabs
            .iter()
            .filter(|tab| Self::is_empty_scratch(tab))
            .count();
        self.remember_closed_many(removed_tabs);
        Some(TabCompaction {
            removed,
            removed_empty_scratch,
            old_to_new,
        })
    }

    /// `true` when close-other-saved-tabs would remove at least one tab.
    pub fn has_other_saved_tabs_to_close(&self) -> bool {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs
            .iter()
            .enumerate()
            .any(|(idx, tab)| idx != active && !tab.is_dirty())
    }

    /// Close clean tabs to the right of the active tab, preserving dirty tabs.
    /// Returns compaction metadata when tabs were removed.
    pub fn close_saved_to_right(&mut self) -> Option<TabCompaction> {
        if self.tabs.is_empty() {
            self.ensure_scratch();
            return None;
        }
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let len = self.tabs.len();
        let mut old_to_new = vec![None; len];
        let mut kept: Vec<Tab> = Vec::new();
        let mut removed_tabs: Vec<Tab> = Vec::new();
        for (idx, tab) in self.tabs.drain(..).enumerate() {
            if idx <= active || tab.is_dirty() {
                old_to_new[idx] = Some(kept.len());
                kept.push(tab);
            } else {
                removed_tabs.push(tab);
            }
        }
        self.tabs = kept;
        self.active = active.min(self.tabs.len().saturating_sub(1));
        let removed = removed_tabs.len();
        if removed == 0 {
            return None;
        }
        let removed_empty_scratch = removed_tabs
            .iter()
            .filter(|tab| Self::is_empty_scratch(tab))
            .count();
        self.remember_closed_many(removed_tabs);
        Some(TabCompaction {
            removed,
            removed_empty_scratch,
            old_to_new,
        })
    }

    /// `true` when close-saved-tabs-to-right would remove at least one tab.
    pub fn has_saved_tabs_to_right(&self) -> bool {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs
            .iter()
            .enumerate()
            .any(|(idx, tab)| idx > active && !tab.is_dirty())
    }

    /// Close clean tabs to the left of the active tab, preserving dirty tabs.
    /// Returns compaction metadata when tabs were removed.
    pub fn close_saved_to_left(&mut self) -> Option<TabCompaction> {
        if self.tabs.is_empty() {
            self.ensure_scratch();
            return None;
        }
        let old_active = self.active.min(self.tabs.len().saturating_sub(1));
        let len = self.tabs.len();
        let mut old_to_new = vec![None; len];
        let mut kept: Vec<(usize, Tab)> = Vec::new();
        let mut removed_tabs: Vec<Tab> = Vec::new();
        for (idx, tab) in self.tabs.drain(..).enumerate() {
            if idx >= old_active || tab.is_dirty() {
                old_to_new[idx] = Some(kept.len());
                kept.push((idx, tab));
            } else {
                removed_tabs.push(tab);
            }
        }
        let removed = removed_tabs.len();
        if removed == 0 {
            self.tabs = kept.into_iter().map(|(_, tab)| tab).collect();
            self.active = old_to_new[old_active].unwrap_or(0);
            return None;
        }
        let new_active = kept
            .iter()
            .position(|(idx, _)| *idx == old_active)
            .unwrap_or(0);
        self.tabs = kept.drain(..).map(|(_, tab)| tab).collect();
        self.active = new_active;
        let removed_empty_scratch = removed_tabs
            .iter()
            .filter(|tab| Self::is_empty_scratch(tab))
            .count();
        self.remember_closed_many(removed_tabs);
        Some(TabCompaction {
            removed,
            removed_empty_scratch,
            old_to_new,
        })
    }

    /// `true` when close-saved-tabs-to-left would remove at least one tab.
    pub fn has_saved_tabs_to_left(&self) -> bool {
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs
            .iter()
            .enumerate()
            .any(|(idx, tab)| idx < active && !tab.is_dirty())
    }

    /// True when tab `idx` has unsaved edits.
    pub fn is_dirty(&self, idx: usize) -> bool {
        self.tabs.get(idx).map(Tab::is_dirty).unwrap_or(false)
    }

    /// Number of tabs with unsaved edits.
    pub fn dirty_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.is_dirty()).count()
    }

    // ---- byte-swap: store the live Mighty buffer into a slot ----

    /// Begin storing into slot `idx`: clear its byte buffer so the caller can
    /// stream fresh bytes. No-op if out of range.
    pub fn store_begin(&mut self, idx: usize) {
        if let Some(t) = self.tabs.get_mut(idx) {
            if t.read_only {
                return;
            }
            t.bytes.clear();
        }
    }

    /// Append one byte to slot `idx`'s buffer (during a store).
    pub fn store_byte(&mut self, idx: usize, byte: u8) {
        if let Some(t) = self.tabs.get_mut(idx) {
            if t.read_only {
                return;
            }
            t.bytes.push(byte);
        }
    }

    /// Commit the stored buffer + editor state into slot `idx`.
    pub fn store_commit(
        &mut self,
        idx: usize,
        cursor_line: i32,
        cursor_col: i32,
        scroll_first: i32,
    ) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.cursor_line = cursor_line.max(0);
            t.cursor_col = cursor_col.max(0);
            t.scroll_first = scroll_first.max(0);
        }
    }

    /// Mark slot `idx` dirty/clean (Mighty sets dirty on edit, clean on save).
    pub fn set_dirty(&mut self, idx: usize, dirty: bool) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.dirty = dirty && !t.read_only;
        }
    }

    /// Mark slot `idx` clean in both the chrome flag and authoritative model.
    pub fn mark_clean(&mut self, idx: usize) {
        if let Some(t) = self.tabs.get_mut(idx) {
            if !t.read_only {
                t.bytes = t.model.to_bytes();
            }
            t.dirty = false;
            t.model.mark_clean();
        }
    }

    /// Discard unsaved edits by restoring the tab from its last clean baseline.
    /// Used before a destructive close so reopen-closed cannot resurrect edits
    /// the user explicitly chose to discard.
    pub fn discard_edits(&mut self, idx: usize) {
        if let Some(t) = self.tabs.get_mut(idx) {
            let bytes = t.bytes.clone();
            let (model, fold, read_only) = model_for_bytes(t.path.as_deref(), &bytes);
            t.model = model;
            t.fold = fold;
            t.read_only = read_only;
            t.dirty = false;
            t.undo.clear();
            t.redo.clear();
        }
    }

    /// Byte length of slot `idx`'s buffer (the count Mighty pulls), or -1.
    pub fn load_len(&self, idx: usize) -> i64 {
        match self.tabs.get(idx) {
            Some(t) => t.bytes.len() as i64,
            None => -1,
        }
    }

    /// Byte at index `i` of slot `idx`'s buffer, or -1 if out of range.
    pub fn load_byte(&self, idx: usize, i: usize) -> i32 {
        match self.tabs.get(idx).and_then(|t| t.bytes.get(i)) {
            Some(b) => *b as i32,
            None => -1,
        }
    }

    /// Path of slot `idx`, if any.
    pub fn path(&self, idx: usize) -> Option<PathBuf> {
        self.tabs.get(idx).and_then(|t| t.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, content: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn open_switch_close_basics() {
        let a = write_tmp("tabs_a.txt", b"aaa\nbbb");
        let b = write_tmp("tabs_b.txt", b"hello");

        let mut s = TabStore::new();
        let ia = s.open_path(a.clone());
        assert_eq!(ia, 0);
        assert_eq!(s.count(), 1);
        assert_eq!(s.active(), 0);

        let ib = s.open_path(b.clone());
        assert_eq!(ib, 1);
        assert_eq!(s.count(), 2);
        assert_eq!(s.active(), 1);

        // Reopening an open path switches, does not duplicate.
        let again = s.open_path(a.clone());
        assert_eq!(again, 0);
        assert_eq!(s.count(), 2);
        assert_eq!(s.active(), 0);

        // next/prev wrap.
        assert_eq!(s.next(), 1);
        assert_eq!(s.next(), 0);
        assert_eq!(s.prev(), 1);

        // close active (idx 1) -> count 1, active clamps to 0.
        s.close(1);
        assert_eq!(s.count(), 1);
        assert_eq!(s.active(), 0);

        // close the last -> empty scratch remains.
        s.close(0);
        assert_eq!(s.count(), 1);
        assert!(s.get(0).unwrap().path.is_none());
    }

    #[test]
    fn open_path_reuses_canonical_equivalent_path() {
        let p = write_tmp("tabs_equivalent_path.txt", b"same");
        let equivalent = p.parent().unwrap().join(".").join(p.file_name().unwrap());

        let mut s = TabStore::new();
        assert_eq!(s.open_path(p), 0);
        assert_eq!(s.open_path(equivalent), 0);
        assert_eq!(s.count(), 1);
        assert_eq!(s.active(), 0);
    }

    #[test]
    fn byte_round_trip_preserves_bytes_and_state() {
        let mut s = TabStore::new();
        s.ensure_scratch(); // one scratch tab
                            // Open a second tab to store into.
        let p = write_tmp("tabs_rt.txt", b"orig");
        s.open_path(p);
        let idx = s.active();

        // Simulate Mighty serializing a fresh buffer "Hi\n!" with state.
        s.store_begin(idx);
        for b in b"Hi\n!" {
            s.store_byte(idx, *b);
        }
        s.store_commit(idx, 1, 1, 0);
        s.set_dirty(idx, true);

        // Load it back.
        assert_eq!(s.load_len(idx), 4);
        let got: Vec<i32> = (0..5).map(|i| s.load_byte(idx, i)).collect();
        assert_eq!(got, vec![b'H' as i32, b'i' as i32, 10, b'!' as i32, -1]);

        let t = s.get(idx).unwrap();
        assert_eq!(t.cursor_line, 1);
        assert_eq!(t.cursor_col, 1);
        assert_eq!(t.scroll_first, 0);
        assert!(t.dirty);
    }

    #[test]
    fn binary_files_open_as_read_only_previews() {
        let original = b"\0\x01\x02PNG-ish bytes";
        let p = write_tmp("tabs_binary_asset.ico", original);

        let mut s = TabStore::new();
        let idx = s.open_path(p.clone());
        let tab = s.get(idx).unwrap();

        assert!(is_probably_binary(original));
        assert!(tab.read_only);
        assert_eq!(tab.bytes, original);
        assert!(tab.model.as_text().contains("Binary file preview"));
        assert!(tab.model.as_text().contains("tabs_binary_asset.ico"));
        assert!(!tab.is_dirty());
        assert!(s.active_read_only());
    }

    #[test]
    fn oversized_files_open_as_read_only_previews_without_loading_full_file() {
        let p = std::env::temp_dir().join("tabs_oversized_asset.log");
        let _ = std::fs::remove_file(&p);
        let file = std::fs::File::create(&p).unwrap();
        file.set_len(MAX_OPEN_FILE_BYTES + 1).unwrap();

        let mut s = TabStore::new();
        let idx = s.open_path(p.clone());
        let tab = s.get(idx).unwrap();

        assert!(tab.read_only);
        assert!(tab.model.as_text().contains("Large file preview"));
        assert!(tab.model.as_text().contains("tabs_oversized_asset.log"));
        assert!(tab
            .model
            .as_text()
            .contains(&(MAX_OPEN_FILE_BYTES + 1).to_string()));
        assert!(tab.bytes.len() < 1024);
        assert!(!tab.is_dirty());
        assert!(s.active_read_only());

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reload_active_oversized_replaces_existing_tab_with_read_only_preview() {
        let p = write_tmp("tabs_reload_oversized_asset.log", b"small\n");

        let mut s = TabStore::new();
        let idx = s.open_path(p.clone());
        s.get_mut(idx)
            .unwrap()
            .undo
            .push(TextModel::from_bytes(b"old"));
        s.reload_active_oversized(MAX_OPEN_FILE_BYTES + 1);

        let tab = s.get(idx).unwrap();
        assert!(tab.read_only);
        assert!(tab.model.as_text().contains("Large file preview"));
        assert!(tab
            .model
            .as_text()
            .contains("tabs_reload_oversized_asset.log"));
        assert!(tab.bytes.len() < 1024);
        assert!(tab.undo.is_empty());
        assert!(tab.redo.is_empty());
        assert!(!tab.is_dirty());

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reload_all_clean_path_oversized_refreshes_clean_matching_tabs() {
        let p = write_tmp("tabs_reload_all_oversized.txt", b"small\n");

        let mut s = TabStore::new();
        let first = s.open_path(p.clone());
        let second = s.duplicate_active();
        s.switch(first);
        s.reload_active_oversized(MAX_OPEN_FILE_BYTES + 1);
        let (refreshed, dirty_skipped) =
            s.reload_all_clean_path_oversized_except(&p, MAX_OPEN_FILE_BYTES + 1, first);

        assert_eq!(second, 1);
        assert_eq!(refreshed, 1);
        assert_eq!(dirty_skipped, 0);
        assert!(s.get(first).unwrap().read_only);
        assert!(s.get(second).unwrap().read_only);
        assert!(s
            .get(second)
            .unwrap()
            .model
            .as_text()
            .contains("Large file preview"));

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn text_tabs_store_normalized_clean_baseline_on_reload() {
        let p = write_tmp("tabs_crlf_reload.mty", b"one\r\ntwo\r\n");

        let mut s = TabStore::new();
        let idx = s.open_path(p);
        let tab = s.get(idx).unwrap();

        assert!(!tab.read_only);
        assert_eq!(tab.model.as_text(), "one\ntwo\n");
        assert_eq!(tab.bytes, b"one\ntwo\n");
        assert!(!tab.is_dirty());
    }

    #[test]
    fn mark_clean_advances_editable_tab_baseline_to_current_model() {
        let p = write_tmp("tabs_mark_clean_baseline.mty", b"old\n");

        let mut s = TabStore::new();
        let idx = s.open_path(p);
        s.get_mut(idx)
            .unwrap()
            .model
            .set_text_preserving_cursor("new\n");
        s.set_dirty(idx, true);

        s.mark_clean(idx);

        let tab = s.get(idx).unwrap();
        assert_eq!(tab.bytes, b"new\n");
        assert!(!tab.model.dirty());
        assert!(!tab.is_dirty());
    }

    #[test]
    fn discard_edits_restores_last_clean_baseline_and_clears_history() {
        let p = write_tmp("tabs_discard_baseline.mty", b"saved\n");

        let mut s = TabStore::new();
        let idx = s.open_path(p);
        {
            let tab = s.get_mut(idx).unwrap();
            tab.model.set_text_preserving_cursor("discard me\n");
            tab.undo.push(TextModel::from_bytes(b"saved\n"));
            tab.redo.push(TextModel::from_bytes(b"redo\n"));
        }
        s.set_dirty(idx, true);

        s.discard_edits(idx);

        let tab = s.get(idx).unwrap();
        assert_eq!(tab.model.as_text(), "saved\n");
        assert_eq!(tab.bytes, b"saved\n");
        assert!(!tab.is_dirty());
        assert!(tab.undo.is_empty());
        assert!(tab.redo.is_empty());
    }

    #[test]
    fn read_only_binary_tabs_preserve_original_bytes_across_store_and_dirty() {
        let original = b"\0\x03\x04font bytes";
        let p = write_tmp("tabs_binary_store.ttf", original);

        let mut s = TabStore::new();
        let idx = s.open_path(p);
        s.store_begin(idx);
        for b in b"not the original" {
            s.store_byte(idx, *b);
        }
        s.set_dirty(idx, true);

        let tab = s.get(idx).unwrap();
        assert_eq!(tab.bytes, original);
        assert!(!tab.dirty);
        assert!(!tab.is_dirty());
        assert_eq!(s.load_len(idx), original.len() as i64);
    }

    #[test]
    fn state_preserved_across_switch() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_s1.txt", b"file a");
        let b = write_tmp("tabs_s2.txt", b"file b");
        s.open_path(a);
        s.open_path(b);

        // On tab 0, store cursor at (3, 2), scroll 0.
        s.store_commit(0, 3, 2, 0);
        // On tab 1, store cursor at (5, 4), scroll 2.
        s.store_commit(1, 5, 4, 2);

        let t0 = s.get(0).unwrap();
        assert_eq!((t0.cursor_line, t0.cursor_col, t0.scroll_first), (3, 2, 0));
        let t1 = s.get(1).unwrap();
        assert_eq!((t1.cursor_line, t1.cursor_col, t1.scroll_first), (5, 4, 2));
    }

    #[test]
    fn basename_of_scratch_and_file() {
        let t = Tab::default();
        assert_eq!(t.basename(), "(scratch)");
        let t2 = Tab {
            path: Some(PathBuf::from("/some/dir/foo.mty")),
            ..Default::default()
        };
        assert_eq!(t2.basename(), "foo.mty");
    }

    #[test]
    fn close_out_of_range_is_noop() {
        let mut s = TabStore::new();
        s.ensure_scratch();
        assert_eq!(s.close(9), 0);
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn duplicate_active_clones_live_tab_state_next_to_source() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_duplicate_a.txt", b"a");
        let b = write_tmp("tabs_duplicate_b.txt", b"b");
        s.open_path(a);
        let b_idx = s.open_path(b);
        s.store_commit(b_idx, 3, 2, 1);
        s.set_dirty(b_idx, true);
        s.get_mut(b_idx)
            .unwrap()
            .undo
            .push(TextModel::from_bytes(b"b"));

        let dup = s.duplicate_active();
        assert_eq!(dup, 2);
        assert_eq!(s.active(), 2);
        assert_eq!(s.count(), 3);
        assert!(s.get(2).unwrap().basename().contains("tabs_duplicate_b"));
        assert!(s.get(2).unwrap().is_dirty());
        assert!(s.get(2).unwrap().undo.is_empty());
        assert_eq!(s.get(b_idx).unwrap().undo.len(), 1);
        assert_eq!(
            (
                s.get(2).unwrap().cursor_line,
                s.get(2).unwrap().cursor_col,
                s.get(2).unwrap().scroll_first
            ),
            (3, 2, 1)
        );
    }

    #[test]
    fn move_active_tab_left_and_right_preserves_tab_state() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_move_a.txt", b"a");
        let b = write_tmp("tabs_move_b.txt", b"b");
        let c = write_tmp("tabs_move_c.txt", b"c");
        s.open_path(a);
        let b_idx = s.open_path(b);
        s.open_path(c);
        s.switch(b_idx);
        s.store_commit(b_idx, 7, 3, 2);
        s.set_dirty(b_idx, true);

        assert_eq!(s.move_active_left(), Some(0));
        assert_eq!(s.active(), 0);
        assert!(s.get(0).unwrap().basename().contains("tabs_move_b"));
        assert!(s.get(0).unwrap().is_dirty());
        assert_eq!(
            (
                s.get(0).unwrap().cursor_line,
                s.get(0).unwrap().cursor_col,
                s.get(0).unwrap().scroll_first
            ),
            (7, 3, 2)
        );
        assert_eq!(s.move_active_left(), None);

        assert_eq!(s.move_active_right(), Some(1));
        assert_eq!(s.active(), 1);
        assert!(s.get(1).unwrap().basename().contains("tabs_move_b"));
    }

    #[test]
    fn sort_by_name_preserves_active_logical_tab_and_returns_remap() {
        let mut s = TabStore::new();
        let z = write_tmp("tabs_sort_z.txt", b"z");
        let a = write_tmp("tabs_sort_a.txt", b"a");
        let m = write_tmp("tabs_sort_m.txt", b"m");
        s.open_path(z);
        let active = s.open_path(a);
        s.open_path(m);
        s.switch(active);

        let remap = s.sort_by_name().unwrap();
        assert_eq!(remap, vec![2, 0, 1]);
        assert_eq!(s.active(), 0);
        assert!(s.get(0).unwrap().basename().contains("tabs_sort_a"));
        assert!(s.get(1).unwrap().basename().contains("tabs_sort_m"));
        assert!(s.get(2).unwrap().basename().contains("tabs_sort_z"));
        assert_eq!(s.sort_by_name(), None);
    }

    #[test]
    fn close_duplicate_file_tabs_preserves_active_and_dirty_duplicates() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_duplicate_clean_a.txt", b"a");
        let b = write_tmp("tabs_duplicate_clean_b.txt", b"b");
        assert_eq!(s.open_path(a.clone()), 0);
        let b_idx = s.open_path(b);

        let duplicate_b = s.duplicate_active();
        assert_eq!(duplicate_b, 2);
        let dirty_duplicate_b = s.duplicate_active();
        s.set_dirty(dirty_duplicate_b, true);
        assert_eq!(s.open_path(a), 0);
        let duplicate_a = s.duplicate_active();

        let compaction = s.close_duplicate_file_tabs().unwrap();
        assert_eq!(compaction.removed, 2);
        assert_eq!(s.count(), 3);
        assert_eq!(
            s.active(),
            0,
            "active duplicate of a.txt should be preserved"
        );
        assert!(s
            .get(0)
            .unwrap()
            .basename()
            .contains("tabs_duplicate_clean_a"));
        assert!(s
            .get(1)
            .unwrap()
            .basename()
            .contains("tabs_duplicate_clean_b"));
        assert!(s.get(2).unwrap().is_dirty());
        assert_eq!(s.closed_count(), 0);
        assert!(s.reopen_closed().is_none());
        assert_eq!(compaction.old_to_new[duplicate_a], Some(0));
        assert_eq!(compaction.old_to_new[b_idx + 1], Some(1));
        assert_eq!(compaction.old_to_new[duplicate_b + 1], None);
    }

    #[test]
    fn close_duplicate_file_tabs_compacts_equivalent_clean_paths() {
        let p = write_tmp("tabs_duplicate_equivalent.txt", b"same");
        let equivalent = p.parent().unwrap().join(".").join(p.file_name().unwrap());
        let model = TextModel::from_bytes(b"same");

        let mut s = TabStore::new();
        s.tabs.push(Tab {
            path: Some(p),
            bytes: b"same".to_vec(),
            model: model.clone(),
            ..Default::default()
        });
        s.tabs.push(Tab {
            path: Some(equivalent),
            bytes: b"same".to_vec(),
            model,
            ..Default::default()
        });
        s.active = 1;

        let compaction = s.close_duplicate_file_tabs().unwrap();

        assert_eq!(compaction.removed, 1);
        assert_eq!(s.count(), 1);
        assert_eq!(s.active(), 0);
        assert_eq!(compaction.old_to_new, vec![None, Some(0)]);
    }

    #[test]
    fn close_clean_path_forget_removes_all_clean_equivalents_without_history() {
        let p = write_tmp("tabs_delete_equivalent.txt", b"same");
        let equivalent = p.parent().unwrap().join(".").join(p.file_name().unwrap());
        let keep = write_tmp("tabs_delete_keep.txt", b"keep");
        let model = TextModel::from_bytes(b"same");

        let mut s = TabStore::new();
        s.open_path(keep.clone());
        s.tabs.push(Tab {
            path: Some(p.clone()),
            bytes: b"same".to_vec(),
            model: model.clone(),
            ..Default::default()
        });
        s.tabs.push(Tab {
            path: Some(equivalent),
            bytes: b"same".to_vec(),
            model,
            ..Default::default()
        });
        s.active = 2;

        assert!(!s.any_dirty_path(&p));
        let compaction = s.close_clean_path_forget(&p).unwrap();

        assert_eq!(compaction.removed, 2);
        assert_eq!(compaction.old_to_new, vec![Some(0), None, None]);
        assert_eq!(s.count(), 1);
        assert_eq!(s.active_path().as_deref(), Some(keep.as_path()));
        assert_eq!(s.closed_count(), 0);
        assert!(s.reopen_closed().is_none());
    }

    #[test]
    fn any_dirty_path_sees_dirty_equivalent_tabs() {
        let p = write_tmp("tabs_dirty_equivalent.txt", b"same");
        let equivalent = p.parent().unwrap().join(".").join(p.file_name().unwrap());

        let mut s = TabStore::new();
        s.open_path(p.clone());
        let duplicate = s.duplicate_active();
        s.get_mut(duplicate).unwrap().path = Some(equivalent);
        s.set_dirty(duplicate, true);

        assert!(s.any_dirty_path(&p));
    }

    #[test]
    fn any_dirty_path_except_ignores_only_requested_tab() {
        let p = write_tmp("tabs_dirty_except.txt", b"same");

        let mut s = TabStore::new();
        let original = s.open_path(p.clone());
        let duplicate = s.duplicate_active();
        s.set_dirty(original, true);
        s.set_dirty(duplicate, true);

        assert!(s.any_dirty_path_except(&p, original));
        assert!(s.any_dirty_path_except(&p, duplicate));
        s.set_dirty(duplicate, false);
        assert!(!s.any_dirty_path_except(&p, original));
    }

    #[test]
    fn rebind_path_updates_all_equivalent_tabs() {
        let old = write_tmp("tabs_rebind_old.txt", b"same");
        let equivalent = old
            .parent()
            .unwrap()
            .join(".")
            .join(old.file_name().unwrap());
        let new = old.parent().unwrap().join("tabs_rebind_new.txt");
        let model = TextModel::from_bytes(b"same");

        let mut s = TabStore::new();
        s.open_path(old.clone());
        s.tabs.push(Tab {
            path: Some(equivalent),
            bytes: b"same".to_vec(),
            model,
            ..Default::default()
        });

        assert_eq!(s.rebind_path(&old, new.clone()), 2);
        assert_eq!(s.get(0).unwrap().path.as_deref(), Some(new.as_path()));
        assert_eq!(s.get(1).unwrap().path.as_deref(), Some(new.as_path()));
    }

    #[test]
    fn reload_all_clean_path_refreshes_equivalent_clean_tabs() {
        let p = write_tmp("tabs_reload_all_clean.txt", b"old\n");
        let equivalent = p.parent().unwrap().join(".").join(p.file_name().unwrap());

        let mut s = TabStore::new();
        let first = s.open_path(p.clone());
        let dirty = s.duplicate_active();
        s.get_mut(dirty)
            .unwrap()
            .model
            .set_text_preserving_cursor("dirty\n");
        s.set_dirty(dirty, true);
        let other_clean = s.duplicate_active();
        s.get_mut(other_clean).unwrap().path = Some(equivalent);
        s.set_dirty(other_clean, false);

        assert_eq!(s.reload_all_clean_path(&p, b"new\n"), (2, 1));
        assert_eq!(s.get(first).unwrap().model.as_text(), "new\n");
        assert_eq!(s.get(other_clean).unwrap().model.as_text(), "new\n");
        assert_eq!(s.get(dirty).unwrap().model.as_text(), "dirty\n");
        assert!(s.is_dirty(dirty));
    }

    #[test]
    fn close_saved_preserves_dirty_tabs_and_active_neighbor() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_close_saved_a.txt", b"a");
        let b = write_tmp("tabs_close_saved_b.txt", b"b");
        let c = write_tmp("tabs_close_saved_c.txt", b"c");
        let ia = s.open_path(a);
        let ib = s.open_path(b);
        let ic = s.open_path(c);
        s.set_dirty(ia, true);
        s.set_dirty(ic, true);
        s.switch(ib);

        assert_eq!(s.close_saved().unwrap().removed, 1);
        assert_eq!(s.count(), 2);
        assert_eq!(s.active(), 1);
        assert!(s.get(0).unwrap().basename().contains("tabs_close_saved_a"));
        assert!(s.get(1).unwrap().basename().contains("tabs_close_saved_c"));
        assert!(s.get(0).unwrap().is_dirty());
        assert!(s.get(1).unwrap().is_dirty());
    }

    #[test]
    fn close_saved_all_clean_leaves_scratch() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_close_saved_all_a.txt", b"a");
        let b = write_tmp("tabs_close_saved_all_b.txt", b"b");
        s.open_path(a);
        s.open_path(b);

        let compaction = s.close_saved().unwrap();
        assert_eq!(compaction.removed, 2);
        assert_eq!(compaction.removed_empty_scratch, 0);
        assert_eq!(s.count(), 1);
        assert!(s.get(0).unwrap().path.is_none());
        assert_eq!(s.active(), 0);
        assert!(s.close_saved().is_none());
    }

    #[test]
    fn close_saved_reports_empty_scratch_removal_separately() {
        let mut s = TabStore::new();
        s.ensure_scratch();
        let file = write_tmp("tabs_close_saved_with_scratch.txt", b"file");
        s.open_path(file);

        let compaction = s.close_saved().unwrap();

        assert_eq!(compaction.removed, 2);
        assert_eq!(compaction.removed_empty_scratch, 1);
        assert_eq!(s.count(), 1);
        assert!(s.get(0).unwrap().path.is_none());
        assert_eq!(s.closed_count(), 1, "empty scratch tabs are not reopenable");
    }

    #[test]
    fn close_saved_tabs_can_be_reopened_from_history() {
        let mut s = TabStore::new();
        let clean_a = write_tmp("tabs_close_saved_reopen_a.txt", b"a");
        let dirty_b = write_tmp("tabs_close_saved_reopen_b.txt", b"b");
        let clean_c = write_tmp("tabs_close_saved_reopen_c.txt", b"c");
        s.open_path(clean_a);
        let dirty = s.open_path(dirty_b);
        s.set_dirty(dirty, true);
        s.open_path(clean_c);

        let compaction = s.close_saved().unwrap();
        assert_eq!(compaction.removed, 2);
        assert_eq!(compaction.removed_empty_scratch, 0);
        assert_eq!(s.closed_count(), 2);
        let reopened = s.reopen_closed().unwrap();
        assert_eq!(s.active(), reopened);
        assert!(s
            .get(reopened)
            .unwrap()
            .basename()
            .contains("tabs_close_saved_reopen_c"));
        let reopened = s.reopen_closed().unwrap();
        assert!(s
            .get(reopened)
            .unwrap()
            .basename()
            .contains("tabs_close_saved_reopen_a"));
        assert_eq!(s.closed_count(), 0);
    }

    #[test]
    fn tab_management_predicates_track_noop_states() {
        let mut s = TabStore::new();
        s.ensure_scratch();
        assert!(s.active_tab_is_first());
        assert!(s.active_tab_is_last());
        assert!(s.tabs_already_sorted_by_name());
        assert!(!s.has_saved_tabs_to_close());
        assert!(!s.has_other_saved_tabs_to_close());
        assert!(!s.has_saved_tabs_to_right());
        assert!(!s.has_saved_tabs_to_left());
        assert!(!s.has_clean_duplicate_file_tabs());

        let b = write_tmp("tabs_predicate_b.txt", b"b");
        let a = write_tmp("tabs_predicate_a.txt", b"a");
        s.open_path(b);
        let active_a = s.open_path(a);
        assert_eq!(s.active(), active_a);
        assert!(!s.active_tab_is_first());
        assert!(s.active_tab_is_last());
        assert!(!s.tabs_already_sorted_by_name());
        assert!(s.has_saved_tabs_to_close());
        assert!(s.has_other_saved_tabs_to_close());
        assert!(!s.has_saved_tabs_to_right());
        assert!(s.has_saved_tabs_to_left());

        let duplicate_a = s.duplicate_active();
        assert!(s.has_clean_duplicate_file_tabs());
        s.set_dirty(duplicate_a, true);
        assert!(!s.has_clean_duplicate_file_tabs());
    }

    #[test]
    fn close_other_saved_keeps_active_and_dirty_tabs() {
        let mut s = TabStore::new();
        let clean_active = write_tmp("tabs_close_other_saved_active.txt", b"a");
        let dirty = write_tmp("tabs_close_other_saved_dirty.txt", b"b");
        let clean_other = write_tmp("tabs_close_other_saved_other.txt", b"c");
        let ia = s.open_path(clean_active);
        let ib = s.open_path(dirty);
        s.open_path(clean_other);
        s.set_dirty(ib, true);
        s.switch(ia);

        assert_eq!(s.close_other_saved().unwrap().removed, 1);
        assert_eq!(s.count(), 2);
        assert_eq!(s.active(), 0);
        assert!(s
            .get(0)
            .unwrap()
            .basename()
            .contains("tabs_close_other_saved_active"));
        assert!(s
            .get(1)
            .unwrap()
            .basename()
            .contains("tabs_close_other_saved_dirty"));
        assert!(!s.get(0).unwrap().is_dirty());
        assert!(s.get(1).unwrap().is_dirty());
        assert!(s.close_other_saved().is_none());
    }

    #[test]
    fn close_saved_to_right_preserves_dirty_right_tabs() {
        let mut s = TabStore::new();
        let active = write_tmp("tabs_close_right_active.txt", b"a");
        let clean = write_tmp("tabs_close_right_clean.txt", b"b");
        let dirty = write_tmp("tabs_close_right_dirty.txt", b"c");
        let ia = s.open_path(active);
        s.open_path(clean);
        let id = s.open_path(dirty);
        s.set_dirty(id, true);
        s.switch(ia);

        assert_eq!(s.close_saved_to_right().unwrap().removed, 1);
        assert_eq!(s.count(), 2);
        assert_eq!(s.active(), 0);
        assert!(s
            .get(0)
            .unwrap()
            .basename()
            .contains("tabs_close_right_active"));
        assert!(s
            .get(1)
            .unwrap()
            .basename()
            .contains("tabs_close_right_dirty"));
        assert!(s.get(1).unwrap().is_dirty());
    }

    #[test]
    fn close_saved_to_left_preserves_dirty_left_tabs() {
        let mut s = TabStore::new();
        let dirty = write_tmp("tabs_close_left_dirty.txt", b"a");
        let clean = write_tmp("tabs_close_left_clean.txt", b"b");
        let active = write_tmp("tabs_close_left_active.txt", b"c");
        let id = s.open_path(dirty);
        s.open_path(clean);
        let ia = s.open_path(active);
        s.set_dirty(id, true);
        s.switch(ia);

        assert_eq!(s.close_saved_to_left().unwrap().removed, 1);
        assert_eq!(s.count(), 2);
        assert_eq!(s.active(), 1);
        assert!(s
            .get(0)
            .unwrap()
            .basename()
            .contains("tabs_close_left_dirty"));
        assert!(s
            .get(1)
            .unwrap()
            .basename()
            .contains("tabs_close_left_active"));
        assert!(s.get(0).unwrap().is_dirty());
        assert_eq!(s.closed_count(), 1);
        let reopened = s.reopen_closed().unwrap();
        assert!(s
            .get(reopened)
            .unwrap()
            .basename()
            .contains("tabs_close_left_clean"));
    }

    #[test]
    fn reopen_closed_restores_last_closed_tab() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_reopen_a.txt", b"a");
        let b = write_tmp("tabs_reopen_b.txt", b"b");
        s.open_path(a);
        let ib = s.open_path(b);
        assert_eq!(s.close(ib), 0);
        assert_eq!(s.closed_count(), 1);

        let reopened = s.reopen_closed().unwrap();
        assert_eq!(reopened, 1);
        assert_eq!(s.active(), 1);
        assert_eq!(s.count(), 2);
        assert!(s.get(1).unwrap().basename().contains("tabs_reopen_b"));
        assert_eq!(s.closed_count(), 0);
        assert!(s.reopen_closed().is_none());
    }

    #[test]
    fn close_forget_does_not_enter_reopen_history() {
        let mut s = TabStore::new();
        let a = write_tmp("tabs_close_forget_a.txt", b"a");
        let b = write_tmp("tabs_close_forget_b.txt", b"b");
        s.open_path(a);
        let ib = s.open_path(b);

        assert_eq!(s.close_forget(ib), 0);
        assert_eq!(s.count(), 1);
        assert_eq!(s.closed_count(), 0);
        assert!(s.reopen_closed().is_none());
    }
}
