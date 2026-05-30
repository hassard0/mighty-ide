//! Quick-fix lightbulb gutter indicator — pure, unit-testable state.
//!
//! When the cursor's line has available code actions (a quick-fix / refactor /
//! `mty fix`), a tasteful accent-tinted bulb is drawn just left of that line.
//! Clicking it (or the existing Ctrl+.) opens the code-actions menu at the line.
//!
//! "Has actions" is computed by requesting code actions for the current line
//! (reusing `language.rs` / the `mui_codeaction_*` path) — but the LSP must not
//! be spammed every frame, so the refresh is **debounced**: it only re-probes
//! when the cursor LINE changes, and then only after a short idle ([`IDLE_FRAMES`]
//! frames sitting on the same line). This module owns the bookkeeping (which line
//! is tracked, whether a probe is due, whether the bulb is visible) and the
//! gutter hit-test; the actual probe (which needs the GPU context + LSP) lives in
//! `crate::wsabi`.

/// Frames the cursor must rest on a line before we probe it for actions. At the
/// IDE's frame cadence this is a fraction of a second — enough to avoid probing
/// every line a held arrow key sweeps through.
pub const IDLE_FRAMES: u32 = 6;

/// The lightbulb state: the line currently associated with the bulb, whether
/// actions exist there, plus the debounce bookkeeping.
#[derive(Debug, Default, Clone)]
pub struct Lightbulb {
    /// The 0-based line the bulb is currently associated with (the line we last
    /// probed). `-1` until the first probe.
    line: i32,
    /// Whether code actions exist for [`line`](Self::line) (drives visibility).
    has_actions: bool,
    /// The line the cursor was on last frame (to detect a line change).
    last_seen_line: i32,
    /// Frames the cursor has rested on the current line since it last changed.
    idle: u32,
    /// The last-drawn gutter rect of the bulb (window space), for click hit-test.
    /// `None` when the bulb isn't drawn this frame.
    rect: Option<(f32, f32, f32, f32)>,
}

impl Lightbulb {
    pub fn new() -> Self {
        Lightbulb {
            line: -1,
            has_actions: false,
            last_seen_line: -1,
            idle: 0,
            rect: None,
        }
    }

    /// Whether the bulb should be shown: only when actions exist AND the cursor is
    /// still on the line we probed (so it never lingers on a moved cursor).
    pub fn visible_for(&self, cursor_line: i32) -> bool {
        self.has_actions && self.line == cursor_line && self.line >= 0
    }

    /// The line the bulb is associated with (the last probed line).
    pub fn line(&self) -> i32 {
        self.line
    }

    /// Advance the debounce for a frame at `cursor_line`. Returns `true` when a
    /// fresh code-action probe is DUE (the caller should run it and feed the
    /// result back via [`set_result`]). The contract:
    ///
    ///   * cursor moved to a new line → reset the idle counter, clear the stale
    ///     bulb, and DON'T probe yet (wait for the line to settle);
    ///   * cursor still on the same line → count idle frames; once it reaches
    ///     [`IDLE_FRAMES`] and we haven't probed this line yet, a probe is due.
    pub fn tick(&mut self, cursor_line: i32) -> bool {
        if cursor_line != self.last_seen_line {
            // The cursor moved: the old bulb no longer applies.
            self.last_seen_line = cursor_line;
            self.idle = 0;
            if self.line != cursor_line {
                self.has_actions = false;
            }
            return false;
        }
        // Same line as last frame. Already probed this exact line? Nothing to do.
        if self.line == cursor_line {
            return false;
        }
        self.idle = self.idle.saturating_add(1);
        self.idle >= IDLE_FRAMES
    }

    /// Record the result of a probe of `line`: whether actions exist there.
    pub fn set_result(&mut self, line: i32, has_actions: bool) {
        self.line = line;
        self.has_actions = has_actions;
        self.idle = 0;
    }

    /// Force the bulb hidden + un-probed (e.g. on tab switch / file reload, where
    /// the previous line's actions are meaningless against the new buffer).
    pub fn reset(&mut self) {
        self.line = -1;
        self.has_actions = false;
        self.last_seen_line = -1;
        self.idle = 0;
        self.rect = None;
    }

    /// Stash the bulb's drawn gutter rect (window space) for the next click test.
    pub fn set_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.rect = Some((x, y, w, h));
    }

    /// Clear the stored rect (the bulb wasn't drawn this frame).
    pub fn clear_rect(&mut self) {
        self.rect = None;
    }

    /// `true` if the point `(px, py)` is inside the last-drawn bulb rect.
    pub fn hit(&self, px: f32, py: f32) -> bool {
        match self.rect {
            Some((x, y, w, h)) => px >= x && px <= x + w && py >= y && py <= y + h,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_until_probed() {
        let lb = Lightbulb::new();
        assert!(!lb.visible_for(0));
        assert!(!lb.visible_for(5));
    }

    #[test]
    fn probe_due_only_after_idle_on_a_settled_line() {
        let mut lb = Lightbulb::new();
        // Frame 1: cursor "appears" on line 3 (last_seen was -1) -> settle, no probe.
        assert!(!lb.tick(3));
        // Idle on line 3: probe becomes due after IDLE_FRAMES same-line frames.
        for _ in 0..(IDLE_FRAMES - 1) {
            assert!(!lb.tick(3));
        }
        assert!(lb.tick(3), "probe should be due after settling");
    }

    #[test]
    fn moving_lines_resets_and_does_not_probe() {
        let mut lb = Lightbulb::new();
        // Settle + probe line 3.
        lb.tick(3);
        for _ in 0..IDLE_FRAMES {
            lb.tick(3);
        }
        lb.set_result(3, true);
        assert!(lb.visible_for(3));
        // Move to line 4: bulb hides, no immediate probe.
        assert!(!lb.tick(4));
        assert!(!lb.visible_for(4));
        assert!(!lb.visible_for(3));
    }

    #[test]
    fn visible_only_when_actions_exist_for_the_current_line() {
        let mut lb = Lightbulb::new();
        // Probe line 10, actions exist.
        lb.set_result(10, true);
        assert!(lb.visible_for(10));
        // A different cursor line is not the probed line -> hidden.
        assert!(!lb.visible_for(11));
        // Probe line 10, NO actions -> hidden even on the same line.
        lb.set_result(10, false);
        assert!(!lb.visible_for(10));
    }

    #[test]
    fn already_probed_line_does_not_re_probe() {
        let mut lb = Lightbulb::new();
        lb.set_result(5, true);
        // Sitting on the already-probed line never re-fires a probe.
        for _ in 0..(IDLE_FRAMES * 3) {
            assert!(!lb.tick(5));
        }
    }

    #[test]
    fn reset_clears_everything() {
        let mut lb = Lightbulb::new();
        lb.set_result(7, true);
        lb.set_rect(1.0, 2.0, 3.0, 4.0);
        assert!(lb.visible_for(7));
        assert!(lb.hit(2.0, 3.0));
        lb.reset();
        assert!(!lb.visible_for(7));
        assert!(!lb.hit(2.0, 3.0));
    }

    #[test]
    fn hit_test_uses_last_rect() {
        let mut lb = Lightbulb::new();
        assert!(!lb.hit(5.0, 5.0));
        lb.set_rect(10.0, 20.0, 16.0, 16.0);
        assert!(lb.hit(12.0, 22.0));
        assert!(!lb.hit(0.0, 0.0));
        lb.clear_rect();
        assert!(!lb.hit(12.0, 22.0));
    }
}
