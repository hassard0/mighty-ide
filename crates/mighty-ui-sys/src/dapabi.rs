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

fn debug_command_display() -> String {
    let mty = crate::mty::path();
    let program = std::path::Path::new(&mty)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(mty.as_str());
    format!("{program} dap")
}

fn debug_spawn_failure_reason(ctx: &MuiContext) -> Option<String> {
    (0..ctx.dbg.console_count()).rev().find_map(|i| {
        let line = ctx.dbg.console_line(i)?;
        if !line.is_error {
            return None;
        }
        line.text
            .strip_prefix("debug: failed to start adapter: ")
            .map(|reason| reason.trim().to_string())
            .filter(|reason| !reason.is_empty())
    })
}

fn append_optional_reason(mut message: String, reason: Option<&str>) -> String {
    if let Some(reason) = reason.map(str::trim).filter(|s| !s.is_empty()) {
        message.push_str(": ");
        message.push_str(reason);
    }
    message
}

fn debug_start_failed_message(path: &std::path::Path, reason: Option<&str>) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    append_optional_reason(
        format!("Debug failed to start: {name} via {}", debug_command_display()),
        reason,
    )
}

fn debug_restart_failed_message(path: &std::path::Path, reason: Option<&str>) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    append_optional_reason(
        format!("Debug restart failed: {name} via {}", debug_command_display()),
        reason,
    )
}

fn debug_restart_failed_no_path_message(reason: Option<&str>) -> String {
    append_optional_reason(
        format!("Debug restart failed via {}", debug_command_display()),
        reason,
    )
}

fn debug_target_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string()
}

fn active_debug_target_name(ctx: &MuiContext) -> String {
    ctx.tabs
        .active_path()
        .as_deref()
        .map(|path| {
            path.file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("file")
                .to_string()
        })
        .unwrap_or_else(|| "(scratch)".to_string())
}

fn debug_needs_file_message(ctx: &MuiContext, action: &str) -> String {
    format!("Save {} before {action}", active_debug_target_name(ctx))
}

fn debug_restart_needs_target_message(ctx: &MuiContext) -> String {
    format!(
        "Start debug before restarting: {} has no previous target",
        active_debug_target_name(ctx)
    )
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
    open_debug_view(ctx);
    dbg_start_or_continue(ctx)
}

fn dbg_start_or_continue(ctx: &mut MuiContext) -> i32 {
    use crate::dap::DebugState;
    match ctx.dbg.state() {
        DebugState::Stopped => {
            crate::abi::trace("dbg_action continue");
            ctx.dbg.continue_();
        }
        DebugState::Running => {
            ctx.push_toast(crate::toast::Kind::Info, "Debug session already running");
            crate::abi::trace("dbg_action already_running");
        }
        DebugState::Idle | DebugState::Terminated => {
            let Some(path) = ctx.tabs.active_path() else {
                ctx.push_toast(
                    crate::toast::Kind::Warn,
                    debug_needs_file_message(ctx, "starting debug"),
                );
                crate::abi::trace("dbg_action start_no_file");
                return ctx.dbg.state().as_i32();
            };
            let label = debug_target_label(&path);
            let stale_reason = match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => None,
                Ok(_) => Some(format!("target is not a file: {label}")),
                Err(err) => Some(format!("{label}: {err}")),
            };
            if let Some(reason) = stale_reason {
                ctx.dbg.fail_before_start(&path, reason);
                let reason = debug_spawn_failure_reason(ctx);
                ctx.push_toast(
                    crate::toast::Kind::Error,
                    debug_start_failed_message(&path, reason.as_deref()),
                );
                crate::abi::trace("dbg_action start_stale_target");
                return ctx.dbg.state().as_i32();
            }
            crate::abi::trace(&format!(
                "dbg_action start path={}",
                path.to_string_lossy().replace('\\', "/")
            ));
            let ok = ctx.dbg.start(&path);
            if !ok {
                let reason = debug_spawn_failure_reason(ctx);
                ctx.push_toast(
                    crate::toast::Kind::Error,
                    debug_start_failed_message(&path, reason.as_deref()),
                );
            }
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
    open_debug_view(ctx);
    match ctx.dbg.state() {
        crate::dap::DebugState::Stopped => {
            ctx.dbg.continue_();
            crate::abi::trace("dbg_action direct_continue");
        }
        crate::dap::DebugState::Running => {
            ctx.push_toast(crate::toast::Kind::Info, "Debug session already running");
            crate::abi::trace("dbg_action continue_already_running");
        }
        crate::dap::DebugState::Idle | crate::dap::DebugState::Terminated => {
            ctx.push_toast(crate::toast::Kind::Info, "Continue is available when paused");
            crate::abi::trace("dbg_action continue_unavailable");
        }
    }
    ctx.dbg.state().as_i32()
}

/// Shift+F5 / Stop: disconnect the session.
#[no_mangle]
pub extern "C" fn mui_dbg_stop(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if matches!(
        ctx.dbg.state(),
        crate::dap::DebugState::Running | crate::dap::DebugState::Stopped
    ) {
        ctx.dbg.stop();
        ctx.push_toast(crate::toast::Kind::Info, "Debug session stopped");
        crate::abi::trace("dbg_action stop");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No debug session to stop");
        crate::abi::trace("dbg_action stop_unavailable");
        0
    }
}

/// Pause a running debuggee.
#[no_mangle]
pub extern "C" fn mui_dbg_pause(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if ctx.dbg.state() == crate::dap::DebugState::Running {
        ctx.dbg.pause();
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Pause is available while running");
        crate::abi::trace("dbg_action pause_unavailable");
    }
    ctx.dbg.state().as_i32()
}

/// Restart the last debug target.
#[no_mangle]
pub extern "C" fn mui_dbg_restart(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    let had_target = ctx.dbg.has_program();
    let target = ctx.dbg.program().map(|p| p.to_path_buf());
    let ok = ctx.dbg.restart();
    if !ok {
        let reason = debug_spawn_failure_reason(ctx);
        if let Some(path) = target.as_deref() {
            ctx.push_toast(
                crate::toast::Kind::Error,
                debug_restart_failed_message(path, reason.as_deref()),
            );
            crate::abi::trace("dbg_restart failed");
        } else if had_target {
            ctx.push_toast(
                crate::toast::Kind::Error,
                debug_restart_failed_no_path_message(reason.as_deref()),
            );
            crate::abi::trace("dbg_restart failed_no_path");
        } else {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                debug_restart_needs_target_message(ctx),
            );
            crate::abi::trace("dbg_restart no_target");
        }
    }
    i32::from(ok)
}

/// F10 / step over (`next`).
#[no_mangle]
pub extern "C" fn mui_dbg_step_over(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if ctx.dbg.state() == crate::dap::DebugState::Stopped {
        ctx.dbg.step_over();
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Step Over is available when paused");
        crate::abi::trace("dbg_action step_over_unavailable");
    }
    ctx.dbg.state().as_i32()
}

/// F11 / step into (`stepIn`).
#[no_mangle]
pub extern "C" fn mui_dbg_step_into(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if ctx.dbg.state() == crate::dap::DebugState::Stopped {
        ctx.dbg.step_into();
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Step Into is available when paused");
        crate::abi::trace("dbg_action step_into_unavailable");
    }
    ctx.dbg.state().as_i32()
}

/// Shift+F11 / step out (`stepOut`).
#[no_mangle]
pub extern "C" fn mui_dbg_step_out(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if ctx.dbg.state() == crate::dap::DebugState::Stopped {
        ctx.dbg.step_out();
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Step Out is available when paused");
        crate::abi::trace("dbg_action step_out_unavailable");
    }
    ctx.dbg.state().as_i32()
}

fn open_debug_view(ctx: &mut MuiContext) {
    ctx.dbg.set_open(true);
    ctx.active_panel = crate::PANEL_DEBUG;
    ctx.sidebar_visible = true;
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

/// Close the Run and Debug panel without stopping or resetting the debug model.
/// Returns `1` when it closed the panel, or `0` when already closed.
#[no_mangle]
pub extern "C" fn mui_dbg_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.active_panel == crate::PANEL_DEBUG || ctx.dbg.is_open() {
        ctx.dbg.set_open(false);
        if ctx.active_panel == crate::PANEL_DEBUG {
            ctx.active_panel = crate::PANEL_EXPLORER;
        }
        ctx.push_toast(crate::toast::Kind::Info, "Run and Debug panel closed");
        crate::abi::trace("dbg_close");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Run and Debug panel is already closed");
    crate::abi::trace("dbg_close noop");
    0
}

/// Clear the current debug session model without clearing breakpoints or the
/// last target. Returns `1` when session state was cleared, or `0` when empty.
#[no_mangle]
pub extern "C" fn mui_dbg_clear_session(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if ctx.dbg.clear_session() {
        ctx.push_toast(crate::toast::Kind::Info, "Debug session cleared");
        crate::abi::trace("dbg_clear_session");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Debug session already empty");
    crate::abi::trace("dbg_clear_session noop");
    0
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
/// breakpoint is now set, `0` if cleared or unavailable.
#[no_mangle]
pub extern "C" fn mui_bp_toggle(handle: i64, line: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let file = active_path_str(ctx);
    if file.is_empty() {
        open_debug_view(ctx);
        ctx.push_toast(
            crate::toast::Kind::Warn,
            debug_needs_file_message(ctx, "setting breakpoints"),
        );
        crate::abi::trace("bp_toggle no_file");
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

/// Palette command: toggle a breakpoint at the active editor cursor. Opens the
/// Run and Debug view and reports both set and cleared outcomes.
#[no_mangle]
pub extern "C" fn mui_bp_toggle_at_cursor(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    let file = active_path_str(ctx);
    if file.is_empty() {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            debug_needs_file_message(ctx, "setting breakpoints"),
        );
        crate::abi::trace("bp_toggle_cursor no_file");
        return 0;
    }
    let line = ctx.tabs.active_model().cursor_line() as i32;
    let now_on = ctx.dbg.toggle_breakpoint(&file, line);
    if ctx.dbg.state() != crate::dap::DebugState::Idle
        && ctx.dbg.state() != crate::dap::DebugState::Terminated
    {
        ctx.dbg.resend_breakpoints();
    }
    let line1 = line + 1;
    let msg = if now_on {
        format!("Breakpoint set on line {line1}")
    } else {
        format!("Breakpoint cleared on line {line1}")
    };
    ctx.push_toast(crate::toast::Kind::Info, msg);
    crate::abi::trace(&format!("bp_toggle_cursor line={line1} on={now_on}"));
    i32::from(now_on)
}

/// Palette command: clear every stored line breakpoint. Opens the Run and Debug
/// view and, if a session is live, sends an empty breakpoint set to the adapter.
#[no_mangle]
pub extern "C" fn mui_bp_clear_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    open_debug_view(ctx);
    if ctx.dbg.clear_breakpoints() {
        if ctx.dbg.state() != crate::dap::DebugState::Idle
            && ctx.dbg.state() != crate::dap::DebugState::Terminated
        {
            ctx.dbg.resend_breakpoints();
        }
        ctx.push_toast(crate::toast::Kind::Info, "Breakpoints cleared");
        crate::abi::trace("bp_clear_all");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No breakpoints to clear");
        crate::abi::trace("bp_clear_all noop");
        0
    }
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
///   `-1` nothing, `0..` a call-stack frame index, breakpoint rows as
///   `BREAKPOINT_BASE + index`, breakpoint-dot remove targets as
///   `BREAKPOINT_REMOVE_BASE + index`, or one of the toolbar codes
///   (`TOOLBAR_*` below) returned as `TOOLBAR_BASE + code`.
const TOOLBAR_BASE: i32 = 1000;
const BREAKPOINT_BASE: i32 = 2000;
const BREAKPOINT_REMOVE_BASE: i32 = 3000;
/// Toolbar action codes (added to `TOOLBAR_BASE`).
pub const TB_CONTINUE: i32 = 0;
pub const TB_STEP_OVER: i32 = 1;
pub const TB_STEP_INTO: i32 = 2;
pub const TB_STEP_OUT: i32 = 3;
pub const TB_STOP: i32 = 4;
pub const TB_CLEAR_SESSION: i32 = 5;
pub(crate) const DEBUG_TOOLBAR_BUTTONS: usize = 6;

/// Geometry of the debug toolbar (a row of icon buttons under the header).
pub(crate) struct ToolbarGeom {
    pub(crate) x0: f32,
    pub(crate) y: f32,
    pub(crate) btn: f32,
    pub(crate) gap: f32,
}

pub(crate) fn toolbar_geom() -> ToolbarGeom {
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    let gap = if sw <= layout::SIDEBAR_MIN_W + 2.0 { 4.0 } else { 6.0 };
    let gaps = DEBUG_TOOLBAR_BUTTONS.saturating_sub(1) as f32;
    let available = (sw - 24.0 - gap * gaps).max(0.0);
    let btn = (available / DEBUG_TOOLBAR_BUTTONS as f32).clamp(22.0, 30.0);
    ToolbarGeom {
        x0: sx + 12.0,
        y: 40.0 + 8.0,
        btn,
        gap,
    }
}

pub(crate) fn debug_breakpoint_visible_rows(count: usize) -> usize {
    count.clamp(1, 4)
}

pub(crate) fn debug_breakpoint_data_rows(count: usize) -> usize {
    if count > 4 {
        3
    } else {
        count
    }
}

pub(crate) fn debug_breakpoint_hidden_count(count: usize) -> usize {
    count.saturating_sub(debug_breakpoint_data_rows(count))
}

pub(crate) fn debug_breakpoint_overflow_label(hidden: usize) -> String {
    if hidden == 1 {
        "1 more breakpoint".to_string()
    } else {
        format!("{hidden} more breakpoints")
    }
}

pub(crate) fn debug_breakpoint_scroll_label(first: usize, count: usize, data_rows: usize) -> String {
    let below = if first == 0 {
        debug_breakpoint_hidden_count(count)
    } else {
        count.saturating_sub(first.saturating_add(data_rows))
    };
    if below > 0 {
        debug_breakpoint_overflow_label(below)
    } else if first == 1 {
        "1 earlier breakpoint".to_string()
    } else if first > 1 {
        format!("{first} earlier breakpoints")
    } else {
        String::new()
    }
}

pub(crate) fn debug_breakpoint_label_y() -> f32 {
    let tb = toolbar_geom();
    tb.y + tb.btn + 10.0
}

pub(crate) fn debug_breakpoint_rows_top() -> f32 {
    debug_breakpoint_label_y() + 20.0
}

pub(crate) fn debug_breakpoint_clear_button_rect() -> (f32, f32, f32, f32) {
    let size = 22.0;
    let x = layout::sidebar_right() - size - 10.0;
    let y = debug_breakpoint_label_y() - 4.0;
    (x, y, size, size)
}

pub(crate) fn debug_breakpoint_remove_target_left() -> f32 {
    layout::RAIL_W + 8.0
}

pub(crate) fn debug_breakpoint_remove_target_right() -> f32 {
    layout::RAIL_W + 28.0
}

pub(crate) fn debug_stack_label_y(breakpoint_count: usize) -> f32 {
    debug_breakpoint_rows_top()
        + debug_breakpoint_visible_rows(breakpoint_count) as f32 * layout::LINE_H()
        + 10.0
}

/// Y pixel (top) of the first Call-Stack row.
fn stack_rows_top(breakpoint_count: usize) -> f32 {
    debug_stack_label_y(breakpoint_count) + 20.0
}

pub(crate) fn debug_state_pill_width(
    text: &mut crate::text::Text,
    state_label: &str,
    size: f32,
) -> f32 {
    text.measure_ui_sized(state_label, size).0 + 18.0
}

pub(crate) fn debug_header_title_for_budget(
    text: &mut crate::text::Text,
    title_x: f32,
    pill_x: f32,
    size: f32,
) -> &'static str {
    let full = "RUN AND DEBUG";
    let tracked: String = full.chars().flat_map(|c| [c, '\u{2009}']).collect();
    let budget = (pill_x - 8.0 - title_x).max(0.0);
    if text.measure_ui_sized(&tracked, size).0 <= budget {
        full
    } else {
        "DEBUG"
    }
}

pub(crate) fn fit_debug_header_title(
    text: &mut crate::text::Text,
    title: &str,
    title_x: f32,
    pill_x: f32,
    size: f32,
) -> String {
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    fit_head_px(text, &tracked, (pill_x - 8.0 - title_x).max(0.0), size)
}

fn fit_head_px(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if max_px <= 0.0 {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    if text.measure_ui_sized(ellipsis, size).0 > max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut candidate: String = chars.iter().take(mid).collect();
        candidate.push_str(ellipsis);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        ellipsis.to_string()
    } else {
        let mut out: String = chars.iter().take(lo).collect();
        out.push_str(ellipsis);
        out
    }
}

pub(crate) fn fit_debug_stack_name(
    text: &mut crate::text::Text,
    name: &str,
    name_x: f32,
    loc_x: f32,
    size: f32,
) -> String {
    fit_head_px(text, name, (loc_x - 8.0 - name_x).max(0.0), size)
}

pub(crate) fn fit_debug_stack_location(
    text: &mut crate::text::Text,
    loc: &str,
    max_px: f32,
    size: f32,
) -> String {
    fit_tail_px(text, loc, max_px, size)
}

pub(crate) fn debug_ui_text_width(text: &mut crate::text::Text, s: &str, size: f32) -> f32 {
    text.measure_ui_sized(s, size).0
}

pub(crate) fn debug_variable_name_budget(
    text: &mut crate::text::Text,
    cells: usize,
    size: f32,
) -> f32 {
    let probe = "m".repeat(cells);
    text.measure_ui_sized(&probe, size).0
}

pub(crate) fn debug_variable_separator_advance(text: &mut crate::text::Text, size: f32) -> f32 {
    text.measure_ui_sized(" = ", size).0
}

pub(crate) fn fit_debug_variable_name(
    text: &mut crate::text::Text,
    name: &str,
    max_px: f32,
    size: f32,
) -> String {
    fit_head_px(text, name, max_px, size)
}

pub(crate) fn fit_debug_variable_value(
    text: &mut crate::text::Text,
    value: &str,
    max_px: f32,
    size: f32,
) -> String {
    fit_head_px(text, value, max_px, size)
}

pub(crate) fn fit_debug_console_line(
    text: &mut crate::text::Text,
    line: &str,
    max_px: f32,
    size: f32,
) -> String {
    fit_head_px(text, line, max_px, size)
}

fn fit_tail_px(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if max_px <= 0.0 {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    if text.measure_ui_sized(ellipsis, size).0 > max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = std::iter::once('\u{2026}')
            .chain(chars[chars.len().saturating_sub(mid)..].iter().copied())
            .collect();
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        ellipsis.to_string()
    } else {
        std::iter::once('\u{2026}')
            .chain(chars[chars.len().saturating_sub(lo)..].iter().copied())
            .collect()
    }
}

/// Map the last click in the debug view: a toolbar button (`TOOLBAR_BASE + code`),
/// breakpoint row (`BREAKPOINT_BASE + index`), or call-stack frame index (`0..`),
/// else `-1`.
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
    let sx1 = layout::sidebar_right();
    if x < sx0 || x > sx1 {
        return -1;
    }
    // Toolbar buttons.
    let tb = toolbar_geom();
    if y >= tb.y && y <= tb.y + tb.btn {
        for code in 0..DEBUG_TOOLBAR_BUTTONS {
            let bx = tb.x0 + code as f32 * (tb.btn + tb.gap);
            if x >= bx && x <= bx + tb.btn {
                return TOOLBAR_BASE + code as i32;
            }
        }
    }
    // Breakpoint rows.
    let breakpoints = ctx.dbg.breakpoint_locations();
    let bp_top = debug_breakpoint_rows_top();
    let bp_rows = debug_breakpoint_visible_rows(breakpoints.len());
    let bp_data_rows = debug_breakpoint_data_rows(breakpoints.len());
    if !breakpoints.is_empty() && y >= bp_top && y < bp_top + bp_rows as f32 * layout::LINE_H() {
        let idx = ((y - bp_top) / layout::LINE_H()).floor() as i32;
        let first = ctx.dbg.breakpoint_window_first(bp_data_rows);
        if idx >= 0
            && first + (idx as usize) < breakpoints.len()
            && (idx as usize) < bp_data_rows
        {
            if x >= debug_breakpoint_remove_target_left()
                && x <= debug_breakpoint_remove_target_right()
            {
                return BREAKPOINT_REMOVE_BASE + idx;
            }
            return BREAKPOINT_BASE + idx;
        }
    }
    // Call-stack rows.
    let top = stack_rows_top(ctx.dbg.total_breakpoint_count());
    if y >= top {
        let idx = ((y - top) / layout::LINE_H()).floor() as i32;
        if idx >= 0 && (idx as usize) < ctx.dbg.stack_count() {
            return idx;
        }
    }
    -1
}

/// Clear the visible Breakpoints header button if the last click hit it. Returns
/// `-1` when not hit, `1` when breakpoints were cleared, or `0` for the visible
/// button's no-op state.
#[no_mangle]
pub extern "C" fn mui_bp_clear_inventory_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_DEBUG || !ctx.dbg.is_open() {
        return -1;
    }
    if ctx.dbg.total_breakpoint_count() == 0 {
        return -1;
    }
    let (x, y, w, h) = debug_breakpoint_clear_button_rect();
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    if px < x || px > x + w || py < y || py > y + h {
        return -1;
    }
    if ctx.dbg.clear_breakpoints() {
        if ctx.dbg.state() != crate::dap::DebugState::Idle
            && ctx.dbg.state() != crate::dap::DebugState::Terminated
        {
            ctx.dbg.resend_breakpoints();
        }
        ctx.push_toast(crate::toast::Kind::Info, "Breakpoints cleared");
        crate::abi::trace("bp_clear_inventory");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No breakpoints to clear");
        crate::abi::trace("bp_clear_inventory noop");
        0
    }
}

fn breakpoint_location_at_code(ctx: &MuiContext, code: i32, base: i32) -> Option<crate::dap::BreakpointLocation> {
    let idx = code - base;
    if idx < 0 {
        return None;
    }
    let locations = ctx.dbg.breakpoint_locations();
    let data_rows = debug_breakpoint_data_rows(locations.len());
    let first = ctx.dbg.breakpoint_window_first(data_rows);
    let source_idx = first + idx as usize;
    if source_idx >= locations.len() || (idx as usize) >= data_rows {
        None
    } else {
        Some(locations[source_idx].clone())
    }
}

fn breakpoint_missing_row_message(ctx: &MuiContext, code: i32, base: i32) -> &'static str {
    let idx = code - base;
    if idx < 0 {
        return "No breakpoint row selected";
    }
    let locations = ctx.dbg.breakpoint_locations();
    let data_rows = debug_breakpoint_data_rows(locations.len());
    let first = ctx.dbg.breakpoint_window_first(data_rows);
    if locations.is_empty() || (idx as usize) >= data_rows || first + idx as usize >= locations.len() {
        "Breakpoint row no longer listed"
    } else {
        "No breakpoint row selected"
    }
}

/// Open the source location for a breakpoint click code returned by
/// [`mui_dbg_click`]. Returns the active tab index, or `-1` if the row/path is
/// unavailable.
#[no_mangle]
pub extern "C" fn mui_bp_open_at_hit(handle: i64, code: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let idx = code - BREAKPOINT_BASE;
    if idx < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No breakpoint row selected");
        return -1;
    }
    let Some(target) = breakpoint_location_at_code(ctx, code, BREAKPOINT_BASE) else {
        ctx.push_toast(
            crate::toast::Kind::Info,
            breakpoint_missing_row_message(ctx, code, BREAKPOINT_BASE),
        );
        return -1;
    };
    let path = std::path::PathBuf::from(&target.file);
    if !path.exists() {
        let name = crate::abi::file_target_name(&path);
        if ctx.dbg.remove_breakpoint(&target.file, target.line)
            && ctx.dbg.state() != crate::dap::DebugState::Idle
            && ctx.dbg.state() != crate::dap::DebugState::Terminated
        {
            ctx.dbg.resend_breakpoints();
        }
        crate::abi::refresh_workspace_file_views(ctx);
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Breakpoint target missing: {name}"),
        );
        crate::abi::trace(&format!("bp_open missing {}", target.file));
        return -1;
    }
    if !path.is_file() {
        let name = crate::abi::file_target_name(&path);
        crate::abi::refresh_workspace_file_views(ctx);
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Breakpoint target is not a file: {name}"),
        );
        crate::abi::trace(&format!("bp_open not-file {}", target.file));
        return -1;
    }
    let tab = ctx.tabs.open_path(path.clone());
    crate::abi::sync_active_path(ctx);
    crate::abi::record_opened_file(ctx, &path);
    let line0 = target.line.saturating_sub(1) as i32;
    let model = ctx.tabs.active_model_mut();
    model.move_to(line0, 0);
    model.set_first_visible(line0.saturating_sub(2) as usize);
    crate::abi::trace(&format!("bp_open line={}", target.line));
    tab as i32
}

/// Remove the breakpoint dot target returned by [`mui_dbg_click`]. Returns 1
/// when a breakpoint was removed.
#[no_mangle]
pub extern "C" fn mui_bp_remove_at_hit(handle: i64, code: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(target) = breakpoint_location_at_code(ctx, code, BREAKPOINT_REMOVE_BASE) else {
        ctx.push_toast(
            crate::toast::Kind::Info,
            breakpoint_missing_row_message(ctx, code, BREAKPOINT_REMOVE_BASE),
        );
        return 0;
    };
    if !ctx.dbg.remove_breakpoint(&target.file, target.line) {
        ctx.push_toast(crate::toast::Kind::Info, "Breakpoint already cleared");
        return 0;
    }
    if ctx.dbg.state() != crate::dap::DebugState::Idle
        && ctx.dbg.state() != crate::dap::DebugState::Terminated
    {
        ctx.dbg.resend_breakpoints();
    }
    let name = std::path::Path::new(&target.file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("source");
    ctx.push_toast(
        crate::toast::Kind::Info,
        format!("Breakpoint removed: {name}:{}", target.line),
    );
    crate::abi::trace(&format!("bp_remove {}:{}", target.file, target.line));
    1
}

/// Scroll the debug sidebar breakpoint inventory if the last wheel event is
/// over its rows. Returns 1 when the event was consumed.
#[no_mangle]
pub extern "C" fn mui_bp_scroll_inventory_at_event(handle: i64, delta: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible
        || ctx.active_panel != crate::PANEL_DEBUG
        || !ctx.dbg.is_open()
    {
        return 0;
    }
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if x < layout::RAIL_W || x > layout::sidebar_right() {
        return 0;
    }
    let count = ctx.dbg.total_breakpoint_count();
    let rows = debug_breakpoint_visible_rows(count);
    let top = debug_breakpoint_rows_top();
    if count == 0 || y < top || y >= top + rows as f32 * layout::LINE_H() {
        return 0;
    }
    let data_rows = debug_breakpoint_data_rows(count);
    if ctx.dbg.scroll_breakpoints(delta, data_rows) {
        1
    } else {
        0
    }
}

/// Decode a [`mui_dbg_click`] toolbar code and perform the action. Mighty calls
/// this when `mui_dbg_click` returned `>= TOOLBAR_BASE`. The `code` is the raw
/// return value. No-op for non-toolbar values.
#[no_mangle]
pub extern "C" fn mui_dbg_toolbar_action(handle: i64, code: i32) {
    match code - TOOLBAR_BASE {
        x if x == TB_CONTINUE => {
            crate::abi::trace("dbg_toolbar action=start_continue");
            let _ = mui_dbg_start(handle);
        }
        x if x == TB_STEP_OVER => {
            crate::abi::trace("dbg_toolbar action=step_over");
            mui_dbg_step_over(handle);
        }
        x if x == TB_STEP_INTO => {
            crate::abi::trace("dbg_toolbar action=step_into");
            mui_dbg_step_into(handle);
        }
        x if x == TB_STEP_OUT => {
            crate::abi::trace("dbg_toolbar action=step_out");
            mui_dbg_step_out(handle);
        }
        x if x == TB_STOP => {
            crate::abi::trace("dbg_toolbar action=stop");
            let _ = mui_dbg_stop(handle);
        }
        x if x == TB_CLEAR_SESSION => {
            crate::abi::trace("dbg_toolbar action=clear_session");
            let _ = mui_dbg_clear_session(handle);
        }
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
    let pane_w = (win_w - region.left).max(0.0);
    let minimap_on = crate::abi::should_show_minimap(crate::settings::minimap(), false, true, pane_w);
    let mm_w = if minimap_on { crate::abi::MINIMAP_W } else { 0.0_f32 };
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
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();

    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // Header band: bug icon + "RUN AND DEBUG" + a state pill.
    let head_h = 40.0;
    ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
    ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
    ctx.dl_icon(sx + 12.0, (head_h - 15.0) * 0.5, 15.0, 15.0, icons::DEBUG, theme::ACCENT_BRIGHT(), 1.5, false);
    let (state_label, state_col) = match ctx.dbg.state() {
        DebugState::Idle => ("idle", theme::TEXT_3()),
        DebugState::Running => ("running\u{2026}", theme::WARNING()),
        DebugState::Stopped => ("paused", theme::GREEN()),
        DebugState::Terminated => ("exited", theme::TEXT_3()),
    };
    let pill_w = debug_state_pill_width(&mut ctx.text, state_label, chrome - 2.0);
    let pill_x = sx + sw - pill_w - 12.0;
    let title_x = sx + 34.0;
    let title = debug_header_title_for_budget(&mut ctx.text, title_x, pill_x, chrome - 2.0);
    let tracked = fit_debug_header_title(&mut ctx.text, title, title_x, pill_x, chrome - 2.0);
    ctx.text.queue_ui_sized(
        title_x,
        (head_h - (chrome - 2.0)) * 0.5 - 1.0,
        &tracked,
        theme::DIM(),
        chrome - 2.0,
        clip,
    );
    let pill_y = (head_h - 17.0) * 0.5;
    ctx.dl_round(pill_x, pill_y, pill_w, 17.0, 6.0, theme::BG_4());
    ctx.text.queue_ui_sized(pill_x + 9.0, pill_y + 2.5, state_label, state_col, chrome - 2.0, clip);

    // Toolbar: continue / step-over / step-into / step-out / stop / clear.
    let tb = toolbar_geom();
    let running = matches!(ctx.dbg.state(), DebugState::Running | DebugState::Stopped);
    let buttons: [(&str, MuiColor, f32, bool); DEBUG_TOOLBAR_BUTTONS] = [
        (icons::DBG_CONTINUE, theme::GREEN(), 1.6, true),
        (icons::DBG_STEP_OVER, theme::ACCENT_BRIGHT(), 1.6, false),
        (icons::DBG_STEP_INTO, theme::ACCENT_BRIGHT(), 1.6, false),
        (icons::DBG_STEP_OUT, theme::ACCENT_BRIGHT(), 1.6, false),
        (icons::DBG_STOP, theme::ERROR(), 1.6, true),
        (icons::TRASH, theme::TEXT_3(), 1.5, false),
    ];
    for (i, (path, color, stroke, fill)) in buttons.iter().enumerate() {
        let bx = tb.x0 + i as f32 * (tb.btn + tb.gap);
        let enabled = if i == 0 {
            true
        } else if i == 4 {
            running
        } else if i == TB_CLEAR_SESSION as usize {
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
    // ---- Breakpoints section ----
    let row_h = layout::LINE_H();
    let breakpoints = ctx.dbg.breakpoint_locations();
    let bp_count = breakpoints.len();
    let bp_label_y = debug_breakpoint_label_y();
    let bp_title = if bp_count == 1 {
        "BREAKPOINTS 1".to_string()
    } else {
        format!("BREAKPOINTS {bp_count}")
    };
    let clear_rect = if bp_count > 0 {
        Some(debug_breakpoint_clear_button_rect())
    } else {
        None
    };
    let title_right = clear_rect.map_or(sx + sw - 14.0, |(x, _, _, _)| x - 8.0);
    let title_x = sx + 14.0;
    let bp_title = fit_head_px(&mut ctx.text, &bp_title, (title_right - title_x).max(0.0), chrome - 2.0);
    ctx.text.queue_ui_sized(title_x, bp_label_y, &bp_title, theme::DIM(), chrome - 2.0, clip);
    if bp_count > 0 {
        let (cx, cy, cw, ch) = clear_rect.unwrap();
        ctx.dl_round(cx, cy, cw, ch, 5.0, theme::BG_4());
        ctx.dl_stroke(cx, cy, cw, ch, 5.0, theme::BORDER_SOFT(), 1.0);
        ctx.dl_icon(cx + 5.0, cy + 5.0, 12.0, 12.0, icons::TRASH, theme::TEXT_3(), 1.5, false);
    }
    let bp_top = debug_breakpoint_rows_top();
    if breakpoints.is_empty() {
        ctx.text.queue_ui_sized(sx + 14.0, bp_top + 2.0, "No breakpoints", theme::TEXT_3(), chrome, clip);
    } else {
        let data_rows = debug_breakpoint_data_rows(bp_count);
        let first = ctx.dbg.breakpoint_window_first(data_rows);
        for (i, bp) in breakpoints.iter().skip(first).take(data_rows).enumerate() {
            let y = bp_top + i as f32 * row_h;
            let ty = y + (row_h - chrome) * 0.5 - 1.0;
            ctx.dl_icon(sx + 13.0, y + (row_h - 10.0) * 0.5, 10.0, 10.0, icons::BREAKPOINT, theme::ERROR(), 0.0, true);
            let file = bp.file.rsplit(['/', '\\']).next().unwrap_or("").to_string();
            let loc = format!(":{}", bp.line);
            let loc_w = ctx.text.measure_ui_sized(&loc, chrome - 1.5).0;
            let loc_x = sx + sw - loc_w - 14.0;
            let name_x = sx + 30.0;
            let name = fit_debug_stack_name(&mut ctx.text, &file, name_x, loc_x, chrome);
            ctx.text.queue_ui_sized(name_x, ty, &name, theme::TEXT_1(), chrome, clip);
            ctx.text.queue_ui_sized(loc_x, ty, &loc, theme::TEXT_4(), chrome - 1.5, clip);
        }
        let scroll_label = debug_breakpoint_scroll_label(first, bp_count, data_rows);
        if !scroll_label.is_empty() {
            let y = bp_top + data_rows as f32 * row_h;
            let ty = y + (row_h - chrome) * 0.5 - 1.0;
            let shown = fit_head_px(&mut ctx.text, &scroll_label, (sx + sw - 16.0 - (sx + 30.0)).max(0.0), chrome);
            ctx.text.queue_ui_sized(sx + 30.0, ty, &shown, theme::TEXT_4(), chrome, clip);
        }
    }

    // ---- Call Stack section ----
    let label_y = debug_stack_label_y(bp_count);
    ctx.text.queue_ui_sized(sx + 14.0, label_y, "CALL STACK", theme::DIM(), chrome - 2.0, clip);
    let top = stack_rows_top(bp_count);
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
            // file:line on the right (dim).
            let loc = format!("{file}:{line}");
            let loc_max = (sw * 0.42).clamp(48.0, 86.0);
            let lc = fit_debug_stack_location(&mut ctx.text, &loc, loc_max, chrome - 1.5);
            let lw = ctx.text.measure_ui_sized(&lc, chrome - 1.5).0;
            let loc_x = sx + sw - lw - 14.0;
            // Function name.
            let name_col = if selected { theme::TEXT() } else { theme::TEXT_1() };
            let name_x = sx + 30.0;
            let nm = fit_debug_stack_name(&mut ctx.text, &name, name_x, loc_x, chrome);
            ctx.text.queue_ui_sized(name_x, ty, &nm, name_col, chrome, clip);
            ctx.text.queue_ui_sized(loc_x, ty, &lc, theme::TEXT_4(), chrome - 1.5, clip);
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
            let name_budget = debug_variable_name_budget(&mut ctx.text, 12, chrome);
            let nm = fit_debug_variable_name(&mut ctx.text, &name, name_budget, chrome);
            ctx.text.queue_ui_sized(sx + 16.0, ty, &nm, theme::SYN_FUNCTION(), chrome, clip);
            let sep = debug_variable_separator_advance(&mut ctx.text, chrome);
            let eq_w = debug_ui_text_width(&mut ctx.text, "=", chrome);
            let space_w = ((sep - eq_w) * 0.5).max(0.0);
            let eq_x = sx + 16.0 + debug_ui_text_width(&mut ctx.text, &nm, chrome) + space_w;
            ctx.text.queue_ui_sized(eq_x, ty, "=", theme::TEXT_4(), chrome, clip);
            let val_x = eq_x + eq_w + space_w;
            let kind_w = if kind.is_empty() {
                0.0
            } else {
                debug_ui_text_width(&mut ctx.text, &kind, chrome - 2.0)
            };
            let kind_x = sx + sw - kind_w - 12.0;
            let value_right = if kind.is_empty() { sx + sw - 14.0 } else { kind_x - 8.0 };
            let vv = fit_debug_variable_value(&mut ctx.text, &value, (value_right - val_x).max(0.0), chrome);
            ctx.text.queue_ui_sized(val_x, ty, &vv, theme::SYN_STRING(), chrome, clip);
            // type badge at the right.
            if !kind.is_empty() {
                ctx.text.queue_ui_sized(kind_x, ty, &kind, theme::TEXT_4(), chrome - 2.0, clip);
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
            let text_x = sx + 14.0;
            let max_w = (sx + sw - 12.0 - text_x).max(0.0);
            let t = fit_debug_console_line(&mut ctx.text, &l.text, max_w, chrome - 0.5);
            ctx.text.queue_ui_sized(text_x, ty, &t, col, chrome - 0.5, clip);
        }
    }
}
