//! Scalar-only C ABI (`mui_*_s` / staging fns) for the Mighty IDE main loop.
//!
//! ## Why a second ABI surface
//!
//! v0.36 Mighty `extern c` can only express **scalar** argument/return shapes
//! end-to-end (I32/I64/F32/F64/U8/USize). It CANNOT, from Mighty-owned data:
//!   * pass a pointer (`*U8`) — `Str → *U8` coercion and address-of-local both
//!     fail (extern-c-matrix rows 03/04/09 only "work" via a C-side wrapper that
//!     owns the buffer);
//!   * pass a `#[repr(C)]` struct by value or receive one (rows 05/07);
//!   * receive a value through an out-pointer (row 04).
//!
//! So the struct/pointer ABI in `lib.rs` (`mui_init`, `mui_fill_rect(.. MuiColor)`,
//! `mui_poll_event(.. *mut MuiEvent)`, `mui_draw_text(.. *u8, len ..)`) is NOT
//! callable from a built Mighty program. This module re-exposes the same
//! capabilities using only scalars:
//!   * the context handle is an opaque `i64` (a `*mut MuiContext` cast to int);
//!   * colors are four `f32` args;
//!   * text is staged into a shim-owned byte buffer one codepoint at a time,
//!     then drawn/flushed;
//!   * events are polled to a scalar tag, with scalar field accessors reading
//!     the last-polled event;
//!   * file I/O lives entirely in the shim (Mighty can't pass paths/bytes),
//!     exposed as load-by-index reads and a staged save buffer.
//!
//! The Rust GPU tests still exercise the struct ABI in `lib.rs`; this module is
//! a thin scalar veneer over the same `MuiContext`.

use std::path::PathBuf;

use crate::diagnostics::{self, Severity};
use crate::ffi::*;
use crate::layout;
use crate::theme;
use crate::MuiContext;

/// Resolve the file to edit: `argv[1]` if given, else a scratch file in the
/// current directory. The scratch file is created empty if it does not exist
/// (so the editor never defaults to its own source — see deliverable 1).
fn resolve_target_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    let scratch = PathBuf::from("scratch.mty");
    if !scratch.exists() {
        if let Err(e) = std::fs::write(&scratch, b"") {
            eprintln!("mui_init_s: could not create scratch file: {e}");
        }
    }
    scratch
}

/// Basename of `path` (file name component), or the whole path as a fallback.
fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Cast an opaque `i64` handle back to a context reference. Returns `None` for
/// null/zero handles.
#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

// ---------------------------------------------------------------------------
// init / shutdown
// ---------------------------------------------------------------------------

/// Open a window `width`x`height` and return an opaque `i64` handle, or `0` on
/// failure. Scalar mirror of [`crate::mui_init`] that additionally:
///   * resolves the target file from `argv[1]` (or a scratch file — never the
///     editor's own source);
///   * titles the window with the file's basename;
///   * eagerly loads the file so [`mui_load`] can report its length.
#[no_mangle]
pub extern "C" fn mui_init_s(width: u32, height: u32) -> i64 {
    let path = resolve_target_path();
    let title = format!("{} — Mighty IDE", basename(&path));
    println!("mui_init_s: editing {}", path.display());

    let handle = crate::build_context(width, height, title, Some(path)) as usize as i64;

    // Launch-test hook: with MUI_TERM_AUTOOPEN set, eagerly open the terminal so
    // a headless (non-interactive) run can prove the PTY/grid wiring end-to-end
    // — the terminal otherwise only opens on a Ctrl+` keypress, which a headless
    // run can't deliver. No effect on normal interactive launches.
    if std::env::var_os("MUI_TERM_AUTOOPEN").is_some() {
        let opened = mui_term_open(handle);
        println!("mui_init_s: MUI_TERM_AUTOOPEN -> mui_term_open = {opened}");
        mui_log_terminal(handle);
    }

    // Launch-test hook for autocomplete: with MUI_COMPLETE_PROBE set, run a
    // scripted completion request so a headless run proves the engine wiring
    // (Ctrl+Space can't be delivered non-interactively). See `mui_complete_probe`.
    if std::env::var_os("MUI_COMPLETE_PROBE").is_some() {
        mui_complete_probe(handle);
        mui_log_completion(handle);
    }

    // Launch-test hook for hover/definition: with MUI_NAV_PROBE set, run scripted
    // hover + definition requests (F12 / the hover key can't be delivered
    // non-interactively). See `mui_nav_probe`.
    if std::env::var_os("MUI_NAV_PROBE").is_some() {
        mui_nav_probe(handle);
    }

    // Launch-test hook for undo/redo + format: with MUI_HISTORY_PROBE set, run a
    // scripted edit -> undo -> redo and a format over the active buffer so a
    // headless run proves the wiring (Ctrl+Z/Y and the format chord can't be
    // delivered non-interactively). See `mui_history_probe`.
    if std::env::var_os("MUI_HISTORY_PROBE").is_some() {
        mui_history_probe(handle);
    }

    // Launch-test hook for the command palette: with MUI_PALETTE_PROBE set, open
    // the palette, type a query, and log the filtered count + selected id
    // (Ctrl+Shift+P can't be delivered non-interactively). See `mui_palette_probe`.
    if std::env::var_os("MUI_PALETTE_PROBE").is_some() {
        mui_palette_probe(handle);
    }

    // Launch-test hook for LIVE editing (L28 workaround): with MUI_EDIT_PROBE set,
    // run a scripted insert/newline/backspace against the shim's authoritative
    // text model and log the resulting line count + line lengths — proving the
    // model mutates live (keystrokes can't be delivered non-interactively). See
    // `mui_edit_probe`. The mutated model also renders into a screenshot frame.
    if std::env::var_os("MUI_EDIT_PROBE").is_some() {
        mui_edit_probe(handle);
    }

    // Screenshot/render hook for the command palette: with MUI_PALETTE_AUTOOPEN
    // set, open the palette and LEAVE it open so it renders into the frame
    // (`mui_palette_draw` is a no-op unless the palette is active). Unlike
    // `mui_palette_probe`, this does not cancel — used to capture the palette
    // overlay in a headless screenshot run. No effect on normal launches.
    if std::env::var_os("MUI_PALETTE_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.palette.open();
            // Optionally seed a query so the filtered list is shown.
            if let Some(seed) = std::env::var_os("MUI_PALETTE_AUTOOPEN") {
                let q = seed.to_string_lossy();
                if !q.trim().is_empty() && q != "1" {
                    for ch in q.chars() {
                        ctx.palette.push_char(ch);
                    }
                }
            }
            println!(
                "mui_init_s: MUI_PALETTE_AUTOOPEN -> palette open, count={}",
                ctx.palette.count()
            );
        }
    }

    // Screenshot/render hook for autocomplete: with MUI_COMPLETE_AUTOOPEN set,
    // run a scripted completion request against the active buffer and LEAVE the
    // dropdown open + anchored, so a headless screenshot shows it (the dropdown
    // otherwise only renders while the Mighty loop is `completing`). The env
    // value is the prefix to complete (default `"cl"`). No effect on launches.
    if std::env::var_os("MUI_COMPLETE_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let prefix = std::env::var("MUI_COMPLETE_AUTOOPEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty() && v != "1")
                .unwrap_or_else(|| "cl".to_string());
            // Build active-tab bytes + a newline + the prefix; request there.
            let active = ctx.tabs.active();
            let mut buf: Vec<u8> = Vec::new();
            let n = ctx.tabs.load_len(active);
            for i in 0..(n.max(0) as usize) {
                let b = ctx.tabs.load_byte(active, i);
                if (0..=255).contains(&b) {
                    buf.push(b as u8);
                }
            }
            buf.push(b'\n');
            buf.extend_from_slice(prefix.as_bytes());
            let cursor = buf.len();
            ctx.complete_buf = buf;
            let count = ctx.complete.request(&ctx.complete_buf, cursor, &[]);
            // Anchor near the top of the editor body so the card is fully visible.
            ctx.complete_autoopen = Some((6, prefix.chars().count() as i32 + 8));
            println!("mui_init_s: MUI_COMPLETE_AUTOOPEN -> prefix=\"{prefix}\" candidates={count}");
        }
    }

    handle
}

/// Tear down a context created with [`mui_init_s`].
#[no_mangle]
pub extern "C" fn mui_shutdown_s(handle: i64) {
    if handle != 0 {
        unsafe { crate::mui_shutdown(handle as usize as *mut MuiContext) };
    }
}

// ---------------------------------------------------------------------------
// frame lifecycle
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mui_begin_frame_s(handle: i64) {
    unsafe { crate::mui_begin_frame(handle as usize as *mut MuiContext) };
}

#[no_mangle]
pub extern "C" fn mui_end_frame_s(handle: i64) {
    unsafe { crate::mui_end_frame(handle as usize as *mut MuiContext) };
}

#[no_mangle]
pub extern "C" fn mui_set_clip_s(handle: i64, x: u32, y: u32, w: u32, h: u32) {
    unsafe { crate::mui_set_clip(handle as usize as *mut MuiContext, x, y, w, h) };
}

// ---------------------------------------------------------------------------
// rects
// ---------------------------------------------------------------------------

/// Queue a solid rect; color as four `f32` components in `0.0..=1.0`.
#[no_mangle]
pub extern "C" fn mui_fill_rect_s(
    handle: i64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            w,
            h,
            MuiColor::new(r, g, b, a),
        )
    };
}

// ---------------------------------------------------------------------------
// text staging + draw
// ---------------------------------------------------------------------------

/// Clear the shim-owned text-staging buffer.
#[no_mangle]
pub extern "C" fn mui_text_clear(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.text_stage.clear();
    }
}

/// Append one Unicode scalar value to the text-staging buffer.
#[no_mangle]
pub extern "C" fn mui_text_push(handle: i64, codepoint: u32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(ch) = char::from_u32(codepoint) {
            ctx.text_stage.push(ch);
        }
    }
}

/// Draw the staged text at (`x`,`y`) in the given color, then clear the stage.
#[no_mangle]
pub extern "C" fn mui_text_draw(
    handle: i64,
    x: f32,
    y: f32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        // Take the staged string so the borrow on `ctx.text_stage` ends before
        // we borrow `ctx.text`/`ctx.clip`.
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text.queue(x, y, &s, MuiColor::new(r, g, b, a), clip);
    }
}

/// Draw a text-cursor caret at logical (`line`, `col`) using the shim's own
/// monospace metrics (see [`crate::layout`]). Avoids forcing the Mighty side to
/// convert integer line/col into float pixels, which v0.36 can't do (no
/// int→float cast; see docs/mighty-language-lessons.md L19).
///
/// This legacy entry point assumes no gutter and no scroll (line == screen row,
/// col relative to the left padding). Retained for back-compat; the IDE uses
/// [`mui_draw_cursor_row`].
#[no_mangle]
pub extern "C" fn mui_draw_cursor(handle: i64, line: i32, col: i32, r: f32, g: f32, b: f32, a: f32) {
    let x = layout::PAD + (col.max(0) as f32) * layout::CHAR_W;
    let y = layout::row_y(line);
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            2.0,
            16.0,
            MuiColor::new(r, g, b, a),
        )
    };
}

/// Draw the staged text at logical `line` (column 0) using the shim's metrics,
/// then clear the stage. Legacy (no gutter / no scroll); the IDE uses
/// [`mui_text_draw_row`].
#[no_mangle]
pub extern "C" fn mui_text_draw_line(handle: i64, line: i32, r: f32, g: f32, b: f32, a: f32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let y = layout::row_y(line);
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text
            .queue(layout::PAD, y, &s, MuiColor::new(r, g, b, a), clip);
    }
}

// ---------------------------------------------------------------------------
// gutter + scroll-aware draw (used by the IDE render loop)
// ---------------------------------------------------------------------------

/// Number of whole text rows that fit in the current window height. The IDE
/// uses this to size its viewport for cursor-following scroll. Region-aware:
/// the tab bar (top) and prompt+status bands (bottom) are reserved.
#[no_mangle]
pub extern "C" fn mui_visible_rows(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| {
        let region = layout::region(c.sidebar_visible);
        layout::visible_rows_in(region, c.gpu.height, c.term_open) as i32
    })
}

/// Number of lines in the shim's current `load_buf` (>= 1). Mighty uses this to
/// size the gutter when it draws the buffer via [`mui_draw_buffer_self`].
#[no_mangle]
pub extern "C" fn mui_buf_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| {
        (c.load_buf.iter().filter(|&&b| b == b'\n').count() + 1) as i32
    })
}

/// Draw the editor body — gutter line numbers, source text, and the cursor —
/// directly from the shim's `load_buf` (populated by [`mui_tab_load_into`]).
///
/// This is the rendering counterpart used by the IDE loop. The Mighty side keeps
/// the authoritative edit buffer for editing, but drawing the whole visible
/// window shim-side (one `ctx.text.queue` per line, plus a cursor rect) is both
/// faithful — it issues the SAME GPU rect/text calls — and robust against the
/// v0.36 native-codegen `Vec.push` fragility on the buffer-pull path. `first`
/// is the top visible buffer line; `rows` the visible row count; `cur_line` /
/// `cur_col` the 0-based cursor cell. Colors are fixed to the editor theme.
#[no_mangle]
pub extern "C" fn mui_draw_buffer_self(
    handle: i64,
    first: i32,
    rows: i32,
    cur_line: i32,
    cur_col: i32,
) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let clip = ctx.clip;
    let first = first.max(0) as usize;
    let rows = rows.max(0) as usize;

    // Split the buffer into lines (lossy UTF-8 per line for rendering).
    let src = String::from_utf8_lossy(&ctx.load_buf);
    let lines: Vec<&str> = src.split('\n').collect();
    let total = lines.len().max(1);
    let total_u64 = total as u64;

    let text_x = layout::text_left_in(region, total_u64);
    let gutter_x = region.left + layout::PAD;

    // Theme colors (match the Mighty-side draw_buffer choices).
    let fg = MuiColor::new(0.85, 0.87, 0.9, 1.0);
    let kw = MuiColor::new(0.55, 0.75, 1.0, 1.0); // keywords / leading token
    let gut = MuiColor::new(0.45, 0.48, 0.55, 1.0);

    let last_visible = first + rows;
    for line_idx in first..last_visible {
        if line_idx >= total {
            break;
        }
        let row = (line_idx - first) as i32;
        let y = layout::row_y_in(region, row);
        // Gutter line number (1-based).
        let num = (line_idx + 1).to_string();
        ctx.text.queue(gutter_x, y, &num, gut, clip);
        // Source text. A light syntax cue: color a leading keyword-ish token.
        let text = lines.get(line_idx).copied().unwrap_or("");
        let first_word_end = text
            .char_indices()
            .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_'))
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        let head = &text[..first_word_end];
        const KEYWORDS: &[&str] = &[
            "fn", "let", "mut", "while", "if", "else", "return", "match", "struct", "enum",
            "extern", "effect", "import", "pub", "for", "in", "type", "true", "false",
        ];
        if !head.is_empty() && KEYWORDS.contains(&head) {
            ctx.text.queue(text_x, y, head, kw, clip);
            let rest_x = text_x + (head.chars().count() as f32) * layout::CHAR_W;
            ctx.text.queue(rest_x, y, &text[first_word_end..], fg, clip);
        } else {
            ctx.text.queue(text_x, y, text, fg, clip);
        }
    }

    // Cursor caret, if on a visible row.
    let cl = cur_line.max(0) as usize;
    if cl >= first && cl < last_visible {
        let row = (cl - first) as i32;
        let cx = layout::text_x_in(region, total_u64, cur_col);
        let cy = layout::row_y_in(region, row);
        let handle_ptr = handle as usize as *mut MuiContext;
        unsafe {
            crate::mui_fill_rect(
                handle_ptr,
                cx,
                cy,
                2.0,
                16.0,
                MuiColor::new(0.9, 0.7, 0.2, 1.0),
            );
        }
    }
}

/// Draw the staged text as a buffer line at screen row `row` (0-based from the
/// top of the view), offset right of the line-number gutter sized for
/// `total_lines`. Clears the stage.
#[no_mangle]
pub extern "C" fn mui_text_draw_row(
    handle: i64,
    row: i32,
    total_lines: i32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let region = layout::region(ctx.sidebar_visible);
        let x = layout::text_left_in(region, total_lines.max(1) as u64);
        let y = layout::row_y_in(region, row);
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text.queue(x, y, &s, MuiColor::new(r, g, b, a), clip);
    }
}

/// Draw the staged text (the 1-based line number, staged digit-by-digit) in the
/// gutter at screen row `row`, right-aligned-ish at the left padding. Clears the
/// stage.
#[no_mangle]
pub extern "C" fn mui_gutter_draw_row(handle: i64, row: i32, r: f32, g: f32, b: f32, a: f32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let region = layout::region(ctx.sidebar_visible);
        let x = region.left + layout::PAD;
        let y = layout::row_y_in(region, row);
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text.queue(x, y, &s, MuiColor::new(r, g, b, a), clip);
    }
}

/// Draw the cursor caret at screen `row` and buffer `col`, offset right of the
/// gutter sized for `total_lines`.
#[no_mangle]
pub extern "C" fn mui_draw_cursor_row(
    handle: i64,
    row: i32,
    col: i32,
    total_lines: i32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let region = unsafe { ctx(handle) }.map_or(layout::region(false), |c| {
        layout::region(c.sidebar_visible)
    });
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            2.0,
            16.0,
            MuiColor::new(r, g, b, a),
        )
    };
}

// ---------------------------------------------------------------------------
// mouse-click -> cell (deliverable 4)
// ---------------------------------------------------------------------------

/// Map the last-polled event's pixel `(x, y)` to a buffer line, given the
/// current top line `first_line` and gutter sizing `total_lines`. Stored for
/// readback via [`mui_click_line`] / [`mui_click_col`]. Returns the line.
#[no_mangle]
pub extern "C" fn mui_click_line(
    handle: i64,
    first_line: i32,
    total_lines: i32,
) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let region = layout::region(ctx.sidebar_visible);
    let (line, _) = layout::pixel_to_cell_in(
        region,
        ctx.last_event.x,
        ctx.last_event.y,
        first_line.max(0) as u64,
        total_lines.max(1) as u64,
    );
    line as i32
}

/// Companion to [`mui_click_line`]: the column of the last mouse event's pixel.
#[no_mangle]
pub extern "C" fn mui_click_col(handle: i64, total_lines: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let region = layout::region(ctx.sidebar_visible);
    let (_, col) = layout::pixel_to_cell_in(
        region,
        ctx.last_event.x,
        ctx.last_event.y,
        0,
        total_lines.max(1) as u64,
    );
    col as i32
}

// ---------------------------------------------------------------------------
// event pump (scalar accessors over the last-polled event)
// ---------------------------------------------------------------------------

/// Pump + pop one event, storing it as the "current" event for the scalar
/// accessors below. Returns the event tag (`MUI_EVENT_*`), or `0` when the
/// queue is empty.
#[no_mangle]
pub extern "C" fn mui_poll_event_s(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let mut ev = MuiEvent::none();
    let got = unsafe {
        crate::mui_poll_event(handle as usize as *mut MuiContext, &mut ev as *mut MuiEvent)
    };
    if got {
        ctx.last_event = ev;
        ev.tag as i32
    } else {
        ctx.last_event = MuiEvent::none();
        0
    }
}

#[no_mangle]
pub extern "C" fn mui_event_codepoint(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.last_event.codepoint as i32)
}

#[no_mangle]
pub extern "C" fn mui_event_key(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.last_event.key as i32)
}

#[no_mangle]
pub extern "C" fn mui_event_mods(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.last_event.mods as i32)
}

/// Sign of the last scroll event's vertical delta: `-1` (scroll content up /
/// wheel down), `+1` (wheel up), or `0`. Mighty can't take a float delta and do
/// int math with it (L19), so the shim reduces it to a sign here.
#[no_mangle]
pub extern "C" fn mui_event_scroll_dir(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        let dy = c.last_event.scroll_y;
        if dy > 0.0 {
            1
        } else if dy < 0.0 {
            -1
        } else {
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn mui_event_width(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.last_event.width as i32)
}

#[no_mangle]
pub extern "C" fn mui_event_height(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.last_event.height as i32)
}

// ---------------------------------------------------------------------------
// file I/O — shim-owned (Mighty can't pass paths or byte buffers across FFI)
// ---------------------------------------------------------------------------

/// Read the file at the shim's configured source path into a load buffer.
/// Returns the byte length, or `-1` on error. The path is set with
/// [`mui_set_path_*`] staging fns (or defaults to `src/main.mty`).
#[no_mangle]
pub extern "C" fn mui_load(handle: i64) -> i64 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // The path is always set by `mui_init_s`; never default to the editor's own
    // source (the old footgun). With no path configured, report empty.
    let Some(path) = ctx.file_path.clone() else {
        eprintln!("mui_load: no file path configured");
        ctx.load_buf.clear();
        return 0;
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let n = bytes.len() as i64;
            println!(
                "mui_load: {} ({} bytes, {} lines)",
                path.display(),
                n,
                bytes.iter().filter(|&&b| b == b'\n').count() + 1
            );
            ctx.load_buf = bytes;
            n
        }
        Err(e) => {
            eprintln!("mui_load({}): {e}", path.display());
            ctx.load_buf.clear();
            -1
        }
    }
}

/// Byte at index `i` of the load buffer, or `-1` if out of range.
#[no_mangle]
pub extern "C" fn mui_load_byte(handle: i64, i: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        return -1;
    }
    match ctx.load_buf.get(i as usize) {
        Some(b) => *b as i32,
        None => -1,
    }
}

// ---- path staging (one byte at a time) ----

#[no_mangle]
pub extern "C" fn mui_path_clear(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.path_stage.clear();
    }
}

#[no_mangle]
pub extern "C" fn mui_path_push(handle: i64, byte: u32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.path_stage.push(byte as u8);
    }
}

/// Commit the staged bytes as the source/target file path.
#[no_mangle]
pub extern "C" fn mui_path_commit(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let s = String::from_utf8_lossy(&ctx.path_stage).into_owned();
        ctx.file_path = Some(PathBuf::from(s));
    }
}

// ---- save buffer staging (one byte at a time) ----

#[no_mangle]
pub extern "C" fn mui_save_clear(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.save_buf.clear();
    }
}

#[no_mangle]
pub extern "C" fn mui_save_push(handle: i64, byte: u32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.save_buf.push(byte as u8);
    }
}

/// Write the staged save buffer to the configured file path.
/// Returns `0` on success, `-1` on error.
#[no_mangle]
pub extern "C" fn mui_save_commit(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.file_path.clone() else {
        eprintln!("mui_save_commit: no file path set");
        return -1;
    };
    match std::fs::write(&path, &ctx.save_buf) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mui_save_commit({}): {e}", path.display());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// live diagnostics (scalar getters over the parsed `mty check` result)
// ---------------------------------------------------------------------------

/// Re-run `mty check` on the currently-configured file path, parse the result,
/// store it in the context, and return the diagnostic count. Returns `0` (and
/// clears the stored set) if there is no configured path or the handle is null.
///
/// The IDE calls this after the initial load and after each Ctrl+S save (the
/// on-disk file is current after save), so the markers track the saved file.
#[no_mangle]
pub extern "C" fn mui_diag_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.file_path.clone() else {
        ctx.diags.clear();
        return 0;
    };
    ctx.diags = diagnostics::run_check(&path);
    let n = ctx.diags.len() as i32;
    println!("diags: {n}");
    for d in &ctx.diags {
        let sev = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        println!(
            "  diag[{sev} {}] line={} col={}..{} {}",
            d.code, d.line, d.col_start, d.col_end, d.message
        );
    }
    n
}

/// Number of diagnostics currently stored.
#[no_mangle]
pub extern "C" fn mui_diag_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.diags.len() as i32)
}

/// 0-based line of diagnostic `i`, or `-1` if out of range.
#[no_mangle]
pub extern "C" fn mui_diag_line(handle: i64, i: i32) -> i32 {
    diag_field(handle, i, |d| d.line)
}

/// 0-based start column of diagnostic `i`, or `-1` if out of range.
#[no_mangle]
pub extern "C" fn mui_diag_col_start(handle: i64, i: i32) -> i32 {
    diag_field(handle, i, |d| d.col_start)
}

/// 0-based end column (exclusive) of diagnostic `i`, or `-1` if out of range.
#[no_mangle]
pub extern "C" fn mui_diag_col_end(handle: i64, i: i32) -> i32 {
    diag_field(handle, i, |d| d.col_end)
}

/// Severity of diagnostic `i`: `0` = error, `1` = warning, or `-1` if out of
/// range.
#[no_mangle]
pub extern "C" fn mui_diag_severity(handle: i64, i: i32) -> i32 {
    diag_field(handle, i, |d| d.severity as i32)
}

/// Shared accessor: project a field of diagnostic `i`, returning `-1` for a
/// null handle or out-of-range index.
fn diag_field(handle: i64, i: i32, f: impl Fn(&diagnostics::Diag) -> i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        return -1;
    }
    match ctx.diags.get(i as usize) {
        Some(d) => f(d),
        None => -1,
    }
}

/// Draw a thin diagnostic underline at screen `row` spanning text columns
/// `[col_start, col_end)`, offset right of the gutter sized for `total_lines`.
/// Pixel math lives here because Mighty has no int->float cast (L19). A zero or
/// negative width is widened to one cell so a marker is always visible.
#[no_mangle]
pub extern "C" fn mui_underline_row(
    handle: i64,
    row: i32,
    col_start: i32,
    col_end: i32,
    total_lines: i32,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) {
    let Some(ctx) = (unsafe { ctx(handle) }) else { return };
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col_start);
    let cells = (col_end - col_start).max(1) as f32;
    let w = cells * layout::CHAR_W;
    // Sit the wavy squiggle near the bottom of the row's line box.
    let y = layout::row_y_in(region, row) + layout::LINE_H - 4.0;
    ctx.dl_squiggle(x, y, w, MuiColor::new(r, g, b, a));
}

/// Draw a diagnostic marker in the gutter at screen `row` (a small square at the
/// left padding). Used to flag a row that has a diagnostic even when its span is
/// off to the side.
#[no_mangle]
pub extern "C" fn mui_diag_gutter_mark(handle: i64, row: i32, r: f32, g: f32, b: f32, a: f32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else { return };
    let region = layout::region(ctx.sidebar_visible);
    // A small rounded dot in the gutter flagging the diagnostic row.
    let cy = layout::row_y_in(region, row) + layout::LINE_H * 0.5 - 3.0;
    ctx.dl_round(region.left + 3.0, cy, 6.0, 6.0, 3.0, MuiColor::new(r, g, b, a));
}

/// Draw the bottom status bar: a full-width band across the bottom of the
/// window, green when `error_count == 0` else red. Mighty can't build strings,
/// so the error count itself is rendered by the Mighty side staging digits into
/// the text buffer and drawing them over this bar.
#[no_mangle]
pub extern "C" fn mui_status_bar(handle: i64, error_count: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = layout::LINE_H;
    let y = (h - bar_h).max(0.0);
    let color = if error_count == 0 {
        MuiColor::new(0.16, 0.45, 0.20, 1.0) // green
    } else {
        MuiColor::new(0.55, 0.14, 0.14, 1.0) // red
    };
    unsafe {
        crate::mui_fill_rect(handle as usize as *mut MuiContext, 0.0, y, w, bar_h, color);
    }
}

/// Draw the staged text (the status label/count, staged codepoint-by-codepoint)
/// inside the status bar at the bottom of the window. Clears the stage.
#[no_mangle]
pub extern "C" fn mui_status_draw_text(handle: i64, r: f32, g: f32, b: f32, a: f32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let h = ctx.gpu.height as f32;
        let y = (h - layout::LINE_H + 1.0).max(0.0);
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text
            .queue(layout::PAD, y, &s, MuiColor::new(r, g, b, a), clip);
    }
}

// ---------------------------------------------------------------------------
// Feature 1 — enriched status bar (filename + cursor pos + error count)
// ---------------------------------------------------------------------------

/// Feed the **1-based** cursor `(line, col)` for the status bar. Cheap setter
/// the IDE calls each frame before [`mui_status_render`].
#[no_mangle]
pub extern "C" fn mui_status_set_cursor(handle: i64, line1: i32, col1: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.status_cursor = (line1.max(1), col1.max(1));
    }
}

/// Draw the bottom status bar with the band (green when `error_count == 0`,
/// else red) AND the composed label `"<basename>   Ln L, Col C   N errors"`
/// (or `"... OK"` when clean). The whole string is built and drawn shim-side
/// because Mighty can't compose strings (L17); Mighty just feeds the scalars.
#[no_mangle]
pub extern "C" fn mui_status_render(handle: i64, error_count: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };

    // Full-width elevated band + a thin top divider.
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = 30.0_f32;
    let y = (h - bar_h).max(0.0);
    let chrome = theme::CHROME_FONT_SIZE - 1.0;
    let clip = ctx.clip;
    let scale = chrome / theme::FONT_SIZE;
    let advance = layout::CHAR_W * scale;
    let text_w = |s: &str| s.chars().count() as f32 * advance;

    // Elevated band (subtle vertical gradient) + a thin top divider.
    ctx.dl_grad_v(0.0, y, w, bar_h, 0.0, theme::hex(0x11141b, 1.0), theme::hex(0x0c0e13, 1.0));
    ctx.dl_rect(0.0, y, w, 1.0, theme::BORDER);
    let ty = y + (bar_h - chrome) * 0.5 - 1.0;

    let (line1, col1) = ctx.status_cursor;
    let path = if ctx.file_name.is_empty() {
        "scratch.mty".to_string()
    } else {
        let parent = ctx
            .tree
            .root()
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if parent.is_empty() {
            ctx.file_name.clone()
        } else {
            format!("{parent}/{}", ctx.file_name)
        }
    };

    // ---- left cluster: branch glyph + "main"  |  file path ----
    let mut x = 12.0;
    ctx.text.queue_sized(x, ty, "\u{25C6}", theme::TEXT_4, chrome, clip);
    x += advance + 4.0;
    ctx.text.queue_sized(x, ty, "main", theme::DIM, chrome, clip);
    x += text_w("main") + 12.0;
    ctx.dl_rect(x, y + 7.0, 1.0, bar_h - 14.0, theme::BORDER_SOFT);
    x += 12.0;
    ctx.text.queue_sized(x, ty, &path, theme::DIM, chrome, clip);

    // ---- right cluster (laid out right-to-left) ----
    // Segments: Ln/Col · Spaces:2 · UTF-8 · Mighty(ember dot) · diagnostics chip.
    let mut rx = w - 12.0;

    // Diagnostics chip: a rounded pill (green tint/●N when clean, red on errors).
    let chip_n = error_count.max(0);
    let chip = format!("\u{25CF} {chip_n}");
    let chip_text_w = text_w(&chip);
    let pill_pad = 9.0;
    let pill_w = chip_text_w + 2.0 * pill_pad;
    let pill_h = 18.0;
    rx -= pill_w;
    let (chip_fg, chip_bg, chip_border) = if error_count > 0 {
        (theme::ERROR, theme::hex(0xF2545B, 0.10), theme::hex(0xF2545B, 0.20))
    } else {
        (theme::GREEN, theme::hex(0x5BD6A0, 0.10), theme::hex(0x5BD6A0, 0.20))
    };
    let py = y + (bar_h - pill_h) * 0.5;
    ctx.dl_round(rx, py, pill_w, pill_h, pill_h * 0.5, chip_bg);
    ctx.dl_stroke(rx, py, pill_w, pill_h, pill_h * 0.5, chip_border, 1.0);
    ctx.text.queue_sized(rx + pill_pad, ty, &chip, chip_fg, chrome, clip);
    rx -= 14.0;
    ctx.dl_rect(rx, y + 7.0, 1.0, bar_h - 14.0, theme::BORDER_SOFT);
    rx -= 12.0;

    // "Mighty" with an ember dot.
    let mighty = "Mighty";
    rx -= text_w(mighty);
    ctx.text.queue_sized(rx, ty, mighty, theme::EMBER, chrome, clip);
    rx -= 12.0;
    ctx.dl_round(rx + 2.0, y + bar_h * 0.5 - 3.5, 7.0, 7.0, 3.5, theme::EMBER);
    rx -= 12.0;
    ctx.dl_rect(rx, y + 7.0, 1.0, bar_h - 14.0, theme::BORDER_SOFT);
    rx -= 12.0;

    // "UTF-8".
    let enc = "UTF-8";
    rx -= text_w(enc);
    ctx.text.queue_sized(rx, ty, enc, theme::DIM, chrome, clip);
    rx -= 12.0;
    ctx.dl_rect(rx, y + 7.0, 1.0, bar_h - 14.0, theme::BORDER_SOFT);
    rx -= 12.0;

    // "Spaces: 2".
    let sp = "Spaces: 2";
    rx -= text_w(sp);
    ctx.text.queue_sized(rx, ty, sp, theme::DIM, chrome, clip);
    rx -= 12.0;
    ctx.dl_rect(rx, y + 7.0, 1.0, bar_h - 14.0, theme::BORDER_SOFT);
    rx -= 12.0;

    // "Ln L, Col C".
    let lc = format!("Ln {line1}, Col {col1}");
    rx -= text_w(&lc);
    ctx.text.queue_sized(rx, ty, &lc, theme::DIM, chrome, clip);
}

// ---------------------------------------------------------------------------
// Feature 2 — reusable bottom prompt/input mode (shim-owned query buffer)
// ---------------------------------------------------------------------------

/// Open the bottom prompt for `kind` (1 = goto, 2 = find), clearing any prior
/// query. Unknown kinds are ignored.
#[no_mangle]
pub extern "C" fn mui_prompt_open(handle: i64, kind: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.prompt.open(kind);
    }
}

/// Append one Unicode scalar value to the active prompt's query.
#[no_mangle]
pub extern "C" fn mui_prompt_push(handle: i64, codepoint: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if codepoint >= 0 {
            ctx.prompt.push(codepoint as u32);
        }
    }
}

/// Delete the last query char (no-op on empty).
#[no_mangle]
pub extern "C" fn mui_prompt_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.prompt.backspace();
    }
}

/// Close the prompt and clear its query.
#[no_mangle]
pub extern "C" fn mui_prompt_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.prompt.cancel();
    }
}

/// `1` if a prompt is currently active, else `0`.
#[no_mangle]
pub extern "C" fn mui_prompt_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.prompt.is_active() { 1 } else { 0 })
}

/// Length (chars) of the current query.
#[no_mangle]
pub extern "C" fn mui_prompt_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.prompt.len() as i32)
}

/// The `i`th query char as a codepoint, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_prompt_char(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.prompt.char_at(i as usize))
}

/// Draw the prompt (label + current query) as a band across the bottom of the
/// window, just above the status bar. No-op when no prompt is active.
#[no_mangle]
pub extern "C" fn mui_prompt_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.prompt.is_active() {
        return;
    }
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = layout::LINE_H;
    // Sit the prompt band one row above the status bar.
    let y = (h - 2.0 * bar_h).max(0.0);
    let chrome = theme::CHROME_FONT_SIZE;
    let text = ctx.prompt.display_line();
    let text_y = y + (bar_h - chrome) * 0.5 - 1.0;
    let clip = ctx.clip;
    let handle_ptr = handle as usize as *mut MuiContext;
    let text_x = layout::region(ctx.sidebar_visible).left + layout::PAD + 12.0;
    unsafe {
        // Elevated band + top divider + an ember accent bar on the left edge.
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, bar_h, theme::ELEVATED);
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, 1.0, theme::BORDER);
        crate::mui_fill_rect(handle_ptr, layout::region(ctx.sidebar_visible).left, y, 3.0, bar_h, theme::EMBER);
    }
    ctx.text.queue_sized(text_x, text_y, &text, theme::TEXT, chrome, clip);
}

// ---------------------------------------------------------------------------
// Feature 3 — go-to-line: parse the goto query
// ---------------------------------------------------------------------------

/// Parse the active prompt's query as a 1-based line number, or `-1` if the
/// query is empty / not all digits / overflows. Mighty calls this on Enter.
#[no_mangle]
pub extern "C" fn mui_prompt_goto_target(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.prompt.goto_target())
}

// ---------------------------------------------------------------------------
// Feature 4 — find: stream the buffer in, search shim-side, read matches back
// ---------------------------------------------------------------------------

/// Clear the find search buffer (and prior matches). Mighty calls this before
/// streaming the editor buffer for a fresh search.
#[no_mangle]
pub extern "C" fn mui_find_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.find.reset();
    }
}

/// Append one editor-buffer byte to the find search buffer.
#[no_mangle]
pub extern "C" fn mui_find_push_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.find.push_byte(byte as u32);
    }
}

/// Run the substring search using the active prompt's query as the needle.
/// Returns the match count. Stores matches for `mui_find_*` readback.
#[no_mangle]
pub extern "C" fn mui_find_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let needle = ctx.prompt.query_string();
    ctx.find.run(&needle)
}

/// Number of stored find matches.
#[no_mangle]
pub extern "C" fn mui_find_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.find.count())
}

/// 0-based line of find match `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_find_match_line(handle: i64, i: i32) -> i32 {
    find_match_field(handle, i, |m| m.line)
}

/// 0-based column of find match `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_find_match_col(handle: i64, i: i32) -> i32 {
    find_match_field(handle, i, |m| m.col)
}

/// Byte offset of find match `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_find_match_offset(handle: i64, i: i32) -> i32 {
    find_match_field(handle, i, |m| m.offset as i32)
}

/// Length (bytes) of the find needle (the prompt query), `0` if none.
#[no_mangle]
pub extern "C" fn mui_find_needle_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.prompt.query_string().len() as i32)
}

fn find_match_field(handle: i64, i: i32, f: impl Fn(&crate::prompt::FindMatch) -> i32) -> i32 {
    if i < 0 {
        return -1;
    }
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.find.get(i as usize) {
        Some(m) => f(&m),
        None => -1,
    }
}

/// Draw a subtle highlight rect behind a match span on a visible screen `row`,
/// from text column `col_start` for `len` columns, offset past the gutter sized
/// for `total_lines`. Pixel math lives here (Mighty has no int->float cast, L19).
#[no_mangle]
pub extern "C" fn mui_find_highlight_row(
    handle: i64,
    row: i32,
    col_start: i32,
    len: i32,
    total_lines: i32,
) {
    let region = unsafe { ctx(handle) }.map_or(layout::region(false), |c| {
        layout::region(c.sidebar_visible)
    });
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col_start);
    let cells = len.max(1) as f32;
    let w = cells * layout::CHAR_W;
    let y = layout::row_y_in(region, row) - 2.0;
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            w,
            layout::LINE_H,
            theme::FIND_HIGHLIGHT,
        )
    };
}

// ---------------------------------------------------------------------------
// Multi-file workspace — tab store
// ---------------------------------------------------------------------------

/// Point the shim's file I/O (load / save / diagnostics) at the active tab's
/// path and update the status-bar basename. Called internally after any tab
/// open/switch/close so Ctrl+S and `mty check` follow the active file.
fn sync_active_path(ctx: &mut MuiContext) {
    let active = ctx.tabs.active();
    let path = ctx.tabs.path(active);
    ctx.file_name = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    ctx.file_path = path;
}

/// Number of open tabs (always >= 1).
#[no_mangle]
pub extern "C" fn mui_tab_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.count() as i32)
}

/// Index (0-based) of the active tab.
#[no_mangle]
pub extern "C" fn mui_tab_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active() as i32)
}

/// Open the path staged via `mui_path_*` as a new tab (or switch to it if
/// already open), reading its bytes from disk. Returns the resulting tab index,
/// or -1 on a null handle. The staged path is resolved relative to the tree
/// root when not absolute, so Ctrl+O "foo.mty" opens beside the initial file.
#[no_mangle]
pub extern "C" fn mui_tab_open_path(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let raw = String::from_utf8_lossy(&ctx.path_stage).into_owned();
    let raw = raw.trim();
    if raw.is_empty() {
        return ctx.tabs.active() as i32;
    }
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        ctx.tree.root().join(&candidate)
    };
    let idx = ctx.tabs.open_path(resolved);
    sync_active_path(ctx);
    idx as i32
}

/// Switch the active tab to `idx`. Returns the resulting active index.
#[no_mangle]
pub extern "C" fn mui_tab_switch(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx < 0 {
        return ctx.tabs.active() as i32;
    }
    let a = ctx.tabs.switch(idx as usize);
    sync_active_path(ctx);
    a as i32
}

/// Switch to the next tab (wraps). Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_tab_next(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let a = ctx.tabs.next();
    sync_active_path(ctx);
    a as i32
}

/// Switch to the previous tab (wraps). Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_tab_prev(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let a = ctx.tabs.prev();
    sync_active_path(ctx);
    a as i32
}

/// Close tab `idx`, keeping at least one tab (last close -> empty scratch).
/// Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_tab_close(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx < 0 {
        return ctx.tabs.active() as i32;
    }
    let a = ctx.tabs.close(idx as usize);
    sync_active_path(ctx);
    a as i32
}

/// Map the tab bar pixel x of the last click to a tab index, or -1 if the click
/// is past the last tab. Used to switch tabs by clicking.
#[no_mangle]
pub extern "C" fn mui_tab_index_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // Only clicks within the tab-bar band (top) count.
    if ctx.last_event.y > layout::TAB_BAR_H {
        return -1;
    }
    let i = layout::tab_index_at(ctx.last_event.x) as usize;
    if i < ctx.tabs.count() {
        i as i32
    } else {
        -1
    }
}

// ---- tab byte-swap: store the live Mighty buffer into a slot ----

/// Begin storing the live buffer into tab `idx`: clear its bytes.
#[no_mangle]
pub extern "C" fn mui_tab_store_begin(handle: i64, idx: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs.store_begin(idx as usize);
        }
    }
}

/// Append one byte to tab `idx`'s buffer during a store.
#[no_mangle]
pub extern "C" fn mui_tab_store_byte(handle: i64, idx: i32, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs.store_byte(idx as usize, (byte & 0xff) as u8);
        }
    }
}

/// Commit the editor state (0-based cursor line/col + scroll first line) into
/// tab `idx` after streaming its bytes.
#[no_mangle]
pub extern "C" fn mui_tab_store_commit(
    handle: i64,
    idx: i32,
    cursor_line: i32,
    cursor_col: i32,
    scroll_first: i32,
) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs
                .store_commit(idx as usize, cursor_line, cursor_col, scroll_first);
        }
    }
}

/// Mark tab `idx` dirty (1) or clean (0).
#[no_mangle]
pub extern "C" fn mui_tab_set_dirty(handle: i64, idx: i32, dirty: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs.set_dirty(idx as usize, dirty != 0);
        }
    }
}

/// Byte length of tab `idx`'s buffer (what the Mighty side pulls back), or -1.
#[no_mangle]
pub extern "C" fn mui_tab_load(handle: i64, idx: i32) -> i64 {
    if idx < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.tabs.load_len(idx as usize))
}

/// Copy tab `idx`'s buffer into the shim's `load_buf` and return its byte
/// length (or -1 on a null handle / bad index). The Mighty side then pulls the
/// bytes back through the **two-argument** `mui_load_byte(h, i)` getter
/// (proven-safe under v0.36 native codegen) rather than the three-argument
/// `mui_tab_load_byte(h, idx, i)`, which corrupts a `Vec.push` accumulator when
/// driven from a tight Mighty loop. Used for the initial load + every tab
/// switch so the live editor buffer is always actually populated.
#[no_mangle]
pub extern "C" fn mui_tab_load_into(handle: i64, idx: i32) -> i64 {
    if idx < 0 {
        return -1;
    }
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.get(idx as usize) {
        Some(t) => {
            ctx.load_buf = t.bytes.clone();
            ctx.load_buf.len() as i64
        }
        None => {
            ctx.load_buf.clear();
            -1
        }
    }
}

/// Byte at index `i` of tab `idx`'s buffer, or -1 out of range.
#[no_mangle]
pub extern "C" fn mui_tab_load_byte(handle: i64, idx: i32, i: i64) -> i32 {
    if idx < 0 || i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.tabs.load_byte(idx as usize, i as usize))
}

/// Saved 0-based cursor line of tab `idx`, or 0.
#[no_mangle]
pub extern "C" fn mui_tab_cursor_line(handle: i64, idx: i32) -> i32 {
    if idx < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.get(idx as usize).map_or(0, |t| t.cursor_line))
}

/// Saved 0-based cursor column of tab `idx`, or 0.
#[no_mangle]
pub extern "C" fn mui_tab_cursor_col(handle: i64, idx: i32) -> i32 {
    if idx < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.get(idx as usize).map_or(0, |t| t.cursor_col))
}

/// Saved scroll first-line of tab `idx`, or 0.
#[no_mangle]
pub extern "C" fn mui_tab_scroll(handle: i64, idx: i32) -> i32 {
    if idx < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.get(idx as usize).map_or(0, |t| t.scroll_first))
}

/// Draw the far-left activity rail: the brand mark on top, a column of icon
/// glyphs, and an ember selection bar + ember-tinted active icon for the
/// Explorer (the only active view). Drawn first so the tab bar / sidebar sit to
/// its right. Mighty calls this once per frame.
#[no_mangle]
pub extern "C" fn mui_rail_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let rw = layout::RAIL_W;

    // Rail panel: a top→bottom gradient (deepest bg) + a soft right divider.
    ctx.dl_grad_v(0.0, 0.0, rw, h, 0.0, theme::BG_2, theme::hex(0x080a0d, 1.0));
    ctx.dl_rect(rw - 1.0, 0.0, 1.0, h, theme::BORDER_SOFT);

    // Brand mark: an ember radial-gradient rounded tile near the top, with a
    // soft ember glow-shadow beneath (matches the mockup's `.brand`).
    let bx = (rw - 30.0) * 0.5;
    ctx.dl_shadow(bx, 18.0, 30.0, 30.0, 9.0, theme::hex(0xF4A259, 0.45), 10.0);
    ctx.dl_round(bx, 12.0, 30.0, 30.0, 9.0, theme::EMBER);
    // Radial highlight (warm top-left → ember) inside the tile.
    ctx.dl_glow(
        bx + 9.0,
        18.0,
        26.0,
        theme::hex(0xffd9a8, 1.0),
        theme::hex(0xF4A259, 0.0),
        bx,
        12.0,
        30.0,
        30.0,
    );
    // Brand "M" centered on the mark (dark on ember), UI family.
    ctx.text.queue_ui_sized(
        (rw - 11.0) * 0.5,
        17.0,
        "M",
        theme::hex(0x2a1a0c, 1.0),
        16.0,
        clip,
    );

    // Icon column. Explorer (index 0) is the active view. Glyphs chosen to be
    // present in JetBrains Mono: Explorer ≡, Search ○, Source-control ◆, Run ▷,
    // Agents ✶.
    let icons = ["\u{2261}", "\u{25CB}", "\u{25C6}", "\u{25B7}", "\u{2736}"];
    let icon_top = 64.0;
    let step = 38.0;
    for (i, ic) in icons.iter().enumerate() {
        let cy = icon_top + i as f32 * step;
        let active = i == 0;
        if active {
            // Ember selection bar (rounded) at the left edge of the rail + glow.
            ctx.dl_round(0.0, cy + 6.0, 2.0, 20.0, 1.0, theme::EMBER);
        }
        let color = if active { theme::EMBER } else { theme::TEXT_3 };
        ctx.text
            .queue_sized((rw - 12.0) * 0.5, cy + 4.0, ic, color, 15.0, clip);
    }
    // Settings (gear ⚙ is absent in JetBrains Mono → use ⊙) near the bottom.
    ctx.text.queue_sized(
        (rw - 12.0) * 0.5,
        h - 34.0,
        "\u{2299}",
        theme::TEXT_3,
        15.0,
        clip,
    );
}

/// Draw the breadcrumb bar at the top of the editor body (`path › file › symbol`,
/// the file segment in ember). Sits between the tab bar and the editor field,
/// spanning from the editor's left edge to the right of the window.
#[no_mangle]
pub extern "C" fn mui_breadcrumb_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let w = ctx.gpu.width as f32;
    let handle_ptr = handle as usize as *mut MuiContext;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let top = layout::TAB_BAR_H;
    let bar_h = layout::BREADCRUMB_H;

    // Editor field background under the breadcrumb + a soft bottom divider.
    unsafe {
        crate::mui_fill_rect(handle_ptr, left, top, w - left, bar_h, theme::BG_EDIT);
        crate::mui_fill_rect(handle_ptr, left, top + bar_h - 1.0, w - left, 1.0, theme::BORDER_SOFT);
    }

    let parent = ctx
        .tree
        .root()
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    let file = if ctx.file_name.is_empty() {
        "(scratch)".to_string()
    } else {
        ctx.file_name.clone()
    };

    let ty = top + (bar_h - chrome) * 0.5 - 1.0;
    let scale = chrome / theme::FONT_SIZE;
    let advance = layout::CHAR_W * scale;
    let mut x = left + 16.0;
    let mut put = |ctx: &mut MuiContext, s: &str, color| {
        ctx.text.queue_sized(x, ty, s, color, chrome, clip);
        x += s.chars().count() as f32 * advance;
    };
    put(ctx, &parent, theme::TEXT_3);
    put(ctx, "  \u{203A}  ", theme::TEXT_4);
    put(ctx, &file, theme::EMBER);
    put(ctx, "  \u{203A}  ", theme::TEXT_4);
    put(ctx, "fn main", theme::TEXT_3);
}

/// Draw the tab bar across the top of the window (right of the activity rail):
/// one fixed-width cell per tab with its basename, a file-type dot, an ember
/// underline + dirty dot on the active tab. Mighty calls this once per frame.
#[no_mangle]
pub extern "C" fn mui_tab_bar_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let w = ctx.gpu.width as f32;
    let active = ctx.tabs.active();
    let count = ctx.tabs.count();
    let clip = ctx.clip;
    let bar_h = layout::TAB_BAR_H;
    let chrome = theme::CHROME_FONT_SIZE;
    let rail = layout::RAIL_W;

    // Elevated background band (right of the rail), a subtle top→bottom gradient
    // matching the mockup, + a thin bottom divider.
    ctx.dl_grad_v(rail, 0.0, w - rail, bar_h, 0.0, theme::hex(0x0d1016, 1.0), theme::hex(0x0b0d12, 1.0));
    ctx.dl_rect(rail, bar_h - 1.0, w - rail, 1.0, theme::BORDER);

    for i in 0..count {
        let x = rail + i as f32 * layout::TAB_W;
        let is_active = i == active;
        // Active tab: editor-field bg + a top highlight + a soft ember underline
        // glow (a blurred ember bar) and a crisp ember underline.
        if is_active {
            ctx.dl_rect(x, 0.0, layout::TAB_W, bar_h, theme::BG_EDIT);
            ctx.dl_rect(x, 0.0, layout::TAB_W, 1.0, theme::HIGHLIGHT);
            // Soft, restrained ember underline glow (blurred) + a crisp centered
            // 2px ember bar that reads as fading at the ends.
            ctx.dl_shadow(x + 30.0, bar_h - 2.0, layout::TAB_W - 60.0, 2.0, 1.0, theme::hex(0xF4A259, 0.6), 5.0);
            ctx.dl_round(x + 24.0, bar_h - 2.0, layout::TAB_W - 48.0, 2.0, 1.0, theme::EMBER);
        }
        // Right divider between tabs.
        ctx.dl_rect(x + layout::TAB_W - 1.0, 9.0, 1.0, bar_h - 18.0, theme::BORDER_SOFT);
        if let Some(tab) = ctx.tabs.get(i) {
            let base = tab.basename();
            let is_mty = base.ends_with(".mty");
            let dirty = tab.dirty;
            // File-type dot: ember for .mty, aurora otherwise. A dirty tab shows a
            // dim filled dot instead (matches the mockup's dirty indicator).
            let dot_color = if dirty {
                theme::DIM
            } else if is_mty {
                theme::EMBER
            } else {
                theme::TEAL
            };
            ctx.dl_round(x + 14.0, bar_h * 0.5 - 3.5, 7.0, 7.0, 3.5, dot_color);
            let mut label = base;
            let max_chars = ((layout::TAB_W - 44.0) / layout::CHAR_W).floor() as usize;
            if label.chars().count() > max_chars && max_chars > 1 {
                label = label.chars().take(max_chars - 1).collect::<String>() + "…";
            }
            let fg = if is_active { theme::TEXT } else { theme::TEXT_3 };
            let ty = (bar_h - chrome) * 0.5 - 1.0;
            ctx.text.queue_sized(x + 28.0, ty, &label, fg, chrome, clip);
        }
    }

    // Right-aligned wordmark: "MIGHTY IDE" in the UI family. Approximate the
    // proportional width as ~0.62em for right-alignment (decorative; no layout
    // depends on it).
    let wm = "MIGHTY IDE";
    let wm_w = wm.chars().count() as f32 * chrome * 0.62;
    ctx.text.queue_ui_sized(
        (w - wm_w - 16.0).max(rail),
        (bar_h - chrome) * 0.5 - 1.0,
        wm,
        theme::TEXT_4,
        chrome,
        clip,
    );
}

// ---------------------------------------------------------------------------
// Multi-file workspace — file-tree sidebar
// ---------------------------------------------------------------------------

/// Whether the sidebar is currently shown (1) or hidden (0).
#[no_mangle]
pub extern "C" fn mui_sidebar_visible(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.sidebar_visible { 1 } else { 0 })
}

/// Toggle the sidebar's visibility. Returns the new state (1 shown / 0 hidden).
#[no_mangle]
pub extern "C" fn mui_sidebar_toggle(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.sidebar_visible = !ctx.sidebar_visible;
    if ctx.sidebar_visible {
        1
    } else {
        0
    }
}

/// Re-scan the tree from its root (honoring the current expand state).
#[no_mangle]
pub extern "C" fn mui_tree_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.tree.refresh();
    ctx.tree.count() as i32
}

/// Number of visible tree rows.
#[no_mangle]
pub extern "C" fn mui_tree_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tree.count() as i32)
}

/// `1` if tree row `i` is a directory, `0` if a file, `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_tree_is_dir(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tree
            .get(i as usize)
            .map_or(-1, |r| if r.is_dir { 1 } else { 0 })
    })
}

/// Indentation depth of tree row `i` (0 = top level), or -1 out of range.
#[no_mangle]
pub extern "C" fn mui_tree_depth(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tree.get(i as usize).map_or(-1, |r| r.depth as i32)
    })
}

/// `1` if tree row `i` is an expanded directory, else `0` (-1 out of range).
#[no_mangle]
pub extern "C" fn mui_tree_is_expanded(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tree
            .get(i as usize)
            .map_or(-1, |r| if r.expanded { 1 } else { 0 })
    })
}

/// Toggle expand/collapse of the directory at tree row `i`. Returns the new
/// tree row count (rows shift when a dir expands/collapses).
#[no_mangle]
pub extern "C" fn mui_tree_toggle(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i >= 0 {
        ctx.tree.toggle(i as usize);
    }
    ctx.tree.count() as i32
}

/// Map the last click's pixel y to a tree row index, or -1 if past the last
/// row / not in the sidebar.
#[no_mangle]
pub extern "C" fn mui_tree_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // Only count clicks within the sidebar's x band (right of the rail).
    let sx0 = layout::RAIL_W;
    let sx1 = layout::RAIL_W + layout::SIDEBAR_W;
    if !ctx.sidebar_visible || ctx.last_event.x < sx0 || ctx.last_event.x > sx1 {
        return -1;
    }
    let i = layout::tree_row_at(ctx.last_event.y) as usize;
    if i < ctx.tree.count() {
        i as i32
    } else {
        -1
    }
}

/// Open the file at tree row `i` as a tab (no-op for directories / out of
/// range). Returns the resulting tab index, or -1 if not a file.
#[no_mangle]
pub extern "C" fn mui_tree_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        return -1;
    }
    let Some(row) = ctx.tree.get(i as usize) else {
        return -1;
    };
    if row.is_dir {
        return -1;
    }
    let path = row.path.clone();
    let idx = ctx.tabs.open_path(path);
    sync_active_path(ctx);
    idx as i32
}

/// Draw the file-tree sidebar (background band + one row per visible entry,
/// indented by depth, dirs marked). No-op when the sidebar is hidden. Mighty
/// calls this once per frame after the tab bar.
#[no_mangle]
pub extern "C" fn mui_sidebar_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible {
        return;
    }
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let sx = layout::RAIL_W; // sidebar starts right of the rail
    let sw = layout::SIDEBAR_W;

    // Panel background (subtle vertical gradient) + a right divider.
    ctx.dl_grad_v(sx, 0.0, sw, h, 0.0, theme::hex(0x0a0c11, 1.0), theme::hex(0x090b0f, 1.0));
    ctx.dl_rect(sx + sw - 1.0, layout::TAB_BAR_H, 1.0, h, theme::BORDER);

    // Uppercase tracked section header (a chevron + the workspace folder name).
    let header = ctx
        .tree
        .root()
        .file_name()
        .map(|s| s.to_string_lossy().to_uppercase())
        .unwrap_or_else(|| "EXPLORER".to_string());
    ctx.text.queue_sized(
        sx + layout::PAD + 2.0,
        layout::TAB_BAR_H + layout::SPACE,
        "\u{25BE}",
        theme::TEXT_4,
        chrome,
        clip,
    );
    // Letter-spaced header (insert thin spaces for the "tracked" look),
    // in the distinctive UI family (Bricolage Grotesque).
    let tracked: String = header
        .chars()
        .flat_map(|c| [c, '\u{2009}'])
        .collect();
    ctx.text.queue_ui_sized(
        sx + layout::PAD + 18.0,
        layout::TAB_BAR_H + layout::SPACE,
        &tracked,
        theme::TEXT_3,
        chrome - 1.0,
        clip,
    );

    // File rows start below the header.
    let row_top = layout::TAB_BAR_H + layout::SPACE + layout::LINE_H;
    let active_path = ctx.tabs.active_path();
    let count = ctx.tree.count();
    for i in 0..count {
        // Snapshot the row fields into owned values so the immutable borrow on
        // `ctx.tree` ends before any `ctx.dl_*` / `ctx.text` mutable borrow.
        let (is_dir, expanded, depth, name, selected) = {
            let Some(row) = ctx.tree.get(i) else { continue };
            let selected = !row.is_dir
                && active_path.is_some()
                && row.path == *active_path.as_ref().unwrap();
            (row.is_dir, row.expanded, row.depth, row.display_name(), selected)
        };
        let y = row_top + (i as f32) * layout::LINE_H;
        if y > h {
            break;
        }
        // Selected row: an ember-soft left→right gradient tint + an ember left
        // bar (rounded), matching the mockup's `.row.active`.
        if selected {
            ctx.dl_grad_h(sx + 4.0, y - 1.0, sw - 6.0, layout::LINE_H, 6.0, theme::hex(0xF4A259, 0.16), 0.85);
            ctx.dl_round(sx, y - 1.0, 2.0, layout::LINE_H, 1.0, theme::EMBER);
        }
        let base_indent = sx + layout::PAD + 6.0;
        let indent = base_indent + (depth as f32) * layout::TREE_INDENT;
        // Indent guides for depth.
        for d in 0..depth {
            let gx = base_indent + (d as f32) * layout::TREE_INDENT;
            ctx.dl_rect(gx, y - 1.0, 1.0, layout::LINE_H, theme::hex(0xffffff, 0.05));
        }
        let is_mty = name.ends_with(".mty");
        // Icon: a chevron for dirs, a small glyph for files.
        let (icon, icon_color): (&str, _) = if is_dir {
            (if expanded { "\u{25BE}" } else { "\u{25B8}" }, theme::TEAL)
        } else if is_mty {
            ("\u{25CF}", theme::EMBER)
        } else {
            ("\u{25E6}", theme::TEXT_3)
        };
        ctx.text.queue_sized(indent, y + 3.0, icon, icon_color, chrome, clip);

        let name_x = indent + 16.0;
        let avail = (((sx + sw) - name_x) / layout::CHAR_W).floor() as usize;
        let mut shown = name;
        if shown.chars().count() > avail && avail > 1 {
            shown = shown.chars().take(avail - 1).collect::<String>() + "…";
        }
        let fg = if selected || is_dir {
            theme::TEXT
        } else {
            theme::DIM
        };
        ctx.text.queue_sized(name_x, y + 3.0, &shown, fg, chrome, clip);
    }
}

/// Print the live workspace counts to stdout (tab count, active tab, tree
/// entries). Used as launch-test evidence for the Mighty side, which can't
/// `log` computed integers (L1). No-op on a null handle.
#[no_mangle]
pub extern "C" fn mui_log_workspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        println!(
            "workspace: tab_count={} active={} tree_entries={} sidebar={}",
            ctx.tabs.count(),
            ctx.tabs.active(),
            ctx.tree.count(),
            if ctx.sidebar_visible { "on" } else { "off" }
        );
    }
}

/// Buffer-accumulation probe (L28 / arena-runtime verdict). The Mighty side
/// passes the length of its live `buf: Vec[I32]` (`mty_buf_len`) after the
/// load loop; the shim prints it next to its own byte count for the active tab
/// so a launch test can confirm whether the Mighty Vec actually accumulated.
/// Mighty native `log` can't print computed integers (L1/L23), so this FFI
/// printer is the only way to surface `buf.len()`.
#[no_mangle]
pub extern "C" fn mui_probe_buf_len(handle: i64, mty_buf_len: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let shim_bytes = ctx.load_buf.len();
        println!(
            "probe: mty_buf_len={} shim_load_bytes={} match={}",
            mty_buf_len,
            shim_bytes,
            mty_buf_len as usize == shim_bytes
        );
    } else {
        println!("probe: mty_buf_len={mty_buf_len} (no ctx)");
    }
}

// ---------------------------------------------------------------------------
// Integrated terminal — PTY-backed shell + VT grid (all logic in terminal.rs)
// ---------------------------------------------------------------------------

/// One queued terminal text run: position, string, and resolved RGBA color.
type TermRun = (f32, f32, String, (f32, f32, f32, f32));

/// Grid dimensions for the terminal panel given the current window + sidebar.
fn term_dims(ctx: &MuiContext) -> (usize, usize) {
    let region = layout::region(ctx.sidebar_visible);
    let rows = layout::term_grid_rows(ctx.gpu.height);
    let cols = layout::term_grid_cols(ctx.gpu.width, region);
    (rows, cols)
}

/// Open (spawn if needed) the integrated terminal, sizing its grid/PTY to the
/// current panel. Marks the panel open. Returns `1` if a terminal is running
/// afterwards, `0` on spawn failure or null handle.
#[no_mangle]
pub extern "C" fn mui_term_open(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (rows, cols) = term_dims(ctx);
    if ctx.terminal.is_none() {
        match crate::terminal::Terminal::spawn(rows, cols) {
            Ok(t) => {
                println!("mui_term_open: spawned shell, grid {rows}x{cols}");
                ctx.terminal = Some(t);
            }
            Err(e) => {
                eprintln!("mui_term_open: {e}");
                return 0;
            }
        }
    } else if let Some(t) = ctx.terminal.as_mut() {
        // Re-size to the current panel in case the window changed while closed.
        t.resize(rows, cols);
    }
    ctx.term_open = true;
    1
}

/// Close the terminal panel and tear down the shell (frees the PTY + grid).
/// Marks the panel closed.
#[no_mangle]
pub extern "C" fn mui_term_close(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.term_open = false;
        // Dropping the Terminal kills the child + joins nothing (reader thread
        // exits on EOF). Keep this explicit for clarity.
        ctx.terminal = None;
    }
}

/// `1` if the terminal panel is currently open AND a shell is running, else `0`.
#[no_mangle]
pub extern "C" fn mui_term_running(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.term_open {
        return 0;
    }
    match ctx.terminal.as_mut() {
        Some(t) => i32::from(t.is_alive()),
        None => 0,
    }
}

/// `1` if the terminal panel is open (regardless of shell liveness), else `0`.
/// The Mighty side uses this for focus routing.
#[no_mangle]
pub extern "C" fn mui_term_is_open(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.term_open { 1 } else { 0 })
}

/// Map a named key (`MUI_KEY_*`) + mods to terminal stdin bytes and write them
/// to the PTY. No-op if the terminal is not running. The key->byte mapping lives
/// shim-side (see [`crate::terminal::key_to_bytes`]).
#[no_mangle]
pub extern "C" fn mui_term_key(handle: i64, keycode: i32, mods: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if keycode >= 0 {
                if let Some(bytes) =
                    crate::terminal::key_to_bytes(keycode as u32, mods.max(0) as u32)
                {
                    t.send(&bytes);
                }
            }
        }
    }
}

/// Map a typed codepoint + mods to terminal stdin bytes (Ctrl+letter -> control
/// code, else UTF-8) and write them to the PTY. No-op if not running.
#[no_mangle]
pub extern "C" fn mui_term_send_codepoint(handle: i64, codepoint: i32, mods: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if codepoint >= 0 {
                if let Some(bytes) =
                    crate::terminal::codepoint_to_bytes(codepoint as u32, mods.max(0) as u32)
                {
                    t.send(&bytes);
                }
            }
        }
    }
}

/// Write a single raw byte to the PTY stdin. No-op if not running.
#[no_mangle]
pub extern "C" fn mui_term_send_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if (0..=255).contains(&byte) {
                t.send(&[byte as u8]);
            }
        }
    }
}

/// Drain pending PTY output through the VT parser into the grid. Call once per
/// frame while the panel is open. No-op if not running.
#[no_mangle]
pub extern "C" fn mui_term_pump(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            t.pump();
        }
    }
}

/// Number of rows in the terminal grid (0 if not running).
#[no_mangle]
pub extern "C" fn mui_term_rows(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.terminal.as_ref().map_or(0, |t| t.rows() as i32))
}

/// Number of columns in the terminal grid (0 if not running).
#[no_mangle]
pub extern "C" fn mui_term_cols(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.terminal.as_ref().map_or(0, |t| t.cols() as i32))
}

/// Draw the terminal panel: a background band, then the grid cells (each glyph
/// in its palette color), then a block cursor. Resizes the grid/PTY to the
/// current panel first so it tracks window resizes. No-op if the panel is closed
/// or no shell is running. Mighty calls this once per frame after `mui_term_pump`.
#[no_mangle]
pub extern "C" fn mui_term_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.term_open || ctx.terminal.is_none() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let (panel_rows, panel_cols) = term_dims(ctx);
    let width = ctx.gpu.width;
    let height = ctx.gpu.height;
    let handle_ptr = handle as usize as *mut MuiContext;
    let clip = ctx.clip;

    // Resize the grid + PTY to the current panel before drawing.
    if let Some(t) = ctx.terminal.as_mut() {
        t.resize(panel_rows, panel_cols);
    }

    // Panel geometry.
    let panel_top = layout::term_panel_top(height);
    let panel_h = layout::term_panel_height(height);
    let panel_left = layout::term_panel_left(region);
    let panel_w = (width as f32 - panel_left).max(0.0);

    // Rounded-top panel (a rounded rect whose bottom corners are off-screen) +
    // an ember top accent line + a dim "TERMINAL" header (UI family).
    ctx.dl_round(panel_left, panel_top, panel_w, panel_h + 12.0, 10.0, theme::ELEVATED);
    ctx.dl_rect(panel_left, panel_top, panel_w, 1.0, theme::BORDER);
    ctx.text.queue_ui_sized(
        panel_left + layout::PAD + 4.0,
        panel_top + 4.0,
        "TERMINAL",
        theme::DIM,
        theme::CHROME_FONT_SIZE - 1.0,
        clip,
    );
    let _ = handle_ptr;

    // Snapshot the grid into owned data so the borrow on `ctx.terminal` ends
    // before we borrow `ctx.text`.
    let (rows, cols, cursor, glyphs) = {
        let t = ctx.terminal.as_ref().expect("terminal present");
        let g = t.grid();
        let rows = g.rows();
        let cols = g.cols();
        // Build one (x, y, string, color) run per row, splitting on color change
        // to keep the draw-call count modest while preserving per-cell color.
        let mut runs: Vec<TermRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r);
            let mut col = 0usize;
            while col < cols {
                let fg = g.cell(r, col).fg;
                let start = col;
                let mut s = String::new();
                while col < cols && g.cell(r, col).fg == fg {
                    s.push(g.cell(r, col).ch);
                    col += 1;
                }
                // Trim a trailing run of spaces (don't draw blank tails).
                if !s.trim_end().is_empty() {
                    let x = layout::term_cell_x(region, start);
                    runs.push((x, y, s, crate::terminal::palette_rgba(fg)));
                }
            }
        }
        (rows, cols, g.cursor(), runs)
    };

    for (x, y, s, (r, gc, b, a)) in &glyphs {
        ctx.text
            .queue(*x, *y, s, MuiColor::new(*r, *gc, *b, *a), clip);
    }

    // Block cursor at the grid cursor position (clamped into the panel).
    let (cr, cc) = cursor;
    if cr < rows && cc <= cols {
        let cx = layout::term_cell_x(region, cc);
        let cy = layout::term_cell_y(height, cr);
        unsafe {
            crate::mui_fill_rect(
                handle_ptr,
                cx,
                cy,
                layout::CHAR_W,
                layout::LINE_H - 2.0,
                MuiColor::new(0.85, 0.85, 0.50, 0.6),
            );
        }
    }
}

/// Print the live terminal status to stdout (open?, running?, grid dims). Used
/// as launch-test evidence since the Mighty side can't `log` computed ints (L1).
#[no_mangle]
pub extern "C" fn mui_log_terminal(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let (rows, cols) = ctx
            .terminal
            .as_ref()
            .map_or((0, 0), |t| (t.rows(), t.cols()));
        let running = match ctx.terminal.as_mut() {
            Some(t) => t.is_alive(),
            None => false,
        };
        println!(
            "terminal: open={} running={running} grid={rows}x{cols}",
            ctx.term_open
        );
    }
}

/// Smoke export retained from the spike + a scalar variant for the FFI probe.
#[no_mangle]
pub extern "C" fn mui_smoke_add_s(a: i32, b: i32) -> i32 {
    a + b
}

// ---------------------------------------------------------------------------
// Autocomplete dropdown — shim-side engine (logic in completion.rs)
// ---------------------------------------------------------------------------
//
// Mighty can't pass its edit buffer across FFI (L17), so — like find — it
// streams the buffer in byte-by-byte (`mui_complete_reset` + `_push_byte`),
// then asks for completion at a cursor byte-offset (`mui_complete_request`).
// The shim extracts buffer words, optionally merges mty-lsp semantic labels,
// and owns the candidate list + selection. Mighty reads the accepted text back
// and drives the dropdown via the scalar getters/movers below.

/// Begin streaming the editor buffer for a completion request: clear the buffer.
#[no_mangle]
pub extern "C" fn mui_complete_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete_buf.clear();
    }
}

/// Append one editor-buffer byte to the completion buffer.
#[no_mangle]
pub extern "C" fn mui_complete_push_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete_buf.push((byte & 0xff) as u8);
    }
}

/// Translate a 0-based `(line, col)` to a byte offset in `buf` (col is a byte
/// count from the line start, clamped to the line length). Shim-side because
/// Mighty already tracks the cursor as a byte offset, but the ABI is specified
/// as `(line, col)`; this keeps the two in agreement.
fn line_col_to_offset(buf: &[u8], line: i32, col: i32) -> usize {
    if line < 0 {
        return 0;
    }
    let target = line as usize;
    let mut l = 0usize;
    let mut i = 0usize;
    // Advance to the start of `target`.
    while i < buf.len() && l < target {
        if buf[i] == b'\n' {
            l += 1;
        }
        i += 1;
    }
    // Walk `col` bytes into the line, stopping at its newline / EOF.
    let mut c = 0i32;
    while i < buf.len() && buf[i] != b'\n' && c < col.max(0) {
        i += 1;
        c += 1;
    }
    i
}

/// Build the candidate list for the prefix at the cursor `(line, col)` (0-based)
/// in the streamed buffer. Merges mty-lsp semantic labels (best-effort, with a
/// short timeout; silently empty on any failure) ahead of the buffer words.
/// Returns the candidate count (0 leaves the dropdown closed).
///
/// The LSP query uses the active file's path as the document id and the streamed
/// buffer bytes as the document text, so it reflects the live (unsaved) edit.
#[no_mangle]
pub extern "C" fn mui_complete_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let cursor = line_col_to_offset(&ctx.complete_buf, line, col);

    // Best-effort semantic labels from mty-lsp. The buffer is the live source;
    // the path is just the document id. Any failure -> empty -> buffer words.
    let lsp_labels: Vec<String> = match ctx.file_path.clone() {
        Some(path) => {
            let source = String::from_utf8_lossy(&ctx.complete_buf).into_owned();
            crate::completion::lsp::semantic_labels(&path, &source, line.max(0) as u32, col.max(0) as u32)
        }
        None => Vec::new(),
    };

    let n = ctx
        .complete
        .request(&ctx.complete_buf, cursor, &lsp_labels)
        .min(i32::MAX as usize) as i32;
    println!("complete: candidates={n} (lsp={})", lsp_labels.len());
    n
}

/// Number of candidates currently in the dropdown.
#[no_mangle]
pub extern "C" fn mui_complete_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.count() as i32)
}

/// `1` if the dropdown is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_complete_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.complete.is_active()))
}

/// Index (0-based) of the currently selected candidate.
#[no_mangle]
pub extern "C" fn mui_complete_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.selection() as i32)
}

/// Move the selection by `delta` (positive = down), wrapping.
#[no_mangle]
pub extern "C" fn mui_complete_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete.move_sel(delta);
    }
}

/// Number of chars before the cursor to delete when accepting (the prefix len).
#[no_mangle]
pub extern "C" fn mui_complete_prefix_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.prefix_len() as i32)
}

/// Number of chars in the accepted (selected) candidate's text.
#[no_mangle]
pub extern "C" fn mui_complete_accept_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.accepted_text().chars().count() as i32)
}

/// The `i`th char (codepoint) of the accepted candidate's text, or `-1` out of
/// range. Mighty reads these to insert the accepted text after deleting the
/// prefix.
#[no_mangle]
pub extern "C" fn mui_complete_accept_char(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.complete
            .accepted_text()
            .chars()
            .nth(i as usize)
            .map_or(-1, |ch| ch as i32)
    })
}

/// Close the dropdown and clear its state.
#[no_mangle]
pub extern "C" fn mui_complete_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete.cancel();
    }
}

/// Draw the dropdown near the cursor pixel `(cursor_px_x, cursor_px_y)`. No-op
/// when the dropdown is closed. Mighty passes the cursor's pixel position; the
/// shim positions the box, clamps it on-screen, and highlights the selection.
#[no_mangle]
pub extern "C" fn mui_complete_draw(handle: i64, cursor_px_x: f32, cursor_px_y: f32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    // Split the borrow: `draw` needs `&mut ctx` for both rects + text.
    let engine = std::mem::take(&mut ctx.complete);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, cursor_px_x, cursor_px_y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.complete = engine;
}

/// Compute the cursor's pixel `(x, y)` for the dropdown given the screen `row`
/// and buffer `col`, offset past the gutter sized for `total_lines`. Mighty has
/// no int->float cast (L19), so the pixel math lives here. The result is read
/// back via [`mui_complete_cursor_px_x`] / [`mui_complete_cursor_px_y`] — but to
/// keep the ABI scalar-simple, Mighty instead passes row/col straight to
/// [`mui_complete_draw_at`].
#[no_mangle]
pub extern "C" fn mui_complete_draw_at(
    handle: i64,
    row: i32,
    col: i32,
    total_lines: i32,
) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let engine = std::mem::take(&mut ctx.complete);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, x, y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.complete = engine;
}

/// Print the live completion state to stdout (candidate count, selection,
/// accepted text). Launch-test evidence for headless runs, since Mighty's `log`
/// is literal-only (L23). No-op on a null handle.
#[no_mangle]
pub extern "C" fn mui_log_completion(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        println!(
            "completion: active={} count={} sel={} prefix_len={} accept=\"{}\"",
            ctx.complete.is_active(),
            ctx.complete.count(),
            ctx.complete.selection(),
            ctx.complete.prefix_len(),
            ctx.complete.accepted_text()
        );
    }
}

/// Launch-test hook: with `MUI_COMPLETE_PROBE` set, run a scripted completion
/// request against the active buffer so a headless run proves the engine wiring
/// (which a non-interactive launch can't trigger via Ctrl+Space). The env value
/// is the prefix to seed (default `"l"`); the probe streams the active tab's
/// bytes, appends the prefix at EOF, requests completion there, and logs the
/// result. No effect unless the env var is set.
#[no_mangle]
pub extern "C" fn mui_complete_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(seed) = std::env::var_os("MUI_COMPLETE_PROBE") else {
        return;
    };
    let prefix = seed.to_string_lossy();
    let prefix = if prefix.trim().is_empty() {
        "l".to_string()
    } else {
        prefix.into_owned()
    };
    // Build a synthetic buffer = active tab bytes + a newline + the prefix.
    let active = ctx.tabs.active();
    let mut buf: Vec<u8> = Vec::new();
    let n = ctx.tabs.load_len(active);
    if n > 0 {
        for i in 0..(n as usize) {
            let b = ctx.tabs.load_byte(active, i);
            if (0..=255).contains(&b) {
                buf.push(b as u8);
            }
        }
    }
    buf.push(b'\n');
    buf.extend_from_slice(prefix.as_bytes());
    let cursor = buf.len();
    ctx.complete_buf = buf;
    let lsp_labels: Vec<String> = match ctx.file_path.clone() {
        Some(path) => {
            let source = String::from_utf8_lossy(&ctx.complete_buf).into_owned();
            // Position at the synthetic prefix: last line, col = prefix len.
            let last_line = source.bytes().filter(|&b| b == b'\n').count() as u32;
            crate::completion::lsp::semantic_labels(
                &path,
                &source,
                last_line,
                prefix.chars().count() as u32,
            )
        }
        None => Vec::new(),
    };
    let count = ctx.complete.request(&ctx.complete_buf, cursor, &lsp_labels);
    println!(
        "complete-probe: prefix=\"{prefix}\" candidates={count} lsp={} top=\"{}\"",
        lsp_labels.len(),
        ctx.complete.accepted_text()
    );
}

// ---------------------------------------------------------------------------
// Command palette (Ctrl+Shift+P) — shim-side registry (logic in palette.rs)
// ---------------------------------------------------------------------------
//
// Mirrors the completion dropdown. The command registry + query/filter +
// selection live shim-side (L17/L21: Mighty never holds the command Vec). Mighty
// opens the palette, routes Char/Backspace/Up/Down to it, and on Enter reads the
// selected command id back (`mui_palette_selected_id`) to dispatch to the SAME
// helper the keybinding triggers.

/// Open the command palette: list all commands, select the first, clear the
/// query. Mighty calls this on Ctrl+Shift+P.
#[no_mangle]
pub extern "C" fn mui_palette_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.open();
    }
}

/// Append a typed char (codepoint) to the palette query and refilter. Ignores
/// non-printable / out-of-BMP-as-char values.
#[no_mangle]
pub extern "C" fn mui_palette_push_char(handle: i64, cp: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            ctx.palette.push_char(ch);
        }
    }
}

/// Delete the last char of the palette query and refilter.
#[no_mangle]
pub extern "C" fn mui_palette_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.backspace();
    }
}

/// Number of commands currently matching the query.
#[no_mangle]
pub extern "C" fn mui_palette_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.palette.count() as i32)
}

/// Move the palette selection by `delta` (positive = down), wrapping.
#[no_mangle]
pub extern "C" fn mui_palette_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.move_sel(delta);
    }
}

/// Index (0-based) of the currently selected command in the filtered list.
#[no_mangle]
pub extern "C" fn mui_palette_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.palette.selection() as i32)
}

/// The command id of the current selection, or `-1` when nothing matches. Mighty
/// reads this on Enter and dispatches to the matching command helper.
#[no_mangle]
pub extern "C" fn mui_palette_selected_id(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.palette.selected_id())
}

/// `1` if the palette overlay is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_palette_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.palette.is_active()))
}

/// Close the palette and clear its state (Escape, or after Enter dispatch).
#[no_mangle]
pub extern "C" fn mui_palette_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.cancel();
    }
}

/// Draw the palette as a centered overlay box (query line + filtered commands
/// with right-aligned keybindings, selection highlighted). No-op when closed.
#[no_mangle]
pub extern "C" fn mui_palette_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    // Split the borrow: `draw` needs `&mut ctx` for both rects + text.
    let engine = std::mem::take(&mut ctx.palette);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.palette = engine;
}

/// Print the live palette state to stdout (count, selection, selected id,
/// query). Launch-test evidence for headless runs (Mighty's `log` is
/// literal-only, L23). No-op on a null handle.
#[no_mangle]
pub extern "C" fn mui_log_palette(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        println!(
            "palette: active={} count={} sel={} selected_id={} query=\"{}\"",
            ctx.palette.is_active(),
            ctx.palette.count(),
            ctx.palette.selection(),
            ctx.palette.selected_id(),
            ctx.palette.query()
        );
    }
}

/// Launch-test hook: with `MUI_PALETTE_PROBE` set, open the palette, type the env
/// value as a query, log the filtered count + selected id, then close it — so a
/// headless run proves the palette wiring (Ctrl+Shift+P can't be delivered
/// non-interactively). The env value is the query to type (default `"sa"`). No
/// effect unless the env var is set.
#[no_mangle]
pub extern "C" fn mui_palette_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(seed) = std::env::var_os("MUI_PALETTE_PROBE") else {
        return;
    };
    let query = seed.to_string_lossy();
    let query = if query.trim().is_empty() {
        "sa".to_string()
    } else {
        query.into_owned()
    };
    ctx.palette.open();
    println!("palette-probe: opened, all-commands count={}", ctx.palette.count());
    for ch in query.chars() {
        ctx.palette.push_char(ch);
    }
    println!(
        "palette-probe: query=\"{}\" count={} sel={} selected_id={}",
        query,
        ctx.palette.count(),
        ctx.palette.selection(),
        ctx.palette.selected_id()
    );
    ctx.palette.cancel();
}

// ---------------------------------------------------------------------------
// hover + go-to-definition (sub-project 7): shim-side LSP nav
// ---------------------------------------------------------------------------
//
// Like completion, Mighty streams the live buffer into the shim (it can't pass a
// buffer across FFI, L17), then asks for hover/definition at the cursor
// `(line, col)` (0-based). The shim spawns `mty lsp`, runs the staged handshake
// (L24), fires the request, parses the answer, and owns the result state. Mighty
// reads scalars back: hover availability + a draw call; definition path-match +
// target line/col + an open-target call.

/// Begin streaming the editor buffer for a hover/def request: clear the buffer.
#[no_mangle]
pub extern "C" fn mui_nav_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.nav_buf.clear();
    }
}

/// Append one editor-buffer byte to the nav (hover/def) buffer.
#[no_mangle]
pub extern "C" fn mui_nav_push_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.nav_buf.push((byte & 0xff) as u8);
    }
}

/// Request hover at the cursor `(line, col)` (0-based) over the streamed buffer.
/// Spawns `mty lsp` (best-effort, short timeout), parses the hover markup, wraps
/// it to a small popup, and stores it. Returns `1` if hover text is available,
/// else `0` (and clears any prior popup). Graceful no-op if the buffer is empty
/// or the server is absent.
#[no_mangle]
pub extern "C" fn mui_hover_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.hover.clear();
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => return 0,
    };
    let source = String::from_utf8_lossy(&ctx.nav_buf).into_owned();
    let raw = crate::nav::lsp::request(
        &path,
        &source,
        line.max(0) as u32,
        col.max(0) as u32,
        crate::nav::lsp::Req::Hover,
    );
    let available = match crate::nav::parse_hover_value(&raw) {
        Some(v) => ctx.hover.set_text(&v),
        None => false,
    };
    println!(
        "hover: line={} col={} available={} lines={}",
        line,
        col,
        available,
        ctx.hover.line_count()
    );
    i32::from(available)
}

/// `1` if a hover popup is currently active.
#[no_mangle]
pub extern "C" fn mui_hover_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.hover.is_active()))
}

/// Clear the hover popup.
#[no_mangle]
pub extern "C" fn mui_hover_clear(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.hover.clear();
    }
}

/// Draw the hover popup near the cursor `(row, col)` (screen row + buffer col),
/// offset past the gutter sized for `total_lines`. No-op when no hover is active.
/// Mirrors `mui_complete_draw_at`'s pixel math (Mighty has no int->float, L19).
#[no_mangle]
pub extern "C" fn mui_hover_draw(handle: i64, row: i32, col: i32, total_lines: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.hover.is_active() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let hover = std::mem::take(&mut ctx.hover);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    hover.draw(ctx, x, y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.hover = hover;
}

/// Request go-to-definition at the cursor `(line, col)` (0-based) over the
/// streamed buffer. Spawns `mty lsp`, parses the `Location`, resolves the uri to
/// a path, and stores the target. Returns `1` if a definition location was
/// found, else `0` (and clears any prior target).
#[no_mangle]
pub extern "C" fn mui_def_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.def.clear();
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => return 0,
    };
    let source = String::from_utf8_lossy(&ctx.nav_buf).into_owned();
    let raw = crate::nav::lsp::request(
        &path,
        &source,
        line.max(0) as u32,
        col.max(0) as u32,
        crate::nav::lsp::Req::Definition,
    );
    let found = match crate::nav::parse_definition(&raw) {
        Some((uri, tline, tcol)) => match crate::nav::uri_to_path(&uri) {
            Some(tpath) => {
                ctx.def.set(Some(crate::nav::DefTarget {
                    path: tpath,
                    line: tline,
                    col: tcol,
                }));
                true
            }
            None => false,
        },
        None => false,
    };
    println!("def: line={line} col={col} found={found}");
    i32::from(found)
}

/// `1` if the resolved definition target is in the CURRENTLY ACTIVE file (so
/// Mighty moves the cursor in place rather than opening a tab). `0` if there is
/// no target or it is in another file.
#[no_mangle]
pub extern "C" fn mui_def_path_matches_current(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let current = ctx.file_path.clone();
    i32::from(ctx.def.path_matches(current.as_deref()))
}

/// 0-based target line of the resolved definition, or `-1` if none.
#[no_mangle]
pub extern "C" fn mui_def_target_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.def.target().map_or(-1, |t| t.line.min(i32::MAX as u32) as i32)
    })
}

/// 0-based target column of the resolved definition, or `-1` if none.
#[no_mangle]
pub extern "C" fn mui_def_target_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.def.target().map_or(-1, |t| t.col.min(i32::MAX as u32) as i32)
    })
}

/// Open the resolved definition target's file as a tab (via the existing tab
/// store) and switch to it. Returns the tab index, or `-1` if there is no target
/// / no path. Keeps `file_path` in sync so a follow-up hover/def queries the
/// right document. Mighty calls this only when the target is in another file
/// (after byte-swapping the live buffer into its own slot, as for any tab open).
#[no_mangle]
pub extern "C" fn mui_def_open_target(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let target_path = match ctx.def.target() {
        Some(t) => t.path.clone(),
        None => return -1,
    };
    let idx = ctx.tabs.open_path(target_path);
    sync_active_path(ctx);
    idx as i32
}

/// Launch-test hook: with `MUI_NAV_PROBE` set, run scripted hover + definition
/// requests against a synthetic buffer so a headless run proves the wiring
/// (F12 / the hover key can't be delivered non-interactively). The env value is
/// an optional symbol whose definition+hover to probe (default a small built-in
/// program). Logs the parsed results to stdout. No effect unless the var is set.
#[no_mangle]
pub extern "C" fn mui_nav_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if std::env::var_os("MUI_NAV_PROBE").is_none() {
        return;
    }
    // A self-contained program where `add` is defined on line 0 and used on
    // line 5; hover + definition are probed on the use site (line 5, col 10).
    let source = "fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let r = add(1, 2)\n}\n";
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => {
            println!("nav-probe: no file_path — skipped");
            return;
        }
    };
    let hraw = crate::nav::lsp::request(&path, source, 5, 10, crate::nav::lsp::Req::Hover);
    match crate::nav::parse_hover_value(&hraw) {
        Some(v) => {
            let one_line = v.replace('\n', " ");
            println!("nav-probe: hover=\"{}\"", one_line.trim());
        }
        None => println!("nav-probe: hover=<none>"),
    }
    let draw = crate::nav::lsp::request(&path, source, 5, 10, crate::nav::lsp::Req::Definition);
    match crate::nav::parse_definition(&draw) {
        Some((uri, line, col)) => {
            let resolved = crate::nav::uri_to_path(&uri)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| uri.clone());
            println!("nav-probe: def line={line} col={col} path=\"{resolved}\"");
        }
        None => println!("nav-probe: def=<none>"),
    }
}

// ---------------------------------------------------------------------------
// Feature A — undo / redo (shim-owned history; logic in history.rs)
// ---------------------------------------------------------------------------
//
// The undo/redo history lives shim-side to avoid Mighty managing nested undo
// Vecs (L21). Recording scheme (see history.rs): Mighty streams its FULL
// post-edit buffer after each edit-group via `mui_undo_record_begin` +
// `_byte` + `_commit(cur_line, cur_col)`; the shim diffs against the current top
// and either coalesces a single-char typing run into it or pushes a fresh
// snapshot. `mui_undo_break` marks a typing-run boundary (cursor move, newline,
// delete, save, format, find-jump, tab switch) so one Ctrl+Z undoes a contiguous
// typing run rather than the whole file or one char at a time.
//
// On load / tab switch Mighty calls `mui_undo_seed_*` to install the freshly
// loaded buffer as the per-buffer baseline (history is per active buffer).

/// Begin seeding the baseline buffer (clears history + staging). Mighty streams
/// the freshly loaded buffer, then commits with `mui_undo_seed_commit`.
#[no_mangle]
pub extern "C" fn mui_undo_seed_begin(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_begin();
    }
}

/// Append one byte to the baseline-seed staging buffer.
#[no_mangle]
pub extern "C" fn mui_undo_seed_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_byte((byte & 0xff) as u8);
    }
}

/// Install the staged buffer as the history baseline at cursor `(line, col)`
/// (0-based), clearing all prior undo/redo. Called on load / tab switch.
#[no_mangle]
pub extern "C" fn mui_undo_seed_commit(handle: i64, cur_line: i32, cur_col: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        // `record_begin/byte` staged into the same buffer `seed` consumes via
        // `record_commit`; reuse it by taking the staged bytes through a record
        // path. To keep `seed`'s clear-then-baseline semantics, drain staging here.
        ctx.history.seed_from_staging(cur_line, cur_col);
    }
}

/// Mark a typing-run boundary: the next record starts a fresh undo step rather
/// than coalescing. Mighty calls this on any non-insert action.
#[no_mangle]
pub extern "C" fn mui_undo_break(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.break_run();
    }
}

/// Begin streaming a post-edit buffer for a history record (clears staging).
#[no_mangle]
pub extern "C" fn mui_undo_record_begin(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_begin();
    }
}

/// Append one byte to the record staging buffer.
#[no_mangle]
pub extern "C" fn mui_undo_record_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_byte((byte & 0xff) as u8);
    }
}

/// Commit the staged post-edit buffer as a history record at cursor `(line,
/// col)` (0-based). Coalesces a typing run into the current step or pushes a new
/// one. Returns `1` if a snapshot was recorded/coalesced, `0` if it was a no-op
/// (no byte change).
#[no_mangle]
pub extern "C" fn mui_undo_record_commit(handle: i64, cur_line: i32, cur_col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.history.record_commit(cur_line, cur_col))
}

/// Undo one step. On success the restored buffer becomes the shim's load buffer
/// (so Mighty pulls it via `mui_load_byte`) and the restored cursor is readable
/// via `mui_undo_cursor_line` / `_col`. Returns the restored buffer's byte count,
/// or `-1` if there is nothing to undo.
#[no_mangle]
pub extern "C" fn mui_undo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.history.undo() {
        Some(snap) => {
            let n = snap.bytes.len() as i32;
            ctx.load_buf = snap.bytes;
            ctx.restored_cursor = (snap.cursor_line, snap.cursor_col);
            println!("undo: restored {n} bytes, cursor=({},{})", snap.cursor_line, snap.cursor_col);
            n
        }
        None => {
            println!("undo: nothing to undo");
            -1
        }
    }
}

/// Redo one step (mirror of [`mui_undo`]). Returns the restored buffer's byte
/// count, or `-1` if there is nothing to redo.
#[no_mangle]
pub extern "C" fn mui_redo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.history.redo() {
        Some(snap) => {
            let n = snap.bytes.len() as i32;
            ctx.load_buf = snap.bytes;
            ctx.restored_cursor = (snap.cursor_line, snap.cursor_col);
            println!("redo: restored {n} bytes, cursor=({},{})", snap.cursor_line, snap.cursor_col);
            n
        }
        None => {
            println!("redo: nothing to redo");
            -1
        }
    }
}

/// 0-based cursor line restored by the last `mui_undo` / `mui_redo`.
#[no_mangle]
pub extern "C" fn mui_undo_cursor_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.restored_cursor.0)
}

/// 0-based cursor column restored by the last `mui_undo` / `mui_redo`.
#[no_mangle]
pub extern "C" fn mui_undo_cursor_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.restored_cursor.1)
}

/// Undo steps currently available (states behind the current one).
#[no_mangle]
pub extern "C" fn mui_undo_depth(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.history.undo_depth() as i32)
}

/// Redo steps currently available.
#[no_mangle]
pub extern "C" fn mui_redo_depth(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.history.redo_depth() as i32)
}

// ---------------------------------------------------------------------------
// Feature B — format document (`mty fmt`; logic in format.rs)
// ---------------------------------------------------------------------------

/// Format the currently-configured file in place via `mty fmt <path>`. The
/// Mighty side saves the live buffer to disk FIRST (so the formatter sees the
/// current text), then calls this, then reloads the formatted file (only when
/// this returns `1`).
///
/// Return codes are DISTINCT so the editor can pick the right status message
/// without corrupting data:
///   * `1` — formatted (a `.mty` file, `mty fmt` succeeded) → reload.
///   * `0` — not applicable (the active file is NOT `.mty`) → no-op; the editor
///     shows "format: only .mty supported". This is the L26 guard: `mty fmt`
///     truncates non-`.mty` input to 1 byte, so we never spawn it.
///   * `-1` — failed (a `.mty` file but `mty fmt` errored / exited non-zero).
///
/// `mty fmt` formats in place (confirmed via `mty fmt --help`), so no extra
/// flags are needed.
#[no_mangle]
pub extern "C" fn mui_format_current(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.file_path.clone() else {
        eprintln!("format: no file path configured");
        return -1;
    };
    match crate::format::run_fmt(&path) {
        crate::format::FmtOutcome::Formatted => {
            println!("format: {} -> ok", path.display());
            1
        }
        crate::format::FmtOutcome::NotApplicable => {
            println!("format: {} -> skipped (only .mty supported)", path.display());
            0
        }
        crate::format::FmtOutcome::Failed => {
            println!("format: {} -> failed", path.display());
            -1
        }
    }
}

/// Launch-test hook: with `MUI_HISTORY_PROBE` set, run a scripted edit -> undo
/// -> redo and a format over the active tab's buffer so a headless run proves
/// the undo/redo + format wiring (Ctrl+Z / Ctrl+Y / the format chord can't be
/// delivered non-interactively). Logs buffer lengths at each step. No effect
/// unless the env var is set.
#[no_mangle]
pub extern "C" fn mui_history_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if std::env::var_os("MUI_HISTORY_PROBE").is_none() {
        return;
    }
    // Build the active buffer bytes from the tab store.
    let active = ctx.tabs.active();
    let mut buf: Vec<u8> = Vec::new();
    let n = ctx.tabs.load_len(active);
    if n > 0 {
        for i in 0..(n as usize) {
            let b = ctx.tabs.load_byte(active, i);
            if (0..=255).contains(&b) {
                buf.push(b as u8);
            }
        }
    }
    let base_len = buf.len();

    // Seed the baseline (mirrors the Mighty load path).
    ctx.history.record_begin();
    for b in &buf {
        ctx.history.record_byte(*b);
    }
    ctx.history.seed_from_staging(0, 0);
    println!("history-probe: seed len={base_len} undo_depth={}", ctx.history.undo_depth());

    // Simulate typing two chars (a coalescing run) at EOF, recording after each.
    let mut edited = buf.clone();
    edited.push(b'/');
    ctx.history.break_run(); // first char after seed starts a fresh step
    ctx.history.record(edited.clone(), 0, edited.len() as i32);
    edited.push(b'/');
    ctx.history.record(edited.clone(), 0, edited.len() as i32);
    println!(
        "history-probe: after typing len={} undo_depth={}",
        edited.len(),
        ctx.history.undo_depth()
    );

    // Undo -> should return to the baseline length in one step (typing coalesced).
    match ctx.history.undo() {
        Some(s) => println!("history-probe: undo -> len={} (expect {base_len})", s.bytes.len()),
        None => println!("history-probe: undo -> nothing"),
    }
    // Redo -> back to the edited length.
    match ctx.history.redo() {
        Some(s) => println!("history-probe: redo -> len={} (expect {})", s.bytes.len(), edited.len()),
        None => println!("history-probe: redo -> nothing"),
    }

    // Format the on-disk active file (if any), logging the before/after lengths.
    if let Some(path) = ctx.file_path.clone() {
        let before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let outcome = crate::format::run_fmt(&path);
        let after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        println!("history-probe: format outcome={outcome:?} on-disk {before} -> {after} bytes");
    } else {
        println!("history-probe: format skipped (no file_path)");
    }
}

// ---------------------------------------------------------------------------
// Authoritative editor text model (shim-side; L28 workaround)
// ---------------------------------------------------------------------------
//
// Live editing under v0.36 native `mty build` was impossible: the Mighty
// `Vec[I32]` edit buffer comes back EMPTY (L28 codegen bug). So the editable
// buffer + cursor now live shim-side in the active tab's `TextModel`
// (`editor.rs`), and Mighty drives edits through these scalar ops. Editing is
// genuinely LIVE: `mui_ed_draw` renders directly from this mutated model each
// frame. Move the model back to Mighty once the codegen bug is fixed.

use crate::editor::TextModel;

/// The active tab's editable model (mutable). `None` on a null handle.
#[inline]
unsafe fn model_mut<'a>(handle: i64) -> Option<&'a mut TextModel> {
    ctx(handle).map(|c| c.tabs.active_model_mut())
}

/// Owned snapshot of the model fields [`mui_ed_draw`] needs, taken so the borrow
/// on the model ends before the rect/text draw calls borrow the context again.
struct EdDrawSnapshot {
    total: usize,
    first: usize,
    cur_line: usize,
    cur_col: usize,
    sel: Option<((usize, usize), (usize, usize))>,
    lines_for_view: Vec<(usize, String)>,
}

/// Insert one Unicode scalar at the cursor (a `\n` codepoint splits the line).
#[no_mangle]
pub extern "C" fn mui_ed_insert_char(handle: i64, cp: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            m.insert_char(ch);
        }
    }
}

/// Delete the char before the cursor (joining lines at column 0).
#[no_mangle]
pub extern "C" fn mui_ed_backspace(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.backspace();
    }
}

/// Delete the char at the cursor (joining the next line at end of line).
#[no_mangle]
pub extern "C" fn mui_ed_delete(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.delete();
    }
}

/// Insert a newline at the cursor.
#[no_mangle]
pub extern "C" fn mui_ed_newline(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.newline();
    }
}

/// Move the cursor one step in `dir` (0=L 1=R 2=Up 3=Down 4=Home 5=End).
#[no_mangle]
pub extern "C" fn mui_ed_move(handle: i64, dir: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_cursor(dir);
    }
}

/// Move the cursor to an explicit 0-based `(line, col)`, clamped.
#[no_mangle]
pub extern "C" fn mui_ed_move_to(handle: i64, line: i32, col: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_to(line, col);
    }
}

/// 0-based cursor line of the active model.
#[no_mangle]
pub extern "C" fn mui_ed_cursor_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().cursor_line() as i32)
}

/// 0-based cursor column of the active model.
#[no_mangle]
pub extern "C" fn mui_ed_cursor_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().cursor_col() as i32)
}

/// Number of lines in the active model (>= 1).
#[no_mangle]
pub extern "C" fn mui_ed_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| c.tabs.active_model().line_count() as i32)
}

/// Char length of line `line` (0-based) in the active model.
#[no_mangle]
pub extern "C" fn mui_ed_line_len(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().line_len(line as usize) as i32)
}

/// Set the top visible line (scroll offset) of the active model, clamped.
#[no_mangle]
pub extern "C" fn mui_ed_set_scroll(handle: i64, first: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.set_first_visible(first.max(0) as usize);
    }
}

/// The active model's top visible line (scroll offset).
#[no_mangle]
pub extern "C" fn mui_ed_first_visible(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().first_visible() as i32)
}

/// `1` if the active model has unsaved edits, else `0`.
#[no_mangle]
pub extern "C" fn mui_ed_dirty(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.tabs.active_model().dirty()))
}

/// Mark the active model clean (after a load) or dirty.
#[no_mangle]
pub extern "C" fn mui_ed_set_dirty(handle: i64, dirty: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.set_dirty(dirty != 0);
    }
}

/// Load the active tab's file from disk into the active model (replacing it),
/// resetting the cursor to the top. Returns the byte length, or `-1` on error.
#[no_mangle]
pub extern "C" fn mui_ed_load(handle: i64) -> i64 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // Edit-probe screenshot mode: preserve the scripted-edit model so a headless
    // capture shows the LIVE-edited buffer rather than the on-disk file.
    if ctx.edit_probe_lock {
        return ctx.tabs.active_model().to_bytes().len() as i64;
    }
    let Some(path) = ctx.tabs.active_path() else {
        // No file (scratch tab): keep the empty model.
        ctx.tabs.reload_active(b"");
        return 0;
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let n = bytes.len() as i64;
            ctx.tabs.reload_active(&bytes);
            println!("mui_ed_load: {} ({} bytes)", path.display(), n);
            n
        }
        Err(e) => {
            eprintln!("mui_ed_load({}): {e}", path.display());
            ctx.tabs.reload_active(b"");
            -1
        }
    }
}

/// Write the active model to its tab's file path. Returns `0` on success, `-1`
/// on error (no path / IO failure). Marks the model clean on success.
#[no_mangle]
pub extern "C" fn mui_ed_save(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.tabs.active_path() else {
        eprintln!("mui_ed_save: no file path for active tab");
        return -1;
    };
    let bytes = ctx.tabs.active_model().to_bytes();
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            ctx.tabs.active_model_mut().mark_clean();
            println!("mui_ed_save: {} ({} bytes)", path.display(), bytes.len());
            0
        }
        Err(e) => {
            eprintln!("mui_ed_save({}): {e}", path.display());
            -1
        }
    }
}

/// Stream the active model's bytes into the shim's find engine and run the
/// search using the active prompt's query. Replaces the Mighty byte-push loop —
/// the model is the source of truth. Returns the match count.
#[no_mangle]
pub extern "C" fn mui_ed_find_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let text = ctx.tabs.active_model().as_text();
    ctx.find.reset();
    for b in text.bytes() {
        ctx.find.push_byte(b as u32);
    }
    let needle = ctx.prompt.query_string();
    ctx.find.run(&needle)
}

/// Stream the active model into the completion engine and request completion at
/// the cursor. Returns the candidate count. Replaces the Mighty byte-push loop.
#[no_mangle]
pub extern "C" fn mui_ed_complete_request(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (line, col) = {
        let m = ctx.tabs.active_model();
        (m.cursor_line() as i32, m.cursor_col() as i32)
    };
    let text = ctx.tabs.active_model().as_text();
    ctx.complete_buf = text.into_bytes();
    let cursor = line_col_to_offset(&ctx.complete_buf, line, col);
    let lsp_labels: Vec<String> = match ctx.file_path.clone() {
        Some(path) => {
            let source = String::from_utf8_lossy(&ctx.complete_buf).into_owned();
            crate::completion::lsp::semantic_labels(&path, &source, line.max(0) as u32, col.max(0) as u32)
        }
        None => Vec::new(),
    };
    ctx.complete
        .request(&ctx.complete_buf, cursor, &lsp_labels)
        .min(i32::MAX as usize) as i32
}

/// Accept the selected completion candidate into the active model: delete the
/// prefix chars before the cursor, then insert the accepted text. Returns the
/// accepted text's char length.
#[no_mangle]
pub extern "C" fn mui_ed_complete_accept(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let prefix = ctx.complete.prefix_len();
    let accepted = ctx.complete.accepted_text().to_string();
    let m = ctx.tabs.active_model_mut();
    for _ in 0..prefix {
        m.backspace();
    }
    for ch in accepted.chars() {
        m.insert_char(ch);
    }
    accepted.chars().count() as i32
}

/// Stream the active model into the nav buffer (hover / go-to-definition).
#[no_mangle]
pub extern "C" fn mui_ed_nav_stream(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let text = ctx.tabs.active_model().as_text();
        ctx.nav_buf = text.into_bytes();
    }
}

/// Switch to tab `idx`, syncing the active path. Tab switching is now a plain
/// index change (each tab owns its model), so no byte-swap loop is needed.
/// Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_ed_tab_switch(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx >= 0 {
        ctx.tabs.switch(idx as usize);
        sync_active_path(ctx);
    }
    ctx.tabs.active() as i32
}

/// Map the last mouse-click pixel to a buffer `(line, col)` and move the active
/// model's cursor there. Returns the resulting cursor line. Uses the gutter
/// sizing from the model's own line count.
#[no_mangle]
pub extern "C" fn mui_ed_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let region = layout::region(ctx.sidebar_visible);
    let total = ctx.tabs.active_model().line_count() as u64;
    let first = ctx.tabs.active_model().first_visible() as u64;
    let (line, col) =
        layout::pixel_to_cell_in(region, ctx.last_event.x, ctx.last_event.y, first, total);
    let m = ctx.tabs.active_model_mut();
    m.move_to(line as i32, col as i32);
    m.cursor_line() as i32
}

/// Draw the editor body from the authoritative model: the current-line band,
/// right-aligned gutter numbers (the cursor's line brighter), syntax-colored
/// source text, the translucent selection rect, and the 2px ember caret.
/// `rows` is the visible row count; the model owns the scroll offset.
#[no_mangle]
pub extern "C" fn mui_ed_draw(handle: i64, rows: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let clip = ctx.clip;
    let handle_ptr = handle as usize as *mut MuiContext;
    let rows = rows.max(0) as usize;

    // Snapshot what we need from the model (ends the borrow before text/rect).
    let snap = {
        let m = ctx.tabs.active_model();
        let total = m.line_count();
        let first = m.first_visible();
        let last = (first + rows).min(total);
        EdDrawSnapshot {
            total,
            first,
            cur_line: m.cursor_line(),
            cur_col: m.cursor_col(),
            sel: m.selection_range(),
            lines_for_view: (first..last).map(|i| (i, m.line(i).to_string())).collect(),
        }
    };
    let EdDrawSnapshot {
        total,
        first,
        cur_line,
        cur_col,
        sel,
        lines_for_view,
    } = snap;

    let total_u64 = total.max(1) as u64;
    let text_x = layout::text_left_in(region, total_u64);
    let gutter_right = text_x - layout::GUTTER_GAP; // right edge for right-align
    let chrome = theme::CHROME_FONT_SIZE;
    let win_w = ctx.gpu.width as f32;
    let win_h = ctx.gpu.height as f32;

    // 0) Editor field background (so the atmospheric glow doesn't wash the code).
    //    Spans from the body's left edge to the right, below the breadcrumb and
    //    above the status bar. Slightly translucent so a hint of glow remains.
    {
        let field_top = region.top;
        let field_h = (win_h - 30.0 - field_top).max(0.0); // 30 = status bar
        ctx.dl_rect(
            region.left,
            field_top,
            win_w - region.left,
            field_h,
            MuiColor::new(0.055, 0.063, 0.086, 0.84),
        );
        // A soft left inner shadow against the sidebar edge for depth.
        ctx.dl_grad_h(
            region.left,
            field_top,
            18.0,
            field_h,
            0.0,
            MuiColor::new(0.0, 0.0, 0.0, 0.28),
            1.0,
        );
    }

    // 1) Current-line highlight band (only when the cursor row is visible), with
    //    a soft ember left→clear gradient glow.
    if cur_line >= first && cur_line < first + rows {
        let row = (cur_line - first) as i32;
        let y = layout::row_y_in(region, row);
        let band_w = win_w - region.left;
        // Faint full-row band.
        ctx.dl_rect(region.left, y - 2.0, band_w, layout::LINE_H, theme::CURRENT_LINE);
        // Ember left glow fading across the left ~45% of the band.
        ctx.dl_grad_h(region.left, y - 2.0, band_w, layout::LINE_H, 0.0, MuiColor::new(0.957, 0.635, 0.349, 0.14), 0.45);
    }

    // 2) Selection rects (per visible line within the range).
    if let Some(((l0, c0), (l1, c1))) = sel {
        for (line_idx, line) in &lines_for_view {
            let li = *line_idx;
            if li < l0 || li > l1 {
                continue;
            }
            let line_chars = line.chars().count();
            let s = if li == l0 { c0 } else { 0 };
            // Extend one cell past EOL for multi-line selections to read as a
            // full-line highlight.
            let e = if li == l1 { c1 } else { line_chars + 1 };
            if e <= s {
                continue;
            }
            let row = (li - first) as i32;
            let x = layout::text_x_in(region, total_u64, s as i32);
            let w = (e - s) as f32 * layout::CHAR_W;
            let y = layout::row_y_in(region, row);
            unsafe {
                crate::mui_fill_rect(handle_ptr, x, y - 2.0, w, layout::LINE_H, theme::SELECTION);
            }
        }
    }

    // 3) Gutter numbers + syntax-colored source text.
    for (line_idx, line) in &lines_for_view {
        let li = *line_idx;
        let row = (li - first) as i32;
        let y = layout::row_y_in(region, row);
        // Right-aligned gutter number; the cursor's line is brighter.
        let num = (li + 1).to_string();
        let num_w = num.chars().count() as f32 * layout::CHAR_W * (chrome / theme::FONT_SIZE);
        let gx = (gutter_right - num_w).max(region.left + 2.0);
        let gcol = if li == cur_line {
            theme::GUTTER_ACTIVE
        } else {
            theme::GUTTER
        };
        ctx.text.queue_sized(gx, y + 3.0, &num, gcol, chrome, clip);

        // Syntax spans for the line.
        let spans = crate::syntax::highlight_line(line);
        if spans.is_empty() {
            // Nothing to draw (blank line) — still leave the band.
        } else {
            let chars: Vec<char> = line.chars().collect();
            for sp in spans {
                let frag: String = chars
                    .iter()
                    .skip(sp.start)
                    .take(sp.len)
                    .collect();
                if frag.trim().is_empty() {
                    continue;
                }
                let x = text_x + sp.start as f32 * layout::CHAR_W;
                ctx.text.queue(x, y, &frag, sp.color, clip);
            }
        }
    }

    // 4) Caret — a 2px-wide ember vertical bar with a soft ember glow behind it.
    if cur_line >= first && cur_line < first + rows {
        let row = (cur_line - first) as i32;
        let cx = layout::text_x_in(region, total_u64, cur_col as i32);
        let cy = layout::row_y_in(region, row);
        // Soft blurred glow.
        ctx.dl_shadow(cx, cy + 1.0, 2.0, layout::LINE_H - 6.0, 1.0, theme::hex(0xF4A259, 0.9), 4.0);
        // Crisp 2px rounded caret.
        ctx.dl_round(cx, cy - 1.0, 2.0, layout::LINE_H - 2.0, 1.0, theme::EMBER);
    }
    let _ = handle_ptr;
}

/// Launch-test hook: with `MUI_EDIT_PROBE` set, run a scripted insert, newline,
/// then backspace against the active model and log the resulting line count plus
/// a line's char length, proving the model mutates LIVE under native codegen
/// (where the old Mighty `Vec` buffer stayed empty, L28). The env value is the
/// text to type (default `hello`); the probe types it, inserts a newline, types
/// `world`, then backspaces once. No effect unless the var is set.
#[no_mangle]
pub extern "C" fn mui_edit_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(seed) = std::env::var_os("MUI_EDIT_PROBE") else {
        return;
    };
    let typed = seed.to_string_lossy();
    let typed = if typed.trim().is_empty() || typed == "1" {
        "hello".to_string()
    } else {
        typed.into_owned()
    };

    // Lock out the IDE's initial reload so the edited model is what renders.
    ctx.edit_probe_lock = true;

    let m = ctx.tabs.active_model_mut();
    let before_lines = m.line_count();
    // Move to end of document so the probe appends rather than splitting.
    let last = before_lines.saturating_sub(1);
    m.move_to(last as i32, m.line_len(last) as i32);
    for ch in typed.chars() {
        m.insert_char(ch);
    }
    let after_type_line = m.cursor_line();
    let after_type_len = m.line_len(after_type_line);
    m.newline();
    for ch in "world".chars() {
        m.insert_char(ch);
    }
    let nl_line = m.cursor_line();
    let nl_len_before_bs = m.line_len(nl_line);
    m.backspace();
    let nl_len_after_bs = m.line_len(nl_line);

    println!(
        "edit-probe: typed=\"{typed}\" lines {before_lines}->{} \
         typed_line_len={after_type_len} newline_line_len {nl_len_before_bs}->{nl_len_after_bs} \
         cursor=({},{}) dirty={}",
        m.line_count(),
        m.cursor_line(),
        m.cursor_col(),
        m.dirty()
    );
}

// ---- live-model undo / redo (shim-side snapshots; L28 workaround) ----

/// Cap the undo depth so a long session doesn't grow without bound.
const ED_UNDO_CAP: usize = 256;

/// Reset the editor undo/redo history (called on load / tab switch — history is
/// per active buffer).
#[no_mangle]
pub extern "C" fn mui_ed_undo_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.ed_undo.clear();
        ctx.ed_redo.clear();
    }
}

/// Push the CURRENT active model as an undo checkpoint (call before an edit
/// group). Clears the redo stack. Coalesces no-op duplicates.
#[no_mangle]
pub extern "C" fn mui_ed_undo_record(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let snap = ctx.tabs.active_model().clone();
        // Skip if identical to the most recent checkpoint.
        if let Some(last) = ctx.ed_undo.last() {
            if last.as_text() == snap.as_text() {
                return;
            }
        }
        ctx.ed_undo.push(snap);
        if ctx.ed_undo.len() > ED_UNDO_CAP {
            ctx.ed_undo.remove(0);
        }
        ctx.ed_redo.clear();
    }
}

/// Undo: restore the most recent checkpoint into the active model, pushing the
/// current state onto the redo stack. Returns `1` on success, `0` if nothing to
/// undo.
#[no_mangle]
pub extern "C" fn mui_ed_undo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    match ctx.ed_undo.pop() {
        Some(prev) => {
            let current = ctx.tabs.active_model().clone();
            ctx.ed_redo.push(current);
            *ctx.tabs.active_model_mut() = prev;
            1
        }
        None => 0,
    }
}

/// Redo: restore the most recent redo checkpoint, pushing the current state back
/// onto the undo stack. Returns `1` on success, `0` if nothing to redo.
#[no_mangle]
pub extern "C" fn mui_ed_redo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    match ctx.ed_redo.pop() {
        Some(next) => {
            let current = ctx.tabs.active_model().clone();
            ctx.ed_undo.push(current);
            *ctx.tabs.active_model_mut() = next;
            1
        }
        None => 0,
    }
}
