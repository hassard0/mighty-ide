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
        ctx.active_panel = panel;
        ctx.sidebar_visible = true;
        if panel == crate::PANEL_DEBUG {
            ctx.dbg.set_open(true);
        } else if panel == crate::PANEL_TEST {
            ctx.tests_panel.open();
        }
    }
    ctx.active_panel
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
    n
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
        return -1;
    }
    let (path, root) = {
        let Some(entry) = ctx.scm.get(i as usize) else {
            return -1;
        };
        let Some(root) = ctx.scm.root.clone() else {
            return -1;
        };
        (entry.path.clone(), root)
    };
    let full = root.join(&path);
    if !full.exists() {
        return -1;
    }
    let idx = ctx.tabs.open_path(full);
    crate::abi::sync_active_path(ctx);
    idx as i32
}

/// Stage/unstage the row `i` (toggles based on its current state), then refresh.
/// Returns `1` on success, `0` otherwise.
#[no_mangle]
pub extern "C" fn mui_scm_toggle_stage(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        return 0;
    }
    let dir = workspace_dir(ctx);
    if ctx.scm.toggle_stage(i as usize, &dir) {
        1
    } else {
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
    if ctx.scm.stage_all(&dir) {
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
    if ctx.scm.unstage_all(&dir) {
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

/// Commit the staged changes with the current message, then clear it + refresh.
/// Returns `1` on success, `0` on failure (nothing staged / empty msg / error).
#[no_mangle]
pub extern "C" fn mui_scm_commit(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    if ctx.scm.commit_message(&dir) {
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
/// `1` = commit, `2` = pull, `3` = push, `4` = fetch, `0` = none. Mirrors the
/// header icon geometry in `mui_scm_draw` (four 15px icons in the right of the
/// 40px header band).
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
    let hit = |cx: f32| -> bool { (x - cx).abs() <= 11.0 };
    if hit(sx + sw - 94.0 + 7.0) {
        1
    } else if hit(sx + sw - 72.0 + 7.0) {
        2
    } else if hit(sx + sw - 50.0 + 7.0) {
        3
    } else if hit(sx + sw - 28.0 + 7.0) {
        4
    } else {
        0
    }
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
    let first_action_x = sx + sw - 94.0;
    let max_px = (first_action_x - 8.0 - label_x).max(0.0);
    let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
    fit_head_px(text, &tracked, max_px, size)
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
    let title = "SOURCE CONTROL";
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
    ctx.dl_icon(sx + sw - 94.0, act_y, 15.0, 15.0, icons::CHECK, theme::GREEN(), 1.8, false);
    ctx.dl_icon(sx + sw - 72.0, act_y, 15.0, 15.0, icons::ARROW_DOWN, theme::TEXT_3(), 1.7, false);
    ctx.dl_icon(sx + sw - 50.0, act_y, 15.0, 15.0, icons::ARROW_UP, theme::TEXT_3(), 1.7, false);
    ctx.dl_icon(sx + sw - 28.0, act_y, 15.0, 15.0, icons::REFRESH, theme::TEXT_3(), 1.5, false);

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
    let shown = fit_tail_px(&mut ctx.text, &msg_text, sw - 36.0, chrome);
    ctx.text.queue_ui_sized(sx + 18.0, box_y + (box_h - chrome) * 0.5 - 1.0, &shown, msg_col, chrome, clip);

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
        let hint = fit_tail_px(&mut ctx.text, "Use the refresh action to scan this workspace for Git status.", sw - 28.0, chrome - 1.0);
        ctx.text.queue_ui_sized(sx + 14.0, top + 25.0, &hint, theme::TEXT_3(), chrome - 1.0, clip);
        return;
    }
    if count == 0 {
        let top = scm_rows_top();
        ctx.text.queue_ui_sized(sx + 14.0, top + 4.0, "Working tree clean", theme::TEXT_1(), chrome, clip);
        let hint = fit_tail_px(&mut ctx.text, "Pull, fetch, or edit a file to start a change.", sw - 28.0, chrome - 1.0);
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

fn branch_picker_geometry(width: u32, height: u32, rows: usize) -> (f32, f32, f32, f32, f32, f32) {
    let w = width as f32;
    let h = height as f32;
    let row_h = 34.0_f32;
    let head_h = 50.0_f32;
    let box_w = 460.0_f32.min(w - 80.0);
    let box_h = head_h + rows as f32 * row_h + 16.0;
    let box_x = ((w - box_w) * 0.5).max(0.0);
    let box_y = 100.0_f32.min((h - box_h).max(0.0));
    let list_top = box_y + head_h + 6.0;
    (box_x, box_y, box_w, box_h, list_top, row_h)
}

fn branch_picker_close_rect(width: u32, height: u32, rows: usize) -> (f32, f32, f32, f32) {
    let (box_x, box_y, box_w, _box_h, _list_top, _row_h) =
        branch_picker_geometry(width, height, rows);
    (box_x + box_w - 38.0, box_y + 13.0, 24.0, 24.0)
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
    let rows = if ctx.branch_picker.is_creating() {
        1
    } else {
        ctx.branch_picker.count().min(10)
    };
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
            ctx.push_toast(crate::toast::Kind::Error, format!("Git error: {}", res.message));
            0
        }
    } else {
        0
    }
}

/// Close the branch switcher without acting.
#[no_mangle]
pub extern "C" fn mui_branch_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.branch_picker.cancel();
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
    let rows = if creating { 1 } else { p.count().min(10) };
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
    let (qtext, qcol) = if q.is_empty() {
        let ph = if creating { "New branch name\u{2026}" } else { "Filter branches\u{2026}" };
        (ph.to_string(), theme::TEXT_3())
    } else {
        (q.clone(), theme::TEXT())
    };
    ctx.text.queue_ui_sized(box_x + 40.0, box_y + 26.0, &qtext, qcol, chrome, clip);
    let qadv = chrome * 0.52;
    let caret_x = box_x + 40.0 + q.chars().count() as f32 * qadv + 1.0;
    ctx.dl_round(caret_x, box_y + 25.0, 2.0, 15.0, 1.0, theme::ACCENT_BRIGHT());
    let (cx, cy, cw, ch) = branch_picker_close_rect(width, height, rows);
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
            let mut nm = e.name.clone();
            let avail = ((box_w - 110.0) / qadv).floor() as usize;
            if nm.chars().count() > avail && avail > 4 {
                nm = nm.chars().take(avail - 1).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(box_x + 42.0, ry + (row_h - chrome) * 0.5 - 1.0, &nm, name_col, chrome, clip);
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

/// Run the project-wide search over the workspace root. Returns total matches.
#[no_mangle]
pub extern "C" fn mui_search_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let dir = workspace_dir(ctx);
    let n = ctx.search.run(&dir);
    println!(
        "search: query=\"{}\" files={} matches={}",
        ctx.search.query_string(),
        ctx.search.file_count(),
        n
    );
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
    let dir = workspace_dir(ctx);
    let (n, changed_paths) = ctx.search.replace_all_with_changed_paths(&dir);
    let (refreshed, dirty_skipped) = refresh_replaced_open_tabs(ctx, &changed_paths);
    println!("search: replaced {n}");
    if n > 0 {
        let suffix = if n == 1 { "" } else { "s" };
        if dirty_skipped > 0 {
            let tab_suffix = if dirty_skipped == 1 { "" } else { "s" };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!(
                    "Replaced {n} occurrence{suffix}; {dirty_skipped} dirty open tab{tab_suffix} not refreshed"
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
        match ctx.tabs.reload_clean_path(path, &bytes) {
            Some(true) => refreshed += 1,
            Some(false) => dirty_skipped += 1,
            None => {}
        }
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
        return -1;
    }
    let (path, line, col) = {
        let Some(m) = ctx.search.match_at(i as usize) else {
            return -1;
        };
        let Some(f) = ctx.search.file_at(m.file) else {
            return -1;
        };
        (f.path.clone(), m.line, m.col)
    };
    if !path.exists() {
        return -1;
    }
    let idx = ctx.tabs.open_path(path);
    crate::abi::sync_active_path(ctx);
    let model = ctx.tabs.active_model_mut();
    model.move_to(line, col);
    let first = (line - 2).max(0);
    model.set_first_visible(first as usize);
    idx as i32
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
    (sx + sw - 40.0, sx + sw - 14.0)
}

/// Search-panel mouse action for the last click:
/// `0` = no action, `1` = run search, `2` = replace all.
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
    let box_x1 = sx + sw - 10.0;
    let (btn_x0, btn_x1) = search_field_button_x(sx, sw);
    if (box_x0..=box_x1).contains(&x) && (qy..=qy + box_h).contains(&y) {
        ctx.search.replace_focus = false;
        if (btn_x0..=btn_x1).contains(&x) {
            return 1;
        }
        return 0;
    }
    if (box_x0..=box_x1).contains(&x) && (ry..=ry + box_h).contains(&y) {
        ctx.search.replace_focus = true;
        if (btn_x0..=btn_x1).contains(&x) {
            return 2;
        }
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

/// Show the rightmost `avail` chars of `s` (used to keep the tail / filename
/// visible when a path or query is too long for the field).
fn tail(s: &str, avail: usize) -> String {
    if s.chars().count() <= avail || avail <= 1 {
        return s.to_string();
    }
    s.chars()
        .rev()
        .take(avail - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn truncate_head(s: &str, avail: usize) -> String {
    if s.chars().count() <= avail || avail <= 1 {
        return s.to_string();
    }
    s.chars().take(avail - 1).collect::<String>() + "\u{2026}"
}

#[cfg(test)]
mod search_panel_tests {
    use super::{tail, truncate_head};

    #[test]
    fn search_preview_truncates_from_head_so_match_columns_stay_stable() {
        assert_eq!(truncate_head("abcdef", 4), "abc\u{2026}");
        assert_eq!(truncate_head("abcdef", 1), "abcdef");
        assert_eq!(truncate_head("abc", 4), "abc");
    }

    #[test]
    fn search_inputs_keep_tail_when_fields_overflow() {
        assert_eq!(tail("abcdef", 4), "def");
        assert_eq!(tail("abcdef", 1), "abcdef");
        assert_eq!(tail("abc", 4), "abc");
    }
}

#[cfg(test)]
mod branch_picker_surface_tests {
    use super::{branch_picker_close_rect, branch_picker_geometry};

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
    let adv = chrome * 0.55;
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
    ctx.dl_icon(sx + sw - 28.0, (head_h - 15.0) * 0.5, 15.0, 15.0, icons::REFRESH, theme::TEXT_3(), 1.5, false);

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
    let qavail = ((btn_x0 - (sx + 38.0)) / adv).floor() as usize;
    let qshown = tail(&q_text, qavail);
    ctx.text.queue_ui_sized(sx + 34.0, qy + (box_h - chrome) * 0.5 - 1.0, &qshown, q_col, chrome, clip);
    ctx.dl_round(btn_x0, qy + 4.0, btn_x1 - btn_x0, box_h - 8.0, 5.0, theme::BG_4());
    ctx.dl_icon(btn_x0 + 6.0, qy + 8.0, 14.0, 14.0, icons::REFRESH, theme::TEXT_1(), 1.4, false);

    // replace box
    let r_border = if replace_focus { theme::ACCENT_LINE() } else { theme::BORDER_STRONG() };
    ctx.dl_round(sx + 10.0, ry, sw - 20.0, box_h, 7.0, theme::BG_1());
    ctx.dl_stroke(sx + 10.0, ry, sw - 20.0, box_h, 7.0, r_border, 1.0);
    ctx.dl_icon(sx + 16.0, ry + (box_h - 13.0) * 0.5, 13.0, 13.0, icons::REPLACE, theme::TEXT_3(), 1.5, false);
    let (r_text, r_col) = if replace.is_empty() {
        ("Replace".to_string(), theme::TEXT_3())
    } else {
        (replace.clone(), theme::TEXT())
    };
    let rshown = tail(&r_text, qavail);
    ctx.text.queue_ui_sized(sx + 34.0, ry + (box_h - chrome) * 0.5 - 1.0, &rshown, r_col, chrome, clip);
    ctx.dl_round(btn_x0, ry + 4.0, btn_x1 - btn_x0, box_h - 8.0, 5.0, theme::BG_4());
    ctx.dl_icon(btn_x0 + 6.0, ry + 8.0, 14.0, 14.0, icons::CHECK, theme::TEXT_1(), 1.7, false);

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
        let ravail = (((sx + sw - 40.0) - (sx + 46.0)) / adv).floor() as usize;
        let rshown = tail(&rel, ravail);
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
            let preview_x = sx + 30.0 + (ln.chars().count() as f32) * adv + 8.0;
            let rel_col = col - trimmed_off;
            let pavail = (((sx + sw - 14.0) - preview_x) / adv).floor() as usize;
            let pv = truncate_head(trimmed, pavail);
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

/// `1` if the AI panel is currently open, else `0`.
#[no_mangle]
pub extern "C" fn mui_ai_is_open(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.ai.open { 1 } else { 0 })
}

/// Map the last click to the right-docked AI panel:
/// `0` = miss, `1` = input/body focus, `2` = send button, `3` = close button.
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
    let (px, pw, input_y, input_h) =
        crate::ai::input_geometry(&ctx.ai.input, visible_w, ctx.gpu.height);
    if x < px || x > px + pw || y < layout::TAB_BAR_H || y > ctx.gpu.height as f32 {
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
        return 3;
    }
    let send_x0 = px + pw - 44.0;
    let send_x1 = px + pw - 12.0;
    let send_y0 = input_y + input_h - 36.0;
    let send_y1 = input_y + input_h - 4.0;
    if (send_x0..=send_x1).contains(&x) && (send_y0..=send_y1).contains(&y) {
        return 2;
    }
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

/// Send the current input as a new turn, embedding the active file's content
/// (and any selection) as context. Spawns the background streaming request.
/// Returns `1` if a request was started, `0` otherwise (blank input / already
/// streaming / no key).
#[no_mangle]
pub extern "C" fn mui_ai_send(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let file_name = ctx.file_name.clone();
    let content = ctx.tabs.active_model().as_text();
    let selection = ctx.tabs.active_model().selected_text();
    let system = crate::ai::build_system_prompt(&file_name, &content, &selection);
    if ctx.ai.send(system) {
        1
    } else {
        0
    }
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
    let file_name = ctx.file_name.clone();
    let content = ctx.tabs.active_model().as_text();
    let selection = ctx.tabs.active_model().selected_text();
    let system = crate::ai::build_system_prompt(&file_name, &content, &selection);
    if ctx.ai.send(system) {
        1
    } else {
        0
    }
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
    // Render on the overlay layer so the chat card occludes editor glyphs that
    // sit underneath the right-docked panel band.
    let panel = std::mem::take(&mut ctx.ai);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    panel.draw(ctx, visible_w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.ai = panel;
}
