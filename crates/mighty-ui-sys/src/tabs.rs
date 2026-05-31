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

/// Snapshot a model's lines into owned strings (for the fold scanner, which is
/// pure over `&[String]`). The model stores newlines as line boundaries, so this
/// is one `String` per buffer line.
fn model_lines(model: &TextModel) -> Vec<String> {
    (0..model.line_count())
        .map(|i| model.line(i).to_string())
        .collect()
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
        self.dirty || self.model.dirty()
    }
}

/// The ordered set of open tabs plus the active index. Always holds at least one
/// tab (closing the last tab leaves an empty scratch tab).
#[derive(Debug, Default)]
pub struct TabStore {
    tabs: Vec<Tab>,
    active: usize,
}

impl TabStore {
    pub fn new() -> Self {
        TabStore {
            tabs: Vec::new(),
            active: 0,
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

    /// Find an already-open tab whose path matches `path` (canonicalized loosely
    /// via direct equality). Returns its index.
    pub fn find_by_path(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path))
    }

    /// Open `path` as a new tab (reading its bytes from disk), or switch to the
    /// existing tab if already open. Returns the tab index. If the file can't be
    /// read it is opened empty (so a brand-new file path still gets a tab).
    pub fn open_path(&mut self, path: PathBuf) -> usize {
        if let Some(i) = self.find_by_path(&path) {
            self.active = i;
            return i;
        }
        let bytes = std::fs::read(&path).unwrap_or_default();
        let model = TextModel::from_bytes(&bytes);
        let mut fold = FoldState::new();
        fold.recompute_owned(&model_lines(&model));
        self.tabs.push(Tab {
            path: Some(path),
            bytes,
            model,
            fold,
            cursor_line: 0,
            cursor_col: 0,
            scroll_first: 0,
            dirty: false,
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

    /// Set the active tab's file path (Save As on an untitled buffer binds it to a
    /// real path so subsequent saves write there).
    pub fn set_active_path(&mut self, path: PathBuf) {
        let i = self.active.min(self.tabs.len().saturating_sub(1));
        self.tabs[i].path = Some(path);
    }

    /// `true` when the active tab is backed by a file path (vs an untitled buffer).
    pub fn active_has_path(&self) -> bool {
        self.tabs
            .get(self.active.min(self.tabs.len().saturating_sub(1)))
            .map(|t| t.path.is_some())
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
        self.tabs[i].model = TextModel::from_bytes(bytes);
        self.tabs[i].bytes = bytes.to_vec();
        self.tabs[i].dirty = false;
        // A fresh buffer: recompute folds and drop any stale folded state.
        let lines = model_lines(&self.tabs[i].model);
        self.tabs[i].fold = FoldState::new();
        self.tabs[i].fold.recompute_owned(&lines);
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
        if idx >= self.tabs.len() {
            return self.active;
        }
        self.tabs.remove(idx);
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

    /// True when tab `idx` has unsaved edits.
    pub fn is_dirty(&self, idx: usize) -> bool {
        self.tabs.get(idx).map(Tab::is_dirty).unwrap_or(false)
    }

    // ---- byte-swap: store the live Mighty buffer into a slot ----

    /// Begin storing into slot `idx`: clear its byte buffer so the caller can
    /// stream fresh bytes. No-op if out of range.
    pub fn store_begin(&mut self, idx: usize) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.bytes.clear();
        }
    }

    /// Append one byte to slot `idx`'s buffer (during a store).
    pub fn store_byte(&mut self, idx: usize, byte: u8) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.bytes.push(byte);
        }
    }

    /// Commit the stored buffer + editor state into slot `idx`.
    pub fn store_commit(&mut self, idx: usize, cursor_line: i32, cursor_col: i32, scroll_first: i32) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.cursor_line = cursor_line.max(0);
            t.cursor_col = cursor_col.max(0);
            t.scroll_first = scroll_first.max(0);
        }
    }

    /// Mark slot `idx` dirty/clean (Mighty sets dirty on edit, clean on save).
    pub fn set_dirty(&mut self, idx: usize, dirty: bool) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.dirty = dirty;
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
}
