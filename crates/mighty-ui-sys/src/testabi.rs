//! Scalar `mui_test_*` ABI for the Test panel + its Vivid-Modern results view.
//!
//! Same shim-owns-everything, scalar-only shape as the rest of the IDE ABI
//! (L17): Mighty runs / stops `mty test`, reads back the running state + parsed
//! counts + per-row status/name, pumps the run each frame, jumps the editor on a
//! row click, and draws the Testing view. All state + work lives in
//! [`crate::tests_panel`].
//!
//! The Testing view is a sidebar panel (rail slot [`crate::PANEL_TEST`], the
//! beaker icon) styled like the Source-Control / Debug panels: a **header** with
//! a Run/Re-run button + a colored pass/fail summary bar, then a **results tree**
//! — one row per `test NAME ... ok|FAILED` with a green check / red x icon, the
//! short test name, and (for failures) the assertion/trap message on a wrapped
//! detail row. A failed row whose declaration we can locate is clickable to jump
//! the editor to its `fn` definition.

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

/// The active tab's file path, or `None` (scratch / no path).
fn active_path(ctx: &MuiContext) -> Option<std::path::PathBuf> {
    ctx.tabs.active_path()
}

// ===========================================================================
// Run / stop lifecycle (Ctrl+Shift+T / "Run Tests")
// ===========================================================================

/// Run `mty test` over the active file's package on a background thread. Opens
/// the Testing view + clears prior results. Returns `1` if the process spawned,
/// else `0` (no file / spawn error). The IDE then pumps + draws each frame.
#[no_mangle]
pub extern "C" fn mui_test_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = active_path(ctx) else {
        ctx.tests_panel.open();
        ctx.active_panel = crate::PANEL_TEST;
        ctx.sidebar_visible = true;
        return 0;
    };
    ctx.active_panel = crate::PANEL_TEST;
    ctx.sidebar_visible = true;
    if ctx.tests_panel.start(&path, None) {
        println!("test: started `mty test` in {}", ctx.tests_panel.pkg());
        1
    } else {
        0
    }
}

/// Run tests with the test under the cursor recorded as the highlight focus.
/// `mty test` has no name filter, so this re-runs the whole package; the focused
/// name is stored so the UI can mark that row. Returns `1` if spawned.
#[no_mangle]
pub extern "C" fn mui_test_run_at_cursor(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = active_path(ctx) else {
        return 0;
    };
    // Find the nearest enclosing `fn test_*` above the cursor in the live model.
    let focus = nearest_test_fn(ctx);
    ctx.active_panel = crate::PANEL_TEST;
    ctx.sidebar_visible = true;
    if ctx.tests_panel.start(&path, focus) {
        println!(
            "test: started `mty test` (focus={}) in {}",
            ctx.tests_panel.focus_test(),
            ctx.tests_panel.pkg()
        );
        1
    } else {
        0
    }
}

/// Scan the active model upward from the cursor for the enclosing `fn test_*`
/// name, so "Run Test at Cursor" can highlight it.
fn nearest_test_fn(ctx: &MuiContext) -> Option<String> {
    let model = ctx.tabs.active_model();
    let cur = model.cursor_line();
    let mut line = cur as i64;
    while line >= 0 {
        let text = model.line(line as usize);
        let t = text.trim_start();
        if let Some(rest) = t.strip_prefix("fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.starts_with("test_") {
                return Some(name);
            }
        }
        line -= 1;
    }
    None
}

/// Stop the running `mty test` (best-effort kill). No-op if idle.
#[no_mangle]
pub extern "C" fn mui_test_stop(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.tests_panel.stop();
    }
}

/// Toggle the Testing view open/closed (the beaker rail icon). Returns `1` if
/// now the active panel.
#[no_mangle]
pub extern "C" fn mui_test_toggle(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.active_panel == crate::PANEL_TEST {
        ctx.active_panel = crate::PANEL_EXPLORER;
        ctx.tests_panel.close();
        0
    } else {
        ctx.active_panel = crate::PANEL_TEST;
        ctx.sidebar_visible = true;
        ctx.tests_panel.open();
        1
    }
}

/// `1` while `mty test` is still running, else `0`.
#[no_mangle]
pub extern "C" fn mui_test_running(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.tests_panel.is_running()))
}

/// Drain pending output into the results tree. Returns `1` if anything changed
/// this frame (the IDE redraws). Call once per frame while the panel is open.
#[no_mangle]
pub extern "C" fn mui_test_pump(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let changed = c.tests_panel.pump();
    if c.tests_panel.take_just_finished() {
        let passed = c.tests_panel.passed();
        let failed = c.tests_panel.failed();
        if failed == 0 {
            c.push_toast(crate::toast::Kind::Success, format!("{passed} tests passed"));
        } else {
            c.push_toast(crate::toast::Kind::Error, format!("{failed} of {} tests failed", passed + failed));
        }
    }
    i32::from(changed)
}

// ===========================================================================
// Summary read-back
// ===========================================================================

/// Number of passing tests.
#[no_mangle]
pub extern "C" fn mui_test_passed(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tests_panel.passed() as i32)
}

/// Number of failing tests.
#[no_mangle]
pub extern "C" fn mui_test_failed(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tests_panel.failed() as i32)
}

/// Total tests (summary total once parsed, else the live row count).
#[no_mangle]
pub extern "C" fn mui_test_total(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tests_panel.total() as i32)
}

/// Last run's wall-clock duration in milliseconds.
#[no_mangle]
pub extern "C" fn mui_test_duration_ms(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tests_panel.duration_ms() as i32)
}

// ===========================================================================
// Per-row read-back
// ===========================================================================

/// Number of result rows.
#[no_mangle]
pub extern "C" fn mui_test_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tests_panel.row_count() as i32)
}

/// Status of row `i`: 0 pending, 1 passed, 2 failed; `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_test_row_status(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tests_panel.row(i as usize).map_or(-1, |r| r.status.as_i32())
    })
}

/// Length (chars) of row `i`'s short test name, or `-1`.
#[no_mangle]
pub extern "C" fn mui_test_row_name_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tests_panel
            .row(i as usize)
            .map_or(-1, |r| r.short_name.chars().count() as i32)
    })
}

/// `j`-th char (codepoint) of row `i`'s short name, or `-1`.
#[no_mangle]
pub extern "C" fn mui_test_row_name_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tests_panel
            .row(i as usize)
            .and_then(|r| r.short_name.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

/// `1` if row `i` is clickable (its `fn` declaration is locatable in the
/// package's tests), else `0`.
#[no_mangle]
pub extern "C" fn mui_test_row_clickable(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| {
        i32::from(c.tests_panel.resolve_row_target(i as usize).is_some())
    })
}

// ===========================================================================
// Click routing + click-to-jump
// ===========================================================================

/// Map the last click's pixel position to a results-tree row index, or `-1` if
/// the click was not on a row. Accounts for the per-failure detail lines (a
/// failed row with a message occupies two visual rows).
#[no_mangle]
pub extern "C" fn mui_test_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_TEST {
        return -1;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let sx0 = layout::RAIL_W;
    let sx1 = layout::RAIL_W + layout::SIDEBAR_W;
    if x < sx0 || x > sx1 {
        return -1;
    }
    let top = rows_top();
    if y < top {
        return -1;
    }
    let row_h = layout::LINE_H();
    // Walk the visual rows, accounting for detail lines, to find the hit row.
    let first = ctx.tests_panel.first();
    let count = ctx.tests_panel.row_count();
    let mut yy = top;
    for idx in first..count {
        let has_detail = ctx
            .tests_panel
            .row(idx)
            .map(|r| !r.message.is_empty())
            .unwrap_or(false);
        let span = if has_detail { row_h * 2.0 } else { row_h };
        if y >= yy && y < yy + span {
            return idx as i32;
        }
        yy += span;
    }
    -1
}

/// Resolve + record the clicked row `i`'s jump target (the test fn declaration)
/// and return `1` if locatable; the IDE then reads `mui_test_click_*` and opens
/// the file + jumps. `0` if the row has no resolvable location.
#[no_mangle]
pub extern "C" fn mui_test_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        return 0;
    }
    let Some((full, line, col)) = ctx.tests_panel.resolve_row_target(i as usize) else {
        ctx.tests_panel.set_click_target(None);
        return 0;
    };
    if !full.exists() {
        ctx.tests_panel.set_click_target(None);
        return 0;
    }
    let _idx = ctx.tabs.open_path(full.clone());
    crate::abi::sync_active_path(ctx);
    ctx.tests_panel
        .set_click_target(Some((full.to_string_lossy().into_owned(), line, col)));
    1
}

/// The 0-based target line of the last `mui_test_open_row`, or `-1`.
#[no_mangle]
pub extern "C" fn mui_test_click_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.tests_panel.click_target().map_or(-1, |t| t.1))
}

/// The 0-based target column of the last `mui_test_open_row`, or `-1`.
#[no_mangle]
pub extern "C" fn mui_test_click_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.tests_panel.click_target().map_or(-1, |t| t.2))
}

/// The active-tab index after `mui_test_open_row` opened the target file, so the
/// IDE can switch its model. `-1` if no pending click.
#[no_mangle]
pub extern "C" fn mui_test_click_tab(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        if c.tests_panel.click_target().is_some() {
            c.tabs.active() as i32
        } else {
            -1
        }
    })
}

/// Scroll the results tree by `delta` rows.
#[no_mangle]
pub extern "C" fn mui_test_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.tests_panel.scroll(delta);
    }
}

// ===========================================================================
// Drawing — the Testing view sidebar panel
// ===========================================================================

/// Header height (matches the SCM/Debug panels' header band).
const HEAD_H: f32 = 40.0;

/// Y pixel (top) of the first result row.
fn rows_top() -> f32 {
    // header + toolbar row + summary bar + section label.
    HEAD_H + 8.0 + 30.0 + 8.0 + 22.0 + 20.0
}

/// Geometry of the toolbar Run/Re-run + Stop buttons (under the header).
struct ToolbarGeom {
    run_x: f32,
    stop_x: f32,
    y: f32,
    btn_w: f32,
    btn_h: f32,
}

fn toolbar_geom() -> ToolbarGeom {
    let sx = layout::RAIL_W;
    ToolbarGeom {
        run_x: sx + 12.0,
        stop_x: sx + 12.0 + 96.0 + 8.0,
        y: HEAD_H + 8.0,
        btn_w: 96.0,
        btn_h: 30.0,
    }
}

/// Toolbar action codes returned by [`mui_test_toolbar_at_click`].
pub const TB_RUN: i32 = 1;
pub const TB_STOP: i32 = 2;

/// Map the last click to a Test toolbar action (`TB_RUN` / `TB_STOP`), or `-1`.
#[no_mangle]
pub extern "C" fn mui_test_toolbar_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_TEST {
        return -1;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let tb = toolbar_geom();
    if y < tb.y || y > tb.y + tb.btn_h {
        return -1;
    }
    if x >= tb.run_x && x <= tb.run_x + tb.btn_w {
        return TB_RUN;
    }
    if x >= tb.stop_x && x <= tb.stop_x + tb.btn_w {
        return TB_STOP;
    }
    -1
}

/// Draw the Testing view sidebar panel (toolbar + summary bar + results tree).
/// No-op when the sidebar is hidden or this panel isn't active.
#[no_mangle]
pub extern "C" fn mui_test_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_TEST {
        return;
    }
    use crate::icons;
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let adv = chrome * 0.55;
    let sx = layout::RAIL_W;
    let sw = layout::SIDEBAR_W;
    let row_h = layout::LINE_H();

    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // Header: beaker icon + "TESTING" + a state pill.
    ctx.dl_rect(sx, 0.0, sw, HEAD_H, theme::BG_2());
    ctx.dl_rect(sx, HEAD_H - 1.0, sw, 1.0, theme::BORDER_SOFT());
    ctx.dl_icon(sx + 12.0, (HEAD_H - 15.0) * 0.5, 15.0, 15.0, icons::BEAKER, theme::ACCENT_BRIGHT(), 1.5, false);
    let title = "TESTING";
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ctx.text.queue_ui_sized(sx + 34.0, (HEAD_H - (chrome - 2.0)) * 0.5 - 1.0, &tracked, theme::DIM(), chrome - 2.0, clip);

    let (state_label, state_col) = if ctx.tests_panel.is_running() {
        ("running\u{2026}", theme::WARNING())
    } else if ctx.tests_panel.total() == 0 {
        ("idle", theme::TEXT_3())
    } else if ctx.tests_panel.failed() > 0 {
        ("failed", theme::ERROR())
    } else {
        ("passed", theme::GREEN())
    };
    let pill_w = state_label.chars().count() as f32 * (chrome * 0.5) + 18.0;
    let pill_x = sx + sw - pill_w - 12.0;
    let pill_y = (HEAD_H - 17.0) * 0.5;
    ctx.dl_round(pill_x, pill_y, pill_w, 17.0, 6.0, theme::BG_4());
    ctx.text.queue_ui_sized(pill_x + 9.0, pill_y + 2.5, state_label, state_col, chrome - 2.0, clip);

    // Toolbar: Run/Re-run + Stop buttons.
    let tb = toolbar_geom();
    let ran = ctx.tests_panel.total() > 0 || ctx.tests_panel.row_count() > 0;
    let run_label = if ran { "Re-run" } else { "Run Tests" };
    // Run button (accent, with a play/beaker icon).
    ctx.dl_round(tb.run_x, tb.y, tb.btn_w, tb.btn_h, 7.0, theme::accent_a(0.22));
    ctx.dl_stroke(tb.run_x, tb.y, tb.btn_w, tb.btn_h, 7.0, theme::ACCENT(), 1.0);
    ctx.dl_icon(tb.run_x + 9.0, tb.y + (tb.btn_h - 13.0) * 0.5, 13.0, 13.0, icons::RUN, theme::ACCENT_BRIGHT(), 1.6, true);
    ctx.text.queue_ui_sized(tb.run_x + 28.0, tb.y + (tb.btn_h - chrome) * 0.5 - 1.0, run_label, theme::TEXT(), chrome - 1.0, clip);
    // Stop button (enabled only while running).
    let stop_on = ctx.tests_panel.is_running();
    let stop_bg = if stop_on { theme::BG_4() } else { theme::BG_1() };
    let stop_col = if stop_on { theme::ERROR() } else { theme::TEXT_4() };
    ctx.dl_round(tb.stop_x, tb.y, tb.btn_w, tb.btn_h, 7.0, stop_bg);
    ctx.dl_stroke(tb.stop_x, tb.y, tb.btn_w, tb.btn_h, 7.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_icon(tb.stop_x + 9.0, tb.y + (tb.btn_h - 12.0) * 0.5, 12.0, 12.0, icons::DBG_STOP, stop_col, 1.4, true);
    ctx.text.queue_ui_sized(tb.stop_x + 28.0, tb.y + (tb.btn_h - chrome) * 0.5 - 1.0, "Stop", stop_col, chrome - 1.0, clip);

    // Summary line + a proportional pass/fail bar.
    let passed = ctx.tests_panel.passed();
    let failed = ctx.tests_panel.failed();
    let total = ctx.tests_panel.total();
    let sum_y = tb.y + tb.btn_h + 8.0;
    let bar_x = sx + 12.0;
    let bar_w = sw - 24.0;
    let bar_h = 6.0;
    // Track.
    ctx.dl_round(bar_x, sum_y, bar_w, bar_h, 3.0, theme::BG_4());
    if total > 0 {
        let p_frac = passed as f32 / total as f32;
        let f_frac = failed as f32 / total as f32;
        let p_w = (bar_w * p_frac).max(0.0);
        let f_w = (bar_w * f_frac).max(0.0);
        if p_w > 0.0 {
            ctx.dl_round(bar_x, sum_y, p_w, bar_h, 3.0, theme::GREEN());
        }
        if f_w > 0.0 {
            ctx.dl_round(bar_x + p_w, sum_y, f_w, bar_h, 3.0, theme::ERROR());
        }
    }
    // Summary text + duration.
    let summary = if total == 0 && !ctx.tests_panel.is_running() {
        "No tests run yet".to_string()
    } else {
        format!("{passed} passed \u{00b7} {failed} failed \u{00b7} {total} total")
    };
    let sum_text_y = sum_y + bar_h + 4.0;
    ctx.text.queue_ui_sized(bar_x, sum_text_y, &summary, theme::TEXT_1(), chrome - 1.0, clip);
    if ctx.tests_panel.duration_ms() > 0 {
        let dur = format!("{}ms", ctx.tests_panel.duration_ms());
        let dw = dur.chars().count() as f32 * (chrome * 0.5);
        ctx.text.queue_ui_sized(sx + sw - dw - 14.0, sum_text_y, &dur, theme::TEXT_4(), chrome - 1.5, clip);
    }

    // Section label.
    let label_y = sum_text_y + 18.0;
    ctx.text.queue_ui_sized(sx + 14.0, label_y, "RESULTS", theme::DIM(), chrome - 2.0, clip);

    // Results tree.
    let top = rows_top();
    let count = ctx.tests_panel.row_count();
    let first = ctx.tests_panel.first();
    if count == 0 {
        let msg = if ctx.tests_panel.is_running() {
            "Running\u{2026}"
        } else {
            "Run the package's tests to see results."
        };
        ctx.text.queue_ui_sized(sx + 14.0, top + 2.0, msg, theme::TEXT_3(), chrome, clip);
        return;
    }

    let focus = ctx.tests_panel.focus_test().to_string();
    let mut y = top;
    for idx in first..count {
        if y > h - 24.0 {
            break;
        }
        let (status, name, message, suite) = {
            let Some(r) = ctx.tests_panel.row(idx) else { break };
            (r.status, r.short_name.clone(), r.message.clone(), r.suite.clone())
        };
        use crate::tests_panel::Status;
        let (icon, icon_col, fill) = match status {
            Status::Passed => (icons::CHECK, theme::GREEN(), false),
            Status::Failed => (icons::XMARK, theme::ERROR(), false),
            Status::Pending => (icons::DOTS, theme::TEXT_3(), true),
        };
        // Focus highlight: a faint accent wash on the cursor test's row.
        let focused = !focus.is_empty() && focus == name;
        if focused {
            ctx.dl_grad_h(sx + 5.0, y + 1.0, sw - 12.0, row_h - 2.0, 5.0, theme::accent_a(0.16), 0.85);
            ctx.dl_rect(sx + 5.0, y + 1.0, 2.0, row_h - 2.0, theme::ACCENT());
        }
        let ty = y + (row_h - chrome) * 0.5 - 1.0;
        ctx.dl_icon(sx + 12.0, y + (row_h - 13.0) * 0.5, 13.0, 13.0, icon, icon_col, 1.8, fill);
        // Test name (failed rows are clickable -> info-tinted).
        let clickable = status == Status::Failed && !message.is_empty();
        let name_col = if clickable {
            theme::INFO()
        } else if focused {
            theme::TEXT()
        } else {
            theme::TEXT_1()
        };
        let mut nm = name;
        let navail = ((sw - 100.0) / adv).floor() as usize;
        if nm.chars().count() > navail && navail > 1 {
            nm = nm.chars().take(navail - 1).collect::<String>() + "\u{2026}";
        }
        ctx.text.queue_ui_sized(sx + 32.0, ty, &nm, name_col, chrome, clip);
        // Suite badge on the right (dim).
        if !suite.is_empty() {
            let mut sb = suite;
            let savail = 12usize;
            if sb.chars().count() > savail {
                sb = sb.chars().rev().take(savail - 1).collect::<Vec<_>>().into_iter().rev().collect::<String>();
            }
            let sbw = sb.chars().count() as f32 * (chrome * 0.45);
            ctx.text.queue_ui_sized(sx + sw - sbw - 14.0, ty, &sb, theme::TEXT_4(), chrome - 2.0, clip);
        }
        y += row_h;
        // Failure message on a wrapped detail row beneath the failed test.
        if !message.is_empty() {
            let dy = y + (row_h - (chrome - 1.0)) * 0.5 - 1.0;
            // A thin red rail to the left of the detail.
            ctx.dl_rect(sx + 32.0, y + 2.0, 2.0, row_h - 4.0, theme::error_wash(0.7));
            let mut dm = message;
            let davail = ((sw - 44.0) / (adv * 0.92)).floor() as usize;
            if dm.chars().count() > davail && davail > 1 {
                dm = dm.chars().take(davail - 1).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(sx + 40.0, dy, &dm, theme::ERROR(), chrome - 1.5, clip);
            y += row_h;
        }
    }
}
