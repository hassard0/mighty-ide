//! Scalar `mui_dbg_*` / `mui_bp_*` ABI for the debugger + its Vivid-Modern UI.
//!
//! Same shim-owns-everything, scalar-only shape as the rest of the IDE ABI
//! (L17): Mighty starts / steps / stops a session, toggles gutter breakpoints,
//! reads back the run state + current stop line + call stack + variables, pumps
//! the session each frame, and draws the debug view + gutter decorations. All
//! the work + state lives in [`crate::dap`].
//!
//! The debug view is a sidebar panel (rail slot [`crate::PANEL_DEBUG`], the bug
//! icon) styled like the Source-Control / Search panels: a **debug toolbar**
//! (continue / step-over / step-into / step-out / stop), a **Call Stack**
//! section (frame name + file:line, click to select), a **Variables** section
//! (name : value rows), and a small **Debug Console** at the bottom (reuses the
//! `output`-event text). The stopped line is painted in the editor by
//! [`mui_dbg_draw`] (a distinct band + a gutter arrow) and breakpoints by
//! [`mui_bp_gutter_draw`].

use crate::ffi::MuiColor;
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

/// The active tab's file path string (absolute), or empty.
fn active_path_str(ctx: &MuiContext) -> String {
    ctx.tabs
        .active_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ===========================================================================
// Session lifecycle (F5 / Shift+F5) + stepping (F10 / F11 / Shift+F11)
// ===========================================================================

/// Start a debug session for the active file (F5 with no session), or
/// Continue if already stopped. Opens the debug view. Returns the run state
/// code (see [`mui_dbg_state`]).
#[no_mangle]
pub extern "C" fn mui_dbg_start(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    use crate::dap::DebugState;
    match ctx.dbg.state() {
        DebugState::Stopped => {
            ctx.dbg.continue_();
        }
        DebugState::Running => { /* already running; nothing to do */ }
        DebugState::Idle | DebugState::Terminated => {
            let Some(path) = ctx.tabs.active_path() else {
                ctx.dbg.set_open(true);
                return ctx.dbg.state().as_i32();
            };
            ctx.active_panel = crate::PANEL_DEBUG;
            ctx.sidebar_visible = true;
            let ok = ctx.dbg.start(&path);
            println!("dbg: start {} -> {ok}", path.display());
        }
    }
    ctx.dbg.state().as_i32()
}

/// F5 / Continue (only meaningful when stopped). Returns the run state.
#[no_mangle]
pub extern "C" fn mui_dbg_continue(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.dbg.continue_();
    ctx.dbg.state().as_i32()
}

/// Shift+F5 / Stop: disconnect the session.
#[no_mangle]
pub extern "C" fn mui_dbg_stop(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.dbg.stop();
    }
}

/// F10 / step over (`next`).
#[no_mangle]
pub extern "C" fn mui_dbg_step_over(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.dbg.step_over();
    }
}

/// F11 / step into (`stepIn`).
#[no_mangle]
pub extern "C" fn mui_dbg_step_into(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.dbg.step_into();
    }
}

/// Shift+F11 / step out (`stepOut`).
#[no_mangle]
pub extern "C" fn mui_dbg_step_out(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.dbg.step_out();
    }
}

// ===========================================================================
// Debug-view open/close + run-state read-back
// ===========================================================================

/// Toggle the debug view (the bug rail icon). Returns `1` if now open.
#[no_mangle]
pub extern "C" fn mui_dbg_toggle(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let open = ctx.dbg.toggle_open();
    if open {
        ctx.active_panel = crate::PANEL_DEBUG;
        ctx.sidebar_visible = true;
    }
    i32::from(open)
}

/// `1` if the debug view is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_dbg_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.dbg.is_open()))
}

/// Coarse run state: 0 idle, 1 running, 2 stopped, 3 terminated.
#[no_mangle]
pub extern "C" fn mui_dbg_state(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.dbg.state().as_i32())
}

/// Drain pending adapter events into the model. Returns `1` if anything changed
/// this frame (so the IDE redraws + may jump the editor). Call once per frame.
#[no_mangle]
pub extern "C" fn mui_dbg_pump(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.dbg.pump()))
}

/// `1` if a fresh stop arrived since the last call (consume-once): the IDE then
/// switches to / jumps the editor to [`mui_dbg_cur_line`] in the current file.
#[no_mangle]
pub extern "C" fn mui_dbg_take_stopped(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.dbg.take_just_stopped()))
}

/// The 0-based current stop line (the selected frame's line), or `-1`.
#[no_mangle]
pub extern "C" fn mui_dbg_cur_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        if c.dbg.state() == crate::dap::DebugState::Stopped {
            c.dbg.cur_line()
        } else {
            -1
        }
    })
}

/// `1` if the current stop file matches the active tab's path (so the IDE knows
/// whether the stopped-line highlight applies to the visible buffer).
#[no_mangle]
pub extern "C" fn mui_dbg_cur_file_matches(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.dbg.state() != crate::dap::DebugState::Stopped {
        return 0;
    }
    let cur = ctx.dbg.cur_file().replace('\\', "/");
    let active = active_path_str(ctx).replace('\\', "/");
    i32::from(!cur.is_empty() && (cur == active || cur.ends_with(&active) || active.ends_with(&cur)))
}

// ===========================================================================
// Gutter breakpoints
// ===========================================================================

/// Toggle a breakpoint on (0-based) `line` of the active file. If a session is
/// live, the updated breakpoints are re-sent to the adapter. Returns `1` if the
/// breakpoint is now set, `0` if cleared.
#[no_mangle]
pub extern "C" fn mui_bp_toggle(handle: i64, line: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let file = active_path_str(ctx);
    if file.is_empty() {
        return 0;
    }
    let now_on = ctx.dbg.toggle_breakpoint(&file, line);
    // Live session: re-push breakpoints for the program file.
    if ctx.dbg.state() != crate::dap::DebugState::Idle
        && ctx.dbg.state() != crate::dap::DebugState::Terminated
    {
        ctx.dbg.resend_breakpoints();
    }
    println!("bp: {file}:{} -> {now_on}", line + 1);
    i32::from(now_on)
}

/// `1` if there's a breakpoint on (0-based) `line` of the active file.
#[no_mangle]
pub extern "C" fn mui_bp_has(handle: i64, line: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let file = active_path_str(ctx);
    i32::from(ctx.dbg.has_breakpoint(&file, line))
}

/// Number of breakpoints on the program file.
#[no_mangle]
pub extern "C" fn mui_bp_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.dbg.breakpoint_count() as i32)
}

/// 1-based DAP breakpoint line `i` of the program file, or `-1`.
#[no_mangle]
pub extern "C" fn mui_bp_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.dbg.breakpoint_line_at(i as usize))
}

/// Map the last click's pixel position to a gutter breakpoint toggle: returns
/// the 0-based buffer line if the click landed in the gutter of the active
/// editor (so Mighty can call [`mui_bp_toggle`]), else `-1`. `first_line` is the
/// top visible line; `total_lines` sizes the gutter.
#[no_mangle]
pub extern "C" fn mui_bp_gutter_click(handle: i64, first_line: i32, total_lines: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let region = layout::region(ctx.sidebar_visible);
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    // The gutter spans from the body's left edge to the text column.
    let text_x = layout::text_left_in(region, total_lines.max(1) as u64);
    if x < region.left || x >= text_x {
        return -1;
    }
    if y < region.top {
        return -1;
    }
    let (line, _) =
        layout::pixel_to_cell_in(region, region.left + 1.0, y, first_line.max(0) as u64, total_lines.max(1) as u64);
    line as i32
}

// ===========================================================================
// Call stack + variables read-back
// ===========================================================================

/// Number of call-stack frames (valid while stopped).
#[no_mangle]
pub extern "C" fn mui_dbg_stack_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.dbg.stack_count() as i32)
}

/// The selected call-stack frame index.
#[no_mangle]
pub extern "C" fn mui_dbg_sel_frame(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.dbg.selected_frame() as i32)
}

/// 0-based line of frame `i`'s source location (1-based DAP line minus 1), or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_frame_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg.frame(i as usize).map_or(-1, |f| (f.line as i32 - 1).max(0))
    })
}

/// Length (chars) of frame `i`'s function name, or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_frame_name_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg.frame(i as usize).map_or(-1, |f| f.name.chars().count() as i32)
    })
}

/// `j`-th char (codepoint) of frame `i`'s name, or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_frame_name_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg
            .frame(i as usize)
            .and_then(|f| f.name.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

/// Select call-stack frame `i` (updates variables + the editor jump target).
/// Returns the resulting 0-based line of that frame, or -1 if out of range.
#[no_mangle]
pub extern "C" fn mui_dbg_select_frame(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 || !ctx.dbg.select_frame(i as usize) {
        return -1;
    }
    ctx.dbg.cur_line()
}

/// Number of variables in the selected frame's scope.
#[no_mangle]
pub extern "C" fn mui_dbg_var_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.dbg.variable_count() as i32)
}

/// Length (chars) of variable `i`'s name, or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_var_name_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg.variable(i as usize).map_or(-1, |v| v.name.chars().count() as i32)
    })
}

/// `j`-th char of variable `i`'s name, or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_var_name_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg
            .variable(i as usize)
            .and_then(|v| v.name.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

/// Length (chars) of variable `i`'s value, or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_var_value_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg.variable(i as usize).map_or(-1, |v| v.value.chars().count() as i32)
    })
}

/// `j`-th char of variable `i`'s value, or -1.
#[no_mangle]
pub extern "C" fn mui_dbg_var_value_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.dbg
            .variable(i as usize)
            .and_then(|v| v.value.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

// ===========================================================================
// Click routing inside the debug view + toolbar
// ===========================================================================

/// What the last click hit inside the debug view, encoded for Mighty:
///   `-1` nothing, `0..` a call-stack frame index, or one of the toolbar codes
///   (`TOOLBAR_*` below) returned as `1000 + code`.
const TOOLBAR_BASE: i32 = 1000;
/// Toolbar action codes (added to `TOOLBAR_BASE`).
pub const TB_CONTINUE: i32 = 0;
pub const TB_STEP_OVER: i32 = 1;
pub const TB_STEP_INTO: i32 = 2;
pub const TB_STEP_OUT: i32 = 3;
pub const TB_STOP: i32 = 4;

/// Geometry of the debug toolbar (a row of 5 icon buttons under the header).
struct ToolbarGeom {
    x0: f32,
    y: f32,
    btn: f32,
    gap: f32,
}

fn toolbar_geom() -> ToolbarGeom {
    let sx = layout::RAIL_W;
    ToolbarGeom {
        x0: sx + 12.0,
        y: 40.0 + 8.0,
        btn: 30.0,
        gap: 6.0,
    }
}

/// Y pixel (top) of the first Call-Stack row.
fn stack_rows_top() -> f32 {
    40.0 + 8.0 + 30.0 + 10.0 + 20.0 // header + toolbar + gap + section label
}

/// Map the last click in the debug view: a toolbar button (`TOOLBAR_BASE + code`)
/// or a call-stack frame index (`0..`), else `-1`.
#[no_mangle]
pub extern "C" fn mui_dbg_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.dbg.is_open() || ctx.active_panel != crate::PANEL_DEBUG {
        return -1;
    }
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    let sx0 = layout::RAIL_W;
    let sx1 = layout::RAIL_W + layout::SIDEBAR_W;
    if x < sx0 || x > sx1 {
        return -1;
    }
    // Toolbar buttons.
    let tb = toolbar_geom();
    if y >= tb.y && y <= tb.y + tb.btn {
        for code in 0..5 {
            let bx = tb.x0 + code as f32 * (tb.btn + tb.gap);
            if x >= bx && x <= bx + tb.btn {
                return TOOLBAR_BASE + code;
            }
        }
    }
    // Call-stack rows.
    let top = stack_rows_top();
    if y >= top {
        let idx = ((y - top) / layout::LINE_H()).floor() as i32;
        if idx >= 0 && (idx as usize) < ctx.dbg.stack_count() {
            return idx;
        }
    }
    -1
}

/// Decode a [`mui_dbg_click`] toolbar code and perform the action. Mighty calls
/// this when `mui_dbg_click` returned `>= TOOLBAR_BASE`. The `code` is the raw
/// return value. No-op for non-toolbar values.
#[no_mangle]
pub extern "C" fn mui_dbg_toolbar_action(handle: i64, code: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    match code - TOOLBAR_BASE {
        x if x == TB_CONTINUE => ctx.dbg.continue_(),
        x if x == TB_STEP_OVER => ctx.dbg.step_over(),
        x if x == TB_STEP_INTO => ctx.dbg.step_into(),
        x if x == TB_STEP_OUT => ctx.dbg.step_out(),
        x if x == TB_STOP => ctx.dbg.stop(),
        _ => {}
    }
}

// ===========================================================================
// Drawing — gutter decorations (editor body) + the debug-view panel
// ===========================================================================

/// Draw the breakpoint dots in the editor gutter + the stopped-line decorations
/// (a distinct band across the row + a current-instruction arrow in the gutter).
/// `first` is the top visible line, `rows` the visible row count, `total_lines`
/// sizes the gutter. Drawn after the editor body each frame; a no-op when there
/// are no breakpoints / no active stop on the visible file.
#[no_mangle]
pub extern "C" fn mui_dbg_draw(handle: i64, first: i32, rows: i32, total_lines: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    // The inline-diff view owns the body; skip then.
    if ctx.diff.is_active() {
        return;
    }
    use crate::dap::DebugState;
    use crate::icons;
    let region = layout::region(ctx.sidebar_visible);
    let first = first.max(0);
    let rows = rows.max(0);
    let total_u64 = total_lines.max(1) as u64;
    let text_x = layout::text_left_in(region, total_u64);
    let win_w = ctx.gpu.width as f32;
    let line_h = layout::LINE_H();
    let minimap_on = crate::settings::minimap();
    let mm_w = if minimap_on { 70.0_f32 } else { 0.0_f32 };
    let band_w = (win_w - mm_w) - region.left;

    let file = active_path_str(ctx);

    // 1) Stopped-line band + gutter arrow (only when stopped on the visible file).
    let stopped_here = ctx.dbg.state() == DebugState::Stopped && {
        let cur = ctx.dbg.cur_file().replace('\\', "/");
        let active = file.replace('\\', "/");
        !cur.is_empty() && (cur == active || cur.ends_with(&active) || active.ends_with(&cur))
    };
    let cur_line = ctx.dbg.cur_line();
    if stopped_here && cur_line >= first && cur_line < first + rows {
        let row = cur_line - first;
        let y = layout::row_y_in(region, row);
        let band_top = (y - 1.0).max(region.top);
        let band_h = line_h - (band_top - (y - 1.0));
        // A distinct amber/green stopped band (separate visual language from the
        // indigo current-line band) + a left edge bar.
        let stop_tint = MuiColor::new(0.92, 0.74, 0.30, 0.16);
        ctx.dl_grad_h(region.left, band_top, band_w, band_h, 0.0, stop_tint, 0.55);
        ctx.dl_rect(region.left, band_top, 2.5, band_h, theme::WARNING());
        // Current-instruction arrow in the gutter.
        let ay = y + (line_h - 14.0) * 0.5;
        ctx.dl_icon(region.left + 4.0, ay, 14.0, 14.0, icons::DBG_ARROW, theme::WARNING(), 0.0, true);
    }

    // 2) Breakpoint dots in the gutter (every visible breakpoint line).
    if !file.is_empty() {
        let gutter_dot_x = region.left + 5.0;
        for line0 in ctx.dbg.breakpoint_lines0(&file) {
            if line0 < first || line0 >= first + rows {
                continue;
            }
            let row = line0 - first;
            let y = layout::row_y_in(region, row);
            let cy = y + (line_h - 11.0) * 0.5;
            ctx.dl_icon(gutter_dot_x, cy, 11.0, 11.0, icons::BREAKPOINT, theme::ERROR(), 0.0, true);
        }
        // Don't let the breakpoint dot overlap the text column.
        let _ = text_x;
    }
}

/// Draw the debug view sidebar panel (toolbar + Call Stack + Variables + a small
/// console). No-op when the sidebar is hidden or this panel isn't active.
#[no_mangle]
pub extern "C" fn mui_dbg_view_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_DEBUG {
        return;
    }
    use crate::dap::DebugState;
    use crate::icons;
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let adv = chrome * 0.55;
    let sx = layout::RAIL_W;
    let sw = layout::SIDEBAR_W;

    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // Header band: bug icon + "RUN AND DEBUG" + a state pill.
    let head_h = 40.0;
    ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
    ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
    ctx.dl_icon(sx + 12.0, (head_h - 15.0) * 0.5, 15.0, 15.0, icons::DEBUG, theme::ACCENT_BRIGHT(), 1.5, false);
    let title = "RUN AND DEBUG";
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ctx.text.queue_ui_sized(sx + 34.0, (head_h - (chrome - 2.0)) * 0.5 - 1.0, &tracked, theme::DIM(), chrome - 2.0, clip);

    let (state_label, state_col) = match ctx.dbg.state() {
        DebugState::Idle => ("idle", theme::TEXT_3()),
        DebugState::Running => ("running\u{2026}", theme::WARNING()),
        DebugState::Stopped => ("paused", theme::GREEN()),
        DebugState::Terminated => ("exited", theme::TEXT_3()),
    };
    let pill_w = state_label.chars().count() as f32 * (chrome * 0.5) + 18.0;
    let pill_x = sx + sw - pill_w - 12.0;
    let pill_y = (head_h - 17.0) * 0.5;
    ctx.dl_round(pill_x, pill_y, pill_w, 17.0, 6.0, theme::BG_4());
    ctx.text.queue_ui_sized(pill_x + 9.0, pill_y + 2.5, state_label, state_col, chrome - 2.0, clip);

    // Toolbar: continue / step-over / step-into / step-out / stop.
    let tb = toolbar_geom();
    let running = matches!(ctx.dbg.state(), DebugState::Running | DebugState::Stopped);
    let buttons: [(&str, MuiColor, f32, bool); 5] = [
        (icons::DBG_CONTINUE, theme::GREEN(), 1.6, true),
        (icons::DBG_STEP_OVER, theme::ACCENT_BRIGHT(), 1.6, false),
        (icons::DBG_STEP_INTO, theme::ACCENT_BRIGHT(), 1.6, false),
        (icons::DBG_STEP_OUT, theme::ACCENT_BRIGHT(), 1.6, false),
        (icons::DBG_STOP, theme::ERROR(), 1.6, true),
    ];
    for (i, (path, color, stroke, fill)) in buttons.iter().enumerate() {
        let bx = tb.x0 + i as f32 * (tb.btn + tb.gap);
        let enabled = if i == 0 || i == 4 {
            true
        } else {
            ctx.dbg.state() == DebugState::Stopped
        };
        let bg = if enabled { theme::BG_4() } else { theme::BG_1() };
        ctx.dl_round(bx, tb.y, tb.btn, tb.btn, 7.0, bg);
        ctx.dl_stroke(bx, tb.y, tb.btn, tb.btn, 7.0, theme::BORDER_STRONG(), 1.0);
        let col = if enabled { *color } else { theme::TEXT_4() };
        let isz = 16.0;
        let off = (tb.btn - isz) * 0.5;
        ctx.dl_icon(bx + off, tb.y + off, isz, isz, path, col, *stroke, *fill);
    }
    let _ = running;

    // ---- Call Stack section ----
    let label_y = tb.y + tb.btn + 10.0;
    ctx.text.queue_ui_sized(sx + 14.0, label_y, "CALL STACK", theme::DIM(), chrome - 2.0, clip);
    let row_h = layout::LINE_H();
    let top = stack_rows_top();
    let sel = ctx.dbg.selected_frame();
    let stack_n = ctx.dbg.stack_count();
    let mut next_y = top;
    if stack_n == 0 {
        let msg = match ctx.dbg.state() {
            DebugState::Idle | DebugState::Terminated => "Not paused. F5 to start.",
            _ => "Running\u{2026}",
        };
        ctx.text.queue_ui_sized(sx + 14.0, top + 2.0, msg, theme::TEXT_3(), chrome, clip);
        next_y = top + row_h;
    } else {
        for i in 0..stack_n {
            let (name, line, file) = {
                let Some(f) = ctx.dbg.frame(i) else { continue };
                let base = f.file.rsplit(['/', '\\']).next().unwrap_or("").to_string();
                (f.name.clone(), f.line, base)
            };
            let y = top + i as f32 * row_h;
            if y > h - 100.0 {
                break;
            }
            let selected = i == sel;
            if selected {
                ctx.dl_grad_h(sx + 5.0, y + 1.0, sw - 12.0, row_h - 2.0, 5.0, theme::accent_a(0.18), 0.85);
                ctx.dl_rect(sx + 5.0, y + 1.0, 2.0, row_h - 2.0, theme::ACCENT());
            }
            let ty = y + (row_h - chrome) * 0.5 - 1.0;
            // Frame icon (arrow on top frame, dot otherwise).
            let fcol = if selected { theme::ACCENT_BRIGHT() } else { theme::SYN_FUNCTION() };
            ctx.dl_icon(sx + 12.0, y + (row_h - 12.0) * 0.5, 12.0, 12.0, icons::FN_SYMBOL, fcol, 1.6, false);
            // Function name.
            let name_col = if selected { theme::TEXT() } else { theme::TEXT_1() };
            let mut nm = name;
            let navail = ((sw - 90.0) / adv).floor() as usize;
            if nm.chars().count() > navail && navail > 1 {
                nm = nm.chars().take(navail - 1).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(sx + 30.0, ty, &nm, name_col, chrome, clip);
            // file:line on the right (dim).
            let loc = format!("{file}:{line}");
            let mut lc = loc;
            let lavail = 16usize;
            if lc.chars().count() > lavail {
                lc = lc.chars().rev().take(lavail - 1).collect::<Vec<_>>().into_iter().rev().collect::<String>();
            }
            let lw = lc.chars().count() as f32 * (chrome * 0.5);
            ctx.text.queue_ui_sized(sx + sw - lw - 14.0, ty, &lc, theme::TEXT_4(), chrome - 1.5, clip);
            next_y = y + row_h;
        }
    }

    // ---- Variables section ----
    let var_label_y = next_y + 10.0;
    ctx.text.queue_ui_sized(sx + 14.0, var_label_y, "VARIABLES", theme::DIM(), chrome - 2.0, clip);
    let var_top = var_label_y + 20.0;
    let var_n = ctx.dbg.variable_count();
    let mut var_next_y = var_top;
    if var_n == 0 {
        ctx.text.queue_ui_sized(sx + 14.0, var_top, "\u{2014}", theme::TEXT_3(), chrome, clip);
        var_next_y = var_top + row_h;
    } else {
        for i in 0..var_n {
            let (name, value, kind) = {
                let Some(v) = ctx.dbg.variable(i) else { continue };
                (v.name.clone(), v.value.clone(), v.kind.clone())
            };
            let y = var_top + i as f32 * row_h;
            if y > h - 60.0 {
                break;
            }
            let ty = y + (row_h - chrome) * 0.5 - 1.0;
            // name (function color) : value (string color), type dim.
            let mut nm = name;
            if nm.chars().count() > 12 {
                nm = nm.chars().take(11).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(sx + 16.0, ty, &nm, theme::SYN_FUNCTION(), chrome, clip);
            let eq_x = sx + 16.0 + (nm.chars().count() as f32 + 1.0) * adv;
            ctx.text.queue_ui_sized(eq_x, ty, "=", theme::TEXT_4(), chrome, clip);
            let val_x = eq_x + 2.0 * adv;
            let mut vv = value;
            let vavail = (((sx + sw - 50.0) - val_x) / adv).floor() as usize;
            if vv.chars().count() > vavail && vavail > 1 {
                vv = vv.chars().take(vavail - 1).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(val_x, ty, &vv, theme::SYN_STRING(), chrome, clip);
            // type badge at the right.
            if !kind.is_empty() {
                let kw = kind.chars().count() as f32 * (chrome * 0.45);
                ctx.text.queue_ui_sized(sx + sw - kw - 12.0, ty, &kind, theme::TEXT_4(), chrome - 2.0, clip);
            }
            var_next_y = y + row_h;
        }
    }

    // ---- Debug Console (bottom of the panel) ----
    let con_label_y = var_next_y + 10.0;
    if con_label_y < h - 40.0 {
        ctx.text.queue_ui_sized(sx + 14.0, con_label_y, "DEBUG CONSOLE", theme::DIM(), chrome - 2.0, clip);
        let con_top = con_label_y + 18.0;
        let con_n = ctx.dbg.console_count();
        let visible = (((h - 8.0) - con_top) / row_h).floor().max(0.0) as usize;
        let start = con_n.saturating_sub(visible);
        for (vis, i) in (start..con_n).enumerate() {
            let Some(l) = ctx.dbg.console_line(i) else { continue };
            let y = con_top + vis as f32 * row_h;
            let ty = y + (row_h - chrome) * 0.5 - 1.0;
            let col = if l.is_error { theme::ERROR() } else { theme::TEXT_1() };
            let mut t = l.text.clone();
            let avail = ((sw - 24.0) / adv).floor() as usize;
            if t.chars().count() > avail && avail > 1 {
                t = t.chars().take(avail - 1).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(sx + 14.0, ty, &t, col, chrome - 0.5, clip);
        }
    }
}
