//! Undo / redo history (pure, unit-testable).
//!
//! The Mighty side keeps exactly ONE live edit buffer (`Vec[I32]` of byte
//! values). Managing nested undo Vecs in Mighty would hit L21 (a Vec param read
//! deep in a branchy loop is clobbered by native codegen), so the entire undo
//! history lives shim-side here. Mighty streams its post-edit buffer in (reusing
//! the same byte-streaming path as save / tab-store) and the shim decides whether
//! to push a new snapshot or coalesce into the last one.
//!
//! ## Snapshot model
//!
//! A [`Snapshot`] is a full copy of the buffer bytes plus the cursor `(line,
//! col)` at the time of the edit. Full snapshots (rather than diffs) keep the
//! logic dead-simple and robust; editor buffers are small enough that a capped
//! stack of a few hundred copies is cheap.
//!
//! ## Recording scheme (chosen: post-edit record + coalescing)
//!
//! Mighty calls [`record`](HistoryStore::record) AFTER each edit, streaming its
//! full post-edit buffer. The shim:
//!   * ignores a record identical to the current top (no-op, e.g. a cursor-only
//!     move that didn't change bytes);
//!   * COALESCES a pure single-char typing run into the most recent entry, so a
//!     burst of typing collapses to one undo step. A run is "broken" — forcing a
//!     fresh snapshot — by [`break_run`](HistoryStore::break_run), which Mighty
//!     calls on any non-insert action (newline, cursor move, delete, save,
//!     format, find-jump, tab switch, …). This gives the documented granularity:
//!     **one Ctrl+Z undoes a contiguous typing run**, not the whole file and not
//!     one char at a time.
//!   * pushes redo-invalidating: any new record clears the redo stack.
//!
//! ## Initial state
//!
//! [`seed`](HistoryStore::seed) installs the loaded buffer as the baseline
//! "current" snapshot WITHOUT making it undoable past that point. Undo returns
//! the previous snapshot; with only the seed present, undo reports "nothing".

/// A captured buffer state: full bytes + 0-based cursor line/col.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub bytes: Vec<u8>,
    pub cursor_line: i32,
    pub cursor_col: i32,
}

impl Snapshot {
    fn new(bytes: Vec<u8>, cursor_line: i32, cursor_col: i32) -> Self {
        Snapshot {
            bytes,
            cursor_line: cursor_line.max(0),
            cursor_col: cursor_col.max(0),
        }
    }
}

/// Undo / redo stacks of full buffer snapshots.
///
/// Invariant: `undo` always holds at least one entry once [`seed`] has run — its
/// top is the CURRENT buffer state. `record` pushes the post-edit state as the
/// new top (or coalesces into it). `undo` moves the top onto `redo` and returns
/// the new top; `redo` moves a `redo` entry back onto `undo` and returns it.
#[derive(Debug, Default)]
pub struct HistoryStore {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Max depth of the undo stack; oldest entries are dropped past this.
    cap: usize,
    /// When true, the next `record` of a single-char-larger buffer starts a NEW
    /// undo entry instead of coalescing into the current top. Set by `break_run`
    /// (a non-typing boundary) and after any structural edit.
    run_broken: bool,
    // ---- staging for a streamed record (mirrors the save/tab-store path) ----
    staging: Vec<u8>,
}

/// Default undo depth cap.
pub const DEFAULT_CAP: usize = 200;

impl HistoryStore {
    pub fn new() -> Self {
        HistoryStore {
            undo: Vec::new(),
            redo: Vec::new(),
            cap: DEFAULT_CAP,
            run_broken: true,
            staging: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn with_cap(cap: usize) -> Self {
        HistoryStore {
            cap: cap.max(1),
            ..HistoryStore::new()
        }
    }

    /// Install `bytes` as the baseline current state, clearing all history. Used
    /// when a buffer is first loaded or replaced wholesale (open / tab switch).
    pub fn seed(&mut self, bytes: Vec<u8>, cursor_line: i32, cursor_col: i32) {
        self.undo.clear();
        self.redo.clear();
        self.undo.push(Snapshot::new(bytes, cursor_line, cursor_col));
        self.run_broken = true;
        self.staging.clear();
    }

    /// Mark the current typing run as broken, so the next `record` begins a new
    /// undo entry rather than coalescing. Cheap; Mighty calls this on any
    /// non-insert boundary (cursor move, newline, delete, save, format, …).
    pub fn break_run(&mut self) {
        self.run_broken = true;
    }

    // ---- streamed record (one byte at a time, like save / tab-store) ----

    /// Begin streaming a post-edit buffer: clear the staging buffer.
    pub fn record_begin(&mut self) {
        self.staging.clear();
    }

    /// Append one byte to the staging buffer.
    pub fn record_byte(&mut self, byte: u8) {
        self.staging.push(byte);
    }

    /// Commit the staged buffer as a record at cursor `(line, col)`. See
    /// [`record`] for the coalescing rules. Returns `true` if a snapshot was
    /// pushed or coalesced (i.e. the buffer changed), `false` if it was a no-op.
    pub fn record_commit(&mut self, cursor_line: i32, cursor_col: i32) -> bool {
        let bytes = std::mem::take(&mut self.staging);
        self.record(bytes, cursor_line, cursor_col)
    }

    /// Install the staged buffer (from `record_begin`/`record_byte`) as the
    /// history baseline at cursor `(line, col)`, clearing all prior history.
    /// Used on load / tab switch where Mighty streams via the record-staging
    /// path but wants seed (not record) semantics.
    pub fn seed_from_staging(&mut self, cursor_line: i32, cursor_col: i32) {
        let bytes = std::mem::take(&mut self.staging);
        self.seed(bytes, cursor_line, cursor_col);
    }

    /// Record the post-edit `bytes` + cursor as the new current state.
    ///
    /// Rules:
    ///   * identical to the current top → no-op (returns false);
    ///   * any record invalidates the redo stack;
    ///   * a pure single-char-append typing edit (buffer grew by exactly one
    ///     byte that is the only change at the tail, the new byte is not a
    ///     newline, and the run is not broken) COALESCES: it overwrites the
    ///     current top instead of pushing, so a typing burst is one undo step;
    ///   * otherwise push a fresh snapshot and cap the stack depth.
    pub fn record(&mut self, bytes: Vec<u8>, cursor_line: i32, cursor_col: i32) -> bool {
        let snap = Snapshot::new(bytes, cursor_line, cursor_col);

        // No-op if nothing changed.
        if let Some(top) = self.undo.last() {
            if top.bytes == snap.bytes {
                return false;
            }
        }

        // Any genuine edit invalidates redo.
        self.redo.clear();

        let coalesce = !self.run_broken && self.is_single_char_typing(&snap.bytes);
        if coalesce {
            // Overwrite the current top: the typing run remains one undo step.
            if let Some(top) = self.undo.last_mut() {
                *top = snap;
                return true;
            }
        }

        self.undo.push(snap);
        // A pushed single-char append opens a coalescing run for the NEXT char.
        self.run_broken = false;
        // Cap depth: drop the oldest, but never drop below one entry.
        while self.undo.len() > self.cap {
            self.undo.remove(0);
        }
        true
    }

    /// True if `new_bytes` is the current top with exactly one extra non-newline
    /// byte appended at the cursor tail — the shape of a single typed character.
    /// (We treat "grew by one and shares the whole old prefix up to the insert"
    /// conservatively as: len grew by exactly 1 and the old bytes are a
    /// subsequence-prefix+suffix around one insert. Cheap exact check below.)
    fn is_single_char_typing(&self, new_bytes: &[u8]) -> bool {
        let Some(top) = self.undo.last() else {
            return false;
        };
        let old = &top.bytes;
        if new_bytes.len() != old.len() + 1 {
            return false;
        }
        // Find the single insertion point: the first index where they differ.
        let mut i = 0;
        while i < old.len() && old[i] == new_bytes[i] {
            i += 1;
        }
        // The inserted byte is new_bytes[i]; the rest must match.
        if new_bytes[i] == b'\n' {
            return false; // newline always breaks a run
        }
        old[i..] == new_bytes[i + 1..]
    }

    /// Undo: if there is a prior state, move the current top onto the redo stack
    /// and return a clone of the new top. Returns `None` if nothing to undo
    /// (only the baseline remains). Always breaks the typing run.
    pub fn undo(&mut self) -> Option<Snapshot> {
        if self.undo.len() < 2 {
            return None;
        }
        let popped = self.undo.pop().unwrap();
        self.redo.push(popped);
        self.run_broken = true;
        self.undo.last().cloned()
    }

    /// Redo: if there is a redone state available, move it back onto the undo
    /// stack and return a clone of it. Returns `None` if nothing to redo. Always
    /// breaks the typing run.
    pub fn redo(&mut self) -> Option<Snapshot> {
        let snap = self.redo.pop()?;
        self.undo.push(snap.clone());
        self.run_broken = true;
        Some(snap)
    }

    /// Number of undo steps available (states behind the current one).
    pub fn undo_depth(&self) -> usize {
        self.undo.len().saturating_sub(1)
    }

    /// Number of redo steps available.
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_bytes(h: &HistoryStore) -> Option<&[u8]> {
        h.undo.last().map(|s| s.bytes.as_slice())
    }

    #[test]
    fn seed_then_nothing_to_undo() {
        let mut h = HistoryStore::new();
        h.seed(b"hello".to_vec(), 0, 5);
        assert_eq!(h.undo_depth(), 0);
        assert!(h.undo().is_none());
        assert!(h.redo().is_none());
    }

    #[test]
    fn structural_edit_pushes_and_undoes() {
        let mut h = HistoryStore::new();
        h.seed(b"abc".to_vec(), 0, 3);
        h.break_run();
        // A delete (shorter buffer) — never coalesces.
        assert!(h.record(b"ab".to_vec(), 0, 2));
        assert_eq!(h.undo_depth(), 1);
        let s = h.undo().expect("undo available");
        assert_eq!(s.bytes, b"abc");
        assert_eq!((s.cursor_line, s.cursor_col), (0, 3));
        // Now redo brings back "ab".
        let r = h.redo().expect("redo available");
        assert_eq!(r.bytes, b"ab");
        assert!(h.redo().is_none());
    }

    #[test]
    fn typing_run_coalesces_into_one_step() {
        let mut h = HistoryStore::new();
        h.seed(b"".to_vec(), 0, 0);
        // Simulate typing "abc" char-by-char, no break between.
        assert!(h.record(b"a".to_vec(), 0, 1)); // pushes (first char after seed)
        assert!(h.record(b"ab".to_vec(), 0, 2)); // coalesce
        assert!(h.record(b"abc".to_vec(), 0, 3)); // coalesce
        // Only ONE undo step beyond the seed.
        assert_eq!(h.undo_depth(), 1);
        let s = h.undo().expect("undo");
        assert_eq!(s.bytes, b""); // back to the seed in one step
    }

    #[test]
    fn break_run_starts_new_step() {
        let mut h = HistoryStore::new();
        h.seed(b"".to_vec(), 0, 0);
        h.record(b"a".to_vec(), 0, 1);
        h.record(b"ab".to_vec(), 0, 2); // coalesced with "a"
        h.break_run(); // e.g. cursor move
        h.record(b"abc".to_vec(), 0, 3); // new step
        assert_eq!(h.undo_depth(), 2);
        let s = h.undo().unwrap();
        assert_eq!(s.bytes, b"ab");
        let s2 = h.undo().unwrap();
        assert_eq!(s2.bytes, b"");
    }

    #[test]
    fn newline_does_not_coalesce() {
        let mut h = HistoryStore::new();
        h.seed(b"a".to_vec(), 0, 1);
        h.record(b"ab".to_vec(), 0, 2); // typing, pushes
        h.record(b"ab\n".to_vec(), 1, 0); // newline -> new step (not coalesced)
        assert_eq!(h.undo_depth(), 2);
    }

    #[test]
    fn new_edit_invalidates_redo() {
        let mut h = HistoryStore::new();
        h.seed(b"x".to_vec(), 0, 1);
        h.break_run();
        h.record(b"xy".to_vec(), 0, 2);
        h.break_run();
        h.record(b"xyz".to_vec(), 0, 3);
        // Undo twice -> redo has 2.
        h.undo();
        h.undo();
        assert_eq!(h.redo_depth(), 2);
        // A new edit clears redo.
        h.break_run();
        assert!(h.record(b"xQ".to_vec(), 0, 2));
        assert_eq!(h.redo_depth(), 0);
    }

    #[test]
    fn identical_record_is_noop() {
        let mut h = HistoryStore::new();
        h.seed(b"same".to_vec(), 0, 4);
        assert!(!h.record(b"same".to_vec(), 0, 4));
        assert_eq!(h.undo_depth(), 0);
    }

    #[test]
    fn depth_cap_drops_oldest() {
        let mut h = HistoryStore::with_cap(3);
        h.seed(b"".to_vec(), 0, 0);
        // Push 5 distinct structural edits; cap is 3 entries total.
        for i in 1..=5 {
            h.break_run();
            let bytes = vec![b'x'; i];
            h.record(bytes, 0, i as i32);
        }
        // Undo stack capped at 3 entries.
        assert!(h.undo.len() <= 3);
        // The newest state is still "xxxxx".
        assert_eq!(snap_bytes(&h), Some(b"xxxxx".as_slice()));
    }

    #[test]
    fn streamed_record_round_trip() {
        let mut h = HistoryStore::new();
        h.seed(b"hi".to_vec(), 0, 2);
        h.break_run();
        h.record_begin();
        for b in b"hi!" {
            h.record_byte(*b);
        }
        assert!(h.record_commit(0, 3));
        assert_eq!(snap_bytes(&h), Some(b"hi!".as_slice()));
        let s = h.undo().unwrap();
        assert_eq!(s.bytes, b"hi");
    }

    #[test]
    fn cursor_restored_on_undo_redo() {
        let mut h = HistoryStore::new();
        h.seed(b"line0\nline1".to_vec(), 1, 5);
        h.break_run();
        h.record(b"line0\nline1X".to_vec(), 1, 6);
        let u = h.undo().unwrap();
        assert_eq!((u.cursor_line, u.cursor_col), (1, 5));
        let r = h.redo().unwrap();
        assert_eq!((r.cursor_line, r.cursor_col), (1, 6));
    }
}
