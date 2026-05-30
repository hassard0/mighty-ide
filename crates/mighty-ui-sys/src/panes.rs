//! Editor **pane layout** — an additive side-by-side split over the existing
//! single-editor model (pure, unit-testable).
//!
//! ## Why this is purely additive (the unsplit invariant)
//!
//! Today the IDE renders ONE editor view (the active tab) into the editor
//! region. Many features (completion / diagnostics / nav / sticky / ghost /
//! snippets / minimap) target "the active editor". To add a split WITHOUT
//! touching any of them, panes are a thin layer ON TOP of the existing
//! active-tab + per-tab `TextModel` state:
//!
//! * A [`PaneLayout`] is a list of [`Pane`]s (each a **tab index** + its own
//!   **scroll offset**) plus a **focused** index. It starts with exactly ONE
//!   pane.
//! * **INVARIANT:** with exactly one pane the layout is inert — the single pane
//!   simply mirrors the current active tab, and every `mui_ed_*` op operates on
//!   the focused pane's tab exactly as before. So all existing tests pass
//!   unchanged and the unsplit render path is byte-identical.
//! * When **focus changes**, the focused pane's tab becomes "the active tab"
//!   (the existing [`crate::tabs::TabStore`] active index) and its saved scroll
//!   is restored into that tab's model — so completion / diagnostics / nav / etc.
//!   keep working on the focused pane with ZERO per-feature changes.
//!
//! The split itself (column geometry, the divider, per-pane draw) is the only
//! genuinely new render work; it lives behind `mui_pane_*` and `mui_pane_draw`.

/// Maximum number of editor panes (v1 supports a single side-by-side split).
pub const MAX_PANES: usize = 2;

/// One editor pane: which tab it shows + its own scroll offset. The pane does
/// NOT own a `TextModel` — the model still lives in the tab (so a tab shown in
/// two panes is the SAME document). Only the scroll is per-pane; when a pane is
/// focused its scroll is pushed into the tab's model (see [`PaneLayout::focus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pane {
    /// The tab index this pane shows.
    pub tab: usize,
    /// This pane's top visible line (scroll offset), saved while it is unfocused.
    pub scroll: usize,
}

impl Pane {
    fn new(tab: usize) -> Self {
        Pane { tab, scroll: 0 }
    }
}

/// The editor pane layout: 1..=[`MAX_PANES`] panes laid out left→right, plus the
/// focused index. Always holds at least one pane.
#[derive(Debug, Clone)]
pub struct PaneLayout {
    panes: Vec<Pane>,
    focused: usize,
}

impl Default for PaneLayout {
    fn default() -> Self {
        PaneLayout {
            panes: vec![Pane::new(0)],
            focused: 0,
        }
    }
}

impl PaneLayout {
    /// A fresh single-pane layout bound to `tab` (the current active tab).
    pub fn new(tab: usize) -> Self {
        PaneLayout {
            panes: vec![Pane::new(tab)],
            focused: 0,
        }
    }

    /// Number of panes (>= 1).
    pub fn count(&self) -> usize {
        self.panes.len()
    }

    /// `true` when split into more than one pane (the only state that changes
    /// the render path; with one pane everything is unsplit / identical). The
    /// shim reads `count() > 1` inline on the hot draw path; this stays as the
    /// readable predicate for callers/tests.
    #[allow(dead_code)]
    pub fn is_split(&self) -> bool {
        self.panes.len() > 1
    }

    /// The focused pane index (0-based).
    pub fn focused(&self) -> usize {
        self.focused.min(self.panes.len().saturating_sub(1))
    }

    /// The tab index shown in pane `i`, or `None` out of range.
    pub fn tab_at(&self, i: usize) -> Option<usize> {
        self.panes.get(i).map(|p| p.tab)
    }

    /// The saved scroll of pane `i`, or `None` out of range.
    pub fn scroll_at(&self, i: usize) -> Option<usize> {
        self.panes.get(i).map(|p| p.scroll)
    }

    /// The tab shown in the focused pane (the one all `mui_ed_*` ops target).
    pub fn focused_tab(&self) -> usize {
        self.panes[self.focused()].tab
    }

    /// Set pane `i`'s tab (used when the tab bar opens a file into a pane).
    /// No-op out of range.
    pub fn set_tab(&mut self, i: usize, tab: usize) {
        if let Some(p) = self.panes.get_mut(i) {
            p.tab = tab;
        }
    }

    /// Save the live scroll of the CURRENTLY focused pane (the value the active
    /// tab's model holds right now), so a focus change / split can stash it
    /// before re-binding the active tab. Call this with the focused pane's model
    /// scroll before mutating focus.
    pub fn save_focused_scroll(&mut self, scroll: usize) {
        let f = self.focused();
        if let Some(p) = self.panes.get_mut(f) {
            p.scroll = scroll;
        }
    }

    /// Split the focused pane to the RIGHT, creating a new pane that shows
    /// `new_tab` (the caller chooses the current tab or the next one) and
    /// focusing it. Caps at [`MAX_PANES`]: a second split call just refocuses /
    /// retargets the existing right pane. Returns the new focused pane index.
    ///
    /// `cur_scroll` is the live scroll of the (about-to-be-unfocused) pane, saved
    /// so returning focus to it restores its position.
    pub fn split_right(&mut self, new_tab: usize, cur_scroll: usize) -> usize {
        self.save_focused_scroll(cur_scroll);
        if self.panes.len() >= MAX_PANES {
            // Already split: just retarget + focus the LAST (right) pane.
            let last = self.panes.len() - 1;
            self.panes[last].tab = new_tab;
            self.focused = last;
            return self.focused;
        }
        // Insert the new pane directly to the right of the focused one.
        let at = self.focused() + 1;
        self.panes.insert(at, Pane::new(new_tab));
        self.focused = at;
        self.focused
    }

    /// Focus pane `i` (clamped). `cur_scroll` is the live scroll of the currently
    /// focused pane, saved before the switch. Returns the new focused index.
    pub fn focus(&mut self, i: usize, cur_scroll: usize) -> usize {
        self.save_focused_scroll(cur_scroll);
        self.focused = i.min(self.panes.len().saturating_sub(1));
        self.focused
    }

    /// Cycle focus to the next pane (wraps). `cur_scroll` is saved into the
    /// current pane first. Returns the new focused index. With one pane this is a
    /// no-op (stays focused on pane 0).
    pub fn focus_next(&mut self, cur_scroll: usize) -> usize {
        self.save_focused_scroll(cur_scroll);
        if self.panes.len() > 1 {
            self.focused = (self.focused() + 1) % self.panes.len();
        }
        self.focused
    }

    /// Close the focused pane. If one pane remains the layout returns to the
    /// unsplit state (the remaining pane keeps its tab; its scroll is restored by
    /// the caller). No-op when only one pane exists. Returns the new focused index.
    pub fn close_focused(&mut self) -> usize {
        if self.panes.len() <= 1 {
            return self.focused();
        }
        let f = self.focused();
        self.panes.remove(f);
        // Focus the neighbor to the left (or 0).
        self.focused = f.saturating_sub(1).min(self.panes.len() - 1);
        self.focused
    }

    /// Remap pane→tab indices after a tab at `closed` was removed from the store
    /// (every tab index above `closed` shifts down by one; a pane that showed the
    /// closed tab falls back to the clamped neighbor). Keeps panes valid so a
    /// closed tab never leaves a pane pointing past the end. `tab_count` is the
    /// post-close tab count (>= 1).
    pub fn on_tab_closed(&mut self, closed: usize, tab_count: usize) {
        let last = tab_count.saturating_sub(1);
        for p in &mut self.panes {
            if p.tab > closed {
                p.tab -= 1;
            } else if p.tab == closed {
                p.tab = p.tab.min(last);
            }
            p.tab = p.tab.min(last);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_pane_is_inert() {
        let l = PaneLayout::new(0);
        assert_eq!(l.count(), 1);
        assert!(!l.is_split());
        assert_eq!(l.focused(), 0);
        assert_eq!(l.focused_tab(), 0);
        assert_eq!(l.tab_at(0), Some(0));
        assert_eq!(l.tab_at(1), None);
    }

    #[test]
    fn single_pane_focus_ops_are_noops() {
        let mut l = PaneLayout::new(3);
        assert_eq!(l.focus_next(5), 0); // no second pane to move to
        assert_eq!(l.focused(), 0);
        assert_eq!(l.close_focused(), 0); // can't close the last pane
        assert_eq!(l.count(), 1);
        // The saved scroll is recorded but irrelevant while unsplit.
        assert_eq!(l.scroll_at(0), Some(5));
        assert_eq!(l.focused_tab(), 3);
    }

    #[test]
    fn split_creates_second_pane_and_focuses_it() {
        let mut l = PaneLayout::new(0);
        // Focused pane (0) is on tab 0, scrolled to line 12; split to show tab 1.
        let f = l.split_right(1, 12);
        assert_eq!(f, 1);
        assert_eq!(l.count(), 2);
        assert!(l.is_split());
        assert_eq!(l.focused(), 1);
        // Pane 0 kept its tab + its scroll was stashed.
        assert_eq!(l.tab_at(0), Some(0));
        assert_eq!(l.scroll_at(0), Some(12));
        // Pane 1 shows the new tab, fresh scroll.
        assert_eq!(l.tab_at(1), Some(1));
        assert_eq!(l.scroll_at(1), Some(0));
        assert_eq!(l.focused_tab(), 1);
    }

    #[test]
    fn focus_switch_rebinds_tab_and_saves_per_pane_scroll() {
        let mut l = PaneLayout::new(0);
        l.split_right(1, 0);
        // On the right pane (tab 1) scroll to line 30, then focus back to pane 0.
        let f0 = l.focus(0, 30);
        assert_eq!(f0, 0);
        assert_eq!(l.focused_tab(), 0); // active tab is now pane 0's tab
        assert_eq!(l.scroll_at(1), Some(30)); // right pane's scroll saved
        // Focus the right pane again; its scroll (30) is what the caller restores.
        let f1 = l.focus_next(7);
        assert_eq!(f1, 1);
        assert_eq!(l.scroll_at(0), Some(7)); // left pane's live scroll saved
        assert_eq!(l.scroll_at(1), Some(30));
        assert_eq!(l.focused_tab(), 1);
    }

    #[test]
    fn focus_next_wraps_two_panes() {
        let mut l = PaneLayout::new(0);
        l.split_right(1, 0);
        assert_eq!(l.focused(), 1);
        assert_eq!(l.focus_next(0), 0);
        assert_eq!(l.focus_next(0), 1);
    }

    #[test]
    fn close_pane_returns_to_single_pane_state() {
        let mut l = PaneLayout::new(0);
        l.split_right(1, 0);
        assert!(l.is_split());
        // Close the focused (right) pane -> back to one pane, identical to unsplit.
        let f = l.close_focused();
        assert_eq!(l.count(), 1);
        assert!(!l.is_split());
        assert_eq!(f, 0);
        assert_eq!(l.focused(), 0);
        assert_eq!(l.focused_tab(), 0);
        // Further close is a no-op.
        assert_eq!(l.close_focused(), 0);
        assert_eq!(l.count(), 1);
    }

    #[test]
    fn close_left_pane_keeps_right_tab() {
        let mut l = PaneLayout::new(0);
        l.split_right(5, 0); // right pane shows tab 5, focused
        l.focus(0, 0); // focus the left pane (tab 0)
        let f = l.close_focused(); // close left -> only the (former right) pane remains
        assert_eq!(l.count(), 1);
        assert_eq!(f, 0);
        assert_eq!(l.focused_tab(), 5); // the surviving pane shows tab 5
    }

    #[test]
    fn third_split_caps_at_two_panes() {
        let mut l = PaneLayout::new(0);
        l.split_right(1, 0);
        l.focus(0, 0);
        // A second split from the left pane just retargets+focuses the right one.
        let f = l.split_right(7, 0);
        assert_eq!(l.count(), 2);
        assert_eq!(f, 1);
        assert_eq!(l.tab_at(1), Some(7));
        assert_eq!(l.focused_tab(), 7);
    }

    #[test]
    fn set_tab_retargets_a_pane() {
        let mut l = PaneLayout::new(0);
        l.split_right(1, 0);
        l.set_tab(0, 4);
        assert_eq!(l.tab_at(0), Some(4));
        l.set_tab(9, 2); // out of range: no-op
        assert_eq!(l.tab_at(0), Some(4));
    }

    #[test]
    fn on_tab_closed_remaps_pane_tabs() {
        let mut l = PaneLayout::new(2);
        l.split_right(3, 0); // panes show tabs 2 and 3
        // Tab 1 closed: indices above shift down (2->1, 3->2). Post count = 3.
        l.on_tab_closed(1, 3);
        assert_eq!(l.tab_at(0), Some(1));
        assert_eq!(l.tab_at(1), Some(2));
    }

    #[test]
    fn on_tab_closed_clamps_pane_showing_closed_tab() {
        let mut l = PaneLayout::new(0);
        l.split_right(2, 0); // panes show tabs 0 and 2
        // Tab 2 itself closed; post count = 2 (tabs 0,1). The right pane clamps.
        l.on_tab_closed(2, 2);
        assert_eq!(l.tab_at(0), Some(0));
        assert_eq!(l.tab_at(1), Some(1));
    }
}
