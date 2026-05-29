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

    let ptr = crate::build_context(width, height, title, Some(path));
    ptr as usize as i64
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
/// uses this to size its viewport for cursor-following scroll.
#[no_mangle]
pub extern "C" fn mui_visible_rows(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| layout::visible_rows(c.gpu.height) as i32)
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
        let x = layout::text_left(total_lines.max(1) as u64);
        let y = layout::row_y(row);
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
        let y = layout::row_y(row);
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text
            .queue(layout::PAD, y, &s, MuiColor::new(r, g, b, a), clip);
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
    let x = layout::text_x(total_lines.max(1) as u64, col);
    let y = layout::row_y(row);
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
    let (line, _) = layout::pixel_to_cell(
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
    let (_, col) = layout::pixel_to_cell(
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
    let x = layout::text_x(total_lines.max(1) as u64, col_start);
    let cells = (col_end - col_start).max(1) as f32;
    let w = cells * layout::CHAR_W;
    // Sit the underline at the bottom of the row's line box.
    let y = layout::row_y(row) + layout::LINE_H - 2.0;
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            w,
            2.0,
            MuiColor::new(r, g, b, a),
        )
    };
}

/// Draw a diagnostic marker in the gutter at screen `row` (a small square at the
/// left padding). Used to flag a row that has a diagnostic even when its span is
/// off to the side.
#[no_mangle]
pub extern "C" fn mui_diag_gutter_mark(handle: i64, row: i32, r: f32, g: f32, b: f32, a: f32) {
    let y = layout::row_y(row) + 4.0;
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            2.0,
            y,
            4.0,
            layout::LINE_H - 8.0,
            MuiColor::new(r, g, b, a),
        )
    };
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

    // Band.
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = layout::LINE_H;
    let y = (h - bar_h).max(0.0);
    let band = if error_count == 0 {
        MuiColor::new(0.16, 0.45, 0.20, 1.0) // green
    } else {
        MuiColor::new(0.55, 0.14, 0.14, 1.0) // red
    };

    // Compose the label text.
    let (line1, col1) = ctx.status_cursor;
    let name = if ctx.file_name.is_empty() {
        "(scratch)"
    } else {
        ctx.file_name.as_str()
    };
    let err_part = match error_count {
        0 => "OK".to_string(),
        1 => "1 error".to_string(),
        n => format!("{n} errors"),
    };
    let label = format!("{name}    Ln {line1}, Col {col1}    {err_part}");

    let text_y = (h - layout::LINE_H + 1.0).max(0.0);
    let fg = if error_count == 0 {
        MuiColor::new(0.85, 0.95, 0.85, 1.0)
    } else {
        MuiColor::new(1.0, 0.9, 0.9, 1.0)
    };

    let clip = ctx.clip;
    let handle_ptr = handle as usize as *mut MuiContext;
    unsafe {
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, bar_h, band);
    }
    ctx.text.queue(layout::PAD, text_y, &label, fg, clip);
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
    let band = MuiColor::new(0.12, 0.18, 0.28, 1.0); // dark blue
    let text = ctx.prompt.display_line();
    let text_y = (y + 1.0).max(0.0);
    let clip = ctx.clip;
    let handle_ptr = handle as usize as *mut MuiContext;
    unsafe {
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, bar_h, band);
    }
    ctx.text.queue(
        layout::PAD,
        text_y,
        &text,
        MuiColor::new(0.9, 0.92, 0.96, 1.0),
        clip,
    );
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
    let x = layout::text_x(total_lines.max(1) as u64, col_start);
    let cells = len.max(1) as f32;
    let w = cells * layout::CHAR_W;
    let y = layout::row_y(row);
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            w,
            layout::LINE_H,
            MuiColor::new(0.35, 0.32, 0.10, 0.85), // subtle amber wash
        )
    };
}

/// Smoke export retained from the spike + a scalar variant for the FFI probe.
#[no_mangle]
pub extern "C" fn mui_smoke_add_s(a: i32, b: i32) -> i32 {
    a + b
}
