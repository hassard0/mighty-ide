//! Scalar C ABI for the three code-navigation surfaces (Outline, Problems,
//! interactive breadcrumb). The state + draw + parsing live in
//! [`crate::outline`], [`crate::problems`], and [`crate::crumbmenu`]; this module
//! is the flat `mui_*` veneer Mighty drives (L17).

use std::path::PathBuf;

use crate::crumbmenu::{CrumbLayout, MenuItem, MenuKind, Segment};
use crate::layout;
use crate::MuiContext;

#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

// ===========================================================================
// Feature 1 — Outline / document symbols
// ===========================================================================

/// Re-scan the active document's symbols. Tries LSP `documentSymbol` first (when
/// the server implements it), else the shim-side scanner. Returns the symbol
/// count. The IDE calls this on open/save/tab-switch.
#[no_mangle]
pub extern "C" fn mui_outline_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let source = ctx.tabs.active_model().as_text();
    // Try the LSP path when we have a real on-disk path; mty-lsp v0.5 returns
    // -32601 so this falls through to the scanner (recorded by used_lsp()).
    let lsp_json = if let Some(path) = ctx.tabs.active_path() {
        crate::language::lsp::request(
            &path,
            &source,
            crate::language::lsp::Req::DocumentSymbol,
        )
    } else {
        String::new()
    };
    let n = ctx.outline.refresh(&source, &lsp_json);
    // Track the symbol under the cursor immediately.
    let line = ctx.status_cursor.0.max(1) - 1;
    let _ = ctx.outline.set_cursor(line as u32);
    println!(
        "outline: {n} symbols ({})",
        if ctx.outline.used_lsp() { "lsp documentSymbol" } else { "shim scanner" }
    );
    n as i32
}

/// Number of outline symbols.
#[no_mangle]
pub extern "C" fn mui_outline_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.outline.count() as i32)
}

/// Scalar kind of symbol `i` (see [`SymKind`]), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_outline_row_kind(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.outline.get(i as usize).map_or(-1, |s| s.kind as i32))
}

/// Number of chars in symbol `i`'s name (for sizing), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_outline_row_name_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.outline.get(i as usize).map_or(-1, |s| s.name.chars().count() as i32)
    })
}

/// The `j`th char of symbol `i`'s name as a codepoint, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_outline_row_name_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.outline
            .get(i as usize)
            .and_then(|s| s.name.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

/// Nesting depth of symbol `i` (0 = top level), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_outline_row_depth(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.outline.get(i as usize).map_or(-1, |s| s.depth as i32))
}

/// 0-based declaration line of symbol `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_outline_row_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.outline.line_of(i as usize))
}

/// Jump the editor to symbol `i`'s declaration line (cursor to line start,
/// scrolled near the top). Returns the line jumped to, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_outline_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        return -1;
    }
    let line = ctx.outline.line_of(i as usize);
    if line < 0 {
        return -1;
    }
    let model = ctx.tabs.active_model_mut();
    model.move_to(line, 0);
    let first = (line - 2).max(0);
    model.set_first_visible(first as usize);
    let _ = ctx.outline.set_cursor(line as u32);
    line
}

/// Update the cursor-tracked current symbol from a 0-based `line`. Returns the
/// current symbol index, or `-1` if none. The IDE feeds the cursor line.
#[no_mangle]
pub extern "C" fn mui_outline_set_cursor(handle: i64, line: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    ctx.outline.set_cursor(line.max(0) as u32)
}

/// The index of the symbol the cursor is currently inside, or `-1`.
#[no_mangle]
pub extern "C" fn mui_outline_current(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.outline.current())
}

/// Map the last click's pixel y to an outline row index, or `-1` if not on a row
/// / wrong panel. Mirrors the row geometry in [`outline::OutlineState::draw`].
#[no_mangle]
pub extern "C" fn mui_outline_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let sx0 = layout::RAIL_W;
    let sx1 = layout::RAIL_W + layout::SIDEBAR_W;
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_OUTLINE {
        return -1;
    }
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if x < sx0 || x > sx1 {
        return -1;
    }
    let top = 40.0 + 6.0; // header band + pad (mirror draw)
    if y < top {
        return -1;
    }
    let i = ((y - top) / layout::LINE_H()).floor() as i32;
    if i >= 0 && (i as usize) < ctx.outline.count() {
        i
    } else {
        -1
    }
}

/// Draw the Outline panel (no-op unless the sidebar is shown + this panel active).
#[no_mangle]
pub extern "C" fn mui_outline_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_OUTLINE {
        return;
    }
    let panel = std::mem::take(&mut ctx.outline);
    panel.draw(ctx);
    ctx.outline = panel;
}

// ===========================================================================
// Feature 2 — Problems panel
// ===========================================================================

/// Open paths to check: the active file first, then every other open `.mty` tab.
fn problem_paths(ctx: &MuiContext) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = ctx.tabs.active_path() {
        paths.push(p);
    }
    for i in 0..ctx.tabs.count() {
        if let Some(p) = ctx.tabs.path(i) {
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    paths
}

/// Re-run `mty check` across the open tabs and aggregate. Returns the total
/// problem count. The IDE calls this on save + when diagnostics update.
#[no_mangle]
pub extern "C" fn mui_problems_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let paths = problem_paths(ctx);
    let n = ctx.problems.refresh(&paths);
    println!(
        "problems: {n} ({} errors, {} warnings) across {} files",
        ctx.problems.error_count(),
        ctx.problems.warn_count(),
        ctx.problems.file_count()
    );
    n as i32
}

/// Toggle the Problems panel open/closed. Opening it also closes the Run panel
/// (they share the bottom band). Returns `1` if now open, else `0`.
#[no_mangle]
pub extern "C" fn mui_problems_toggle(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let open = ctx.problems.toggle();
    if open {
        ctx.run.close();
    }
    if open {
        1
    } else {
        0
    }
}

/// Open (show) the Problems panel — wired to the status-bar problems chip click.
/// Refreshes first so the list is current. Returns `1`.
#[no_mangle]
pub extern "C" fn mui_problems_open(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let paths = problem_paths(ctx);
    let _ = ctx.problems.refresh(&paths);
    ctx.problems.set_open(true);
    ctx.run.close();
    1
}

/// `1` if the Problems panel is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_problems_is_open(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.problems.is_open() { 1 } else { 0 })
}

/// Total problem count.
#[no_mangle]
pub extern "C" fn mui_problems_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.problems.count() as i32)
}

/// Error count.
#[no_mangle]
pub extern "C" fn mui_problems_error_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.problems.error_count())
}

/// Warning count.
#[no_mangle]
pub extern "C" fn mui_problems_warn_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.problems.warn_count())
}

/// Severity of problem `i` (0 = error, 1 = warning), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_problems_row_severity(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.problems.get(i as usize).map_or(-1, |p| p.severity as i32))
}

/// 0-based line of problem `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_problems_row_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.problems.get(i as usize).map_or(-1, |p| p.line))
}

/// 0-based column of problem `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_problems_row_col(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.problems.get(i as usize).map_or(-1, |p| p.col))
}

/// Scroll the Problems list by `delta` rows.
#[no_mangle]
pub extern "C" fn mui_problems_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.problems.scroll_by(delta);
    }
}

/// Map the last click to a Problems row index, or `-1` (header / outside).
#[no_mangle]
pub extern "C" fn mui_problems_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    ctx.problems.row_at(ctx.last_event.x, ctx.last_event.y, w, h, left)
}

/// Open the file of problem `i` as a tab and jump to its line:col. Returns the
/// resulting tab index, or `-1` out of range / missing file.
#[no_mangle]
pub extern "C" fn mui_problems_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        return -1;
    }
    let (path, line, col) = {
        let Some(p) = ctx.problems.get(i as usize) else {
            return -1;
        };
        (p.path.clone(), p.line, p.col)
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

/// Draw the Problems panel (no-op when closed).
#[no_mangle]
pub extern "C" fn mui_problems_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.problems.is_open() {
        return;
    }
    let left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let panel = std::mem::take(&mut ctx.problems);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    panel.draw(ctx, left);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.problems = panel;
}

// ===========================================================================
// Feature 3 — interactive breadcrumb
// ===========================================================================

/// Build the [`CrumbLayout`] that reproduces the breadcrumb's segment x-math.
fn crumb_layout(ctx: &MuiContext) -> CrumbLayout {
    let left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let chrome = crate::theme::CHROME_FONT_SIZE;
    let folder = ctx
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
    let symbol = current_symbol_name(ctx);
    CrumbLayout {
        left,
        advance: chrome * 0.54,
        folder_chars: folder.chars().count(),
        file_chars: file.chars().count(),
        symbol_chars: symbol.chars().count(),
    }
}

/// The display name of the symbol under the cursor (else `main` as a default,
/// matching the prior static breadcrumb).
fn current_symbol_name(ctx: &MuiContext) -> String {
    let cur = ctx.outline.current();
    if cur >= 0 {
        if let Some(s) = ctx.outline.get(cur as usize) {
            return s.name.clone();
        }
    }
    "main".to_string()
}

/// The folder that holds the active file (for the file dropdown).
fn active_file_dir(ctx: &MuiContext) -> Option<PathBuf> {
    let p = ctx.tabs.active_path()?;
    p.parent().map(|d| d.to_path_buf())
}

/// List the source files in `dir` (`.mty` + common text exts), sorted, as
/// `(name, full_path)`.
fn list_dir_files(dir: &std::path::Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_file() {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "mty" | "toml" | "md" | "txt" | "rs" | "json") {
                if let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) {
                    out.push((name, p));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Pure hit-test: which breadcrumb segment the last click landed on, gated to
/// the breadcrumb y-band, as a scalar (0 = folder, 1 = file, 2 = symbol, -1 =
/// none). Lets Mighty decide whether to open a dropdown without side effects.
/// (Only file/symbol are clickable; folder returns -1.)
#[no_mangle]
pub extern "C" fn mui_breadcrumb_click_row(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let y = ctx.last_event.y;
    let top = layout::TAB_BAR_H;
    let bot = top + layout::BREADCRUMB_H;
    if y < top || y > bot {
        return -1;
    }
    let lay = crumb_layout(ctx);
    match lay.hit(ctx.last_event.x) {
        Segment::File => 1,
        Segment::Symbol => 2,
        _ => -1,
    }
}

/// Handle a click on the breadcrumb at the last event x: open the file dropdown
/// (file segment) or the symbol dropdown (symbol segment). Returns the segment
/// hit as a scalar (0 = folder, 1 = file, 2 = symbol, -1 = none/no menu opened).
#[no_mangle]
pub extern "C" fn mui_breadcrumb_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let cx = ctx.last_event.x;
    let lay = crumb_layout(ctx);
    let seg = lay.hit(cx);
    match seg {
        Segment::File => {
            // Folder files dropdown.
            let dir = active_file_dir(ctx);
            let files = dir.as_ref().map(|d| list_dir_files(d)).unwrap_or_default();
            let active = ctx.tabs.active_path();
            let items: Vec<MenuItem> = files
                .iter()
                .enumerate()
                .map(|(i, (name, full))| {
                    let (icon, color) = crate::abi::file_icon_for(name, Some(full) == active.as_ref());
                    MenuItem {
                        label: name.clone(),
                        icon: Some(icon),
                        icon_color: color,
                        depth: 0,
                        target: i as i32,
                    }
                })
                .collect();
            // Stash the file paths so open-by-index can resolve them.
            ctx.crumb_files = files.into_iter().map(|(_, p)| p).collect();
            let anchor = lay.anchor_x(Segment::File);
            let n = ctx.crumb_menu.open(MenuKind::Files, items, anchor);
            if n > 0 {
                1
            } else {
                -1
            }
        }
        Segment::Symbol => {
            let items: Vec<MenuItem> = ctx
                .outline
                .symbols()
                .iter()
                .enumerate()
                .map(|(i, s)| MenuItem {
                    label: s.name.clone(),
                    icon: Some(s.kind.icon()),
                    icon_color: s.kind.color(),
                    depth: s.depth,
                    target: i as i32,
                })
                .collect();
            let anchor = lay.anchor_x(Segment::Symbol);
            let n = ctx.crumb_menu.open(MenuKind::Symbols, items, anchor);
            if n > 0 {
                2
            } else {
                -1
            }
        }
        Segment::Folder | Segment::None => -1,
    }
}

/// `1` if the breadcrumb dropdown is active, else `0`.
#[no_mangle]
pub extern "C" fn mui_crumb_menu_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.crumb_menu.is_active() { 1 } else { 0 })
}

/// Move the dropdown selection by `delta` (wraps).
#[no_mangle]
pub extern "C" fn mui_crumb_menu_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.crumb_menu.move_sel(delta);
    }
}

/// Cancel / close the dropdown.
#[no_mangle]
pub extern "C" fn mui_crumb_menu_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.crumb_menu.cancel();
    }
}

/// Map the last click's y to a dropdown row index, or `-1` if outside.
#[no_mangle]
pub extern "C" fn mui_crumb_menu_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    ctx.crumb_menu.row_at(ctx.last_event.y)
}

/// Accept dropdown row `i` (`-1` = the current selection): jump to the chosen
/// file (opening it as a tab) or symbol line. Closes the menu. Returns the
/// resulting tab index (Files) or jumped line (Symbols), or `-1`.
#[no_mangle]
pub extern "C" fn mui_crumb_menu_accept(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.crumb_menu.is_active() {
        return -1;
    }
    let target = if i < 0 {
        ctx.crumb_menu.selected_target()
    } else {
        ctx.crumb_menu.target_at(i as usize)
    };
    let kind = ctx.crumb_menu.kind();
    ctx.crumb_menu.cancel();
    if target < 0 {
        return -1;
    }
    match kind {
        MenuKind::Files => {
            let Some(path) = ctx.crumb_files.get(target as usize).cloned() else {
                return -1;
            };
            if !path.exists() {
                return -1;
            }
            let idx = ctx.tabs.open_path(path);
            crate::abi::sync_active_path(ctx);
            idx as i32
        }
        MenuKind::Symbols => {
            let line = ctx.outline.line_of(target as usize);
            if line < 0 {
                return -1;
            }
            let model = ctx.tabs.active_model_mut();
            model.move_to(line, 0);
            let first = (line - 2).max(0);
            model.set_first_visible(first as usize);
            let _ = ctx.outline.set_cursor(line as u32);
            line
        }
        MenuKind::None => -1,
    }
}

/// Draw the breadcrumb dropdown (no-op when inactive). Drawn over the body.
#[no_mangle]
pub extern "C" fn mui_crumb_menu_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.crumb_menu.is_active() {
        return;
    }
    let menu = std::mem::take(&mut ctx.crumb_menu);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    menu.draw(ctx);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.crumb_menu = menu;
}

// kind constants exposed for tests / Mighty mirror.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::SymKind;

    #[test]
    fn sym_kind_scalars_match_abi() {
        assert_eq!(SymKind::Function as i32, 0);
        assert_eq!(SymKind::Struct as i32, 1);
        assert_eq!(SymKind::Enum as i32, 2);
        assert_eq!(SymKind::Agent as i32, 3);
        assert_eq!(SymKind::Protocol as i32, 4);
        assert_eq!(SymKind::TypeAlias as i32, 5);
    }

    #[test]
    fn list_dir_files_filters_and_sorts() {
        let dir = std::env::temp_dir().join("mui_crumb_test");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("b.mty"), "fn b() {}");
        let _ = std::fs::write(dir.join("a.mty"), "fn a() {}");
        let _ = std::fs::write(dir.join("note.png"), "x"); // filtered out
        let files = list_dir_files(&dir);
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"a.mty"));
        assert!(names.contains(&"b.mty"));
        assert!(!names.contains(&"note.png"));
        // sorted: a before b
        let ai = names.iter().position(|n| *n == "a.mty").unwrap();
        let bi = names.iter().position(|n| *n == "b.mty").unwrap();
        assert!(ai < bi);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
