//! Scalar `mui_*` ABI for the three developer-workflow features:
//!   * **Run panel** (`mui_run_*`) — run the active file via `mty run` on a
//!     background thread, stream output, surface clickable diagnostics;
//!   * **inline git diff** (`mui_diff_*`) — open/parse/draw a unified diff in the
//!     editor area, read-only;
//!   * **Settings panel** (`mui_settings_*`) — edit live editor preferences.
//!
//! Same shim-owns-everything, scalar-only shape as the rest of the ABI (L17):
//! the IDE opens / drives / reads back via these entry points and calls the
//! `*_draw` each frame; all state + work lives in [`crate::run`] /
//! [`crate::diff`] / [`crate::settingspanel`] / [`crate::settings`].

use crate::layout;
use crate::theme;
use crate::MuiContext;

#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

/// The active tab's file path (absolute), or `None` (scratch / no path).
fn active_path(ctx: &MuiContext) -> Option<std::path::PathBuf> {
    ctx.tabs.active_path()
}

// ===========================================================================
// Feature 1 — Run panel
// ===========================================================================

/// Run the active file via `mty run <path>` on a background thread. Opens the
/// Run panel + clears prior output. Returns `1` if the process spawned, else `0`
/// (no file / spawn error). The IDE then pumps + draws each frame.
#[no_mangle]
pub extern "C" fn mui_run_start(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = active_path(ctx) else {
        ctx.run.open();
        return 0;
    };
    if ctx.run.start(&path) {
        println!("run: started `mty run {}`", path.display());
        1
    } else {
        0
    }
}

/// Stop the running process (best-effort kill). No-op if idle.
#[no_mangle]
pub extern "C" fn mui_run_stop(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.run.stop();
    }
}

/// Toggle the Run panel open/closed (the Run rail icon / Ctrl+Shift+R). Returns
/// `1` if now open, `0` if closed.
#[no_mangle]
pub extern "C" fn mui_run_toggle(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.run.toggle()))
}

/// `1` if the Run panel is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_run_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.run.is_active()))
}

/// `1` while the process is still running, else `0`.
#[no_mangle]
pub extern "C" fn mui_run_running(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.run.is_running()))
}

/// Drain pending output into the line list. Returns `1` if anything changed this
/// frame (the IDE redraws). Call once per frame while the panel is open.
#[no_mangle]
pub extern "C" fn mui_run_pump(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.run.pump()))
}

/// The process exit code, or `-1000` while running / never run (so `-1` can mean
/// "terminated"). The IDE only reads this once `mui_run_running` is `0`.
#[no_mangle]
pub extern "C" fn mui_run_exit_code(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1000, |c| c.run.exit_code().unwrap_or(-1000))
}

/// The last run's wall-clock duration in milliseconds.
#[no_mangle]
pub extern "C" fn mui_run_duration_ms(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.run.duration_ms() as i32)
}

/// Number of output lines.
#[no_mangle]
pub extern "C" fn mui_run_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.run.line_count() as i32)
}

/// Scroll the output by `delta` lines.
#[no_mangle]
pub extern "C" fn mui_run_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.run.scroll(delta);
    }
}

/// `1` if output line `i` is a clickable diagnostic, else `0`.
#[no_mangle]
pub extern "C" fn mui_run_line_clickable(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.run.line(i as usize).map_or(0, |l| i32::from(l.clickable)))
}

/// Map the last click's pixel position to a Run-panel output row index, or `-1`
/// if the click was not on an output row.
#[no_mangle]
pub extern "C" fn mui_run_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.run.is_active() {
        return -1;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let g = run_geom(ctx);
    if x < g.x0 || x > g.x1 || y < g.rows_top {
        return -1;
    }
    let row = ((y - g.rows_top) / g.row_h).floor() as i32;
    let idx = ctx.run.first() as i32 + row;
    if idx >= 0 && (idx as usize) < ctx.run.line_count() {
        idx
    } else {
        -1
    }
}

/// Resolve + record the clicked row `i`'s diagnostic target (file:line:col) and
/// return `1` if it is a clickable line whose file exists; the IDE then reads
/// `mui_run_click_*` and opens/jumps. `0` if not clickable.
#[no_mangle]
pub extern "C" fn mui_run_click_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        return 0;
    }
    let root = ctx.tree.root().to_path_buf();
    let target = {
        let Some(line) = ctx.run.line(i as usize) else {
            return 0;
        };
        if !line.clickable {
            return 0;
        }
        crate::run::resolve_target(&root, &line.file, line.line, line.col)
    };
    let (full, l, c) = target;
    if !full.exists() {
        ctx.run.set_click_target(None);
        return 0;
    }
    // Open the file as a tab now, store the jump target for read-back.
    let _idx = ctx.tabs.open_path(full.clone());
    crate::abi::sync_active_path(ctx);
    ctx.run.set_click_target(Some((full.to_string_lossy().into_owned(), l, c)));
    1
}

/// The 0-based target line of the last `mui_run_click_row`, or `-1`.
#[no_mangle]
pub extern "C" fn mui_run_click_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.run.click_target().map_or(-1, |t| t.1))
}

/// The 0-based target column of the last `mui_run_click_row`, or `-1`.
#[no_mangle]
pub extern "C" fn mui_run_click_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.run.click_target().map_or(-1, |t| t.2))
}

/// The active-tab index after `mui_run_click_row` opened the target file, so the
/// IDE can switch its model. `-1` if no pending click.
#[no_mangle]
pub extern "C" fn mui_run_click_tab(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        if c.run.click_target().is_some() {
            c.tabs.active() as i32
        } else {
            -1
        }
    })
}

/// Geometry of the Run panel (a lower band, like the terminal).
struct RunGeom {
    x0: f32,
    x1: f32,
    y0: f32,
    rows_top: f32,
    row_h: f32,
    panel_h: f32,
}

fn run_geom(ctx: &MuiContext) -> RunGeom {
    let region = layout::region(ctx.sidebar_visible);
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height;
    let panel_h = layout::term_panel_height(h);
    let y0 = layout::term_panel_top(h);
    let header_h = layout::term_header_h();
    RunGeom {
        x0: region.left,
        x1: w,
        y0,
        rows_top: y0 + header_h,
        row_h: layout::LINE_H(),
        panel_h,
    }
}

/// Draw the Run panel as a lower band (header + status line + scrollable output
/// with clickable diagnostics tinted). No-op when closed.
#[no_mangle]
pub extern "C" fn mui_run_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.run.is_active() {
        return;
    }
    use crate::icons;
    let g = run_geom(ctx);
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let line_h = layout::LINE_H();
    let w = g.x1 - g.x0;

    // Panel surface + top divider.
    ctx.dl_rect(g.x0, g.y0, w, g.panel_h, theme::BG_2());
    ctx.dl_rect(g.x0, g.y0, w, 1.0, theme::BORDER());

    // Header band: a play icon + "RUN" + the file basename + status pill.
    let header_h = layout::term_header_h();
    ctx.dl_rect(g.x0, g.y0, w, header_h, theme::BG_1());
    ctx.dl_rect(g.x0, g.y0 + header_h - 1.0, w, 1.0, theme::BORDER_SOFT());
    let hy = g.y0 + (header_h - chrome) * 0.5 - 1.0;
    ctx.dl_icon(g.x0 + 12.0, g.y0 + (header_h - 13.0) * 0.5, 13.0, 13.0, icons::RUN, theme::GREEN(), 1.6, true);
    ctx.text.queue_ui_sized(g.x0 + 32.0, hy, "RUN", theme::DIM(), chrome - 1.0, clip);

    let base = ctx
        .run
        .path()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string();
    ctx.text.queue_ui_sized(g.x0 + 66.0, hy, &base, theme::TEXT_1(), chrome - 1.0, clip);

    // Status pill (right): running / exit code + duration.
    let (status, scol) = if ctx.run.is_running() {
        ("running\u{2026}".to_string(), theme::WARNING())
    } else if let Some(code) = ctx.run.exit_code() {
        if code == 0 {
            (format!("exit 0 \u{00b7} {}ms", ctx.run.duration_ms()), theme::GREEN())
        } else {
            (format!("exit {code} \u{00b7} {}ms", ctx.run.duration_ms()), theme::ERROR())
        }
    } else {
        ("ready".to_string(), theme::TEXT_3())
    };
    let sw = status.chars().count() as f32 * (chrome * 0.5) + 22.0;
    let sx = g.x1 - sw - 12.0;
    let sy = g.y0 + (header_h - 18.0) * 0.5;
    ctx.dl_round(sx, sy, sw, 18.0, 6.0, theme::BG_4());
    ctx.dl_stroke(sx, sy, sw, 18.0, 6.0, theme::BORDER_STRONG(), 1.0);
    ctx.text.queue_ui_sized(sx + 10.0, sy + 3.0, &status, scol, chrome - 2.0, clip);

    // Output rows.
    let first = ctx.run.first();
    let visible = ((g.panel_h - header_h) / line_h).floor().max(0.0) as usize;
    let adv = layout::CHAR_W();
    let count = ctx.run.line_count();
    for vis in 0..visible {
        let idx = first + vis;
        if idx >= count {
            break;
        }
        let (text, clickable, is_error) = {
            let l = ctx.run.line(idx).unwrap();
            (l.text.clone(), l.clickable, l.is_error)
        };
        let y = g.rows_top + vis as f32 * line_h;
        let ty = y + (line_h - chrome) * 0.5 - 1.0;
        let col = if clickable {
            theme::INFO()
        } else if is_error {
            theme::ERROR()
        } else {
            theme::TEXT_1()
        };
        // Clickable diagnostic rows get a faint underline + hover-able tint.
        if clickable {
            ctx.dl_grad_h(g.x0, y, w - 4.0, line_h, 0.0, theme::accent_a(0.08), 0.7);
        }
        // Clip the row text to the panel width.
        let avail = (((g.x1 - 14.0) - (g.x0 + 12.0)) / adv).floor() as usize;
        let mut shown = text;
        if shown.chars().count() > avail && avail > 1 {
            shown = shown.chars().take(avail - 1).collect::<String>() + "\u{2026}";
        }
        ctx.text.queue(g.x0 + 12.0, ty, &shown, col, clip);
    }
}

// ===========================================================================
// Feature 2 — inline git diff view
// ===========================================================================

/// Open the inline diff for `staged`(0/1) side of the file at SCM/explorer row
/// click. The path is the most-recently SCM-selected entry; callers use
/// [`mui_diff_open_row`]. This direct opener diffs the ACTIVE tab's file.
#[no_mangle]
pub extern "C" fn mui_diff_open(handle: i64, staged: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(root) = ctx.scm.root.clone().or_else(|| Some(ctx.tree.root().to_path_buf())) else {
        return 0;
    };
    // Repo-relative path of the active file.
    let Some(abs) = active_path(ctx) else {
        return 0;
    };
    let rel = abs
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs.to_string_lossy().into_owned());
    let blob = crate::diff::run_diff(&root, &rel, staged != 0);
    let n = ctx.diff.open(&rel, staged != 0, &blob);
    println!("diff: {rel} staged={} lines={n}", staged != 0);
    i32::from(n > 0)
}

/// Open the inline diff for SCM changes-list row `i` (worktree side; if empty,
/// falls back to the staged side). Returns `1` if a non-empty diff opened.
#[no_mangle]
pub extern "C" fn mui_diff_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        return 0;
    }
    let (path, staged, root) = {
        let Some(entry) = ctx.scm.get(i as usize) else {
            return 0;
        };
        let Some(root) = ctx.scm.root.clone() else {
            return 0;
        };
        (entry.path.clone(), entry.staged, root)
    };
    // Prefer the side matching the row; fall back to the other if empty.
    let mut blob = crate::diff::run_diff(&root, &path, staged);
    let mut used_staged = staged;
    if blob.trim().is_empty() {
        blob = crate::diff::run_diff(&root, &path, !staged);
        used_staged = !staged;
    }
    let n = ctx.diff.open(&path, used_staged, &blob);
    println!("diff: row {i} {path} staged={used_staged} lines={n}");
    i32::from(n > 0)
}

/// `1` if the diff view is currently shown, else `0`.
#[no_mangle]
pub extern "C" fn mui_diff_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.diff.is_active()))
}

/// Close the diff view (return to editing).
#[no_mangle]
pub extern "C" fn mui_diff_close(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.diff.close();
    }
}

/// Number of parsed diff display lines.
#[no_mangle]
pub extern "C" fn mui_diff_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.diff.line_count() as i32)
}

/// Scroll the diff view by `delta` lines.
#[no_mangle]
pub extern "C" fn mui_diff_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.diff.scroll(delta);
    }
}

/// Kind of diff line `i` (0=hunk 1=context 2=add 3=remove 4=meta), or `-1`.
#[no_mangle]
pub extern "C" fn mui_diff_line_kind(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.diff.line(i as usize).map_or(-1, |l| l.kind as i32))
}

/// Draw the inline diff view in the editor body (read-only). No-op when
/// inactive. Renders a header (file + ± counts), then hunk headers / colored
/// add (green) / remove (red) / context lines with old+new line-number gutters.
#[no_mangle]
pub extern "C" fn mui_diff_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.diff.is_active() {
        return;
    }
    use crate::diff::LineKind;
    use crate::icons;
    let region = layout::region(ctx.sidebar_visible);
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let line_h = layout::LINE_H();
    let adv = layout::CHAR_W();
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;

    // Field background (cover the editor body).
    let field_top = region.top;
    let field_h = (h - 30.0 - field_top).max(0.0);
    ctx.dl_rect(region.left, field_top, w - region.left, field_h, theme::BG_1());

    // Header band: file path + staged badge + ± summary + close hint.
    let head_h = 28.0_f32;
    ctx.dl_rect(region.left, field_top, w - region.left, head_h, theme::BG_2());
    ctx.dl_rect(region.left, field_top + head_h - 1.0, w - region.left, 1.0, theme::BORDER_SOFT());
    let hy = field_top + (head_h - chrome) * 0.5 - 1.0;
    ctx.dl_icon(region.left + 12.0, field_top + (head_h - 14.0) * 0.5, 14.0, 14.0, icons::GIT, theme::ACCENT_BRIGHT(), 1.5, false);
    let path = ctx.diff.path().to_string();
    ctx.text.queue_ui_sized(region.left + 34.0, hy, &path, theme::TEXT(), chrome, clip);
    let side = if ctx.diff.staged() { "Staged" } else { "Working Tree" };
    let adds = ctx.diff.add_count();
    let rems = ctx.diff.remove_count();
    let summary = format!("{side}   +{adds} \u{2212}{rems}   esc to close");
    let sw = summary.chars().count() as f32 * (chrome * 0.5);
    ctx.text.queue_ui_sized((w - 14.0 - sw).max(region.left + 34.0), hy, &summary, theme::TEXT_3(), chrome - 1.0, clip);

    // Diff body. Two-column line-number gutter (old | new) then the text.
    let body_top = field_top + head_h + 4.0;
    let gut_w = 84.0_f32; // room for "old | new"
    let text_x = region.left + gut_w + 8.0;
    let first = ctx.diff.first();
    let visible = ((field_h - head_h - 8.0) / line_h).floor().max(0.0) as usize;
    let count = ctx.diff.line_count();

    for vis in 0..visible {
        let idx = first + vis;
        if idx >= count {
            break;
        }
        let (kind, text, old_no, new_no) = {
            let l = ctx.diff.line(idx).unwrap();
            (l.kind, l.text.clone(), l.old_no, l.new_no)
        };
        let y = body_top + vis as f32 * line_h;
        let ty = y + (line_h - chrome) * 0.5 - 1.0;

        // Row background tint by kind.
        let (bg, fg) = match kind {
            LineKind::Add => (Some(theme::green_wash(0.14)), theme::GREEN()),
            LineKind::Remove => (Some(theme::error_wash(0.14)), theme::ERROR()),
            LineKind::Hunk => (Some(theme::accent_a(0.10)), theme::ACCENT_BRIGHT()),
            LineKind::Meta => (None, theme::TEXT_3()),
            LineKind::Context => (None, theme::TEXT_1()),
        };
        if let Some(c) = bg {
            ctx.dl_rect(region.left, y, w - region.left, line_h, c);
        }

        if kind == LineKind::Hunk {
            // Hunk header spans the row (its own text already includes @@...@@).
            ctx.text.queue(region.left + 8.0, ty, &text, fg, clip);
            continue;
        }

        // Old / new line-number gutter (dim; '·' for the missing side).
        let old_s = if old_no >= 0 { old_no.to_string() } else { "\u{00b7}".to_string() };
        let new_s = if new_no >= 0 { new_no.to_string() } else { "\u{00b7}".to_string() };
        ctx.text.queue_sized(region.left + 6.0, y + 3.0, &old_s, theme::GUTTER(), chrome, clip);
        ctx.text.queue_sized(region.left + 44.0, y + 3.0, &new_s, theme::GUTTER(), chrome, clip);
        // +/- marker glyph in the small gap before the text.
        let marker = match kind {
            LineKind::Add => "+",
            LineKind::Remove => "\u{2212}",
            _ => " ",
        };
        ctx.text.queue(region.left + gut_w - 6.0, ty, marker, fg, clip);

        // Diff line text (clipped to the window width).
        let avail = (((w - 12.0) - text_x) / adv).floor() as usize;
        let mut shown = text;
        if shown.chars().count() > avail && avail > 1 {
            shown = shown.chars().take(avail - 1).collect::<String>() + "\u{2026}";
        }
        let text_col = if kind == LineKind::Context { theme::TEXT_1() } else { fg };
        ctx.text.queue(text_x, ty, &shown, text_col, clip);
    }

    // Gutter divider.
    ctx.dl_rect(region.left + gut_w, body_top, 1.0, field_h - head_h - 8.0, theme::BORDER_SOFT());
}

// ===========================================================================
// Feature 3 — Settings panel
// ===========================================================================

/// Open the Settings panel (Preferences: Settings / gear). Returns `1`.
#[no_mangle]
pub extern "C" fn mui_settings_open(handle: i64) -> i32 {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.settings_panel.open();
        1
    } else {
        0
    }
}

/// Close the Settings panel.
#[no_mangle]
pub extern "C" fn mui_settings_close(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.settings_panel.close();
    }
}

/// `1` if the Settings panel is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_settings_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.settings_panel.is_active()))
}

/// Move the highlighted settings row by `delta` (wrapping).
#[no_mangle]
pub extern "C" fn mui_settings_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.settings_panel.move_sel(delta);
    }
}

/// Selected settings row index.
#[no_mangle]
pub extern "C" fn mui_settings_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.settings_panel.selection() as i32)
}

/// Adjust the selected numeric/theme row by `delta` (font px, tab spaces, or
/// theme cycle). Applies live + persists.
#[no_mangle]
pub extern "C" fn mui_settings_adjust(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.settings_panel.adjust(delta);
    }
}

/// Toggle / activate the selected row (boolean flip, theme cycle, numeric +1).
/// Applies live + persists.
#[no_mangle]
pub extern "C" fn mui_settings_toggle(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.settings_panel.toggle();
    }
}

/// Draw the Settings panel overlay (no-op when closed).
#[no_mangle]
pub extern "C" fn mui_settings_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.settings_panel.is_active() {
        return;
    }
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let panel = std::mem::take(&mut ctx.settings_panel);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    panel.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.settings_panel = panel;
}

// ---------------------------------------------------------------------------
// Settings getters the renderer reads (live editor metrics + prefs).
// ---------------------------------------------------------------------------

/// Live editor font size (px, rounded).
#[no_mangle]
pub extern "C" fn mui_pref_font_size(_handle: i64) -> i32 {
    crate::settings::font_size().round() as i32
}

/// Live tab width (spaces).
#[no_mangle]
pub extern "C" fn mui_pref_tab_width(_handle: i64) -> i32 {
    crate::settings::tab_width()
}

/// `1` if word wrap is on, else `0`.
#[no_mangle]
pub extern "C" fn mui_pref_word_wrap(_handle: i64) -> i32 {
    i32::from(crate::settings::word_wrap())
}

/// `1` if the minimap is shown, else `0`.
#[no_mangle]
pub extern "C" fn mui_pref_minimap(_handle: i64) -> i32 {
    i32::from(crate::settings::minimap())
}
