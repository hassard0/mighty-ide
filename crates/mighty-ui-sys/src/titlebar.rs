//! Custom window title bar — window controls (minimize / maximize / close), the
//! draggable caption strip, and the borderless resize-edge hit regions.
//!
//! The window is borderless (`with_decorations(false)`), so the IDE owns the
//! chrome. Rather than add a separate caption row (which would reflow the whole
//! editor), the window controls live at the RIGHT of the existing top tab-bar
//! row and the rest of that row + the rail header act as the OS-drag region.
//!
//! All geometry here is in LOGICAL pixels (the same space the layout/click math
//! uses), so it scales with `ui_scale` like the rest of the UI.

use crate::layout;

/// Height (px) of the top row that doubles as the title bar (== the tab bar).
pub fn bar_h() -> f32 {
    layout::TAB_BAR_H
}

/// Width (px) of one window-control button (min / max / close).
pub const BTN_W: f32 = 46.0;
/// Thickness (px) of the resize-grab band along each window edge. Widened from a
/// too-thin 6px so the borderless window is actually easy to grab-resize.
pub const EDGE: f32 = 9.0;
/// Corners get a larger square grab zone (diagonal resize is the common case and
/// the hardest to hit), so a corner wins within this distance of two edges.
pub const CORNER: f32 = 18.0;

/// A title-bar hit result (what a press at a point landed on).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TitleHit {
    /// The minimize button.
    Minimize,
    /// The maximize / restore button.
    Maximize,
    /// The close button.
    Close,
    /// The draggable caption strip (begin an OS window drag).
    Drag,
}

/// The x of the left edge of the first (leftmost) window-control button, for a
/// window `win_w` wide. The three buttons occupy `[controls_x, win_w)`.
pub fn controls_x(win_w: f32) -> f32 {
    win_w - 3.0 * BTN_W
}

/// Hit-test a point `(x, y)` (logical px) against the title-bar controls + drag
/// strip. `win_w` is the window width; `body_left` is where the tab-bar row
/// starts (right of the rail/sidebar). Returns `None` when the point is outside
/// the bar entirely (so the normal click routing runs).
pub fn hit(x: f32, y: f32, win_w: f32, body_left: f32) -> Option<TitleHit> {
    if y < 0.0 || y >= bar_h() {
        return None;
    }
    let cx = controls_x(win_w);
    if x >= cx && x < win_w {
        let which = ((x - cx) / BTN_W).floor() as i32;
        return Some(match which {
            0 => TitleHit::Minimize,
            1 => TitleHit::Maximize,
            _ => TitleHit::Close,
        });
    }
    // The caption strip is the tab-bar row right of the body, but NOT over a tab
    // (tabs handle their own clicks first in the IDE routing) — we report Drag
    // for the empty region to the right of the last tab and left of the controls.
    // The run/more-actions icons live in [cx-60, cx); exclude that strip (+8px
    // padding) so those clicks pass through to the IDE instead of starting a drag.
    if x >= body_left && x < cx - 68.0 {
        return Some(TitleHit::Drag);
    }
    // The rail header (above the first rail icon, the M-mark strip) is also a
    // drag handle, so the user can grab the top-left to move the window.
    if (0.0..layout::RAIL_W).contains(&x) {
        return Some(TitleHit::Drag);
    }
    None
}

/// Map a point near a window edge to a resize-direction code (mirrors
/// [`crate::window::ResizeDir::from_code`]), or `0` when not on an edge. The grab
/// band is [`EDGE`] px thick; corners (within `EDGE` of two edges) win.
pub fn resize_code(x: f32, y: f32, win_w: f32, win_h: f32) -> i32 {
    // Corners first, with a larger grab square (diagonal resize is hardest to hit).
    let cw = x <= CORNER;
    let ce = x >= win_w - CORNER;
    let cn = y <= CORNER;
    let cs = y >= win_h - CORNER;
    if cn && cw {
        return 5; // NorthWest
    }
    if cn && ce {
        return 6; // NorthEast
    }
    if cs && cw {
        return 7; // SouthWest
    }
    if cs && ce {
        return 8; // SouthEast
    }
    // Edges (thinner band).
    let on_w = x <= EDGE;
    let on_e = x >= win_w - EDGE;
    let on_n = y <= EDGE;
    let on_s = y >= win_h - EDGE;
    if on_n {
        return 3; // North
    }
    if on_s {
        return 4; // South
    }
    if on_w {
        return 1; // West
    }
    if on_e {
        return 2; // East
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_resolve_left_to_right() {
        let w = 1000.0;
        let cx = controls_x(w);
        assert_eq!(hit(cx + 1.0, 5.0, w, 100.0), Some(TitleHit::Minimize));
        assert_eq!(hit(cx + BTN_W + 1.0, 5.0, w, 100.0), Some(TitleHit::Maximize));
        assert_eq!(hit(cx + 2.0 * BTN_W + 1.0, 5.0, w, 100.0), Some(TitleHit::Close));
        // The far-right pixel is still the close button.
        assert_eq!(hit(w - 1.0, 5.0, w, 100.0), Some(TitleHit::Close));
    }

    #[test]
    fn caption_strip_is_drag() {
        let w = 1000.0;
        // Between the body left and the controls: drag.
        assert_eq!(hit(500.0, 5.0, w, 100.0), Some(TitleHit::Drag));
        // The rail header (top-left) is also a drag handle.
        assert_eq!(hit(10.0, 5.0, w, 100.0), Some(TitleHit::Drag));
    }

    #[test]
    fn below_the_bar_is_none() {
        let w = 1000.0;
        assert_eq!(hit(500.0, bar_h() + 1.0, w, 100.0), None);
        assert_eq!(hit(500.0, -1.0, w, 100.0), None);
    }

    #[test]
    fn resize_corners_and_edges() {
        let (w, h) = (800.0, 600.0);
        assert_eq!(resize_code(1.0, 1.0, w, h), 5); // NW
        assert_eq!(resize_code(w - 1.0, 1.0, w, h), 6); // NE
        assert_eq!(resize_code(1.0, h - 1.0, w, h), 7); // SW
        assert_eq!(resize_code(w - 1.0, h - 1.0, w, h), 8); // SE
        assert_eq!(resize_code(1.0, 300.0, w, h), 1); // W
        assert_eq!(resize_code(w - 1.0, 300.0, w, h), 2); // E
        assert_eq!(resize_code(400.0, 1.0, w, h), 3); // N
        assert_eq!(resize_code(400.0, h - 1.0, w, h), 4); // S
        // Interior: no resize.
        assert_eq!(resize_code(400.0, 300.0, w, h), 0);
    }
}
