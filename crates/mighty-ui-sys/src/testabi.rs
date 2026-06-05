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

fn active_test_target_label(ctx: &MuiContext) -> String {
    active_path(ctx)
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(scratch)")
        .to_string()
}

fn test_command_display() -> String {
    let mty = crate::mty::path();
    let program = std::path::Path::new(&mty)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(mty.as_str());
    format!("{program} test")
}

fn compact_test_start_reason(ctx: &MuiContext) -> Option<String> {
    let row = ctx.tests_panel.row(0)?;
    row.message
        .split_once(": ")
        .map(|(_, reason)| reason.trim().to_string())
        .filter(|reason| !reason.is_empty())
}

fn test_start_failed_message(path: &std::path::Path, reason: Option<&str>) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let base = format!("Test run failed to start: {name} via {}", test_command_display());
    match reason.map(str::trim).filter(|s| !s.is_empty()) {
        Some(reason) => format!("{base}: {reason}"),
        None => base,
    }
}

fn test_target_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .to_string()
}

fn stale_test_target_reason(path: &std::path::Path) -> Option<String> {
    let label = test_target_label(path);
    match test_target_kind(path) {
        TestTargetKind::File => None,
        TestTargetKind::Missing => Some(format!("target missing: {label}")),
        TestTargetKind::NotFile => Some(format!("target is not a file: {label}")),
    }
}

enum TestTargetKind {
    File,
    Missing,
    NotFile,
}

fn test_target_kind(path: &std::path::Path) -> TestTargetKind {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => TestTargetKind::File,
        Ok(_) => TestTargetKind::NotFile,
        Err(_) => TestTargetKind::Missing,
    }
}

fn reject_bad_test_target(
    ctx: &mut MuiContext,
    path: &std::path::Path,
    kind: TestTargetKind,
) -> i32 {
    let name = crate::abi::file_target_name(path);
    crate::abi::refresh_workspace_file_views(ctx);
    let message = match kind {
        TestTargetKind::Missing => format!("Test target missing: {name}"),
        TestTargetKind::NotFile => format!("Test target is not a file: {name}"),
        TestTargetKind::File => return 0,
    };
    ctx.push_toast(crate::toast::Kind::Warn, message);
    0
}

fn fail_test_before_start(ctx: &mut MuiContext, path: &std::path::Path, focus: Option<String>) {
    let Some(reason) = stale_test_target_reason(path) else {
        return;
    };
    ctx.tests_panel.fail_before_start(path, focus, reason);
    let reason = compact_test_start_reason(ctx);
    ctx.push_toast(
        crate::toast::Kind::Error,
        test_start_failed_message(path, reason.as_deref()),
    );
}

pub(crate) fn workspace_test_target_for_root(root: &std::path::Path) -> Option<std::path::PathBuf> {
    if !workspace_root_is_searchable(root) {
        return None;
    }
    let manifest = root.join("mighty.toml");
    if manifest.is_file() {
        return Some(manifest);
    }

    let mut manifests = Vec::new();
    collect_workspace_candidates(root, root, &mut manifests, CandidateKind::Manifest, 3);
    manifests.sort_by(|a, b| candidate_rank(root, a).cmp(&candidate_rank(root, b)));
    if let Some(path) = manifests.into_iter().next() {
        return Some(path);
    }

    let mut tests = Vec::new();
    collect_workspace_candidates(root, root, &mut tests, CandidateKind::TestFile, 5);
    tests.sort_by(|a, b| candidate_rank(root, a).cmp(&candidate_rank(root, b)));
    if let Some(path) = tests.into_iter().next() {
        return Some(path);
    }

    let mut files = Vec::new();
    collect_workspace_candidates(root, root, &mut files, CandidateKind::MightyFile, 4);
    files.sort_by(|a, b| candidate_rank(root, a).cmp(&candidate_rank(root, b)));
    files.into_iter().next()
}

fn workspace_root_is_searchable(root: &std::path::Path) -> bool {
    !root.as_os_str().is_empty()
        && std::fs::metadata(root).is_ok_and(|meta| meta.is_dir())
}

fn workspace_test_target(ctx: &MuiContext) -> Option<std::path::PathBuf> {
    workspace_test_target_for_root(&crate::wsabi::effective_root(ctx))
}

#[derive(Clone, Copy)]
enum CandidateKind {
    Manifest,
    TestFile,
    MightyFile,
}

fn collect_workspace_candidates(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
    kind: CandidateKind,
    depth_left: usize,
) {
    if depth_left == 0 || should_skip_workspace_dir(root, dir) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_workspace_candidates(root, &path, out, kind, depth_left - 1);
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let is_mty = path.extension().and_then(|e| e.to_str()) == Some("mty");
        let matched = match kind {
            CandidateKind::Manifest => name == "mighty.toml",
            CandidateKind::TestFile => is_mty && name.ends_with(".test.mty"),
            CandidateKind::MightyFile => is_mty,
        };
        if matched {
            out.push(path);
        }
    }
}

fn should_skip_workspace_dir(root: &std::path::Path, dir: &std::path::Path) -> bool {
    if dir == root {
        return false;
    }
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    matches!(
        name,
        ".git" | "target" | "dist" | "node_modules" | ".venv" | "__pycache__"
    )
}

fn candidate_rank(root: &std::path::Path, path: &std::path::Path) -> (usize, i32, String) {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let depth = rel.components().count();
    let rel_s = rel.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let priority = if rel_s == "mighty.toml" {
        0
    } else if rel_s.starts_with("tests/") {
        1
    } else if rel_s.starts_with("src/") {
        2
    } else {
        3
    };
    (depth, priority, rel_s)
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
    let active = active_path(ctx);
    let Some(path) = active.clone().or_else(|| workspace_test_target(ctx)) else {
        ctx.tests_panel.open();
        ctx.active_panel = crate::PANEL_TEST;
        ctx.sidebar_visible = true;
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!(
                "Save {} or open a Mighty folder before running tests",
                active_test_target_label(ctx)
            ),
        );
        crate::abi::trace("test_run no_target");
        return 0;
    };
    ctx.active_panel = crate::PANEL_TEST;
    ctx.sidebar_visible = true;
    if active.is_some() && stale_test_target_reason(&path).is_some() {
        fail_test_before_start(ctx, &path, None);
        crate::abi::trace(&format!("test_run stale_target target={}", path.display()));
        return 0;
    }
    if ctx.tests_panel.start(&path, None) {
        println!("test: started `mty test` in {}", ctx.tests_panel.pkg());
        crate::abi::trace(&format!("test_run start target={}", path.display()));
        1
    } else {
        let reason = compact_test_start_reason(ctx);
        ctx.push_toast(
            crate::toast::Kind::Error,
            test_start_failed_message(&path, reason.as_deref()),
        );
        crate::abi::trace(&format!("test_run failed target={}", path.display()));
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
        ctx.tests_panel.open();
        ctx.active_panel = crate::PANEL_TEST;
        ctx.sidebar_visible = true;
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!(
                "Save {} before running test at cursor",
                active_test_target_label(ctx)
            ),
        );
        crate::abi::trace("test_run_at_cursor no_target");
        return 0;
    };
    // Find the nearest enclosing `fn test_*` above the cursor in the live model.
    let focus = nearest_test_fn(ctx);
    ctx.active_panel = crate::PANEL_TEST;
    ctx.sidebar_visible = true;
    if stale_test_target_reason(&path).is_some() {
        fail_test_before_start(ctx, &path, focus);
        crate::abi::trace(&format!(
            "test_run_at_cursor stale_target target={}",
            path.display()
        ));
        return 0;
    }
    if ctx.tests_panel.start(&path, focus) {
        println!(
            "test: started `mty test` (focus={}) in {}",
            ctx.tests_panel.focus_test(),
            ctx.tests_panel.pkg()
        );
        1
    } else {
        let reason = compact_test_start_reason(ctx);
        ctx.push_toast(
            crate::toast::Kind::Error,
            test_start_failed_message(&path, reason.as_deref()),
        );
        crate::abi::trace(&format!("test_run_at_cursor failed target={}", path.display()));
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

/// Stop the running `mty test` (best-effort kill). Returns `1` when a run was
/// stopped. If idle, opens Testing, reports the no-op, and returns `0`.
#[no_mangle]
pub extern "C" fn mui_test_stop(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.tests_panel.open();
    ctx.active_panel = crate::PANEL_TEST;
    ctx.sidebar_visible = true;
    if ctx.tests_panel.is_running() {
        ctx.tests_panel.stop();
        ctx.push_toast(crate::toast::Kind::Info, "Test run stopped");
        crate::abi::trace("test_stop running");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No test run to stop");
        crate::abi::trace("test_stop idle");
        0
    }
}

/// Clear parsed Test results without stopping a running `mty test`. Returns how
/// many result rows were removed.
#[no_mangle]
pub extern "C" fn mui_test_clear(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.tests_panel.open();
    ctx.active_panel = crate::PANEL_TEST;
    ctx.sidebar_visible = true;
    let cleared = ctx.tests_panel.clear_results() as i32;
    if cleared > 0 {
        ctx.push_toast(crate::toast::Kind::Info, "Test results cleared");
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Test results already empty");
    }
    crate::abi::trace(&format!("test_clear rows={cleared}"));
    cleared
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

/// Close the Testing panel without stopping a running test process or clearing
/// parsed results. Returns `1` when it closed Testing, or `0` when already closed.
#[no_mangle]
pub extern "C" fn mui_test_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tests_panel.is_active() || ctx.active_panel == crate::PANEL_TEST {
        ctx.tests_panel.close();
        if ctx.active_panel == crate::PANEL_TEST {
            ctx.active_panel = crate::PANEL_EXPLORER;
        }
        ctx.push_toast(crate::toast::Kind::Info, "Testing panel closed");
        crate::abi::trace("test_close");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Testing panel is already closed");
    crate::abi::trace("test_close noop");
    0
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
    let sx1 = layout::sidebar_right();
    if x < sx0 || x > sx1 {
        return -1;
    }
    let top = rows_top(sx1 - sx0);
    if y < top {
        return -1;
    }
    let row_h = layout::LINE_H();
    let chrome = theme::CHROME_FONT_SIZE;
    // Walk the visual rows, accounting for wrapped detail lines, to find the hit row.
    let first = ctx.tests_panel.first();
    let count = ctx.tests_panel.row_count();
    let mut yy = top;
    for idx in first..count {
        let message = ctx
            .tests_panel
            .row(idx)
            .map(|r| r.message.clone())
            .unwrap_or_default();
        let detail_rows = detail_visual_rows(&mut ctx.text, &message, sx1 - sx0, chrome - 1.5);
        let span = row_h * (1 + detail_rows) as f32;
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
    ctx.tests_panel.set_click_target(None);
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No test result row selected");
        return 0;
    }
    let Some(row) = ctx.tests_panel.row(i as usize) else {
        ctx.push_toast(crate::toast::Kind::Info, "Test result row no longer listed");
        return 0;
    };
    let short = row.short_name.trim();
    let full = row.full_name.trim();
    let row_name = if !short.is_empty() {
        short
    } else if !full.is_empty() {
        full
    } else {
        "row"
    }
    .to_string();
    let Some((full, line, col)) = ctx.tests_panel.resolve_row_target(i as usize) else {
        let expected_suite = if !ctx.tests_panel.pkg().trim().is_empty()
            && !row.suite.trim().is_empty()
        {
            Some(
                std::path::PathBuf::from(ctx.tests_panel.pkg())
                    .join("tests")
                    .join(&row.suite),
            )
        } else {
            None
        };
        crate::abi::refresh_workspace_file_views(ctx);
        if let Some(path) = expected_suite {
            match test_target_kind(&path) {
                TestTargetKind::File => {}
                kind @ (TestTargetKind::Missing | TestTargetKind::NotFile) => {
                    return reject_bad_test_target(ctx, &path, kind);
                }
            }
        }
        ctx.push_toast(
            crate::toast::Kind::Info,
            format!("Test result row has no file target: {row_name}"),
        );
        return 0;
    };
    match test_target_kind(&full) {
        TestTargetKind::File => {}
        kind @ (TestTargetKind::Missing | TestTargetKind::NotFile) => {
            return reject_bad_test_target(ctx, &full, kind);
        }
    }
    let _idx = crate::abi::open_path_in_focused_pane(ctx, full.clone());
    crate::abi::record_opened_file(ctx, &full);
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
fn rows_top(sidebar_w: f32) -> f32 {
    // header + toolbar row + summary bar + section label.
    let summary_h = if compact_testing_summary(sidebar_w) { 38.0 } else { 22.0 };
    HEAD_H + 8.0 + 30.0 + 8.0 + summary_h + 20.0
}

fn fit_ui_text(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    let max_px = max_px.max(0.0);
    if max_px <= 1.0 {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    let ellipsis_w = text.measure_ui_sized(ellipsis, size).0;
    if ellipsis_w >= max_px {
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
        return ellipsis.to_string();
    }
    let mut out: String = chars.iter().take(lo).collect();
    out.push_str(ellipsis);
    out
}

fn wrap_detail_lines(
    text: &mut crate::text::Text,
    s: &str,
    max_px: f32,
    size: f32,
    max_lines: usize,
) -> Vec<String> {
    let max_lines = max_lines.max(1);
    if s.trim().is_empty() || max_px <= 1.0 {
        return Vec::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return vec![s.to_string()];
    }
    if max_lines == 1 {
        return vec![fit_ui_text(text, s, max_px, size)];
    }

    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= 1 {
        return vec![fit_ui_text(text, s, max_px, size)];
    }

    let mut first = String::new();
    let mut consumed = 0usize;
    for word in &words {
        let candidate = if first.is_empty() {
            (*word).to_string()
        } else {
            format!("{first} {word}")
        };
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            first = candidate;
            consumed += 1;
        } else {
            break;
        }
    }
    if first.is_empty() {
        return vec![fit_ui_text(text, s, max_px, size)];
    }
    let rest = words[consumed..].join(" ");
    if rest.is_empty() {
        vec![first]
    } else {
        vec![first, fit_ui_text(text, &rest, max_px, size)]
    }
}

fn detail_visual_rows(
    text: &mut crate::text::Text,
    message: &str,
    sidebar_w: f32,
    size: f32,
) -> usize {
    if message.is_empty() {
        0
    } else {
        wrap_detail_lines(text, message, sidebar_w - 54.0, size, 2)
            .len()
            .max(1)
    }
}

fn testing_header_title_budget(sw: f32, state_pill_w: f32) -> f32 {
    // Sidebar-local pixels between the title start (rail + icon + 34) and the
    // right-side state pill. Keep an 8px breathing gap before the pill.
    (sw - 34.0 - 12.0 - state_pill_w - 8.0).max(0.0)
}

fn testing_suite_budget(sw: f32) -> f32 {
    if sw < 220.0 {
        0.0
    } else {
        (sw * 0.30).clamp(42.0, 76.0)
    }
}

fn testing_run_label(ran: bool, compact: bool) -> &'static str {
    if compact {
        "Run"
    } else if ran {
        "Re-run"
    } else {
        "Run Tests"
    }
}

fn testing_stop_label_size(compact: bool) -> f32 {
    let chrome = theme::CHROME_FONT_SIZE;
    if compact { chrome - 2.0 } else { chrome - 1.0 }
}

fn compact_testing_summary(sidebar_w: f32) -> bool {
    sidebar_w < 220.0
}

fn testing_summary_lines(passed: usize, failed: usize, total: usize, running: bool, sidebar_w: f32) -> Vec<String> {
    if total == 0 && !running {
        return vec!["No tests run yet".to_string()];
    }
    if compact_testing_summary(sidebar_w) {
        vec![
            format!("{passed} passed \u{00b7} {failed} failed"),
            format!("{total} total"),
        ]
    } else {
        vec![format!("{passed} passed \u{00b7} {failed} failed \u{00b7} {total} total")]
    }
}

fn testing_state_label(running: bool, final_summary: bool, total: usize, failed: usize) -> &'static str {
    if running && !final_summary {
        "running\u{2026}"
    } else if running {
        "finalizing\u{2026}"
    } else if total == 0 {
        "idle"
    } else if failed > 0 {
        "failed"
    } else {
        "passed"
    }
}

/// Geometry of the toolbar Run/Re-run + Stop + Clear buttons (under the header).
struct ToolbarGeom {
    run_x: f32,
    stop_x: f32,
    clear_x: f32,
    y: f32,
    btn_w: f32,
    clear_w: f32,
    btn_h: f32,
    compact: bool,
}

fn toolbar_geom() -> ToolbarGeom {
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    let gap = 8.0;
    let clear_w = 32.0;
    let btn_w = ((sw - 24.0 - clear_w - gap * 2.0) / 2.0).clamp(72.0, 96.0);
    let compact = btn_w < 90.0;
    let run_x = sx + 12.0;
    let stop_x = run_x + btn_w + gap;
    ToolbarGeom {
        run_x,
        stop_x,
        clear_x: stop_x + btn_w + gap,
        y: HEAD_H + 8.0,
        btn_w,
        clear_w,
        btn_h: 30.0,
        compact,
    }
}

/// Toolbar action codes returned by [`mui_test_toolbar_at_click`].
pub const TB_RUN: i32 = 1;
pub const TB_STOP: i32 = 2;
pub const TB_CLEAR: i32 = 3;

/// Map the last click to a Test toolbar action, or `-1`.
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
    if x >= tb.clear_x && x <= tb.clear_x + tb.clear_w {
        return TB_CLEAR;
    }
    -1
}

#[cfg(test)]
pub(crate) fn test_toolbar_clear_rect() -> (f32, f32, f32, f32) {
    let tb = toolbar_geom();
    (tb.clear_x, tb.y, tb.clear_w, tb.btn_h)
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
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    let row_h = layout::LINE_H();

    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // Header: beaker icon + "TESTING" + a state pill.
    ctx.dl_rect(sx, 0.0, sw, HEAD_H, theme::BG_2());
    ctx.dl_rect(sx, HEAD_H - 1.0, sw, 1.0, theme::BORDER_SOFT());
    let passed = ctx.tests_panel.passed();
    let failed = ctx.tests_panel.failed();
    let total = ctx.tests_panel.total();
    let running = ctx.tests_panel.is_running();
    let final_summary = ctx.tests_panel.has_final_summary();
    let visual_running = running && !final_summary;
    let state_label = testing_state_label(running, final_summary, total, failed);
    let state_col = if running {
        theme::WARNING()
    } else if total == 0 {
        theme::TEXT_3()
    } else if failed > 0 {
        theme::ERROR()
    } else {
        theme::GREEN()
    };
    let (state_w, _) = ctx.text.measure_ui_sized(state_label, chrome - 2.0);
    let pill_w = state_w + 18.0;
    let pill_x = sx + sw - pill_w - 12.0;
    let pill_y = (HEAD_H - 17.0) * 0.5;

    ctx.dl_icon(sx + 12.0, (HEAD_H - 15.0) * 0.5, 15.0, 15.0, icons::BEAKER, theme::ACCENT_BRIGHT(), 1.5, false);
    let title = "TESTING";
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    let title_fit = fit_ui_text(&mut ctx.text, &tracked, testing_header_title_budget(sw, pill_w), chrome - 2.0);
    if !title_fit.is_empty() {
        ctx.text.queue_ui_sized(sx + 34.0, (HEAD_H - (chrome - 2.0)) * 0.5 - 1.0, &title_fit, theme::DIM(), chrome - 2.0, clip);
    }
    ctx.dl_round(pill_x, pill_y, pill_w, 17.0, 6.0, theme::BG_4());
    ctx.text.queue_ui_sized(pill_x + 9.0, pill_y + 2.5, state_label, state_col, chrome - 2.0, clip);

    // Toolbar: Run/Re-run + Stop + Clear buttons.
    let tb = toolbar_geom();
    let ran = ctx.tests_panel.total() > 0 || ctx.tests_panel.row_count() > 0;
    let run_label = testing_run_label(ran, tb.compact);
    // Run button (accent, with a play/beaker icon).
    ctx.dl_round(tb.run_x, tb.y, tb.btn_w, tb.btn_h, 7.0, theme::accent_a(0.22));
    ctx.dl_stroke(tb.run_x, tb.y, tb.btn_w, tb.btn_h, 7.0, theme::ACCENT(), 1.0);
    ctx.dl_icon(tb.run_x + 9.0, tb.y + (tb.btn_h - 13.0) * 0.5, 13.0, 13.0, icons::RUN, theme::ACCENT_BRIGHT(), 1.6, true);
    let label_x = if tb.compact { tb.run_x + 29.0 } else { tb.run_x + 28.0 };
    let label_size = if tb.compact { chrome - 2.0 } else { chrome - 1.0 };
    ctx.text.queue_ui_sized(label_x, tb.y + (tb.btn_h - chrome) * 0.5 - 1.0, run_label, theme::TEXT(), label_size, clip);
    // Stop button (enabled only while running).
    let stop_on = ctx.tests_panel.is_running();
    let stop_bg = if stop_on { theme::BG_4() } else { theme::BG_1() };
    let stop_col = if stop_on { theme::ERROR() } else { theme::TEXT_4() };
    ctx.dl_round(tb.stop_x, tb.y, tb.btn_w, tb.btn_h, 7.0, stop_bg);
    ctx.dl_stroke(tb.stop_x, tb.y, tb.btn_w, tb.btn_h, 7.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_icon(tb.stop_x + 9.0, tb.y + (tb.btn_h - 12.0) * 0.5, 12.0, 12.0, icons::DBG_STOP, stop_col, 1.4, true);
    let stop_label_size = testing_stop_label_size(tb.compact);
    ctx.text.queue_ui_sized(tb.stop_x + 28.0, tb.y + (tb.btn_h - stop_label_size) * 0.5 - 1.0, "Stop", stop_col, stop_label_size, clip);
    // Clear button (icon-only): clears parsed results without stopping a run.
    ctx.dl_round(tb.clear_x, tb.y, tb.clear_w, tb.btn_h, 7.0, theme::BG_4());
    ctx.dl_stroke(tb.clear_x, tb.y, tb.clear_w, tb.btn_h, 7.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_icon(
        tb.clear_x + (tb.clear_w - 13.0) * 0.5,
        tb.y + (tb.btn_h - 13.0) * 0.5,
        13.0,
        13.0,
        icons::TRASH,
        theme::TEXT_3(),
        1.4,
        false,
    );

    // Summary line + a proportional pass/fail bar.
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
    // Summary text + duration. Compact sidebars keep real words and wrap onto a
    // second line instead of falling back to p/f/t implementation shorthand.
    let summary_lines = testing_summary_lines(passed, failed, total, visual_running, sw);
    let sum_text_y = sum_y + bar_h + 4.0;
    let duration = if ctx.tests_panel.duration_ms() > 0 {
        let dur = format!("{}ms", ctx.tests_panel.duration_ms());
        let (dw, _) = ctx.text.measure_ui_sized(&dur, chrome - 1.5);
        if sw >= 180.0 && !compact_testing_summary(sw) {
            ctx.text.queue_ui_sized(sx + sw - dw - 14.0, sum_text_y, &dur, theme::DIM(), chrome - 1.5, clip);
            dw + 10.0
        } else {
            0.0
        }
    } else {
        0.0
    };
    for (i, summary) in summary_lines.iter().enumerate() {
        let shown_summary = fit_ui_text(&mut ctx.text, summary, sw - 24.0 - duration, chrome - 1.0);
        if !shown_summary.is_empty() {
            ctx.text.queue_ui_sized(
                bar_x,
                sum_text_y + i as f32 * (chrome + 2.0),
                &shown_summary,
                theme::TEXT_1(),
                chrome - 1.0,
                clip,
            );
        }
    }

    // Section label.
    let label_y = sum_text_y + if compact_testing_summary(sw) { 34.0 } else { 18.0 };
    ctx.text.queue_ui_sized(sx + 14.0, label_y, "RESULTS", theme::DIM(), chrome - 2.0, clip);

    // Results tree.
    let top = rows_top(sw);
    let count = ctx.tests_panel.row_count();
    let first = ctx.tests_panel.first();
    if count == 0 {
        let msg = if visual_running {
            "Running\u{2026}"
        } else if running && final_summary {
            "Finalizing test process\u{2026}"
        } else {
            "Run the package's tests to see results."
        };
        let lines = wrap_detail_lines(&mut ctx.text, msg, sw - 28.0, chrome, 2);
        for (i, line) in lines.iter().enumerate() {
            ctx.text.queue_ui_sized(
                sx + 14.0,
                top + 2.0 + i as f32 * (chrome + 3.0),
                line,
                theme::TEXT_3(),
                chrome,
                clip,
            );
        }
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
        // Suite badge on the right (dim).
        let mut suite_x = sx + sw - 14.0;
        let suite_budget = testing_suite_budget(sw);
        if !suite.is_empty() && suite_budget > 0.0 {
            let sb = fit_ui_text(&mut ctx.text, &suite, suite_budget, chrome - 2.0);
            if !sb.is_empty() {
                let (sbw, _) = ctx.text.measure_ui_sized(&sb, chrome - 2.0);
                let candidate_x = sx + sw - sbw - 14.0;
                if candidate_x - (sx + 32.0) >= 92.0 {
                    suite_x = candidate_x;
                    ctx.text.queue_ui_sized(suite_x, ty, &sb, theme::DIM(), chrome - 2.0, clip);
                }
            }
        }
        let name_x = sx + 32.0;
        let nm = fit_ui_text(&mut ctx.text, &name, suite_x - name_x - 8.0, chrome);
        if !nm.is_empty() {
            ctx.text.queue_ui_sized(name_x, ty, &nm, name_col, chrome, clip);
        }
        y += row_h;
        // Failure message on measured detail rows beneath the failed test.
        if !message.is_empty() {
            let lines = wrap_detail_lines(&mut ctx.text, &message, sw - 54.0, chrome - 1.5, 2);
            let detail_rows = lines.len().max(1);
            ctx.dl_rect(
                sx + 32.0,
                y + 2.0,
                2.0,
                row_h * detail_rows as f32 - 4.0,
                theme::error_wash(0.7),
            );
            for (line_idx, line) in lines.iter().enumerate() {
                let ly = y + line_idx as f32 * row_h;
                let dy = ly + (row_h - (chrome - 1.0)) * 0.5 - 1.0;
                if !line.is_empty() {
                    ctx.text.queue_ui_sized(sx + 40.0, dy, line, theme::ERROR(), chrome - 1.5, clip);
                }
            }
            y += row_h * detail_rows as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testing_header_title_reserves_state_pill_gap() {
        assert_eq!(testing_header_title_budget(220.0, 62.0), 104.0);
        assert_eq!(testing_header_title_budget(96.0, 62.0), 0.0);
    }

    #[test]
    fn testing_suite_budget_hides_when_sidebar_is_too_narrow() {
        assert_eq!(testing_suite_budget(184.0), 0.0);
        assert_eq!(testing_suite_budget(220.0), 66.0);
        assert_eq!(testing_suite_budget(320.0), 76.0);
    }

    #[test]
    fn compact_testing_run_button_keeps_action_verb() {
        assert_eq!(testing_run_label(false, false), "Run Tests");
        assert_eq!(testing_run_label(true, false), "Re-run");
        assert_eq!(testing_run_label(false, true), "Run");
        assert_eq!(testing_run_label(true, true), "Run");
    }

    #[test]
    fn compact_testing_stop_button_keeps_action_verb() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(560, 520) else {
            return;
        };
        crate::layout::reset_sidebar_preset();
        crate::layout::set_window_width(560);
        let tb = toolbar_geom();
        assert!(tb.compact, "560px gallery sidebar should use compact toolbar");

        let label_x = tb.stop_x + 28.0;
        let label_size = testing_stop_label_size(true);
        let (label_w, _) = ctx.text.measure_ui_sized("Stop", label_size);
        assert!(
            label_x + label_w <= tb.stop_x + tb.btn_w - 8.0,
            "compact Stop label should fit inside the button"
        );

        crate::layout::reset_sidebar_preset();
        crate::layout::set_window_width(900);
    }

    #[test]
    fn compact_testing_summary_keeps_readable_words() {
        assert_eq!(
            testing_summary_lines(16, 0, 16, false, 184.0),
            vec!["16 passed \u{00b7} 0 failed".to_string(), "16 total".to_string()]
        );
        assert_eq!(
            testing_summary_lines(16, 0, 16, false, 260.0),
            vec!["16 passed \u{00b7} 0 failed \u{00b7} 16 total".to_string()]
        );
        assert_eq!(rows_top(184.0), rows_top(260.0) + 16.0);
    }

    #[test]
    fn testing_state_distinguishes_final_summary_from_active_running() {
        assert_eq!(testing_state_label(true, false, 0, 0), "running\u{2026}");
        assert_eq!(testing_state_label(true, true, 16, 0), "finalizing\u{2026}");
        assert_eq!(testing_state_label(false, false, 0, 0), "idle");
        assert_eq!(testing_state_label(false, true, 16, 0), "passed");
        assert_eq!(testing_state_label(false, true, 16, 1), "failed");
    }

    #[test]
    fn pathless_tabs_run_tests_against_workspace_file() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 600) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("mighty-ide-test-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("main.mty");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        ctx.workspace = crate::workspace::Workspace::new(root.clone());

        assert_eq!(workspace_test_target(&ctx), Some(file));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_target_prefers_root_manifest() {
        let root = std::env::temp_dir().join(format!(
            "mui-workspace-test-root-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("mighty.toml"), b"[package]\nname=\"root\"\n").unwrap();
        std::fs::write(root.join("tests").join("a.test.mty"), b"fn test_a() {}\n").unwrap();

        assert_eq!(
            workspace_test_target_for_root(&root).unwrap(),
            root.join("mighty.toml")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_target_rejects_file_backed_root() {
        let root = std::env::temp_dir().join(format!(
            "mui-workspace-test-file-root-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&root);
        std::fs::write(&root, b"not a workspace").unwrap();

        assert_eq!(workspace_test_target_for_root(&root), None);
        let _ = std::fs::remove_file(&root);
    }

    #[test]
    fn workspace_target_finds_tests_folder_when_manifest_is_absent() {
        let root = std::env::temp_dir().join(format!(
            "mui-workspace-test-tests-folder-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("target").join("tests")).unwrap();
        std::fs::write(
            root.join("target").join("tests").join("ignore.test.mty"),
            b"fn test_ignore() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests").join("suite.test.mty"),
            b"fn test_suite() {}\n",
        )
        .unwrap();

        assert_eq!(
            workspace_test_target_for_root(&root).unwrap(),
            root.join("tests").join("suite.test.mty")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_target_finds_nested_manifest_before_loose_files() {
        let root = std::env::temp_dir().join(format!(
            "mui-workspace-test-nested-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("examples").join("demo")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.mty"), b"fn main() {}\n").unwrap();
        std::fs::write(
            root.join("examples").join("demo").join("mighty.toml"),
            b"[package]\nname=\"demo\"\n",
        )
        .unwrap();

        assert_eq!(
            workspace_test_target_for_root(&root).unwrap(),
            root.join("examples").join("demo").join("mighty.toml")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_test_detail_wraps_to_two_measured_rows() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 600) else {
            return;
        };
        let msg = "trap MT5001: assertion failed: tokens.len() > 0";
        let lines = wrap_detail_lines(&mut ctx.text, msg, 180.0, theme::CHROME_FONT_SIZE - 1.5, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("MT5001"));
        assert!(lines[1].ends_with('\u{2026}') || lines[1].contains("tokens"));
    }

    #[test]
    fn empty_testing_help_wraps_in_narrow_sidebar() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 600) else {
            return;
        };
        let msg = "Run the package's tests to see results.";
        let lines = wrap_detail_lines(&mut ctx.text, msg, 132.0, theme::CHROME_FONT_SIZE, 2);
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].ends_with('\u{2026}'));
        assert!(lines[0].contains("package"));
        assert!(lines[1].contains("results"));
    }

    #[test]
    fn short_test_detail_uses_one_row() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 600) else {
            return;
        };
        let rows = detail_visual_rows(
            &mut ctx.text,
            "trap MT5001: boom",
            260.0,
            theme::CHROME_FONT_SIZE - 1.5,
        );
        assert_eq!(rows, 1);
    }
}
