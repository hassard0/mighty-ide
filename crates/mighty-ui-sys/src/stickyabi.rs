//! Scalar C ABI for Sticky scroll + Peek definition. The state + draw + pure
//! logic live in [`crate::sticky`] and [`crate::peek`]; this module is the flat
//! `mui_*` veneer Mighty drives each frame (L17).

use crate::layout;
use crate::MuiContext;

#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

// ===========================================================================
// Feature 1 — Sticky scroll
// ===========================================================================

/// Recompute + return the number of sticky headers to pin for the current frame.
///
/// The shim derives the enclosing-scope chain from the active document's outline
/// symbols + the editor's scroll offset (the top visible line). Mighty calls this
/// once per frame BEFORE drawing; it mirrors the `sticky_scroll` preference (so
/// the toggle takes effect live) and returns 0 when nothing should be pinned
/// (not scrolled / disabled / no enclosing scope).
#[no_mangle]
pub extern "C" fn mui_sticky_count(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    // Keep the enabled flag in sync with the live Settings pref each frame.
    ctx.sticky.set_enabled(crate::settings::sticky_scroll());
    let top = ctx.tabs.active_model().first_visible() as u32;
    let source = ctx.tabs.active_model().as_text();
    let src_lines: Vec<&str> = source.split('\n').collect();
    // Borrow the outline symbols + sticky state disjointly.
    let outline = std::mem::take(&mut ctx.outline);
    let n = ctx.sticky.recompute(outline.symbols(), &src_lines, top);
    ctx.outline = outline;
    n as i32
}

/// The 0-based jump line of pinned sticky row `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_sticky_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.sticky.line_of(i as usize))
}

/// Draw the pinned sticky headers as an elevated band at the top of the editor
/// body. No-op when nothing is pinned. `region` is unused as a scalar (kept for
/// ABI symmetry / future per-region control); the draw computes geometry itself.
#[no_mangle]
pub extern "C" fn mui_sticky_draw(handle: i64, region: i32) {
    let _ = region;
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if ctx.sticky.count() == 0 {
        return;
    }
    let lang = ctx.language;
    let total = ctx.tabs.active_model().line_count().max(1) as u64;
    let sticky = std::mem::take(&mut ctx.sticky);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    sticky.draw(ctx, lang, total);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.sticky = sticky;
}

/// Map the last click (the shim's stored `last_event`) to a pinned sticky row's
/// jump line, or `-1` when the click is not on a sticky header. Clicking a sticky
/// header jumps to that scope's declaration line (the IDE moves the cursor +
/// scrolls there). Takes no pixel arg because Mighty can't pass the event coords;
/// the shim reads them itself (mirrors `mui_outline_row_at_click`).
#[no_mangle]
pub extern "C" fn mui_sticky_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if ctx.sticky.count() == 0 {
        return -1;
    }
    let region = layout::region(ctx.sidebar_visible);
    let y = ctx.last_event.y;
    let row = ctx.sticky.row_at(region, y);
    if row < 0 {
        return -1;
    }
    ctx.sticky.line_of(row as usize)
}

// ===========================================================================
// Feature 2 — Peek definition
// ===========================================================================

/// Open the inline peek card at the cursor: resolve the definition (reusing the
/// nav `textDocument/definition` request), read a window of source lines around
/// the target, and store them. Returns `1` if a definition was found + a preview
/// built, else `0` (and leaves peek closed). The IDE streams the live buffer into
/// the nav buffer (via `mui_ed_nav_stream`) before calling this.
#[no_mangle]
pub extern "C" fn mui_peek_open(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.peek.close();
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => return 0,
    };
    let source = String::from_utf8_lossy(&ctx.nav_buf).into_owned();
    // Resolve the definition target (path + 0-based line/col).
    let raw = crate::abi::lsp_def_raw(ctx.language, &path, &source, line.max(0) as u32, col.max(0) as u32);
    let target = match crate::nav::parse_definition(&raw) {
        Some((uri, tline, tcol)) => crate::nav::uri_to_path(&uri).map(|p| (p, tline, tcol)),
        None => None,
    };
    let Some((tpath, tline, tcol)) = target else {
        println!("peek: line={line} col={col} found=0 (no definition)");
        return 0;
    };
    // Use the LIVE buffer for the preview when the target is the active file (so
    // unsaved edits show), else read the other file from disk.
    let same = crate::nav::paths_equal(&tpath, &path);
    let live = if same { Some(source.as_str()) } else { None };
    let lang = ctx.language;
    let opened = ctx
        .peek
        .open_at(tpath, tline, tcol, line.max(0) as u32, lang, live);
    println!(
        "peek: line={line} col={col} found=1 target_line={tline} same_file={same} preview_lines={}",
        ctx.peek.line_count()
    );
    i32::from(opened)
}

/// `1` if the peek card is currently active.
#[no_mangle]
pub extern "C" fn mui_peek_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.peek.is_active()))
}

/// Number of previewed source lines in the peek card.
#[no_mangle]
pub extern "C" fn mui_peek_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.peek.line_count() as i32)
}

/// Number of chars in previewed peek line `i` (for sizing), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_peek_line_text(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        let t = c.peek.line_text(i as usize);
        if t.is_empty() && i as usize >= c.peek.line_count() {
            -1
        } else {
            t.chars().count() as i32
        }
    })
}

/// The 0-based source line number of previewed peek row `i`, or `-1` out of
/// range. (`_kind` companion to `_text`; exposes the true line for the gutter.)
#[no_mangle]
pub extern "C" fn mui_peek_line_kind(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.peek.line_no(i as usize))
}

/// Close the peek card (Esc).
#[no_mangle]
pub extern "C" fn mui_peek_close(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.peek.close();
    }
}

/// Scroll the peek preview body by `delta` rows (clamped).
#[no_mangle]
pub extern "C" fn mui_peek_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.peek.scroll_by(delta);
    }
}

/// Resolve the peek target into a tab + cursor jump (Enter / "go to"): if the
/// target is in another file, open it as a tab; then move the cursor to the
/// definition line/col. Closes the peek card. Returns the resulting tab index
/// (other file) or `1` (same file), or `-1` when there is no target.
#[no_mangle]
pub extern "C" fn mui_peek_goto(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let (tpath, tline, tcol) = match ctx.peek.target_path() {
        Some(p) => (p.to_path_buf(), ctx.peek.target_line(), ctx.peek.target_col()),
        None => return -1,
    };
    ctx.peek.close();
    let current = ctx.file_path.clone();
    let same = current
        .as_deref()
        .map(|c| crate::nav::paths_equal(&tpath, c))
        .unwrap_or(false);
    if same {
        let m = ctx.tabs.active_model_mut();
        m.move_to(tline as i32, tcol as i32);
        let first = (tline as i32 - 2).max(0);
        m.set_first_visible(first as usize);
        1
    } else {
        let idx = ctx.tabs.open_path(tpath);
        crate::abi::sync_active_path(ctx);
        let m = ctx.tabs.active_model_mut();
        m.move_to(tline as i32, tcol as i32);
        let first = (tline as i32 - 2).max(0);
        m.set_first_visible(first as usize);
        idx as i32
    }
}

/// Draw the inline peek card below the cursor line. No-op when inactive. The
/// IDE passes the editor scroll offset (`first`) + visible `rows` so the card
/// anchors under the correct row.
#[no_mangle]
pub extern "C" fn mui_peek_draw(handle: i64, first: i32, rows: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.peek.is_active() {
        return;
    }
    let total = ctx.tabs.active_model().line_count().max(1) as u64;
    let peek = std::mem::take(&mut ctx.peek);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    peek.draw(ctx, first.max(0) as u32, rows.max(0) as u32, total);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.peek = peek;
}
