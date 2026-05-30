//! Workspace (Open Folder) + quick-fix lightbulb ABI — the scalar `mui_*`
//! veneer over [`crate::workspace`] and [`crate::lightbulb`].
//!
//! ## Workspace / Open Folder
//! The workspace root is an EXPLICIT, settable concept. Setting it (via a native
//! Windows folder picker, or a typed-path fallback) re-roots EVERYTHING that
//! reads the project directory: the file tree, the Quick-Open index, project
//! Search, git status, and Agents discovery. A recent-folders MRU (cap 10) is
//! persisted to the config dir and surfaced on the Welcome screen + the "File:
//! Open Recent" palette command.
//!
//! ## Quick-fix lightbulb
//! A debounced gutter indicator: when the cursor's line has code actions, an
//! accent-tinted bulb is drawn just left of the line. Clicking it (or Ctrl+.)
//! opens the code-action menu. The "has actions" probe reuses the code-action
//! request path ([`crate::abi::compute_line_actions`]) but never opens the menu,
//! and is rate-limited by [`crate::lightbulb::Lightbulb`] so the LSP isn't
//! spammed each frame.

use std::path::PathBuf;

use crate::{layout, theme, MuiContext};

/// Cast an opaque `i64` handle back to a context reference (mirrors `abi::ctx`).
#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

// ===========================================================================
// Workspace root — the source of truth for the project directory
// ===========================================================================

/// The effective project root: the EXPLICIT workspace root when set, else the
/// file-tree root (the historical derived behavior). Used by the file tree,
/// Quick-Open, Search, git and Agents so they all agree on one directory.
pub(crate) fn effective_root(ctx: &MuiContext) -> PathBuf {
    if ctx.workspace.is_empty() {
        ctx.tree.root().to_path_buf()
    } else {
        ctx.workspace.root().to_path_buf()
    }
}

/// Number of chars in the workspace root path (for the Mighty side to size /
/// stream it). `0` when no explicit workspace is set.
#[no_mangle]
pub extern "C" fn mui_ws_root_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.workspace.root().to_string_lossy().chars().count() as i32)
}

/// The `i`th char of the workspace root path as a codepoint, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_ws_root_char(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.workspace
            .root()
            .to_string_lossy()
            .chars()
            .nth(i as usize)
            .map_or(-1, |ch| ch as i32)
    })
}

/// Number of chars in the workspace display name (root basename / "workspace").
#[no_mangle]
pub extern "C" fn mui_ws_name_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.workspace.name().chars().count() as i32)
}

/// The `i`th char of the workspace name as a codepoint, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_ws_name_char(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.workspace.name().chars().nth(i as usize).map_or(-1, |ch| ch as i32)
    })
}

/// Re-root the workspace from a path STAGED via the existing `mui_path_push` /
/// `mui_path_clear` byte-staging ABI (the same buffer the Open-File prompt uses).
/// Validates the staged path is an existing directory, sets the workspace + tree
/// root, invalidates the Quick-Open index, re-runs git status + Agents discovery,
/// and records the folder in the recents MRU (persisted). Toasts the result.
///
/// Returns `1` on success, `0` on failure (bad path; a warn toast explains).
#[no_mangle]
pub extern "C" fn mui_ws_open(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let typed = String::from_utf8_lossy(&staged).into_owned();
    open_folder(ctx, &typed)
}

/// Open the native Windows folder picker (a `FolderBrowserDialog` driven through
/// PowerShell) and re-root to the chosen folder. Returns `1` on success, `0` if
/// the user cancelled / the dialog is unavailable (the IDE then falls back to a
/// typed-path prompt). The interactive dialog is the user's machine doing its
/// thing — Mighty just launches it + reads the chosen path back.
#[no_mangle]
pub extern "C" fn mui_ws_open_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    match pick_folder_native() {
        Some(path) if !path.trim().is_empty() => open_folder(ctx, &path),
        _ => {
            println!("ws: native folder dialog cancelled / unavailable");
            0
        }
    }
}

/// Re-root to `path` from an already-borrowed context (the Welcome recent-folder
/// click path). Mirrors [`mui_ws_open_recent`] but takes a `PathBuf` directly.
pub(crate) fn mui_ws_open_recent_path(ctx: &mut MuiContext, path: &std::path::Path) -> i32 {
    open_folder(ctx, &path.to_string_lossy())
}

/// Shared re-root worker: validate `input` as a folder, set the workspace + tree
/// root, refresh the dependent indexes, record + persist the recents, and toast.
fn open_folder(ctx: &mut MuiContext, input: &str) -> i32 {
    match crate::workspace::validate_folder(input) {
        Ok(root) => {
            let changed = ctx.workspace.set_root(root.clone());
            // Always re-root the tree (it mirrors the workspace) + refresh the
            // dependent indexes; even an unchanged root benefits from a rescan.
            ctx.tree.set_root(root.clone());
            refresh_dependents(ctx, &root);
            // Record + persist the recents MRU.
            ctx.recent_workspaces.record(root.clone());
            persist_recents(ctx);
            let name = ctx.workspace.name().to_string();
            ctx.push_toast(crate::toast::Kind::Success, format!("Opened folder: {name}"));
            println!(
                "ws: opened {} (name={name}, changed={changed}, recents={})",
                root.display(),
                ctx.recent_workspaces.len()
            );
            1
        }
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("ws: open failed: {e}");
            0
        }
    }
}

/// Re-trigger the dependent indexes after a re-root: invalidate + rebuild the
/// Quick-Open file index, re-run git status, and re-scan the Agents topology.
/// (Project Search walks the root on demand, so it picks up the new root for
/// free.) Returns the indexed file count (for the test signal).
pub(crate) fn refresh_dependents(ctx: &mut MuiContext, root: &std::path::Path) -> i32 {
    let n = ctx.quickopen.ensure_index(root, true);
    let _ = ctx.scm.refresh(root);
    // Re-scan the Agents topology against the new root (mirrors
    // `agentsabi::mui_agents_refresh`, inlined to avoid re-aliasing the borrow).
    let mut topo = std::mem::take(&mut ctx.agents);
    let _ = topo.refresh(root);
    ctx.agents = topo;
    // Reset the quick-fix lightbulb — the prior line's actions don't apply to a
    // freshly-rooted project / new active buffer.
    ctx.lightbulb.reset();
    n as i32
}

/// Persist the current recents MRU to the config dir (best-effort).
fn persist_recents(ctx: &MuiContext) {
    let _ = crate::config::save_recent_workspaces(&ctx.recent_workspaces.to_blob());
}

// ---- recent workspaces (MRU) ----

/// Number of recent workspace folders (cap 10).
#[no_mangle]
pub extern "C" fn mui_ws_recent_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.recent_workspaces.len() as i32)
}

/// Number of chars in recent folder `i`'s path, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_ws_recent_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.recent_workspaces
            .get(i as usize)
            .map_or(-1, |p| p.to_string_lossy().chars().count() as i32)
    })
}

/// The `j`th char of recent folder `i`'s path, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_ws_recent_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.recent_workspaces.get(i as usize).map_or(-1, |p| {
            p.to_string_lossy().chars().nth(j as usize).map_or(-1, |ch| ch as i32)
        })
    })
}

/// Open recent workspace `i` as the workspace. Returns `1` on success, `0` if the
/// index is out of range or the folder no longer validates.
#[no_mangle]
pub extern "C" fn mui_ws_open_recent(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        return 0;
    }
    let Some(path) = ctx.recent_workspaces.get(i as usize).cloned() else {
        return 0;
    };
    open_folder(ctx, &path.to_string_lossy())
}

// ===========================================================================
// Quick-fix lightbulb
// ===========================================================================

/// Per-frame lightbulb tick: advance the debounce for the current cursor `line`
/// and, when a probe is DUE, request code actions for that line (without opening
/// the menu) and record whether any exist. Returns `1` if the lightbulb is
/// visible after this tick (the IDE then draws it), else `0`. Called once per
/// frame before drawing.
#[no_mangle]
pub extern "C" fn mui_lightbulb_tick(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    // Screenshot hook: pin the bulb to the configured line each frame (re-seating
    // the cursor) so a headless capture shows the gutter bulb deterministically.
    if let Some(line) = ctx.lightbulb_autoopen {
        ctx.tabs.active_model_mut().move_to(line, 0);
        ctx.lightbulb.set_result(line, true);
        return 1;
    }
    let cursor_line = ctx.tabs.active_model().cursor_line() as i32;
    // Take the lightbulb out so we can borrow ctx for the (read-only) probe.
    let mut lb = std::mem::take(&mut ctx.lightbulb);
    if lb.tick(cursor_line) {
        let actions = crate::abi::compute_line_actions(ctx, cursor_line, 0);
        lb.set_result(cursor_line, !actions.is_empty());
    }
    let visible = lb.visible_for(cursor_line);
    ctx.lightbulb = lb;
    i32::from(visible)
}

/// The line the lightbulb is currently associated with (0-based), or `-1` if not
/// probed / not visible.
#[no_mangle]
pub extern "C" fn mui_lightbulb_line(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let cursor_line = ctx.tabs.active_model().cursor_line() as i32;
    if ctx.lightbulb.visible_for(cursor_line) {
        ctx.lightbulb.line()
    } else {
        -1
    }
}

/// `1` if the lightbulb should be drawn for the current cursor line, else `0`.
#[no_mangle]
pub extern "C" fn mui_lightbulb_visible(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let cursor_line = ctx.tabs.active_model().cursor_line() as i32;
    i32::from(ctx.lightbulb.visible_for(cursor_line))
}

/// Reset the lightbulb (e.g. on tab switch / file reload — the prior line's
/// actions are meaningless against a fresh buffer).
#[no_mangle]
pub extern "C" fn mui_lightbulb_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.lightbulb.reset();
    }
}

/// Draw the quick-fix lightbulb in the gutter at screen `row` (the bulb line's
/// row relative to the first visible line). No-op when the bulb isn't visible.
/// Records the drawn rect for [`mui_lightbulb_click`]. Drawn accent-tinted, just
/// left of the line text in the gutter.
#[no_mangle]
pub extern "C" fn mui_lightbulb_draw(handle: i64, row: i32, total_lines: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let cursor_line = ctx.tabs.active_model().cursor_line() as i32;
    if !ctx.lightbulb.visible_for(cursor_line) {
        ctx.lightbulb.clear_rect();
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let _ = total_lines;
    // A 15px bulb centered vertically in the row, just inside the gutter's left
    // padding. Sit it where the diagnostic dot would, so the two never collide
    // (the dot is 6px at +3; the bulb occupies the gutter's icon slot).
    let sz = 15.0_f32;
    let x = region.left + 2.0;
    let y = layout::row_y_in(region, row) + (layout::LINE_H() - sz) * 0.5;
    // A soft accent glow behind the bulb so it reads as an actionable affordance.
    ctx.dl_shadow(x + 1.0, y + 1.0, sz - 2.0, sz - 2.0, 4.0, theme::ACCENT_GLOW(), 8.0);
    ctx.dl_icon(x, y, sz, sz, crate::icons::LIGHTBULB, theme::WARNING(), 1.6, false);
    // A small accent dot accent at the base hints "click me".
    ctx.dl_round(x + sz * 0.5 - 1.0, y + sz - 2.0, 2.0, 2.0, 1.0, theme::ACCENT_BRIGHT());
    ctx.lightbulb.set_rect(x - 1.0, y - 1.0, sz + 2.0, sz + 2.0);
}

/// `1` if the last click landed on the drawn lightbulb (so the IDE should open
/// the code-action menu at the bulb's line), else `0`.
#[no_mangle]
pub extern "C" fn mui_lightbulb_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.lightbulb.hit(ctx.last_event.x, ctx.last_event.y))
}

// ===========================================================================
// Palette dispatch (one Mighty arm range for the workspace commands, L37/L38)
// ===========================================================================

/// Single shim-side dispatcher for the workspace palette commands so the Mighty
/// palette / quick-open dispatch ladders need only ONE new arm (calling this).
/// `cmd_id` is a `palette::CMD_OPEN_*` id. Returns: `1` = Open Folder dialog was
/// launched + applied; `2` = caller should open the typed-path prompt (the
/// native dialog was cancelled/unavailable — the IDE falls back to a prompt);
/// `3` = Open Recent was requested (the caller opens the recents picker / first
/// recent); `0` = not a workspace command (caller falls through).
#[no_mangle]
pub extern "C" fn mui_ws_dispatch(handle: i64, cmd_id: i32) -> i32 {
    use crate::palette;
    let id = cmd_id as u32;
    if id == palette::CMD_OPEN_FOLDER {
        // Prefer the native dialog; signal a prompt fallback when it didn't apply.
        if mui_ws_open_dialog(handle) == 1 {
            1
        } else {
            2
        }
    } else if id == palette::CMD_OPEN_RECENT {
        3
    } else {
        0
    }
}

// ===========================================================================
// Native folder picker (Windows FolderBrowserDialog via PowerShell)
// ===========================================================================

/// Show a native Windows folder picker and return the chosen absolute path, or
/// `None` if cancelled / unavailable (non-Windows, or PowerShell/WinForms
/// missing). Runs a tiny PowerShell snippet that opens a `FolderBrowserDialog`
/// and prints the selected path; we read it back off stdout.
fn pick_folder_native() -> Option<String> {
    // A folder dialog only exists on Windows; elsewhere the IDE uses the prompt.
    if !cfg!(windows) {
        return None;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.FolderBrowserDialog
$d.Description = 'Open Folder as Workspace'
$d.ShowNewFolderButton = $true
if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.SelectedPath) }
"#;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", script])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    /// `effective_root` prefers the explicit workspace, else the tree root. We
    /// can't build a full MuiContext in a unit test (it needs a GPU); the integ
    /// tests in `tests.rs` exercise the re-root end-to-end, and the pure
    /// workspace logic (set_root / is_empty) is unit-tested in `workspace.rs`.
    /// This just documents the empty-default contract the fallback relies on.
    #[test]
    fn empty_workspace_default_falls_back() {
        assert!(crate::workspace::Workspace::default().is_empty());
    }
}
