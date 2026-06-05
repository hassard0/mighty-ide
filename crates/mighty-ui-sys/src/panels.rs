//! Activity-rail panel ABI: rail-click panel switching + the Source Control
//! (git) and Search (project-wide find/replace) panels.
//!
//! The shim owns all panel state + data + git/search work (modules
//! [`crate::scm`] / [`crate::search`]); Mighty forwards rail clicks to
//! [`mui_panel_set`], routes keys/clicks to the active panel's input/open
//! actions, and draws the active panel each frame. All entry points are the
//! scalar `mui_*` shape required by v0.36 extern-c (L17).

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;
use crate::MuiContext;

/// Cast an opaque `i64` handle back to a context reference (mirrors `abi::ctx`).
#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

// ===========================================================================
// Activity-rail panel switching (Explorer / Search / Source Control / Outline /
// Debug / Test / Mighty Agents)
// ===========================================================================

/// The active sidebar panel: Explorer, Search, Source Control, Outline, Debug,
/// Test, or Mighty Agents.
#[no_mangle]
pub extern "C" fn mui_panel_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(crate::PANEL_EXPLORER, |c| c.active_panel)
}

/// Set the active sidebar panel (clamped to a known panel; unknown ids ignored).
/// Switching to a panel also ensures the sidebar is shown. Returns the resulting
/// active panel.
#[no_mangle]
pub extern "C" fn mui_panel_set(handle: i64, panel: i32) -> i32 {
    crate::abi::trace(&format!("panel_set req={panel}"));
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return crate::PANEL_EXPLORER;
    };
    if (crate::PANEL_EXPLORER..=crate::PANEL_SCM).contains(&panel)
        || panel == crate::PANEL_OUTLINE
        || panel == crate::PANEL_DEBUG
        || panel == crate::PANEL_TEST
        || panel == crate::PANEL_AGENTS_MTY
    {
        let changed = ctx.active_panel != panel;
        ctx.active_panel = panel;
        ctx.sidebar_visible = true;
        if changed && ctx.toasts.clear_low_priority() {
            crate::abi::trace("toast_clear_low_priority panel_switch");
        }
        if panel == crate::PANEL_DEBUG {
            ctx.dbg.set_open(true);
        } else if panel == crate::PANEL_TEST {
            ctx.tests_panel.open();
        }
    }
    ctx.active_panel
}

/// Close the Explorer panel by hiding the sidebar without clearing tree state.
/// Returns `1` when it closed Explorer, or `0` when Explorer was already closed.
#[no_mangle]
pub extern "C" fn mui_explorer_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.sidebar_visible && ctx.active_panel == crate::PANEL_EXPLORER {
        ctx.sidebar_visible = false;
        ctx.push_toast(crate::toast::Kind::Info, "Explorer panel closed");
        crate::abi::trace("explorer_close");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Explorer panel is already closed");
    crate::abi::trace("explorer_close noop");
    0
}

/// Map the last click's pixel position to a rail icon slot, or `-1` if the click
/// was not on a rail icon. The rail geometry mirrors `mui_rail_draw`: a column of
/// 38px cells starting at y=52 with a 4px gap. Slots 0/1/2/5/6/7/8 are sidebar
/// panels; slot 3 opens the Run panel; slot 4 toggles the AI copilot.
#[no_mangle]
pub extern "C" fn mui_rail_panel_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    let mut out = -1;
    if (0.0..=layout::RAIL_W).contains(&x) {
        let cell = 38.0_f32;
        let gap = 4.0_f32;
        let icon_top = 52.0_f32;
        if y >= icon_top {
            let slot = ((y - icon_top) / (cell + gap)).floor() as i32;
            if (0..=8).contains(&slot) {
                let cy = icon_top + slot as f32 * (cell + gap);
                if y <= cy + cell {
                    out = slot;
                }
            }
        }
    }
    crate::abi::trace(&format!("rail_panel_at_click x={x:.1} y={y:.1} -> {out}"));
    out
}

/// The workspace directory the SCM/search panels operate over: the EXPLICIT
/// workspace root (set via Open Folder), falling back to the file-tree root when
/// no explicit workspace is set. Cloned so callers don't hold a borrow.
fn workspace_dir(ctx: &MuiContext) -> std::path::PathBuf {
    crate::wsabi::effective_root(ctx)
}

fn scm_trace_root(ctx: &MuiContext) -> String {
    ctx.scm
        .root
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<none>".to_string())
}

fn scm_root_missing_message(ctx: &MuiContext, path: &str) -> String {
    let target = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(path);
    let boundary = workspace_dir(ctx);
    let boundary = boundary
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let shown = boundary.display().to_string();
            if shown.is_empty() {
                "workspace".to_string()
            } else {
                shown
            }
        });
    if target.is_empty() {
        format!("Source control root missing in {boundary}")
    } else {
        format!("Source control root missing for {target} in {boundary}")
    }
}

// ===========================================================================
// Source Control panel — git status / stage / commit (shim shells to git)
// ===========================================================================

/// Re-discover the repo + re-run `git status`, refreshing the changes list.
/// Returns the number of changed entries (0 if not a git repo). The IDE calls
/// this on panel open + after each save.
#[no_mangle]
pub extern "C" fn mui_scm_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    let n = ctx.scm.refresh(&dir);
    println!(
        "scm: branch={} ahead={} behind={} changes={}",
        ctx.scm.status.branch, ctx.scm.status.ahead, ctx.scm.status.behind, n
    );
    crate::abi::trace(&format!(
        "scm_refresh branch=\"{}\" changes={} root={}",
        ctx.scm.status.branch,
        n,
        scm_trace_root(ctx)
    ));
    n
}

/// Close the Source Control panel without clearing status, branch, or message state.
/// Returns `1` when it closed Source Control, or `0` when already closed.
#[no_mangle]
pub extern "C" fn mui_scm_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.active_panel == crate::PANEL_SCM {
        ctx.active_panel = crate::PANEL_EXPLORER;
        ctx.push_toast(crate::toast::Kind::Info, "Source Control panel closed");
        crate::abi::trace("scm_close");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Source Control panel is already closed");
    crate::abi::trace("scm_close noop");
    0
}

/// Number of changed entries in the last status.
#[no_mangle]
pub extern "C" fn mui_scm_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.scm.count())
}

/// `1` if entry `i` is staged, `0` if unstaged, `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_scm_row_staged(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.scm.get(i as usize).map_or(-1, |e| if e.staged { 1 } else { 0 })
    })
}

/// Status letter of entry `i` as a codepoint (M/A/D/R/U/C), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_scm_row_status(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.scm.get(i as usize).map_or(-1, |e| e.status as i32)
    })
}

/// Open the file of changed entry `i` as a tab (resolved under the repo root).
/// Returns the resulting tab index, or `-1` out of range / no repo / deleted.
#[no_mangle]
pub extern "C" fn mui_scm_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No source control row selected");
        return -1;
    }
    let (path, root) = {
        let Some(entry) = ctx.scm.get(i as usize) else {
            ctx.push_toast(crate::toast::Kind::Info, "Source control row no longer listed");
            return -1;
        };
        let Some(root) = ctx.scm.root.clone() else {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                scm_root_missing_message(ctx, entry.path.as_str()),
            );
            return -1;
        };
        (entry.path.clone(), root)
    };
    let full = root.join(&path);
    let name = crate::abi::file_target_name(std::path::Path::new(&path));
    match scm_target_kind(&full) {
        ScmTargetKind::File => {}
        ScmTargetKind::Missing => {
            return refresh_bad_scm_target(
                ctx,
                &root,
                format!("Source control target missing: {name}"),
            );
        }
        ScmTargetKind::NotFile => {
            return refresh_bad_scm_target(
                ctx,
                &root,
                format!("Source control target is not a file: {name}"),
            );
        }
    }
    let idx = ctx.tabs.open_path(full.clone());
    crate::abi::sync_active_path(ctx);
    crate::abi::record_opened_file(ctx, &full);
    idx as i32
}

enum ScmTargetKind {
    File,
    Missing,
    NotFile,
}

fn scm_target_kind(path: &std::path::Path) -> ScmTargetKind {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => ScmTargetKind::File,
        Ok(_) => ScmTargetKind::NotFile,
        Err(_) => ScmTargetKind::Missing,
    }
}

fn refresh_bad_scm_target(ctx: &mut MuiContext, root: &std::path::Path, message: String) -> i32 {
    ctx.push_toast(crate::toast::Kind::Warn, message);
    let _ = ctx.scm.refresh(root);
    crate::abi::refresh_workspace_file_views(ctx);
    -1
}

/// Stage/unstage the row `i` (toggles based on its current state), then refresh.
/// Returns `1` on success, `0` otherwise.
#[no_mangle]
pub extern "C" fn mui_scm_toggle_stage(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No source control row selected");
        crate::abi::trace("scm_toggle_stage ok=0 idx=-1");
        return 0;
    }
    let (path_before, staged_before) = match ctx.scm.get(i as usize) {
        Some(entry) => (entry.path.clone(), entry.staged),
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "Source control row no longer listed");
            crate::abi::trace(&format!("scm_toggle_stage ok=0 idx={i} missing-row"));
            return 0;
        }
    };
    if ctx.scm.root.is_none() {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            scm_root_missing_message(ctx, path_before.as_str()),
        );
        crate::abi::trace(&format!("scm_toggle_stage ok=0 idx={i} missing-root"));
        return 0;
    }
    let dir = workspace_dir(ctx);
    let ok = ctx.scm.toggle_stage(i as usize, &dir);
    let staged = ctx.scm.status.staged_count();
    let unstaged = ctx.scm.status.unstaged_count();
    crate::abi::trace(&format!(
        "scm_toggle_stage ok={} idx={i} staged={staged} unstaged={unstaged} root={}",
        i32::from(ok),
        scm_trace_root(ctx)
    ));
    if ok {
        1
    } else {
        let action = if staged_before { "unstage" } else { "stage" };
        let name = std::path::Path::new(&path_before)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(path_before.as_str());
        let _ = ctx.scm.refresh(&dir);
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Source control {action} failed: {name}"),
        );
        0
    }
}

/// Stage every changed path, including untracked files. Returns `1` on success.
#[no_mangle]
pub extern "C" fn mui_scm_stage_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    if ctx.scm.root.is_none() {
        ctx.scm.refresh(&dir);
    }
    if ctx.scm.root.is_none() {
        ctx.push_toast(crate::toast::Kind::Warn, "Not a git repository");
        crate::abi::trace("scm_stage_all ok=0 root=<none>");
        return 0;
    }
    let ok = ctx.scm.stage_all(&dir);
    let staged = ctx.scm.status.staged_count();
    let unstaged = ctx.scm.status.unstaged_count();
    crate::abi::trace(&format!(
        "scm_stage_all ok={} staged={staged} unstaged={unstaged} root={}",
        i32::from(ok),
        scm_trace_root(ctx)
    ));
    if ok {
        ctx.push_toast(crate::toast::Kind::Success, "Staged all changes");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Warn, "Nothing to stage");
        0
    }
}

/// Unstage every staged path. Returns `1` on success.
#[no_mangle]
pub extern "C" fn mui_scm_unstage_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    if ctx.scm.root.is_none() {
        ctx.scm.refresh(&dir);
    }
    if ctx.scm.root.is_none() {
        ctx.push_toast(crate::toast::Kind::Warn, "Not a git repository");
        crate::abi::trace("scm_unstage_all ok=0 root=<none>");
        return 0;
    }
    let ok = ctx.scm.unstage_all(&dir);
    let staged = ctx.scm.status.staged_count();
    let unstaged = ctx.scm.status.unstaged_count();
    crate::abi::trace(&format!(
        "scm_unstage_all ok={} staged={staged} unstaged={unstaged} root={}",
        i32::from(ok),
        scm_trace_root(ctx)
    ));
    if ok {
        ctx.push_toast(crate::toast::Kind::Success, "Unstaged all changes");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Warn, "Nothing to unstage");
        0
    }
}

/// Current branch name length (chars), for sizing. `0` if none.
#[no_mangle]
pub extern "C" fn mui_scm_branch_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.scm.status.branch.chars().count() as i32)
}

/// Ahead count (commits ahead of upstream).
#[no_mangle]
pub extern "C" fn mui_scm_ahead(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.scm.status.ahead)
}

/// Behind count (commits behind upstream).
#[no_mangle]
pub extern "C" fn mui_scm_behind(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.scm.status.behind)
}

// ---- commit-message input (shim-owned buffer) ----

/// Append one Unicode scalar to the commit message.
#[no_mangle]
pub extern "C" fn mui_scm_msg_push(handle: i64, codepoint: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if codepoint >= 0 {
            if let Some(ch) = char::from_u32(codepoint as u32) {
                ctx.scm.message.push(ch);
            }
        }
    }
}

/// Delete the last commit-message char.
#[no_mangle]
pub extern "C" fn mui_scm_msg_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.scm.message.pop();
    }
}

/// Number of chars in the commit message.
#[no_mangle]
pub extern "C" fn mui_scm_msg_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.scm.message.len() as i32)
}

/// Clear the commit-message draft without refreshing or changing git status.
/// Returns `1` when a draft was cleared, or `0` when it was already empty.
#[no_mangle]
pub extern "C" fn mui_scm_clear_message(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.scm.message.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Source Control message already empty");
        return 0;
    }
    ctx.scm.message.clear();
    ctx.push_toast(crate::toast::Kind::Info, "Source Control message cleared");
    1
}

/// Commit the staged changes with the current message, then clear it + refresh.
/// Returns `1` on success, `0` on failure (nothing staged / empty msg / error).
#[no_mangle]
pub extern "C" fn mui_scm_commit(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    ctx.scm.refresh(&dir);
    if ctx.scm.root.is_none() {
        ctx.push_toast(crate::toast::Kind::Warn, "Not a git repository");
        crate::abi::trace("scm_commit ok=0 root=<none>");
        return 0;
    }
    if ctx.scm.status.staged_count() == 0 {
        ctx.push_toast(crate::toast::Kind::Warn, "No staged changes to commit");
        crate::abi::trace(&format!(
            "scm_commit ok=0 staged=0 root={}",
            scm_trace_root(ctx)
        ));
        return 0;
    }
    if ctx.scm.message_string().trim().is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Enter a commit message");
        crate::abi::trace(&format!(
            "scm_commit ok=0 empty-message staged={} root={}",
            ctx.scm.status.staged_count(),
            scm_trace_root(ctx)
        ));
        return 0;
    }
    let ok = ctx.scm.commit_message(&dir);
    let staged = ctx.scm.status.staged_count();
    let unstaged = ctx.scm.status.unstaged_count();
    crate::abi::trace(&format!(
        "scm_commit ok={} staged={staged} unstaged={unstaged} root={}",
        i32::from(ok),
        scm_trace_root(ctx)
    ));
    if ok {
        println!("scm: committed");
        ctx.push_toast(crate::toast::Kind::Success, "Committed changes");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Warn, "Nothing to commit");
        0
    }
}

/// Map the last click's pixel y to a Source-Control changes-list row index, or
/// `-1` if not on a row. Mirrors the row geometry in `mui_scm_draw`.
#[no_mangle]
pub extern "C" fn mui_scm_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let sx0 = layout::RAIL_W;
    let sx1 = layout::sidebar_right();
    if !ctx.sidebar_visible || ctx.last_event.x < sx0 || ctx.last_event.x > sx1 {
        return -1;
    }
    let top = scm_rows_top();
    let y = ctx.last_event.y;
    if y < top {
        return -1;
    }
    let i = ((y - top) / layout::LINE_H()).floor() as i32;
    if i >= 0 && i < ctx.scm.count() {
        i
    } else {
        -1
    }
}

/// `1` if the last click landed on the stage/unstage action button (right edge)
/// of a Source-Control row, else `0`. Lets Mighty distinguish "open the file"
/// (row body) from "stage/unstage" (action button).
#[no_mangle]
pub extern "C" fn mui_scm_click_is_stage(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let action_x0 = layout::sidebar_right() - 30.0;
    if ctx.last_event.x >= action_x0 {
        1
    } else {
        0
    }
}

/// Map the last click to a Source-Control HEADER action button:
/// `1` = commit, `2` = pull, `3` = push, `4` = refresh, `5` = stage all,
/// `6` = unstage all, `0` = none. Mirrors the header icon geometry in
/// `mui_scm_draw`.
#[no_mangle]
pub extern "C" fn mui_scm_header_action_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_SCM {
        return 0;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    if !(0.0..=40.0).contains(&y) {
        return 0;
    }
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    // Icon centers at sw-94 (commit), sw-72 (pull), sw-50 (push), sw-28 (fetch),
    // each ~15px wide — use 11px half-windows around each.
    let action = scm_header_action_centers(sx, sw)
        .into_iter()
        .find_map(|(cx, action)| if (x - cx).abs() <= 9.0 { Some(action) } else { None })
        .unwrap_or(0);
    if action > 0 {
        let label = match action {
            1 => "commit",
            2 => "pull",
            3 => "push",
            4 => "refresh",
            5 => "stage_all",
            _ => "unstage_all",
        };
        crate::abi::trace(&format!("scm_header action={label}"));
    }
    action
}

/// Y pixel (top) of the first Source-Control changes row.
fn scm_rows_top() -> f32 {
    40.0 + 54.0 + layout::LINE_H()
}

/// Display color for a git status letter (Vivid Modern palette).
fn git_status_color(status: char) -> MuiColor {
    match status {
        'A' => theme::GREEN(),
        'M' => theme::WARNING(),
        'D' => theme::ERROR(),
        'U' => theme::INFO(),
        'R' => theme::ACCENT_BRIGHT(),
        'C' => theme::ERROR(),
        _ => theme::DIM(),
    }
}

pub(crate) fn fit_tail_px(
    text: &mut crate::text::Text,
    s: &str,
    max_px: f32,
    size: f32,
) -> String {
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

pub(crate) fn fit_scm_header(
    text: &mut crate::text::Text,
    title: &str,
    sx: f32,
    sw: f32,
    size: f32,
) -> String {
    let label_x = sx + 14.0;
    let first_action_x = scm_header_action_centers(sx, sw)[0].0 - 7.0;
    let max_px = (first_action_x - 8.0 - label_x).max(0.0);
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    fit_head_px(text, &tracked, max_px, size)
}

pub(crate) fn scm_header_action_centers(sx: f32, sw: f32) -> [(f32, i32); 6] {
    [
        (sx + sw - 124.0 + 7.0, 5),
        (sx + sw - 105.0 + 7.0, 6),
        (sx + sw - 86.0 + 7.0, 1),
        (sx + sw - 67.0 + 7.0, 2),
        (sx + sw - 48.0 + 7.0, 3),
        (sx + sw - 29.0 + 7.0, 4),
    ]
}

pub(crate) fn scm_message_clear_rect(sx: f32, sw: f32) -> (f32, f32, f32, f32) {
    let head_h = 40.0;
    let box_y = head_h + 8.0;
    let size = 24.0;
    (sx + sw - 38.0, box_y + 7.0, size, size)
}

pub(crate) fn scm_header_title_for_budget(
    text: &mut crate::text::Text,
    sx: f32,
    sw: f32,
    size: f32,
) -> &'static str {
    let label_x = sx + 14.0;
    let first_action_x = scm_header_action_centers(sx, sw)[0].0 - 7.0;
    let max_px = (first_action_x - 8.0 - label_x).max(0.0);
    let full = "SOURCE CONTROL";
    let tracked: String = full.chars().flat_map(|c| [c, '\u{2009}']).collect();
    if text.measure_ui_sized(&tracked, size).0 <= max_px {
        full
    } else {
        "SCM"
    }
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

pub(crate) fn scm_section_branch_budget(
    text: &mut crate::text::Text,
    sx: f32,
    sw: f32,
    count: usize,
    ahead: i32,
    behind: i32,
    size: f32,
) -> f32 {
    let changes_x = sx + 14.0;
    let changes_w = text.measure_ui_sized("CHANGES", size).0;
    let cnt_str = count.to_string();
    let count_x = changes_x + changes_w + 8.0;
    let count_w = text.measure_ui_sized(&cnt_str, size).0;
    let branch_left = count_x + count_w + 12.0;
    let branch_right = sx + sw - 12.0;
    let ab_w = if ahead > 0 || behind > 0 {
        text.measure_ui_sized(&format!("\u{2191}{ahead} \u{2193}{behind}"), size - 1.0).0 + 8.0
    } else {
        0.0
    };
    branch_right - branch_left - 16.0 - ab_w
}

/// `1` if the last click landed on the Source Control commit-message clear
/// affordance, else `0`.
#[no_mangle]
pub extern "C" fn mui_scm_message_clear_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_SCM {
        return 0;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let (rx, ry, rw, rh) = scm_message_clear_rect(layout::RAIL_W, layout::sidebar_w());
    if x >= rx && x <= rx + rw && y >= ry && y <= ry + rh {
        crate::abi::trace("scm_message_clear hit");
        1
    } else {
        0
    }
}

/// Draw the Source Control panel (header + branch/ahead-behind, commit-message
/// box + Commit affordance, changes list with colored status badges + file
/// icons). No-op when the sidebar is hidden or this panel isn't active.
#[no_mangle]
pub extern "C" fn mui_scm_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_SCM {
        return;
    }
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    use crate::icons;

    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // header band
    let head_h = 40.0;
    ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
    ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
    let title = scm_header_title_for_budget(&mut ctx.text, sx, sw, chrome - 2.0);
    let tracked = fit_scm_header(&mut ctx.text, title, sx, sw, chrome - 2.0);
    ctx.text.queue_ui_sized(
        sx + 14.0,
        (head_h - (chrome - 2.0)) * 0.5 - 1.0,
        &tracked,
        theme::DIM(),
        chrome - 2.0,
        clip,
    );
    // Header action row: commit (check) · pull (down) · push (up) · fetch
    // (refresh). Hit-tested by `mui_scm_header_action_at_click`.
    let act_y = (head_h - 15.0) * 0.5;
    for (cx, action) in scm_header_action_centers(sx, sw) {
        let (icon, color, stroke) = match action {
            1 => (icons::CHECK, theme::GREEN(), 1.8),
            2 => (icons::ARROW_DOWN, theme::TEXT_3(), 1.7),
            3 => (icons::ARROW_UP, theme::TEXT_3(), 1.7),
            4 => (icons::REFRESH, theme::TEXT_3(), 1.5),
            5 => (icons::STAGE_PLUS, theme::GREEN(), 1.7),
            _ => (icons::UNSTAGE_MINUS, theme::TEXT_3(), 1.7),
        };
        ctx.dl_icon(cx - 7.0, act_y, 15.0, 15.0, icon, color, stroke, false);
    }

    // commit-message box
    let box_y = head_h + 8.0;
    let box_h = 38.0;
    ctx.dl_round(sx + 10.0, box_y, sw - 20.0, box_h, 7.0, theme::BG_1());
    ctx.dl_stroke(sx + 10.0, box_y, sw - 20.0, box_h, 7.0, theme::BORDER_STRONG(), 1.0);
    let msg = ctx.scm.message_string();
    let (msg_text, msg_col) = if msg.is_empty() {
        ("Message (Enter to commit)".to_string(), theme::TEXT_3())
    } else {
        (msg, theme::TEXT())
    };
    let (clear_x, clear_y, clear_w, clear_h) = scm_message_clear_rect(sx, sw);
    let clear_col = if ctx.scm.message.is_empty() { theme::TEXT_4() } else { theme::TEXT_3() };
    let shown = fit_tail_px(&mut ctx.text, &msg_text, (clear_x - 8.0 - (sx + 18.0)).max(0.0), chrome);
    ctx.text.queue_ui_sized(sx + 18.0, box_y + (box_h - chrome) * 0.5 - 1.0, &shown, msg_col, chrome, clip);
    ctx.dl_stroke(clear_x, clear_y, clear_w, clear_h, 5.0, theme::BORDER_SOFT(), 1.0);
    ctx.dl_icon(clear_x + 4.5, clear_y + 4.5, 15.0, 15.0, icons::TRASH, clear_col, 1.5, false);

    // section header + branch pill
    let branch = ctx.scm.status.branch.clone();
    let ahead = ctx.scm.status.ahead;
    let behind = ctx.scm.status.behind;
    let count = ctx.scm.count();
    let sec_y = box_y + box_h + 6.0;
    let changes_x = sx + 14.0;
    ctx.text.queue_ui_sized(changes_x, sec_y + 3.0, "CHANGES", theme::DIM(), chrome - 2.0, clip);
    let cnt_str = count.to_string();
    let changes_w = ctx.text.measure_ui_sized("CHANGES", chrome - 2.0).0;
    let count_x = changes_x + changes_w + 8.0;
    ctx.text.queue_ui_sized(count_x, sec_y + 3.0, &cnt_str, theme::TEXT_3(), chrome - 2.0, clip);
    if !branch.is_empty() {
        let count_w = ctx.text.measure_ui_sized(&cnt_str, chrome - 2.0).0;
        let branch_left = count_x + count_w + 12.0;
        let branch_right = sx + sw - 12.0;
        let ab = if ahead > 0 || behind > 0 {
            Some(format!("\u{2191}{ahead} \u{2193}{behind}"))
        } else {
            None
        };
        let ab_w = ab
            .as_ref()
            .map(|s| ctx.text.measure_ui_sized(s, chrome - 3.0).0 + 8.0)
            .unwrap_or(0.0);
        let branch_budget = scm_section_branch_budget(
            &mut ctx.text,
            sx,
            sw,
            count.max(0) as usize,
            ahead,
            behind,
            chrome - 2.0,
        );
        if branch_budget >= 24.0 {
            ctx.dl_icon(branch_left, sec_y + 1.0, 12.0, 12.0, icons::BRANCH, theme::ACCENT_BRIGHT(), 1.5, false);
            let bp = fit_tail_px(&mut ctx.text, &branch, branch_budget, chrome - 2.0);
            let bp_x = branch_left + 16.0;
            ctx.text.queue_ui_sized(bp_x, sec_y + 3.0, &bp, theme::TEXT_1(), chrome - 2.0, clip);
            if let Some(ab) = ab {
                ctx.text.queue_ui_sized(branch_right - ab_w + 8.0, sec_y + 3.0, &ab, theme::TEXT_3(), chrome - 3.0, clip);
            }
        }
    }

    if ctx.scm.root.is_none() {
        let top = scm_rows_top();
        ctx.text.queue_ui_sized(sx + 14.0, top + 4.0, "Source control not scanned", theme::TEXT_1(), chrome, clip);
        let hint = fit_head_px(&mut ctx.text, "Refresh to scan Git status.", sw - 28.0, chrome - 1.0);
        ctx.text.queue_ui_sized(sx + 14.0, top + 25.0, &hint, theme::TEXT_3(), chrome - 1.0, clip);
        return;
    }
    if count == 0 {
        let top = scm_rows_top();
        ctx.text.queue_ui_sized(sx + 14.0, top + 4.0, "Working tree clean", theme::TEXT_1(), chrome, clip);
        let hint = fit_head_px(&mut ctx.text, "Edit a file to start a change.", sw - 28.0, chrome - 1.0);
        ctx.text.queue_ui_sized(sx + 14.0, top + 25.0, &hint, theme::TEXT_3(), chrome - 1.0, clip);
        return;
    }

    let row_h = layout::LINE_H();
    let row_top = scm_rows_top();
    for i in 0..count {
        let (status, staged, name, dir) = {
            let Some(e) = ctx.scm.get(i as usize) else { continue };
            (e.status, e.staged, e.name().to_string(), e.dir().to_string())
        };
        let y = row_top + (i as f32) * row_h;
        if y > h {
            break;
        }
        let icon_y = y + (row_h - 15.0) * 0.5;
        let txt_y = y + (row_h - chrome) * 0.5 - 1.0;

        let scol = git_status_color(status);
        let badge: String = status.to_string();
        ctx.text.queue_ui_sized(sx + 14.0, txt_y, &badge, scol, chrome, clip);

        let (icon, _icol) = crate::abi::file_icon_for(&name, false);
        ctx.dl_icon(sx + 28.0, icon_y, 15.0, 15.0, icon, scol, 1.4, false);

        let name_x = sx + 47.0;
        let action_left = sx + sw - 30.0;
        let dir_reserve = if dir.is_empty() { 8.0 } else { 72.0 };
        let shown_name = fit_tail_px(&mut ctx.text, &name, (action_left - name_x - dir_reserve).max(0.0), chrome);
        if !shown_name.is_empty() {
            ctx.text.queue_ui_sized(name_x, txt_y, &shown_name, theme::TEXT_1(), chrome, clip);
        }
        if !dir.is_empty() {
            let (name_w, _) = ctx.text.measure_ui_sized(&shown_name, chrome);
            let dx = name_x + name_w + 6.0;
            if dx < action_left - 8.0 {
                let shown_dir = fit_tail_px(&mut ctx.text, &dir, (action_left - dx - 8.0).max(0.0), chrome - 1.5);
                ctx.text.queue_ui_sized(dx, txt_y, &shown_dir, theme::TEXT_4(), chrome - 1.5, clip);
            }
        }

        let act_x = sx + sw - 26.0;
        let glyph = if staged { icons::UNSTAGE_MINUS } else { icons::STAGE_PLUS };
        let acol = if staged { theme::TEXT_3() } else { theme::GREEN() };
        ctx.dl_icon(act_x, icon_y, 14.0, 14.0, glyph, acol, 1.7, false);
    }
}

// ===========================================================================
// Git network actions (push / pull / fetch) + branch switcher + blame gutter
// ===========================================================================

/// Run `git push` (never force). Refreshes ahead/behind + toasts the result.
/// Returns `1` on success, `0` on failure.
#[no_mangle]
pub extern "C" fn mui_git_push(handle: i64) -> i32 {
    git_action(handle, crate::scm::GitAction::Push, "Pushed")
}

/// Run `git pull --ff-only`. Refreshes status + toasts the result.
#[no_mangle]
pub extern "C" fn mui_git_pull(handle: i64) -> i32 {
    git_action(handle, crate::scm::GitAction::Pull, "Pulled")
}

/// Run `git fetch`. Refreshes status + toasts the result.
#[no_mangle]
pub extern "C" fn mui_git_fetch(handle: i64) -> i32 {
    git_action(handle, crate::scm::GitAction::Fetch, "Fetched")
}

/// Shared push/pull/fetch worker: run + refresh + toast (success message uses
/// git's own last line; failures surface git's exact error).
fn git_action(handle: i64, action: crate::scm::GitAction, ok_verb: &str) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    let res = ctx.scm.run_action(action, &dir);
    if res.ok {
        ctx.push_toast(crate::toast::Kind::Success, format!("{ok_verb}: {}", res.message));
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Error, format!("Git error: {}", res.message));
        0
    }
}

/// Open the branch switcher overlay (refreshing the branch list first). Returns
/// the number of branches, or `-1` if not a git repo.
#[no_mangle]
pub extern "C" fn mui_git_branches(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let dir = workspace_dir(ctx);
    if ctx.scm.root.is_none() {
        ctx.scm.refresh(&dir);
    }
    if ctx.scm.root.is_none() {
        ctx.push_toast(crate::toast::Kind::Warn, "Not a git repository");
        return -1;
    }
    let n = ctx.scm.refresh_branches();
    let list = ctx.scm.branches.clone();
    ctx.branch_picker.open(&list);
    crate::abi::trace(&format!("branch_open count={n}"));
    n
}

/// `1` if the branch switcher overlay is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_branch_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.branch_picker.is_active()))
}

/// Append a typed char to the branch filter / new-branch-name buffer.
#[no_mangle]
pub extern "C" fn mui_branch_push_char(handle: i64, codepoint: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if codepoint >= 0 {
            if let Some(ch) = char::from_u32(codepoint as u32) {
                ctx.branch_picker.push_char(ch);
            }
        }
    }
}

/// Backspace the branch filter / new-branch-name buffer.
#[no_mangle]
pub extern "C" fn mui_branch_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.branch_picker.backspace();
    }
}

/// Length (chars) of the branch filter / new-branch-name buffer.
#[no_mangle]
pub extern "C" fn mui_branch_query_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.branch_picker.query_len() as i32)
}

/// Number of rows in the branch picker (filtered branches + the create row).
#[no_mangle]
pub extern "C" fn mui_branch_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.branch_picker.count() as i32)
}

/// Move the branch-picker selection by `delta` (wrapping).
#[no_mangle]
pub extern "C" fn mui_branch_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.branch_picker.move_sel(delta);
    }
}

pub(crate) fn branch_picker_visible_rows(height: u32, wanted_rows: usize) -> usize {
    let h = height as f32;
    let row_h = 34.0_f32;
    let fixed_h = 50.0_f32 + 16.0;
    let available = (h - 32.0 - fixed_h).max(row_h);
    let capacity = (available / row_h).floor().max(1.0) as usize;
    wanted_rows.min(capacity).max(usize::from(wanted_rows > 0))
}

pub(crate) fn branch_picker_geometry(width: u32, height: u32, rows: usize) -> (f32, f32, f32, f32, f32, f32) {
    let w = width as f32;
    let h = height as f32;
    let row_h = 34.0_f32;
    let head_h = 50.0_f32;
    let box_w = (w - 32.0).max(1.0).min(460.0);
    let box_h = head_h + rows as f32 * row_h + 16.0;
    let box_x = ((w - box_w) * 0.5).max(0.0);
    let box_y = 100.0_f32.min((h - box_h - 16.0).max(8.0));
    let list_top = box_y + head_h + 6.0;
    (box_x, box_y, box_w, box_h, list_top, row_h)
}

fn branch_picker_close_rect(width: u32, height: u32, rows: usize) -> (f32, f32, f32, f32) {
    let (box_x, box_y, box_w, _box_h, _list_top, _row_h) =
        branch_picker_geometry(width, height, rows);
    (box_x + box_w - 38.0, box_y + 13.0, 24.0, 24.0)
}

fn branch_picker_query_budget(query_x: f32, close_x: f32, is_placeholder: bool) -> f32 {
    let trailing_gap = if is_placeholder { 24.0 } else { 14.0 };
    (close_x - trailing_gap - query_x).max(0.0)
}

fn branch_picker_entry_name_right(box_x: f32, box_w: f32, has_badge: bool) -> f32 {
    if has_badge {
        box_x + box_w - 72.0
    } else {
        box_x + box_w - 20.0
    }
}

/// Select the branch-picker row under the last click. Returns the selected row
/// index, `-2` for the close button, or `-1` if the click missed the picker rows.
#[no_mangle]
pub extern "C" fn mui_branch_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.branch_picker.is_active() {
        return -1;
    }
    let rows = branch_picker_visible_rows(
        ctx.gpu.height,
        if ctx.branch_picker.is_creating() {
            1
        } else {
            ctx.branch_picker.count().min(10)
        },
    );
    let (box_x, box_y, box_w, _box_h, list_top, row_h) =
        branch_picker_geometry(ctx.gpu.width, ctx.gpu.height, rows);
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if x < box_x || x > box_x + box_w || y < box_y {
        return -1;
    }
    let (cx, cy, cw, ch) = branch_picker_close_rect(ctx.gpu.width, ctx.gpu.height, rows);
    if (cx..=cx + cw).contains(&x) && (cy..=cy + ch).contains(&y) {
        crate::abi::trace("branch_close");
        return -2;
    }
    if ctx.branch_picker.is_creating() || y < list_top {
        return -1;
    }
    let idx = ((y - list_top) / row_h).floor() as usize;
    if idx >= rows {
        return -1;
    }
    if ctx.branch_picker.select(idx) {
        idx as i32
    } else {
        -1
    }
}

/// `1` if the picker is in "Create branch…" (typing a new name) mode, else `0`.
#[no_mangle]
pub extern "C" fn mui_branch_is_creating(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.branch_picker.is_creating()))
}

/// Accept the current branch-picker selection.
///
///   * On a branch row → checkout that branch (refreshes + toasts), closes.
///   * On the "Create branch…" row (and not yet creating) → switch into create
///     mode (returns `2`; the IDE keeps the overlay open for name entry).
///   * While creating → create + switch to the typed branch, closes.
///
/// Returns `1` on a completed checkout/create, `2` when it entered create mode,
/// `0` on failure (toasts the git result).
#[no_mangle]
pub extern "C" fn mui_branch_accept(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.branch_picker.is_active() {
        ctx.push_toast(crate::toast::Kind::Info, "No branch picker open");
        return 0;
    }
    let dir = workspace_dir(ctx);
    if ctx.branch_picker.is_creating() {
        let name = ctx.branch_picker.query_string();
        let name = name.trim().to_string();
        if name.is_empty() {
            ctx.push_toast(crate::toast::Kind::Warn, "Enter a branch name");
            return 0;
        }
        let res = ctx.scm.create_and_switch(&name, &dir);
        if res.ok {
            ctx.push_toast(crate::toast::Kind::Success, format!("Created branch {name}"));
            ctx.branch_picker.cancel();
            1
        } else {
            refresh_branch_picker_after_failed_action(ctx);
            ctx.push_toast(crate::toast::Kind::Error, format!("Git error: {}", res.message));
            0
        }
    } else if ctx.branch_picker.selection_is_create() {
        ctx.branch_picker.enter_create_mode();
        2
    } else if let Some(name) = ctx.branch_picker.selected_name() {
        let res = ctx.scm.checkout_branch(&name, &dir);
        if res.ok {
            ctx.push_toast(crate::toast::Kind::Success, format!("Switched to {name}"));
            ctx.branch_picker.cancel();
            1
        } else {
            refresh_branch_picker_after_failed_action(ctx);
            ctx.push_toast(crate::toast::Kind::Error, format!("Git error: {}", res.message));
            0
        }
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No branch selected");
        0
    }
}

fn refresh_branch_picker_after_failed_action(ctx: &mut MuiContext) {
    let _ = ctx.scm.refresh_branches();
    let list = ctx.scm.branches.clone();
    ctx.branch_picker.open(&list);
}

/// Close the branch switcher without acting. Returns `1` when it closed an open
/// picker, or `0` when no branch picker was open.
#[no_mangle]
pub extern "C" fn mui_branch_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.branch_picker.is_active() {
        ctx.branch_picker.cancel();
        ctx.push_toast(crate::toast::Kind::Info, "Branch switcher closed");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No branch picker open");
        0
    }
}

/// `1` if the last click landed on the status-bar branch segment (branch icon +
/// name + ahead/behind, in the left cluster). Lets the IDE open the branch
/// switcher by clicking the branch in the status bar.
#[no_mangle]
pub extern "C" fn mui_status_branch_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let h = ctx.gpu.height as f32;
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    if y < h - 30.0 {
        return 0;
    }
    // The branch segment is the leftmost ~150px of the status bar (icon at x=10,
    // name + ahead/behind through ~x 150), before the problems cluster.
    if (0.0..=150.0).contains(&x) {
        1
    } else {
        0
    }
}

/// Single shim-side dispatcher for the new Git palette commands, so the Mighty
/// palette / quick-open dispatch ladders need only ONE new arm each (calling
/// this) instead of one per command — keeping clear of the mty parse-stack
/// ceiling (L37/L38). `cmd_id` is a `palette::CMD_GIT_*` id. Returns `1` if the
/// id was a git command (handled), `0` otherwise (caller falls through).
#[no_mangle]
pub extern "C" fn mui_git_dispatch(handle: i64, cmd_id: i32) -> i32 {
    use crate::palette;
    let id = cmd_id as u32;
    if id < palette::CMD_GIT_FIRST {
        return 0;
    }
    if id == palette::CMD_GIT_SWITCH_BRANCH {
        let _ = mui_git_branches(handle);
        1
    } else if id == palette::CMD_GIT_PUSH {
        let _ = mui_git_push(handle);
        1
    } else if id == palette::CMD_GIT_PULL {
        let _ = mui_git_pull(handle);
        1
    } else if id == palette::CMD_GIT_FETCH {
        let _ = mui_git_fetch(handle);
        1
    } else if id == palette::CMD_GIT_TOGGLE_BLAME {
        let _ = crate::featureabi::mui_blame_toggle(handle);
        1
    } else if id == palette::CMD_GIT_STAGE_ALL {
        let _ = mui_scm_stage_all(handle);
        1
    } else if id == palette::CMD_GIT_UNSTAGE_ALL {
        let _ = mui_scm_unstage_all(handle);
        1
    } else {
        0
    }
}

/// Draw the branch-switcher overlay (a palette-styled centered card). No-op when
/// inactive. Lists the filtered branches (current marked) + a "Create branch…"
/// row, or — in create mode — a single new-branch-name input.
#[no_mangle]
pub extern "C" fn mui_branch_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.branch_picker.is_active() {
        return;
    }
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let picker = std::mem::take(&mut ctx.branch_picker);
    let old_clip = ctx.clip;
    ctx.clip = None;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    draw_branch_picker(&picker, ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.clip = old_clip;
    ctx.branch_picker = picker;
}

/// Render the branch picker card (extracted so `mui_branch_draw` can take the
/// picker out of `ctx` for the borrow).
fn draw_branch_picker(p: &crate::scm::BranchPicker, ctx: &mut MuiContext, width: u32, height: u32) {
    use crate::icons;
    let w = width as f32;
    let h = height as f32;
    let chrome = theme::CHROME_FONT_SIZE;
    let clip = ctx.clip;

    let creating = p.is_creating();
    let rows = branch_picker_visible_rows(height, if creating { 1 } else { p.count().min(10) });
    let (box_x, box_y, box_w, box_h, list_top, row_h) =
        branch_picker_geometry(width, height, rows);
    let head_h = 50.0_f32;
    let radius = 12.0_f32;

    // Scrim + glow + card.
    ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, 0.55));
    ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.85), 40.0);
    ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW(), 40.0);
    let mut card = theme::ELEVATED();
    card.a = 1.0;
    ctx.dl_round(box_x, box_y, box_w, box_h, radius, card);
    ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

    // Header: branch icon + title + the filter / new-name input.
    ctx.dl_icon(box_x + 16.0, box_y + 16.0, 16.0, 16.0, icons::BRANCH, theme::ACCENT_BRIGHT(), 1.6, false);
    let title = if creating { "Create Branch" } else { "Switch Branch" };
    ctx.text.queue_ui_sized(box_x + 40.0, box_y + 8.0, title, theme::TEXT(), 13.0, clip);
    let q = p.query_string();
    let (cx, cy, cw, ch) = branch_picker_close_rect(width, height, rows);
    let (qtext, qcol) = if q.is_empty() {
        let ph = if creating { "New branch name\u{2026}" } else { "Filter branches\u{2026}" };
        (ph.to_string(), theme::TEXT_3())
    } else {
        (q.clone(), theme::TEXT())
    };
    let query_x = box_x + 40.0;
    let query_budget = branch_picker_query_budget(query_x, cx, q.is_empty());
    let qtext = fit_head_px(&mut ctx.text, &qtext, query_budget, chrome);
    ctx.text.queue_ui_sized(query_x, box_y + 26.0, &qtext, qcol, chrome, clip);
    let (q_w, _) = ctx.text.measure_ui_sized(&qtext, chrome);
    let caret_x = if q.is_empty() {
        query_x + 1.0
    } else {
        (query_x + q_w + 1.0).min(cx - 14.0)
    };
    ctx.dl_round(caret_x, box_y + 25.0, 2.0, 15.0, 1.0, theme::ACCENT_BRIGHT());
    ctx.dl_round(cx, cy, cw, ch, 6.0, theme::BG_2());
    ctx.dl_stroke(cx, cy, cw, ch, 6.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_icon(cx + 5.0, cy + 5.0, 14.0, 14.0, icons::CLOSE, theme::TEXT_1(), 1.6, false);
    ctx.dl_rect(box_x + 1.0, box_y + head_h - 1.0, box_w - 2.0, 1.0, theme::BORDER());

    if creating {
        ctx.text.queue_ui_sized(box_x + 18.0, list_top + 8.0, "Press Enter to create & switch \u{00b7} Esc to cancel", theme::TEXT_3(), chrome - 1.0, clip);
        return;
    }
    for vis in 0..rows {
        let ry = list_top + vis as f32 * row_h;
        let selected = vis == p.selection();
        if selected {
            ctx.dl_round(box_x + 8.0, ry + 1.0, box_w - 16.0, row_h - 2.0, 7.0, theme::accent_a(0.20));
            ctx.dl_stroke(box_x + 8.0, ry + 1.0, box_w - 16.0, row_h - 2.0, 7.0, theme::ACCENT_LINE(), 1.0);
        }
        if p.is_create_row(vis) {
            ctx.dl_icon(box_x + 18.0, ry + (row_h - 14.0) * 0.5, 14.0, 14.0, icons::PLUS, theme::GREEN(), 1.7, false);
            ctx.text.queue_ui_sized(box_x + 42.0, ry + (row_h - chrome) * 0.5 - 1.0, "Create branch\u{2026}", theme::TEXT(), chrome, clip);
            continue;
        }
        if let Some(e) = p.entry_at(vis) {
            let (icon, icol) = if e.remote {
                (icons::GIT, theme::TEXT_3())
            } else {
                (icons::BRANCH, theme::ACCENT_BRIGHT())
            };
            ctx.dl_icon(box_x + 18.0, ry + (row_h - 14.0) * 0.5, 14.0, 14.0, icon, icol, 1.5, false);
            let name_col = if e.current { theme::GREEN() } else { theme::TEXT_1() };
            let name_x = box_x + 42.0;
            let name_right = branch_picker_entry_name_right(box_x, box_w, e.current || e.remote);
            let nm = fit_head_px(&mut ctx.text, &e.name, (name_right - name_x).max(0.0), chrome);
            ctx.text.queue_ui_sized(name_x, ry + (row_h - chrome) * 0.5 - 1.0, &nm, name_col, chrome, clip);
            if e.current {
                ctx.text.queue_ui_sized(box_x + box_w - 64.0, ry + (row_h - (chrome - 2.0)) * 0.5 - 1.0, "current", theme::GREEN(), chrome - 2.0, clip);
            } else if e.remote {
                ctx.text.queue_ui_sized(box_x + box_w - 60.0, ry + (row_h - (chrome - 2.0)) * 0.5 - 1.0, "remote", theme::TEXT_4(), chrome - 2.0, clip);
            }
        }
    }
}

// ===========================================================================
// Search panel — project-wide find/replace (shim walks the workspace)
// ===========================================================================

/// Append one Unicode scalar to the focused search field (query or replace).
#[no_mangle]
pub extern "C" fn mui_search_push_char(handle: i64, codepoint: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if codepoint >= 0 {
            ctx.search.push_char(codepoint as u32);
        }
    }
}

/// Backspace the focused search field.
#[no_mangle]
pub extern "C" fn mui_search_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.search.backspace();
    }
}

/// Toggle focus between the query field (0) and the replace field (1). Returns
/// the new focus (`1` if replace has focus).
#[no_mangle]
pub extern "C" fn mui_search_toggle_focus(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.search.replace_focus = !ctx.search.replace_focus;
    if ctx.search.replace_focus {
        1
    } else {
        0
    }
}

/// `1` if the replace field currently has focus, else `0`.
#[no_mangle]
pub extern "C" fn mui_search_replace_focus(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.search.replace_focus { 1 } else { 0 })
}

/// Length (chars) of the query field.
#[no_mangle]
pub extern "C" fn mui_search_query_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.search.query.len() as i32)
}

/// Close the Search panel without clearing query, replacement text, or results.
/// Returns `1` when it closed Search, or `0` when Search was already closed.
#[no_mangle]
pub extern "C" fn mui_search_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.active_panel == crate::PANEL_SEARCH {
        ctx.active_panel = crate::PANEL_EXPLORER;
        ctx.push_toast(crate::toast::Kind::Info, "Search panel closed");
        crate::abi::trace("search_close");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Search panel is already closed");
    crate::abi::trace("search_close noop");
    0
}

/// Clear Search results without changing query, replacement text, or focus.
/// Returns `1` when results were cleared, or `0` when they were already empty.
#[no_mangle]
pub extern "C" fn mui_search_clear_results(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.search.clear_results() {
        ctx.push_toast(crate::toast::Kind::Info, "Search results cleared");
        crate::abi::trace("search_clear_results");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Search results already empty");
    crate::abi::trace("search_clear_results noop");
    0
}

/// Run the project-wide search over the workspace root. Returns total matches.
#[no_mangle]
pub extern "C" fn mui_search_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    let query = ctx.search.query_string();
    let n = ctx.search.run(&dir);
    println!(
        "search: query=\"{}\" files={} matches={}",
        ctx.search.query_string(),
        ctx.search.file_count(),
        n
    );
    crate::abi::trace(&format!(
        "search_run query=\"{}\" files={} matches={}",
        ctx.search.query_string(),
        ctx.search.file_count(),
        n
    ));
    if query.trim().is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Enter text to search");
    } else if n == 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No project search results");
    }
    n
}

/// Replace every match of the query with the replacement across matched files.
/// Returns the number of replacements written. SAFE: ASCII-only substitution,
/// matched files only (see `search::SearchState::replace_all`).
#[no_mangle]
pub extern "C" fn mui_search_replace_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let query = ctx.search.query_string();
    if query.trim().is_empty() {
        ctx.search.clear_results();
        ctx.push_toast(crate::toast::Kind::Info, "Enter search text to replace");
        crate::abi::trace("search_replace_all replaced=0 empty-query");
        return 0;
    }
    if !ctx.search.results_match_current_query() {
        ctx.push_toast(crate::toast::Kind::Info, "Run Search before replacing");
        crate::abi::trace("search_replace_all replaced=0 stale-results");
        return 0;
    }
    let dir = workspace_dir(ctx);
    let (
        n,
        changed_paths,
        dirty_skipped,
        stale_skipped,
        missing_skipped,
        write_failed,
    ) = ctx
        .search
        .replace_all_with_changed_paths_skipping(&dir, |path| ctx.tabs.any_dirty_path(path));
    let (refreshed, stale_dirty) = refresh_replaced_open_tabs(ctx, &changed_paths);
    let dirty_skipped = dirty_skipped + stale_dirty;
    let skipped = dirty_skipped + stale_skipped + missing_skipped + write_failed;
    println!("search: replaced {n}");
    crate::abi::trace(&format!("search_replace_all replaced={n}"));
    if n > 0 || skipped > 0 {
        let suffix = if n == 1 { "" } else { "s" };
        if skipped > 0 {
            let file_suffix = if skipped == 1 { "" } else { "s" };
            let reason = match (
                dirty_skipped > 0,
                stale_skipped > 0,
                missing_skipped > 0,
                write_failed > 0,
            ) {
                (true, true, true, true) => "dirty, changed, missing, or failed",
                (true, true, true, false) => "dirty, changed, or missing",
                (true, true, false, true) => "dirty, changed, or failed",
                (true, true, false, false) => "dirty or changed",
                (true, false, true, true) => "dirty, missing, or failed",
                (true, false, true, false) => "dirty or missing",
                (true, false, false, true) => "dirty or failed",
                (true, false, false, false) => "dirty open",
                (false, true, true, true) => "changed, missing, or failed",
                (false, true, true, false) => "changed or missing",
                (false, true, false, true) => "changed or failed",
                (false, true, false, false) => "changed",
                (false, false, true, true) => "missing or failed",
                (false, false, true, false) => "missing",
                (false, false, false, true) => "failed",
                (false, false, false, false) => "changed",
            };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!(
                    "Replaced {n} occurrence{suffix}; skipped {skipped} {reason} file{file_suffix}"
                ),
            );
        } else {
            ctx.push_toast(
                crate::toast::Kind::Success,
                format!("Replaced {n} occurrence{suffix}"),
            );
        }
        if refreshed > 0 {
            crate::abi::sync_active_path(ctx);
        }
    } else {
        ctx.push_toast(crate::toast::Kind::Warn, "No project replacements");
    }
    n
}

fn refresh_replaced_open_tabs(
    ctx: &mut MuiContext,
    changed_paths: &[std::path::PathBuf],
) -> (usize, usize) {
    let mut refreshed = 0usize;
    let mut dirty_skipped = 0usize;
    for path in changed_paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let (path_refreshed, path_dirty_skipped) = ctx.tabs.reload_all_clean_path(path, &bytes);
        refreshed += path_refreshed;
        dirty_skipped += path_dirty_skipped;
    }
    (refreshed, dirty_skipped)
}

/// Number of files with matches.
#[no_mangle]
pub extern "C" fn mui_search_file_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.search.file_count())
}

/// Total match count across all files.
#[no_mangle]
pub extern "C" fn mui_search_match_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.search.match_count())
}

/// File index of match `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_search_match_file(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.search.match_at(i as usize).map_or(-1, |m| m.file as i32))
}

/// 0-based line of match `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_search_match_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.search.match_at(i as usize).map_or(-1, |m| m.line))
}

/// 0-based column of match `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_search_match_col(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.search.match_at(i as usize).map_or(-1, |m| m.col))
}

/// Open the file of match `i` as a tab and move the cursor to the match
/// (line + col), scrolling it near the top. Returns the resulting tab index, or
/// `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_search_open(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No search result selected");
        return -1;
    }
    let (path, line, col, fingerprint) = {
        let Some(m) = ctx.search.match_at(i as usize) else {
            ctx.push_toast(crate::toast::Kind::Info, "No search result selected");
            return -1;
        };
        let Some(f) = ctx.search.file_at(m.file) else {
            let _ = ctx.search.clear_results();
            ctx.push_toast(crate::toast::Kind::Info, "Search result file no longer listed");
            return -1;
        };
        (f.path.clone(), m.line, m.col, f.fingerprint)
    };
    let name = crate::abi::file_target_name(&path);
    let bytes = match search_target_kind(&path) {
        SearchTargetKind::File => match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                return refresh_bad_search_target(
                    ctx,
                    format!("Search target missing: {name}"),
                    true,
                );
            }
        },
        SearchTargetKind::Missing => {
            return refresh_bad_search_target(
                ctx,
                format!("Search target missing: {name}"),
                true,
            );
        }
        SearchTargetKind::NotFile => {
            return refresh_bad_search_target(
                ctx,
                format!("Search target is not a file: {name}"),
                true,
            );
        }
    };
    if crate::search::content_fingerprint(&bytes) != fingerprint {
        return refresh_bad_search_target(
            ctx,
            format!("Search result changed: {name}; results refreshed"),
            false,
        );
    }
    let opened_path = path.to_string_lossy().replace('\\', "/");
    let idx = ctx.tabs.open_path(path.clone());
    crate::abi::sync_active_path(ctx);
    crate::abi::record_opened_file(ctx, &path);
    let model = ctx.tabs.active_model_mut();
    model.move_to(line, col);
    let first = (line - 2).max(0);
    model.set_first_visible(first as usize);
    crate::abi::trace(&format!(
        "search_open idx={} path={} line={} col={}",
        i,
        opened_path,
        line + 1,
        col + 1
    ));
    idx as i32
}

enum SearchTargetKind {
    File,
    Missing,
    NotFile,
}

fn search_target_kind(path: &std::path::Path) -> SearchTargetKind {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => SearchTargetKind::File,
        Ok(_) => SearchTargetKind::NotFile,
        Err(_) => SearchTargetKind::Missing,
    }
}

fn refresh_bad_search_target(
    ctx: &mut MuiContext,
    message: String,
    refresh_workspace_views: bool,
) -> i32 {
    let dir = workspace_dir(ctx);
    let _ = ctx.search.run(&dir);
    if refresh_workspace_views {
        crate::abi::refresh_workspace_file_views(ctx);
    }
    ctx.push_toast(crate::toast::Kind::Warn, message);
    -1
}

/// Y pixel (top) of the first search-result row.
fn search_rows_top() -> f32 {
    40.0 + 30.0 + 6.0 + 30.0 + 24.0
}

fn search_rows_bottom(height: u32) -> f32 {
    height as f32 - layout::LINE_H() - 4.0
}

fn search_field_geometry() -> (f32, f32, f32) {
    let head_h = 40.0;
    let box_h = 30.0;
    let qy = head_h + 6.0;
    let ry = qy + box_h + 6.0;
    (qy, ry, box_h)
}

fn search_field_button_x(sx: f32, sw: f32) -> (f32, f32) {
    (sx + sw - 46.0, sx + sw - 8.0)
}

pub(crate) fn search_header_action_centers(sx: f32, sw: f32) -> [(f32, i32); 2] {
    [(sx + sw - 57.5, 3), (sx + sw - 27.5, 1)]
}

fn search_replace_button_enabled(query: &str, matches: i32, current_results: bool) -> bool {
    !query.trim().is_empty() && matches > 0 && current_results
}

/// Search-panel mouse action for the last click:
/// `0` = no action, `1` = run search, `2` = replace all, `3` = clear results.
///
/// Clicking either input also moves keyboard focus to that field, so the panel
/// no longer depends on Tab-only field switching.
#[no_mangle]
pub extern "C" fn mui_search_action_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    if !ctx.sidebar_visible
        || ctx.active_panel != crate::PANEL_SEARCH
        || ctx.last_event.x < sx
        || ctx.last_event.x > sx + sw
    {
        return 0;
    }

    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    let (qy, ry, box_h) = search_field_geometry();
    let box_x0 = sx + 10.0;
    let (btn_x0, btn_x1) = search_field_button_x(sx, sw);
    let box_x1 = (sx + sw - 10.0).max(btn_x1);
    if (0.0..=40.0).contains(&y) {
        for (cx, action) in search_header_action_centers(sx, sw) {
            if x >= cx - 3.0 && x < cx + 18.0 {
                ctx.search.replace_focus = false;
                let label = if action == 1 { "header_run" } else { "header_clear" };
                crate::abi::trace(&format!("search_action x={x:.1} y={y:.1} -> {label}"));
                return action;
            }
        }
    }
    if (box_x0..=box_x1).contains(&x) && (qy..=qy + box_h).contains(&y) {
        ctx.search.replace_focus = false;
        if (btn_x0..=btn_x1).contains(&x) {
            crate::abi::trace(&format!("search_action x={x:.1} y={y:.1} -> run"));
            return 1;
        }
        crate::abi::trace(&format!("search_action x={x:.1} y={y:.1} -> focus_query"));
        return 0;
    }
    if (box_x0..=box_x1).contains(&x) && (ry..=ry + box_h).contains(&y) {
        ctx.search.replace_focus = true;
        if (btn_x0..=btn_x1).contains(&x)
            && search_replace_button_enabled(
                &ctx.search.query_string(),
                ctx.search.match_count(),
                ctx.search.results_match_current_query(),
            )
        {
            crate::abi::trace(&format!("search_action x={x:.1} y={y:.1} -> replace_all"));
            return 2;
        }
        crate::abi::trace(&format!("search_action x={x:.1} y={y:.1} -> focus_replace"));
    }
    0
}

/// Map the last click's pixel y to a flattened search-result match index, or
/// `-1` for a file-header row / no row.
#[no_mangle]
pub extern "C" fn mui_search_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let sx0 = layout::RAIL_W;
    let sx1 = layout::sidebar_right();
    if !ctx.sidebar_visible || ctx.last_event.x < sx0 || ctx.last_event.x > sx1 {
        return -1;
    }
    let top = search_rows_top();
    let y = ctx.last_event.y;
    if y < top || y >= search_rows_bottom(ctx.gpu.height) {
        return -1;
    }
    let clicked = ((y - top) / layout::LINE_H()).floor() as i32;
    let mut visual = 0;
    let fc = ctx.search.file_count();
    let mut mi = 0;
    for f in 0..fc {
        if visual == clicked {
            return -1;
        }
        visual += 1;
        let fmcount = ctx.search.file_at(f as usize).map_or(0, |x| x.match_count);
        for _ in 0..fmcount {
            if visual == clicked {
                return mi;
            }
            visual += 1;
            mi += 1;
        }
    }
    -1
}

#[cfg(test)]
mod search_panel_tests {
    use super::{fit_head_px, fit_tail_px, search_replace_button_enabled};

    #[test]
    fn search_preview_fits_measured_row_budget() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };

        let size = crate::theme::CHROME_FONT_SIZE;
        let budget = 165.0;
        let shown = fit_head_px(
            &mut ctx.text,
            "render_really_long_search_preview_line_that_used_to_clip_in_sidebar",
            budget,
            size,
        );
        let shown_w = ctx.text.measure_ui_sized(&shown, size).0;

        assert!(shown.ends_with('\u{2026}'));
        assert!(
            shown_w <= budget + 0.5,
            "search preview should fit measured row budget: {shown}"
        );
    }

    #[test]
    fn search_field_tail_fits_measured_input_budget() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };

        let size = crate::theme::CHROME_FONT_SIZE;
        let budget = 150.0;
        let shown = fit_tail_px(
            &mut ctx.text,
            "workspace/src/deeply/nested/search_target_with_long_name.mty",
            budget,
            size,
        );
        let shown_w = ctx.text.measure_ui_sized(&shown, size).0;

        assert!(shown.starts_with('\u{2026}'));
        assert!(
            shown_w <= budget + 0.5,
            "search tail text should fit measured input budget: {shown}"
        );
    }

    #[test]
    fn search_replace_button_tracks_real_query_availability() {
        assert!(!search_replace_button_enabled("", 1, true));
        assert!(!search_replace_button_enabled("   ", 1, true));
        assert!(!search_replace_button_enabled("opened", 0, true));
        assert!(!search_replace_button_enabled("opened", 1, false));
        assert!(search_replace_button_enabled("opened", 1, true));
    }
}

#[cfg(test)]
mod branch_picker_surface_tests {
    use super::{
        branch_picker_close_rect, branch_picker_entry_name_right, branch_picker_geometry,
        branch_picker_query_budget,
    };

    #[test]
    fn branch_close_rect_stays_inside_header() {
        let rows = 6;
        let (box_x, box_y, box_w, _box_h, list_top, _row_h) =
            branch_picker_geometry(860, 560, rows);
        let (cx, cy, cw, ch) = branch_picker_close_rect(860, 560, rows);
        assert!(cx >= box_x);
        assert!(cx + cw <= box_x + box_w);
        assert!(cy >= box_y);
        assert!(cy + ch < list_top);
    }

    #[test]
    fn branch_query_budget_stops_before_close_button() {
        let rows = 6;
        let (box_x, _box_y, _box_w, _box_h, _list_top, _row_h) =
            branch_picker_geometry(860, 560, rows);
        let (close_x, _close_y, _close_w, _close_h) = branch_picker_close_rect(860, 560, rows);
        let query_x = box_x + 40.0;
        let placeholder_budget = branch_picker_query_budget(query_x, close_x, true);
        let query_budget = branch_picker_query_budget(query_x, close_x, false);

        assert!(placeholder_budget < query_budget);
        assert!(query_x + placeholder_budget <= close_x - 24.0);
        assert!(query_x + query_budget <= close_x - 14.0);
        assert_eq!(branch_picker_query_budget(close_x, query_x, false), 0.0);
    }

    #[test]
    fn branch_row_name_budget_reserves_badge_space() {
        let (box_x, _box_y, box_w, _box_h, _list_top, _row_h) =
            branch_picker_geometry(860, 560, 6);
        let plain_right = branch_picker_entry_name_right(box_x, box_w, false);
        let badge_right = branch_picker_entry_name_right(box_x, box_w, true);

        assert!(badge_right < plain_right);
        assert!(badge_right <= box_x + box_w - 72.0);
        assert!(plain_right <= box_x + box_w - 20.0);
    }
}

#[cfg(test)]
mod scm_empty_state_tests {
    use super::fit_head_px;

    #[test]
    fn scm_empty_hint_keeps_action_verb_when_narrow() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(360, 240) else {
            return;
        };
        let shown = fit_head_px(
            &mut ctx.text,
            "Refresh to scan Git status.",
            220.0,
            crate::theme::CHROME_FONT_SIZE - 1.0,
        );
        assert_eq!(shown, "Refresh to scan Git status.");
    }
}

/// Draw the Search panel (query + replace inputs, then results grouped by file
/// with per-match `line: preview` rows and the matched span highlighted in
/// indigo). No-op when the sidebar is hidden or this panel isn't active.
#[no_mangle]
pub extern "C" fn mui_search_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_SEARCH {
        return;
    }
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let sx = layout::RAIL_W;
    let sw = layout::sidebar_w();
    use crate::icons;

    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // header band
    let head_h = 40.0;
    ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
    ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
    let title = "SEARCH";
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ctx.text.queue_ui_sized(sx + 14.0, (head_h - (chrome - 2.0)) * 0.5 - 1.0, &tracked, theme::DIM(), chrome - 2.0, clip);
    let act_y = (head_h - 15.0) * 0.5;
    for (x, action) in search_header_action_centers(sx, sw) {
        let icon = if action == 1 { icons::REFRESH } else { icons::TRASH };
        let color = if action == 1 || ctx.search.match_count() > 0 { theme::TEXT_3() } else { theme::TEXT_4() };
        ctx.dl_stroke(x - 2.5, 8.0, 24.0, 24.0, 5.0, theme::BORDER_SOFT(), 1.0);
        ctx.dl_icon(x + 2.0, act_y, 15.0, 15.0, icon, color, 1.5, false);
    }

    let replace_focus = ctx.search.replace_focus;
    let query = ctx.search.query_string();
    let replace = ctx.search.replace_string();

    // query box
    let (qy, ry, box_h) = search_field_geometry();
    let q_border = if !replace_focus { theme::ACCENT_LINE() } else { theme::BORDER_STRONG() };
    ctx.dl_round(sx + 10.0, qy, sw - 20.0, box_h, 7.0, theme::BG_1());
    ctx.dl_stroke(sx + 10.0, qy, sw - 20.0, box_h, 7.0, q_border, 1.0);
    ctx.dl_icon(sx + 16.0, qy + (box_h - 13.0) * 0.5, 13.0, 13.0, icons::SEARCH, theme::TEXT_3(), 1.5, false);
    let (q_text, q_col) = if query.is_empty() {
        ("Search".to_string(), theme::TEXT_3())
    } else {
        (query.clone(), theme::TEXT())
    };
    let (btn_x0, btn_x1) = search_field_button_x(sx, sw);
    let field_budget = (btn_x0 - (sx + 38.0)).max(0.0);
    let qshown = fit_tail_px(&mut ctx.text, &q_text, field_budget, chrome);
    ctx.text.queue_ui_sized(sx + 34.0, qy + (box_h - chrome) * 0.5 - 1.0, &qshown, q_col, chrome, clip);
    ctx.dl_round(btn_x0, qy + 4.0, btn_x1 - btn_x0, box_h - 8.0, 5.0, theme::BG_4());
    ctx.dl_icon(btn_x0 + 6.0, qy + 8.0, 14.0, 14.0, icons::REFRESH, theme::TEXT_1(), 1.4, false);

    // replace box
    let r_border = if replace_focus { theme::ACCENT_LINE() } else { theme::BORDER_STRONG() };
    ctx.dl_round(sx + 10.0, ry, sw - 20.0, box_h, 7.0, theme::BG_1());
    ctx.dl_stroke(sx + 10.0, ry, sw - 20.0, box_h, 7.0, r_border, 1.0);
    let replace_ready =
        search_replace_button_enabled(&query, ctx.search.match_count(), ctx.search.results_match_current_query());
    let replace_icon_col = if replace_focus { theme::ACCENT_BRIGHT() } else { theme::DIM() };
    ctx.dl_icon(sx + 16.0, ry + (box_h - 13.0) * 0.5, 13.0, 13.0, icons::REPLACE, replace_icon_col, 1.5, false);
    let (r_text, r_col) = if replace.is_empty() {
        ("Replace".to_string(), if replace_focus { theme::TEXT_1() } else { theme::DIM() })
    } else {
        (replace.clone(), theme::TEXT())
    };
    let rshown = fit_tail_px(&mut ctx.text, &r_text, field_budget, chrome);
    ctx.text.queue_ui_sized(sx + 34.0, ry + (box_h - chrome) * 0.5 - 1.0, &rshown, r_col, chrome, clip);
    let replace_btn_bg = if replace_ready { theme::accent_a(0.16) } else { theme::BG_4() };
    let replace_btn_border = if replace_ready { theme::ACCENT_LINE() } else { theme::BORDER_STRONG() };
    let replace_btn_icon = if replace_ready { theme::ACCENT_BRIGHT() } else { theme::TEXT_3() };
    ctx.dl_round(btn_x0, ry + 4.0, btn_x1 - btn_x0, box_h - 8.0, 5.0, replace_btn_bg);
    ctx.dl_stroke(btn_x0, ry + 4.0, btn_x1 - btn_x0, box_h - 8.0, 5.0, replace_btn_border, 1.0);
    ctx.dl_icon(btn_x0 + 6.0, ry + 8.0, 14.0, 14.0, icons::REPLACE, replace_btn_icon, 1.6, false);

    // results
    let total = ctx.search.match_count();
    let fc = ctx.search.file_count();
    if total == 0 {
        let msg = if query.trim().is_empty() {
            "Type to search the project"
        } else {
            "No results"
        };
        ctx.text.queue_ui_sized(sx + 14.0, search_rows_top() + 4.0, msg, theme::TEXT_3(), chrome, clip);
        return;
    }
    let summary = format!("{total} results in {fc} files");
    ctx.text.queue_ui_sized(sx + 14.0, ry + box_h + 6.0, &summary, theme::TEXT_3(), chrome - 2.0, clip);

    let row_h = layout::LINE_H();
    let top = search_rows_top();
    let bottom = search_rows_bottom(ctx.gpu.height);
    let needle_len = ctx.search.query.len() as i32;
    let mut visual = 0i32;
    let mut mi = 0i32;
    for f in 0..fc {
        let (rel, mc) = {
            let Some(file) = ctx.search.file_at(f as usize) else { continue };
            (file.rel.clone(), file.match_count)
        };
        let y = top + (visual as f32) * row_h;
        if y + row_h > bottom {
            break;
        }
        ctx.dl_icon(sx + 12.0, y + (row_h - 12.0) * 0.5, 12.0, 12.0, icons::CHEVRON_DOWN, theme::TEXT_3(), 2.0, false);
        let (icon, icol) = crate::abi::file_icon_for(&rel, false);
        ctx.dl_icon(sx + 28.0, y + (row_h - 14.0) * 0.5, 14.0, 14.0, icon, icol, 1.4, false);
        let rel_budget = (sx + sw - 40.0 - (sx + 46.0)).max(0.0);
        let rshown = fit_tail_px(&mut ctx.text, &rel, rel_budget, chrome);
        ctx.text.queue_ui_sized(sx + 46.0, y + (row_h - chrome) * 0.5 - 1.0, &rshown, theme::TEXT_1(), chrome, clip);
        let cnt = mc.to_string();
        ctx.dl_round(sx + sw - 30.0, y + (row_h - 15.0) * 0.5, 20.0, 15.0, 7.5, theme::BG_4());
        ctx.text.queue_ui_sized(sx + sw - 26.0, y + (row_h - (chrome - 2.0)) * 0.5 - 1.0, &cnt, theme::TEXT_3(), chrome - 2.0, clip);
        visual += 1;

        for _ in 0..mc {
            let y = top + (visual as f32) * row_h;
            if y + row_h > bottom {
                return;
            }
            let (line, col, preview) = {
                let Some(m) = ctx.search.match_at(mi as usize) else { break };
                (m.line, m.col, m.preview.clone())
            };
            let trimmed = preview.trim_start();
            let trimmed_off = preview.chars().count() as i32 - trimmed.chars().count() as i32;
            let ln = format!("{}", line + 1);
            ctx.text.queue_ui_sized(sx + 30.0, y + (row_h - chrome) * 0.5 - 1.0, &ln, theme::TEXT_4(), chrome - 1.0, clip);
            let ln_w = ctx.text.measure_ui_sized(&ln, chrome - 1.0).0;
            let preview_x = sx + 30.0 + ln_w + 8.0;
            let rel_col = col - trimmed_off;
            let preview_budget = (sx + sw - 14.0 - preview_x).max(0.0);
            let pv = fit_head_px(&mut ctx.text, trimmed, preview_budget, chrome);
            if rel_col >= 0 && needle_len > 0 {
                let start = rel_col as usize;
                let pv_chars = pv.chars().count();
                if start < pv_chars {
                    let take = (needle_len as usize).min(pv_chars.saturating_sub(start));
                    let prefix = pv.chars().take(start).collect::<String>();
                    let matched = pv.chars().skip(start).take(take).collect::<String>();
                    let (prefix_w, _) = ctx.text.measure_ui_sized(&prefix, chrome);
                    let (match_w, _) = ctx.text.measure_ui_sized(&matched, chrome);
                    let hx = preview_x + prefix_w;
                    if hx < sx + sw - 12.0 {
                        let hw = match_w.max(2.0);
                        ctx.dl_round(hx - 1.0, y + 2.0, hw + 2.0, row_h - 5.0, 3.0, theme::SELECTION());
                    }
                }
            }
            ctx.text.queue_ui_sized(preview_x, y + (row_h - chrome) * 0.5 - 1.0, &pv, theme::TEXT_1(), chrome, clip);
            visual += 1;
            mi += 1;
        }
    }
}

// ===========================================================================
// AI copilot panel — right-docked chat over the Anthropic Messages API.
// (Backend + state + draw live in `crate::ai`; this is the scalar ABI veneer.)
// ===========================================================================

/// Toggle the AI panel open/closed (the Agents rail icon / Ctrl+Shift+A).
/// Returns `1` if it is now open, `0` if closed.
#[no_mangle]
pub extern "C" fn mui_ai_open(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.ai.open = !ctx.ai.open;
    if ctx.ai.open {
        crate::abi::trace("ai_open");
        1
    } else {
        crate::abi::trace("ai_close");
        0
    }
}

/// Open the AI copilot without toggling it closed. Returns `1` when visible.
#[no_mangle]
pub extern "C" fn mui_ai_show(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.ai.open = true;
    crate::abi::trace("ai_open");
    1
}

/// Close the AI copilot without clearing its transcript/input. Returns `1` when
/// a visible panel was closed, or `0` when it was already hidden.
#[no_mangle]
pub extern "C" fn mui_ai_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.ai.open {
        ctx.push_toast(crate::toast::Kind::Info, "AI Copilot is already closed");
        crate::abi::trace("ai_close noop");
        return 0;
    }
    ctx.ai.open = false;
    ctx.push_toast(crate::toast::Kind::Info, "AI Copilot closed");
    crate::abi::trace("ai_close");
    1
}

/// Clear the AI transcript and draft, leaving the panel visible so the empty
/// copilot surface is immediately obvious. Returns `1` when state changed.
#[no_mangle]
pub extern "C" fn mui_ai_clear(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.ai.open = true;
    if ctx.ai.clear() {
        ctx.push_toast(crate::toast::Kind::Info, "AI Copilot chat cleared");
        crate::abi::trace("ai_clear");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "AI Copilot chat is already empty");
        crate::abi::trace("ai_clear noop");
        0
    }
}

/// `1` if the AI panel is currently open, else `0`.
#[no_mangle]
pub extern "C" fn mui_ai_is_open(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.ai.open { 1 } else { 0 })
}

pub const AI_CLICK_CLEAR: i32 = 4;

/// Map the last click to the right-docked AI panel:
/// `0` = miss, `1` = input/body focus, `2` = send button, `3` = close button,
/// `4` = clear chat.
#[no_mangle]
pub extern "C" fn mui_ai_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.ai.open {
        return 0;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let visible_w = layout::dock_visible_width(ctx.gpu.width, ctx.gpu.phys_width);
    let visible_h = layout::visible_height(ctx.gpu.height, ctx.gpu.phys_height);
    let chat_available = ctx.ai.force_transcript || crate::ai::api_key().is_some();
    let (px, pw, input_y, input_h) =
        crate::ai::input_geometry_for_state_measured(&mut ctx.text, &ctx.ai.input, chat_available, visible_w, visible_h);
    if x < px || x > px + pw || y < layout::TAB_BAR_H || y > visible_h as f32 {
        crate::abi::trace(&format!("ai_click x={x:.1} y={y:.1} -> 0"));
        return 0;
    }
    // Preserve the title-bar run/more/window-control strip: it is drawn above
    // the AI panel and should continue to receive clicks.
    let controls_x = crate::titlebar::controls_x(ctx.gpu.width as f32);
    if y <= layout::TAB_BAR_H && x >= controls_x - crate::titlebar::ACTION_STRIP_W {
        return 0;
    }
    let (close_x, close_y, close_w, close_h) = crate::ai::close_geometry(visible_w);
    if x >= close_x && x <= close_x + close_w && y >= close_y && y <= close_y + close_h {
        crate::abi::trace(&format!("ai_click x={x:.1} y={y:.1} -> 3"));
        return 3;
    }
    let (clear_x, clear_y, clear_w, clear_h) = crate::ai::clear_geometry(visible_w);
    if x >= clear_x && x <= clear_x + clear_w && y >= clear_y && y <= clear_y + clear_h {
        crate::abi::trace(&format!("ai_click x={x:.1} y={y:.1} -> clear"));
        return AI_CLICK_CLEAR;
    }
    let send_x0 = px + pw - 44.0;
    let send_x1 = px + pw - 12.0;
    let send_y0 = input_y + input_h - 36.0;
    let send_y1 = input_y + input_h - 4.0;
    if (send_x0..=send_x1).contains(&x) && (send_y0..=send_y1).contains(&y) {
        crate::abi::trace(&format!("ai_click x={x:.1} y={y:.1} -> 2"));
        return 2;
    }
    crate::abi::trace(&format!("ai_click x={x:.1} y={y:.1} -> 1"));
    1
}

/// `1` if an `ANTHROPIC_API_KEY` (or `CLAUDE_API_KEY`) is set, else `0`. The IDE
/// uses this to decide whether sending is meaningful.
#[no_mangle]
pub extern "C" fn mui_ai_has_key(_handle: i64) -> i32 {
    if crate::ai::api_key().is_some() {
        1
    } else {
        0
    }
}

/// Append one Unicode scalar to the AI input buffer.
#[no_mangle]
pub extern "C" fn mui_ai_input_push(handle: i64, codepoint: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if codepoint >= 0 {
            if let Some(ch) = char::from_u32(codepoint as u32) {
                ctx.ai.input.push(ch);
            }
        }
    }
}

/// Delete the last char of the AI input buffer.
#[no_mangle]
pub extern "C" fn mui_ai_input_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.ai.input.pop();
    }
}

/// Insert a newline into the AI input (Shift+Enter).
#[no_mangle]
pub extern "C" fn mui_ai_input_newline(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.ai.input.push('\n');
    }
}

/// Number of chars in the AI input buffer.
#[no_mangle]
pub extern "C" fn mui_ai_input_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.ai.input.chars().count() as i32)
}

fn ai_send_with_feedback(ctx: &mut MuiContext) -> i32 {
    if ctx.ai.is_streaming() {
        ctx.push_toast(crate::toast::Kind::Info, "AI response already in progress");
        crate::abi::trace("ai_send blocked=streaming");
        return 0;
    }
    if ctx.ai.input.trim().is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Type a message before sending");
        crate::abi::trace("ai_send blocked=blank");
        return 0;
    }
    if crate::ai::api_key().is_none() {
        ctx.push_toast(crate::toast::Kind::Warn, "Set ANTHROPIC_API_KEY to enable AI Copilot");
        crate::abi::trace("ai_send blocked=no_key");
        return 0;
    }
    let file_name = ctx.file_name.clone();
    let content = ctx.tabs.active_model().as_text();
    let selection = ctx.tabs.active_model().selected_text();
    let system = crate::ai::build_system_prompt(&file_name, &content, &selection);
    if ctx.ai.send(system) {
        crate::abi::trace("ai_send started");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Warn, "AI Copilot could not start");
        crate::abi::trace("ai_send blocked=send_failed");
        0
    }
}

/// Send the current input as a new turn, embedding the active file's content
/// (and any selection) as context. Spawns the background streaming request.
/// Returns `1` if a request was started, `0` otherwise (blank input / already
/// streaming / no key).
#[no_mangle]
pub extern "C" fn mui_ai_send(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ai_send_with_feedback(ctx)
}

/// Seed an inline-ask: pre-fill the AI input with `instruction` about the current
/// selection/file, open the panel, and send it. Mighty stages the instruction
/// via `mui_ai_input_push` (reusing the prompt UI) then calls this. Returns the
/// same as [`mui_ai_send`].
#[no_mangle]
pub extern "C" fn mui_ai_send_inline(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.ai.open = true;
    // The instruction is already in ctx.ai.input (pushed char-by-char by Mighty).
    ai_send_with_feedback(ctx)
}

/// Drain pending stream deltas into the transcript. Returns `1` if the
/// transcript changed this frame (the IDE redraws), else `0`. Called each frame.
#[no_mangle]
pub extern "C" fn mui_ai_pump(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.ai.pump() {
        1
    } else {
        0
    }
}

/// `1` while a request is in flight (assistant turn streaming), else `0`.
#[no_mangle]
pub extern "C" fn mui_ai_streaming(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.ai.is_streaming() { 1 } else { 0 })
}

/// Scroll the transcript by `dir` (negative = up/earlier, positive = down).
#[no_mangle]
pub extern "C" fn mui_ai_scroll(handle: i64, dir: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let step = layout::LINE_H() * 3.0;
        ctx.ai.scroll += dir as f32 * step;
        if ctx.ai.scroll < 0.0 {
            ctx.ai.scroll = 0.0;
        }
    }
}

/// Number of turns in the transcript (for tests / status).
#[no_mangle]
pub extern "C" fn mui_ai_turn_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.ai.transcript.len() as i32)
}

/// Draw the AI panel (no-op when closed). Mighty calls this each frame.
#[no_mangle]
pub extern "C" fn mui_ai_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.ai.open {
        return;
    }
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let visible_w = layout::dock_visible_width(w, ctx.gpu.phys_width);
    let visible_h = layout::visible_height(h, ctx.gpu.phys_height);
    // Render on the overlay layer so the chat card occludes editor glyphs that
    // sit underneath the right-docked panel band.
    let panel = std::mem::take(&mut ctx.ai);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    panel.draw(ctx, visible_w, visible_h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.ai = panel;
}
