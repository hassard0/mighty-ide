//! Scalar `mui_ghost_*` ABI for inline AI ghost-text completions.
//!
//! The engine + all logic live in [`crate::ghost`]; this is the thin scalar
//! veneer the Mighty loop drives (mirrors `dapabi` / `panels` AI wiring):
//!
//!   * [`mui_ghost_enabled`] — `1` when inline AI is on AND a key is present.
//!   * [`mui_ghost_arm`] — schedule a debounced request after an edit.
//!   * [`mui_ghost_tick`] — each frame; fires the request once the debounce
//!     elapses and the editor is idle. Returns `1` if a request started.
//!   * [`mui_ghost_poll`] — each frame; drains a finished background result into
//!     the ghost. Returns `1` when a fresh suggestion became available.
//!   * [`mui_ghost_has`] — `1` if a ghost is currently shown.
//!   * [`mui_ghost_accept`] — insert the FULL suggestion at the cursor (reusing
//!     the editor insert path), clear the ghost. Returns `1` if it accepted.
//!   * [`mui_ghost_accept_word`] — insert ONE word; keep the remainder as ghost.
//!   * [`mui_ghost_dismiss`] — clear the ghost + cancel any pending request.
//!   * [`mui_ghost_force`] — explicit trigger (Alt+\), bypassing the debounce.
//!   * [`mui_ghost_draw`] — paint the dim ghost overlay at the cursor.

use std::time::Instant;

use crate::ghost::{Context, GhostState};
use crate::layout;
use crate::theme;
use crate::MuiContext;

/// Cast an opaque `i64` handle back to a context reference.
#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

/// Snapshot the active editor into a completion [`Context`] (full text, cursor,
/// language, filename).
fn snapshot(ctx: &MuiContext) -> Context {
    let m = ctx.tabs.active_model();
    Context {
        text: m.as_text(),
        cur_line: m.cursor_line(),
        cur_col: m.cursor_col(),
        language: ctx.language.display_name().to_string(),
        file_name: ctx.file_name.clone(),
    }
}

/// `1` when inline AI ghost-text is enabled (setting on AND an API key present).
#[no_mangle]
pub extern "C" fn mui_ghost_enabled(_handle: i64) -> i32 {
    i32::from(GhostState::enabled())
}

/// Schedule a debounced ghost request after an edit. No-op when disabled.
#[no_mangle]
pub extern "C" fn mui_ghost_arm(handle: i64) {
    if let Some(c) = unsafe { ctx(handle) } {
        c.ghost.arm();
    }
}

/// Per-frame tick: fire the debounced request if its idle deadline elapsed and no
/// request is in flight. Returns `1` if a request was started this frame.
#[no_mangle]
pub extern "C" fn mui_ghost_tick(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    // The borrow checker needs the snapshot closure to capture an immutable view
    // distinct from the &mut ghost; take the snapshot eagerly only if armed math
    // would fire is not possible without &self, so snapshot lazily via raw ptr.
    let ptr: *const MuiContext = c;
    let now = Instant::now();
    i32::from(c.ghost.tick(now, || snapshot(unsafe { &*ptr })))
}

/// Per-frame poll: drain a finished result into the ghost. Returns `1` when a
/// fresh suggestion became available (the IDE redraws to show it).
#[no_mangle]
pub extern "C" fn mui_ghost_poll(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let anchor = {
        let m = c.tabs.active_model();
        (m.cursor_line(), m.cursor_col())
    };
    i32::from(c.ghost.poll(anchor))
}

/// `1` if a ghost suggestion is currently shown.
#[no_mangle]
pub extern "C" fn mui_ghost_has(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.ghost.has_ghost()))
}

/// Accept the FULL suggestion: insert it at the cursor via the editor model, then
/// clear the ghost. Returns `1` if a suggestion was accepted, else `0`.
#[no_mangle]
pub extern "C" fn mui_ghost_accept(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(text) = c.ghost.accept() else {
        return 0;
    };
    let m = c.tabs.active_model_mut();
    for ch in text.chars() {
        m.insert_char(ch);
    }
    1
}

/// Partial accept: insert ONE word of the suggestion; keep the remainder as the
/// live ghost (re-anchored at the new cursor). Returns `1` if anything was
/// accepted, else `0`.
#[no_mangle]
pub extern "C" fn mui_ghost_accept_word(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(word) = c.ghost.accept_word() else {
        return 0;
    };
    let new_anchor = {
        let m = c.tabs.active_model_mut();
        for ch in word.chars() {
            m.insert_char(ch);
        }
        (m.cursor_line(), m.cursor_col())
    };
    // Re-anchor the remaining ghost (if any) at the post-insert cursor.
    c.ghost.set_anchor(new_anchor);
    1
}

/// Dismiss the ghost + cancel any pending/in-flight request.
#[no_mangle]
pub extern "C" fn mui_ghost_dismiss(handle: i64) {
    if let Some(c) = unsafe { ctx(handle) } {
        c.ghost.dismiss();
    }
}

/// Explicit trigger (Alt+\): fire a request immediately, bypassing the debounce.
/// Returns `1` if a request was started.
#[no_mangle]
pub extern "C" fn mui_ghost_force(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let ptr: *const MuiContext = c;
    let snap = snapshot(unsafe { &*ptr });
    i32::from(c.ghost.force(snap))
}

/// Draw the dim ghost-text overlay starting at the cursor. The first line
/// continues to the right of the real text on the cursor row; any following lines
/// are painted dimmed below, as a non-destructive overlay (the real buffer is
/// untouched). No-op when no ghost is shown or the anchor row is off-screen.
#[no_mangle]
pub extern "C" fn mui_ghost_draw(handle: i64) {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(sugg) = c.ghost.suggestion().map(|s| s.to_string()) else {
        return;
    };
    let (anchor_line, anchor_col) = c.ghost.anchor();
    let region = layout::region(c.sidebar_visible);
    let clip = c.clip;
    let total = c.tabs.active_model().line_count().max(1) as u64;
    let first = c.tabs.active_model().first_visible();
    let rows = layout::visible_rows_in(region, c.gpu.height, c.term_open) as usize;

    // The anchor row must be on-screen for the (first) ghost line to show.
    if anchor_line < first || anchor_line >= first + rows {
        return;
    }

    let dim = theme::DIM();
    let lines: Vec<&str> = sugg.split('\n').collect();

    // Ghost renders on the base layer over the editor body (after the editor text
    // draw, before chrome). It does not occlude — it's just dim glyphs.
    for (i, gline) in lines.iter().enumerate() {
        let screen_line = anchor_line + i;
        if screen_line < first || screen_line >= first + rows {
            continue;
        }
        let row = (screen_line - first) as i32;
        let y = layout::row_y_in(region, row);
        // The first ghost line begins at the cursor column; subsequent lines start
        // at column 0 (a continuation, like a real multi-line insert).
        let col = if i == 0 { anchor_col as i32 } else { 0 };
        let x = layout::text_x_in(region, total, col);
        if gline.is_empty() {
            continue;
        }
        c.text.queue(x, y, gline, dim, clip);
    }
}

#[cfg(test)]
mod tests {
    // The engine logic is exhaustively tested in `crate::ghost`. These ABI fns are
    // thin field-access veneers; a null-handle smoke test confirms they never
    // panic / deref a null pointer.
    use super::*;

    #[test]
    fn null_handle_is_safe() {
        assert_eq!(mui_ghost_enabled(0), i32::from(GhostState::enabled()));
        mui_ghost_arm(0);
        assert_eq!(mui_ghost_tick(0), 0);
        assert_eq!(mui_ghost_poll(0), 0);
        assert_eq!(mui_ghost_has(0), 0);
        assert_eq!(mui_ghost_accept(0), 0);
        assert_eq!(mui_ghost_accept_word(0), 0);
        mui_ghost_dismiss(0);
        assert_eq!(mui_ghost_force(0), 0);
        mui_ghost_draw(0);
    }
}
