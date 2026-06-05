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
use std::sync::{Mutex, OnceLock};

use crate::diagnostics::{self, Severity};
use crate::ffi::*;
use crate::langdetect::Language;
use crate::layout;
use crate::theme;
use crate::MuiContext;

fn record_recent_file(ctx: &mut MuiContext, path: PathBuf) {
    ctx.quickopen.record_mru(path);
    persist_recent_files(ctx);
}

pub(crate) fn record_opened_file(ctx: &mut MuiContext, path: &std::path::Path) {
    record_recent_file(ctx, path.to_path_buf());
    refresh_workspace_file_views(ctx);
}

fn remove_recent_file(ctx: &mut MuiContext, path: &std::path::Path) {
    if ctx.quickopen.remove_recent_path(path) {
        persist_recent_files(ctx);
    }
}

pub(crate) fn refresh_workspace_file_views(ctx: &mut MuiContext) {
    ctx.tree.refresh();
    let root = quickopen_root(ctx);
    let _ = ctx.quickopen.ensure_index(&root, true);
    prune_missing_recent_files(ctx);
    ctx.quickopen.refresh_file_rows();
}

fn persist_recent_files(ctx: &MuiContext) {
    let _ = crate::config::save_recent_files(&ctx.quickopen.recent_blob());
}

/// Highlight one line for the active `lang`, preferring Markdown's tailored
/// handling (headings/bullets/quotes) when the file is Markdown.
pub(crate) fn highlight_for(line: &str, lang: Language) -> Vec<crate::syntax::Span> {
    if lang == Language::Markdown {
        crate::syntax::highlight_markdown_line(line)
    } else {
        crate::syntax::highlight_line_lang(line, lang)
    }
}

// ---------------------------------------------------------------------------
// LSP routing: Mighty keeps its dedicated `mty lsp` clients; every other
// language routes through the generic `lspclient` against a registry-resolved
// server (only when the binary is installed; otherwise silently no LSP).
// ---------------------------------------------------------------------------

/// The workspace root for an LSP `initialize` (the file's parent dir, else cwd).
fn workspace_root(path: &std::path::Path) -> PathBuf {
    path.parent()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Semantic completion labels for the active `lang`. Mighty → the existing
/// `mty lsp` completion client (unchanged). Other languages → the generic
/// client against the registry server, parsed for `label`s; empty (→ buffer
/// words fallback) when no server is installed.
fn lsp_semantic_labels(lang: Language, path: &std::path::Path, source: &str, line: u32, col: u32) -> Vec<String> {
    if lang == Language::Mighty {
        return crate::completion::lsp::semantic_labels(path, source, line, col);
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return Vec::new();
    };
    let root = workspace_root(path);
    let raw = crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::Completion,
        line,
        col,
    );
    crate::completion::lsp::scrape_labels(&raw)
}

/// Raw `textDocument/hover` response for the active `lang` (isolated id:2
/// object). Mighty → `nav::lsp`; others → generic client; empty when no server.
fn lsp_hover_raw(lang: Language, path: &std::path::Path, source: &str, line: u32, col: u32) -> String {
    if lang == Language::Mighty {
        return crate::nav::lsp::request(path, source, line, col, crate::nav::lsp::Req::Hover);
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return String::new();
    };
    let root = workspace_root(path);
    crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::Hover,
        line,
        col,
    )
}

/// Raw `textDocument/definition` response for the active `lang`. Mighty →
/// `nav::lsp`; others → generic client; empty when no server.
pub(crate) fn lsp_def_raw(lang: Language, path: &std::path::Path, source: &str, line: u32, col: u32) -> String {
    if lang == Language::Mighty {
        return crate::nav::lsp::request(path, source, line, col, crate::nav::lsp::Req::Definition);
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return String::new();
    };
    let root = workspace_root(path);
    crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::Definition,
        line,
        col,
    )
}

fn lsp_signature_raw(
    lang: Language,
    path: &std::path::Path,
    source: &str,
    line: u32,
    col: u32,
) -> String {
    if lang == Language::Mighty {
        return crate::language::lsp::request(
            path,
            source,
            crate::language::lsp::Req::SignatureHelp { line, col },
        );
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return String::new();
    };
    let root = workspace_root(path);
    crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::SignatureHelp,
        line,
        col,
    )
}

fn lsp_prepare_rename_raw(
    lang: Language,
    path: &std::path::Path,
    source: &str,
    line: u32,
    col: u32,
) -> String {
    if lang == Language::Mighty {
        return crate::language::lsp::request(
            path,
            source,
            crate::language::lsp::Req::PrepareRename { line, col },
        );
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return String::new();
    };
    let root = workspace_root(path);
    crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::PrepareRename,
        line,
        col,
    )
}

fn lsp_rename_raw(
    lang: Language,
    path: &std::path::Path,
    source: &str,
    line: u32,
    col: u32,
    new_name: String,
) -> String {
    if lang == Language::Mighty {
        return crate::language::lsp::request(
            path,
            source,
            crate::language::lsp::Req::Rename {
                line,
                col,
                new_name,
            },
        );
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return String::new();
    };
    let root = workspace_root(path);
    crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::Rename { new_name },
        line,
        col,
    )
}

fn lsp_execute_command_raw(
    lang: Language,
    path: &std::path::Path,
    source: &str,
    command: &crate::language::CommandAction,
) -> String {
    if lang == Language::Mighty {
        return String::new();
    }
    let Some(spec) = crate::lspregistry::server_for(lang) else {
        return String::new();
    };
    let root = workspace_root(path);
    crate::lspclient::request(
        &spec,
        lang.lsp_id(),
        &root,
        path,
        source,
        crate::lspclient::Method::ExecuteCommand {
            command: command.command.clone(),
            arguments_json: command.arguments_json.clone(),
        },
        0,
        0,
    )
}

fn code_action_diagnostics_json(diags: &[diagnostics::Diag], line: u32) -> String {
    code_action_diagnostics_json_with_mapper(diags, line, |_, col| col)
}

fn code_action_diagnostics_json_lsp_utf16(
    source: &str,
    diags: &[diagnostics::Diag],
    line: u32,
) -> String {
    code_action_diagnostics_json_with_mapper(diags, line, |line, col| {
        source_char_col_to_utf16(source, line, col)
    })
}

fn code_action_diagnostics_json_with_mapper(
    diags: &[diagnostics::Diag],
    line: u32,
    mut map_col: impl FnMut(u32, u32) -> u32,
) -> String {
    let mut out = String::from("[");
    let mut first = true;
    for d in diags.iter().filter(|d| d.line.max(0) as u32 == line) {
        if !first {
            out.push(',');
        }
        first = false;
        let line = d.line.max(0) as u32;
        let start = d.col_start.max(0) as u32;
        let mut end = d.col_end.max(d.col_start + 1).max(0) as u32;
        if end <= start {
            end = start + 1;
        }
        let start = map_col(line, start);
        let mut end = map_col(line, end);
        if end <= start {
            end = start + 1;
        }
        let severity = match d.severity {
            Severity::Error => 1,
            Severity::Warning => 2,
        };
        out.push_str(&format!(
            r#"{{"range":{{"start":{{"line":{line},"character":{start}}},"end":{{"line":{line},"character":{end}}}}},"severity":{severity},"code":"{}","source":"mighty-ide","message":"{}"}}"#,
            crate::lspclient::json_escape(&d.code),
            crate::lspclient::json_escape(&d.message)
        ));
    }
    out.push(']');
    out
}

fn source_char_col_to_utf16(source: &str, line: u32, char_col: u32) -> u32 {
    source
        .split('\n')
        .nth(line as usize)
        .map(|line_text| {
            line_text
                .chars()
                .take(char_col as usize)
                .map(|ch| ch.len_utf16() as u32)
                .sum()
        })
        .unwrap_or(0)
}

fn source_utf16_col_to_char(source: &str, line: u32, utf16_col: u32) -> u32 {
    let Some(line_text) = source.split('\n').nth(line as usize) else {
        return 0;
    };
    let mut units = 0u32;
    let mut chars = 0u32;
    for ch in line_text.chars() {
        if units >= utf16_col {
            return chars;
        }
        let next = units + ch.len_utf16() as u32;
        if next > utf16_col {
            return chars;
        }
        units = next;
        chars += 1;
    }
    chars
}

pub(crate) fn definition_target_from_lsp(
    lang: Language,
    current_path: &std::path::Path,
    current_source: &str,
    uri: &str,
    line: u32,
    col: u32,
) -> Option<crate::nav::DefTarget> {
    let path = crate::nav::uri_to_path(uri)?;
    let col = if lang == Language::Mighty {
        col
    } else if crate::nav::paths_equal(&path, current_path) {
        source_utf16_col_to_char(current_source, line, col)
    } else {
        std::fs::read_to_string(&path)
            .map(|target_source| source_utf16_col_to_char(&target_source, line, col))
            .unwrap_or(col)
    };
    Some(crate::nav::DefTarget { path, line, col })
}

pub(crate) fn definition_not_found_message(path: &std::path::Path, line: i32, col: i32) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    format!("No definition found at {name}:{}:{}", line.max(0) + 1, col.max(0) + 1)
}

fn completion_not_found_message(ctx: &MuiContext) -> String {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string());
    let model = ctx.tabs.active_model();
    format!(
        "No completions available at {name}:{}:{}",
        model.cursor_line() + 1,
        model.cursor_col() + 1
    )
}

fn codeaction_not_found_message(ctx: &MuiContext, line: i32, col: i32) -> String {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string());
    format!(
        "No code actions available at {name}:{}:{}",
        line.max(0) + 1,
        col.max(0) + 1
    )
}

fn hover_not_found_message(path: &std::path::Path, line: i32, col: i32) -> String {
    let name = basename(path);
    format!("No hover information at {name}:{}:{}", line.max(0) + 1, col.max(0) + 1)
}

fn signature_not_found_message(path: &std::path::Path, line: i32, col: i32) -> String {
    let name = basename(path);
    format!(
        "No signature help available at {name}:{}:{}",
        line.max(0) + 1,
        col.max(0) + 1
    )
}

pub(crate) fn language_needs_file_message(ctx: &MuiContext, action: &str) -> String {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string());
    format!("Save {name} before {action}")
}

fn rename_not_found_message(ctx: &MuiContext, line: i32, col: i32) -> String {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string());
    format!("No rename target at {name}:{}:{}", line.max(0) + 1, col.max(0) + 1)
}

#[cfg(test)]
mod code_action_diagnostics_tests {
    use super::*;

    fn diag(start: i32, end: i32) -> diagnostics::Diag {
        diagnostics::Diag {
            line: 0,
            col_start: start,
            col_end: end,
            severity: Severity::Error,
            code: "E".to_string(),
            message: "bad".to_string(),
        }
    }

    #[test]
    fn code_action_diagnostics_json_keeps_editor_character_columns() {
        let json = code_action_diagnostics_json(&[diag(1, 4)], 0);
        assert!(json.contains(r#""start":{"line":0,"character":1}"#));
        assert!(json.contains(r#""end":{"line":0,"character":4}"#));
    }

    #[test]
    fn code_action_diagnostics_json_lsp_utf16_converts_columns() {
        let json = code_action_diagnostics_json_lsp_utf16("😀abc", &[diag(1, 4)], 0);
        assert!(json.contains(r#""start":{"line":0,"character":2}"#));
        assert!(json.contains(r#""end":{"line":0,"character":5}"#));
    }

    #[test]
    fn source_utf16_col_to_char_converts_lsp_columns() {
        assert_eq!(source_utf16_col_to_char("😀abc", 0, 0), 0);
        assert_eq!(source_utf16_col_to_char("😀abc", 0, 1), 0);
        assert_eq!(source_utf16_col_to_char("😀abc", 0, 2), 1);
        assert_eq!(source_utf16_col_to_char("😀abc", 0, 5), 4);
        assert_eq!(source_utf16_col_to_char("😀abc", 9, 5), 0);
    }
}

#[cfg(test)]
mod definition_target_tests {
    use super::*;

    #[test]
    fn generic_definition_target_maps_same_file_utf16_columns() {
        let path = std::path::Path::new("C:/tmp/main.rs");
        let target = definition_target_from_lsp(
            Language::Rust,
            path,
            "\u{1f600} target",
            "file:///C:/tmp/main.rs",
            0,
            3,
        )
        .expect("target");

        assert_eq!(target.line, 0);
        assert_eq!(target.col, 2);
    }

    #[test]
    fn mighty_definition_target_keeps_server_columns() {
        let path = std::path::Path::new("C:/tmp/main.mty");
        let target = definition_target_from_lsp(
            Language::Mighty,
            path,
            "\u{1f600} target",
            "file:///C:/tmp/main.mty",
            0,
            3,
        )
        .expect("target");

        assert_eq!(target.col, 3);
    }

    #[test]
    fn generic_definition_target_maps_cross_file_utf16_columns() {
        let dir = std::env::temp_dir().join(format!(
            "mighty_ide_def_target_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let current_path = dir.join("main.rs");
        let target_path = dir.join("lib.rs");
        std::fs::write(&target_path, "\u{1f600} target").expect("target source");
        let uri = crate::language::lsp::file_uri(&target_path);

        let target = definition_target_from_lsp(
            Language::Rust,
            &current_path,
            "",
            &uri,
            0,
            3,
        )
        .expect("target");

        assert_eq!(target.path, target_path);
        assert_eq!(target.col, 2);
        let _ = std::fs::remove_file(&target.path);
        let _ = std::fs::remove_dir(&dir);
    }
}

/// Resolve the file to edit: `argv[1]` if given, else a virtual scratch tab.
/// The scratch tab is not file-backed, so startup does not create `scratch.mty`
/// in the workspace or make a clean Git repo dirty.
///
/// The `bool` return is `true` when a file argument WAS supplied. On a no-arg
/// launch (`false`) the IDE forces the branded Welcome screen open so a
/// double-click lands on the landing page instead of an anonymous scratch
/// buffer. Typing dismisses Welcome straight into the virtual scratch tab.
fn resolve_target_path() -> (Option<PathBuf>, bool) {
    if let Some(arg) = std::env::args().nth(1) {
        return (Some(PathBuf::from(arg)), true);
    }
    (None, false)
}

/// First-run onboarding: if the recent-folders MRU is empty AND a bundled
/// `samples/` directory exists beside the running exe, record it so the Welcome
/// screen surfaces a clickable "samples" recent folder. Idempotent — only seeds
/// when the MRU is empty, so a returning user's real history is never touched.
fn seed_first_run_samples(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let mut changed = ctx.recent_workspaces.remove(dir);
    let samples = dir.join("samples");
    if ctx.recent_workspaces.is_empty() && samples.is_dir() {
        ctx.recent_workspaces.record(samples.clone());
        println!("mui_init_s: first-run -> seeded samples folder {}", samples.display());
        changed = true;
    }
    if changed {
        let _ = crate::config::save_recent_workspaces(&ctx.recent_workspaces.to_blob());
    }
}

/// Basename of `path` (file name component), or the whole path as a fallback.
fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn open_failed_message(path: &std::path::Path, reason: &str) -> String {
    let name = basename(path);
    if reason.trim().is_empty() {
        format!("Open failed: {name}")
    } else {
        format!("Open failed: {name}: {}", reason.trim())
    }
}

fn file_operation_failed_message(action: &str, path: &std::path::Path, e: &std::io::Error) -> String {
    let name = basename(path);
    let reason = e.to_string();
    if reason.trim().is_empty() {
        format!("{action} failed: {name}")
    } else {
        format!("{action} failed: {name}: {}", reason.trim())
    }
}

/// Initial directory for native file dialogs. Prefer the folder of the active
/// file so Open/New/Save As land where the user is already working; fall back to
/// the workspace root when the active tab is untitled or its parent is missing.
pub(crate) fn file_dialog_initial_dir(ctx: &MuiContext) -> PathBuf {
    if let Some(parent) = ctx
        .tabs
        .active_path()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .filter(|p| p.is_dir())
    {
        parent
    } else {
        crate::wsabi::effective_root(ctx)
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn dialog_owner_hwnd(ctx: &MuiContext) -> Option<isize> {
    ctx.host.as_ref().and_then(|host| host.hwnd_isize())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn dialog_owner_hwnd(_ctx: &MuiContext) -> Option<isize> {
    None
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

pub(crate) fn visible_surface_size_for(
    width: u32,
    phys_width: u32,
    height: u32,
    phys_height: u32,
) -> (u32, u32) {
    let w = layout::dock_visible_width(width, phys_width);
    let h = layout::visible_height(height, phys_height);
    (
        env_surface_cap(&["MUI_SCREENSHOT_W", "MUI_WIDTH"], w),
        env_surface_cap(&["MUI_SCREENSHOT_H", "MUI_HEIGHT"], h),
    )
}

fn visible_surface_size(ctx: &MuiContext) -> (u32, u32) {
    visible_surface_size_for(ctx.gpu.width, ctx.gpu.phys_width, ctx.gpu.height, ctx.gpu.phys_height)
}

fn env_surface_cap(keys: &[&str], fallback: u32) -> u32 {
    let cap = keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|&n| n >= 64)
    });
    cap.map_or(fallback.max(1), |n| fallback.min(n).max(1))
}

// ---------------------------------------------------------------------------
// init / shutdown
// ---------------------------------------------------------------------------

/// Open a window `width`x`height` and return an opaque `i64` handle, or `0` on
/// failure. Scalar mirror of [`crate::mui_init`] that additionally:
///   * resolves the target file from `argv[1]` or creates a virtual scratch tab;
///   * titles the window with the file basename or scratch label;
///   * eagerly loads the file when a file-backed tab exists.
#[no_mangle]
pub extern "C" fn mui_init_s(width: u32, height: u32) -> i64 {
    let (path, had_file_arg) = resolve_target_path();
    let title_name = path
        .as_ref()
        .map(|p| basename(p))
        .unwrap_or_else(|| "(scratch)".to_string());
    let title = format!("{title_name} — Mighty IDE");
    match path.as_ref() {
        Some(path) => println!("mui_init_s: editing {}", path.display()),
        None => println!("mui_init_s: no file arg -> virtual scratch tab"),
    }

    // Optional window-size override (used by screenshot capture to hit an exact
    // size, e.g. 1320x860). Falls back to the size Mighty passed.
    let env_dim = |key: &str, fallback: u32| -> u32 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|&n| n >= 64)
            .unwrap_or(fallback)
    };
    let width = env_dim("MUI_WIDTH", width);
    let height = env_dim("MUI_HEIGHT", height);

    let handle = crate::build_context(width, height, title, path) as usize as i64;

    // First-run onboarding: when there are no recent folders yet (a fresh
    // install) and a bundled `samples/` dir sits next to the exe, seed it into
    // the recents MRU so the Welcome screen's "Recent Folders" offers a
    // one-click "samples" entry to explore. Non-destructive + idempotent (it
    // only fires when the MRU is empty), so it never overrides a real history.
    seed_first_run_samples(handle);

    // No-arg launch (double-click): force the branded Welcome screen so the IDE
    // opens to its landing page, not an anonymous scratch buffer. The virtual
    // scratch tab is still active underneath; "New File" / typing dismisses
    // Welcome straight into it. A file-argument launch skips this and goes
    // directly to the file. Suppressed under any headless/screenshot/probe env so
    // the scripted captures + body screenshots aren't hijacked by the landing
    // (the dedicated MUI_WELCOME_AUTOOPEN hook below covers capturing Welcome).
    if !had_file_arg && !headless_mode_active() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.welcome.open();
            println!("mui_init_s: no file arg -> Welcome screen forced open");
        }
    }

    // Launch-test hook: with MUI_TERM_AUTOOPEN set, eagerly open the terminal so
    // a headless (non-interactive) run can prove the PTY/grid wiring end-to-end
    // — the terminal otherwise only opens on a Ctrl+` keypress, which a headless
    // run can't deliver. No effect on normal interactive launches.
    if std::env::var_os("MUI_TERM_AUTOOPEN").is_some() {
        let opened = mui_term_open(handle);
        println!("mui_init_s: MUI_TERM_AUTOOPEN -> mui_term_open = {opened}");
        mui_log_terminal(handle);
    }

    // Launch-test hook for autocomplete: with MUI_COMPLETE_PROBE set, run a
    // scripted completion request so a headless run proves the engine wiring
    // (Ctrl+Space can't be delivered non-interactively). See `mui_complete_probe`.
    if std::env::var_os("MUI_COMPLETE_PROBE").is_some() {
        mui_complete_probe(handle);
        mui_log_completion(handle);
    }

    // Launch-test hook for hover/definition: with MUI_NAV_PROBE set, run scripted
    // hover + definition requests (F12 / the hover key can't be delivered
    // non-interactively). See `mui_nav_probe`.
    if std::env::var_os("MUI_NAV_PROBE").is_some() {
        mui_nav_probe(handle);
    }

    // Launch-test hook for undo/redo + format: with MUI_HISTORY_PROBE set, run a
    // scripted edit -> undo -> redo and a format over the active buffer so a
    // headless run proves the wiring (Ctrl+Z/Y and the format chord can't be
    // delivered non-interactively). See `mui_history_probe`.
    if std::env::var_os("MUI_HISTORY_PROBE").is_some() {
        mui_history_probe(handle);
    }

    // Launch-test hook for the command palette: with MUI_PALETTE_PROBE set, open
    // the palette, type a query, and log the filtered count + selected id
    // (Ctrl+Shift+P can't be delivered non-interactively). See `mui_palette_probe`.
    if std::env::var_os("MUI_PALETTE_PROBE").is_some() {
        mui_palette_probe(handle);
    }

    // Launch-test hook for LIVE editing (L28 workaround): with MUI_EDIT_PROBE set,
    // run a scripted insert/newline/backspace against the shim's authoritative
    // text model and log the resulting line count + line lengths — proving the
    // model mutates live (keystrokes can't be delivered non-interactively). See
    // `mui_edit_probe`. The mutated model also renders into a screenshot frame.
    if std::env::var_os("MUI_EDIT_PROBE").is_some() {
        mui_edit_probe(handle);
    }

    // Screenshot/render hook for the unsaved-work confirmation. This arms the
    // same state as a real dirty-tab close so headless captures can verify the
    // modal layout without synthetic input.
    if std::env::var_os("MUI_DIRTY_CONFIRM_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let active = ctx.tabs.active();
            ctx.tabs.set_dirty(active, true);
            ctx.pending_dirty_close = Some((active, std::time::Instant::now()));
            println!("mui_init_s: MUI_DIRTY_CONFIRM_AUTOOPEN -> dirty confirm open");
        }
    }

    // Screenshot/render hook for binary-file previews. This opens the packaged
    // icon by default so gallery runs prove binary assets cannot become corrupt
    // editable text. Set MUI_BINARY_AUTOOPEN to a path to override the sample.
    if let Some(seed) = std::env::var_os("MUI_BINARY_AUTOOPEN") {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let raw = seed.to_string_lossy();
            let mut path = if !raw.trim().is_empty() && raw != "1" {
                std::path::PathBuf::from(raw.as_ref())
            } else {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("mighty-ide.ico")))
                    .unwrap_or_else(|| std::env::temp_dir().join("mighty-ide-binary-preview.bin"))
            };
            if !path.exists() {
                path = std::env::temp_dir().join("mighty-ide-binary-preview.bin");
                let _ = std::fs::write(&path, b"\0Mighty IDE binary preview sample");
            }
            let idx = ctx.tabs.open_path(path.clone());
            sync_active_path(ctx);
            ctx.panes = crate::panes::PaneLayout::new(idx);
            ensure_tab_visible(ctx, idx);
            ctx.welcome.dismiss();
            println!("mui_init_s: MUI_BINARY_AUTOOPEN -> {}", path.display());
        }
    }

    // Screenshot/render hook for the command palette: with MUI_PALETTE_AUTOOPEN
    // set, open the palette and LEAVE it open so it renders into the frame
    // (`mui_palette_draw` is a no-op unless the palette is active). Unlike
    // `mui_palette_probe`, this does not cancel — used to capture the palette
    // overlay in a headless screenshot run. No effect on normal launches.
    if std::env::var_os("MUI_PALETTE_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.palette.open();
            // Optionally seed a query so the filtered list is shown.
            if let Some(seed) = std::env::var_os("MUI_PALETTE_AUTOOPEN") {
                let q = seed.to_string_lossy();
                if !q.trim().is_empty() && q != "1" {
                    for ch in q.chars() {
                        ctx.palette.push_char(ch);
                    }
                }
            }
            println!(
                "mui_init_s: MUI_PALETTE_AUTOOPEN -> palette open, count={}",
                ctx.palette.count()
            );
        }
    }

    // Screenshot/render hook for autocomplete: with MUI_COMPLETE_AUTOOPEN set,
    // run a scripted completion request against the active buffer and LEAVE the
    // dropdown open + anchored, so a headless screenshot shows it (the dropdown
    // otherwise only renders while the Mighty loop is `completing`). The env
    // value is the prefix to complete (default `"cl"`). No effect on launches.
    if std::env::var_os("MUI_COMPLETE_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let prefix = std::env::var("MUI_COMPLETE_AUTOOPEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty() && v != "1")
                .unwrap_or_else(|| "cl".to_string());
            // Build active-tab bytes + a newline + the prefix; request there.
            let active = ctx.tabs.active();
            let mut buf: Vec<u8> = Vec::new();
            let n = ctx.tabs.load_len(active);
            for i in 0..(n.max(0) as usize) {
                let b = ctx.tabs.load_byte(active, i);
                if (0..=255).contains(&b) {
                    buf.push(b as u8);
                }
            }
            // Screenshot-only seed: inject a few identifiers sharing the prefix
            // so the captured dropdown shows the rich multi-row card (varied type
            // badges + signatures). This affects the AUTOOPEN capture path only.
            let seeds: &[&str] = match prefix.as_str() {
                "cl" => &["classify", "clamp", "clone", "close"],
                _ => &[],
            };
            for s in seeds {
                buf.extend_from_slice(format!(" {s}").as_bytes());
            }
            buf.push(b'\n');
            buf.extend_from_slice(prefix.as_bytes());
            let cursor = buf.len();
            ctx.complete_buf = buf;
            let count = ctx.complete.request(&ctx.complete_buf, cursor, &[]);
            // Anchor near the top of the editor body so the card is fully visible.
            ctx.complete_autoopen = Some((6, prefix.chars().count() as i32 + 8));
            println!("mui_init_s: MUI_COMPLETE_AUTOOPEN -> prefix=\"{prefix}\" candidates={count}");
        }
    }

    // Launch-test hook for the language-intelligence features: with
    // MUI_LANG_PROBE set, drive the REAL ABI (signatureHelp / rename / codeAction)
    // against the active model + live `mty lsp` and log the results, proving the
    // shim wiring end-to-end (the F2 / Ctrl+. / `(` triggers can't be delivered
    // non-interactively). No effect on normal launches.
    if std::env::var_os("MUI_LANG_PROBE").is_some() {
        // Signature help: place the cursor just after `add(` in the demo, request.
        if let Some(ctx) = unsafe { ctx(handle) } {
            // Find a line containing `(` to probe signature help; default cursor 0.
            let text = ctx.tabs.active_model().as_text();
            let mut sl = 0i32;
            let mut sc = 0i32;
            for (i, line) in text.split('\n').enumerate() {
                if let Some(p) = line.find('(') {
                    sl = i as i32;
                    sc = line[..=p].chars().count() as i32;
                    break;
                }
            }
            ctx.tabs.active_model_mut().move_to(sl, sc);
        }
        let sig = mui_sig_request(handle, {
            unsafe { ctx(handle) }.map(|c| c.tabs.active_model().cursor_line() as i32).unwrap_or(0)
        }, {
            unsafe { ctx(handle) }.map(|c| c.tabs.active_model().cursor_col() as i32).unwrap_or(0)
        });
        println!("lang-probe: signatureHelp available={sig}");
        // Code actions on the cursor line.
        let (cl, cc) = unsafe { ctx(handle) }
            .map(|c| (c.tabs.active_model().cursor_line() as i32, c.tabs.active_model().cursor_col() as i32))
            .unwrap_or((0, 0));
        let ca = mui_codeaction_request(handle, cl, cc);
        println!("lang-probe: codeActions={ca}");
        mui_codeaction_cancel(handle);
        // Rename prepare on the same position (don't commit — read-only probe).
        let rp = mui_rename_prepare(handle, cl, cc);
        println!("lang-probe: rename-prepare={rp}");
        mui_rename_cancel(handle);
    }

    // Screenshot/render hooks for the deeper language-intelligence features:
    // MUI_SIG_AUTOOPEN / MUI_RENAME_AUTOOPEN / MUI_CODEACTION_AUTOOPEN leave the
    // signature popup / rename input / code-action menu open + anchored so a
    // headless screenshot captures them (each draw is otherwise a no-op unless
    // its UI is active, which a non-interactive run can't trigger). The env value
    // optionally seeds the request position / new name. No effect on launches.
    if std::env::var_os("MUI_SIG_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let demo = b"fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let total = add(40, 2)\n  print(total)\n}\n";
            *ctx.tabs.active_model_mut() = crate::editor::TextModel::from_bytes(demo);
            ctx.tabs.active_model_mut().move_to(5, 22);
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
            // Seed a signature directly (so the capture is deterministic even if
            // the LSP is slow): a representative `fn add` signature, active param 1.
            let ok = ctx.sig.set(Some(crate::language::ParsedSignature {
                label: "fn add(a: I32, b: I32) -> I32".to_string(),
                params: vec!["a: I32".to_string(), "b: I32".to_string()],
                active: 1,
                doc: "Adds two integers and returns the sum.".to_string(),
            }));
            ctx.sig_autoopen = Some((5, 22));
            println!("mui_init_s: MUI_SIG_AUTOOPEN -> signature active={ok}");
        }
    }
    if std::env::var_os("MUI_RENAME_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let demo = b"fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let total = add(40, 2)\n  print(total)\n}\n";
            *ctx.tabs.active_model_mut() = crate::editor::TextModel::from_bytes(demo);
            ctx.tabs.active_model_mut().move_to(0, 3);
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
            let seed = std::env::var("MUI_RENAME_AUTOOPEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty() && v != "1")
                .unwrap_or_else(|| "add".to_string());
            ctx.rename.open(&seed);
            // Type a fresh name so the field shows an edited value.
            for ch in "compute_sum".chars() {
                ctx.rename.push(ch as u32);
            }
            ctx.rename_autoopen = true;
            println!("mui_init_s: MUI_RENAME_AUTOOPEN -> rename open for \"{seed}\"");
        }
    }
    if std::env::var_os("MUI_CODEACTION_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let demo = b"fn main() {\n  prnt(\"hello\")\n}\n";
            *ctx.tabs.active_model_mut() = crate::editor::TextModel::from_bytes(demo);
            ctx.tabs.active_model_mut().move_to(1, 4);
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
            let actions = vec![
                crate::language::CodeAction {
                    title: "Replace 'prnt' with 'print'".to_string(),
                    edit: None,
                    command_edit: None,
                    command: None,
                    fix_all_mty: false,
                },
                crate::language::CodeAction {
                    title: "Import 'print' from std".to_string(),
                    edit: None,
                    command_edit: None,
                    command: None,
                    fix_all_mty: false,
                },
                crate::language::CodeAction {
                    title: "Fix all (mty)".to_string(),
                    edit: None,
                    command_edit: None,
                    command: None,
                    fix_all_mty: true,
                },
            ];
            let n = ctx.codeaction.set(actions);
            ctx.codeaction_autoopen = Some((1, 4));
            println!("mui_init_s: MUI_CODEACTION_AUTOOPEN -> {n} actions");
        }
    }

    // Screenshot/render hook for the quick-fix lightbulb: with
    // MUI_LIGHTBULB_AUTOOPEN set, mark the cursor line as having code actions so
    // the gutter bulb is drawn for a headless capture (it otherwise needs a live
    // LSP probe via the debounced tick). The env value optionally seeds the line.
    if std::env::var_os("MUI_LIGHTBULB_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            // Default to line 0 so the bulb survives main()'s `mui_ed_load`
            // (which resets the cursor to the top); the line value is honored only
            // when it stays put. The cursor is moved to match so `visible_for`
            // agrees, and a `_force_line` field keeps it pinned past the reset.
            let line = std::env::var("MUI_LIGHTBULB_AUTOOPEN")
                .ok()
                .and_then(|v| v.trim().parse::<i32>().ok())
                .filter(|v| *v >= 0)
                .unwrap_or(0);
            ctx.tabs.active_model_mut().move_to(line, 0);
            ctx.lightbulb.set_result(line, true);
            ctx.lightbulb_autoopen = Some(line);
            println!("mui_init_s: MUI_LIGHTBULB_AUTOOPEN -> bulb on line {line}");
        }
    }

    // Screenshot/render hook for the in-file replace bar: with
    // MUI_REPLACE_AUTOOPEN set, open the replace bar with seeded find/replace
    // fields and LEAVE it open + focused on the replace field so a headless
    // capture shows it (the bar otherwise only draws while `replacing` in the
    // Mighty loop, which a non-interactive run can't enter). The env value is an
    // optional "find:replace" seed (default "world:Mighty"). No effect on launches.
    if std::env::var_os("MUI_REPLACE_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let raw = std::env::var("MUI_REPLACE_AUTOOPEN").unwrap_or_default();
            let (find, repl) = crate::prompt::parse_replace_seed(&raw);
            ctx.replace_bar.open(&find);
            ctx.replace_bar.toggle_focus(); // focus the replace field
            for ch in repl.chars() {
                ctx.replace_bar.push(ch as u32);
            }
            println!("mui_init_s: MUI_REPLACE_AUTOOPEN -> find=\"{find}\" repl=\"{repl}\"");
        }
    }

    // Screenshot/render hook for the theme picker: with MUI_THEMEPICKER_AUTOOPEN
    // set, open the chooser and LEAVE it open so a headless screenshot shows the
    // overlay (it otherwise only draws while the Mighty loop routes to it). The
    // active theme itself is selected by MUI_THEME (resolved in build_context).
    if std::env::var_os("MUI_THEMEPICKER_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.theme_picker.open();
            ctx.theme_picker_autoopen = true;
            println!(
                "mui_init_s: MUI_THEMEPICKER_AUTOOPEN -> theme picker open, active={}",
                crate::theme::active_id().name()
            );
        }
    }

    // Screenshot/render hook for the keyboard-shortcuts overlay: with
    // MUI_SHORTCUTS_AUTOOPEN set, open the overlay (optionally seeding a filter
    // query, e.g. "alt") and LEAVE it open so a headless screenshot shows it
    // (`mui_keys_draw` is a no-op unless active). No effect on normal launches.
    if std::env::var_os("MUI_SHORTCUTS_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.shortcuts.open();
            if let Some(seed) = std::env::var_os("MUI_SHORTCUTS_AUTOOPEN") {
                let q = seed.to_string_lossy();
                if !q.trim().is_empty() && q != "1" {
                    for ch in q.chars() {
                        ctx.shortcuts.push_char(ch);
                    }
                }
            }
            ctx.shortcuts_autoopen = true;
            println!(
                "mui_init_s: MUI_SHORTCUTS_AUTOOPEN -> shortcuts open, count={}",
                ctx.shortcuts.count()
            );
        }
    }

    // Screenshot/render hook for the AI copilot panel: with MUI_AI_AUTOOPEN set,
    // open the right-docked AI panel and seed a fake transcript (no network) so a
    // headless screenshot captures the chat UI — distinct user/assistant turns, a
    // monospace code card, and (with the value "stream") a live "thinking…"
    // indicator. No effect on normal launches.
    if std::env::var_os("MUI_AI_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.ai.open = true;
            ctx.ai.force_transcript = true;
            ctx.ai.transcript.push(crate::ai::Turn {
                role: crate::ai::Role::User,
                text: "How do I read a file and print its line count in Mighty?".to_string(),
            });
            ctx.ai.transcript.push(crate::ai::Turn {
                role: crate::ai::Role::Assistant,
                text: "Use the std `fs` effect to read the bytes, then count the \
                       newlines. Here's a small function:\n\n\
                       ```\nfn line_count(path: Str) -> I32 {\n  \
                       let bytes = fs::read(path)\n  \
                       let mut n: I32 = 1\n  \
                       for b in bytes { if b == 10 { n = n + 1 } }\n  \
                       n\n}\n```\n\n\
                       Call it from `main` and `log` the result. The `for` loop \
                       walks the bytes once, so it's O(n)."
                    .to_string(),
            });
            println!(
                "mui_init_s: MUI_AI_AUTOOPEN -> AI panel open, {} turns, has_key={}",
                ctx.ai.transcript.len(),
                crate::ai::api_key().is_some()
            );
        }
    }

    // Screenshot/render hook for inline AI ghost-text: with MUI_GHOST_AUTOOPEN
    // set, seed a fake multi-line ghost suggestion anchored at the end of the
    // active file's first non-empty line, so a headless capture shows the DIM
    // ghost-text overlay after the cursor — without a live API call. The env value
    // optionally overrides the suggestion text (newlines as "\n"). No effect on
    // normal launches (the engine otherwise only fires on a real debounced call).
    if let Some(seed) = std::env::var_os("MUI_GHOST_AUTOOPEN") {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let demo = b"fn collect(items: List<Str>) -> I32 {\n  let mut total: I32 = 0\n  for item in items {\n    total";
            *ctx.tabs.active_model_mut() = crate::editor::TextModel::from_bytes(demo);
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
            let raw = seed.to_string_lossy();
            let raw = raw.trim();
            let suggestion = if raw.is_empty() || raw == "1" {
                " = total + 1\n  }\n  total\n}".to_string()
            } else {
                raw.replace("\\n", "\n")
            };
            // Anchor at the end of the incomplete expression, with empty space
            // below so the ghost lines do not overlap real source text.
            let (al, ac) = {
                let m = ctx.tabs.active_model();
                let li = m.line_count().saturating_sub(1);
                (li, m.line_len(li))
            };
            ctx.tabs.active_model_mut().move_to(al as i32, ac as i32);
            ctx.ghost.seed_demo(&suggestion, (al, ac));
            println!(
                "mui_init_s: MUI_GHOST_AUTOOPEN -> ghost seeded at ({al},{ac}), has_key={}",
                crate::ai::api_key().is_some()
            );
        }
    }

    // Screenshot/render hook for the activity-rail panels: with
    // MUI_PANEL_AUTOOPEN set to "scm" or "search", switch the sidebar to that
    // panel and seed its data (run git status / a search) so a headless
    // screenshot captures the populated panel. No effect on normal launches.
    if let Some(which) = std::env::var_os("MUI_PANEL_AUTOOPEN") {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let which = which.to_string_lossy().to_lowercase();
            let dir = ctx.tree.root().to_path_buf();
            if which.contains("scm") || which.contains("git") || which.contains("source") {
                ctx.active_panel = crate::PANEL_SCM;
                ctx.sidebar_visible = true;
                let n = ctx.scm.refresh(&dir);
                println!("mui_init_s: MUI_PANEL_AUTOOPEN -> SCM, {n} changes, branch={}", ctx.scm.status.branch);
            } else if which.contains("search") {
                ctx.active_panel = crate::PANEL_SEARCH;
                ctx.sidebar_visible = true;
                // Seed a query so the results list renders. Default "fn"; override
                // via the env value, e.g. MUI_PANEL_AUTOOPEN="search:mui".
                let seed = which.split(':').nth(1).filter(|s| !s.is_empty()).unwrap_or("fn");
                for ch in seed.chars() {
                    ctx.search.push_char(ch as u32);
                }
                let n = ctx.search.run(&dir);
                println!("mui_init_s: MUI_PANEL_AUTOOPEN -> SEARCH \"{seed}\", {n} matches");
            }
        }
    }

    // Screenshot/render hook for the Run panel: with MUI_RUN_AUTOOPEN set, open
    // the Run panel and seed fake output (a clickable diagnostic + an exit line)
    // so a headless capture shows the panel without spawning a real process.
    if std::env::var_os("MUI_RUN_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let p = ctx
                .tabs
                .active_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "demo.mty".to_string());
            ctx.run.seed_demo(&p);
            println!("mui_init_s: MUI_RUN_AUTOOPEN -> run panel seeded ({} lines)", ctx.run.line_count());
        }
    }

    // Screenshot/render hook for the Web Playground: with MUI_WEB_AUTOOPEN set,
    // open the Web panel and seed fake `mty serve` output (a scraped URL + build
    // status) so a headless capture shows the panel without spawning a real
    // server. No effect on normal launches.
    if std::env::var_os("MUI_WEB_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let p = ctx
                .tabs
                .active_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "examples/webspin/src/main.mty".to_string());
            ctx.run.close();
            ctx.web.seed_demo(&p);
            println!(
                "mui_init_s: MUI_WEB_AUTOOPEN -> web playground seeded ({} lines, url={})",
                ctx.web.line_count(),
                ctx.web.url()
            );
        }
    }

    // Screenshot/render hook for the Test panel: with MUI_TEST_AUTOOPEN set,
    // switch the sidebar to the Testing view and seed a mix of pass/fail results
    // + a summary so a headless capture shows the results tree without spawning a
    // real `mty test`. No effect on normal launches.
    if std::env::var_os("MUI_TEST_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let pkg = ctx
                .tabs
                .active_path()
                .map(|p| crate::tests_panel::TestPanel::package_dir(&p).to_string_lossy().into_owned())
                .unwrap_or_else(|| "demo".to_string());
            ctx.tests_panel.seed_demo(&pkg);
            ctx.active_panel = crate::PANEL_TEST;
            ctx.sidebar_visible = true;
            println!(
                "mui_init_s: MUI_TEST_AUTOOPEN -> testing view seeded ({} passed, {} failed, {} total)",
                ctx.tests_panel.passed(),
                ctx.tests_panel.failed(),
                ctx.tests_panel.total()
            );
        }
    }

    // Screenshot/render hook for the debugger: with MUI_DEBUG_AUTOOPEN set, open
    // the Run-and-Debug view, switch the sidebar to it, and seed a fake stopped
    // state (breakpoints + a stopped line + call stack + variables) so a headless
    // capture shows the debug view without a live `mty dap` session.
    if std::env::var_os("MUI_DEBUG_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let p = ctx
                .tabs
                .active_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "demo.mty".to_string());
            ctx.dbg.seed_demo(&p);
            ctx.active_panel = crate::PANEL_DEBUG;
            ctx.sidebar_visible = true;
            println!(
                "mui_init_s: MUI_DEBUG_AUTOOPEN -> debug view seeded ({} frames, {} vars)",
                ctx.dbg.stack_count(),
                ctx.dbg.variable_count()
            );
        }
    }

    // Screenshot/render hook for the inline git diff: with MUI_DIFF_AUTOOPEN set,
    // open the diff view with a representative sample diff (so a headless capture
    // shows the green/red hunk rendering without external git state).
    if std::env::var_os("MUI_DIFF_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            const SAMPLE: &str = "\
diff --git a/src/main.mty b/src/main.mty
index 83db48f..f735c2d 100644
--- a/src/main.mty
+++ b/src/main.mty
@@ -1,6 +1,7 @@
 fn main() {
-  let name: Str = \"world\"
-  log(\"Hello\")
+  let name: Str = \"Mighty\"
+  log(\"Hello, Mighty!\")
+  log(\"Welcome to the IDE\")
   let n: I32 = 42
 }
@@ -20,3 +21,4 @@ fn helper() {
   compute()
+  validate()
   done()
";
            let n = ctx.diff.open("src/main.mty", false, SAMPLE);
            ctx.welcome.dismiss_empty_auto();
            println!("mui_init_s: MUI_DIFF_AUTOOPEN -> diff view open ({n} lines)");
        }
    }

    // Screenshot/render hook for the live Markdown preview: with MUI_MD_AUTOOPEN
    // set, seed the active buffer with a crafted markdown sample (or the existing
    // `.md` buffer), open the split preview, and the unconditional `mui_ed_draw`
    // pane loop then renders source-on-left / rendered-on-right for the capture.
    if std::env::var_os("MUI_MD_AUTOOPEN").is_some() {
        {
            use crate::editor::TextModel;
            const SAMPLE: &str = "\
# Markdown Preview

A **live** preview rendered to the active *theme*, updating as you type. It \
supports `inline code`, [links](https://mighty.dev), and ~~strikethrough~~.

## Features

- ATX headings, scaled by level
- **Bold**, *italic*, and `code` spans
  - nested list items by indent
- ordered lists and tables

1. Parse the buffer
2. Build a block model
3. Draw with Vello

```rust
fn render(md: &str) -> Scene {
    let blocks = markdown::parse(md);
    paint(blocks)
}
```

> Blockquotes get an accent left-bar and dimmed text.

| Feature | Status |
|---------|--------|
| Headings | done |
| Code | done |

---

That's the whole set.
";
            if let Some(c) = unsafe { ctx(handle) } {
                let m = c.tabs.active_model_mut();
                *m = TextModel::from_bytes(SAMPLE.as_bytes());
                c.edit_probe_lock = true;
                c.language = crate::langdetect::Language::Markdown;
            }
            let r = mui_md_open(handle);
            if let Some(c) = unsafe { ctx(handle) } {
                println!(
                    "mui_init_s: MUI_MD_AUTOOPEN -> preview open={r}, panes={}, md_pane={:?}",
                    c.panes.count(),
                    c.md_pane
                );
            }
        }
    }

    // Screenshot/render hook for the branch switcher: with MUI_BRANCH_AUTOOPEN
    // set, open the picker over a representative branch list so a headless capture
    // shows the overlay without external git state.
    if std::env::var_os("MUI_BRANCH_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let demo = "\
* main
  develop
  feature/branch-switcher
  feature/blame-gutter
  remotes/origin/HEAD -> origin/main
  remotes/origin/main
  remotes/origin/develop
";
            ctx.scm.branches = crate::scm::parse_branches(demo);
            let list = ctx.scm.branches.clone();
            ctx.branch_picker.open(&list);
            println!(
                "mui_init_s: MUI_BRANCH_AUTOOPEN -> branch picker open ({} branches)",
                list.len()
            );
        }
    }

    // Screenshot/render hook for the typography pass: with MUI_TYPO_AUTOOPEN set,
    // seed a comment-rich Mighty buffer so a headless capture shows the editor's
    // real italic-comment face (and the bold active-tab / EXPLORER header chrome).
    if std::env::var_os("MUI_TYPO_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"// greeter.mty \xE2\x80\x94 comments render in a TRUE italic face.\n// Block of doc text: note the slanted glyphs vs. the upright code below.\n\nagent Greeter {\n  state name: Str  // inline comment, also italic\n\n  // Build a friendly greeting for the stored name.\n  fn greet(self) -> Str {\n    let prefix = \"Hello, \"   // string + comment on one line\n    prefix + self.name + \"!\"\n  }\n}\n\nfn main() {\n  // The active tab label + EXPLORER header read in a bold UI face.\n  let g = Greeter { name: \"Mighty\" }\n  print(g.greet())\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            ctx.edit_probe_lock = true;
            ctx.language = crate::langdetect::Language::Mighty;
            println!("mui_init_s: MUI_TYPO_AUTOOPEN -> comment-rich buffer seeded");
        }
    }

    // Screenshot/render hook for the git blame gutter: with MUI_BLAME_AUTOOPEN
    // set, seed a representative buffer + a parsed blame for it and activate the
    // gutter so a headless capture shows the dim per-line sha · author · date band.
    if std::env::var_os("MUI_BLAME_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"fn main() {\n  let name: Str = \"Mighty\"\n  log(\"Hello, Mighty!\")\n  let n: I32 = 42\n  for i in 0..n {\n    log(i)\n  }\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            ctx.edit_probe_lock = true;
            // A porcelain blob covering the 8 demo lines across three commits.
            let blob = "\
1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b 1 1 2
author Ada Lovelace
author-time 1136239445
author-tz +0000
summary scaffold main
filename src/main.mty
\tfn main() {
1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b 2 2
\t  let name: Str = \"Mighty\"
9f8e7d6c5b4a39281706f5e4d3c2b1a0978695a4 3 3 1
author Grace Hopper
author-time 1700000000
author-tz +0000
summary friendly greeting
filename src/main.mty
\t  log(\"Hello, Mighty!\")
c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2 4 4 4
author Linus T
author-time 1685000000
author-tz +0000
summary loop demo
filename src/main.mty
\t  let n: I32 = 42
c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2 5 5
\t  for i in 0..n {
c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2 6 6
\t    log(i)
c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2 7 7
\t  }
0000000000000000000000000000000000000000 8 8 1
author Not Committed Yet
author-time 1700000000
author-tz +0000
summary Version of ... (Not Committed Yet)
filename src/main.mty
\t}
";
            let n = ctx.blame.seed_demo(blob);
            println!("mui_init_s: MUI_BLAME_AUTOOPEN -> blame gutter on ({n} lines)");
        }
    }

    // Screenshot/render hook for code folding: with MUI_FOLD_AUTOOPEN set, seed a
    // buffer with several brace blocks, compute foldable ranges, and FOLD a couple
    // regions so a headless capture shows the ▸/▾ gutter chevrons + the faint
    // "⋯ N lines" indicator on a collapsed region. No effect on normal launches.
    if std::env::var_os("MUI_FOLD_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"struct Vec2 {\n  x: F32,\n  y: F32,\n}\n\nfn length(v: Vec2) -> F32 {\n  let s = v.x * v.x + v.y * v.y\n  sqrt(s)\n}\n\nfn normalize(v: Vec2) -> Vec2 {\n  let len = length(v)\n  if len > 0.0 {\n    Vec2 { x: v.x / len, y: v.y / len }\n  } else {\n    v\n  }\n}\n\nagent Mover {\n  fn step(self, by: Vec2) {\n    log(\"moving\")\n  }\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            ctx.language = crate::langdetect::Language::Mighty;
            ctx.edit_probe_lock = true;
            ctx.tabs.recompute_active_fold();
            // Fold the `struct Vec2` block (header line 0) and the `step` method
            // (header line 20) so the capture shows both a folded + open chevron.
            let f = ctx.tabs.active_fold_mut();
            f.toggle(0);
            f.toggle(20);
            let n = f.ranges().len();
            println!("mui_init_s: MUI_FOLD_AUTOOPEN -> {n} foldable regions, 2 folded");
        }
    }

    // Screenshot/render hook for the Settings panel: with MUI_SETTINGS_AUTOOPEN
    // set, open the Settings panel (and optionally pre-select a row via the env
    // value, e.g. "2") so a headless capture shows the preference list.
    if let Some(seed) = std::env::var_os("MUI_SETTINGS_AUTOOPEN") {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.settings_panel.open();
            let v = seed.to_string_lossy();
            if let Ok(row) = v.trim().parse::<i32>() {
                // move_sel from row 0 to the requested row.
                ctx.settings_panel.move_sel(row);
            }
            println!("mui_init_s: MUI_SETTINGS_AUTOOPEN -> settings panel open");
        }
    }

    // Screenshot/render hook for the Outline panel: with MUI_OUTLINE_AUTOOPEN set,
    // switch the sidebar to the Outline panel and scan the active document's
    // symbols so a headless capture shows the populated tree. Reports the path
    // used (scanner / LSP). No effect on normal launches.
    if std::env::var_os("MUI_OUTLINE_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"agent EditorAgent {\n  state root: Str\n\n  fn open_workspace(self, path: Str) -> I32 {\n    1\n  }\n\n  fn run_checks(self) -> I32 {\n    0\n  }\n}\n\nstruct WorkspaceFile {\n  path: Str\n}\n\nfn main() {\n  let agent = EditorAgent { root: \"samples\" }\n  agent.run_checks()\n}\n";
            *ctx.tabs.active_model_mut() = TextModel::from_bytes(demo);
            ctx.tabs.active_model_mut().move_to(7, 4);
            ctx.edit_probe_lock = true;
        }
        let _ = crate::navsurfaces::mui_outline_refresh(handle);
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.active_panel = crate::PANEL_OUTLINE;
            ctx.sidebar_visible = true;
            // Park the cursor inside the second symbol so the current-row
            // highlight is visible in the capture.
            let target = ctx.outline.get(1).or_else(|| ctx.outline.get(0)).map(|s| s.line).unwrap_or(0);
            let _ = ctx.outline.set_cursor(target);
            println!(
                "mui_init_s: MUI_OUTLINE_AUTOOPEN -> outline open, {} symbols ({})",
                ctx.outline.count(),
                if ctx.outline.used_lsp() { "lsp" } else { "scanner" }
            );
        }
    }

    // Screenshot/render hook for the Problems panel: with MUI_PROBLEMS_AUTOOPEN
    // set, open the Problems dock and seed a representative aggregated set (no
    // subprocess) so a headless capture shows grouped error/warning rows.
    if std::env::var_os("MUI_PROBLEMS_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::diagnostics::{Diag, Severity};
            let path = ctx
                .tabs
                .active_path()
                .unwrap_or_else(|| std::path::PathBuf::from("src/main.mty"));
            let other = path
                .parent()
                .map(|d| d.join("util.mty"))
                .unwrap_or_else(|| std::path::PathBuf::from("util.mty"));
            let mk = |l: i32, c: i32, s: Severity, code: &str, m: &str| Diag {
                line: l,
                col_start: c,
                col_end: c + 1,
                severity: s,
                code: code.into(),
                message: m.into(),
            };
            ctx.problems.aggregate(vec![
                (
                    path,
                    vec![
                        mk(4, 17, Severity::Error, "MT2001", "expected `I32`, found `Str`"),
                        mk(11, 2, Severity::Warning, "MT3001", "unused variable `tmp`"),
                    ],
                ),
                (
                    other,
                    vec![mk(7, 0, Severity::Error, "MT2019", "function returns `I32`, body produces `Bool`")],
                ),
            ]);
            ctx.problems.set_open(true);
            println!(
                "mui_init_s: MUI_PROBLEMS_AUTOOPEN -> problems open ({} errors, {} warnings)",
                ctx.problems.error_count(),
                ctx.problems.warn_count()
            );
        }
    }

    // Screenshot/render hook for the interactive breadcrumb: with
    // MUI_BREADCRUMB_AUTOOPEN set ("symbol" [default] or "file"), scan symbols
    // and open the corresponding breadcrumb dropdown so a headless capture shows
    // the palette-styled menu under the breadcrumb.
    if let Some(which) = std::env::var_os("MUI_BREADCRUMB_AUTOOPEN") {
        let which = which.to_string_lossy().to_lowercase();
        if !which.contains("file") {
            if let Some(ctx) = unsafe { ctx(handle) } {
                use crate::editor::TextModel;
                let demo = b"agent BreadcrumbAgent {\n  state root: Str\n\n  fn open_workspace(self, path: Str) -> I32 {\n    1\n  }\n\n  fn run_checks(self) -> I32 {\n    0\n  }\n}\n\nstruct BreadcrumbFile {\n  path: Str\n}\n\nfn main() {\n  let agent = BreadcrumbAgent { root: \"samples\" }\n  agent.run_checks()\n}\n";
                *ctx.tabs.active_model_mut() = TextModel::from_bytes(demo);
                ctx.tabs.active_model_mut().move_to(7, 4);
                ctx.edit_probe_lock = true;
            }
        }
        let _ = crate::navsurfaces::mui_outline_refresh(handle);
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.crumb_menu_autoopen = true;
            use crate::crumbmenu::{MenuItem, MenuKind};
            if which.contains("file") {
                // Build a file menu from the active file's directory.
                let dir = ctx.tabs.active_path().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                let files: Vec<(String, std::path::PathBuf)> = dir
                    .as_ref()
                    .map(|d| {
                        let mut v: Vec<_> = std::fs::read_dir(d)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .map(|e| e.path())
                            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("mty"))
                            .filter_map(|p| p.file_name().map(|n| (n.to_string_lossy().into_owned(), p.clone())))
                            .collect();
                        v.sort();
                        v
                    })
                    .unwrap_or_default();
                let active = ctx.tabs.active_path();
                let items: Vec<MenuItem> = files
                    .iter()
                    .enumerate()
                    .map(|(i, (name, full))| {
                        let (icon, color) = file_icon_for(name, Some(full) == active.as_ref());
                        MenuItem { label: name.clone(), icon: Some(icon), icon_color: color, depth: 0, target: i as i32 }
                    })
                    .collect();
                ctx.crumb_files = files.into_iter().map(|(_, p)| p).collect();
                let anchor = layout::sidebar_right() + 90.0;
                let n = ctx.crumb_menu.open(MenuKind::Files, items, anchor);
                println!("mui_init_s: MUI_BREADCRUMB_AUTOOPEN -> file menu ({n} files)");
            } else {
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
                let anchor = layout::sidebar_right() + 220.0;
                let n = ctx.crumb_menu.open(MenuKind::Symbols, items, anchor);
                println!("mui_init_s: MUI_BREADCRUMB_AUTOOPEN -> symbol menu ({n} symbols)");
            }
        }
    }

    // Screenshot/render hook for Quick-Open: with MUI_QUICKOPEN_AUTOOPEN set,
    // open the finder (seeded with the env value as a query) and LEAVE it open
    // so a headless capture shows the overlay. A leading `@` first refreshes the
    // outline so the symbol mode is populated. No effect on normal launches.
    if let Some(seed) = std::env::var_os("MUI_QUICKOPEN_AUTOOPEN") {
        let q = seed.to_string_lossy().into_owned();
        let q = if q.trim() == "1" { String::new() } else { q };
        if q.starts_with('@') {
            let _ = crate::navsurfaces::mui_outline_refresh(handle);
        }
        mui_quickopen_open(handle);
        for ch in q.chars() {
            mui_qo_push_char(handle, ch as i32);
        }
        if let Some(ctx) = unsafe { ctx(handle) } {
            println!(
                "mui_init_s: MUI_QUICKOPEN_AUTOOPEN -> quick-open open, mode={} rows={} query=\"{}\"",
                ctx.quickopen.mode().scalar(),
                ctx.quickopen.count(),
                ctx.quickopen.query()
            );
        }
    }

    // Screenshot/render hook for Sticky scroll: with MUI_STICKY_AUTOOPEN set,
    // seed a representative nested buffer + scroll the editor INTO a method so the
    // enclosing-scope headers (struct + fn) pin at the top, and recompute the
    // sticky set so a headless capture shows the pinned band. No effect normally.
    if std::env::var_os("MUI_STICKY_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            // A long-enough document that the view genuinely scrolls deep inside a
            // method (so the `struct Painter` + `fn render` headers pin above).
            let demo = b"struct Painter {\n  width: I32,\n  height: I32,\n  buffer: Vec[Pixel],\n  gamma: F64,\n\n  fn render(self, scene: Scene) -> Frame {\n    let mut frame = Frame.new(self.width, self.height)\n    let mut depth = DepthBuffer.new(self.width, self.height)\n    let clear = Color.rgb(0.05, 0.06, 0.09)\n    frame.fill(clear)\n    for shape in scene.shapes {\n      let pixels = self.rasterize(shape)\n      for p in pixels {\n        if p.z < depth.at(p.x, p.y) {\n          depth.set(p.x, p.y, p.z)\n          let shaded = self.shade_pixel(p, scene.lights)\n          frame.blend(shaded)\n        }\n      }\n    }\n    self.apply_post_effects(frame)\n    frame.present()\n    frame\n  }\n\n  fn shade_pixel(self, p: Pixel, lights: Vec[Light]) -> Pixel {\n    let mut lit = p.albedo\n    for light in lights {\n      lit = lit + light.contribution(p.normal, p.position)\n    }\n    p.with_color(lit.clamp())\n  }\n\n  fn rasterize(self, shape: Shape) -> Vec[Pixel] {\n    shape.tessellate().map(|t| t.shade())\n  }\n\n  fn apply_post_effects(self, frame: Frame) {\n    frame.bloom(0.4)\n    frame.tonemap(self.gamma)\n    frame.vignette(0.2)\n  }\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            // Scroll so the top visible line is deep inside `render` (line 11),
            // leaving the `struct Painter` + `fn render` headers above the fold.
            m.set_first_visible(11);
            m.move_to(12, 8);
            // Lock out the IDE's initial reload so the seeded buffer survives.
            ctx.edit_probe_lock = true;
        }
        let _ = crate::navsurfaces::mui_outline_refresh(handle);
        let n = crate::stickyabi::mui_sticky_count(handle);
        println!("mui_init_s: MUI_STICKY_AUTOOPEN -> {n} sticky headers pinned");
    }

    // Screenshot/render hook for Peek definition: with MUI_PEEK_AUTOOPEN set, seed
    // a buffer where a call site references a definition above, then open the peek
    // card directly from the live buffer (no LSP dependency for the capture) so a
    // headless screenshot shows the inline framed preview. No effect normally.
    if std::env::var_os("MUI_PEEK_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"fn greeting(name: Str) -> Str {\n  let prefix = \"Hello, \"\n  prefix + name + \"!\"\n}\n\nfn main() {\n  let msg = greeting(\"world\")\n  print(msg)\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            // Cursor on the `greeting` call (line 6); peek the def at line 0.
            m.move_to(6, 12);
            let src = m.as_text();
            let path = ctx
                .file_path
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("src/main.mty"));
            let lang = ctx.language;
            let ok = ctx.peek.open_at(path, 0, 3, 6, lang, Some(&src));
            // Lock out the IDE's initial reload so the seeded buffer survives.
            ctx.edit_probe_lock = true;
            println!(
                "mui_init_s: MUI_PEEK_AUTOOPEN -> peek open={ok} preview_lines={}",
                ctx.peek.line_count()
            );
        }
    }

    // Screenshot/render hook for snippets: with MUI_SNIPPET_AUTOOPEN set, seed a
    // representative buffer, type a snippet prefix on a fresh indented line, and
    // EXPAND it via the real engine so a headless capture shows the snippet body
    // inserted with the first tab-stop ($1) selected. The env value optionally
    // overrides the prefix (default "fn"). No effect on normal launches.
    if let Some(seed) = std::env::var_os("MUI_SNIPPET_AUTOOPEN") {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let prefix = {
                let v = seed.to_string_lossy().trim().to_string();
                if v.is_empty() || v == "1" { "fn".to_string() } else { v }
            };
            // A small program with a blank, indented call site at the end where the
            // snippet expands (so the multi-line body + selection are clearly shown).
            let demo = b"// snippets: type a prefix + Tab to expand,\n// then Tab / Shift+Tab to jump between $1 $2 ... $0.\n\nstruct Vec2 {\n  x: F64,\n  y: F64,\n}\n\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            // Type the prefix on the trailing blank line (with a small indent so the
            // continuation-line indentation is visible in the capture).
            let last = m.line_count().saturating_sub(1);
            m.move_to(last as i32, 0);
            for ch in "  ".chars() {
                m.insert_char(ch);
            }
            for ch in prefix.chars() {
                m.insert_char(ch);
            }
            ctx.edit_probe_lock = true;
            // Expand via the real engine + begin the tab-stop session (selects $1).
            let lang = ctx.language;
            let session = &mut ctx.snippet_session;
            let model = ctx.tabs.active_model_mut();
            let ok = crate::snippets::try_expand(model, session, lang);
            println!(
                "mui_init_s: MUI_SNIPPET_AUTOOPEN -> prefix=\"{prefix}\" expanded={ok}, active={}",
                ctx.snippet_session.is_active()
            );
        }
    }

    // Screenshot/render hook for multi-cursor: with MUI_MULTICURSOR_AUTOOPEN set,
    // seed a representative buffer and several carets + selections (a column block
    // plus Ctrl+D occurrence selections) so a headless capture shows multiple
    // carets/selections rendering in the Vivid-Modern look. No effect on launches.
    if std::env::var_os("MUI_MULTICURSOR_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"fn main() {\n  let count = count + 1\n  let count = count + 2\n  let count = count + 3\n  print(count)\n  print(count)\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            // Ctrl+D chain: select "count" under the caret, then add the next few
            // occurrences as secondary carets (each a live selection).
            m.move_to(1, 6); // on the first "count"
            let _ = m.add_caret_next_occurrence(); // select "count"
            let _ = m.add_caret_next_occurrence(); // + next occurrence
            let _ = m.add_caret_next_occurrence(); // + next
            let _ = m.add_caret_next_occurrence(); // + next
            let _ = m.add_caret_next_occurrence(); // + next
            // Lock out the IDE's initial reload so the seeded carets survive.
            ctx.edit_probe_lock = true;
            println!(
                "mui_init_s: MUI_MULTICURSOR_AUTOOPEN -> {} carets seeded",
                ctx.tabs.active_model().caret_count()
            );
        }
    }

    // Screenshot/render hook for the Welcome screen: with MUI_WELCOME_AUTOOPEN set,
    // force the Welcome landing open and seed a couple of recents so a headless
    // capture shows the branded landing with a populated "Recently Opened" column.
    if std::env::var_os("MUI_WELCOME_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.welcome.open();
            // Seed representative recents (newest first) for the right column.
            let base = ctx
                .file_path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| std::path::PathBuf::from("src"));
            for name in ["main.mty", "lexer.mty", "Cargo.toml", "README.md"] {
                ctx.quickopen.record_mru(base.join(name));
            }
            // Seed real bundled folders for the capture so Welcome never shows
            // fake paths that users cannot open.
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    let samples = exe_dir.join("samples");
                    for folder in [exe_dir.join("examples"), samples.clone()] {
                        if folder.is_dir() {
                            ctx.recent_workspaces.record(folder);
                        }
                    }
                    if samples.is_dir() {
                        ctx.workspace = crate::workspace::Workspace::new(samples);
                    }
                }
            }
            println!(
                "mui_init_s: MUI_WELCOME_AUTOOPEN -> welcome open, {} recents, {} folders",
                ctx.quickopen.mru_len(),
                ctx.recent_workspaces.len()
            );
        }
    }

    // Screenshot/render hook for the focused Open Recent picker. Unlike the
    // branded Welcome capture, this opens the operational chooser and seeds real
    // packaged sample/example paths so visual tests cover row density, footer
    // actions, overflow messaging, and close affordance in the picker itself.
    if std::env::var_os("MUI_RECENT_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.welcome.open_recent_picker();
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    let samples = exe_dir.join("samples");
                    let examples = exe_dir.join("examples");
                    for path in [
                        examples.join("sample.json"),
                        examples.join("sample.rs"),
                        examples.join("sample.py"),
                        examples.join("demo.mty"),
                        examples.join("agents.mty"),
                        samples.join("web-spinner.mty"),
                        samples.join("hello.mty"),
                        samples.join("agents.mty"),
                    ] {
                        if path.is_file() {
                            ctx.quickopen.record_mru(path);
                        }
                    }
                    for folder in [examples, samples.clone()] {
                        if folder.is_dir() {
                            ctx.recent_workspaces.record(folder);
                        }
                    }
                    if samples.is_dir() {
                        ctx.workspace = crate::workspace::Workspace::new(samples);
                    }
                }
            }
            println!(
                "mui_init_s: MUI_RECENT_AUTOOPEN -> recent picker open, {} recents, {} folders",
                ctx.quickopen.mru_len(),
                ctx.recent_workspaces.len()
            );
        }
    }

    // Screenshot/render hook for toasts: with MUI_TOAST_AUTOOPEN set, push a few
    // stacked toasts of varied severity so a headless capture shows the bottom-
    // right stack (toasts otherwise only appear on shim events a non-interactive
    // run can't trigger).
    if std::env::var_os("MUI_TOAST_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::toast::Kind;
            ctx.push_toast(Kind::Error, "MT2001: expected I32, found Str");
            ctx.push_toast(Kind::Info, "Theme: Vivid Modern");
            ctx.push_toast(Kind::Success, "Run finished in 142 ms");
            ctx.push_toast(Kind::Success, "Saved main.mty");
            println!("mui_init_s: MUI_TOAST_AUTOOPEN -> {} toasts seeded", ctx.toasts.len());
        }
    }

    // Screenshot/render hook for Zen mode: with MUI_ZEN_AUTOOPEN set, enable Zen
    // mode AND seed a representative buffer so a headless capture shows the full-
    // window distraction-free editor with real code (not an empty scratch).
    if std::env::var_os("MUI_ZEN_AUTOOPEN").is_some() {
        layout::set_zen(true);
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"// Zen / focus mode \xE2\x80\x94 distraction-free editing.\n\nfn fib(n: I32) -> I32 {\n  if n < 2 {\n    n\n  } else {\n    fib(n - 1) + fib(n - 2)\n  }\n}\n\nagent Greeter {\n  state name: Str\n\n  fn greet(self) -> Str {\n    let prefix = \"Hello, \"\n    prefix + self.name + \"!\"\n  }\n}\n\nfn main() {\n  let mut total = 0\n  for i in 0..12 {\n    total = total + fib(i)\n  }\n  print(total)\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            m.move_to(15, 28);
            ctx.edit_probe_lock = true;
        }
        println!("mui_init_s: MUI_ZEN_AUTOOPEN -> zen mode on (demo buffer seeded)");
    }

    // Screenshot/render hook for the Mighty Agents panel: with MUI_AGENTS_AUTOOPEN
    // set, switch the sidebar to the topology view and seed the model from the
    // bundled examples/agents.mty so a headless capture shows the full topology
    // (protocols/agents/handlers/tools/supervisors) without scanning a real tree.
    if std::env::var_os("MUI_AGENTS_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            ctx.agents.seed_demo();
            ctx.active_panel = crate::PANEL_AGENTS_MTY;
            ctx.sidebar_visible = true;
            println!(
                "mui_init_s: MUI_AGENTS_AUTOOPEN -> agents topology seeded ({} agents, {} protocols, {} tools, {} supervisors)",
                ctx.agents.agent_count(),
                ctx.agents.protocol_count(),
                ctx.agents.tool_count(),
                ctx.agents.supervisor_count()
            );
        }
    }

    // Screenshot/render hook for tab overflow: with MUI_TABS_AUTOOPEN set, seed
    // enough files that the tab strip overflows and the scroll affordances are
    // visible in compact captures. No effect on normal launches.
    if std::env::var_os("MUI_TABS_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let dir = std::env::temp_dir().join("mighty_tabs_overflow_demo");
            let _ = std::fs::create_dir_all(&dir);
            for i in 0..8 {
                let p = dir.join(format!("tab-{i}.mty"));
                let src = format!("// tab {i}\n\nfn tab_{i}() -> I32 {{\n  {i}\n}}\n");
                let _ = std::fs::write(&p, src.as_bytes());
                ctx.tabs.open_path(p);
            }
            let active = ctx.tabs.active();
            ensure_tab_visible(ctx, active);
            ctx.welcome.dismiss();
            println!(
                "mui_init_s: MUI_TABS_AUTOOPEN -> {} tabs, first visible {}",
                ctx.tabs.count(),
                ctx.tab_scroll
            );
        }
    }

    // Screenshot/render hook for the SPLIT EDITOR: with MUI_SPLIT_AUTOOPEN set,
    // seed two files as tabs and split the editor side-by-side (left = first file,
    // right = second), focusing the right pane — so a headless capture shows the
    // two panes + divider + focus outline. No effect on normal launches.
    if std::env::var_os("MUI_SPLIT_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let dir = std::env::temp_dir();
            let left = dir.join("mighty_split_left.mty");
            let right = dir.join("mighty_split_right.mty");
            let left_src = b"// fib.mty \xE2\x80\x94 left pane\n\nfn fib(n: I32) -> I32 {\n  if n < 2 {\n    n\n  } else {\n    fib(n - 1) + fib(n - 2)\n  }\n}\n\nfn main() {\n  let mut total = 0\n  for i in 0..12 {\n    total = total + fib(i)\n  }\n  print(total)\n}\n";
            let right_src = b"// greeter.mty \xE2\x80\x94 right pane (focused)\n\nagent Greeter {\n  state name: Str\n\n  fn greet(self) -> Str {\n    let prefix = \"Hello, \"\n    prefix + self.name + \"!\"\n  }\n}\n\nfn main() {\n  let g = Greeter { name: \"Mighty\" }\n  print(g.greet())\n}\n";
            let _ = std::fs::write(&left, left_src);
            let _ = std::fs::write(&right, right_src);
            // Tab 0 (the initial scratch/file) becomes the left file; open the
            // right file as tab 1.
            let li = ctx.tabs.open_path(left.clone());
            *ctx.tabs.active_model_mut() = TextModel::from_bytes(left_src);
            let ri = ctx.tabs.open_path(right.clone());
            *ctx.tabs.active_model_mut() = TextModel::from_bytes(right_src);
            ctx.tabs.active_model_mut().move_to(11, 10);
            // Bind pane 0 -> left tab, split right showing the right tab, focus it.
            ctx.panes = crate::panes::PaneLayout::new(li);
            let s = ctx.tabs.model_at(li).map(|m| m.first_visible()).unwrap_or(0);
            ctx.tabs.switch(li);
            ctx.panes.split_right(ri, s);
            pane_rebind_focus(ctx);
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
            println!(
                "mui_init_s: MUI_SPLIT_AUTOOPEN -> {} panes (focused={}, left tab={}, right tab={})",
                ctx.panes.count(),
                ctx.panes.focused(),
                li,
                ri
            );
        }
    }

    // Screenshot/render hook for bracket colors + indent guides: with
    // MUI_BRACKETS_AUTOOPEN set, seed a deeply-nested buffer and place the cursor
    // inside a nested block so a headless capture shows the rainbow brackets +
    // (active) indent guides. No effect on normal launches.
    if std::env::var_os("MUI_BRACKETS_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            let demo = b"// Bracket colors + indent guides \xE2\x80\x94 nested code.\nfn process(items: List, opts: Opts) -> Result {\n  let mut acc = []\n  for item in items {\n    if item.valid {\n      match item.kind {\n        Kind::A => {\n          acc.push(transform(item, [opts.a, opts.b, (opts.c + 1)]))\n        }\n        Kind::B => {\n          while item.has_next() {\n            let next = item.next({ depth: (level * 2), tags: [\"x\", \"y\"] })\n            acc.push(next)\n          }\n        }\n        _ => { skip(item) }\n      }\n    }\n  }\n  Ok({ values: acc, count: len(acc) })\n}\n";
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(demo);
            m.move_to(13, 12); // inside the nested while block
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
        }
        println!("mui_init_s: MUI_BRACKETS_AUTOOPEN -> nested demo seeded");
    }

    // Screenshot/render hook for the interactive minimap: with MUI_MINIMAP_AUTOOPEN
    // set, seed a TALL buffer and scroll partway down so a headless capture shows
    // the minimap bars + the viewport rectangle over the visible range.
    if std::env::var_os("MUI_MINIMAP_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            use crate::editor::TextModel;
            crate::settings::update(|s| s.minimap = true);
            let mut src = String::from("// minimap.mty \u{2014} a tall file for the minimap viewport.\n");
            for i in 0..160 {
                src.push_str(&format!(
                    "fn unit_{i}(n: I32) -> I32 {{\n  if n < 2 {{\n    n\n  }} else {{\n    unit_{i}(n - 1) + n\n  }}\n}}\n\n"
                ));
            }
            let m = ctx.tabs.active_model_mut();
            *m = TextModel::from_bytes(src.as_bytes());
            m.move_to(300, 4);
            m.set_first_visible(280);
            ctx.welcome.dismiss();
            ctx.edit_probe_lock = true;
            ctx.force_minimap_visible = true;
        }
        println!("mui_init_s: MUI_MINIMAP_AUTOOPEN -> tall demo seeded");
    }

    handle
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
    // Heartbeat + frame-time: log every 60th frame with the avg ms/frame since the
    // last heartbeat so the trace reveals both a frozen loop and real lag (a slow
    // per-frame scene build the vsync'd present would otherwise hide as low fps).
    use std::sync::Mutex;
    use std::time::Instant;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let n = FRAME_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if n == 1 {
        // One-time geometry line so the (external) test harness can compute the
        // exact logical<->physical scale and click logical targets precisely.
        if let Some(c) = unsafe { ctx(handle) } {
            trace(&format!(
                "STARTUP_GEOM logical_w={} logical_h={} phys_w={} phys_h={} scale={:.4}",
                c.gpu.width, c.gpu.height, c.gpu.phys_width, c.gpu.phys_height,
                crate::uiscale::ui_scale()
            ));
        }
    }
    if n % 60 == 0 {
        let now = Instant::now();
        if let Ok(mut g) = LAST.lock() {
            if let Some(prev) = *g {
                let ms = now.duration_since(prev).as_secs_f64() * 1000.0 / 60.0;
                trace(&format!("FRAME {n}  avg {ms:.1}ms/frame ({:.0} fps)", 1000.0 / ms));
            }
            *g = Some(now);
        }
    }
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
    let x = layout::PAD + (col.max(0) as f32) * layout::CHAR_W();
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
/// uses this to size its viewport for cursor-following scroll. Region-aware:
/// the tab bar (top) and prompt+status bands (bottom) are reserved.
#[no_mangle]
pub extern "C" fn mui_visible_rows(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| {
        let region = layout::region(c.sidebar_visible);
        let (_, visible_h) = visible_surface_size(c);
        layout::visible_rows_in(region, visible_h, c.bottom_dock_open()) as i32
    })
}

/// Hit-test the visible resize band at the top of the shared lower dock.
#[no_mangle]
pub extern "C" fn mui_bottom_dock_resize_at_click(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        let (_, visible_h) = visible_surface_size(c);
        if c.bottom_dock_open()
            && c.last_event.button == crate::ffi::MUI_MOUSE_LEFT
            && layout::dock_resize_hit(visible_h, c.last_event.y)
        {
            c.bottom_dock_resizing = true;
            c.bottom_dock_resize_grab_dy = layout::term_panel_top(visible_h) - c.last_event.y;
            trace(&format!(
                "dock_resize start y={:.1} grab_dy={:.1} h={:.1}",
                c.last_event.y,
                c.bottom_dock_resize_grab_dy,
                layout::term_panel_height(visible_h)
            ));
            1
        } else {
            0
        }
    })
}

/// Resize the shared lower dock so its top edge follows the latest mouse event.
/// Returns the resulting panel height in pixels for deterministic tests.
#[no_mangle]
pub extern "C" fn mui_bottom_dock_resize_to_event_y(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        if !c.bottom_dock_open() {
            return 0;
        }
        let (_, visible_h) = visible_surface_size(c);
        let edge_y = c.last_event.y + c.bottom_dock_resize_grab_dy;
        let h = layout::resize_dock_to_y(visible_h, edge_y).round() as i32;
        trace(&format!(
            "dock_resize drag y={:.1} edge_y={edge_y:.1} h={h}",
            c.last_event.y
        ));
        h
    })
}

/// Finish a manual lower-dock resize and acknowledge the resulting height.
#[no_mangle]
pub extern "C" fn mui_bottom_dock_resize_finish(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.bottom_dock_open() {
        return 0;
    }
    let (_, visible_h) = visible_surface_size(ctx);
    let h = layout::term_panel_height(visible_h).round() as i32;
    ctx.bottom_dock_resizing = false;
    ctx.push_toast(crate::toast::Kind::Info, format!("Dock resized to {h}px"));
    trace(&format!("dock_resize finish h={h}"));
    h
}

/// Close whichever shared lower dock is currently open when the latest mouse
/// down lands on the visible close affordance. Returns 1 when it handled.
#[no_mangle]
pub extern "C" fn mui_bottom_dock_close_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.bottom_dock_open() {
        return 0;
    }
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let (x, y, w, h) = layout::dock_close_rect(visible_w, visible_h);
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    if px < x || px > x + w || py < y || py > y + h {
        return 0;
    }
    if close_bottom_dock(ctx) {
        ctx.push_toast(crate::toast::Kind::Info, "Bottom dock closed");
        trace("dock_close_button closed");
        1
    } else {
        0
    }
}

fn close_bottom_dock(ctx: &mut MuiContext) -> bool {
    if !ctx.bottom_dock_open() {
        return false;
    }
    if ctx.term_open {
        ctx.term_open = false;
        ctx.terminal = None;
    }
    if ctx.run.is_active() {
        ctx.run.close();
    }
    if ctx.web.is_active() {
        ctx.web.close();
    }
    if ctx.problems.is_open() {
        ctx.problems.set_open(false);
    }
    ctx.bottom_dock_resizing = false;
    true
}

/// Apply a shared lower-dock size preset from the latest mouse-down.
/// Returns 1 compact, 2 default, 3 expanded, or 0 when no preset was hit.
#[no_mangle]
pub extern "C" fn mui_bottom_dock_preset_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.bottom_dock_open() || ctx.last_event.button != crate::ffi::MUI_MOUSE_LEFT {
        return 0;
    }
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    for idx in 0..3 {
        let (x, y, w, h) = layout::dock_preset_rect(visible_w, visible_h, idx);
        if px >= x && px <= x + w && py >= y && py <= y + h {
            let (frac, label) = match idx {
                0 => (layout::TERM_FRACTION_MIN, "Dock compact"),
                1 => (layout::TERM_FRACTION, "Dock reset"),
                _ => (layout::TERM_FRACTION_MAX, "Dock expanded"),
            };
            layout::set_dock_fraction(frac);
            ctx.bottom_dock_resizing = false;
            ctx.push_toast(crate::toast::Kind::Info, label);
            trace(&format!("dock_preset idx={idx} frac={frac:.2} {label}"));
            return idx as i32 + 1;
        }
    }
    0
}

/// Apply a shared lower-dock command from the palette.
/// `91` = compact, `92` = default, `93` = expanded, `99` = close. Returns the
/// preset number (`1..=3`), `4` for close, or `0` for no-op/unrelated command id.
#[no_mangle]
pub extern "C" fn mui_dock_dispatch(handle: i64, id: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if id as u32 == crate::palette::CMD_DOCK_CLOSE {
        if close_bottom_dock(ctx) {
            ctx.push_toast(crate::toast::Kind::Info, "Bottom dock closed");
            trace(&format!("dock_dispatch id={id} close"));
            return 4;
        }
        ctx.push_toast(crate::toast::Kind::Info, "No bottom dock is open");
        trace(&format!("dock_dispatch id={id} close noop"));
        return 0;
    }
    let (frac, label, code) = match id as u32 {
        crate::palette::CMD_DOCK_COMPACT => (layout::TERM_FRACTION_MIN, "Dock compact", 1),
        crate::palette::CMD_DOCK_RESET => (layout::TERM_FRACTION, "Dock default", 2),
        crate::palette::CMD_DOCK_EXPANDED => (layout::TERM_FRACTION_MAX, "Dock expanded", 3),
        _ => return 0,
    };
    layout::set_dock_fraction(frac);
    ctx.bottom_dock_resizing = false;
    if !ctx.bottom_dock_open() {
        ctx.run.open();
        ctx.term_open = false;
        ctx.web.close();
        ctx.problems.set_open(false);
    }
    ctx.push_toast(crate::toast::Kind::Info, label);
    trace(&format!("dock_dispatch id={id} frac={frac:.2} {label}"));
    code
}

/// Draw the visible grab target for the shared bottom dock. Every lower panel
/// uses the same layout, so drawing it once late keeps Terminal/Run/Web/Problems
/// consistent and prevents the handle from being hidden by panel contents.
#[no_mangle]
pub extern "C" fn mui_bottom_dock_resize_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.bottom_dock_open() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let x0 = layout::term_panel_left(region);
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let w = (visible_w as f32 - x0).max(0.0);
    if w < 80.0 {
        return;
    }
    let top = layout::term_panel_top(visible_h);
    let band_y = top - layout::DOCK_RESIZE_H;
    let was_clip = ctx.clip;
    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    ctx.clip = None;
    ctx.dl_rect(x0, band_y, w, layout::DOCK_RESIZE_H, theme::BG_1());
    ctx.dl_rect(x0, band_y, w, 1.0, theme::BORDER());
    ctx.dl_rect(x0, top - 1.0, w, 1.0, theme::BORDER_STRONG());
    ctx.dl_rect(x0, top, w, 1.0, theme::BG_2());
    let grip_w = 88.0_f32.min((w - 32.0).max(0.0));
    let grip_x = x0 + (w - grip_w) * 0.5;
    let grip_y = band_y + 2.0;
    let grip_bg = if ctx.bottom_dock_resizing {
        theme::accent_a(0.22)
    } else {
        theme::BG_4()
    };
    ctx.dl_shadow(
        grip_x - 8.0,
        grip_y + 2.0,
        grip_w + 16.0,
        12.0,
        6.0,
        theme::SHADOW(),
        10.0,
    );
    ctx.dl_round(
        grip_x - 8.0,
        grip_y,
        grip_w + 16.0,
        12.0,
        6.0,
        grip_bg,
    );
    ctx.dl_stroke(grip_x - 8.0, grip_y, grip_w + 16.0, 12.0, 6.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_round(
        grip_x,
        grip_y + 5.0,
        grip_w,
        2.0,
        1.0,
        if ctx.bottom_dock_resizing { theme::ACCENT() } else { theme::ACCENT_LINE() },
    );
    let dot_y = grip_y + 5.0;
    let dot_color = if ctx.bottom_dock_resizing { theme::ACCENT_BRIGHT() } else { theme::TEXT_3() };
    for dx in [-12.0_f32, 0.0, 12.0] {
        ctx.dl_round(grip_x + grip_w * 0.5 + dx - 1.5, dot_y - 1.5, 3.0, 3.0, 1.5, dot_color);
    }
    let preset_icons = [
        crate::icons::ARROW_DOWN,
        crate::icons::WIN_MIN,
        crate::icons::ARROW_UP,
    ];
    let active_preset = layout::dock_preset_index();
    for (idx, icon) in preset_icons.iter().enumerate() {
        let (px, py, pw, ph) = layout::dock_preset_rect(visible_w, visible_h, idx);
        let is_active = idx == active_preset;
        let bg = if is_active { theme::accent_a(0.20) } else { theme::BG_1() };
        let border = if is_active { theme::ACCENT() } else { theme::BORDER() };
        let icon_col = if is_active { theme::TEXT() } else { theme::TEXT_1() };
        ctx.dl_round(px, py, pw, ph, 5.0, bg);
        ctx.dl_stroke(px, py, pw, ph, 6.0, border, if is_active { 1.5 } else { 1.0 });
        ctx.dl_icon(
            px + (pw - 12.0) * 0.5,
            py + (ph - 12.0) * 0.5,
            12.0,
            12.0,
            *icon,
            icon_col,
            1.5,
            false,
        );
    }
    let (cx, cy, cw, ch) = layout::dock_close_rect(visible_w, visible_h);
    ctx.dl_round(cx, cy, cw, ch, 5.0, theme::BG_1());
    ctx.dl_stroke(cx, cy, cw, ch, 5.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_icon(
        cx + (cw - 12.0) * 0.5,
        cy + (ch - 12.0) * 0.5,
        12.0,
        12.0,
        crate::icons::CLOSE,
        theme::TEXT(),
        1.6,
        false,
    );
    ctx.overlay = was_overlay;
    ctx.clip = was_clip;
}

/// Number of lines in the shim's current `load_buf` (>= 1). Mighty uses this to
/// size the gutter when it draws the buffer via [`mui_draw_buffer_self`].
#[no_mangle]
pub extern "C" fn mui_buf_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| {
        (c.load_buf.iter().filter(|&&b| b == b'\n').count() + 1) as i32
    })
}

/// Draw the editor body — gutter line numbers, source text, and the cursor —
/// directly from the shim's `load_buf` (populated by [`mui_tab_load_into`]).
///
/// This is the rendering counterpart used by the IDE loop. The Mighty side keeps
/// the authoritative edit buffer for editing, but drawing the whole visible
/// window shim-side (one `ctx.text.queue` per line, plus a cursor rect) is both
/// faithful — it issues the SAME GPU rect/text calls — and robust against the
/// v0.36 native-codegen `Vec.push` fragility on the buffer-pull path. `first`
/// is the top visible buffer line; `rows` the visible row count; `cur_line` /
/// `cur_col` the 0-based cursor cell. Colors are fixed to the editor theme.
#[no_mangle]
pub extern "C" fn mui_draw_buffer_self(
    handle: i64,
    first: i32,
    rows: i32,
    cur_line: i32,
    cur_col: i32,
) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let clip = ctx.clip;
    let first = first.max(0) as usize;
    let rows = rows.max(0) as usize;

    // Split the buffer into lines (lossy UTF-8 per line for rendering).
    let src = String::from_utf8_lossy(&ctx.load_buf);
    let lines: Vec<&str> = src.split('\n').collect();
    let total = lines.len().max(1);
    let total_u64 = total as u64;

    let text_x = layout::text_left_in(region, total_u64);
    let gutter_x = region.left + layout::PAD;

    // Theme colors (match the Mighty-side draw_buffer choices).
    let fg = MuiColor::new(0.85, 0.87, 0.9, 1.0);
    let kw = MuiColor::new(0.55, 0.75, 1.0, 1.0); // keywords / leading token
    let gut = MuiColor::new(0.45, 0.48, 0.55, 1.0);

    let last_visible = first + rows;
    for line_idx in first..last_visible {
        if line_idx >= total {
            break;
        }
        let row = (line_idx - first) as i32;
        let y = layout::row_y_in(region, row);
        // Gutter line number (1-based).
        let num = (line_idx + 1).to_string();
        ctx.text.queue(gutter_x, y, &num, gut, clip);
        // Source text. A light syntax cue: color a leading keyword-ish token.
        let text = lines.get(line_idx).copied().unwrap_or("");
        let first_word_end = text
            .char_indices()
            .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_'))
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        let head = &text[..first_word_end];
        const KEYWORDS: &[&str] = &[
            "fn", "let", "mut", "while", "if", "else", "return", "match", "struct", "enum",
            "extern", "effect", "import", "pub", "for", "in", "type", "true", "false",
        ];
        if !head.is_empty() && KEYWORDS.contains(&head) {
            ctx.text.queue(text_x, y, head, kw, clip);
            let rest_x = syntax_rest_x(&mut ctx.text, text_x, head);
            ctx.text.queue(rest_x, y, &text[first_word_end..], fg, clip);
        } else {
            ctx.text.queue(text_x, y, text, fg, clip);
        }
    }

    // Cursor caret, if on a visible row.
    let cl = cur_line.max(0) as usize;
    if cl >= first && cl < last_visible {
        let row = (cl - first) as i32;
        let cx = layout::text_x_in(region, total_u64, cur_col);
        let cy = layout::row_y_in(region, row);
        let handle_ptr = handle as usize as *mut MuiContext;
        unsafe {
            crate::mui_fill_rect(
                handle_ptr,
                cx,
                cy,
                2.0,
                16.0,
                MuiColor::new(0.9, 0.7, 0.2, 1.0),
            );
        }
    }
}

pub(crate) fn syntax_rest_x(text: &mut crate::text::Text, text_x: f32, head: &str) -> f32 {
    text_x + text.measure_sized(head, theme::FONT_SIZE()).0
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
        let region = layout::region(ctx.sidebar_visible);
        let x = layout::text_left_in(region, total_lines.max(1) as u64);
        let y = layout::row_y_in(region, row);
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
        let region = layout::region(ctx.sidebar_visible);
        let x = region.left + layout::PAD;
        let y = layout::row_y_in(region, row);
        let s = std::mem::take(&mut ctx.text_stage);
        let clip = ctx.clip;
        ctx.text.queue(x, y, &s, MuiColor::new(r, g, b, a), clip);
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
    let region = unsafe { ctx(handle) }.map_or(layout::region(false), |c| {
        layout::region(c.sidebar_visible)
    });
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
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
    let region = layout::region(ctx.sidebar_visible);
    let (line, _) = layout::pixel_to_cell_in(
        region,
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
    let region = layout::region(ctx.sidebar_visible);
    let (_, col) = layout::pixel_to_cell_in(
        region,
        ctx.last_event.x,
        ctx.last_event.y,
        0,
        total_lines.max(1) as u64,
    );
    col as i32
}

// ---------------------------------------------------------------------------
// headless / screenshot self-termination cap
// ---------------------------------------------------------------------------

/// Default per-mode frame cap when a headless/screenshot/probe env is set.
/// Overridable with `MUI_HEADLESS_FRAMES=<n>`.
const DEFAULT_HEADLESS_FRAMES: i32 = 240;

/// True when the process is running in a non-interactive (headless / screenshot
/// / probe) mode and the main loop should self-terminate after a frame cap
/// rather than block on a window Close event that will never arrive.
///
/// Detected when ANY of these env vars is set:
///   * `MUI_HEADLESS_FRAMES` (dedicated, also sets the cap value),
///   * `MUI_SCREENSHOT` (offscreen screenshot capture),
///   * any `MUI_*_AUTOOPEN` (screenshot autoopen hooks),
///   * any `MUI_*_PROBE` (scripted headless probes).
///
/// A plain interactive launch sets none of these, so it runs forever until the
/// user closes the window.
pub(crate) fn headless_mode_active() -> bool {
    if std::env::var_os("MUI_HEADLESS_FRAMES").is_some()
        || std::env::var_os("MUI_SCREENSHOT").is_some()
    {
        return true;
    }
    std::env::vars_os().any(|(k, _)| {
        let Some(k) = k.to_str() else { return false };
        k.starts_with("MUI_") && (k.ends_with("_AUTOOPEN") || k.ends_with("_PROBE"))
    })
}

/// The frame cap the IDE main loop should self-terminate at, or `0` to run
/// forever (until a window Close event). Returns a positive cap only when a
/// headless/screenshot/probe env is set (see [`headless_mode_active`]); the
/// value is `MUI_HEADLESS_FRAMES` if a valid positive integer, else
/// [`DEFAULT_HEADLESS_FRAMES`]. A normal interactive run returns `0`.
#[no_mangle]
pub extern "C" fn mui_headless_frames() -> i32 {
    if !headless_mode_active() {
        return 0;
    }
    std::env::var("MUI_HEADLESS_FRAMES")
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_HEADLESS_FRAMES)
}

// ---------------------------------------------------------------------------
// event pump (scalar accessors over the last-polled event)
// ---------------------------------------------------------------------------

/// What the shim's own event interception decided to do with a popped event.
enum ShimAction {
    /// The shim consumed the event entirely (window drag/resize/min/max, or a
    /// zoom gesture). The poll loop should drop it and pop the next one so the
    /// IDE (main.mty) never sees it.
    Consume,
    /// Replace the event with this one before handing it to the IDE (used to turn
    /// a title-bar Close-button press into a normal `MUI_EVENT_CLOSE`, which the
    /// IDE's existing close path already handles).
    Replace(MuiEvent),
    /// Not a window-chrome / zoom event — hand it to the IDE unchanged.
    PassThrough,
}

/// Append a line to the trace file named by the `MUI_TRACE` env var, if set.
/// Used by the Windows UI harness to see exactly what input the live event loop
/// receives and how the shim classifies it (clicks, keys, drag/zoom intercepts,
/// frame heartbeat) — the offscreen render tests could not observe any of this.
pub(crate) fn trace(msg: &str) {
    use std::io::Write;
    use std::sync::OnceLock;
    // Resolve MUI_TRACE once: when unset (the normal case) this is a cheap cached
    // `None` check, so the trace calls scattered through the hot event/frame paths
    // cost nothing in a normal run.
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var("MUI_TRACE").ok()) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

fn trace_event(ev: &MuiEvent, action: &str) {
    trace(&format!(
        "EV tag={} btn={} x={:.1} y={:.1} mods={} cp={} key={} scrolly={:.1} -> {}",
        ev.tag, ev.button, ev.x, ev.y, ev.mods, ev.codepoint, ev.key, ev.scroll_y, action
    ));
}

/// Monotonic frame counter for the trace heartbeat (detects a frozen render loop
/// even when the OS message pump still answers — i.e. a logical, not Win32, hang).
static FRAME_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The custom (borderless) title bar + UI zoom live ENTIRELY shim-side: the IDE
/// (main.mty) is unaware of them. This intercepts events as they are popped, so
/// `main.mty`'s dispatch ladder never gains the extra nesting that overflowed the
/// v0.36 recursive-descent parser (L37). Decides what to do with `ev` given the
/// context `ctx`; performs the OS window action / zoom side effects inline.
fn shim_intercept(ctx: &mut MuiContext, ev: &MuiEvent) -> ShimAction {
    match ev.tag {
        // Drag-move events are only meaningful after Mighty captures a visible
        // drag target. Ordinary hover movement and unrelated OS/titlebar drags
        // are consumed here so click-focused routing stays deterministic.
        MUI_EVENT_MOUSE_MOVE if !ctx.bottom_dock_resizing && !ctx.sidebar_resizing => {
            update_hover_cursor(ctx, ev);
            if term_wants_mouse_motion_at(ctx, ev.x, ev.y) {
                ShimAction::PassThrough
            } else {
                ShimAction::Consume
            }
        }
        MUI_EVENT_MOUSE_UP if ctx.bottom_dock_resizing || ctx.sidebar_resizing => {
            ctx.bottom_dock_resizing = false;
            ctx.sidebar_resizing = false;
            ShimAction::PassThrough
        }
        // --- Mouse press: title-bar controls / drag strip / resize edges win
        // over the IDE's normal click routing on a borderless window. Coords are
        // already LOGICAL px (CursorMoved applied `phys_to_logical`). ---
        MUI_EVENT_MOUSE_DOWN if ev.button == MUI_MOUSE_LEFT => {
            let w = ctx.gpu.width as f32;
            let h = ctx.gpu.height as f32;
            let body_left = titlebar_body_left(ctx);
            // PRIORITY: interactive title-bar BUTTONS (min/max/close) win over the
            // resize edges — otherwise the enlarged top-right corner-resize zone
            // swallows the close button. Resize edges then win over the drag strip
            // (so you can grab the top edge to resize); drag is last.
            let hit = crate::titlebar::hit(ev.x, ev.y, w, body_left);
            if ctx.ai.open {
                let visible_w = visible_surface_size(ctx).0;
                let (close_x, close_y, close_w, close_h) = crate::ai::close_geometry(visible_w);
                if ev.x >= close_x
                    && ev.x <= close_x + close_w
                    && ev.y >= close_y
                    && ev.y <= close_y + close_h
                {
                    return ShimAction::PassThrough;
                }
            }
            match hit {
                Some(crate::titlebar::TitleHit::Minimize) => {
                    if let Some(host) = ctx.host.as_ref() {
                        host.minimize();
                    }
                    return ShimAction::Consume;
                }
                Some(crate::titlebar::TitleHit::Maximize) => {
                    let now = ctx.host.as_ref().is_some_and(|h| h.toggle_maximize());
                    ctx.window_maximized = now;
                    return ShimAction::Consume;
                }
                Some(crate::titlebar::TitleHit::Close) => {
                    return ShimAction::Replace(MuiEvent::close());
                }
                _ => {}
            }
            // Resize edges/corners next.
            if rail_utility_hit(ev.x, ev.y, h) > 0 {
                return ShimAction::PassThrough;
            }
            if tab_index_at_point(ctx, ev.x, ev.y).is_some() {
                return ShimAction::PassThrough;
            }
            let rc = crate::titlebar::resize_code(ev.x, ev.y, w, h);
            if rc > 0 {
                if let (Some(dir), Some(host)) =
                    (crate::window::ResizeDir::from_code(rc), ctx.host.as_ref())
                {
                    trace(&format!("window_resize code={rc}"));
                    host.drag_resize(dir);
                }
                return ShimAction::Consume;
            }
            // Drag strip last (caption row / rail header, not over a tab).
            if hit == Some(crate::titlebar::TitleHit::Drag) {
                let tab_end = (body_left + ctx.tabs.count() as f32 * layout::TAB_W)
                    .min(crate::titlebar::controls_x(w) - crate::titlebar::ACTION_STRIP_W);
                if ev.x >= body_left && ev.x < tab_end {
                    return ShimAction::PassThrough;
                }
                if topbar_command_center_hit(ctx, ev.x, ev.y) {
                    return ShimAction::PassThrough;
                }
                if let Some(host) = ctx.host.as_ref() {
                    host.drag();
                }
                return ShimAction::Consume;
            }
            ShimAction::PassThrough
        }
        // --- Ctrl+wheel zooms the whole UI; a plain wheel passes through to the
        // IDE as a normal scroll. ---
        MUI_EVENT_SCROLL if (ev.mods & MUI_MOD_CTRL) != 0 => {
            if term_wants_mouse_reporting_at(ctx, ev.x, ev.y) {
                return ShimAction::PassThrough;
            }
            if ev.scroll_y > 0.0 {
                let _ = mui_zoom_in(ctx as *mut MuiContext as i64);
                ShimAction::Consume
            } else if ev.scroll_y < 0.0 {
                let _ = mui_zoom_out(ctx as *mut MuiContext as i64);
                ShimAction::Consume
            } else {
                ShimAction::PassThrough
            }
        }
        MUI_EVENT_SCROLL if tab_bar_contains_point(ctx, ev.x, ev.y) => {
            let dir = if ev.scroll_y < 0.0 { 1 } else if ev.scroll_y > 0.0 { -1 } else { 0 };
            let _ = tab_scroll_by(ctx, dir);
            ShimAction::Consume
        }
        // --- Ctrl+= / Ctrl++ / Ctrl+- / Ctrl+0 zoom (and are NOT emitted as text
        // into the editor). ---
        MUI_EVENT_CHAR if (ev.mods & MUI_MOD_CTRL) != 0 => {
            let handle = ctx as *mut MuiContext as i64;
            match ev.codepoint {
                // '=' or '+'
                61 | 43 => {
                    let _ = mui_zoom_in(handle);
                    ShimAction::Consume
                }
                // '-'
                45 => {
                    let _ = mui_zoom_out(handle);
                    ShimAction::Consume
                }
                // '0'
                48 => {
                    let _ = mui_zoom_reset(handle);
                    ShimAction::Consume
                }
                _ => ShimAction::PassThrough,
            }
        }
        _ => ShimAction::PassThrough,
    }
}

fn update_hover_cursor(ctx: &mut MuiContext, ev: &MuiEvent) {
    let Some(host) = ctx.host.as_ref() else {
        return;
    };
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    if rail_utility_hit(ev.x, ev.y, h) > 0 {
        host.set_cursor_default();
        return;
    }
    let rc = crate::titlebar::resize_code(ev.x, ev.y, w, h);
    if let Some(dir) = crate::window::ResizeDir::from_code(rc) {
        host.set_cursor_resize(dir);
    } else if layout::sidebar_resize_hit(ctx.sidebar_visible, ev.x, ev.y, visible_surface_size(ctx).1) {
        host.set_cursor_col_resize();
    } else if ctx.bottom_dock_open() && layout::dock_resize_hit(visible_surface_size(ctx).1, ev.y) {
        host.set_cursor_row_resize();
    } else {
        host.set_cursor_default();
    }
}

/// Pump + pop one event, storing it as the "current" event for the scalar
/// accessors below. Returns the event tag (`MUI_EVENT_*`), or `0` when the
/// queue is empty.
///
/// Window-chrome presses (custom title bar: drag / minimize / maximize / resize
/// edges) and UI-zoom gestures (Ctrl+=/-/0, Ctrl+wheel) are intercepted HERE,
/// shim-side, and never surface to the IDE — so `main.mty` needs no window/zoom
/// code (and its dispatch ladder stays under the v0.36 parser's nesting ceiling).
#[no_mangle]
pub extern "C" fn mui_poll_event_s(handle: i64) -> i32 {
    if unsafe { ctx(handle) }.is_none() {
        return 0;
    }
    loop {
        let mut ev = MuiEvent::none();
        let got = unsafe {
            crate::mui_poll_event(handle as usize as *mut MuiContext, &mut ev as *mut MuiEvent)
        };
        // Borrow fresh AFTER the raw-handle pump call above (which can't coexist
        // with a live `&mut MuiContext`).
        let Some(c) = (unsafe { ctx(handle) }) else {
            return 0;
        };
        if !got {
            c.last_event = MuiEvent::none();
            return 0;
        }
        match shim_intercept(c, &ev) {
            ShimAction::Consume => {
                trace_event(&ev, "consume");
                continue;
            }
            ShimAction::Replace(rep) => {
                trace_event(&ev, "replace->close");
                c.last_event = rep;
                return rep.tag as i32;
            }
            ShimAction::PassThrough => {
                trace_event(&ev, "passthrough");
                c.last_event = ev;
                return ev.tag as i32;
            }
        }
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

/// Sign of the last scroll event's vertical delta WITH the Ctrl modifier held
/// (zoom gesture): `+1` (wheel up → zoom in), `-1` (wheel down → zoom out), `0`
/// otherwise. The IDE checks this first so Ctrl+wheel zooms instead of scrolls.
#[no_mangle]
pub extern "C" fn mui_event_zoom_dir(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        if c.last_event.tag != MUI_EVENT_SCROLL || (c.last_event.mods & MUI_MOD_CTRL) == 0 {
            return 0;
        }
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

// ---------------------------------------------------------------------------
// UI zoom (Ctrl+= / Ctrl+- / Ctrl+0, Ctrl+wheel). The factor is `os_scale ×
// user_zoom`; these adjust `user_zoom`, persist it, and recompute the logical
// surface size + projection so the next frame reflows at the new scale.
// ---------------------------------------------------------------------------

fn apply_zoom(handle: i64, new_zoom: f32) -> i32 {
    crate::uiscale::set_user_zoom(new_zoom);
    crate::config::save_zoom(crate::uiscale::user_zoom());
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.gpu.rescale();
    }
    (crate::uiscale::user_zoom() * 100.0).round() as i32
}

/// Zoom in one step. Returns the new zoom percent (e.g. 110).
#[no_mangle]
pub extern "C" fn mui_zoom_in(handle: i64) -> i32 {
    let z = crate::uiscale::clamp_zoom(crate::uiscale::user_zoom() + crate::uiscale::ZOOM_STEP);
    apply_zoom(handle, z)
}

/// Zoom out one step. Returns the new zoom percent.
#[no_mangle]
pub extern "C" fn mui_zoom_out(handle: i64) -> i32 {
    let z = crate::uiscale::clamp_zoom(crate::uiscale::user_zoom() - crate::uiscale::ZOOM_STEP);
    apply_zoom(handle, z)
}

/// Reset the user zoom to 100%. Returns 100.
#[no_mangle]
pub extern "C" fn mui_zoom_reset(handle: i64) -> i32 {
    apply_zoom(handle, 1.0)
}

// ---------------------------------------------------------------------------
// Custom (borderless) window title bar: hit-test the controls / drag strip /
// resize edges, and drive the OS window actions (drag / minimize / maximize /
// close). All hit-testing uses the LAST mouse position (logical px), matching
// every other `*_at_click` entry point.
// ---------------------------------------------------------------------------

fn titlebar_body_left(ctx: &MuiContext) -> f32 {
    layout::body_left(ctx.sidebar_visible)
}

/// Hit-test the last click against the title bar. Returns: 0 = none, 1 = drag
/// strip, 2 = minimize, 3 = maximize/restore, 4 = close.
#[no_mangle]
pub extern "C" fn mui_titlebar_hit_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    use crate::titlebar::TitleHit;
    let w = ctx.gpu.width as f32;
    let body_left = titlebar_body_left(ctx);
    match crate::titlebar::hit(ctx.last_event.x, ctx.last_event.y, w, body_left) {
        Some(TitleHit::Drag) => 1,
        Some(TitleHit::Minimize) => 2,
        Some(TitleHit::Maximize) => 3,
        Some(TitleHit::Close) => 4,
        None => 0,
    }
}

/// Resize-edge hit code for the last mouse position (1..=8 per
/// `crate::window::ResizeDir::from_code`), or 0 when not on an edge.
#[no_mangle]
pub extern "C" fn mui_window_resize_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    if rail_utility_hit(ctx.last_event.x, ctx.last_event.y, h) > 0 {
        return 0;
    }
    crate::titlebar::resize_code(ctx.last_event.x, ctx.last_event.y, w, h)
}

/// Begin an OS window drag (call when the drag strip is pressed).
#[no_mangle]
pub extern "C" fn mui_window_drag(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(host) = ctx.host.as_ref() {
            host.drag();
        }
    }
}

/// Begin an OS resize drag in direction `code` (1..=8). No-op for 0/unknown.
#[no_mangle]
pub extern "C" fn mui_window_resize(handle: i64, code: i32) {
    if let Some(dir) = crate::window::ResizeDir::from_code(code) {
        if let Some(ctx) = unsafe { ctx(handle) } {
            trace(&format!("window_resize code={code}"));
            if let Some(host) = ctx.host.as_ref() {
                host.drag_resize(dir);
            }
        }
    }
}

/// Minimize the window.
#[no_mangle]
pub extern "C" fn mui_window_minimize(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(host) = ctx.host.as_ref() {
            host.minimize();
        }
        ctx.push_toast(crate::toast::Kind::Info, "Window minimized");
        trace("window_minimize");
    }
}

/// Toggle maximize / restore. Returns 1 when now maximized, else 0.
#[no_mangle]
pub extern "C" fn mui_window_toggle_maximize(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let now = match ctx.host.as_ref() {
        Some(host) => host.toggle_maximize(),
        None => false,
    };
    ctx.window_maximized = now;
    let label = if now { "Window maximized" } else { "Window restored" };
    ctx.push_toast(crate::toast::Kind::Info, label);
    trace(&format!("window_toggle_maximize now={}", if now { 1 } else { 0 }));
    if now {
        1
    } else {
        0
    }
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
        let pb = PathBuf::from(s);
        ctx.language = crate::langdetect::detect_path(&pb);
        ctx.file_path = Some(pb);
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
    if ctx.tabs.any_dirty_path(&path) {
        eprintln!("mui_save_commit({}): skipped dirty open tab", path.display());
        return -1;
    }
    let resurrected_path = !path.is_file();
    match std::fs::write(&path, &ctx.save_buf) {
        Ok(()) => {
            let _ = ctx.tabs.reload_all_clean_path(&path, &ctx.save_buf);
            if resurrected_path {
                record_recent_file(ctx, path.clone());
                refresh_workspace_file_views(ctx);
            }
            0
        }
        Err(e) => {
            eprintln!("mui_save_commit({}): {e}", path.display());
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Save", &path, &e),
            );
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// live diagnostics (scalar getters over the parsed diagnostic result)
// ---------------------------------------------------------------------------

/// Refresh diagnostics on the currently-configured file path, store the result
/// in the context, and return the diagnostic count. Returns `0` (and clears the
/// stored set) if there is no configured path or the handle is null.
///
/// Mighty files use saved-file `mty check`; generic LSP-backed languages use
/// the active tab's live text when available so unsaved edits can surface
/// diagnostics without waiting for a save.
#[no_mangle]
pub extern "C" fn mui_diag_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.file_path.clone() else {
        ctx.diags.clear();
        return 0;
    };
    // Mighty keeps using `mty check`; other languages surface their language
    // server's publishDiagnostics (only when a server is installed). Either path
    // is best-effort — failure/no-server yields an empty set, never a crash.
    if ctx.language == Language::Mighty {
        ctx.diags = diagnostics::run_check(&path);
    } else if let Some(spec) = crate::lspregistry::server_for(ctx.language) {
        let source = if ctx.tabs.active_path().as_ref() == Some(&path) {
            ctx.tabs.active_model().as_text()
        } else {
            std::fs::read_to_string(&path).unwrap_or_default()
        };
        let root = workspace_root(&path);
        ctx.diags = crate::lspclient::diagnostics(&spec, ctx.language.lsp_id(), &root, &path, &source);
    } else {
        ctx.diags.clear();
    }
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
    let Some(ctx) = (unsafe { ctx(handle) }) else { return };
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col_start);
    let cells = (col_end - col_start).max(1) as f32;
    let w = cells * layout::CHAR_W();
    // Sit the wavy squiggle near the bottom of the row's line box.
    let y = layout::row_y_in(region, row) + layout::LINE_H() - 4.0;
    ctx.dl_squiggle(x, y, w, MuiColor::new(r, g, b, a));
}

/// Draw a diagnostic marker in the gutter at screen `row` (a small square at the
/// left padding). Used to flag a row that has a diagnostic even when its span is
/// off to the side.
#[no_mangle]
pub extern "C" fn mui_diag_gutter_mark(handle: i64, row: i32, r: f32, g: f32, b: f32, a: f32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else { return };
    let region = layout::region(ctx.sidebar_visible);
    // A small rounded dot in the gutter flagging the diagnostic row.
    let cy = layout::row_y_in(region, row) + layout::LINE_H() * 0.5 - 3.0;
    ctx.dl_round(region.left + 3.0, cy, 6.0, 6.0, 3.0, MuiColor::new(r, g, b, a));
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
    let bar_h = layout::LINE_H();
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
        let y = (h - layout::LINE_H() + 1.0).max(0.0);
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

    // Full-width elevated band + a thin top divider.
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = 30.0_f32;
    let y = (h - bar_h).max(0.0);
    let chrome = theme::CHROME_FONT_SIZE - 1.0;
    let clip = ctx.clip;

    use crate::icons;
    // Status band (mockup linear-gradient near-black) + a thin top divider.
    ctx.dl_grad_v(0.0, y, w, bar_h, 0.0, theme::STATUS_TOP(), theme::STATUS_BOTTOM());
    ctx.dl_rect(0.0, y, w, 1.0, theme::BORDER());
    let ty = y + (bar_h - chrome) * 0.5 - 1.0;
    let icon_y = y + (bar_h - 13.0) * 0.5;

    let (line1, col1) = ctx.status_cursor;

    // ---- right cluster (laid out right-to-left) ----
    let (grip_x, grip_y, grip_w, grip_h) = status_resize_grip_rect(w, h);
    ctx.dl_round(grip_x - 5.0, grip_y - 5.0, grip_w + 10.0, grip_h + 10.0, 6.0, theme::accent_a(0.035));
    ctx.dl_stroke(grip_x - 5.0, grip_y - 5.0, grip_w + 10.0, grip_h + 10.0, 6.0, theme::BORDER_SOFT(), 1.0);
    ctx.dl_icon(grip_x, grip_y, grip_w, grip_h, icons::RESIZE_GRIP, theme::TEXT_3(), 1.5, false);
    let mut rx = grip_x - 10.0;

    // Bell (notifications), kept left of the always-visible resize grip.
    rx -= 16.0;
    ctx.dl_icon(rx, icon_y - 0.5, 14.0, 14.0, icons::BELL, theme::DIM(), 1.5, false);
    rx -= 10.0;

    // Language pill (detected from the active file) with an indigo gradient + an
    // M glyph. Falls back to "Mighty" only when the active file is Mighty.
    let lang = ctx.language.display_name();
    let (lang_w, _) = ctx.text.measure_ui_sized(lang, chrome - 1.5);
    let pill_w = lang_w + 30.0;
    let pill_h = 19.0;
    rx -= pill_w;
    let py = y + (bar_h - pill_h) * 0.5;
    ctx.dl_grad_v(rx, py, pill_w, pill_h, 6.0, theme::accent_a(0.22), theme::accent_a(0.10));
    ctx.dl_stroke(rx, py, pill_w, pill_h, 6.0, theme::ACCENT_LINE(), 1.0);
    ctx.dl_icon(rx + 8.0, py + (pill_h - 11.0) * 0.5, 11.0, 11.0, icons::LANG_M_FILL, theme::ACCENT_BRIGHT(), 0.0, true);
    ctx.text.queue_ui_sized(rx + 22.0, ty + 0.5, lang, theme::ACCENT_BRIGHT(), chrome - 1.5, clip);
    rx -= 12.0;

    // "UTF-8".
    let enc = "UTF-8";
    let (enc_w, _) = ctx.text.measure_ui_sized(enc, chrome);
    rx -= enc_w;
    ctx.text.queue_ui_sized(rx, ty, enc, theme::DIM(), chrome, clip);
    rx -= 14.0;

    // "Spaces: 2".
    let sp = "Spaces: 2";
    let (sp_w, _) = ctx.text.measure_ui_sized(sp, chrome);
    rx -= sp_w;
    ctx.text.queue_ui_sized(rx, ty, sp, theme::DIM(), chrome, clip);
    rx -= 14.0;

    // "Ln L, Col C".
    let lc = format!("Ln {line1}, Col {col1}");
    let (lc_w, _) = ctx.text.measure_ui_sized(&lc, chrome);
    rx -= lc_w;
    ctx.text.queue_ui_sized(rx, ty, &lc, theme::DIM(), chrome, clip);
    let left_limit = (rx - 14.0).max(0.0);

    // ---- left cluster: branch icon + branch ↑N ↓M · problems (err/warn) ----
    // Use the live SCM status when a repo was discovered; else a neutral default.
    let branch = if ctx.scm.status.branch.is_empty() {
        "main".to_string()
    } else {
        ctx.scm.status.branch.clone()
    };
    let ab = format!("\u{2191}{} \u{2193}{}", ctx.scm.status.ahead.max(0), ctx.scm.status.behind.max(0));
    let mut x = 10.0;
    ctx.dl_icon(x, icon_y, 13.0, 13.0, icons::BRANCH, theme::TEXT_1(), 1.5, false);
    x += 18.0;

    // Errors (red circle + N) and warnings (warn triangle + N). Prefer the
    // aggregated Problems counts when the Problems panel has run; otherwise fall
    // back to the per-file `error_count` the caller passed (active-file diags).
    let agg = ctx.problems.count() > 0 || ctx.problems.is_open();
    let n_err = if agg { ctx.problems.error_count() } else { error_count.max(0) };
    let n_warn = if agg { ctx.problems.warn_count() } else { 0 };
    let err = n_err.to_string();
    let warn = n_warn.to_string();
    let err_suffix = " err";
    let warn_suffix = " warn";
    let (ab_w, _) = ctx.text.measure_ui_sized(&ab, chrome);
    let (err_w, _) = ctx.text.measure_ui_sized(&err, chrome);
    let (warn_w, _) = ctx.text.measure_ui_sized(&warn, chrome);
    let (err_suffix_w, _) = ctx.text.measure_ui_sized(err_suffix, chrome);
    let (warn_suffix_w, _) = ctx.text.measure_ui_sized(warn_suffix, chrome);
    let compact_problems_w = 16.0 + err_w + 10.0 + 16.0 + warn_w;
    let labeled_problems_w =
        16.0 + err_w + err_suffix_w + 10.0 + 16.0 + warn_w + warn_suffix_w;
    let available_left = (left_limit - x).max(0.0);
    let use_labeled_problems = 6.0 + ab_w + 12.0 + labeled_problems_w <= available_left;
    let problems_w = if use_labeled_problems {
        labeled_problems_w
    } else {
        compact_problems_w
    };
    let suffix_w = 6.0 + ab_w + 12.0 + problems_w;
    let branch_budget = (left_limit - x - suffix_w).max(0.0);
    let branch = fit_status_tail(&mut ctx.text, &branch, branch_budget, chrome);
    let (branch_w, _) = ctx.text.measure_ui_sized(&branch, chrome);
    if !branch.is_empty() {
        ctx.text.queue_ui_sized(x, ty, &branch, theme::TEXT_1(), chrome, clip);
        x += branch_w + 6.0;
    }
    if x + ab_w + 12.0 + problems_w <= left_limit {
        ctx.text.queue_ui_sized(x, ty, &ab, theme::TEXT_3(), chrome, clip);
        x += ab_w + 12.0;
    }

    let chip_x = x;
    if x + problems_w <= left_limit {
        ctx.dl_icon(x, icon_y, 13.0, 13.0, icons::ERROR_CIRCLE, theme::ERROR(), 1.5, false);
        x += 16.0;
        ctx.text.queue_ui_sized(
            x,
            ty,
            &err,
            if n_err > 0 { theme::ERROR() } else { theme::TEXT_1() },
            chrome,
            clip,
        );
        x += err_w;
        if use_labeled_problems {
            ctx.text.queue_ui_sized(x, ty, err_suffix, theme::TEXT_3(), chrome, clip);
            x += err_suffix_w;
        }
        x += 10.0;
        ctx.dl_icon(x, icon_y, 13.0, 13.0, icons::WARN_TRI, theme::WARNING(), 1.5, false);
        x += 16.0;
        ctx.text.queue_ui_sized(
            x,
            ty,
            &warn,
            if n_warn > 0 { theme::WARNING() } else { theme::TEXT_1() },
            chrome,
            clip,
        );
        x += warn_w;
        if use_labeled_problems {
            ctx.text.queue_ui_sized(x, ty, warn_suffix, theme::TEXT_3(), chrome, clip);
            x += warn_suffix_w;
        }
        ctx.status_problems_rect = Some((chip_x - 4.0, y, (x - chip_x) + 8.0, bar_h));
    } else {
        ctx.status_problems_rect = None;
    }
}

pub(crate) fn status_resize_grip_rect(width: f32, height: f32) -> (f32, f32, f32, f32) {
    let size = 16.0_f32;
    ((width - size - 8.0).max(0.0), (height - size - 7.0).max(0.0), size, size)
}

pub(crate) fn gutter_number_width(text: &mut crate::text::Text, num: &str, size: f32) -> f32 {
    text.measure_sized(num, size).0
}

pub(crate) fn folded_indicator_width(text: &mut crate::text::Text, label: &str, size: f32) -> f32 {
    text.measure_ui_sized(label, size).0 + 12.0
}

fn fit_status_tail(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
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

fn fit_status_head(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
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
        let candidate: String = chars[..mid]
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
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
        chars[..lo]
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
            .collect()
    }
}

pub(crate) fn fit_tab_label(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if max_px <= 0.0 {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    let ellipsis_w = text.measure_ui_sized(ellipsis, size).0;
    if ellipsis_w > max_px {
        return String::new();
    }

    let chars: Vec<char> = s.chars().collect();
    let dot_idx = chars
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(i, ch)| (*ch == '.').then_some(i))
        .last();
    let Some(dot_idx) = dot_idx else {
        return fit_status_head(text, s, max_px, size);
    };

    let suffix_start = dot_idx + 1;
    let suffix: String = chars[suffix_start..].iter().collect();
    let suffix_w = text.measure_ui_sized(&suffix, size).0;
    if suffix_w + ellipsis_w >= max_px * 0.72 {
        return fit_status_head(text, s, max_px, size);
    }

    let stem_len = dot_idx;
    let mut lo = 0usize;
    let mut hi = stem_len;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid]
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
            .chain(chars[suffix_start..].iter().copied())
            .collect();
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }

    if lo == 0 {
        fit_status_head(text, s, max_px, size)
    } else {
        chars[..lo]
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
            .chain(chars[suffix_start..].iter().copied())
            .collect()
    }
}

/// `1` if the last click landed on the status-bar problems chip (the
/// error/warning counters in the left cluster), else `0`. Lets Mighty open the
/// Problems panel when the chip is clicked. The chip's x position follows the
/// rendered branch/ahead/behind text, so hit-test against the last drawn rect.
#[no_mangle]
pub extern "C" fn mui_status_problems_chip_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some((x, y, w, h)) = ctx.status_problems_rect else {
        return 0;
    };
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    if py < y || py > y + h {
        return 0;
    }
    if px >= x && px <= x + w {
        1
    } else {
        0
    }
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
        trace(&format!("prompt_open kind={kind}"));
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
pub extern "C" fn mui_prompt_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.prompt.is_active() {
        ctx.prompt.cancel();
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No prompt input open");
        0
    }
}

/// `1` if a prompt is currently active, else `0`.
#[no_mangle]
pub extern "C" fn mui_prompt_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.prompt.is_active() { 1 } else { 0 })
}

/// `1` when the last mouse position is inside the visible bottom prompt band.
/// Mighty uses this to let outside clicks dismiss prompt fallbacks cleanly.
#[no_mangle]
pub extern "C" fn mui_prompt_hit_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.prompt.is_active() {
        return 0;
    }
    let (_x, y, w, h) = prompt_band_rect(ctx);
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    if px >= 0.0 && px <= w && py >= y && py <= y + h {
        1
    } else {
        0
    }
}

/// `1` when the latest mouse-down hit the visible close button on the bottom
/// prompt. Mighty uses this before the generic prompt-band hit-test so the X
/// affordance is not just decorative.
#[no_mangle]
pub extern "C" fn mui_prompt_close_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.prompt.is_active()
        || ctx.last_event.tag != crate::ffi::MUI_EVENT_MOUSE_DOWN
        || ctx.last_event.button != crate::ffi::MUI_MOUSE_LEFT
    {
        return 0;
    }
    let (_x, y, w, h) = prompt_band_rect(ctx);
    let (cx, cy, cw, ch) = prompt_close_rect(w, y, h);
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    i32::from(px >= cx && px <= cx + cw && py >= cy && py <= cy + ch)
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

fn fit_prompt_tail(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
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
        let tail = crate::prompt::tail_ellipsize_chars(s, mid + 1);
        if text.measure_ui_sized(&tail, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return ellipsis.to_string();
    }
    crate::prompt::tail_ellipsize_chars(s, lo + 1)
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
    let (_x, y, w, bar_h) = prompt_band_rect(ctx);
    let chrome = theme::CHROME_FONT_SIZE;
    let label = prompt_draw_label(ctx);
    let query = ctx.prompt.query_string();
    let text_y = y + (bar_h - chrome) * 0.5 - 1.0;
    let clip = ctx.clip;
    let handle_ptr = handle as usize as *mut MuiContext;
    let text_x = layout::region(ctx.sidebar_visible).left + layout::PAD + 12.0;
    let (close_x, close_y, close_w, close_h) = prompt_close_rect(w, y, bar_h);
    let hint = "Enter / Esc";
    let (hint_w, _) = ctx.text.measure_ui_sized(hint, 11.0);
    let hint_x = close_x - hint_w - 12.0;
    let show_hint = hint_x > text_x + 180.0;
    let max_right = if show_hint { hint_x - 14.0 } else { close_x - 10.0 };
    let max_w = (max_right - text_x).max(0.0);
    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    unsafe {
        // Elevated band + top divider + an ember accent bar on the left edge.
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, bar_h, theme::ELEVATED());
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, 1.0, theme::BORDER());
        crate::mui_fill_rect(handle_ptr, layout::region(ctx.sidebar_visible).left, y, 3.0, bar_h, theme::EMBER());
    }
    if show_hint {
        ctx.text.queue_ui_sized(hint_x, text_y + 1.0, hint, theme::TEXT_3(), 11.0, clip);
    }
    ctx.dl_round(close_x, close_y, close_w, close_h, 6.0, theme::BG_4());
    ctx.dl_icon(
        close_x + 5.0,
        close_y + 5.0,
        close_w - 10.0,
        close_h - 10.0,
        crate::icons::CLOSE,
        theme::TEXT_1(),
        1.6,
        false,
    );
    if max_w <= 1.0 {
        ctx.text.set_overlay(false);
        ctx.overlay = was_overlay;
        return;
    }
    let (label_w, _) = ctx.text.measure_ui_sized(&label, chrome);
    if label_w >= max_w {
        let label = fit_prompt_tail(&mut ctx.text, &label, max_w, chrome);
        if !label.is_empty() {
            ctx.text.queue_sized(text_x, text_y, &label, theme::TEXT_3(), chrome, clip);
        }
        ctx.text.set_overlay(false);
        ctx.overlay = was_overlay;
        return;
    }
    ctx.text.queue_sized(text_x, text_y, &label, theme::TEXT_3(), chrome, clip);
    let qx = text_x + label_w;
    let query = fit_prompt_tail(&mut ctx.text, &query, max_right - qx, chrome);
    if !query.is_empty() {
        ctx.text.queue_sized(qx, text_y, &query, theme::TEXT(), chrome, clip);
    }
    ctx.text.set_overlay(false);
    ctx.overlay = was_overlay;
}

pub(crate) fn prompt_draw_label(ctx: &MuiContext) -> String {
    let label = ctx.prompt.label();
    if ctx.prompt.kind() == Some(crate::prompt::PromptKind::DeleteFile) {
        if let Some(path) = ctx.tabs.active_path() {
            return format!("Delete {}, type name: ", basename(&path));
        }
    }
    label.to_string()
}

fn prompt_band_rect(ctx: &MuiContext) -> (f32, f32, f32, f32) {
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let w = visible_w as f32;
    let h = visible_h as f32;
    let bar_h = layout::LINE_H();
    // Sit the prompt band one row above the status bar.
    let status_h = 30.0_f32;
    (0.0, (h - status_h - bar_h).max(0.0), w, bar_h)
}

fn prompt_close_rect(w: f32, y: f32, bar_h: f32) -> (f32, f32, f32, f32) {
    let size = (bar_h - 6.0).clamp(18.0, 24.0);
    let x = (w - size - 8.0).max(0.0);
    let cy = y + (bar_h - size) * 0.5;
    (x, cy, size, size)
}

// ---------------------------------------------------------------------------
// Feature 3 — go-to-line: parse the goto query
// ---------------------------------------------------------------------------

/// Parse the active prompt's query as a 1-based line number, or `-1` if the
/// query is empty / not all digits / overflows. Mighty calls this on Enter.
#[no_mangle]
pub extern "C" fn mui_prompt_goto_target(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let target = ctx.prompt.goto_target();
    if target < 1 {
        ctx.push_toast(crate::toast::Kind::Info, "Enter a line number");
    }
    target
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
    let region = unsafe { ctx(handle) }.map_or(layout::region(false), |c| {
        layout::region(c.sidebar_visible)
    });
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col_start);
    let cells = len.max(1) as f32;
    let w = cells * layout::CHAR_W();
    let y = layout::row_y_in(region, row) - 2.0;
    unsafe {
        crate::mui_fill_rect(
            handle as usize as *mut MuiContext,
            x,
            y,
            w,
            layout::LINE_H(),
            theme::FIND_HIGHLIGHT(),
        )
    };
}

// ---------------------------------------------------------------------------
// Multi-file workspace — tab store
// ---------------------------------------------------------------------------

/// Point the shim's file I/O and transient active-file UI at the active tab's
/// path, then update the status-bar basename. Called internally after any tab
/// open/switch/close so Ctrl+S, language popups, and `mty check` follow the
/// active file.
pub(crate) fn sync_active_path(ctx: &mut MuiContext) {
    let active = ctx.tabs.active();
    let path = ctx.tabs.path(active);
    ctx.diags.clear();
    ctx.find.reset();
    ctx.hover.clear();
    ctx.def.clear();
    ctx.sig.clear();
    ctx.complete.cancel();
    ctx.codeaction.cancel();
    ctx.rename.cancel();
    ctx.snippet_session.cancel();
    ctx.ghost.dismiss();
    ctx.lightbulb.reset();
    ctx.peek.close();
    ctx.outline.clear_symbols();
    ctx.crumb_menu.cancel();
    ctx.crumb_files.clear();
    ctx.file_name = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    ctx.language = path
        .as_ref()
        .map(|p| crate::langdetect::detect_path(p))
        .unwrap_or(crate::langdetect::Language::Mighty);
    if path.is_some() {
        ctx.welcome.allow_empty_auto();
    }
    ctx.file_path = path;
    ctx.autosave.disarm();
    ctx.autosave_sig = None;
    if crate::settings::autosave()
        && !ctx.tabs.active_read_only()
        && ctx.file_path.is_some()
        && ctx.tabs.is_dirty(active)
    {
        ctx.autosave.touch();
    }
}

fn prune_missing_recent_files(ctx: &mut MuiContext) {
    if ctx.quickopen.prune_missing_recents() {
        persist_recent_files(ctx);
    }
}

fn prune_missing_recent_workspaces(ctx: &mut MuiContext) {
    if ctx.recent_workspaces.prune_missing_dirs() {
        let _ = crate::config::save_recent_workspaces(&ctx.recent_workspaces.to_blob());
    }
}

fn close_tab_unchecked(ctx: &mut MuiContext, idx_u: usize) -> i32 {
    // Remap pane->tab indices so a pane never points past the end after a close.
    ctx.pending_dirty_close = None;
    let a = ctx.tabs.close(idx_u);
    ctx.panes.on_tab_closed(idx_u, ctx.tabs.count());
    sync_active_path(ctx);
    ensure_tab_visible(ctx, a);
    a as i32
}

fn tab_reopen_closed_unchecked(ctx: &mut MuiContext) -> i32 {
    match ctx.tabs.reopen_closed() {
        Some(active) => {
            sync_active_path(ctx);
            ctx.panes = crate::panes::PaneLayout::new(active);
            ensure_tab_visible(ctx, active);
            let name = ctx.tabs.get(active).map(|t| t.basename()).unwrap_or_else(|| "tab".to_string());
            ctx.push_toast(crate::toast::Kind::Info, format!("Reopened {name}"));
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No closed tab to reopen");
            -1
        }
    }
}

/// Number of open tabs (always >= 1).
#[no_mangle]
pub extern "C" fn mui_tab_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.count() as i32)
}

/// Index (0-based) of the active tab.
#[no_mangle]
pub extern "C" fn mui_tab_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active() as i32)
}

/// Open the path staged via `mui_path_*` as a new tab (or switch to it if
/// already open), reading its bytes from disk. Returns the resulting tab index,
/// or -1 when no file was opened. The staged path is resolved relative to the
/// tree root when not absolute, so Ctrl+O "foo.mty" opens beside the initial file.
#[no_mangle]
pub extern "C" fn mui_tab_open_path(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let raw = String::from_utf8_lossy(&staged).into_owned();
    let raw = raw.trim();
    if raw.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "No file path entered");
        return -1;
    }
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        ctx.tree.root().join(&candidate)
    };
    match std::fs::metadata(&resolved) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                open_failed_message(&resolved, "not a file"),
            );
            return -1;
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                open_failed_message(&resolved, &e.to_string()),
            );
            return -1;
        }
    }
    let idx = ctx.tabs.open_path(resolved.clone());
    sync_active_path(ctx);
    record_opened_file(ctx, &resolved);
    idx as i32
}

/// Open a native Windows file picker and open the selected file in a tab.
/// Returns the resulting tab index, `-2` when cancelled, or `-1` when the picker
/// is unavailable. Mighty only falls back to the typed-path prompt for `-1`.
#[no_mangle]
pub extern "C" fn mui_open_file_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let initial_dir = file_dialog_initial_dir(ctx);
    let owner_hwnd = dialog_owner_hwnd(ctx);
    let path = match pick_open_file_native(&initial_dir, owner_hwnd) {
        FileDialogPick::Picked(path) => path,
        FileDialogPick::Cancelled => {
            println!("mui_open_file_dialog: native file dialog cancelled");
            ctx.push_toast(crate::toast::Kind::Info, "Open file cancelled");
            return -2;
        }
        FileDialogPick::Unavailable => {
            println!("mui_open_file_dialog: native file dialog unavailable");
            ctx.push_toast(crate::toast::Kind::Warn, "Open file dialog unavailable");
            return -1;
        }
    };
    trace(&format!("open_file_dialog path={}", path.display()));
    let idx = ctx.tabs.open_path(path.clone());
    sync_active_path(ctx);
    record_opened_file(ctx, &path);
    ensure_tab_visible(ctx, idx);
    idx as i32
}

/// Switch the active tab to `idx`. Returns the resulting active index, or `-1`
/// when the requested tab does not exist.
#[no_mangle]
pub extern "C" fn mui_tab_switch(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx < 0 || idx as usize >= ctx.tabs.count() {
        ctx.push_toast(crate::toast::Kind::Warn, "No tab at that position");
        trace(&format!("tab_switch idx={idx} -> invalid"));
        return -1;
    }
    let a = ctx.tabs.switch(idx as usize);
    sync_active_path(ctx);
    ensure_tab_visible(ctx, a);
    a as i32
}

/// Switch to the next tab (wraps). Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_tab_next(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let a = ctx.tabs.next();
    sync_active_path(ctx);
    ensure_tab_visible(ctx, a);
    a as i32
}

/// Switch to the previous tab (wraps). Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_tab_prev(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let a = ctx.tabs.prev();
    sync_active_path(ctx);
    ensure_tab_visible(ctx, a);
    a as i32
}

/// Close tab `idx`, keeping at least one tab (last close -> empty scratch).
/// Dirty buffers open the unsaved-work confirmation overlay. The tab is only
/// closed after an explicit Save or Discard choice, never from a repeated close
/// click/key press.
/// Returns the active index after a successful close, or `-1` when confirmation
/// is required / the requested tab does not exist.
#[no_mangle]
pub extern "C" fn mui_tab_close(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx < 0 || idx as usize >= ctx.tabs.count() {
        ctx.push_toast(crate::toast::Kind::Warn, "No tab at that position");
        trace(&format!("tab_close idx={idx} -> invalid"));
        return -1;
    }
    let idx_u = idx as usize;
    if ctx.tabs.is_dirty(idx_u) {
        ctx.pending_dirty_close = Some((idx_u, std::time::Instant::now()));
        ctx.pending_quit = None;
        let name = ctx
            .tabs
            .get(idx_u)
            .map(|t| t.basename())
            .unwrap_or_else(|| "tab".to_string());
        ctx.push_toast(crate::toast::Kind::Warn, format!("Review unsaved changes in {name}"));
        trace(&format!("tab_close idx={idx_u} -> dirty-confirm"));
        return -1;
    }
    ctx.pending_dirty_close = None;
    let a = ctx.tabs.close(idx_u);
    // Remap pane→tab indices so a pane never points past the end after a close.
    ctx.panes.on_tab_closed(idx_u, ctx.tabs.count());
    sync_active_path(ctx);
    ensure_tab_visible(ctx, a);
    trace(&format!("tab_close idx={idx_u} -> active={a} count={}", ctx.tabs.count()));
    a as i32
}

/// Reopen the most recently closed tab. Returns the reopened active tab index,
/// or -1 when there is no recoverable tab.
#[no_mangle]
pub extern "C" fn mui_tab_reopen_closed(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    tab_reopen_closed_unchecked(ctx)
}

/// Duplicate the active tab next to itself. Returns the duplicate's active
/// index, or -1 on an invalid handle.
#[no_mangle]
pub extern "C" fn mui_tab_duplicate_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let active = ctx.tabs.duplicate_active();
    sync_active_path(ctx);
    ctx.panes = crate::panes::PaneLayout::new(active);
    ensure_tab_visible(ctx, active);
    let name = ctx
        .tabs
        .get(active)
        .map(|t| t.basename())
        .unwrap_or_else(|| "tab".to_string());
    ctx.push_toast(crate::toast::Kind::Info, format!("Duplicated {name}"));
    active as i32
}

/// Move the active tab one slot left. Returns the new active index, or -1 when
/// the tab is already first / no move is possible.
#[no_mangle]
pub extern "C" fn mui_tab_move_active_left(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let before = ctx.tabs.active();
    match ctx.tabs.move_active_left() {
        Some(active) => {
            ctx.panes.on_tabs_swapped(active, before);
            sync_active_path(ctx);
            ensure_tab_visible(ctx, active);
            ctx.push_toast(crate::toast::Kind::Info, "Moved tab left");
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "Tab is already first");
            trace("tab_move_left noop");
            -1
        }
    }
}

/// Move the active tab one slot right. Returns the new active index, or -1 when
/// the tab is already last / no move is possible.
#[no_mangle]
pub extern "C" fn mui_tab_move_active_right(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let before = ctx.tabs.active();
    match ctx.tabs.move_active_right() {
        Some(active) => {
            ctx.panes.on_tabs_swapped(before, active);
            sync_active_path(ctx);
            ensure_tab_visible(ctx, active);
            ctx.push_toast(crate::toast::Kind::Info, "Moved tab right");
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "Tab is already last");
            trace("tab_move_right noop");
            -1
        }
    }
}

/// Sort open tabs by display name. Returns the new active index, or -1 when the
/// order was already sorted / no move was possible.
#[no_mangle]
pub extern "C" fn mui_tab_sort_by_name(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.sort_by_name() {
        Some(old_to_new) => {
            ctx.panes.on_tabs_reordered(&old_to_new);
            sync_active_path(ctx);
            let active = ctx.tabs.active();
            ensure_tab_visible(ctx, active);
            ctx.push_toast(crate::toast::Kind::Info, "Sorted tabs by name");
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "Tabs already sorted");
            trace("tab_sort_by_name noop");
            -1
        }
    }
}

/// Close clean duplicate file-backed tabs. Dirty duplicates are preserved.
/// Returns the active index after compaction, or -1 when nothing was closed.
#[no_mangle]
pub extern "C" fn mui_tab_close_duplicate_files(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.close_duplicate_file_tabs() {
        Some(compaction) => {
            ctx.panes
                .on_tabs_compacted(&compaction.old_to_new, ctx.tabs.count());
            sync_active_path(ctx);
            let active = ctx.tabs.active();
            ensure_tab_visible(ctx, active);
            let noun = if compaction.removed == 1 { "tab" } else { "tabs" };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Closed {} duplicate {noun}", compaction.removed),
            );
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No duplicate file tabs");
            -1
        }
    }
}

/// Reload the active file-backed tab from disk. Dirty tabs are protected and
/// require the user to save or close/discard explicitly first. Returns the
/// active tab index after reload, or -1 when reload was refused/failed.
#[no_mangle]
pub extern "C" fn mui_tab_reload_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    reload_active_from_disk(ctx, false)
}

/// Discard local edits in the active file-backed tab and reload it from disk.
/// Returns the active tab index after revert, or -1 when the active tab cannot
/// be reloaded from disk.
#[no_mangle]
pub extern "C" fn mui_tab_revert_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    reload_active_from_disk(ctx, true)
}

fn reload_active_from_disk(ctx: &mut MuiContext, allow_dirty: bool) -> i32 {
    let active = ctx.tabs.active();
    let was_dirty = ctx.tabs.is_dirty(active);
    if was_dirty && !allow_dirty {
        let name = ctx
            .tabs
            .get(active)
            .map(|t| t.basename())
            .unwrap_or_else(|| "tab".to_string());
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Save or discard changes before reloading: {name}"),
        );
        return -1;
    }
    let Some(path) = ctx.tabs.active_path() else {
        let action = if allow_dirty { "revert" } else { "reload" };
        ctx.push_toast(crate::toast::Kind::Info, format!("No file-backed tab to {action}"));
        return -1;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            let action = if allow_dirty { "Revert" } else { "Reload" };
            refresh_workspace_file_views(ctx);
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message(action, &path, &e),
            );
            return -1;
        }
    };
    ctx.tabs.reload_active(&bytes);
    let _ = ctx
        .tabs
        .reload_all_clean_path_except(&path, &bytes, active);
    sync_active_path(ctx);
    ensure_tab_visible(ctx, active);
    let name = basename(&path);
    let message = if allow_dirty && was_dirty {
        format!("Reverted {name}")
    } else {
        format!("Reloaded {name}")
    };
    ctx.push_toast(crate::toast::Kind::Info, message);
    active as i32
}

/// Close every clean tab while preserving dirty tabs. Returns the new active tab
/// index, or -1 when nothing was closed.
#[no_mangle]
pub extern "C" fn mui_tab_close_saved(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.close_saved() {
        Some(compaction) => {
            ctx.panes.on_tabs_compacted(&compaction.old_to_new, ctx.tabs.count());
            let active = ctx.tabs.active();
            sync_active_path(ctx);
            ensure_tab_visible(ctx, active);
            let noun = if compaction.removed == 1 { "tab" } else { "tabs" };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Closed {} saved {noun}", compaction.removed),
            );
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No saved tabs to close");
            -1
        }
    }
}

/// Close every clean tab except the active tab, preserving dirty tabs. Returns
/// the new active tab index, or -1 when nothing was closed.
#[no_mangle]
pub extern "C" fn mui_tab_close_other_saved(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.close_other_saved() {
        Some(compaction) => {
            ctx.panes
                .on_tabs_compacted(&compaction.old_to_new, ctx.tabs.count());
            let active = ctx.tabs.active();
            sync_active_path(ctx);
            ensure_tab_visible(ctx, active);
            let noun = if compaction.removed == 1 { "tab" } else { "tabs" };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Closed {} other saved {noun}", compaction.removed),
            );
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No other saved tabs to close");
            -1
        }
    }
}

/// Close clean tabs to the right of the active tab, preserving dirty tabs.
/// Returns the new active tab index, or -1 when nothing was closed.
#[no_mangle]
pub extern "C" fn mui_tab_close_saved_to_right(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.close_saved_to_right() {
        Some(compaction) => {
            ctx.panes
                .on_tabs_compacted(&compaction.old_to_new, ctx.tabs.count());
            let active = ctx.tabs.active();
            sync_active_path(ctx);
            ensure_tab_visible(ctx, active);
            let noun = if compaction.removed == 1 { "tab" } else { "tabs" };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Closed {} saved {noun} to the right", compaction.removed),
            );
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No saved tabs to the right");
            -1
        }
    }
}

/// Close clean tabs to the left of the active tab, preserving dirty tabs.
/// Returns the new active tab index, or -1 when nothing was closed.
#[no_mangle]
pub extern "C" fn mui_tab_close_saved_to_left(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.close_saved_to_left() {
        Some(compaction) => {
            ctx.panes
                .on_tabs_compacted(&compaction.old_to_new, ctx.tabs.count());
            let active = ctx.tabs.active();
            sync_active_path(ctx);
            ensure_tab_visible(ctx, active);
            let noun = if compaction.removed == 1 { "tab" } else { "tabs" };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Closed {} saved {noun} to the left", compaction.removed),
            );
            active as i32
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No saved tabs to the left");
            -1
        }
    }
}

/// Request application exit. If any tab has unsaved edits, opens the
/// unsaved-work confirmation overlay and returns `0`; only the overlay's
/// explicit Save/Discard paths can complete the quit. Clean workspaces return
/// `1` immediately.
#[no_mangle]
pub extern "C" fn mui_quit_request(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 1;
    };
    let dirty = ctx.tabs.dirty_count();
    if dirty == 0 {
        ctx.pending_quit = None;
        return 1;
    }
    ctx.pending_dirty_close = None;
    ctx.pending_quit = Some(std::time::Instant::now());
    let noun = if dirty == 1 { "tab" } else { "tabs" };
    ctx.push_toast(
        crate::toast::Kind::Warn,
        format!("Review {dirty} unsaved {noun} before quitting"),
    );
    0
}

fn dirty_confirm_active(ctx: &MuiContext) -> bool {
    ctx.pending_dirty_close.is_some() || ctx.pending_quit.is_some()
}

fn dirty_confirm_surface_size(ctx: &MuiContext) -> (f32, f32) {
    let (w, h) = visible_surface_size(ctx);
    (w as f32, h as f32)
}

fn dirty_confirm_rects(
    ctx: &MuiContext,
) -> (
    (f32, f32, f32, f32),
    (f32, f32, f32, f32),
    (f32, f32, f32, f32),
) {
    let (w, h) = dirty_confirm_surface_size(ctx);
    let (card_x, card_y, card_w, card_h) = dirty_confirm_card_rect(w, h);
    let btn_w = dirty_confirm_button_width(card_w);
    let btn_h = 34.0;
    let by = card_y + card_h - 54.0;
    let discard = (card_x + card_w - btn_w - 24.0, by, btn_w, btn_h);
    let save = (discard.0 - btn_w - 12.0, by, btn_w, btn_h);
    let cancel = (save.0 - btn_w - 12.0, by, btn_w, btn_h);
    (cancel, save, discard)
}

#[no_mangle]
pub extern "C" fn mui_dirty_confirm_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(dirty_confirm_active(ctx))
}

#[no_mangle]
pub extern "C" fn mui_dirty_confirm_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if dirty_confirm_active(ctx) {
        ctx.pending_dirty_close = None;
        ctx.pending_quit = None;
        ctx.push_toast(crate::toast::Kind::Info, "Unsaved changes confirmation cancelled");
        trace("dirty_confirm cancel");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No unsaved changes confirmation open");
        0
    }
}

/// Commit the destructive choice in the unsaved-work confirmation overlay.
/// Returns -2 for confirmed app quit, -1 if no confirmation is active, or the
/// active tab index after a confirmed tab close.
#[no_mangle]
pub extern "C" fn mui_dirty_confirm_discard(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if ctx.pending_quit.is_some() {
        ctx.pending_quit = None;
        ctx.pending_dirty_close = None;
        trace("dirty_confirm discard -> quit");
        return -2;
    }
    let Some((idx_u, _)) = ctx.pending_dirty_close else {
        return -1;
    };
    trace(&format!("dirty_confirm discard tab={idx_u}"));
    ctx.tabs.discard_edits(idx_u);
    close_tab_unchecked(ctx, idx_u)
}

/// Save the dirty work referenced by the confirmation overlay, then close/quit.
/// Returns -3 if save was cancelled/failed, -2 for confirmed app quit, -1 if no
/// confirmation is active, or the active tab index after saving and closing.
#[no_mangle]
pub extern "C" fn mui_dirty_confirm_save(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if ctx.pending_quit.is_some() {
        let dirty: Vec<usize> = (0..ctx.tabs.count()).filter(|i| ctx.tabs.is_dirty(*i)).collect();
        for idx in dirty {
            if !save_confirm_tab(ctx, idx) {
                trace(&format!("dirty_confirm save tab={idx} -> cancelled"));
                return -3;
            }
        }
        ctx.pending_quit = None;
        ctx.pending_dirty_close = None;
        trace("dirty_confirm save -> quit");
        return -2;
    }
    let Some((idx_u, _)) = ctx.pending_dirty_close else {
        return -1;
    };
    if !save_confirm_tab(ctx, idx_u) {
        trace(&format!("dirty_confirm save tab={idx_u} -> cancelled"));
        return -3;
    }
    trace(&format!("dirty_confirm save tab={idx_u}"));
    close_tab_unchecked(ctx, idx_u)
}

/// Hit-test the unsaved-work confirmation overlay. Returns 1 for Cancel, 2 for
/// Discard, 3 for Save, or 0 for a miss.
#[no_mangle]
pub extern "C" fn mui_dirty_confirm_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !dirty_confirm_active(ctx) {
        return 0;
    }
    let (cancel, save, discard) = dirty_confirm_rects(ctx);
    let (px, py) = (ctx.last_event.x, ctx.last_event.y);
    let hit = |r: (f32, f32, f32, f32)| px >= r.0 && px <= r.0 + r.2 && py >= r.1 && py <= r.1 + r.3;
    if hit(cancel) {
        trace(&format!("dirty_confirm_hit x={px:.1} y={py:.1} -> cancel"));
        1
    } else if hit(save) {
        trace(&format!("dirty_confirm_hit x={px:.1} y={py:.1} -> save"));
        3
    } else if hit(discard) {
        trace(&format!("dirty_confirm_hit x={px:.1} y={py:.1} -> discard"));
        2
    } else {
        trace(&format!("dirty_confirm_hit x={px:.1} y={py:.1} -> miss"));
        0
    }
}

#[no_mangle]
pub extern "C" fn mui_dirty_confirm_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !dirty_confirm_active(ctx) {
        return;
    }

    let (w, h) = dirty_confirm_surface_size(ctx);
    let (card_x, card_y, card_w, card_h) = dirty_confirm_card_rect(w, h);
    let chrome = theme::CHROME_FONT_SIZE;
    let old_clip = ctx.clip;
    ctx.clip = None;
    let clip = ctx.clip;
    let dirty = ctx.tabs.dirty_count();
    let (title, detail) = if ctx.pending_quit.is_some() {
        let noun = if dirty == 1 { "tab has" } else { "tabs have" };
        (
            "Discard unsaved changes?",
            format!("{dirty} {noun} unsaved edits. Discarding closes Mighty IDE."),
        )
    } else {
        let name = ctx
            .pending_dirty_close
            .and_then(|(idx, _)| ctx.tabs.get(idx).map(|t| t.basename()))
            .unwrap_or_else(|| "this tab".to_string());
        (
            "Close unsaved tab?",
            format!("{name} has unsaved edits. Discarding cannot be undone."),
        )
    };
    let detail = fit_dirty_confirm_detail(&mut ctx.text, &detail, card_w, chrome);
    let (cancel, save, discard) = dirty_confirm_rects(ctx);

    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, 0.42));
    ctx.dl_shadow(card_x, card_y, card_w, card_h, 8.0, theme::SHADOW(), 24.0);
    ctx.dl_round(card_x, card_y, card_w, card_h, 8.0, theme::ELEVATED());
    ctx.dl_stroke(card_x, card_y, card_w, card_h, 8.0, theme::BORDER_STRONG(), 1.0);
    ctx.dl_rect(card_x, card_y, 4.0, card_h, theme::WARNING());

    ctx.text.queue_ui_sized(card_x + 24.0, card_y + 24.0, title, theme::TEXT(), chrome + 2.0, clip);
    ctx.text.queue_ui_sized(card_x + 24.0, card_y + 58.0, &detail, theme::TEXT_1(), chrome, clip);
    ctx.text.queue_ui_sized(card_x + 24.0, card_y + 86.0, "Choose Save, Discard, or Cancel.", theme::DIM(), chrome - 1.0, clip);

    ctx.dl_round(cancel.0, cancel.1, cancel.2, cancel.3, 6.0, theme::BG_4());
    ctx.dl_stroke(cancel.0, cancel.1, cancel.2, cancel.3, 6.0, theme::BORDER(), 1.0);
    queue_centered_button_label(ctx, cancel, "Cancel", theme::TEXT(), chrome, clip);

    ctx.dl_round(save.0, save.1, save.2, save.3, 6.0, theme::accent_a(0.18));
    ctx.dl_stroke(save.0, save.1, save.2, save.3, 6.0, theme::ACCENT_LINE(), 1.0);
    queue_centered_button_label(ctx, save, "Save", theme::ACCENT_BRIGHT(), chrome, clip);

    ctx.dl_round(discard.0, discard.1, discard.2, discard.3, 6.0, theme::error_wash(0.22));
    ctx.dl_stroke(discard.0, discard.1, discard.2, discard.3, 6.0, theme::ERROR(), 1.0);
    queue_centered_button_label(ctx, discard, "Discard", theme::ERROR(), chrome, clip);

    ctx.overlay = was_overlay;
    ctx.text.set_overlay(was_overlay);
    ctx.clip = old_clip;
}

pub(crate) fn dirty_confirm_button_width(card_w: f32) -> f32 {
    ((card_w - 48.0 - 24.0) / 3.0).max(1.0).min(112.0)
}

pub(crate) fn dirty_confirm_card_rect(surface_w: f32, surface_h: f32) -> (f32, f32, f32, f32) {
    let card_w = (surface_w - 32.0)
        .max(0.0)
        .clamp(280.0, 520.0)
        .min(surface_w.max(1.0));
    let card_h = 184.0;
    let card_x = ((surface_w - card_w) * 0.5).max(0.0);
    let card_y = ((surface_h - card_h) * 0.5)
        .max(48.0)
        .min((surface_h - card_h).max(0.0));
    (card_x, card_y, card_w, card_h)
}

fn queue_centered_button_label(
    ctx: &mut MuiContext,
    rect: (f32, f32, f32, f32),
    label: &str,
    color: MuiColor,
    chrome: f32,
    clip: Option<(u32, u32, u32, u32)>,
) {
    let label = fit_dirty_confirm_button_label(&mut ctx.text, label, rect.2, chrome);
    if label.is_empty() {
        return;
    }
    let (label_w, _) = ctx.text.measure_ui_sized(&label, chrome);
    let x = rect.0 + ((rect.2 - label_w) * 0.5).max(6.0);
    ctx.text.queue_ui_sized(x, rect.1 + 8.0, &label, color, chrome, clip);
}

pub(crate) fn fit_dirty_confirm_button_label(
    text: &mut crate::text::Text,
    label: &str,
    button_w: f32,
    chrome: f32,
) -> String {
    fit_status_head(text, label, (button_w - 12.0).max(0.0), chrome)
}

pub(crate) fn fit_dirty_confirm_detail(
    text: &mut crate::text::Text,
    detail: &str,
    card_w: f32,
    chrome: f32,
) -> String {
    fit_status_tail(text, detail, (card_w - 48.0).max(0.0), chrome)
}

/// Map the tab bar pixel x of the last click to a tab index, or -1 if the click
/// is past the last tab. Used to switch tabs by clicking.
fn tab_bar_body_left(ctx: &MuiContext) -> f32 {
    layout::body_left(ctx.sidebar_visible)
}

fn tab_bar_right(ctx: &MuiContext) -> f32 {
    let body_left = tab_bar_body_left(ctx);
    (crate::titlebar::controls_x(ctx.gpu.width as f32) - crate::titlebar::ACTION_STRIP_W)
        .max(body_left)
}

fn tab_visible_end(ctx: &MuiContext) -> f32 {
    let body_left = tab_bar_body_left(ctx);
    let tab_right = tab_bar_right(ctx);
    let visible = ctx.tabs.count().saturating_sub(ctx.tab_scroll).min(tab_bar_capacity(ctx));
    (body_left + visible as f32 * layout::TAB_W).min(tab_right)
}

fn tab_bar_capacity(ctx: &MuiContext) -> usize {
    let width = (tab_bar_right(ctx) - tab_bar_body_left(ctx)).max(0.0);
    (width / layout::TAB_W).floor().max(1.0) as usize
}

fn tab_max_scroll(ctx: &MuiContext) -> usize {
    ctx.tabs.count().saturating_sub(tab_bar_capacity(ctx))
}

fn clamp_tab_scroll(ctx: &mut MuiContext) {
    ctx.tab_scroll = ctx.tab_scroll.min(tab_max_scroll(ctx));
}

fn ensure_tab_visible(ctx: &mut MuiContext, idx: usize) {
    let count = ctx.tabs.count();
    if count == 0 {
        ctx.tab_scroll = 0;
        return;
    }
    let idx = idx.min(count - 1);
    let cap = tab_bar_capacity(ctx).max(1);
    if idx < ctx.tab_scroll {
        ctx.tab_scroll = idx;
    } else if idx >= ctx.tab_scroll + cap {
        ctx.tab_scroll = idx + 1 - cap;
    }
    clamp_tab_scroll(ctx);
}

fn tab_slot_rect(ctx: &MuiContext, idx: usize) -> Option<(f32, f32)> {
    if idx < ctx.tab_scroll {
        return None;
    }
    let body_left = tab_bar_body_left(ctx);
    let tab_right = tab_bar_right(ctx);
    let slot = idx - ctx.tab_scroll;
    let x = body_left + slot as f32 * layout::TAB_W;
    if x >= tab_right {
        return None;
    }
    let tab_w = layout::TAB_W.min(tab_right - x);
    if tab_w < 48.0 {
        return None;
    }
    Some((x, tab_w))
}

fn tab_index_at_point(ctx: &MuiContext, x: f32, y: f32) -> Option<usize> {
    if !tab_bar_contains_point(ctx, x, y) {
        return None;
    }
    let body_left = tab_bar_body_left(ctx);
    let slot = ((x - body_left) / layout::TAB_W).floor().max(0.0) as usize;
    let idx = ctx.tab_scroll + slot;
    (idx < ctx.tabs.count()).then_some(idx)
}

fn tab_bar_contains_point(ctx: &MuiContext, x: f32, y: f32) -> bool {
    if y > layout::TAB_BAR_H {
        return false;
    }
    let body_left = tab_bar_body_left(ctx);
    let tab_right = tab_bar_right(ctx);
    if x < body_left || x >= tab_right {
        return false;
    }
    true
}

fn tab_scroll_by(ctx: &mut MuiContext, dir: i32) -> bool {
    let before = ctx.tab_scroll;
    let max = tab_max_scroll(ctx);
    if dir > 0 {
        ctx.tab_scroll = (ctx.tab_scroll + 1).min(max);
    } else if dir < 0 {
        ctx.tab_scroll = ctx.tab_scroll.saturating_sub(1);
    }
    let changed = before != ctx.tab_scroll;
    if changed {
        trace(&format!("tab_scroll dir={dir} from={before} to={}", ctx.tab_scroll));
    }
    changed
}

fn topbar_command_center_rect(ctx: &MuiContext) -> Option<(f32, f32, f32, f32)> {
    if layout::zen_active() {
        return None;
    }
    let left = tab_visible_end(ctx) + 14.0;
    let right = tab_bar_right(ctx) - 14.0;
    let avail = right - left;
    if avail < 210.0 {
        return None;
    }
    let w = avail.min(340.0);
    let x = left + (avail - w) * 0.5;
    let h = 24.0;
    let y = (layout::TAB_BAR_H - h) * 0.5;
    Some((x, y, w, h))
}

fn topbar_command_center_hit(ctx: &MuiContext, x: f32, y: f32) -> bool {
    let Some((cx, cy, cw, ch)) = topbar_command_center_rect(ctx) else {
        return false;
    };
    x >= cx && x <= cx + cw && y >= cy && y <= cy + ch
}

/// Scroll the overflowing tab strip. `dir > 0` shows later tabs; `dir < 0`
/// shows earlier tabs. Returns the new first visible tab index.
#[no_mangle]
pub extern "C" fn mui_tab_strip_scroll(handle: i64, dir: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    tab_scroll_by(ctx, dir);
    ctx.tab_scroll as i32
}

#[no_mangle]
pub extern "C" fn mui_tab_index_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let lx = ctx.last_event.x;
    if let Some(i) = tab_index_at_point(ctx, lx, ctx.last_event.y) {
        trace(&format!("tab_hit x={lx:.1} y={:.1} -> {i}", ctx.last_event.y));
        i as i32
    } else {
        -1
    }
}

/// Map the last click to a tab's trailing close affordance, or -1 if none.
/// Checked before normal tab switching so the visible close icon actually works.
#[no_mangle]
pub extern "C" fn mui_tab_close_index_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let lx = ctx.last_event.x;
    let Some(hit_idx) = tab_index_at_point(ctx, lx, ctx.last_event.y) else {
        return -1;
    };
    for i in ctx.tab_scroll..ctx.tabs.count() {
        let Some((x, tab_w)) = tab_slot_rect(ctx, i) else { break };
        if lx >= x + tab_w - 34.0 && lx <= x + tab_w - 8.0 {
            trace(&format!("tab_close_hit x={lx:.1} y={:.1} -> {i}", ctx.last_event.y));
            return i as i32;
        }
        if i >= hit_idx {
            break;
        }
    }
    -1
}

// ---- tab byte-swap: store the live Mighty buffer into a slot ----

/// Begin storing the live buffer into tab `idx`: clear its bytes.
#[no_mangle]
pub extern "C" fn mui_tab_store_begin(handle: i64, idx: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs.store_begin(idx as usize);
        }
    }
}

/// Append one byte to tab `idx`'s buffer during a store.
#[no_mangle]
pub extern "C" fn mui_tab_store_byte(handle: i64, idx: i32, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs.store_byte(idx as usize, (byte & 0xff) as u8);
        }
    }
}

/// Commit the editor state (0-based cursor line/col + scroll first line) into
/// tab `idx` after streaming its bytes.
#[no_mangle]
pub extern "C" fn mui_tab_store_commit(
    handle: i64,
    idx: i32,
    cursor_line: i32,
    cursor_col: i32,
    scroll_first: i32,
) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            ctx.tabs
                .store_commit(idx as usize, cursor_line, cursor_col, scroll_first);
        }
    }
}

/// Mark tab `idx` dirty (1) or clean (0).
#[no_mangle]
pub extern "C" fn mui_tab_set_dirty(handle: i64, idx: i32, dirty: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if idx >= 0 {
            let idx = idx as usize;
            if dirty != 0 {
                ctx.tabs.set_dirty(idx, true);
            } else {
                ctx.tabs.mark_clean(idx);
            }
        }
    }
}

/// Byte length of tab `idx`'s buffer (what the Mighty side pulls back), or -1.
#[no_mangle]
pub extern "C" fn mui_tab_load(handle: i64, idx: i32) -> i64 {
    if idx < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.tabs.load_len(idx as usize))
}

/// Copy tab `idx`'s buffer into the shim's `load_buf` and return its byte
/// length (or -1 on a null handle / bad index). The Mighty side then pulls the
/// bytes back through the **two-argument** `mui_load_byte(h, i)` getter
/// (proven-safe under v0.36 native codegen) rather than the three-argument
/// `mui_tab_load_byte(h, idx, i)`, which corrupts a `Vec.push` accumulator when
/// driven from a tight Mighty loop. Used for the initial load + every tab
/// switch so the live editor buffer is always actually populated.
#[no_mangle]
pub extern "C" fn mui_tab_load_into(handle: i64, idx: i32) -> i64 {
    if idx < 0 {
        return -1;
    }
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.tabs.get(idx as usize) {
        Some(t) => {
            ctx.load_buf = t.bytes.clone();
            ctx.load_buf.len() as i64
        }
        None => {
            ctx.load_buf.clear();
            -1
        }
    }
}

/// Byte at index `i` of tab `idx`'s buffer, or -1 out of range.
#[no_mangle]
pub extern "C" fn mui_tab_load_byte(handle: i64, idx: i32, i: i64) -> i32 {
    if idx < 0 || i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.tabs.load_byte(idx as usize, i as usize))
}

/// Saved 0-based cursor line of tab `idx`, or 0.
#[no_mangle]
pub extern "C" fn mui_tab_cursor_line(handle: i64, idx: i32) -> i32 {
    if idx < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.get(idx as usize).map_or(0, |t| t.cursor_line))
}

/// Saved 0-based cursor column of tab `idx`, or 0.
#[no_mangle]
pub extern "C" fn mui_tab_cursor_col(handle: i64, idx: i32) -> i32 {
    if idx < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.get(idx as usize).map_or(0, |t| t.cursor_col))
}

/// Saved scroll first-line of tab `idx`, or 0.
#[no_mangle]
pub extern "C" fn mui_tab_scroll(handle: i64, idx: i32) -> i32 {
    if idx < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.get(idx as usize).map_or(0, |t| t.scroll_first))
}

/// Draw the far-left activity rail: the brand mark on top, a column of icon
/// glyphs, and an ember selection bar + ember-tinted active icon for the
/// Explorer (the only active view). Drawn first so the tab bar / sidebar sit to
/// its right. Mighty calls this once per frame.
#[no_mangle]
pub extern "C" fn mui_rail_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let h = ctx.gpu.height as f32;
    let rw = layout::RAIL_W;
    use crate::icons;

    // Rail panel (near-black) + a hairline right divider.
    ctx.dl_rect(0.0, 0.0, rw, h, theme::BG_RAIL());
    ctx.dl_rect(rw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // Brand tile: compact enough for the rail, with the same simplified
    // small-size treatment as the Windows icon so the mark stays crisp.
    let logo_tile = 30.0;
    let lx = (rw - logo_tile) * 0.5;
    let ly = 8.0;
    ctx.dl_shadow(lx, ly + 2.0, logo_tile, logo_tile, 6.0, MuiColor::new(0.35, 0.95, 0.90, 0.13), 10.0);
    ctx.dl_grad_v(
        lx,
        ly,
        logo_tile,
        logo_tile,
        6.0,
        MuiColor::new(0.08, 0.09, 0.15, 1.0),
        MuiColor::new(0.03, 0.04, 0.09, 1.0),
    );
    ctx.dl_stroke(lx, ly, logo_tile, logo_tile, 6.0, MuiColor::new(0.56, 0.96, 0.94, 0.86), 1.1);
    ctx.dl_icon(
        lx + 4.0,
        ly + 4.0,
        logo_tile - 8.0,
        logo_tile - 8.0,
        icons::LANG_M_FILL,
        theme::ACCENT_BRIGHT(),
        0.0,
        true,
    );

    // Activity icons. Explorer (index 0) active. Each is a 38x38 hit cell with a
    // 21px vector icon centered; the active one gets an indigo top-lit tile + a
    // left accent bar with glow (matches `.rail-btn.active`).
    let rail_icons: [&str; 9] = [
        icons::EXPLORER,
        icons::SEARCH,
        icons::GIT,
        icons::RUN,
        icons::AGENTS,
        icons::OUTLINE,
        icons::DEBUG,
        icons::BEAKER,
        icons::AGENTS_NET,
    ];
    let cell = 38.0;
    let icon_sz = 21.0;
    let icon_top = 60.0; // separated from the brand tile so active state never reads as logo chrome
    let gap = 4.0;
    let cx = (rw - cell) * 0.5;
    // The active rail icon reflects the live sidebar panel where applicable.
    // Run is a bottom panel and AI is right-docked, so they track their own state.
    let active_panel = ctx.active_panel;
    let ai_open = ctx.ai.open;
    for (i, path) in rail_icons.iter().enumerate() {
        let cy = icon_top + i as f32 * (cell + gap);
        // Slot 4 (Agents/AI) is active when the AI panel is open, even though it
        // is not a sidebar panel; the others track `active_panel`.
        let active = (i == 4 && ai_open) || (i != 4 && i as i32 == active_panel);
        // Slot 6 (Debug) draws as filled when a session is paused (so the bug
        // glows during a stop) — handled by `color` below via active_panel.
        let ix = (rw - icon_sz) * 0.5;
        let iy = cy + (cell - icon_sz) * 0.5;
        if active {
            // Tile (top-lit indigo gradient) + left accent bar + soft glow.
            ctx.dl_grad_v(cx, cy, cell, cell, 8.0, theme::ACCENT_FAINT(), theme::accent_a(0.035));
            ctx.dl_round(0.0, cy + 9.0, 3.0, cell - 18.0, 1.5, theme::ACCENT());
            ctx.dl_shadow(0.0, cy + 9.0, 3.0, cell - 18.0, 1.5, theme::ACCENT_GLOW(), 8.0);
        }
        let color = if active { theme::ACCENT_BRIGHT() } else { theme::DIM() };
        let fill_run = path == &icons::RUN;
        ctx.dl_icon(ix, iy, icon_sz, icon_sz, path, color, 1.5, fill_run);
        if path == &icons::AGENTS {
            ctx.dl_icon(ix, iy, icon_sz, icon_sz, icons::AGENTS_DOT, color, 0.0, true);
        }
        // Git badge "3".
        if path == &icons::GIT {
            let bw = 15.0;
            let bx = cx + cell - bw - 2.0;
            let by = cy + 3.0;
            ctx.dl_round(bx, by, bw, 15.0, 7.5, theme::ACCENT());
            ctx.text.queue_ui_sized(bx + 4.0, by + 1.5, "3", theme::TEXT(), 9.0, None);
        }
    }

    // Bottom: accounts + settings.
    let sx = (rw - icon_sz) * 0.5;
    for (y, icon) in [(h - 84.0, icons::USER), (h - 46.0, icons::SETTINGS)] {
        ctx.dl_round((rw - 34.0) * 0.5, y - 5.0, 34.0, 31.0, 7.0, theme::accent_a(0.025));
        ctx.dl_icon(sx, y, icon_sz, icon_sz, icon, theme::DIM(), 1.5, false);
    }
}

/// Hit-test the bottom utility icons in the activity rail.
/// Returns 1 = account/user, 2 = settings, -1 = none.
fn rail_utility_hit(x: f32, y: f32, h: f32) -> i32 {
    if !(0.0..=layout::RAIL_W).contains(&x) {
        return -1;
    }
    if y >= h - 84.0 && y <= h - 56.0 {
        return 1;
    }
    if y >= h - 46.0 && y <= h - 18.0 {
        return 2;
    }
    -1
}

/// Hit-test the bottom utility icons in the activity rail.
/// Returns 1 = account/user, 2 = settings, -1 = none.
#[no_mangle]
pub extern "C" fn mui_rail_utility_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    let h = ctx.gpu.height as f32;
    match rail_utility_hit(x, y, h) {
        1 => {
            trace(&format!("rail_utility x={x:.1} y={y:.1} -> account"));
            1
        }
        2 => {
            trace(&format!("rail_utility x={x:.1} y={y:.1} -> settings"));
            2
        }
        _ => -1,
    }
}

/// Draw the breadcrumb bar at the top of the editor body (`path › file › symbol`,
/// the file segment in ember). Sits between the tab bar and the editor field,
/// spanning from the editor's left edge to the right of the window.
#[no_mangle]
pub extern "C" fn mui_breadcrumb_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let w = ctx.gpu.width as f32;
    let handle_ptr = handle as usize as *mut MuiContext;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let left = layout::body_left(ctx.sidebar_visible);
    let top = layout::TAB_BAR_H;
    let bar_h = layout::BREADCRUMB_H;

    // Editor field background under the breadcrumb + a soft bottom divider.
    unsafe {
        crate::mui_fill_rect(handle_ptr, left, top, w - left, bar_h, theme::BG_EDIT());
        crate::mui_fill_rect(handle_ptr, left, top + bar_h - 1.0, w - left, 1.0, theme::BORDER_SOFT());
    }

    let parent = ctx
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

    let ty = top + (bar_h - chrome) * 0.5 - 1.0;
    let icon_y = top + (bar_h - 12.0) * 0.5;
    let md_button = if ctx.language == crate::langdetect::Language::Markdown {
        Some(md_button_rect(w, top, bar_h))
    } else {
        None
    };
    let text_right = md_button.map_or(w - 12.0, |(bx, _, _, _)| bx - 8.0).max(left + 24.0);
    let mut x = left + 16.0;
    // Folder icon for the first segment.
    if x + 19.0 <= text_right {
        ctx.dl_icon(x, icon_y, 13.0, 13.0, crate::icons::FOLDER, theme::DIM(), 1.4, false);
        x += 13.0 + 6.0;
    }
    let reserve_file_space = md_button.is_some();
    let parent_right = if reserve_file_space {
        (x + (text_right - x) * 0.34).min(text_right)
    } else {
        text_right
    };
    let _ = queue_breadcrumb_segment(ctx, &mut x, &parent, theme::DIM(), chrome, ty, clip, parent_right);
    if queue_breadcrumb_separator(ctx, &mut x, icon_y, text_right) {
        let file_right = if reserve_file_space {
            (x + (text_right - x) * 0.68).min(text_right)
        } else {
            text_right
        };
        let file_full = queue_breadcrumb_segment(ctx, &mut x, &file, theme::TEXT_1(), chrome, ty, clip, file_right);
        if file_full {
            let _ = queue_breadcrumb_separator(ctx, &mut x, icon_y, text_right);
        }
    }
    // Symbol segment: the symbol under the cursor (from the Outline data), drawn
    // with its per-kind icon + color. Falls back to "main" when no symbol is
    // resolved (matching the prior static breadcrumb).
    let cur = ctx.outline.current();
    let (sym_name, sym_icon, sym_color) = if cur >= 0 {
        match ctx.outline.get(cur as usize) {
            Some(s) => (s.name.clone(), s.kind.icon(), s.kind.color()),
            None => ("main".to_string(), crate::icons::FN_SYMBOL, theme::SYN_FUNCTION()),
        }
    } else {
        ("main".to_string(), crate::icons::FN_SYMBOL, theme::SYN_FUNCTION())
    };
    if x + 18.0 <= text_right {
        ctx.dl_icon(x, icon_y, 13.0, 13.0, sym_icon, sym_color, 1.5, false);
        x += 13.0 + 5.0;
        let _ = queue_breadcrumb_segment(ctx, &mut x, &sym_name, sym_color, chrome, ty, clip, text_right);
    }

    // Right-aligned "Preview" pill — shown only when the active file is Markdown.
    // Clicking it (hit-tested by `mui_md_button_at_click`) opens the live preview.
    if let Some((bx, by, bw, bh)) = md_button {
        let active = ctx.md_pane.is_some() && ctx.md_preview.is_open();
        let (bg, fg) = if active {
            (theme::accent_a(0.18), theme::ACCENT_BRIGHT())
        } else {
            (theme::accent_a(0.10), theme::ACCENT())
        };
        ctx.dl_round(bx, by, bw, bh, 6.0, bg);
        ctx.dl_stroke(bx, by, bw, bh, 6.0, theme::accent_a(0.30), 1.0);
        ctx.dl_icon(bx + 8.0, by + (bh - 13.0) * 0.5, 13.0, 13.0, crate::icons::FILE_MD, fg, 1.5, false);
        ctx.text.queue_ui_sized(bx + 26.0, by + (bh - chrome) * 0.5 + 0.5, "Preview", fg, chrome - 0.5, clip);
    }
}

pub(crate) fn fit_breadcrumb_segment(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if s.contains('.') {
        fit_tab_label(text, s, max_px, size)
    } else {
        fit_status_head(text, s, max_px, size)
    }
}

fn queue_breadcrumb_segment(
    ctx: &mut MuiContext,
    x: &mut f32,
    s: &str,
    color: MuiColor,
    size: f32,
    y: f32,
    clip: Option<(u32, u32, u32, u32)>,
    right: f32,
) -> bool {
    let max_px = right - *x;
    if max_px <= 0.0 {
        return false;
    }
    let shown = fit_breadcrumb_segment(&mut ctx.text, s, max_px, size);
    if shown.is_empty() {
        return false;
    }
    ctx.text.queue_ui_sized(*x, y, &shown, color, size, clip);
    let (w, _) = ctx.text.measure_ui_sized(&shown, size);
    *x += w;
    shown == s
}

fn queue_breadcrumb_separator(ctx: &mut MuiContext, x: &mut f32, icon_y: f32, right: f32) -> bool {
    if *x + 20.0 > right {
        return false;
    }
    *x += 4.0;
    ctx.dl_icon(*x, icon_y, 12.0, 12.0, crate::icons::CHEVRON, theme::TEXT_4(), 1.5, false);
    *x += 12.0 + 4.0;
    true
}

/// The screen rect `(x, y, w, h)` of the breadcrumb "Preview" pill for window
/// width `w`, breadcrumb `top`, and bar height `bar_h`. Right-aligned.
pub(crate) fn md_button_rect(w: f32, top: f32, bar_h: f32) -> (f32, f32, f32, f32) {
    let bw = 92.0_f32;
    let bh = (bar_h - 10.0).max(16.0);
    let bx = w - bw - 12.0;
    let by = top + (bar_h - bh) * 0.5;
    (bx, by, bw, bh)
}

/// If the last click landed on the breadcrumb "Preview" pill (and the active file
/// is Markdown), open the live preview and return `1`; else `0`. The Mighty side
/// calls this on a click in the breadcrumb band before normal handling.
#[no_mangle]
pub extern "C" fn mui_md_button_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.language != crate::langdetect::Language::Markdown {
        return 0;
    }
    let w = ctx.gpu.width as f32;
    let top = layout::TAB_BAR_H;
    let bar_h = layout::BREADCRUMB_H;
    let (bx, by, bw, bh) = md_button_rect(w, top, bar_h);
    let (px, py) = (ctx.last_event.x, ctx.last_event.y);
    if px >= bx && px <= bx + bw && py >= by && py <= by + bh {
        mui_md_open(handle)
    } else {
        0
    }
}

/// Draw the tab bar across the top of the window (right of the activity rail):
/// one fixed-width cell per tab with its basename, a file-type dot, an ember
/// underline + dirty dot on the active tab. Mighty calls this once per frame.
#[no_mangle]
pub extern "C" fn mui_tab_bar_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let active = ctx.tabs.active();
    ensure_tab_visible(ctx, active);
    let count = ctx.tabs.count();
    let clip = ctx.clip;
    let bar_h = layout::TAB_BAR_H;
    let chrome = theme::CHROME_FONT_SIZE;
    // The tab bar lives over the editor column only — right of the rail AND the
    // sidebar (when shown), so it never overpaints the sidebar/header.
    let body_left = tab_bar_body_left(ctx);
    let tab_right = tab_bar_right(ctx);

    use crate::icons;
    // Tab-bar background (panel) + a thin bottom divider.
    ctx.dl_rect(body_left, 0.0, tab_right - body_left, bar_h, theme::BG_2());
    ctx.dl_rect(body_left, bar_h - 1.0, tab_right - body_left, 1.0, theme::BORDER());

    for i in ctx.tab_scroll..count {
        let Some((x, tab_w)) = tab_slot_rect(ctx, i) else { break };
        let is_active = i == active;
        // Active tab: editor-field bg + a top accent gradient bar (`.tab.active`).
        if is_active {
            ctx.dl_rect(x, 0.0, tab_w, bar_h, theme::BG_1());
            // Top 2px accent gradient bar with glow.
            ctx.dl_shadow(x, 0.0, tab_w, 2.0, 0.0, theme::ACCENT_GLOW(), 6.0);
            ctx.dl_rect(x, 0.0, tab_w, 2.0, theme::ACCENT());
        }
        // Right divider between tabs.
        ctx.dl_rect(x + tab_w - 1.0, 0.0, 1.0, bar_h, theme::BORDER_SOFT());
        if let Some(tab) = ctx.tabs.get(i) {
            let base = tab.basename();
            let dirty = tab.is_dirty();
            let (icon, icon_col) = file_icon_for(&base, is_active);
            let icon_y = (bar_h - 14.0) * 0.5;
            ctx.dl_icon(x + 14.0, icon_y, 14.0, 14.0, icon, icon_col, 1.4, false);
            let fg = if is_active { theme::TEXT() } else { theme::DIM() };
            let ty = (bar_h - chrome) * 0.5 - 1.0;
            let label_x = x + 34.0;
            let close_x = x + tab_w - 24.0;
            let label_right = if dirty { close_x - 12.0 } else { close_x - 6.0 };
            let label_max = (label_right - label_x).max(0.0);
            let label = fit_tab_label(&mut ctx.text, &base, label_max, chrome);
            // The ACTIVE tab's label reads in the bold UI face so the current
            // file stands out among the tabs.
            let style = if is_active {
                crate::vello_ui::FontStyle::Bold
            } else {
                crate::vello_ui::FontStyle::Regular
            };
            if !label.is_empty() {
                ctx.text.queue_ui_styled(label_x, ty, &label, fg, chrome, style, clip);
            }
            // Trailing affordance: always show close; dirty tabs keep a status dot.
            if dirty {
                ctx.dl_round(close_x - 8.0, bar_h * 0.5 - 2.5, 5.0, 5.0, 2.5, theme::ACCENT_BRIGHT());
            }
            let close_col = if is_active { theme::TEXT_1() } else { theme::TEXT_3() };
            ctx.dl_icon(close_x, (bar_h - 12.0) * 0.5, 12.0, 12.0, icons::CLOSE, close_col, 1.6, false);
        }
    }

    if ctx.tab_scroll > 0 {
        ctx.dl_grad_h(body_left, 0.0, 34.0, bar_h - 1.0, 0.0, theme::accent_a(0.18), 0.0);
        ctx.dl_shadow(body_left, 7.0, 4.0, bar_h - 14.0, 1.5, theme::ACCENT_GLOW(), 8.0);
        ctx.dl_round(body_left, 7.0, 4.0, bar_h - 14.0, 1.5, theme::ACCENT());
        ctx.dl_icon(body_left + 5.0, (bar_h - 11.0) * 0.5, 11.0, 11.0, icons::ARROW_LEFT, theme::ACCENT_BRIGHT(), 1.4, false);
    }
    if ctx.tab_scroll < tab_max_scroll(ctx) {
        ctx.dl_grad_h(tab_right - 34.0, 0.0, 34.0, bar_h - 1.0, 0.0, theme::accent_a(0.12), 0.9);
        ctx.dl_shadow(tab_right - 4.0, 7.0, 4.0, bar_h - 14.0, 1.5, theme::ACCENT_GLOW(), 8.0);
        ctx.dl_round(tab_right - 4.0, 7.0, 4.0, bar_h - 14.0, 1.5, theme::ACCENT());
    }

    if let Some((x, y, w, h)) = topbar_command_center_rect(ctx) {
        let bg = theme::accent_a(0.08);
        ctx.dl_round(x, y, w, h, 7.0, bg);
        ctx.dl_stroke(x, y, w, h, 7.0, theme::BORDER_SOFT(), 1.0);
        ctx.dl_icon(x + 10.0, y + 5.0, 14.0, 14.0, icons::SEARCH, theme::TEXT_3(), 1.4, false);
        let label = if w < 250.0 {
            "Quick Open"
        } else {
            "Quick Open files and commands"
        };
        let label_max = w - 42.0;
        let label = fit_status_tail(&mut ctx.text, label, label_max, chrome - 1.0);
        ctx.text
            .queue_ui_sized(x + 30.0, y + 5.0, &label, theme::TEXT_1(), chrome - 1.0, clip);
    }
}

/// Draw the borderless title-bar controls (minimize / maximize / close) at the
/// far right of the top row, plus the run + more-actions icons just left of them.
/// Drawn as a SEPARATE late pass (after the docked panels) so a right-docked panel
/// like the AI copilot can never occlude the window controls — previously these
/// lived at the end of `mui_tab_bar_draw` and the AI panel painted over them, so
/// min/max/close vanished whenever that panel was open.
#[no_mangle]
pub extern "C" fn mui_window_controls_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    use crate::icons;
    let w = ctx.gpu.width as f32;
    let wh = ctx.gpu.height as f32;
    // Subtle 1px window frame: gives the borderless window a visible edge so the
    // resize-grab band is discoverable (was invisible -> "guessing where to drag").
    ctx.dl_rect(0.0, 0.0, w, 1.0, theme::BORDER());
    ctx.dl_rect(0.0, wh - 1.0, w, 1.0, theme::BORDER());
    ctx.dl_rect(0.0, 0.0, 1.0, wh, theme::BORDER());
    ctx.dl_rect(w - 1.0, 0.0, 1.0, wh, theme::BORDER());
    // Visible corner grips for the borderless resize affordance. These stay
    // subtle but give users a target instead of a hidden one-pixel frame.
    let grip = theme::BORDER_STRONG();
    for offset in [7.0_f32, 14.0] {
        let len = 19.0 - (offset - 7.0);
        ctx.dl_rect(w - offset - len, wh - offset, len, 1.0, grip);
        ctx.dl_rect(w - offset, wh - offset - len, 1.0, len, grip);
        ctx.dl_rect(offset, wh - offset, len, 1.0, grip);
        ctx.dl_rect(offset, wh - offset - len, 1.0, len, grip);
    }
    let bar_h = layout::TAB_BAR_H;
    let btn_w = crate::titlebar::BTN_W;
    let controls_x = crate::titlebar::controls_x(w);
    let maximized = ctx.window_maximized;
    let icon_d = 14.0;
    let iy = (bar_h - icon_d) * 0.5;

    // A solid backing under the controls + run/dots so any panel drawn beneath
    // can't bleed through. Match the tab bar instead of the rail; otherwise the
    // action strip reads like a dead tab-shaped block.
    let strip_x = controls_x - 60.0 - 8.0;
    ctx.dl_rect(strip_x, 0.0, w - strip_x, bar_h, theme::BG_2());
    ctx.dl_rect(strip_x, 0.0, 1.0, bar_h, theme::BORDER_SOFT());

    // Run + more-actions (just left of the window controls).
    let ax = controls_x - 60.0;
    let ay = (bar_h - 16.0) * 0.5;
    for (bx, col) in [(ax - 7.0, theme::green_wash(0.12)), (ax + 21.0, theme::accent_a(0.08))] {
        ctx.dl_round(bx, 8.0, 30.0, 28.0, 7.0, col);
        ctx.dl_stroke(bx, 8.0, 30.0, 28.0, 7.0, theme::BORDER_SOFT(), 1.0);
    }
    ctx.dl_icon(ax, ay, 16.0, 16.0, icons::RUN, theme::GREEN(), 1.5, true);
    ctx.dl_icon(ax + 28.0, ay, 16.0, 16.0, icons::DOTS, theme::TEXT_1(), 0.0, true);

    // Minimize / maximize-restore / close. Close gets a red tint.
    for (i, path) in [
        icons::WIN_MIN,
        if maximized { icons::WIN_RESTORE } else { icons::WIN_MAX },
        icons::CLOSE,
    ]
    .iter()
    .enumerate()
    {
        let bx = controls_x + i as f32 * btn_w;
        let col = if i == 2 { theme::ERROR() } else { theme::TEXT_3() };
        let cx = bx + (btn_w - icon_d) * 0.5;
        ctx.dl_icon(cx, iy, icon_d, icon_d, path, col, 1.5, false);
    }
    ctx.overlay = was_overlay;
}

/// Hit-test the top command/action strip. Returns 1 = run, 2 = more-actions,
/// 3 = command center / Quick Open, else 0.
/// Geometry mirrors the reserved action strip from `titlebar`: the left slot is
/// Run and the wider right slot opens More/command palette. The whole strip is
/// actionable so DPI rounding and padding around the glyphs do not fall through
/// into the editor.
#[no_mangle]
pub extern "C" fn mui_topbar_action_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if layout::zen_active() {
        return 0;
    }
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if y < 0.0 || y >= layout::TAB_BAR_H {
        return 0;
    }
    if topbar_command_center_hit(ctx, x, y) {
        trace(&format!("topbar_action x={x:.1} y={y:.1} -> command-center"));
        return 3;
    }
    let controls_x = crate::titlebar::controls_x(ctx.gpu.width as f32);
    let strip_x = controls_x - crate::titlebar::ACTION_STRIP_W;
    let run_right = strip_x + 30.0;
    if x >= strip_x && x < run_right {
        trace(&format!("topbar_action x={x:.1} y={y:.1} -> run"));
        return 1;
    }
    if x >= run_right && x < controls_x {
        trace(&format!("topbar_action x={x:.1} y={y:.1} -> more"));
        return 2;
    }
    if y >= 0.0 && y < layout::TAB_BAR_H && x >= strip_x - 12.0 && x < controls_x + 12.0 {
        trace(&format!(
            "topbar_action x={x:.1} y={y:.1} miss strip=[{strip_x:.1},{controls_x:.1})"
        ));
    }
    0
}

/// Hit-test the Explorer header action icons (new file / new folder / collapse),
/// drawn in `mui_sidebar_draw` as three spaced icon buttons in the 40px header
/// band. Returns 1 = new file, 2 = new folder, 3 = collapse all, else 0.
/// (Only meaningful while the Explorer panel + sidebar are visible.)
pub(crate) fn explorer_header_action_opens_dialog(action: i32) -> bool {
    matches!(action, 1 | 2)
}

pub(crate) fn explorer_header_action_centers(sx: f32, sw: f32) -> [(f32, i32); 3] {
    [
        (sx + sw - 87.5, 1),
        (sx + sw - 57.5, 2),
        (sx + sw - 27.5, 3),
    ]
}

#[no_mangle]
pub extern "C" fn mui_explorer_header_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible {
        return 0;
    }
    if ctx.active_panel != crate::PANEL_EXPLORER {
        return 0;
    }
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if y < 0.0 || y >= 40.0 {
        return 0;
    }
    let right = layout::sidebar_right();
    for (cx, action) in explorer_header_action_centers(layout::RAIL_W, layout::sidebar_w()) {
        if x >= cx - 3.0 && x < cx + 18.0 {
            let label = match action {
                1 => "new-file",
                2 => "new-folder",
                _ => "collapse",
            };
            trace(&format!("explorer_header x={x:.1} y={y:.1} -> {label}"));
            return action;
        }
    }
    if x >= right - 100.0 && x < right - 4.0 {
        trace(&format!("explorer_header x={x:.1} y={y:.1} -> miss"));
    }
    0
}

/// New File: open a fresh untitled tab and make it active. Returns its index.
#[no_mangle]
pub extern "C" fn mui_tab_new_untitled(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let idx = ctx.tabs.new_untitled();
    sync_active_path(ctx);
    ctx.welcome.dismiss_empty_auto();
    idx as i32
}

/// Collapse every expanded folder in the file tree (Explorer "collapse all").
#[no_mangle]
pub extern "C" fn mui_tree_collapse_all(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.tree.collapse_all();
    }
}

/// Create a new folder from the staged path bytes (the New Folder prompt query),
/// resolved under the workspace root. Refreshes the tree. Returns 1 on success.
#[no_mangle]
pub extern "C" fn mui_newfolder_create(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let raw = String::from_utf8_lossy(&staged).into_owned();
    let name = match crate::newproj::validate_name(&raw) {
        Ok(n) => n,
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("newfolder: invalid name: {e}");
            return 0;
        }
    };
    let base = crate::wsabi::effective_root(ctx);
    let target = base.join(&name);
    if target.exists() {
        refresh_workspace_file_views(ctx);
        ctx.push_toast(crate::toast::Kind::Warn, format!("Folder already exists: {name}"));
        println!("newfolder: target already exists: {}", target.display());
        return 0;
    }
    match std::fs::create_dir(&target) {
        Ok(_) => {
            refresh_workspace_file_views(ctx);
            ctx.push_toast(crate::toast::Kind::Success, format!("Created folder: {name}"));
            1
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Folder create", &target, &e),
            );
            println!("newfolder: failed to create {}: {e}", target.display());
            0
        }
    }
}

/// Create/select a folder through the native Windows folder picker. Returns `1`
/// when a folder is ready, `0` on cancel, or `-1` when unavailable so Mighty can
/// fall back to the typed-name prompt.
#[no_mangle]
pub extern "C" fn mui_newfolder_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let initial_dir = crate::wsabi::effective_root(ctx);
    let target = match pick_new_folder_native(&initial_dir, dialog_owner_hwnd(ctx)) {
        FileDialogPick::Picked(path) => path,
        FileDialogPick::Cancelled => {
            println!("mui_newfolder_dialog: native folder dialog cancelled");
            ctx.push_toast(crate::toast::Kind::Info, "New folder cancelled");
            return 0;
        }
        FileDialogPick::Unavailable => {
            println!("mui_newfolder_dialog: native folder dialog unavailable");
            ctx.push_toast(crate::toast::Kind::Warn, "New folder dialog unavailable");
            return -1;
        }
    };
    let name = basename(&target);
    create_or_accept_folder_at(ctx, target, &initial_dir, &name)
}

/// Create a new Mighty project through a native folder picker. The selected path
/// is the final project folder name. Returns `1` on success, `0` on cancel or a
/// rejected folder, and `-1` when native dialogs are unavailable so Mighty can
/// fall back to the typed-name prompt.
#[no_mangle]
pub extern "C" fn mui_newproj_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let root = crate::wsabi::effective_root(ctx);
    let initial_dir = crate::newproj::resolve_parent_dir(Some(&root));
    let target = match pick_new_project_native(&initial_dir, dialog_owner_hwnd(ctx)) {
        FileDialogPick::Picked(path) => path,
        FileDialogPick::Cancelled => {
            trace("new_project_dialog cancel");
            println!("mui_newproj_dialog: native project folder dialog cancelled");
            ctx.push_toast(crate::toast::Kind::Info, "New project cancelled");
            return 0;
        }
        FileDialogPick::Unavailable => {
            trace("new_project_dialog unavailable");
            println!("mui_newproj_dialog: native project folder dialog unavailable");
            ctx.push_toast(crate::toast::Kind::Warn, "New project dialog unavailable");
            return -1;
        }
    };
    trace(&format!("new_project_dialog path={}", target.display()));
    let created = crate::newprojabi::create_project_at(ctx, target);
    trace(&format!("new_project_dialog result={created}"));
    created
}

fn path_is_inside_workspace(root: &std::path::Path, target: &std::path::Path) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if let Ok(target) = std::fs::canonicalize(target) {
        return target.starts_with(&root);
    }
    target
        .parent()
        .map(|parent| {
            std::fs::canonicalize(parent)
                .unwrap_or_else(|_| parent.to_path_buf())
                .starts_with(&root)
        })
        .unwrap_or(false)
}

fn create_or_accept_folder_at(
    ctx: &mut MuiContext,
    target: PathBuf,
    workspace_root: &std::path::Path,
    name: &str,
) -> i32 {
    if let Err(e) = crate::newproj::validate_platform_segment(name) {
        ctx.push_toast(crate::toast::Kind::Warn, e.clone());
        println!("newfolder: invalid selected folder name: {e}");
        return 0;
    }
    if !path_is_inside_workspace(workspace_root, &target) {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            outside_workspace_pick_message("folder", &target, workspace_root),
        );
        println!(
            "newfolder: selected folder is outside workspace: {} (root {})",
            target.display(),
            workspace_root.display()
        );
        return 0;
    }
    if target.is_dir() {
        refresh_workspace_file_views(ctx);
        ctx.push_toast(crate::toast::Kind::Success, format!("Folder ready: {name}"));
        return 1;
    }
    if target.exists() {
        refresh_workspace_file_views(ctx);
        ctx.push_toast(crate::toast::Kind::Warn, format!("Folder already exists: {name}"));
        println!("newfolder: target exists but is not a folder: {}", target.display());
        return 0;
    }
    match std::fs::create_dir_all(&target) {
        Ok(_) => {
            refresh_workspace_file_views(ctx);
            ctx.push_toast(crate::toast::Kind::Success, format!("Created folder: {name}"));
            1
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Folder create", &target, &e),
            );
            println!("newfolder: failed to create {}: {e}", target.display());
            0
        }
    }
}

/// Create a new file from the staged path bytes (the Explorer New File prompt
/// query), resolved under the workspace root. Opens the file as the active tab,
/// refreshes Explorer and Quick-Open, and returns the resulting tab index.
#[no_mangle]
pub extern "C" fn mui_newfile_create(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let raw = String::from_utf8_lossy(&staged).into_owned();
    let name = match crate::newproj::validate_name(&raw) {
        Ok(n) => n,
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("newfile: invalid name: {e}");
            return -1;
        }
    };
    let base = crate::wsabi::effective_root(ctx);
    let target = base.join(&name);
    create_new_file_at(ctx, target, &base, &name, true)
}

/// Create a new workspace file through the native SaveFileDialog path picker.
/// Returns the new tab index, `-2` on cancel/user no-op, or `-1` when the native
/// picker is unavailable so Mighty can fall back to the typed name prompt.
#[no_mangle]
pub extern "C" fn mui_newfile_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let workspace_root = crate::wsabi::effective_root(ctx);
    let initial_dir = file_dialog_initial_dir(ctx);
    let owner_hwnd = dialog_owner_hwnd(ctx);
    let target = match pick_new_file_native(&initial_dir, owner_hwnd) {
        FileDialogPick::Picked(path) => path,
        FileDialogPick::Cancelled => {
            trace("new_file_dialog cancel");
            println!("mui_newfile_dialog: native new-file dialog cancelled");
            ctx.push_toast(crate::toast::Kind::Info, "New file cancelled");
            return -2;
        }
        FileDialogPick::Unavailable => {
            trace("new_file_dialog unavailable");
            println!("mui_newfile_dialog: native new-file dialog unavailable");
            ctx.push_toast(crate::toast::Kind::Warn, "New file dialog unavailable");
            return -1;
        }
    };
    trace(&format!("new_file_dialog path={}", target.display()));
    let name = basename(&target);
    create_new_file_at(ctx, target, &workspace_root, &name, false)
}

/// Create a new workspace file through the native SaveFileDialog path picker.
/// Unlike [`mui_newfile_dialog`], this command is constrained to the current
/// workspace root so Explorer's "new file" action cannot accidentally create a
/// file elsewhere on disk.
#[no_mangle]
pub extern "C" fn mui_newfile_workspace_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let workspace_root = crate::wsabi::effective_root(ctx);
    let owner_hwnd = dialog_owner_hwnd(ctx);
    let target = match pick_new_file_native(&workspace_root, owner_hwnd) {
        FileDialogPick::Picked(path) => path,
        FileDialogPick::Cancelled => {
            trace("new_workspace_file_dialog cancel");
            println!("mui_newfile_workspace_dialog: native new-file dialog cancelled");
            ctx.push_toast(crate::toast::Kind::Info, "New workspace file cancelled");
            return -2;
        }
        FileDialogPick::Unavailable => {
            trace("new_workspace_file_dialog unavailable");
            println!("mui_newfile_workspace_dialog: native new-file dialog unavailable");
            ctx.push_toast(crate::toast::Kind::Warn, "New workspace file dialog unavailable");
            return -1;
        }
    };
    trace(&format!("new_workspace_file_dialog path={}", target.display()));
    let name = basename(&target);
    create_new_file_at(ctx, target, &workspace_root, &name, true)
}

fn create_new_file_at(
    ctx: &mut MuiContext,
    target: PathBuf,
    workspace_root: &std::path::Path,
    name: &str,
    require_workspace: bool,
) -> i32 {
    if let Err(e) = crate::newproj::validate_platform_segment(name) {
        ctx.push_toast(crate::toast::Kind::Warn, e.clone());
        println!("newfile: invalid selected file name: {e}");
        return -2;
    }
    if require_workspace && !path_is_inside_workspace(workspace_root, &target) {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            outside_workspace_pick_message("file", &target, workspace_root),
        );
        println!(
            "newfile: selected file is outside workspace: {} (root {})",
            target.display(),
            workspace_root.display()
        );
        return -2;
    }
    if target.exists() {
        refresh_workspace_file_views(ctx);
        ctx.push_toast(crate::toast::Kind::Warn, format!("File already exists: {name}"));
        println!("newfile: target already exists: {}", target.display());
        return -2;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&target) {
        Ok(_) => {
            let idx = ctx.tabs.open_path(target.clone());
            sync_active_path(ctx);
            record_recent_file(ctx, target.clone());
            refresh_workspace_file_views(ctx);
            ctx.welcome.dismiss();
            ctx.push_toast(crate::toast::Kind::Success, format!("Created file: {name}"));
            idx as i32
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("File create", &target, &e),
            );
            println!("newfile: failed to create {}: {e}", target.display());
            -2
        }
    }
}

/// Rename the active file to the staged single-segment basename. Keeps the tab,
/// active language, Explorer tree, and Quick-Open index aligned with the move.
#[no_mangle]
pub extern "C" fn mui_file_rename_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(old_path) = ctx.tabs.active_path() else {
        ctx.path_stage.clear();
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("No active file to rename: {}", active_file_target_name(ctx)),
        );
        return 0;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let raw = String::from_utf8_lossy(&staged).into_owned();
    let name = match crate::newproj::validate_name(&raw) {
        Ok(n) => n,
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("file-rename: invalid name: {e}");
            return 0;
        }
    };
    let Some(parent) = old_path.parent().map(|p| p.to_path_buf()) else {
        ctx.push_toast(crate::toast::Kind::Warn, "Cannot rename this path");
        return 0;
    };
    let new_path = parent.join(&name);
    if new_path == old_path {
        ctx.push_toast(crate::toast::Kind::Info, format!("Already named {name}"));
        return 1;
    }
    if new_path.exists() {
        ctx.push_toast(crate::toast::Kind::Warn, format!("File already exists: {name}"));
        return 0;
    }
    match std::fs::rename(&old_path, &new_path) {
        Ok(()) => {
            let rebound = ctx.tabs.rebind_path(&old_path, new_path.clone());
            if rebound == 0 {
                ctx.tabs.set_active_path(new_path.clone());
            }
            sync_active_path(ctx);
            ctx.quickopen.remove_recent_path(&old_path);
            record_recent_file(ctx, new_path.clone());
            refresh_workspace_file_views(ctx);
            ctx.push_toast(crate::toast::Kind::Success, format!("Renamed to {name}"));
            println!("file-rename: {} -> {}", old_path.display(), new_path.display());
            1
        }
        Err(e) => {
            refresh_workspace_file_views(ctx);
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Rename", &new_path, &e),
            );
            println!("file-rename: failed {} -> {}: {e}", old_path.display(), new_path.display());
            0
        }
    }
}

/// Reveal the active file in Explorer by expanding parent folders. Returns the
/// visible row index, or -1 if there is no active file / it is outside the root.
#[no_mangle]
pub extern "C" fn mui_file_reveal_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("No active file to reveal: {}", active_file_target_name(ctx)),
        );
        return -1;
    };
    ctx.sidebar_visible = true;
    match ctx.tree.reveal(&path) {
        Some(i) => {
            ctx.push_toast(crate::toast::Kind::Info, format!("Revealed {}", basename(&path)));
            i as i32
        }
        None => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                reveal_outside_root_message(&path, ctx.tree.root()),
            );
            -1
        }
    }
}

pub(crate) fn platform_reveal_command(path: &std::path::Path) -> Option<(String, Vec<String>)> {
    if std::env::var_os("MUI_FILE_REVEAL_FORCE_UNAVAILABLE").is_some() {
        let _ = path;
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        Some(("explorer.exe".to_string(), vec![format!("/select,{}", path.display())]))
    }
    #[cfg(target_os = "macos")]
    {
        Some(("open".to_string(), vec!["-R".to_string(), path.display().to_string()]))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        Some(("xdg-open".to_string(), vec![dir.display().to_string()]))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = path;
        None
    }
}

pub(crate) fn file_manager_reveal_failed_message(
    path: &std::path::Path,
    e: &std::io::Error,
) -> String {
    let name = basename(path);
    let reason = e.to_string();
    if reason.trim().is_empty() {
        format!("Could not show {name} in file manager")
    } else {
        format!("Could not show {name} in file manager: {}", reason.trim())
    }
}

pub(crate) fn reveal_outside_root_message(
    path: &std::path::Path,
    root: &std::path::Path,
) -> String {
    let file = basename(path);
    let root_name = basename(root);
    if root_name.is_empty() || root_name == "." {
        format!("{file} is outside Explorer root")
    } else {
        format!("{file} is outside Explorer root: {root_name}")
    }
}

pub(crate) fn outside_workspace_pick_message(
    kind: &str,
    target: &std::path::Path,
    root: &std::path::Path,
) -> String {
    let target_name = basename(target);
    let root_name = basename(root);
    if root_name.is_empty() || root_name == "." {
        format!("Choose a {kind} inside the workspace: {target_name}")
    } else {
        format!("Choose a {kind} inside the workspace: {target_name} -> {root_name}")
    }
}

pub(crate) fn file_manager_reveal_unavailable_message(path: &std::path::Path) -> String {
    format!("Reveal in file manager is unavailable: {}", basename(path))
}

/// Reveal the active file in the operating system's file manager. Returns 1
/// when the reveal command was launched, else 0.
#[no_mangle]
pub extern "C" fn mui_file_reveal_active_in_os(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("No active file to reveal: {}", active_file_target_name(ctx)),
        );
        return 0;
    };
    let Some((program, args)) = platform_reveal_command(&path) else {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            file_manager_reveal_unavailable_message(&path),
        );
        return 0;
    };
    let forced_error = std::env::var("MUI_FILE_REVEAL_FORCE_SPAWN_ERROR")
        .ok()
        .map(|reason| std::io::Error::new(std::io::ErrorKind::Other, reason));
    let launch = match forced_error {
        Some(e) => Err(e),
        None => std::process::Command::new(&program).args(&args).spawn().map(|_| ()),
    };
    match launch {
        Ok(_) => {
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Showing {} in file manager", basename(&path)),
            );
            1
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_manager_reveal_failed_message(&path, &e),
            );
            println!("file-reveal-os: failed to launch {program} {:?} for {}: {e}", args, path.display());
            0
        }
    }
}

pub(crate) fn platform_clipboard_command() -> Option<(String, Vec<String>)> {
    #[cfg(target_os = "windows")]
    {
        Some((
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Set-Clipboard -Value ([Console]::In.ReadToEnd())".to_string(),
            ],
        ))
    }
    #[cfg(target_os = "macos")]
    {
        Some(("pbcopy".to_string(), Vec::new()))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::process::Command::new("wl-copy").arg("--version").output().is_ok() {
            Some(("wl-copy".to_string(), Vec::new()))
        } else {
            Some(("xclip".to_string(), vec!["-selection".to_string(), "clipboard".to_string()]))
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        None
    }
}

pub(crate) fn platform_clipboard_read_command() -> Option<(String, Vec<String>)> {
    #[cfg(target_os = "windows")]
    {
        Some((
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Get-Clipboard -Raw".to_string(),
            ],
        ))
    }
    #[cfg(target_os = "macos")]
    {
        Some(("pbpaste".to_string(), Vec::new()))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::process::Command::new("wl-paste").arg("--version").output().is_ok() {
            Some(("wl-paste".to_string(), vec!["--no-newline".to_string()]))
        } else {
            Some(("xclip".to_string(), vec!["-selection".to_string(), "clipboard".to_string(), "-o".to_string()]))
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        None
    }
}

fn write_clipboard_text(text: &str) -> std::io::Result<()> {
    use std::io::Write;

    if let Ok(reason) = std::env::var("MUI_CLIPBOARD_WRITE_FORCE_FAIL") {
        let reason = if reason.trim().is_empty() {
            "clipboard command failed".to_string()
        } else {
            reason
        };
        return Err(std::io::Error::new(std::io::ErrorKind::Other, reason));
    }
    let Some((program, args)) = platform_clipboard_command() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "clipboard command unavailable",
        ));
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "clipboard command failed"))
    }
}

fn clipboard_write_failure_message(action: &str, e: &std::io::Error) -> String {
    let msg = e.to_string();
    if msg.trim().is_empty() {
        format!("Could not {action} text")
    } else {
        format!("Could not {action} text: {msg}")
    }
}

fn read_clipboard_text() -> std::io::Result<String> {
    if let Ok(text) = std::env::var("MUI_CLIPBOARD_TEXT") {
        return Ok(text);
    }
    if let Ok(reason) = std::env::var("MUI_CLIPBOARD_READ_FORCE_FAIL") {
        let reason = if reason.trim().is_empty() {
            "clipboard read failed".to_string()
        } else {
            reason
        };
        return Err(std::io::Error::new(std::io::ErrorKind::Other, reason));
    }
    let Some((program, args)) = platform_clipboard_read_command() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "clipboard command unavailable",
        ));
    };
    let output = std::process::Command::new(program).args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "clipboard read failed"))
    }
}

fn clipboard_failure_message(e: &std::io::Error) -> String {
    let msg = e.to_string();
    if msg.trim().is_empty() {
        "Clipboard paste failed".to_string()
    } else {
        format!("Clipboard paste failed: {msg}")
    }
}

pub(crate) fn active_relative_path_text(ctx: &MuiContext, path: &std::path::Path) -> String {
    let root = if !ctx.workspace.is_empty() {
        Some(ctx.workspace.root())
    } else if !ctx.tree.root().as_os_str().is_empty() {
        Some(ctx.tree.root())
    } else {
        None
    };
    root.and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn active_file_name_text(path: &std::path::Path) -> String {
    basename(path)
}

pub(crate) fn active_directory_text(path: &std::path::Path) -> String {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_string_lossy()
        .replace('\\', "/")
}

fn append_clipboard_write_reason(base: String, e: &std::io::Error) -> String {
    let msg = e.to_string();
    if msg.trim().is_empty() {
        base
    } else {
        format!("{base}: {msg}")
    }
}

pub(crate) fn copy_path_failed_message(path: &std::path::Path, e: &std::io::Error) -> String {
    append_clipboard_write_reason(format!("Could not copy path: {}", basename(path)), e)
}

pub(crate) fn copy_relative_path_failed_message(text: &str, e: &std::io::Error) -> String {
    append_clipboard_write_reason(format!("Could not copy relative path: {text}"), e)
}

pub(crate) fn copy_file_name_failed_message(text: &str, e: &std::io::Error) -> String {
    append_clipboard_write_reason(format!("Could not copy file name: {text}"), e)
}

pub(crate) fn copy_directory_failed_message(text: &str, e: &std::io::Error) -> String {
    append_clipboard_write_reason(format!("Could not copy directory: {text}"), e)
}

fn active_file_target_name(ctx: &MuiContext) -> String {
    ctx.tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string())
}

fn copy_needs_file_message(ctx: &MuiContext, what: &str) -> String {
    format!("No active file {what} to copy: {}", active_file_target_name(ctx))
}

/// Copy the active file path to the operating-system clipboard. Returns 1 on
/// success, else 0.
#[no_mangle]
pub extern "C" fn mui_file_copy_active_path(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.push_toast(crate::toast::Kind::Warn, copy_needs_file_message(ctx, "path"));
        return 0;
    };
    let text = path.display().to_string();
    match write_clipboard_text(&text) {
        Ok(()) => {
            ctx.push_toast(crate::toast::Kind::Success, format!("Copied path: {}", basename(&path)));
            1
        }
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Error, copy_path_failed_message(&path, &e));
            println!("file-copy-path: failed for {}: {e}", path.display());
            0
        }
    }
}

/// Copy the active file path relative to the workspace/tree root. Falls back to
/// the absolute path when the file is outside the known root.
#[no_mangle]
pub extern "C" fn mui_file_copy_active_relative_path(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.push_toast(crate::toast::Kind::Warn, copy_needs_file_message(ctx, "relative path"));
        return 0;
    };
    let text = active_relative_path_text(ctx, &path);
    match write_clipboard_text(&text) {
        Ok(()) => {
            ctx.push_toast(crate::toast::Kind::Success, format!("Copied relative path: {text}"));
            1
        }
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Error, copy_relative_path_failed_message(&text, &e));
            println!("file-copy-relative-path: {e}");
            0
        }
    }
}

/// Copy only the active file name to the operating-system clipboard.
#[no_mangle]
pub extern "C" fn mui_file_copy_active_name(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.push_toast(crate::toast::Kind::Warn, copy_needs_file_message(ctx, "name"));
        return 0;
    };
    let text = active_file_name_text(&path);
    match write_clipboard_text(&text) {
        Ok(()) => {
            ctx.push_toast(crate::toast::Kind::Success, format!("Copied file name: {text}"));
            1
        }
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Error, copy_file_name_failed_message(&text, &e));
            println!("file-copy-name: {e}");
            0
        }
    }
}

/// Copy the active file's containing directory to the operating-system clipboard.
#[no_mangle]
pub extern "C" fn mui_file_copy_active_directory(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.push_toast(crate::toast::Kind::Warn, copy_needs_file_message(ctx, "directory"));
        return 0;
    };
    let text = active_directory_text(&path);
    match write_clipboard_text(&text) {
        Ok(()) => {
            ctx.push_toast(crate::toast::Kind::Success, format!("Copied directory: {text}"));
            1
        }
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Error, copy_directory_failed_message(&text, &e));
            println!("file-copy-directory: {e}");
            0
        }
    }
}

/// Delete the active file after the prompt stages an exact basename confirmation.
/// The active tab is closed on success and Explorer / Quick-Open are refreshed.
#[no_mangle]
pub extern "C" fn mui_file_delete_active_confirm(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        ctx.path_stage.clear();
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("No active file to delete: {}", active_file_target_name(ctx)),
        );
        return 0;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let confirm = String::from_utf8_lossy(&staged).trim().to_string();
    let name = basename(&path);
    if confirm != name {
        ctx.push_toast(crate::toast::Kind::Warn, format!("Type {name} to delete"));
        return 0;
    }
    if ctx.tabs.any_dirty_path(&path) {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Save or discard changes in {name} before deleting"),
        );
        return 0;
    }
    let deleted = match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Delete", &path, &e),
            );
            println!("file-delete: failed {}: {e}", path.display());
            false
        }
    };
    if deleted {
        ctx.pending_dirty_close = None;
        let compaction = ctx.tabs.close_clean_path_forget(&path);
        if let Some(compaction) = compaction {
            ctx.panes.on_tabs_compacted(&compaction.old_to_new, ctx.tabs.count());
        }
        sync_active_path(ctx);
        let a = ctx.tabs.active();
        ensure_tab_visible(ctx, a);
        remove_recent_file(ctx, &path);
        refresh_workspace_file_views(ctx);
        ctx.push_toast(crate::toast::Kind::Success, format!("Deleted {name}"));
        println!("file-delete: {}", path.display());
        1
    } else {
        0
    }
}

/// Pick a vector file icon + color for a basename. Active tabs / `.mty` use the
/// accent; `.toml` warns, `.md` info, else generic dim.
pub(crate) fn file_icon_for(base: &str, active: bool) -> (&'static str, MuiColor) {
    use crate::icons;
    if base.ends_with(".mty") {
        (icons::FILE_MTY, if active { theme::ACCENT_BRIGHT() } else { theme::SYN_TYPE() })
    } else if base.ends_with(".toml") {
        (icons::FILE_TOML, theme::WARNING())
    } else if base.ends_with(".md") {
        (icons::FILE_MD, theme::INFO())
    } else {
        (icons::FILE_TXT, theme::TEXT_3())
    }
}

// ---------------------------------------------------------------------------
// Multi-file workspace — file-tree sidebar
// ---------------------------------------------------------------------------

/// Whether the sidebar is currently shown (1) or hidden (0).
#[no_mangle]
pub extern "C" fn mui_sidebar_visible(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.sidebar_visible { 1 } else { 0 })
}

/// Toggle the sidebar's visibility. Returns the new state (1 shown / 0 hidden).
#[no_mangle]
pub extern "C" fn mui_sidebar_toggle(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.sidebar_visible = !ctx.sidebar_visible;
    if ctx.sidebar_visible {
        ctx.push_toast(crate::toast::Kind::Info, "Sidebar opened");
        trace("sidebar_toggle opened");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Sidebar closed");
        trace("sidebar_toggle closed");
        0
    }
}

/// Close the left sidebar drawer without changing its active panel. Returns `1`
/// when a visible sidebar was closed, or `0` when it was already hidden.
#[no_mangle]
pub extern "C" fn mui_sidebar_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible {
        ctx.push_toast(crate::toast::Kind::Info, "Sidebar is already closed");
        trace("sidebar_close noop");
        return 0;
    }
    ctx.sidebar_visible = false;
    ctx.push_toast(crate::toast::Kind::Info, "Sidebar closed");
    trace("sidebar_close");
    1
}

/// Apply a sidebar width preset from the palette.
/// `94` = compact, `95` = default/auto, `96` = wide, `102` = cycle width.
/// Returns the preset number (`1..=3`) or `0` for an unrelated command id.
#[no_mangle]
pub extern "C" fn mui_sidebar_layout_dispatch(handle: i64, id: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (preset, label, code) = match id as u32 {
        crate::palette::CMD_SIDEBAR_COMPACT => (1_u8, "Sidebar compact", 1),
        crate::palette::CMD_SIDEBAR_DEFAULT => (0_u8, "Sidebar default width", 2),
        crate::palette::CMD_SIDEBAR_WIDE => (2_u8, "Sidebar wide", 3),
        crate::palette::CMD_SIDEBAR_CYCLE_WIDTH => match layout::sidebar_preset() {
            0 => (1_u8, "Sidebar compact", 1),
            1 => (2_u8, "Sidebar wide", 3),
            _ => (0_u8, "Sidebar default width", 2),
        },
        _ => return 0,
    };
    layout::set_sidebar_preset(preset);
    if !ctx.sidebar_visible {
        ctx.sidebar_visible = true;
        ctx.active_panel = crate::PANEL_EXPLORER;
    }
    ctx.push_toast(crate::toast::Kind::Info, label);
    trace(&format!(
        "sidebar_layout_dispatch id={id} preset={preset} width={:.1} {label}",
        layout::sidebar_w()
    ));
    code
}

/// Hit-test and capture the sidebar divider for direct mouse resizing.
#[no_mangle]
pub extern "C" fn mui_sidebar_resize_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (_, visible_h) = visible_surface_size(ctx);
    if ctx.last_event.button == crate::ffi::MUI_MOUSE_LEFT
        && layout::sidebar_resize_hit(ctx.sidebar_visible, ctx.last_event.x, ctx.last_event.y, visible_h)
    {
        ctx.sidebar_resizing = true;
        ctx.sidebar_resize_grab_dx = layout::sidebar_right() - ctx.last_event.x;
        trace(&format!(
            "sidebar_resize start x={:.1} grab_dx={:.1} width={:.1}",
            ctx.last_event.x,
            ctx.sidebar_resize_grab_dx,
            layout::sidebar_w()
        ));
        1
    } else {
        0
    }
}

/// Resize the sidebar so its divider follows the latest mouse event.
/// Returns the resulting sidebar width in pixels for deterministic tests.
#[no_mangle]
pub extern "C" fn mui_sidebar_resize_to_event_x(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        if !c.sidebar_visible {
            return 0;
        }
        let edge_x = c.last_event.x + c.sidebar_resize_grab_dx;
        let width = layout::resize_sidebar_to_x(edge_x).round() as i32;
        trace(&format!(
            "sidebar_resize drag x={:.1} edge_x={edge_x:.1} width={width}",
            c.last_event.x
        ));
        width
    })
}

/// Finish a manual sidebar resize and acknowledge the resulting width.
#[no_mangle]
pub extern "C" fn mui_sidebar_resize_finish(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible {
        return 0;
    }
    let width = layout::sidebar_w().round() as i32;
    ctx.sidebar_resizing = false;
    ctx.push_toast(crate::toast::Kind::Info, format!("Sidebar resized to {width}px"));
    trace(&format!("sidebar_resize finish width={width}"));
    width
}

/// Draw the visible divider/handle on the sidebar's right edge.
#[no_mangle]
pub extern "C" fn mui_sidebar_resize_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || layout::zen_active() {
        return;
    }
    let (_visible_w, visible_h) = visible_surface_size(ctx);
    let x = layout::sidebar_right() - 1.0;
    let top = layout::TAB_BAR_H;
    let bottom = visible_h as f32 - 2.0 * layout::LINE_H();
    let h = (bottom - top).max(0.0);
    if h <= 80.0 {
        return;
    }
    let was_overlay = ctx.overlay;
    let was_clip = ctx.clip;
    ctx.overlay = true;
    ctx.clip = None;
    let edge_w = if ctx.sidebar_resizing { 3.0 } else { 2.0 };
    let edge_color = if ctx.sidebar_resizing {
        theme::ACCENT()
    } else {
        theme::BORDER_SOFT()
    };
    ctx.dl_rect(x, top, edge_w, h, edge_color);
    let grip_h = sidebar_resize_grip_height(h);
    let grip_y = top + (h - grip_h) * 0.5;
    let grip_x = x - 5.0;
    let grip_bg = if ctx.sidebar_resizing {
        theme::accent_a(0.24)
    } else {
        theme::BG_4()
    };
    ctx.dl_shadow(grip_x, grip_y, 10.0, grip_h, 5.0, theme::SHADOW(), 10.0);
    ctx.dl_round(grip_x, grip_y, 10.0, grip_h, 5.0, grip_bg);
    ctx.dl_stroke(grip_x, grip_y, 10.0, grip_h, 5.0, theme::BORDER_STRONG(), 1.0);
    let grip_color = if ctx.sidebar_resizing { theme::ACCENT_BRIGHT() } else { theme::TEXT_3() };
    let dot_x = grip_x + 3.5;
    let dot_gap = (grip_h / 4.0).clamp(8.0, 13.0);
    let dot_mid = grip_y + grip_h * 0.5;
    for dy in [-dot_gap, 0.0, dot_gap] {
        ctx.dl_round(dot_x, dot_mid + dy - 1.5, 3.0, 3.0, 1.5, grip_color);
    }
    ctx.clip = was_clip;
    ctx.overlay = was_overlay;
}

pub(crate) fn sidebar_resize_grip_height(available_h: f32) -> f32 {
    42.0_f32.min((available_h - 48.0).max(18.0))
}

/// Re-scan the tree from its root (honoring the current expand state).
#[no_mangle]
pub extern "C" fn mui_tree_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.tree.refresh();
    ctx.tree.count() as i32
}

/// Number of visible tree rows.
#[no_mangle]
pub extern "C" fn mui_tree_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tree.count() as i32)
}

/// `1` if tree row `i` is a directory, `0` if a file, `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_tree_is_dir(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tree
            .get(i as usize)
            .map_or(-1, |r| if r.is_dir { 1 } else { 0 })
    })
}

/// Indentation depth of tree row `i` (0 = top level), or -1 out of range.
#[no_mangle]
pub extern "C" fn mui_tree_depth(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tree.get(i as usize).map_or(-1, |r| r.depth as i32)
    })
}

/// `1` if tree row `i` is an expanded directory, else `0` (-1 out of range).
#[no_mangle]
pub extern "C" fn mui_tree_is_expanded(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tree
            .get(i as usize)
            .map_or(-1, |r| if r.expanded { 1 } else { 0 })
    })
}

/// Toggle expand/collapse of the directory at tree row `i`. Returns the new
/// tree row count (rows shift when a dir expands/collapses).
#[no_mangle]
pub extern "C" fn mui_tree_toggle(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i >= 0 {
        ctx.tree.toggle(i as usize);
    }
    ctx.tree.count() as i32
}

/// Map the last click's pixel y to a tree row index, or -1 if past the last
/// row / not in the sidebar.
#[no_mangle]
pub extern "C" fn mui_tree_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // Only count clicks within the sidebar's x band (right of the rail).
    let sx0 = layout::RAIL_W;
    let sx1 = layout::sidebar_right();
    if !ctx.sidebar_visible || ctx.last_event.x < sx0 || ctx.last_event.x > sx1 {
        return -1;
    }
    let i = layout::tree_row_at(ctx.last_event.y) as usize;
    if i < ctx.tree.count() {
        i as i32
    } else {
        -1
    }
}

/// Open the file at tree row `i` as a tab, or toggle a directory row. Returns
/// the resulting tab index, or -1 when no tab was opened.
#[no_mangle]
pub extern "C" fn mui_tree_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No Explorer row selected");
        return -1;
    }
    let Some(row) = ctx.tree.get(i as usize) else {
        ctx.push_toast(crate::toast::Kind::Info, "Explorer row no longer listed");
        return -1;
    };
    if row.is_dir {
        ctx.tree.toggle(i as usize);
        return -1;
    }
    let path = row.path.clone();
    if !path.is_file() {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("source");
        refresh_workspace_file_views(ctx);
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Explorer target missing: {name}"),
        );
        return -1;
    }
    let idx = ctx.tabs.open_path(path.clone());
    sync_active_path(ctx);
    record_opened_file(ctx, &path);
    idx as i32
}

/// Draw the file-tree sidebar (background band + one row per visible entry,
/// indented by depth, dirs marked). No-op when the sidebar is hidden. Mighty
/// calls this once per frame after the tab bar.
#[no_mangle]
pub extern "C" fn mui_sidebar_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible {
        return;
    }
    let h = ctx.gpu.height as f32;
    let clip = ctx.clip;
    let chrome = theme::CHROME_FONT_SIZE;
    let sx = layout::RAIL_W; // sidebar starts right of the rail
    let sw = layout::sidebar_w();
    use crate::icons;

    // Panel background (panel color) + a right divider.
    ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
    ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

    // Section-header band (mockup `.sb-head`, 40px) with a bottom hairline.
    let head_h = 40.0;
    ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
    ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
    // The explorer header shows the EXPLICIT workspace name (Open Folder), else
    // the tree-root basename, else "EXPLORER".
    let header = if !ctx.workspace.is_empty() {
        ctx.workspace.name().to_uppercase()
    } else {
        ctx.tree
            .root()
            .file_name()
            .map(|s| s.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "EXPLORER".to_string())
    };
    // Letter-spaced uppercase header (insert thin spaces), measured so it never
    // runs under the right-aligned header action buttons in compact sidebars.
    let tracked = fit_explorer_header(&mut ctx.text, &header, sx, sw, chrome - 2.0);
    ctx.text.queue_ui_styled(
        sx + 14.0,
        (head_h - (chrome - 2.0)) * 0.5 - 1.0,
        &tracked,
        theme::DIM(),
        chrome - 2.0,
        crate::vello_ui::FontStyle::Bold,
        clip,
    );
    // Header actions (new file / new folder / collapse) right-aligned as real
    // icon buttons with enough air that they read as separate mouse targets.
    // Dialog-backed actions get a tiny "..." mark so they read differently from
    // immediate tree actions in the compact toolbar.
    let act_y = (head_h - 15.0) * 0.5;
    for (x, action) in explorer_header_action_centers(sx, sw) {
        let icon = match action {
            1 => icons::NEW_FILE,
            2 => icons::NEW_FOLDER,
            _ => icons::COLLAPSE,
        };
        ctx.dl_round(x - 2.5, 8.0, 24.0, 24.0, 5.0, theme::BG_4());
        ctx.dl_stroke(x - 2.5, 8.0, 24.0, 24.0, 5.0, theme::BORDER_SOFT(), 1.0);
        ctx.dl_icon(x + 2.0, act_y, 15.0, 15.0, icon, theme::TEXT_3(), 1.5, false);
        if explorer_header_action_opens_dialog(action) {
            for dx in [11.0, 14.0, 17.0] {
                ctx.dl_round(x - 2.5 + dx, 26.5, 1.5, 1.5, 0.75, theme::TEXT_4());
            }
        }
    }

    // File rows. Mockup row height is 28px; we keep LINE_H rhythm but draw a
    // 28px-tall hover/selection capsule centered on the row baseline.
    let row_h = layout::LINE_H();
    let row_top = head_h + 6.0;
    let active_path = ctx.tabs.active_path();
    let active_path = active_path.as_deref();
    let count = ctx.tree.count();
    for i in 0..count {
        let (is_dir, expanded, depth, name, selected) = {
            let Some(row) = ctx.tree.get(i) else { continue };
            let selected = explorer_row_selected(row.is_dir, &row.path, active_path);
            (row.is_dir, row.expanded, row.depth, row.display_name(), selected)
        };
        let y = row_top + (i as f32) * row_h;
        if y > h {
            break;
        }
        // Selected row: indigo-faint left→right tint capsule + indigo left bar.
        if selected {
            ctx.dl_grad_h(sx + 8.0, y, sw - 16.0, row_h, 5.0, theme::ACCENT_FAINT(), 0.9);
            ctx.dl_round(sx, y + 3.0, 2.0, row_h - 6.0, 1.0, theme::ACCENT());
            ctx.dl_shadow(sx, y + 3.0, 2.0, row_h - 6.0, 1.0, theme::ACCENT_GLOW(), 6.0);
        }
        let base_indent = sx + 12.0;
        let indent = base_indent + (depth as f32) * layout::TREE_INDENT;
        let icon_y = y + (row_h - 15.0) * 0.5;
        let txt_y = y + (row_h - chrome) * 0.5 - 1.0;
        let mut content_x = indent;
        // Dir disclosure chevron (rotated when open via a different glyph is not
        // available; draw chevron-right always, and a folder icon next to it).
        if is_dir {
            // Chevron: pointing down when expanded, right when collapsed.
            if expanded {
                // rotate 90°: draw a downward chevron via a path variant.
                ctx.dl_icon(content_x, icon_y, 12.0, 12.0, "M6 9l6 6 6-6", theme::TEXT_3(), 2.0, false);
            } else {
                ctx.dl_icon(content_x, icon_y, 12.0, 12.0, icons::CHEVRON, theme::TEXT_3(), 2.0, false);
            }
            content_x += 14.0;
            ctx.dl_icon(content_x, icon_y, 15.0, 15.0, icons::FOLDER, theme::DIM(), 1.4, false);
            content_x += 17.0;
        } else {
            // File: skip the chevron column to align under folder contents.
            content_x += 14.0;
            let (icon, icol) = file_icon_for(&name, selected);
            ctx.dl_icon(content_x, icon_y, 15.0, 15.0, icon, icol, 1.4, false);
            content_x += 17.0;
        }
        let name_x = content_x;
        let git = git_status_for(&name);
        let shown = fit_explorer_name(&mut ctx.text, &name, name_x, sx, sw, chrome, git.is_some());
        let fg = if selected { theme::TEXT() } else { theme::TEXT_1() };
        if !shown.is_empty() {
            ctx.text.queue_ui_sized(name_x, txt_y, &shown, fg, chrome, clip);
        }
        // Git status letter, right-aligned (mockup `.row .git`): M/A/U.
        if let Some((gl, gc)) = git {
            ctx.text.queue_ui_sized(sx + sw - 22.0, txt_y, gl, gc, chrome - 2.0, clip);
        }
    }
}

pub(crate) fn explorer_row_selected(
    is_dir: bool,
    row_path: &std::path::Path,
    active_path: Option<&std::path::Path>,
) -> bool {
    !is_dir && active_path == Some(row_path)
}

pub(crate) fn fit_explorer_name(
    text: &mut crate::text::Text,
    name: &str,
    name_x: f32,
    sx: f32,
    sw: f32,
    chrome: f32,
    has_git_badge: bool,
) -> String {
    let right = if has_git_badge { sx + sw - 30.0 } else { sx + sw - 14.0 };
    fit_status_tail(text, name, (right - name_x).max(0.0), chrome)
}

pub(crate) fn fit_explorer_header(
    text: &mut crate::text::Text,
    header: &str,
    sx: f32,
    sw: f32,
    chrome: f32,
) -> String {
    let label_x = sx + 14.0;
    let first_action_x = explorer_header_action_centers(sx, sw)[0].0 - 2.5;
    let max_px = (first_action_x - 8.0 - label_x).max(0.0);
    let tracked: String = header.chars().flat_map(|c| [c, '\u{2009}']).collect();
    fit_status_head(text, &tracked, max_px, chrome)
}

/// A small synthetic git-status badge for a few demo filenames so the tree
/// reads like the mockup (M warn / A green / U info). Returns `None` for clean.
fn git_status_for(name: &str) -> Option<(&'static str, MuiColor)> {
    match name {
        "main.mty" | "Mighty.toml" => Some(("M", theme::WARNING())),
        "agents.mty" => Some(("A", theme::GREEN())),
        "README.md" => Some(("U", theme::INFO())),
        _ => None,
    }
}

/// Print the live workspace counts to stdout (tab count, active tab, tree
/// entries). Used as launch-test evidence for the Mighty side, which can't
/// `log` computed integers (L1). No-op on a null handle.
#[no_mangle]
pub extern "C" fn mui_log_workspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        println!(
            "workspace: tab_count={} active={} tree_entries={} sidebar={}",
            ctx.tabs.count(),
            ctx.tabs.active(),
            ctx.tree.count(),
            if ctx.sidebar_visible { "on" } else { "off" }
        );
    }
}

/// Buffer-accumulation probe (L28 / arena-runtime verdict). The Mighty side
/// passes the length of its live `buf: Vec[I32]` (`mty_buf_len`) after the
/// load loop; the shim prints it next to its own byte count for the active tab
/// so a launch test can confirm whether the Mighty Vec actually accumulated.
/// Mighty native `log` can't print computed integers (L1/L23), so this FFI
/// printer is the only way to surface `buf.len()`.
#[no_mangle]
pub extern "C" fn mui_probe_buf_len(handle: i64, mty_buf_len: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let shim_bytes = ctx.load_buf.len();
        println!(
            "probe: mty_buf_len={} shim_load_bytes={} match={}",
            mty_buf_len,
            shim_bytes,
            mty_buf_len as usize == shim_bytes
        );
    } else {
        println!("probe: mty_buf_len={mty_buf_len} (no ctx)");
    }
}

// ---------------------------------------------------------------------------
// Integrated terminal — PTY-backed shell + VT grid (all logic in terminal.rs)
// ---------------------------------------------------------------------------

/// One queued terminal text run: position, string, resolved RGBA color, italic state, and faint state.
type TermRun = (f32, f32, String, (f32, f32, f32, f32), bool, bool);
/// One queued terminal background run: position, width, and resolved RGBA color.
type TermBgRun = (f32, f32, f32, (f32, f32, f32, f32));
/// One queued terminal underline run: position, width, and resolved RGBA color.
type TermUnderlineRun = (f32, f32, f32, (f32, f32, f32, f32));
/// One queued terminal strikethrough run: position, width, and resolved RGBA color.
type TermStrikethroughRun = (f32, f32, f32, (f32, f32, f32, f32));
/// One queued terminal overline run: position, width, and resolved RGBA color.
type TermOverlineRun = (f32, f32, f32, (f32, f32, f32, f32));

fn terminal_cursor_draw_visible(cursor_visible: bool, cursor_blinking: bool, frame: u64) -> bool {
    cursor_visible && (!cursor_blinking || (frame / 30) % 2 == 0)
}

fn terminal_sgr_blink_visible(frame: u64) -> bool {
    (frame / 30) % 2 == 0
}

fn terminal_header_title(title: &str, current_dir: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return format!("TERMINAL - {title}");
    }
    let current_dir = current_dir.trim();
    if !current_dir.is_empty() {
        return format!("TERMINAL - {current_dir}");
    }
    "TERMINAL".to_string()
}

#[cfg(test)]
mod terminal_cursor_tests {
    use super::{terminal_cursor_draw_visible, terminal_header_title, terminal_sgr_blink_visible};

    #[test]
    fn terminal_cursor_draw_visibility_honors_blink_phase() {
        assert!(!terminal_cursor_draw_visible(false, false, 0));
        assert!(!terminal_cursor_draw_visible(false, true, 0));
        assert!(terminal_cursor_draw_visible(true, false, 30));
        assert!(terminal_cursor_draw_visible(true, true, 0));
        assert!(terminal_cursor_draw_visible(true, true, 29));
        assert!(!terminal_cursor_draw_visible(true, true, 30));
        assert!(!terminal_cursor_draw_visible(true, true, 59));
        assert!(terminal_cursor_draw_visible(true, true, 60));
    }

    #[test]
    fn terminal_sgr_blink_visibility_honors_blink_phase() {
        assert!(terminal_sgr_blink_visible(0));
        assert!(terminal_sgr_blink_visible(29));
        assert!(!terminal_sgr_blink_visible(30));
        assert!(!terminal_sgr_blink_visible(59));
        assert!(terminal_sgr_blink_visible(60));
    }

    #[test]
    fn terminal_header_title_falls_back_to_current_dir() {
        assert_eq!(terminal_header_title("", ""), "TERMINAL");
        assert_eq!(
            terminal_header_title("shell", "C:/repo"),
            "TERMINAL - shell"
        );
        assert_eq!(
            terminal_header_title("", "C:/repo"),
            "TERMINAL - C:/repo"
        );
    }
}

/// Grid dimensions for the terminal panel given the current window + sidebar.
fn term_dims(ctx: &MuiContext) -> (usize, usize) {
    let region = layout::region(ctx.sidebar_visible);
    let (width, height) = visible_surface_size(ctx);
    let rows = layout::term_grid_rows(height);
    let cols = layout::term_grid_cols(width, region);
    (rows, cols)
}

fn term_event_cell(ctx: &MuiContext) -> (usize, usize) {
    let region = layout::region(ctx.sidebar_visible);
    let (_, height) = visible_surface_size(ctx);
    let (rows, cols) = term_dims(ctx);
    let grid_x = ctx.last_event.x - (layout::term_panel_left(region) + layout::PAD);
    let grid_y = ctx.last_event.y - (layout::term_panel_top(height) + layout::term_header_h());
    let col = ((grid_x / layout::CHAR_W()).floor() as isize)
        .clamp(0, cols.saturating_sub(1) as isize) as usize
        + 1;
    let row = ((grid_y / layout::LINE_H()).floor() as isize)
        .clamp(0, rows.saturating_sub(1) as isize) as usize
        + 1;
    (row, col)
}

fn term_grid_contains_point(ctx: &MuiContext, x: f32, y: f32) -> bool {
    if !ctx.term_open || ctx.terminal.is_none() {
        return false;
    }
    let region = layout::region(ctx.sidebar_visible);
    let (width, height) = visible_surface_size(ctx);
    let left = layout::term_panel_left(region) + layout::PAD;
    let top = layout::term_panel_top(height) + layout::term_header_h();
    let right = (left + layout::term_grid_cols(width, region) as f32 * layout::CHAR_W())
        .min(width as f32);
    let bottom = (top + layout::term_grid_rows(height) as f32 * layout::LINE_H())
        .min(height as f32);
    x >= left && x < right && y >= top && y < bottom
}

fn term_grid_contains_event(ctx: &MuiContext) -> bool {
    term_grid_contains_point(ctx, ctx.last_event.x, ctx.last_event.y)
}

pub(crate) const TERM_HEADER_CLICK_NONE: i32 = 0;
pub(crate) const TERM_HEADER_CLICK_CLEAR: i32 = 1;

pub(crate) fn terminal_header_clear_rect(ctx: &MuiContext) -> (f32, f32, f32, f32) {
    let region = layout::region(ctx.sidebar_visible);
    let (width, height) = visible_surface_size(ctx);
    let size = 22.0;
    let x = layout::dock_header_content_right(width, height) - size;
    let y = layout::term_panel_top(height) + (layout::term_header_h() - size) * 0.5;
    (x.max(layout::term_panel_left(region) + 86.0), y, size, size)
}

fn terminal_open_failed_message(shell: &str, reason: Option<&str>) -> String {
    let shell = shell.trim();
    let reason = reason.map(str::trim).filter(|s| !s.is_empty());
    match (shell.is_empty(), reason) {
        (true, None) => "Terminal failed to open".to_string(),
        (true, Some(reason)) => format!("Terminal failed to open: {reason}"),
        (false, None) => format!("Terminal failed to open: {shell}"),
        (false, Some(reason)) => format!("Terminal failed to open: {shell}: {reason}"),
    }
}

#[cfg(test)]
fn terminal_open_forced_failure_reason() -> String {
    std::env::var("MUI_TERM_FORCE_OPEN_FAIL_REASON")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "spawn failed".to_string())
}

fn term_wants_mouse_motion_at(ctx: &MuiContext, x: f32, y: f32) -> bool {
    term_grid_contains_point(ctx, x, y)
        && ctx
            .terminal
            .as_ref()
            .is_some_and(|t| t.mouse_motion_reporting_enabled())
}

fn term_wants_mouse_reporting_at(ctx: &MuiContext, x: f32, y: f32) -> bool {
    term_grid_contains_point(ctx, x, y)
        && ctx
            .terminal
            .as_ref()
            .is_some_and(|t| t.mouse_reporting_enabled())
}

/// Open (spawn if needed) the integrated terminal, sizing its grid/PTY to the
/// current panel. Marks the panel open. Returns `1` if a terminal is running
/// afterwards, `0` on spawn failure or null handle.
#[no_mangle]
pub extern "C" fn mui_term_open(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (rows, cols) = term_dims(ctx);
    let was_open = ctx.term_open;
    let mut spawned = false;
    #[cfg(test)]
    if let Some(shell) = std::env::var_os("MUI_TERM_FORCE_OPEN_FAIL") {
        let shell = shell
            .into_string()
            .unwrap_or_else(|_| crate::terminal::default_shell_display_name());
        let reason = terminal_open_forced_failure_reason();
        ctx.push_toast(
            crate::toast::Kind::Error,
            terminal_open_failed_message(&shell, Some(&reason)),
        );
        trace("term_open forced failure");
        return 0;
    }
    if ctx.terminal.is_none() {
        let shell = crate::terminal::default_shell_display_name();
        match crate::terminal::Terminal::spawn(rows, cols) {
            Ok(t) => {
                println!("mui_term_open: spawned shell, grid {rows}x{cols}");
                ctx.terminal = Some(t);
                spawned = true;
            }
            Err(e) => {
                eprintln!("mui_term_open: {e}");
                let reason = e.to_string();
                ctx.push_toast(
                    crate::toast::Kind::Error,
                    terminal_open_failed_message(&shell, Some(&reason)),
                );
                trace(&format!("term_open failed: {e}"));
                return 0;
            }
        }
    } else if let Some(t) = ctx.terminal.as_mut() {
        // Re-size to the current panel in case the window changed while closed.
        t.resize(rows, cols);
    }
    ctx.run.close();
    ctx.web.close();
    ctx.problems.set_open(false);
    ctx.term_open = true;
    if spawned || !was_open {
        ctx.push_toast(crate::toast::Kind::Info, "Terminal opened");
    }
    trace(&format!(
        "term_open rows={rows} cols={cols} spawned={spawned} was_open={was_open}"
    ));
    1
}

/// Close the terminal panel and tear down the shell (frees the PTY + grid).
/// Marks the panel closed. Returns `1` when a terminal was closed, or `0`
/// when it was already closed.
#[no_mangle]
pub extern "C" fn mui_term_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.term_open && ctx.terminal.is_none() {
        ctx.push_toast(crate::toast::Kind::Info, "Terminal is already closed");
        trace("term_close noop");
        return 0;
    }
    ctx.term_open = false;
    // Dropping the Terminal kills the child + joins nothing (reader thread
    // exits on EOF). Keep this explicit for clarity.
    ctx.terminal = None;
    ctx.push_toast(crate::toast::Kind::Info, "Terminal closed");
    trace("term_close");
    1
}

/// Clear the terminal's visible buffer without killing the shell.
/// Returns `1` when a terminal panel remains open afterwards, else `0`.
#[no_mangle]
pub extern "C" fn mui_term_clear(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.term_open || ctx.terminal.is_none() {
        ctx.push_toast(crate::toast::Kind::Info, "Terminal is already closed");
        trace("term_clear noop");
        return 0;
    }
    ctx.run.close();
    ctx.web.close();
    ctx.problems.set_open(false);
    if let Some(t) = ctx.terminal.as_mut() {
        let had_content = t.clear_buffer();
        let msg = if had_content {
            "Terminal cleared"
        } else {
            "Terminal is already empty"
        };
        ctx.push_toast(crate::toast::Kind::Info, msg);
        trace("term_clear");
        return 1;
    }
    0
}

/// `1` if the terminal panel is currently open AND a shell is running, else `0`.
#[no_mangle]
pub extern "C" fn mui_term_running(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.term_open {
        return 0;
    }
    match ctx.terminal.as_mut() {
        Some(t) => i32::from(t.is_alive()),
        None => 0,
    }
}

/// `1` if the terminal panel is open (regardless of shell liveness), else `0`.
/// The Mighty side uses this for focus routing.
#[no_mangle]
pub extern "C" fn mui_term_is_open(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.term_open { 1 } else { 0 })
}

/// Header action at the latest mouse event. Returns `1` for Clear, else `0`.
#[no_mangle]
pub extern "C" fn mui_term_header_action_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return TERM_HEADER_CLICK_NONE;
    };
    if !ctx.term_open || ctx.terminal.is_none() {
        return TERM_HEADER_CLICK_NONE;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let (bx, by, bw, bh) = terminal_header_clear_rect(ctx);
    if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
        return TERM_HEADER_CLICK_CLEAR;
    }
    TERM_HEADER_CLICK_NONE
}

/// Map a named key (`MUI_KEY_*`) + mods to terminal stdin bytes and write them
/// to the PTY. No-op if the terminal is not running.
#[no_mangle]
pub extern "C" fn mui_term_key(handle: i64, keycode: i32, mods: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if keycode >= 0 {
                t.send_key(keycode as u32, mods.max(0) as u32);
            }
        }
    }
}

/// Map a typed codepoint + mods to terminal stdin bytes (Ctrl+letter -> control
/// code, else UTF-8) and write them to the PTY. No-op if not running.
#[no_mangle]
pub extern "C" fn mui_term_send_codepoint(handle: i64, codepoint: i32, mods: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if codepoint >= 0 {
                if let Some(bytes) =
                    crate::terminal::codepoint_to_bytes(codepoint as u32, mods.max(0) as u32)
                {
                    t.send(&bytes);
                }
            }
        }
    }
}

/// Paste operating-system clipboard text into the PTY. Honors terminal
/// bracketed-paste mode when the shell/application enabled it.
#[no_mangle]
pub extern "C" fn mui_term_paste(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.term_open || ctx.terminal.is_none() {
        ctx.push_toast(crate::toast::Kind::Warn, "Terminal is not open");
        return 0;
    }
    let text = match read_clipboard_text() {
        Ok(text) => text,
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                terminal_paste_failure_message(&e),
            );
            println!("terminal-paste: failed to read clipboard: {e}");
            return 0;
        }
    };
    if text.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Clipboard is empty");
        return 0;
    }
    let Some(t) = ctx.terminal.as_mut() else {
        ctx.push_toast(crate::toast::Kind::Warn, "Terminal is not open");
        return 0;
    };
    t.send_paste(&text);
    ctx.push_toast(crate::toast::Kind::Success, "Pasted to terminal");
    1
}

fn terminal_paste_failure_message(e: &std::io::Error) -> String {
    let msg = e.to_string();
    if msg.trim().is_empty() {
        "Terminal paste failed".to_string()
    } else {
        format!("Terminal paste failed: {msg}")
    }
}

/// Send a mouse-wheel scroll gesture to the PTY. When a terminal app enabled
/// mouse reporting, use the last scroll event's terminal cell coordinate;
/// otherwise this falls back to repeated cursor movement for ordinary shells.
#[no_mangle]
pub extern "C" fn mui_term_scroll(handle: i64, dir: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let (row, col) = term_event_cell(ctx);
        let mods = ctx.last_event.mods;
        if let Some(t) = ctx.terminal.as_mut() {
            t.send_scroll_at(dir, row, col, mods);
        }
    }
}

/// `1` when the latest mouse event is inside the terminal grid body.
#[no_mangle]
pub extern "C" fn mui_term_hit_at_event(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(term_grid_contains_event(c)))
}

/// Send the latest mouse button event to the terminal when reporting is enabled.
/// `pressed != 0` sends a button press; `0` sends release. Returns `1` when the
/// event was in the terminal grid and was routed to terminal focus, even if the
/// running app has not enabled mouse reporting.
#[no_mangle]
pub extern "C" fn mui_term_mouse_button(handle: i64, pressed: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !term_grid_contains_event(ctx) {
        if pressed == 0 {
            if let Some(t) = ctx.terminal.as_mut() {
                t.clear_mouse_button_state();
            }
        }
        return 0;
    }
    let (row, col) = term_event_cell(ctx);
    let button = ctx.last_event.button;
    let mods = ctx.last_event.mods;
    if let Some(t) = ctx.terminal.as_mut() {
        t.send_mouse_button_at(pressed != 0, button, row, col, mods);
        return 1;
    }
    0
}

/// Send the latest mouse move to the terminal when drag/any-motion reporting
/// is enabled. Returns `1` when the event is inside the terminal grid.
#[no_mangle]
pub extern "C" fn mui_term_mouse_move(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !term_grid_contains_event(ctx) {
        return 0;
    }
    let (row, col) = term_event_cell(ctx);
    let mods = ctx.last_event.mods;
    if let Some(t) = ctx.terminal.as_mut() {
        t.send_mouse_motion_at(row, col, mods);
        return 1;
    }
    0
}

/// Publish IDE keyboard focus to the terminal so apps that enabled xterm focus
/// reporting (`CSI ?1004 h`) receive focus-in/focus-out events.
#[no_mangle]
pub extern "C" fn mui_term_focus(handle: i64, focused: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            t.set_focus(focused != 0);
        }
    }
}

/// Write a single raw byte to the PTY stdin. No-op if not running.
#[no_mangle]
pub extern "C" fn mui_term_send_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if (0..=255).contains(&byte) {
                t.send(&[byte as u8]);
            }
        }
    }
}

/// Drain pending PTY output through the VT parser into the grid. Call once per
/// frame while the panel is open. No-op if not running.
#[no_mangle]
pub extern "C" fn mui_term_pump(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let clipboard_text = if let Some(t) = ctx.terminal.as_mut() {
            t.pump();
            let clipboard_text = t.take_clipboard_write();
            if let Ok(probe) = std::env::var("MUI_TERM_PROBE_TEXT") {
                if !probe.is_empty() && t.visible_contains(&probe) {
                    crate::abi::trace(&format!("terminal_probe text={probe}"));
                }
            }
            clipboard_text
        } else {
            None
        };

        if let Some(text) = clipboard_text {
            match write_clipboard_text(&text) {
                Ok(()) => ctx.push_toast(crate::toast::Kind::Success, "Copied from terminal"),
                Err(e) => {
                    ctx.push_toast(
                        crate::toast::Kind::Error,
                        terminal_copy_failure_message(&e),
                    );
                    println!("terminal-osc52: failed to write clipboard: {e}");
                }
            }
        }
    }
}

fn terminal_copy_failure_message(e: &std::io::Error) -> String {
    let msg = e.to_string();
    if msg.trim().is_empty() {
        "Could not copy terminal text".to_string()
    } else {
        format!("Could not copy terminal text: {msg}")
    }
}

/// Number of rows in the terminal grid (0 if not running).
#[no_mangle]
pub extern "C" fn mui_term_rows(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.terminal.as_ref().map_or(0, |t| t.rows() as i32))
}

/// Number of columns in the terminal grid (0 if not running).
#[no_mangle]
pub extern "C" fn mui_term_cols(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.terminal.as_ref().map_or(0, |t| t.cols() as i32))
}

/// Draw the terminal panel: a background band, then the grid cells (each glyph
/// in its palette color), then a block cursor. Resizes the grid/PTY to the
/// current panel first so it tracks window resizes. No-op if the panel is closed
/// or no shell is running. Mighty calls this once per frame after `mui_term_pump`.
#[no_mangle]
pub extern "C" fn mui_term_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.term_open || ctx.terminal.is_none() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let (panel_rows, panel_cols) = term_dims(ctx);
    let (width, height) = visible_surface_size(ctx);
    let handle_ptr = handle as usize as *mut MuiContext;
    let clip = ctx.clip;

    // Resize the grid + PTY to the current panel before drawing.
    if let Some(t) = ctx.terminal.as_mut() {
        t.resize(panel_rows, panel_cols);
    }

    // Panel geometry.
    let panel_top = layout::term_panel_top(height);
    let panel_h = layout::term_panel_height(height);
    let panel_left = layout::term_panel_left(region);
    let panel_w = (width as f32 - panel_left).max(0.0);

    // Rounded-top panel (a rounded rect whose bottom corners are off-screen) +
    // an ember top accent line + a dim terminal header (UI family).
    ctx.dl_round(panel_left, panel_top, panel_w, panel_h + 12.0, 10.0, theme::ELEVATED());
    ctx.dl_rect(panel_left, panel_top, panel_w, 1.0, theme::BORDER());
    let title_text = ctx
        .terminal
        .as_ref()
        .map_or_else(|| "TERMINAL".to_string(), |t| {
            terminal_header_title(t.title(), t.current_dir())
        });
    let title_x = panel_left + layout::PAD + 4.0;
    let (clear_x, clear_y, clear_w, clear_h) = terminal_header_clear_rect(ctx);
    let title_max = (clear_x - title_x - 8.0).max(0.0);
    let title_text = fit_status_head(&mut ctx.text, &title_text, title_max, theme::CHROME_FONT_SIZE - 1.0);
    ctx.text.queue_ui_sized(
        title_x,
        panel_top + 4.0,
        &title_text,
        theme::DIM(),
        theme::CHROME_FONT_SIZE - 1.0,
        clip,
    );
    ctx.dl_round(clear_x, clear_y, clear_w, clear_h, 5.0, theme::BG_4());
    ctx.dl_stroke(clear_x, clear_y, clear_w, clear_h, 5.0, theme::BORDER_SOFT(), 1.0);
    ctx.dl_icon(
        clear_x + 5.0,
        clear_y + 4.5,
        12.0,
        12.0,
        crate::icons::TRASH,
        theme::TEXT_3(),
        1.4,
        false,
    );
    let _ = handle_ptr;
    let frame = FRAME_COUNTER.load(std::sync::atomic::Ordering::Relaxed);
    let blink_visible = terminal_sgr_blink_visible(frame);

    // Snapshot the grid into owned data so the borrow on `ctx.terminal` ends
    // before we borrow `ctx.text`.
    let (
        rows,
        cols,
        cursor,
        cursor_visible,
        cursor_blinking,
        cursor_shape,
        cursor_color,
        backgrounds,
        underlines,
        strikethroughs,
        overlines,
        glyphs,
    ) = {
        let Some(t) = ctx.terminal.as_ref() else {
            return;
        };
        let g = t.grid();
        let rows = g.rows();
        let cols = g.cols();
        let mut bg_runs: Vec<TermBgRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r);
            let mut col = 0usize;
            while col < cols {
                let bg = g.cell(r, col).bg;
                let start = col;
                while col < cols && g.cell(r, col).bg == bg {
                    col += 1;
                }
                if let Some(color) = t.background_rgba(bg) {
                    let x = layout::term_cell_x(region, start);
                    let w = (col - start) as f32 * layout::CHAR_W();
                    bg_runs.push((x, y, w, color));
                }
            }
        }

        let mut underline_runs: Vec<TermUnderlineRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r) + layout::LINE_H() - 4.0;
            let mut col = 0usize;
            while col < cols {
                let cell = g.cell(r, col);
                if (!cell.underline && cell.hyperlink.is_none())
                    || cell.conceal
                    || (cell.blink && !blink_visible)
                {
                    col += 1;
                    continue;
                }
                let fg = cell.fg;
                let start = col;
                while col < cols {
                    let cell = g.cell(r, col);
                    if (!cell.underline && cell.hyperlink.is_none())
                        || cell.conceal
                        || (cell.blink && !blink_visible)
                        || cell.fg != fg
                    {
                        break;
                    }
                    col += 1;
                }
                let x = layout::term_cell_x(region, start);
                let w = (col - start) as f32 * layout::CHAR_W();
                underline_runs.push((x, y, w, t.foreground_rgba(fg)));
            }
        }

        let mut strikethrough_runs: Vec<TermStrikethroughRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r) + layout::LINE_H() * 0.55;
            let mut col = 0usize;
            while col < cols {
                let cell = g.cell(r, col);
                if !cell.strikethrough || cell.conceal || (cell.blink && !blink_visible) {
                    col += 1;
                    continue;
                }
                let fg = cell.fg;
                let start = col;
                while col < cols {
                    let cell = g.cell(r, col);
                    if !cell.strikethrough
                        || cell.conceal
                        || (cell.blink && !blink_visible)
                        || cell.fg != fg
                    {
                        break;
                    }
                    col += 1;
                }
                let x = layout::term_cell_x(region, start);
                let w = (col - start) as f32 * layout::CHAR_W();
                strikethrough_runs.push((x, y, w, t.foreground_rgba(fg)));
            }
        }

        let mut overline_runs: Vec<TermOverlineRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r) + 2.0;
            let mut col = 0usize;
            while col < cols {
                let cell = g.cell(r, col);
                if !cell.overline || cell.conceal || (cell.blink && !blink_visible) {
                    col += 1;
                    continue;
                }
                let fg = cell.fg;
                let start = col;
                while col < cols {
                    let cell = g.cell(r, col);
                    if !cell.overline
                        || cell.conceal
                        || (cell.blink && !blink_visible)
                        || cell.fg != fg
                    {
                        break;
                    }
                    col += 1;
                }
                let x = layout::term_cell_x(region, start);
                let w = (col - start) as f32 * layout::CHAR_W();
                overline_runs.push((x, y, w, t.foreground_rgba(fg)));
            }
        }

        // Build one (x, y, string, color) run per row, splitting on color change
        // to keep the draw-call count modest while preserving per-cell color.
        let mut runs: Vec<TermRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r);
            let mut col = 0usize;
            while col < cols {
                let cell = g.cell(r, col);
                let fg = cell.fg;
                let italic = cell.italic;
                let faint = cell.faint;
                let conceal = cell.conceal;
                let blink_hidden = cell.blink && !blink_visible;
                let start = col;
                let mut s = String::new();
                while col < cols {
                    let cell = g.cell(r, col);
                    if cell.fg != fg
                        || cell.italic != italic
                        || cell.faint != faint
                        || cell.conceal != conceal
                        || (cell.blink && !blink_visible) != blink_hidden
                    {
                        break;
                    }
                    s.push(if conceal || blink_hidden { ' ' } else { cell.ch });
                    col += 1;
                }
                // Trim a trailing run of spaces (don't draw blank tails).
                if !s.trim_end().is_empty() {
                    let x = layout::term_cell_x(region, start);
                    runs.push((x, y, s, t.foreground_rgba(fg), italic, faint));
                }
            }
        }
        (
            rows,
            cols,
            g.cursor(),
            t.cursor_visible(),
            t.cursor_blinking(),
            t.cursor_shape(),
            t.cursor_rgba(),
            bg_runs,
            underline_runs,
            strikethrough_runs,
            overline_runs,
            runs,
        )
    };

    for (x, y, w, (r, gc, b, a)) in &backgrounds {
        ctx.dl_rect(*x, *y, *w, layout::LINE_H() - 2.0, MuiColor::new(*r, *gc, *b, *a));
    }

    for (x, y, s, (r, gc, b, a), italic, faint) in &glyphs {
        let alpha = if *faint { *a * 0.62 } else { *a };
        let color = MuiColor::new(*r, *gc, *b, alpha);
        if *italic {
            ctx.text.queue_styled(
                *x,
                *y,
                s,
                color,
                theme::FONT_SIZE(),
                crate::vello_ui::FontStyle::Italic,
                clip,
            );
        } else {
            ctx.text.queue(*x, *y, s, color, clip);
        }
    }

    for (x, y, w, (r, gc, b, a)) in &underlines {
        ctx.dl_rect(*x, *y, *w, 1.5, MuiColor::new(*r, *gc, *b, *a));
    }

    for (x, y, w, (r, gc, b, a)) in &strikethroughs {
        ctx.dl_rect(*x, *y, *w, 1.5, MuiColor::new(*r, *gc, *b, *a));
    }

    for (x, y, w, (r, gc, b, a)) in &overlines {
        ctx.dl_rect(*x, *y, *w, 1.5, MuiColor::new(*r, *gc, *b, *a));
    }

    // Block cursor at the grid cursor position (clamped into the panel).
    let (cr, cc) = cursor;
    if terminal_cursor_draw_visible(cursor_visible, cursor_blinking, frame)
        && cr < rows
        && cc <= cols
    {
        let (cursor_r, cursor_g, cursor_b, cursor_a) = cursor_color;
        let cx = layout::term_cell_x(region, cc);
        let mut cy = layout::term_cell_y(height, cr);
        let mut cw = layout::CHAR_W();
        let mut ch = layout::LINE_H() - 2.0;
        match cursor_shape {
            crate::terminal::CursorShape::Block => {}
            crate::terminal::CursorShape::Underline => {
                let h = 2.0;
                cy += ch - h;
                ch = h;
            }
            crate::terminal::CursorShape::Bar => {
                cw = 2.0;
            }
        }
        unsafe {
            crate::mui_fill_rect(
                handle_ptr,
                cx,
                cy,
                cw,
                ch,
                MuiColor::new(cursor_r, cursor_g, cursor_b, cursor_a),
            );
        }
    }
}

/// Print the live terminal status to stdout (open?, running?, grid dims). Used
/// as launch-test evidence since the Mighty side can't `log` computed ints (L1).
#[no_mangle]
pub extern "C" fn mui_log_terminal(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let (rows, cols) = ctx
            .terminal
            .as_ref()
            .map_or((0, 0), |t| (t.rows(), t.cols()));
        let running = match ctx.terminal.as_mut() {
            Some(t) => t.is_alive(),
            None => false,
        };
        println!(
            "terminal: open={} running={running} grid={rows}x{cols}",
            ctx.term_open
        );
    }
}

/// Smoke export retained from the spike + a scalar variant for the FFI probe.
#[no_mangle]
pub extern "C" fn mui_smoke_add_s(a: i32, b: i32) -> i32 {
    a + b
}

// ---------------------------------------------------------------------------
// Autocomplete dropdown — shim-side engine (logic in completion.rs)
// ---------------------------------------------------------------------------
//
// Mighty can't pass its edit buffer across FFI (L17), so — like find — it
// streams the buffer in byte-by-byte (`mui_complete_reset` + `_push_byte`),
// then asks for completion at a cursor byte-offset (`mui_complete_request`).
// The shim extracts buffer words, optionally merges mty-lsp semantic labels,
// and owns the candidate list + selection. Mighty reads the accepted text back
// and drives the dropdown via the scalar getters/movers below.

/// Begin streaming the editor buffer for a completion request: clear the buffer.
#[no_mangle]
pub extern "C" fn mui_complete_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete_buf.clear();
    }
}

/// Append one editor-buffer byte to the completion buffer.
#[no_mangle]
pub extern "C" fn mui_complete_push_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete_buf.push((byte & 0xff) as u8);
    }
}

/// Translate a 0-based `(line, col)` to a byte offset in `buf` (col is a
/// character count from the line start, clamped to the line length). Shim-side
/// because the editor model exposes cursor columns as character offsets while
/// the completion engine scans UTF-8 bytes.
fn line_col_to_offset(buf: &[u8], line: i32, col: i32) -> usize {
    if line < 0 {
        return 0;
    }
    let target = line as usize;
    let mut l = 0usize;
    let mut i = 0usize;
    // Advance to the start of `target`.
    while i < buf.len() && l < target {
        if buf[i] == b'\n' {
            l += 1;
        }
        i += 1;
    }

    let line_start = i;
    while i < buf.len() && buf[i] != b'\n' {
        i += 1;
    }
    let line_end = i;
    let col = col.max(0) as usize;
    let line_bytes = &buf[line_start..line_end];
    let Ok(line_text) = std::str::from_utf8(line_bytes) else {
        return line_start + col.min(line_bytes.len());
    };
    line_text
        .char_indices()
        .nth(col)
        .map_or(line_end, |(byte, _)| line_start + byte)
}

#[cfg(test)]
mod completion_abi_tests {
    use super::*;

    #[test]
    fn line_col_to_offset_treats_columns_as_utf8_chars() {
        let text = "éα caf\nxx";
        assert_eq!(line_col_to_offset(text.as_bytes(), 0, 0), 0);
        assert_eq!(line_col_to_offset(text.as_bytes(), 0, 1), "é".len());
        assert_eq!(line_col_to_offset(text.as_bytes(), 0, 3), "éα ".len());
        assert_eq!(line_col_to_offset(text.as_bytes(), 0, 6), "éα caf".len());
        assert_eq!(line_col_to_offset(text.as_bytes(), 0, 99), "éα caf".len());
        assert_eq!(line_col_to_offset(text.as_bytes(), 1, 1), "éα caf\nx".len());
    }

    #[test]
    fn line_col_to_offset_falls_back_to_bytes_for_invalid_utf8() {
        let bytes = [b'a', 0xff, b'b', b'\n', b'c'];
        assert_eq!(line_col_to_offset(&bytes, 0, 2), 2);
        assert_eq!(line_col_to_offset(&bytes, 0, 99), 3);
        assert_eq!(line_col_to_offset(&bytes, 1, 1), 5);
    }
}

/// Build the candidate list for the prefix at the cursor `(line, col)` (0-based)
/// in the streamed buffer. Merges mty-lsp semantic labels (best-effort, with a
/// short timeout; silently empty on any failure) ahead of the buffer words.
/// Returns the candidate count (0 leaves the dropdown closed).
///
/// The LSP query uses the active file's path as the document id and the streamed
/// buffer bytes as the document text, so it reflects the live (unsaved) edit.
#[no_mangle]
pub extern "C" fn mui_complete_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let cursor = line_col_to_offset(&ctx.complete_buf, line, col);

    // Best-effort semantic labels from mty-lsp. The buffer is the live source;
    // the path is just the document id. Any failure -> empty -> buffer words.
    let lsp_labels: Vec<String> = match ctx.file_path.clone() {
        Some(path) => {
            let source = String::from_utf8_lossy(&ctx.complete_buf).into_owned();
            lsp_semantic_labels(ctx.language, &path, &source, line.max(0) as u32, col.max(0) as u32)
        }
        None => Vec::new(),
    };

    let n = ctx
        .complete
        .request(&ctx.complete_buf, cursor, &lsp_labels)
        .min(i32::MAX as usize) as i32;
    println!("complete: candidates={n} (lsp={})", lsp_labels.len());
    n
}

/// Number of candidates currently in the dropdown.
#[no_mangle]
pub extern "C" fn mui_complete_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.count() as i32)
}

/// Report an explicit autocomplete request that produced no candidates.
/// Passive typing paths should stay quiet and avoid calling this helper.
#[no_mangle]
pub extern "C" fn mui_complete_report_empty(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.complete.count() > 0 {
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, completion_not_found_message(ctx));
    0
}

/// `1` if the dropdown is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_complete_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.complete.is_active()))
}

fn active_completion_prefix(ctx: &MuiContext, prefix_chars: usize) -> String {
    if prefix_chars == 0 {
        return String::new();
    }
    let text = ctx.tabs.active_model().as_text();
    let (line, col) = {
        let model = ctx.tabs.active_model();
        (model.cursor_line() as i32, model.cursor_col() as i32)
    };
    let mut cursor = line_col_to_offset(text.as_bytes(), line, col).min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    text[..cursor]
        .chars()
        .rev()
        .take(prefix_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// `1` when accepting the selected completion can mutate the active model.
/// Pure preflight: leaves user-facing read-only/no-suggestion feedback to the
/// stateful accept/cancel commands.
#[no_mangle]
pub extern "C" fn mui_complete_can_accept(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() || !ctx.complete.is_active() || ctx.complete.count() == 0 {
        return 0;
    }
    let accepted = ctx.complete.accepted_text();
    if accepted.is_empty() {
        return 0;
    }
    if ctx.complete.accepted_is_snippet() {
        return 1;
    }
    let prefix = active_completion_prefix(ctx, ctx.complete.prefix_len());
    i32::from(accepted != prefix)
}

/// Index (0-based) of the currently selected candidate.
#[no_mangle]
pub extern "C" fn mui_complete_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.selection() as i32)
}

/// Move the selection by `delta` (positive = down), wrapping.
#[no_mangle]
pub extern "C" fn mui_complete_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete.move_sel(delta);
    }
}

/// Select the completion row under the last click. `row` is the screen row used
/// to draw the dropdown, matching [`mui_complete_draw_at`]. Returns the selected
/// candidate index, or `-1` when the click missed the visible rows.
#[no_mangle]
pub extern "C" fn mui_complete_click_at(handle: i64, row: i32, col: i32, total_lines: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.complete.is_active() {
        return -1;
    }
    let region = layout::region(ctx.sidebar_visible);
    let cx = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let cy = layout::row_y_in(region, row);
    ctx.complete.click_row(
        &mut ctx.text,
        ctx.last_event.x,
        ctx.last_event.y,
        cx,
        cy,
        ctx.gpu.width,
        ctx.gpu.height,
    )
}

/// Number of chars before the cursor to delete when accepting (the prefix len).
#[no_mangle]
pub extern "C" fn mui_complete_prefix_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.prefix_len() as i32)
}

/// Number of chars in the accepted (selected) candidate's text.
#[no_mangle]
pub extern "C" fn mui_complete_accept_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.complete.accepted_text().chars().count() as i32)
}

/// The `i`th char (codepoint) of the accepted candidate's text, or `-1` out of
/// range. Mighty reads these to insert the accepted text after deleting the
/// prefix.
#[no_mangle]
pub extern "C" fn mui_complete_accept_char(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.complete
            .accepted_text()
            .chars()
            .nth(i as usize)
            .map_or(-1, |ch| ch as i32)
    })
}

/// Close the dropdown and clear its state.
#[no_mangle]
pub extern "C" fn mui_complete_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.complete.is_active() {
        ctx.complete.cancel();
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No autocomplete suggestions open");
        0
    }
}

/// Draw the dropdown near the cursor pixel `(cursor_px_x, cursor_px_y)`. No-op
/// when the dropdown is closed. Mighty passes the cursor's pixel position; the
/// shim positions the box, clamps it on-screen, and highlights the selection.
#[no_mangle]
pub extern "C" fn mui_complete_draw(handle: i64, cursor_px_x: f32, cursor_px_y: f32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = visible_surface_size(ctx);
    // Split the borrow: `draw` needs `&mut ctx` for both rects + text.
    let engine = std::mem::take(&mut ctx.complete);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, cursor_px_x, cursor_px_y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.complete = engine;
}

/// Compute the cursor's pixel `(x, y)` for the dropdown given the screen `row`
/// and buffer `col`, offset past the gutter sized for `total_lines`. Mighty has
/// no int->float cast (L19), so the pixel math lives here. The result is read
/// back via [`mui_complete_cursor_px_x`] / [`mui_complete_cursor_px_y`] — but to
/// keep the ABI scalar-simple, Mighty instead passes row/col straight to
/// [`mui_complete_draw_at`].
#[no_mangle]
pub extern "C" fn mui_complete_draw_at(
    handle: i64,
    row: i32,
    col: i32,
    total_lines: i32,
) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    let (w, h) = visible_surface_size(ctx);
    let engine = std::mem::take(&mut ctx.complete);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, x, y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.complete = engine;
}

/// Print the live completion state to stdout (candidate count, selection,
/// accepted text). Launch-test evidence for headless runs, since Mighty's `log`
/// is literal-only (L23). No-op on a null handle.
#[no_mangle]
pub extern "C" fn mui_log_completion(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        println!(
            "completion: active={} count={} sel={} prefix_len={} accept=\"{}\"",
            ctx.complete.is_active(),
            ctx.complete.count(),
            ctx.complete.selection(),
            ctx.complete.prefix_len(),
            ctx.complete.accepted_text()
        );
    }
}

/// Launch-test hook: with `MUI_COMPLETE_PROBE` set, run a scripted completion
/// request against the active buffer so a headless run proves the engine wiring
/// (which a non-interactive launch can't trigger via Ctrl+Space). The env value
/// is the prefix to seed (default `"l"`); the probe streams the active tab's
/// bytes, appends the prefix at EOF, requests completion there, and logs the
/// result. No effect unless the env var is set.
#[no_mangle]
pub extern "C" fn mui_complete_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(seed) = std::env::var_os("MUI_COMPLETE_PROBE") else {
        return;
    };
    let prefix = seed.to_string_lossy();
    let prefix = if prefix.trim().is_empty() {
        "l".to_string()
    } else {
        prefix.into_owned()
    };
    // Build a synthetic buffer = active tab bytes + a newline + the prefix.
    let active = ctx.tabs.active();
    let mut buf: Vec<u8> = Vec::new();
    let n = ctx.tabs.load_len(active);
    if n > 0 {
        for i in 0..(n as usize) {
            let b = ctx.tabs.load_byte(active, i);
            if (0..=255).contains(&b) {
                buf.push(b as u8);
            }
        }
    }
    buf.push(b'\n');
    buf.extend_from_slice(prefix.as_bytes());
    let cursor = buf.len();
    ctx.complete_buf = buf;
    let lsp_labels: Vec<String> = match ctx.file_path.clone() {
        Some(path) => {
            let source = String::from_utf8_lossy(&ctx.complete_buf).into_owned();
            // Position at the synthetic prefix: last line, col = prefix len.
            let last_line = source.bytes().filter(|&b| b == b'\n').count() as u32;
            lsp_semantic_labels(
                ctx.language,
                &path,
                &source,
                last_line,
                prefix.chars().count() as u32,
            )
        }
        None => Vec::new(),
    };
    let count = ctx.complete.request(&ctx.complete_buf, cursor, &lsp_labels);
    println!(
        "complete-probe: prefix=\"{prefix}\" candidates={count} lsp={} top=\"{}\"",
        lsp_labels.len(),
        ctx.complete.accepted_text()
    );
}

// ---------------------------------------------------------------------------
// Command palette (Ctrl+Shift+P) — shim-side registry (logic in palette.rs)
// ---------------------------------------------------------------------------
//
// Mirrors the completion dropdown. The command registry + query/filter +
// selection live shim-side (L17/L21: Mighty never holds the command Vec). Mighty
// opens the palette, routes Char/Backspace/Up/Down to it, and on Enter reads the
// selected command id back (`mui_palette_selected_id`) to dispatch to the SAME
// helper the keybinding triggers.

/// Open the command palette: list all commands, select the first, clear the
/// query. Mighty calls this on Ctrl+Shift+P.
#[no_mangle]
pub extern "C" fn mui_palette_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.open();
        trace(&format!("palette_open count={}", ctx.palette.count()));
    }
}

/// Append a typed char (codepoint) to the palette query and refilter. Ignores
/// non-printable / out-of-BMP-as-char values.
#[no_mangle]
pub extern "C" fn mui_palette_push_char(handle: i64, cp: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            ctx.palette.push_char(ch);
            trace(&format!(
                "palette_query query=\"{}\" count={} selected={}",
                ctx.palette.query(),
                ctx.palette.count(),
                ctx.palette.selected_id()
            ));
        }
    }
}

/// Delete the last char of the palette query and refilter.
#[no_mangle]
pub extern "C" fn mui_palette_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.backspace();
    }
}

/// Number of commands currently matching the query.
#[no_mangle]
pub extern "C" fn mui_palette_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.palette.count() as i32)
}

/// Move the palette selection by `delta` (positive = down), wrapping.
#[no_mangle]
pub extern "C" fn mui_palette_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.move_sel(delta);
    }
}

/// Index (0-based) of the currently selected command in the filtered list.
#[no_mangle]
pub extern "C" fn mui_palette_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.palette.selection() as i32)
}

/// Select the command-palette row under the last click. Returns the selected
/// row index, or `-1` if the click missed the visible results.
#[no_mangle]
pub extern "C" fn mui_palette_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let row = ctx
        .palette
        .click_row(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height);
    let id = if row >= 0 { ctx.palette.selected_id() } else { -1 };
    trace(&format!(
        "palette_click row={} id={} x={:.1} y={:.1}",
        row, id, ctx.last_event.x, ctx.last_event.y
    ));
    row
}

/// The command id of the current selection, or `-1` when nothing matches. Mighty
/// reads this on Enter and dispatches to the matching command helper.
#[no_mangle]
pub extern "C" fn mui_palette_selected_id(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        let id = c.palette.selected_id();
        trace(&format!("palette_selected id={id} query=\"{}\"", c.palette.query()));
        if id < 0 {
            c.push_toast(crate::toast::Kind::Info, "No command selected");
        }
        id
    })
}

/// `1` if the palette overlay is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_palette_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.palette.is_active()))
}

/// Close the palette and clear its state (Escape, or after Enter dispatch).
#[no_mangle]
pub extern "C" fn mui_palette_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    trace(&format!("palette_cancel query=\"{}\"", ctx.palette.query()));
    if ctx.palette.is_active() {
        ctx.palette.cancel();
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No command palette open");
        0
    }
}

/// Draw the palette as a centered overlay box (query line + filtered commands
/// with right-aligned keybindings, selection highlighted). No-op when closed.
#[no_mangle]
pub extern "C" fn mui_palette_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.palette.is_active() {
        return;
    }
    let (w, h) = visible_surface_size(ctx);
    // Split the borrow: `draw` needs `&mut ctx` for both rects + text.
    let engine = std::mem::take(&mut ctx.palette);
    let old_clip = ctx.clip;
    ctx.clip = None;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    engine.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.clip = old_clip;
    ctx.palette = engine;
}

// ---------------------------------------------------------------------------
// Keyboard Shortcuts overlay + remapping (Help: Keyboard Shortcuts).
//
// Same shim-owned, scalar-only ABI shape as the palette: Mighty opens the
// overlay, feeds chars/keys, moves the selection, begins capture, records a
// captured chord, resets, and reads rows back char-by-char (strings can't
// cross the FFI, L17). The chord router consults the override map via
// `crate::shortcuts::Overrides::resolve` (see `mui_chord`).
// ---------------------------------------------------------------------------

/// Open the shortcuts overlay (clears the filter, rebuilds the list).
#[no_mangle]
pub extern "C" fn mui_keys_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.shortcuts.open();
        trace("shortcuts_open");
    }
}

/// `1` while the overlay is active.
#[no_mangle]
pub extern "C" fn mui_keys_active(handle: i64) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) if ctx.shortcuts.is_active() => 1,
        _ => 0,
    }
}

/// `1` while in capture mode (waiting for the new chord).
#[no_mangle]
pub extern "C" fn mui_keys_capturing(handle: i64) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) if ctx.shortcuts.is_capturing() => 1,
        _ => 0,
    }
}

/// Append a typed char to the filter (ignored while capturing).
#[no_mangle]
pub extern "C" fn mui_keys_push_char(handle: i64, cp: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(ch) = char::from_u32(cp.max(0) as u32) {
            ctx.shortcuts.push_char(ch);
        }
    }
}

/// Delete the last filter char.
#[no_mangle]
pub extern "C" fn mui_keys_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.shortcuts.backspace();
    }
}

/// Move the selection by `delta` (wraps).
#[no_mangle]
pub extern "C" fn mui_keys_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.shortcuts.move_sel(delta);
    }
}

/// Handle a click in the keyboard-shortcuts row list.
/// Returns `1` when a row was selected, `2` when the already-selected remappable
/// row was clicked and the caller should begin capture, `3` for the close
/// button, `4` reset selected, `5` reset all, or `-1` when the click missed the
/// visible rows.
#[no_mangle]
pub extern "C" fn mui_keys_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    ctx.shortcuts
        .click_action(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height)
}

/// The selected row's command id (`< 0` for fixed rows / no selection).
#[no_mangle]
pub extern "C" fn mui_keys_sel(handle: i64) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx.shortcuts.selected_id(),
        None => -1,
    }
}

/// Number of (filtered) rows.
#[no_mangle]
pub extern "C" fn mui_keys_count(handle: i64) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx.shortcuts.count() as i32,
        None => 0,
    }
}

/// Char length of row `idx`'s name.
#[no_mangle]
pub extern "C" fn mui_keys_row_name_len(handle: i64, idx: i32) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) if idx >= 0 => ctx.shortcuts.row_name(idx as usize).chars().count() as i32,
        _ => 0,
    }
}

/// The `i`th char of row `idx`'s name (`-1` out of range).
#[no_mangle]
pub extern "C" fn mui_keys_row_name_char(handle: i64, idx: i32, i: i32) -> i32 {
    if idx < 0 || i < 0 {
        return -1;
    }
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx
            .shortcuts
            .row_name(idx as usize)
            .chars()
            .nth(i as usize)
            .map(|c| c as i32)
            .unwrap_or(-1),
        None => -1,
    }
}

/// Char length of row `idx`'s key binding string.
#[no_mangle]
pub extern "C" fn mui_keys_row_keys_len(handle: i64, idx: i32) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) if idx >= 0 => ctx.shortcuts.row_keys(idx as usize).chars().count() as i32,
        _ => 0,
    }
}

/// The `i`th char of row `idx`'s key binding (`-1` out of range).
#[no_mangle]
pub extern "C" fn mui_keys_row_keys_char(handle: i64, idx: i32, i: i32) -> i32 {
    if idx < 0 || i < 0 {
        return -1;
    }
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx
            .shortcuts
            .row_keys(idx as usize)
            .chars()
            .nth(i as usize)
            .map(|c| c as i32)
            .unwrap_or(-1),
        None => -1,
    }
}

/// `1` if row `idx` is remappable (router-routed), else `0`.
#[no_mangle]
pub extern "C" fn mui_keys_row_remappable(handle: i64, idx: i32) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) if idx >= 0 && ctx.shortcuts.row_remappable(idx as usize) => 1,
        _ => 0,
    }
}

/// Enter capture mode for the selected row (only if remappable). Returns `1` if
/// capture started, else `0`.
#[no_mangle]
pub extern "C" fn mui_keys_begin_capture(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.shortcuts.begin_capture())
}

/// Record a captured chord `(cp, mods)` as the override for the command in
/// capture mode. `1` = saved, `2` = saved with a conflict warning, `0` = ignored.
#[no_mangle]
pub extern "C" fn mui_keys_capture_chord(handle: i64, cp: i32, mods: i32) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx.shortcuts.capture_chord(cp, mods),
        None => 0,
    }
}

/// Reset the selected row to its default chord. `1` if an override was cleared.
#[no_mangle]
pub extern "C" fn mui_keys_reset(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.shortcuts.reset_selected())
}

/// Reset ALL overrides to defaults.
#[no_mangle]
pub extern "C" fn mui_keys_reset_all(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let _ = ctx.shortcuts.reset_all();
    }
}

/// Palette command: reset the selected shortcut override and make the shortcuts
/// overlay visible so the target row and status are inspectable.
#[no_mangle]
pub extern "C" fn mui_keys_reset_selected_command(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.shortcuts.is_active() {
        ctx.shortcuts.open();
    }
    if ctx.shortcuts.reset_selected() {
        ctx.push_toast(
            crate::toast::Kind::Success,
            "Keyboard Shortcuts reset selected to default",
        );
        1
    } else {
        ctx.push_toast(
            crate::toast::Kind::Info,
            "Keyboard Shortcuts selection already uses default",
        );
        0
    }
}

/// Palette command: reset every shortcut override and make the shortcuts overlay
/// visible so the resulting defaults are inspectable.
#[no_mangle]
pub extern "C" fn mui_keys_reset_all_command(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.shortcuts.is_active() {
        ctx.shortcuts.open();
    }
    if ctx.shortcuts.reset_all() {
        ctx.push_toast(
            crate::toast::Kind::Success,
            "Keyboard Shortcuts reset all to defaults",
        );
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Keyboard Shortcuts already use defaults");
        0
    }
}

/// Cancel: while capturing, exit capture mode; else close the overlay.
#[no_mangle]
pub extern "C" fn mui_keys_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if ctx.shortcuts.is_capturing() {
            ctx.shortcuts.cancel_capture();
        } else {
            trace("shortcuts_close");
            ctx.shortcuts.cancel();
        }
    }
}

/// Close the shortcuts overlay even when a remap capture is active. Returns `1`
/// when it closed the overlay, or `0` when it was already closed.
#[no_mangle]
pub extern "C" fn mui_keys_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.shortcuts.is_active() {
        ctx.shortcuts.cancel();
        ctx.push_toast(crate::toast::Kind::Info, "Keyboard Shortcuts closed");
        trace("shortcuts_close");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Keyboard Shortcuts is already closed");
        trace("shortcuts_close noop");
        0
    }
}

/// Draw the shortcuts overlay (no-op unless active). Same borrow-split as the
/// palette draw.
#[no_mangle]
pub extern "C" fn mui_keys_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.shortcuts.is_active() {
        return;
    }
    let (w, h) = visible_surface_size(ctx);
    let engine = std::mem::take(&mut ctx.shortcuts);
    let old_clip = ctx.clip;
    ctx.clip = None;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    engine.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.clip = old_clip;
    ctx.shortcuts = engine;
}

// ---------------------------------------------------------------------------
// Color theme — query + set the active theme, and the theme-picker overlay.
// ---------------------------------------------------------------------------

/// Number of selectable color themes.
#[no_mangle]
pub extern "C" fn mui_theme_count(_handle: i64) -> i32 {
    crate::theme::ThemeId::ALL.len() as i32
}

/// Index (0-based) of the currently-active theme.
#[no_mangle]
pub extern "C" fn mui_theme_active(_handle: i64) -> i32 {
    crate::theme::active_id().index()
}

/// Set the active theme to index `idx`, persist the choice, and return the
/// applied index (or the current index if `idx` is out of range).
#[no_mangle]
pub extern "C" fn mui_theme_set(_handle: i64, idx: i32) -> i32 {
    if let Some(id) = crate::theme::ThemeId::from_index(idx) {
        crate::theme::set_active(id);
        crate::config::save_theme(id);
        id.index()
    } else {
        crate::theme::active_id().index()
    }
}

/// Length (chars) of theme `idx`'s display name, or `0` if out of range.
#[no_mangle]
pub extern "C" fn mui_theme_name_len(_handle: i64, idx: i32) -> i32 {
    crate::theme::ThemeId::from_index(idx)
        .map(|id| id.name().chars().count() as i32)
        .unwrap_or(0)
}

/// The `i`th char (codepoint) of theme `idx`'s display name, or `-1` out of
/// range. Mighty reads names char-by-char (strings can't cross the FFI, L17).
#[no_mangle]
pub extern "C" fn mui_theme_name_char(_handle: i64, idx: i32, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    crate::theme::ThemeId::from_index(idx)
        .and_then(|id| id.name().chars().nth(i as usize))
        .map(|c| c as i32)
        .unwrap_or(-1)
}

/// Open the theme-picker overlay (remembers the active theme to revert to).
#[no_mangle]
pub extern "C" fn mui_theme_picker_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.theme_picker.open();
        trace("theme_picker_open");
    }
}

/// `1` if the theme picker is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_theme_picker_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.theme_picker.is_active() { 1 } else { 0 })
}

/// Move the picker highlight by `delta` (wrapping) AND preview that theme live.
#[no_mangle]
pub extern "C" fn mui_theme_picker_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.theme_picker.move_sel(delta);
    }
}

/// 0-based highlighted row index in the picker.
#[no_mangle]
pub extern "C" fn mui_theme_picker_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.theme_picker.selection() as i32)
}

/// Preview the theme row under the last click. Returns 1 on a row hit, 0 miss.
#[no_mangle]
pub extern "C" fn mui_theme_picker_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let out = ctx
        .theme_picker
        .click(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height);
    if out == 2 {
        trace("theme_picker_close");
    }
    out
}

/// Commit the highlighted theme (keep + persist), close the picker; returns the
/// committed theme index.
#[no_mangle]
pub extern "C" fn mui_theme_picker_apply(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let id = ctx.theme_picker.commit();
    ctx.push_toast(
        crate::toast::Kind::Info,
        format!("Theme: {}", theme::active_id().name()),
    );
    id
}

/// Cancel the picker, reverting to the theme that was active when it opened.
#[no_mangle]
pub extern "C" fn mui_theme_picker_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.theme_picker.is_active() {
        ctx.theme_picker.cancel();
        ctx.push_toast(crate::toast::Kind::Info, "Color theme picker cancelled");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No color theme picker open");
        0
    }
}

/// Draw the theme-picker overlay (no-op when inactive).
#[no_mangle]
pub extern "C" fn mui_theme_picker_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.theme_picker.is_active() {
        return;
    }
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let picker = std::mem::take(&mut ctx.theme_picker);
    let old_clip = ctx.clip;
    ctx.clip = None;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    picker.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.clip = old_clip;
    ctx.theme_picker = picker;
}

/// Print the live palette state to stdout (count, selection, selected id,
/// query). Launch-test evidence for headless runs (Mighty's `log` is
/// literal-only, L23). No-op on a null handle.
#[no_mangle]
pub extern "C" fn mui_log_palette(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        println!(
            "palette: active={} count={} sel={} selected_id={} query=\"{}\"",
            ctx.palette.is_active(),
            ctx.palette.count(),
            ctx.palette.selection(),
            ctx.palette.selected_id(),
            ctx.palette.query()
        );
    }
}

/// Launch-test hook: with `MUI_PALETTE_PROBE` set, open the palette, type the env
/// value as a query, log the filtered count + selected id, then close it — so a
/// headless run proves the palette wiring (Ctrl+Shift+P can't be delivered
/// non-interactively). The env value is the query to type (default `"sa"`). No
/// effect unless the env var is set.
#[no_mangle]
pub extern "C" fn mui_palette_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(seed) = std::env::var_os("MUI_PALETTE_PROBE") else {
        return;
    };
    let query = seed.to_string_lossy();
    let query = if query.trim().is_empty() {
        "sa".to_string()
    } else {
        query.into_owned()
    };
    ctx.palette.open();
    println!("palette-probe: opened, all-commands count={}", ctx.palette.count());
    for ch in query.chars() {
        ctx.palette.push_char(ch);
    }
    println!(
        "palette-probe: query=\"{}\" count={} sel={} selected_id={}",
        query,
        ctx.palette.count(),
        ctx.palette.selection(),
        ctx.palette.selected_id()
    );
    ctx.palette.cancel();
}

// ---------------------------------------------------------------------------
// Universal Quick-Open (Ctrl+P): files / `>` commands / `@` symbols / `:` line
// ---------------------------------------------------------------------------
//
// One fast fuzzy finder whose mode switches on the first char of the query. The
// file index + MRU + matcher live shim-side (`crate::quickopen`); the Symbols
// and Commands modes pull their data from the active outline / palette registry.
// Mighty opens it, routes Char/Backspace/Up/Down, and on Enter reads back the
// chosen file path (opened as a tab) / symbol line / go-to-line target.

/// The workspace root for the file index: the git toplevel of the active file's
/// directory if one is found by walking up for a `.git`, else the tree root.
///
/// The start dir is resolved to an ABSOLUTE path first (the active file / tree
/// root can be relative, e.g. launched as `mty src/main.mty`), so the `.git`
/// walk yields concrete ancestor dirs rather than an empty relative parent —
/// otherwise an empty `""` parent's `.git` resolves against cwd and we'd index
/// nothing. Falls back to the current dir when no usable root is found.
fn quickopen_root(ctx: &MuiContext) -> PathBuf {
    // An EXPLICIT workspace (Open Folder) wins — the index is rooted there
    // directly rather than re-deriving from the active file's git toplevel.
    if !ctx.workspace.is_empty() {
        return ctx.workspace.root().to_path_buf();
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let start = ctx
        .tabs
        .active_path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let r = ctx.tree.root().to_path_buf();
            if r.as_os_str().is_empty() { cwd.clone() } else { r }
        });
    // Make absolute so ancestor walking is well-defined.
    let start = if start.is_absolute() { start } else { cwd.join(start) };
    let mut dir = start.as_path();
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => dir = p,
            _ => break,
        }
    }
    if start.as_os_str().is_empty() {
        cwd
    } else {
        start
    }
}

/// Re-seed the rows for the modes that draw from OUTER state (Symbols from the
/// outline, Commands from the palette registry). A no-op for Files / GotoLine
/// (the engine owns those). Called after every Quick-Open keystroke so those
/// modes track the query. The outline already reflects the active document (the
/// IDE refreshes it on open / tab switch), so we read it directly here.
fn quickopen_sync_providers(ctx: &mut MuiContext) {
    match ctx.quickopen.mode() {
        crate::quickopen::Mode::Symbols => {
            let syms: Vec<(String, i32, i32)> = ctx
                .outline
                .symbols()
                .iter()
                .enumerate()
                .map(|(i, s)| (s.name.clone(), s.kind as i32, i as i32))
                .collect();
            ctx.quickopen.set_symbol_rows(&syms);
        }
        crate::quickopen::Mode::Commands => {
            // Reuse the palette's fuzzy filter over the static command registry.
            let q = crate::quickopen::Mode::strip(
                crate::quickopen::Mode::Commands,
                ctx.quickopen.query(),
            )
            .to_string();
            let cmds: Vec<(String, String, i32)> =
                crate::palette::filter_commands(crate::palette::COMMANDS, &q)
                    .into_iter()
                    .map(|c| {
                        let secondary = if c.keybinding.is_empty() {
                            crate::palette::command_static_desc(c.id)
                        } else {
                            c.keybinding
                        };
                        (c.label.to_string(), secondary.to_string(), c.id as i32)
                    })
                    .collect();
            ctx.quickopen.set_command_rows(&cmds);
        }
        _ => {}
    }
}

/// Open Quick-Open: ensure the file index is built for the workspace root, then
/// open the finder (empty query → MRU recents). Mighty calls this on Ctrl+P.
#[no_mangle]
pub extern "C" fn mui_quickopen_open(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let root = quickopen_root(ctx);
    let n = ctx.quickopen.ensure_index(&root, false);
    prune_missing_recent_files(ctx);
    ctx.quickopen.open();
    println!("quickopen: opened ({n} files indexed under {})", root.display());
}

/// Force-rebuild the workspace file index (e.g. after files change). Returns the
/// indexed file count.
#[no_mangle]
pub extern "C" fn mui_quickopen_reindex(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let root = quickopen_root(ctx);
    let n = ctx.quickopen.ensure_index(&root, true) as i32;
    ctx.quickopen.refresh_file_rows();
    n
}

/// Append a typed char (codepoint) to the query and recompute. Re-seeds the
/// Symbols/Commands modes from outer state when needed.
#[no_mangle]
pub extern "C" fn mui_qo_push_char(handle: i64, cp: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
        ctx.quickopen.push_char(ch);
        quickopen_sync_providers(ctx);
    }
}

/// Delete the last query char and recompute (Backspace past the prefix returns
/// to Files mode + re-seeds it automatically).
#[no_mangle]
pub extern "C" fn mui_qo_backspace(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let _ = ctx.quickopen.backspace();
    quickopen_sync_providers(ctx);
}

/// Number of result rows for the current query.
#[no_mangle]
pub extern "C" fn mui_qo_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.quickopen.count() as i32)
}

/// `1` when either recent files or recent workspace folders exist.
#[no_mangle]
pub extern "C" fn mui_recent_any(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    prune_missing_recent_files(ctx);
    prune_missing_recent_workspaces(ctx);
    i32::from(!ctx.quickopen.recent_paths().is_empty() || ctx.recent_workspaces.len() > 0)
}

/// Report that Open Recent has nothing actionable after stale entries were
/// pruned. Kept separate from [`mui_recent_any`] so pure availability checks do
/// not emit toasts.
#[no_mangle]
pub extern "C" fn mui_recent_empty(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    prune_missing_recent_files(ctx);
    prune_missing_recent_workspaces(ctx);
    if ctx.quickopen.recent_paths().is_empty() && ctx.recent_workspaces.len() == 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No recent files or folders");
        1
    } else {
        0
    }
}

/// Move the selection by `delta` (positive = down), wrapping.
#[no_mangle]
pub extern "C" fn mui_qo_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.quickopen.move_sel(delta);
    }
}

/// Index (0-based) of the selected row.
#[no_mangle]
pub extern "C" fn mui_qo_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.quickopen.selection() as i32)
}

/// Select the quick-open row under the last click. Returns the selected row
/// index, or `-1` if the click missed the visible results.
#[no_mangle]
pub extern "C" fn mui_qo_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let row = ctx
        .quickopen
        .click_row(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height);
    trace(&format!(
        "quickopen_click mode={} row={} x={:.1} y={:.1}",
        ctx.quickopen.mode().scalar(),
        row,
        ctx.last_event.x,
        ctx.last_event.y
    ));
    row
}

/// `1` if the finder overlay is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_qo_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.quickopen.is_active()))
}

/// The current mode scalar (0 = files, 1 = commands, 2 = symbols, 3 = line).
#[no_mangle]
pub extern "C" fn mui_qo_mode(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.quickopen.mode().scalar())
}

/// Close the finder and clear its transient state (keeps the cached index/MRU).
#[no_mangle]
pub extern "C" fn mui_qo_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.quickopen.is_active() {
        ctx.quickopen.cancel();
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No Quick Open panel open");
        0
    }
}

/// Icon-kind discriminant of row `i` (see `quickopen::Row::ICON_*` + SymKind
/// scalars), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_qo_row_kind(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.quickopen.row(i as usize).map_or(-1, |r| r.icon_kind))
}

/// Char count of row `i`'s name, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_qo_row_name_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.quickopen.row(i as usize).map_or(-1, |r| r.name.chars().count() as i32)
    })
}

/// The `j`th char (codepoint) of row `i`'s name, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_qo_row_name_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.quickopen
            .row(i as usize)
            .and_then(|r| r.name.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

/// Char count of row `i`'s dim secondary (dir) string, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_qo_row_dir_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.quickopen.row(i as usize).map_or(-1, |r| r.dir.chars().count() as i32)
    })
}

/// The `j`th char (codepoint) of row `i`'s dir string, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_qo_row_dir_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.quickopen
            .row(i as usize)
            .and_then(|r| r.dir.chars().nth(j as usize))
            .map_or(-1, |ch| ch as i32)
    })
}

/// `1` if char position `j` of row `i`'s name is a fuzzy-matched char (drawn in
/// the accent), else `0`. The matched-char mask drives the highlight rendering
/// when Mighty draws the rows itself; the shim's own `mui_qo_draw` uses it too.
#[no_mangle]
pub extern "C" fn mui_qo_row_match(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| {
        c.quickopen
            .row(i as usize)
            .map_or(0, |r| i32::from(r.indices.contains(&(j as usize))))
    })
}

/// Accept row `i` (`-1` = current selection) and act on it by mode:
///   * Files / recents → open the file as a tab, returning the new tab index;
///   * Symbols → jump to the symbol's line, returning the line (0-based);
///   * Go-to-line → move the cursor to the line, returning the line (0-based).
///
/// Closes the finder. Returns `-1` on a non-actionable / empty selection.
#[no_mangle]
pub extern "C" fn mui_qo_accept(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.quickopen.is_active() {
        ctx.push_toast(crate::toast::Kind::Info, "No Quick Open panel open");
        return -1;
    }
    let mode = ctx.quickopen.mode();
    let mut keep_open = false;
    let result: i32 = match mode {
        crate::quickopen::Mode::Files => {
            match ctx.quickopen.accept_file_path(i) {
                Some(path) if path.is_file() => {
                    let idx = ctx.tabs.open_path(path.clone());
                    sync_active_path(ctx);
                    record_opened_file(ctx, &path);
                    idx as i32
                }
                Some(path) => {
                    let removed = ctx.quickopen.remove_recent_path(&path);
                    if removed {
                        persist_recent_files(ctx);
                    }
                    refresh_workspace_file_views(ctx);
                    ctx.push_toast(
                        crate::toast::Kind::Warn,
                        format!("Quick Open target missing: {}", basename(&path)),
                    );
                    keep_open = true;
                    -1
                }
                _ => {
                    let message = if i < 0 {
                        "No Quick Open result selected"
                    } else {
                        "Quick Open row no longer listed"
                    };
                    ctx.push_toast(crate::toast::Kind::Info, message);
                    keep_open = true;
                    -1
                }
            }
        }
        crate::quickopen::Mode::Symbols => {
            let sym = ctx.quickopen.accept_symbol(i);
            if sym < 0 {
                let message = if i < 0 {
                    "No symbol selected"
                } else {
                    "Symbol row no longer listed"
                };
                ctx.push_toast(crate::toast::Kind::Info, message);
                keep_open = true;
                -1
            } else {
                let line = ctx.outline.line_of(sym as usize);
                if line < 0 {
                    ctx.push_toast(crate::toast::Kind::Info, "Symbol row no longer listed");
                    keep_open = true;
                    -1
                } else {
                    let model = ctx.tabs.active_model_mut();
                    model.move_to(line, 0);
                    let first = (line - 2).max(0);
                    model.set_first_visible(first as usize);
                    let _ = ctx.outline.set_cursor(line as u32);
                    line
                }
            }
        }
        crate::quickopen::Mode::GotoLine => {
            let n = ctx.quickopen.goto_line();
            if n < 1 {
                ctx.push_toast(crate::toast::Kind::Info, "Enter a line number");
                keep_open = true;
                -1
            } else {
                let line = n - 1;
                let model = ctx.tabs.active_model_mut();
                model.move_to(line, 0);
                let first = (line - 2).max(0);
                model.set_first_visible(first as usize);
                line
            }
        }
        // Commands mode is dispatched on the Mighty side via the palette id;
        // see `mui_qo_command_id`.
        crate::quickopen::Mode::Commands => -1,
    };
    if !keep_open {
        ctx.quickopen.cancel();
    }
    result
}

/// In Commands mode (`>` query), the palette command id of the selected row
/// (`-1` = current), or `-1` when not in Commands mode / no match. Mighty reads
/// this on Enter and dispatches to the SAME helper a keybinding triggers.
#[no_mangle]
pub extern "C" fn mui_qo_command_id(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if ctx.quickopen.mode() != crate::quickopen::Mode::Commands {
        return -1;
    }
    let idx = if i < 0 { ctx.quickopen.selection() } else { i as usize };
    let id = ctx.quickopen.row(idx).map(|r| r.target).unwrap_or(-1);
    if id < 0 {
        let message = if i < 0 {
            "No command selected"
        } else {
            "Command row no longer listed"
        };
        ctx.push_toast(crate::toast::Kind::Info, message);
    }
    id
}

/// Record `path`-by... — record the ACTIVE file as recently-opened. Called by
/// Mighty whenever a file opens via any path (tabs, tree, prompt) so the MRU
/// reflects real usage. No-op if there is no active file path.
#[no_mangle]
pub extern "C" fn mui_qo_record_active(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(p) = ctx.tabs.active_path() {
            record_opened_file(ctx, &p);
        }
    }
}

/// Draw the Quick-Open overlay (no-op unless active). Centered card over the UI.
#[no_mangle]
pub extern "C" fn mui_qo_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.quickopen.is_active() {
        return;
    }
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let qo = std::mem::take(&mut ctx.quickopen);
    let old_clip = ctx.clip;
    ctx.clip = None;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    qo.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.clip = old_clip;
    ctx.quickopen = qo;
}

// ---------------------------------------------------------------------------
// hover + go-to-definition (sub-project 7): shim-side LSP nav
// ---------------------------------------------------------------------------
//
// Like completion, Mighty streams the live buffer into the shim (it can't pass a
// buffer across FFI, L17), then asks for hover/definition at the cursor
// `(line, col)` (0-based). The shim spawns `mty lsp`, runs the staged handshake
// (L24), fires the request, parses the answer, and owns the result state. Mighty
// reads scalars back: hover availability + a draw call; definition path-match +
// target line/col + an open-target call.

/// Begin streaming the editor buffer for a hover/def request: clear the buffer.
#[no_mangle]
pub extern "C" fn mui_nav_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.nav_buf.clear();
    }
}

/// Append one editor-buffer byte to the nav (hover/def) buffer.
#[no_mangle]
pub extern "C" fn mui_nav_push_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.nav_buf.push((byte & 0xff) as u8);
    }
}

/// Request hover at the cursor `(line, col)` (0-based) over the streamed buffer.
/// Spawns `mty lsp` (best-effort, short timeout), parses the hover markup, wraps
/// it to a small popup, and stores it. Returns `1` if hover text is available,
/// else `0` (and clears any prior popup). Graceful no-op if the buffer is empty
/// or the server is absent.
#[no_mangle]
pub extern "C" fn mui_hover_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.hover.clear();
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                language_needs_file_message(ctx, "hover"),
            );
            return 0;
        }
    };
    let source = String::from_utf8_lossy(&ctx.nav_buf).into_owned();
    let raw = lsp_hover_raw(ctx.language, &path, &source, line.max(0) as u32, col.max(0) as u32);
    let available = match crate::nav::parse_hover_value(&raw) {
        Some(v) => ctx.hover.set_text(&v),
        None => false,
    };
    println!(
        "hover: line={} col={} available={} lines={}",
        line,
        col,
        available,
        ctx.hover.line_count()
    );
    if !available {
        ctx.push_toast(
            crate::toast::Kind::Info,
            hover_not_found_message(&path, line, col),
        );
    }
    i32::from(available)
}

/// `1` if a hover popup is currently active.
#[no_mangle]
pub extern "C" fn mui_hover_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.hover.is_active()))
}

/// Clear the hover popup.
#[no_mangle]
pub extern "C" fn mui_hover_clear(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.hover.clear();
    }
}

/// Close the hover popup as an explicit command. Returns `1` when it closed an
/// active popup and `0` when no hover popup was open.
#[no_mangle]
pub extern "C" fn mui_hover_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.hover.is_active() {
        ctx.hover.clear();
        ctx.push_toast(crate::toast::Kind::Info, "Hover popup closed");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No hover popup open");
        0
    }
}

/// Draw the hover popup near the cursor `(row, col)` (screen row + buffer col),
/// offset past the gutter sized for `total_lines`. No-op when no hover is active.
/// Mirrors `mui_complete_draw_at`'s pixel math (Mighty has no int->float, L19).
#[no_mangle]
pub extern "C" fn mui_hover_draw(handle: i64, row: i32, col: i32, total_lines: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.hover.is_active() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let hover = std::mem::take(&mut ctx.hover);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    hover.draw(ctx, x, y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.hover = hover;
}

/// Request go-to-definition at the cursor `(line, col)` (0-based) over the
/// streamed buffer. Spawns `mty lsp`, parses the `Location`, resolves the uri to
/// a path, and stores the target. Returns `1` if a definition location was
/// found, else `0` (and clears any prior target).
#[no_mangle]
pub extern "C" fn mui_def_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.def.clear();
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                language_needs_file_message(ctx, "Go to Definition"),
            );
            return 0;
        }
    };
    let source = String::from_utf8_lossy(&ctx.nav_buf).into_owned();
    let raw = lsp_def_raw(ctx.language, &path, &source, line.max(0) as u32, col.max(0) as u32);
    let found = match crate::nav::parse_definition(&raw) {
        Some((uri, tline, tcol)) => {
            match definition_target_from_lsp(ctx.language, &path, &source, &uri, tline, tcol) {
                Some(target) => {
                    ctx.def.set(Some(target));
                    true
                }
                None => false,
            }
        }
        None => false,
    };
    println!("def: line={line} col={col} found={found}");
    if !found {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            definition_not_found_message(&path, line, col),
        );
    }
    i32::from(found)
}

/// `1` if the resolved definition target is in the CURRENTLY ACTIVE file (so
/// Mighty moves the cursor in place rather than opening a tab). `0` if there is
/// no target or it is in another file.
#[no_mangle]
pub extern "C" fn mui_def_path_matches_current(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let current = ctx.file_path.clone();
    i32::from(ctx.def.path_matches(current.as_deref()))
}

/// 0-based target line of the resolved definition, or `-1` if none.
#[no_mangle]
pub extern "C" fn mui_def_target_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.def.target().map_or(-1, |t| t.line.min(i32::MAX as u32) as i32)
    })
}

/// 0-based target column of the resolved definition, or `-1` if none.
#[no_mangle]
pub extern "C" fn mui_def_target_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.def.target().map_or(-1, |t| t.col.min(i32::MAX as u32) as i32)
    })
}

/// Open the resolved definition target's file as a tab (via the existing tab
/// store) and switch to it. Returns the tab index, or `-1` if there is no target
/// / no path. Keeps `file_path` in sync so a follow-up hover/def queries the
/// right document. Mighty calls this only when the target is in another file
/// (after byte-swapping the live buffer into its own slot, as for any tab open).
#[no_mangle]
pub extern "C" fn mui_def_open_target(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let target_path = match ctx.def.target() {
        Some(t) => t.path.clone(),
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "No definition target selected");
            return -1;
        }
    };
    if !target_path.exists() {
        let name = target_path.file_name().and_then(|s| s.to_str()).unwrap_or("source");
        ctx.def.clear();
        refresh_workspace_file_views(ctx);
        ctx.push_toast(crate::toast::Kind::Warn, format!("Definition target missing: {name}"));
        return -1;
    }
    let idx = ctx.tabs.open_path(target_path.clone());
    sync_active_path(ctx);
    record_opened_file(ctx, &target_path);
    idx as i32
}

/// Launch-test hook: with `MUI_NAV_PROBE` set, run scripted hover + definition
/// requests against a synthetic buffer so a headless run proves the wiring
/// (F12 / the hover key can't be delivered non-interactively). The env value is
/// an optional symbol whose definition+hover to probe (default a small built-in
/// program). Logs the parsed results to stdout. No effect unless the var is set.
#[no_mangle]
pub extern "C" fn mui_nav_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if std::env::var_os("MUI_NAV_PROBE").is_none() {
        return;
    }
    // A self-contained program where `add` is defined on line 0 and used on
    // line 5; hover + definition are probed on the use site (line 5, col 10).
    let source = "fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let r = add(1, 2)\n}\n";
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => {
            println!("nav-probe: no file_path — skipped");
            return;
        }
    };
    let hraw = crate::nav::lsp::request(&path, source, 5, 10, crate::nav::lsp::Req::Hover);
    match crate::nav::parse_hover_value(&hraw) {
        Some(v) => {
            let one_line = v.replace('\n', " ");
            println!("nav-probe: hover=\"{}\"", one_line.trim());
        }
        None => println!("nav-probe: hover=<none>"),
    }
    let draw = crate::nav::lsp::request(&path, source, 5, 10, crate::nav::lsp::Req::Definition);
    match crate::nav::parse_definition(&draw) {
        Some((uri, line, col)) => {
            let resolved = crate::nav::uri_to_path(&uri)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| uri.clone());
            println!("nav-probe: def line={line} col={col} path=\"{resolved}\"");
        }
        None => println!("nav-probe: def=<none>"),
    }
}

// ---------------------------------------------------------------------------
// Deeper language intelligence — signature help / rename / code actions
// ---------------------------------------------------------------------------
//
// Like hover/def, all three spawn `mty lsp`, run the staged handshake (L24), fire
// one request over the LIVE active-model text, parse the answer, and own the UI
// state. Mighty drives them through scalar getters/actions and reads the result
// back. mty-lsp (v0.5) implements all three (verified): signatureHelp, rename
// (changes WorkspaceEdit) + prepareRename, codeAction (quickfix / refactor /
// source.fixAll.mighty kinds). `mty fix --apply` exists for the synthetic
// "Fix all (mty)" action.

/// The source text of the active model + its cursor as 0-based (line, col).
pub(crate) fn active_source_and_cursor(ctx: &MuiContext) -> (String, u32, u32) {
    let m = ctx.tabs.active_model();
    (
        m.as_text(),
        m.cursor_line() as u32,
        m.cursor_col() as u32,
    )
}

fn is_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic()
}

/// Extract the identifier that contains or ends at the char `col` on `line` of
/// `text`. Returns `""` if the cursor isn't on an identifier (used to prefill
/// the rename input).
fn identifier_at(text: &str, line: u32, col: u32) -> String {
    let line_str = text.split('\n').nth(line as usize).unwrap_or("");
    let chars: Vec<char> = line_str.chars().collect();
    let n = chars.len();
    let c = (col as usize).min(n);
    // Find an identifier covering the cursor: scan left for the start, right for
    // the end, allowing the cursor to sit just after the identifier too.
    let mut start = c;
    while start > 0 && is_identifier_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = c;
    while end < n && is_identifier_char(chars[end]) {
        end += 1;
    }
    if start == end {
        return String::new();
    }
    if !is_identifier_start(chars[start]) {
        return String::new();
    }
    chars[start..end].iter().collect()
}

// ---- signature help ----

/// Request signature help at the cursor `(line, col)` (0-based) over the active
/// model. Spawns `mty lsp`, parses `SignatureInformation`, stores the popup.
/// Returns `1` if a signature is available, else `0` (clearing any prior popup).
#[no_mangle]
pub extern "C" fn mui_sig_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.sig.clear();
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                language_needs_file_message(ctx, "signature help"),
            );
            return 0;
        }
    };
    let (source, _, _) = active_source_and_cursor(ctx);
    let raw = lsp_signature_raw(
        ctx.language,
        &path,
        &source,
        line.max(0) as u32,
        col.max(0) as u32,
    );
    let available = match crate::language::parse_signature_help(&raw) {
        Some(sig) => ctx.sig.set(Some(sig)),
        None => false,
    };
    println!("sig: line={line} col={col} available={available}");
    if !available {
        ctx.push_toast(
            crate::toast::Kind::Info,
            signature_not_found_message(&path, line, col),
        );
    }
    i32::from(available)
}

/// `1` if a signature-help popup is currently active.
#[no_mangle]
pub extern "C" fn mui_sig_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.sig.is_active()))
}

/// Clear the signature-help popup.
#[no_mangle]
pub extern "C" fn mui_sig_clear(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.sig.clear();
    }
}

/// Close the signature-help popup as an explicit command. Returns `1` when it
/// closed an active popup and `0` when no signature-help popup was open.
#[no_mangle]
pub extern "C" fn mui_sig_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.sig.is_active() {
        ctx.sig.clear();
        ctx.push_toast(crate::toast::Kind::Info, "Signature Help popup closed");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No signature help popup open");
        0
    }
}

/// Draw the signature popup ABOVE the cursor `(row, col)` (screen row + buffer
/// col), offset past the gutter sized for `total_lines`. No-op when inactive.
#[no_mangle]
pub extern "C" fn mui_sig_draw(handle: i64, row: i32, col: i32, total_lines: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sig.is_active() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let sig = std::mem::take(&mut ctx.sig);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    sig.draw_inset(ctx, x, y, w, h, region.left + 8.0);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.sig = sig;
}

// ---- rename symbol (F2) ----

/// Prepare a rename at the cursor `(line, col)`: derive the symbol under the
/// cursor, honoring `prepareRename`'s accepted range/rejection when the server
/// answers, and open the inline rename input prefilled with it. Returns `1` if
/// a renamable symbol was found, else `0` (input not opened).
#[no_mangle]
pub extern "C" fn mui_rename_prepare(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (source, _, _) = active_source_and_cursor(ctx);
    let line0 = line.max(0) as u32;
    let col0 = col.max(0) as u32;
    let mut symbol = identifier_at(&source, line0, col0);
    if let Some(path) = ctx.file_path.clone() {
        let raw = lsp_prepare_rename_raw(ctx.language, &path, &source, line0, col0);
        if prepare_rename_explicitly_rejected(&raw) {
            println!("rename: line={line} col={col} prepare-rejected");
            ctx.push_toast(
                crate::toast::Kind::Info,
                rename_not_found_message(ctx, line, col),
            );
            return 0;
        }
        // prepareRename returns a range; re-derive the symbol from its start.
        if let Some((sl, sc)) = parse_prepare_rename_start(&raw) {
            let sc = if ctx.language == Language::Mighty {
                sc
            } else {
                source_utf16_col_to_char(&source, sl, sc)
            };
            symbol = identifier_at(&source, sl, sc);
        }
    }
    if symbol.is_empty() || is_non_renamable_identifier(&symbol) {
        println!("rename: line={line} col={col} no-symbol");
        ctx.push_toast(
            crate::toast::Kind::Info,
            rename_not_found_message(ctx, line, col),
        );
        return 0;
    }
    ctx.rename.open(&symbol);
    println!("rename: prepare symbol=\"{symbol}\"");
    1
}

fn prepare_rename_explicitly_rejected(json: &str) -> bool {
    let bytes = json.as_bytes();
    if top_level_json_field_value_start(bytes, "method").is_some() {
        return false;
    }
    top_level_json_field_value_start(bytes, "error").is_some()
        || top_level_json_field_value_start(bytes, "result")
            .is_some_and(|i| bytes.get(i..i + 4) == Some(b"null"))
}

fn top_level_json_field_value_start(bytes: &[u8], field: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                let (key, past) = read_json_string_at(bytes, i)?;
                if depth == 1 && key == field {
                    let mut value_at = past;
                    while value_at < bytes.len()
                        && matches!(bytes[value_at], b' ' | b':' | b'\t' | b'\r' | b'\n')
                    {
                        value_at += 1;
                    }
                    return Some(value_at);
                }
                i = past;
            }
            _ => i += 1,
        }
    }
    None
}

fn read_json_string_at(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    if bytes.get(pos) != Some(&b'"') {
        return None;
    }
    let mut val = String::new();
    let mut i = pos + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((val, i + 1)),
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                val.push(bytes[i] as char);
                i += 1;
            }
            b => {
                val.push(b as char);
                i += 1;
            }
        }
    }
    None
}

/// Parse the `prepareRename` result's start `(line, character)`. The result is a
/// `Range` `{"start":{"line":N,"character":N},"end":{...}}`.
fn parse_prepare_rename_start(json: &str) -> Option<(u32, u32)> {
    let bytes = json.as_bytes();
    if top_level_json_field_value_start(bytes, "method").is_some() {
        return None;
    }
    let result = top_level_json_object_field(bytes, "result")?;
    let range = top_level_json_object_field(result, "range").unwrap_or(result);
    let start = top_level_json_object_field(range, "start")?;
    let line = top_level_json_uint_field(start, "line")?;
    let col = top_level_json_uint_field(start, "character")?;
    Some((line, col))
}

fn top_level_json_object_field<'a>(bytes: &'a [u8], field: &str) -> Option<&'a [u8]> {
    let value_at = top_level_json_field_value_start(bytes, field)?;
    if bytes.get(value_at) != Some(&b'{') {
        return None;
    }
    let end = match_json_enclosed(bytes, value_at, b'{', b'}').min(bytes.len());
    Some(&bytes[value_at..end])
}

fn top_level_json_uint_field(bytes: &[u8], field: &str) -> Option<u32> {
    let mut i = top_level_json_field_value_start(bytes, field)?;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    let mut value = 0u32;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((bytes[i] - b'0') as u32);
        i += 1;
    }
    if i == start {
        None
    } else {
        Some(value)
    }
}

fn match_json_enclosed(bytes: &[u8], open_at: usize, open: u8, close: u8) -> usize {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut i = open_at;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    bytes.len()
}

fn is_non_renamable_identifier(symbol: &str) -> bool {
    matches!(
        symbol,
        "agent"
            | "as"
            | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "else"
            | "enum"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "protocol"
            | "pub"
            | "return"
            | "self"
            | "struct"
            | "true"
            | "type"
            | "use"
            | "while"
    )
}

#[cfg(test)]
mod rename_prepare_tests {
    use super::*;

    #[test]
    fn prepare_rename_rejects_explicit_lsp_failure() {
        assert!(prepare_rename_explicitly_rejected(
            r#"{"jsonrpc":"2.0","result":null,"id":3}"#
        ));
        assert!(prepare_rename_explicitly_rejected(
            r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"not a symbol"},"id":3}"#
        ));
        assert!(!prepare_rename_explicitly_rejected(
            r#"{"jsonrpc":"2.0","result":{"start":{"line":4,"character":8},"end":{"line":4,"character":12}},"id":3}"#
        ));
    }

    #[test]
    fn prepare_rename_rejection_only_reads_top_level_jsonrpc_fields() {
        assert!(!prepare_rename_explicitly_rejected(
            r#"{"jsonrpc":"2.0","result":{"range":{"start":{"line":4,"character":8},"end":{"line":4,"character":12}},"placeholder":"error"},"id":3}"#
        ));
        assert!(!prepare_rename_explicitly_rejected(
            r#"{"jsonrpc":"2.0","result":{"start":{"line":4,"character":8},"end":{"line":4,"character":12},"metadata":{"error":{"message":"not json-rpc failure"}}},"id":3}"#
        ));
        assert!(!prepare_rename_explicitly_rejected(
            r#"{"jsonrpc":"2.0","id":3,"method":"workspace/applyEdit","result":null}"#
        ));
    }

    #[test]
    fn prepare_rename_start_reads_server_range() {
        let raw = r#"{"jsonrpc":"2.0","result":{"start":{"line":4,"character":8},"end":{"line":4,"character":12}},"id":3}"#;
        assert_eq!(parse_prepare_rename_start(raw), Some((4, 8)));
    }

    #[test]
    fn prepare_rename_start_reads_result_range_owner() {
        let raw = r#"{"jsonrpc":"2.0","metadata":{"start":{"line":99,"character":1}},"result":{"metadata":{"start":{"line":98,"character":2}},"range":{"metadata":{"start":{"line":97,"character":3}},"start":{"metadata":{"line":96,"character":4},"line":4,"character":8},"end":{"line":4,"character":12}},"placeholder":"name"},"id":3}"#;
        assert_eq!(parse_prepare_rename_start(raw), Some((4, 8)));
    }

    #[test]
    fn prepare_rename_start_ignores_result_metadata_start() {
        let raw = r#"{"jsonrpc":"2.0","result":{"metadata":{"start":{"line":99,"character":1}},"start":{"metadata":{"line":98,"character":2},"line":5,"character":9},"end":{"line":5,"character":13}},"id":3}"#;
        assert_eq!(parse_prepare_rename_start(raw), Some((5, 9)));
    }

    #[test]
    fn prepare_rename_start_requires_response_envelope() {
        let raw = r#"{"jsonrpc":"2.0","id":3,"method":"workspace/applyEdit","result":{"start":{"line":9,"character":1},"end":{"line":9,"character":4}}}"#;
        assert_eq!(parse_prepare_rename_start(raw), None);
    }

    #[test]
    fn rename_prepare_filters_keywords_but_allows_identifiers() {
        for kw in ["fn", "let", "struct", "agent", "protocol", "self"] {
            assert!(is_non_renamable_identifier(kw), "{kw} should not open symbol rename");
        }
        for ident in ["add", "workspace_root", "EditorAgent", "café", "δοκιμή"] {
            assert!(
                !is_non_renamable_identifier(ident),
                "{ident} should remain eligible for symbol rename"
            );
        }
    }

    #[test]
    fn identifier_at_rejects_numeric_literals_and_reads_symbol_edges() {
        let src = "fn add(value: I32) {\n  let thing_2 = value\n  1234\n}";
        assert_eq!(identifier_at(src, 0, 4), "add");
        assert_eq!(identifier_at(src, 1, 13), "thing_2");
        assert_eq!(identifier_at(src, 2, 2), "");
    }

    #[test]
    fn identifier_at_supports_unicode_symbols() {
        let src = "fn café(δοκιμή: I32) {\n  let 東京_2 = δοκιμή\n}";
        assert_eq!(identifier_at(src, 0, 5), "café");
        assert_eq!(identifier_at(src, 0, 10), "δοκιμή");
        assert_eq!(identifier_at(src, 1, 8), "東京_2");
        assert_eq!(identifier_at(src, 1, 17), "δοκιμή");
    }

    #[test]
    fn prepare_rename_utf16_start_maps_to_identifier_column() {
        let src = "😀target";
        let raw = r#"{"jsonrpc":"2.0","result":{"start":{"line":0,"character":2},"end":{"line":0,"character":8}},"id":3}"#;
        let (line, utf16_col) = parse_prepare_rename_start(raw).unwrap();
        let char_col = source_utf16_col_to_char(src, line, utf16_col);
        assert_eq!(char_col, 1);
        assert_eq!(identifier_at(src, line, char_col), "target");
    }

    #[test]
    fn identifier_at_rejects_unicode_numeric_literals() {
        let src = "let x = １２３\nlet y = 2fast";
        assert_eq!(identifier_at(src, 0, 9), "");
        assert_eq!(identifier_at(src, 1, 10), "");
    }

    #[test]
    fn fallback_rename_edits_respect_unicode_identifier_boundaries() {
        let src = "let café = 1\nlet decafé = café + café_2\ncafé\n";
        let edits = fallback_rename_edits(src, "café");
        let ranges: Vec<(u32, u32, u32)> = edits
            .iter()
            .map(|e| (e.start_line, e.start_col, e.end_col))
            .collect();
        assert_eq!(ranges, vec![(0, 4, 8), (1, 13, 17), (2, 0, 4)]);
    }

    #[test]
    fn fallback_rename_edits_emit_lsp_utf16_columns() {
        let src = "\u{1f600} target\nplain target";
        let edits = fallback_rename_edits(src, "target");
        let ranges: Vec<(u32, u32, u32)> = edits
            .iter()
            .map(|e| (e.start_line, e.start_col, e.end_col))
            .collect();
        assert_eq!(ranges, vec![(0, 3, 9), (1, 6, 12)]);

        let mut edits = edits;
        for edit in &mut edits {
            edit.new_text = "next".to_string();
        }
        assert_eq!(
            crate::language::apply_text_edits(src, &edits),
            "\u{1f600} next\nplain next"
        );
    }
}

/// Open the rename input directly with an explicit `symbol` (used when Mighty
/// already knows the identifier; kept simple for the ABI). Returns `1`.
#[no_mangle]
pub extern "C" fn mui_rename_open(handle: i64, line: i32, col: i32) -> i32 {
    mui_rename_prepare(handle, line, col)
}

/// Append one Unicode scalar to the rename new-name buffer.
#[no_mangle]
pub extern "C" fn mui_rename_push_char(handle: i64, codepoint: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if codepoint >= 0 {
            ctx.rename.push(codepoint as u32);
        }
    }
}

/// Remove the last char of the rename buffer.
#[no_mangle]
pub extern "C" fn mui_rename_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.rename.backspace();
    }
}

/// `1` while the rename inline input is active.
#[no_mangle]
pub extern "C" fn mui_rename_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.rename.is_active()))
}

/// `1` when committing the open rename input can attempt an edit.
/// Pure preflight: no toasts; `mui_rename_commit` remains the stateful reporter.
#[no_mangle]
pub extern "C" fn mui_rename_can_commit(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() || ctx.file_path.is_none() || !ctx.rename.is_active() {
        return 0;
    }
    let new_name = ctx.rename.name_string();
    i32::from(!new_name.is_empty() && new_name != ctx.rename.original())
}

/// Cancel the rename input (discard the buffer + any staged edit).
#[no_mangle]
pub extern "C" fn mui_rename_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.rename.is_active() {
        ctx.rename.cancel();
        ctx.push_toast(crate::toast::Kind::Info, "Rename cancelled");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No rename input open");
        0
    }
}

/// Commit the rename: fire `textDocument/rename` with the typed new name at the
/// cursor `(line, col)`, parse the `WorkspaceEdit`, apply it to every affected
/// file (the active buffer's model in-place; other files on disk, refreshing any
/// open tab for them), and save the active file. Returns the number of FILES
/// changed (>= 1 on success), `0` if rename produced no edit, or `-1` on error.
///
/// Falls back to a workspace-wide identifier replace scoped to the original
/// symbol (active file only) when the server returns no edit — clearly logged.
#[no_mangle]
pub extern "C" fn mui_rename_commit(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.rename.is_active() {
        return -1;
    }
    let new_name = ctx.rename.name_string();
    let original = ctx.rename.original().to_string();
    if new_name.is_empty() || new_name == original {
        ctx.rename.cancel();
        return 0;
    }
    let path = match ctx.file_path.clone() {
        Some(p) => p,
        None => {
            ctx.rename.cancel();
            return -1;
        }
    };
    let (source, _, _) = active_source_and_cursor(ctx);
    let raw = lsp_rename_raw(
        ctx.language,
        &path,
        &source,
        line.max(0) as u32,
        col.max(0) as u32,
        new_name.clone(),
    );
    let mut we = crate::language::parse_workspace_edit(&raw);

    // Fallback: server gave nothing — do a scoped identifier replace in the
    // ACTIVE buffer only, clearly flagged as a fallback.
    let mut fallback = false;
    if we.is_empty() {
        fallback = true;
        let edits = fallback_rename_edits(&source, &original);
        if edits.is_empty() {
            ctx.rename.cancel();
            println!("rename: commit new=\"{new_name}\" edits=0 (no LSP, no fallback match)");
            return 0;
        }
        let uri = crate::language::lsp::file_uri(&path);
        we.files.push((uri, edits));
    }

    let result = apply_workspace_edit(ctx, &we, &new_name);
    ctx.rename.set_edit(Some(we));
    if result.skipped_dirty > 0 {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            result
                .first_skipped_dirty_message
                .as_deref()
                .unwrap_or("Skipped dirty file during workspace edit"),
        );
    }
    ctx.rename.cancel();
    let files_changed = result.changed;
    println!(
        "rename: commit new=\"{new_name}\" files={files_changed} fallback={fallback}"
    );
    files_changed
}

/// Build fallback rename edits: every whole-word occurrence of `symbol` in
/// `source`, as `TextEdit`s. A coarse but clearly-labeled fallback used only when
/// the LSP returns no `WorkspaceEdit`.
fn fallback_rename_edits(source: &str, symbol: &str) -> Vec<crate::language::TextEdit> {
    let mut out = Vec::new();
    if symbol.is_empty() {
        return out;
    }
    let sym_chars: Vec<char> = symbol.chars().collect();
    let slen = sym_chars.len();
    for (li, raw_line) in source.split('\n').enumerate() {
        let chars: Vec<char> = raw_line.chars().collect();
        let mut i = 0usize;
        while i + slen <= chars.len() {
            if chars[i..i + slen] == sym_chars[..] {
                let before_ok = i == 0 || !is_identifier_char(chars[i - 1]);
                let after_ok =
                    i + slen == chars.len() || !is_identifier_char(chars[i + slen]);
                if before_ok && after_ok {
                    let start_col: u32 = chars[..i]
                        .iter()
                        .map(|ch| ch.len_utf16() as u32)
                        .sum();
                    let end_col = start_col
                        + chars[i..i + slen]
                            .iter()
                            .map(|ch| ch.len_utf16() as u32)
                            .sum::<u32>();
                    out.push(crate::language::TextEdit {
                        start_line: li as u32,
                        start_col,
                        end_line: li as u32,
                        end_col,
                        new_text: String::new(), // filled by apply_workspace_edit
                    });
                    i += slen;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

/// Apply a [`WorkspaceEdit`](crate::language::WorkspaceEdit) across files,
/// substituting `new_name` for any fallback edit whose `new_text` is empty (the
/// LSP edits already carry their text). The active file's model is mutated
/// in-place + saved; other clean files are rewritten on disk and any open tab
/// for them is reloaded. Dirty non-active tabs are left untouched.
struct WorkspaceEditApplyResult {
    changed: i32,
    skipped_dirty: i32,
    skipped_missing: i32,
    first_skipped_dirty_message: Option<String>,
    first_skipped_missing_message: Option<String>,
}

fn skipped_dirty_workspace_edit_message(path: &std::path::Path) -> String {
    format!(
        "Skipped dirty file during workspace edit: {}",
        basename(path)
    )
}

fn workspace_edits_can_create_missing_file(edits: &[crate::language::TextEdit]) -> bool {
    !edits.is_empty()
        && edits.iter().all(|e| {
            e.start_line == 0 && e.start_col == 0 && e.end_line == 0 && e.end_col == 0
        })
}

fn apply_workspace_edit(
    ctx: &mut MuiContext,
    we: &crate::language::WorkspaceEdit,
    new_name: &str,
) -> WorkspaceEditApplyResult {
    let current = ctx.file_path.clone();
    let mut result = WorkspaceEditApplyResult {
        changed: 0,
        skipped_dirty: 0,
        skipped_missing: 0,
        first_skipped_dirty_message: None,
        first_skipped_missing_message: None,
    };
    for (uri, edits) in &we.files {
        if edits.is_empty() {
            continue;
        }
        let Some(fpath) = crate::nav::uri_to_path(uri) else {
            continue;
        };
        // Fill empty new_text (fallback edits) with new_name.
        let edits: Vec<crate::language::TextEdit> = edits
            .iter()
            .cloned()
            .map(|mut e| {
                if e.new_text.is_empty() {
                    e.new_text = new_name.to_string();
                }
                e
            })
            .collect();

        let is_current = current
            .as_deref()
            .map(|c| crate::nav::paths_equal(c, &fpath))
            .unwrap_or(false);

        if is_current {
            if ctx.tabs.any_dirty_path_except(&fpath, ctx.tabs.active()) {
                result.skipped_dirty += 1;
                if result.first_skipped_dirty_message.is_none() {
                    result.first_skipped_dirty_message =
                        Some(skipped_dirty_workspace_edit_message(&fpath));
                }
                println!(
                    "workspace edit: skipped active path with dirty duplicate path={}",
                    fpath.display()
                );
                continue;
            }
            // Apply to the active model in-place (preserves the live edit state),
            // then save it to disk.
            let edited_bytes = {
                let m = ctx.tabs.active_model_mut();
                let text = m.as_text();
                let cl = m.cursor_line() as i32;
                let cc = m.cursor_col() as i32;
                let edited = crate::language::apply_text_edits(&text, &edits);
                *m = crate::editor::TextModel::from_bytes(edited.as_bytes());
                m.move_to(cl, cc);
                m.to_bytes()
            };
            if let Some(p) = current.clone() {
                let resurrected_path = !p.is_file();
                if std::fs::write(&p, &edited_bytes).is_ok() {
                    mark_active_clean(ctx);
                    let active = ctx.tabs.active();
                    let _ = ctx
                        .tabs
                        .reload_all_clean_path_except(&p, &edited_bytes, active);
                    if resurrected_path {
                        record_recent_file(ctx, p.clone());
                        refresh_workspace_file_views(ctx);
                    }
                }
            }
            result.changed += 1;
        } else {
            // Other file: do not rewrite disk underneath an open dirty buffer.
            if ctx.tabs.any_dirty_path(&fpath) {
                result.skipped_dirty += 1;
                if result.first_skipped_dirty_message.is_none() {
                    result.first_skipped_dirty_message =
                        Some(skipped_dirty_workspace_edit_message(&fpath));
                }
                println!(
                    "workspace edit: skipped dirty non-active tab path={}",
                    fpath.display()
                );
                continue;
            }
            // Other clean file: read from disk, apply, write back; refresh an open tab.
            // Missing files are only valid for explicit create-style workspace
            // edits (all inserts at 0:0). Replacement edits against stale files
            // must not be applied to an empty fallback buffer.
            let disk = match std::fs::read(&fpath) {
                Ok(bytes) => bytes,
                Err(_) if workspace_edits_can_create_missing_file(&edits) => Vec::new(),
                Err(e) => {
                    result.skipped_missing += 1;
                    if result.first_skipped_missing_message.is_none() {
                        result.first_skipped_missing_message = Some(format!(
                            "Skipped missing file during workspace edit: {}: {}",
                            basename(&fpath),
                            e
                        ));
                    }
                    println!(
                        "workspace edit: skipped missing non-active path={} err={e}",
                        fpath.display()
                    );
                    refresh_workspace_file_views(ctx);
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&disk).into_owned();
            let edited = crate::language::apply_text_edits(&text, &edits);
            let resurrected_path = !fpath.is_file();
            if std::fs::write(&fpath, edited.as_bytes()).is_ok() {
                result.changed += 1;
                let _ = ctx.tabs.reload_all_clean_path(&fpath, edited.as_bytes());
                if resurrected_path {
                    record_recent_file(ctx, fpath.clone());
                    refresh_workspace_file_views(ctx);
                }
            }
        }
    }
    result
}

fn toast_codeaction_workspace_result(ctx: &mut MuiContext, result: &WorkspaceEditApplyResult) {
    if result.skipped_dirty > 0 {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            result
                .first_skipped_dirty_message
                .as_deref()
                .unwrap_or("Skipped dirty file during workspace edit"),
        );
    } else if result.skipped_missing > 0 {
        ctx.push_toast(
            crate::toast::Kind::Warn,
            result
                .first_skipped_missing_message
                .as_deref()
                .unwrap_or("Skipped missing file during workspace edit"),
        );
    } else if result.changed > 0 {
        ctx.push_toast(crate::toast::Kind::Success, "Applied code action");
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Code action produced no edit");
    }
}

/// Draw the rename inline input. No-op when inactive.
#[no_mangle]
pub extern "C" fn mui_rename_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.rename.is_active() {
        return;
    }
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let rename = std::mem::take(&mut ctx.rename);
    let old_clip = ctx.clip;
    ctx.clip = None;
    ctx.overlay = true;
    ctx.text.clear_overlay_runs();
    ctx.text.set_overlay(true);
    rename.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.clip = old_clip;
    ctx.rename = rename;
}

// ---- code actions / quick-fix (Ctrl+.) ----

/// Request code actions for the current line/selection. Fires
/// `textDocument/codeAction` for the cursor line range, parses the actions, and
/// (when `mty fix` is available) appends a synthetic "Fix all (mty)" action.
/// Returns the action count (0 leaves the menu closed).
#[no_mangle]
pub extern "C" fn mui_codeaction_request(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.codeaction.cancel();
    let actions = compute_line_actions(ctx, line, col);
    if actions.is_empty() {
        println!("codeaction: line={line} total=0");
        ctx.push_toast(
            crate::toast::Kind::Info,
            codeaction_not_found_message(ctx, line, col),
        );
        return 0;
    }
    let count = ctx.codeaction.set(actions);
    println!("codeaction: line={line} total={count}");
    count as i32
}

/// Compute the code actions available for `line` (0-based) at `col` WITHOUT
/// touching the menu state — the shared core of [`mui_codeaction_request`] and
/// the quick-fix lightbulb probe ([`crate::wsabi::mui_lightbulb_tick`]). Fires
/// `textDocument/codeAction` over the line's full range, parses the actions, and
/// (when `mty fix` is available + the LSP returned at least one action) appends
/// the synthetic "Fix all (mty)" action. Returns the actions in menu order.
pub(crate) fn compute_line_actions(
    ctx: &MuiContext,
    line: i32,
    col: i32,
) -> Vec<crate::language::CodeAction> {
    let Some(path) = ctx.file_path.clone() else {
        return Vec::new();
    };
    let (source, _, _) = active_source_and_cursor(ctx);
    let line0 = line.max(0) as u32;
    let line_len = source
        .split('\n')
        .nth(line0 as usize)
        .map(|l| l.chars().count() as u32)
        .unwrap_or(0);
    let end_col = line_len.max(col.max(0) as u32);
    let raw = if ctx.language == Language::Mighty {
        let diagnostics_json = code_action_diagnostics_json(&ctx.diags, line0);
        crate::language::lsp::request(
            &path,
            &source,
            crate::language::lsp::Req::CodeAction {
                start_line: line0,
                start_col: 0,
                end_line: line0,
                end_col,
                diagnostics_json: diagnostics_json.clone(),
            },
        )
    } else if let Some(spec) = crate::lspregistry::server_for(ctx.language) {
        let root = workspace_root(&path);
        let diagnostics_json =
            code_action_diagnostics_json_lsp_utf16(&source, &ctx.diags, line0);
        crate::lspclient::request(
            &spec,
            ctx.language.lsp_id(),
            &root,
            &path,
            &source,
            crate::lspclient::Method::CodeAction {
                end_line: line0,
                end_col,
                diagnostics_json,
            },
            line0,
            0,
        )
    } else {
        String::new()
    };
    let mut actions = crate::language::parse_code_actions(&raw);
    // Only offer "Fix all (mty)" when there's an actual fixable diagnostic on the
    // line (the LSP returned at least one action) — so the lightbulb never lights
    // every line just because the `mty fix` subcommand exists.
    if ctx.language == Language::Mighty && !actions.is_empty() && mty_fix_available() {
        actions.push(crate::language::CodeAction {
            title: "Fix all (mty)".to_string(),
            edit: None,
            command_edit: None,
            command: None,
            fix_all_mty: true,
        });
    }
    actions
}

/// `1` if `mty fix --help` succeeds (the fixer subcommand exists).
pub(crate) fn mty_fix_available() -> bool {
    let mty = mty_default();
    std::process::Command::new(&mty)
        .arg("fix")
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn mty_default() -> String {
    crate::mty::path()
}

fn mty_program_display(mty: &str) -> &str {
    std::path::Path::new(mty)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(mty)
}

fn fix_all_command_display(mty: &str) -> String {
    format!("{} fix --apply", mty_program_display(mty))
}

fn fix_all_failed_message(path: &std::path::Path, mty: &str) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    format!("Fix all (mty) failed: {name} via {}", fix_all_command_display(mty))
}

fn codeaction_presave_failed_message(path: &std::path::Path, e: &std::io::Error) -> String {
    let name = basename(path);
    let reason = e.to_string();
    if reason.trim().is_empty() {
        format!("Save failed before code action: {name}")
    } else {
        format!("Save failed before code action: {name}: {}", reason.trim())
    }
}

fn codeaction_needs_file_message(ctx: &MuiContext) -> String {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string());
    format!("Code action needs a file: {name}")
}

/// `1` while the code-action menu is active.
#[no_mangle]
pub extern "C" fn mui_codeaction_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.codeaction.is_active()))
}

/// Number of code actions in the menu.
#[no_mangle]
pub extern "C" fn mui_codeaction_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.codeaction.count() as i32)
}

/// 0-based selected action index.
#[no_mangle]
pub extern "C" fn mui_codeaction_sel(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.codeaction.selection() as i32)
}

/// `1` when applying the selected code action can attempt an edit/command.
/// Pure preflight: no toasts; `mui_codeaction_apply` reports missing files,
/// command failures, and no-edit outcomes.
#[no_mangle]
pub extern "C" fn mui_codeaction_can_apply(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() || ctx.file_path.is_none() {
        return 0;
    }
    let Some(action) = ctx.codeaction.selected() else {
        return 0;
    };
    let can_apply = action.fix_all_mty
        || action.edit.as_ref().is_some_and(|we| !we.is_empty())
        || action.command_edit.as_ref().is_some_and(|we| !we.is_empty())
        || action.command.is_some();
    i32::from(can_apply)
}

/// Move the code-action selection by `delta` (wraps).
#[no_mangle]
pub extern "C" fn mui_codeaction_move(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.codeaction.move_sel(delta);
    }
}

/// Select the code-action row under the last click. `row` is the screen row
/// used to draw the popup, matching [`mui_codeaction_draw`]. Returns the
/// selected action index, or `-1` for a miss.
#[no_mangle]
pub extern "C" fn mui_codeaction_click(handle: i64, row: i32, col: i32, total_lines: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if !ctx.codeaction.is_active() {
        return -1;
    }
    let region = layout::region(ctx.sidebar_visible);
    let cx = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let cy = layout::row_y_in(region, row);
    let (visible_w, visible_h) = visible_surface_size(ctx);
    ctx.codeaction.click_row_inset(
        &mut ctx.text,
        ctx.last_event.x,
        ctx.last_event.y,
        cx,
        cy,
        visible_w,
        visible_h,
        region.left + 8.0,
    )
}

/// Cancel/close the code-action menu.
#[no_mangle]
pub extern "C" fn mui_codeaction_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.codeaction.is_active() {
        ctx.codeaction.cancel();
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No code action menu open");
        0
    }
}

/// Apply the selected code action: apply its inline `WorkspaceEdit`, or run
/// `mty fix --apply` on the active file (the "Fix all (mty)" action) + reload.
/// Returns `1` if anything changed, `0` otherwise. Successful applies close the
/// menu; failed attempts leave it open so the user can choose another action or
/// retry after correcting the problem.
#[no_mangle]
pub extern "C" fn mui_codeaction_apply(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let selected = ctx.codeaction.selected().cloned();
    let Some(action) = selected else {
        ctx.push_toast(crate::toast::Kind::Info, "No code action selected");
        return 0;
    };

    if action.fix_all_mty {
        // Save the live buffer, run `mty fix --apply`, reload.
        let path = match ctx.file_path.clone() {
            Some(p) => p,
            None => {
                ctx.push_toast(crate::toast::Kind::Warn, codeaction_needs_file_message(ctx));
                return 0;
            }
        };
        if ctx.tabs.any_dirty_path_except(&path, ctx.tabs.active()) {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                skipped_dirty_workspace_edit_message(&path),
            );
            println!(
                "codeaction: fix-all skipped dirty duplicate path={}",
                path.display()
            );
            return 0;
        }
        let bytes = ctx.tabs.active_model().to_bytes();
        let resurrected_path = !path.is_file();
        match std::fs::write(&path, &bytes) {
            Ok(()) => {}
            Err(e) => {
                ctx.push_toast(
                    crate::toast::Kind::Error,
                    codeaction_presave_failed_message(&path, &e),
                );
                return 0;
            }
        }
        mark_active_clean(ctx);
        let active = ctx.tabs.active();
        let _ = ctx
            .tabs
            .reload_all_clean_path_except(&path, &bytes, active);
        if resurrected_path {
            record_recent_file(ctx, path.clone());
            refresh_workspace_file_views(ctx);
        }
        let mty = mty_default();
        let ok = std::process::Command::new(&mty)
            .arg("fix")
            .arg("--apply")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            if let Ok(reloaded) = std::fs::read(&path) {
                ctx.tabs.reload_active_preserving_history(&reloaded);
                let active = ctx.tabs.active();
                let _ = ctx
                    .tabs
                    .reload_all_clean_path_except(&path, &reloaded, active);
            }
            ctx.push_toast(crate::toast::Kind::Success, "Applied Fix all (mty)");
            ctx.codeaction.cancel();
        } else {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                fix_all_failed_message(&path, &mty),
            );
        }
        println!("codeaction: apply Fix-all-mty ok={ok}");
        return i32::from(ok);
    }

    // Inline-edit action.
    if let Some(we) = &action.edit {
        let we = we.clone();
        let result = apply_workspace_edit(ctx, &we, "");
        println!("codeaction: apply edit files={}", result.changed);
        toast_codeaction_workspace_result(ctx, &result);
        if result.changed > 0 {
            ctx.codeaction.cancel();
        }
        return i32::from(result.changed > 0);
    }
    if let Some(we) = &action.command_edit {
        let we = we.clone();
        let result = apply_workspace_edit(ctx, &we, "");
        println!("codeaction: apply command-edit files={}", result.changed);
        toast_codeaction_workspace_result(ctx, &result);
        if result.changed > 0 {
            ctx.codeaction.cancel();
        }
        return i32::from(result.changed > 0);
    }
    if let Some(command) = &action.command {
        let Some(path) = ctx.file_path.clone() else {
            println!("codeaction: execute command={} no active path", command.command);
            ctx.push_toast(crate::toast::Kind::Warn, codeaction_needs_file_message(ctx));
            return 0;
        };
        let (source, _, _) = active_source_and_cursor(ctx);
        let raw = lsp_execute_command_raw(ctx.language, &path, &source, command);
        let we = crate::language::parse_workspace_edit(&raw);
        if !we.is_empty() {
            let result = apply_workspace_edit(ctx, &we, "");
            println!(
                "codeaction: execute command={} files={}",
                command.command, result.changed
            );
            toast_codeaction_workspace_result(ctx, &result);
            if result.changed > 0 {
                ctx.codeaction.cancel();
            }
            return i32::from(result.changed > 0);
        }
        println!("codeaction: execute command={} no-edit", command.command);
        ctx.push_toast(crate::toast::Kind::Info, "Code action produced no edit");
        return 0;
    }
    println!("codeaction: apply (command/no-edit) — no-op");
    ctx.push_toast(crate::toast::Kind::Info, "Code action produced no edit");
    0
}

/// The title of code action `i` as a staged string Mighty reads char-by-char:
/// store it, then call `mui_codeaction_title_len` / `_char`. We stage into the
/// existing `text_stage` buffer to avoid adding another scalar string channel.
#[no_mangle]
pub extern "C" fn mui_codeaction_title_stage(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.text_stage.clear();
    if let Some(t) = ctx.codeaction.title(i.max(0) as usize) {
        ctx.text_stage.push_str(t);
        ctx.text_stage.chars().count() as i32
    } else {
        0
    }
}

/// Length (chars) of the staged code-action title.
#[no_mangle]
pub extern "C" fn mui_codeaction_title_len(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.text_stage.chars().count() as i32)
}

/// The `i`th char (codepoint) of the staged code-action title, or `-1`.
#[no_mangle]
pub extern "C" fn mui_codeaction_title_char(handle: i64, i: i32) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.text_stage
            .chars()
            .nth(i.max(0) as usize)
            .map(|ch| ch as i32)
            .unwrap_or(-1)
    })
}

/// Draw the code-action menu near the cursor `(row, col)`. No-op when inactive.
#[no_mangle]
pub extern "C" fn mui_codeaction_draw(handle: i64, row: i32, col: i32, total_lines: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.codeaction.is_active() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let x = layout::text_x_in(region, total_lines.max(1) as u64, col);
    let y = layout::row_y_in(region, row);
    let (w, h) = visible_surface_size(ctx);
    let menu = std::mem::take(&mut ctx.codeaction);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    menu.draw_inset(ctx, x, y, w, h, region.left + 8.0);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.codeaction = menu;
}

// ---------------------------------------------------------------------------
// Feature A — undo / redo (shim-owned history; logic in history.rs)
// ---------------------------------------------------------------------------
//
// The undo/redo history lives shim-side to avoid Mighty managing nested undo
// Vecs (L21). Recording scheme (see history.rs): Mighty streams its FULL
// post-edit buffer after each edit-group via `mui_undo_record_begin` +
// `_byte` + `_commit(cur_line, cur_col)`; the shim diffs against the current top
// and either coalesces a single-char typing run into it or pushes a fresh
// snapshot. `mui_undo_break` marks a typing-run boundary (cursor move, newline,
// delete, save, format, find-jump, tab switch) so one Ctrl+Z undoes a contiguous
// typing run rather than the whole file or one char at a time.
//
// On load / tab switch Mighty calls `mui_undo_seed_*` to install the freshly
// loaded buffer as the per-buffer baseline (history is per active buffer).

/// Begin seeding the baseline buffer (clears history + staging). Mighty streams
/// the freshly loaded buffer, then commits with `mui_undo_seed_commit`.
#[no_mangle]
pub extern "C" fn mui_undo_seed_begin(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_begin();
    }
}

/// Append one byte to the baseline-seed staging buffer.
#[no_mangle]
pub extern "C" fn mui_undo_seed_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_byte((byte & 0xff) as u8);
    }
}

/// Install the staged buffer as the history baseline at cursor `(line, col)`
/// (0-based), clearing all prior undo/redo. Called on load / tab switch.
#[no_mangle]
pub extern "C" fn mui_undo_seed_commit(handle: i64, cur_line: i32, cur_col: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        // `record_begin/byte` staged into the same buffer `seed` consumes via
        // `record_commit`; reuse it by taking the staged bytes through a record
        // path. To keep `seed`'s clear-then-baseline semantics, drain staging here.
        ctx.history.seed_from_staging(cur_line, cur_col);
    }
}

/// Mark a typing-run boundary: the next record starts a fresh undo step rather
/// than coalescing. Mighty calls this on any non-insert action.
#[no_mangle]
pub extern "C" fn mui_undo_break(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.break_run();
    }
}

/// Begin streaming a post-edit buffer for a history record (clears staging).
#[no_mangle]
pub extern "C" fn mui_undo_record_begin(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_begin();
    }
}

/// Append one byte to the record staging buffer.
#[no_mangle]
pub extern "C" fn mui_undo_record_byte(handle: i64, byte: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.history.record_byte((byte & 0xff) as u8);
    }
}

/// Commit the staged post-edit buffer as a history record at cursor `(line,
/// col)` (0-based). Coalesces a typing run into the current step or pushes a new
/// one. Returns `1` if a snapshot was recorded/coalesced, `0` if it was a no-op
/// (no byte change).
#[no_mangle]
pub extern "C" fn mui_undo_record_commit(handle: i64, cur_line: i32, cur_col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.history.record_commit(cur_line, cur_col))
}

/// Undo one step. On success the restored buffer becomes the shim's load buffer
/// (so Mighty pulls it via `mui_load_byte`) and the restored cursor is readable
/// via `mui_undo_cursor_line` / `_col`. Returns the restored buffer's byte count,
/// or `-1` if there is nothing to undo.
#[no_mangle]
pub extern "C" fn mui_undo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.history.undo() {
        Some(snap) => {
            let n = snap.bytes.len() as i32;
            ctx.load_buf = snap.bytes;
            ctx.restored_cursor = (snap.cursor_line, snap.cursor_col);
            println!("undo: restored {n} bytes, cursor=({},{})", snap.cursor_line, snap.cursor_col);
            n
        }
        None => {
            println!("undo: nothing to undo");
            -1
        }
    }
}

/// Redo one step (mirror of [`mui_undo`]). Returns the restored buffer's byte
/// count, or `-1` if there is nothing to redo.
#[no_mangle]
pub extern "C" fn mui_redo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    match ctx.history.redo() {
        Some(snap) => {
            let n = snap.bytes.len() as i32;
            ctx.load_buf = snap.bytes;
            ctx.restored_cursor = (snap.cursor_line, snap.cursor_col);
            println!("redo: restored {n} bytes, cursor=({},{})", snap.cursor_line, snap.cursor_col);
            n
        }
        None => {
            println!("redo: nothing to redo");
            -1
        }
    }
}

/// 0-based cursor line restored by the last `mui_undo` / `mui_redo`.
#[no_mangle]
pub extern "C" fn mui_undo_cursor_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.restored_cursor.0)
}

/// 0-based cursor column restored by the last `mui_undo` / `mui_redo`.
#[no_mangle]
pub extern "C" fn mui_undo_cursor_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.restored_cursor.1)
}

/// Undo steps currently available (states behind the current one).
#[no_mangle]
pub extern "C" fn mui_undo_depth(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.history.undo_depth() as i32)
}

/// Redo steps currently available.
#[no_mangle]
pub extern "C" fn mui_redo_depth(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.history.redo_depth() as i32)
}

// ---------------------------------------------------------------------------
// Feature B — format document (`mty fmt`; logic in format.rs)
// ---------------------------------------------------------------------------

/// Pure preflight for Format Document. Returns `1` only when the active tab is
/// file-backed, editable, and safe to hand to `mty fmt`; emits no feedback.
#[no_mangle]
pub extern "C" fn mui_format_can_current(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let Some(path) = ctx.file_path.as_deref() else {
        return 0;
    };
    i32::from(crate::format::is_mty_path(path) && !ctx.tabs.any_dirty_path(path))
}

/// Format the currently-configured file in place via `mty fmt <path>`. The
/// Mighty side saves the live buffer to disk FIRST (so the formatter sees the
/// current text), then calls this, then reloads the formatted file (only when
/// this returns `1`).
///
/// Return codes are DISTINCT so the editor can pick the right status message
/// without corrupting data:
///   * `1` — formatted (a `.mty` file, `mty fmt` succeeded) → reload.
///   * `0` — not applicable (the active file is NOT `.mty`) → no-op; the editor
///     shows "format: only .mty supported". This is the L26 guard: `mty fmt`
///     truncates non-`.mty` input to 1 byte, so we never spawn it.
///   * `-1` — failed (a `.mty` file but `mty fmt` errored / exited non-zero).
///
/// `mty fmt` formats in place (confirmed via `mty fmt --help`), so no extra
/// flags are needed.
fn format_command_display() -> String {
    let mty = crate::mty::path();
    let program = std::path::Path::new(&mty)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(mty.as_str());
    format!("{program} fmt")
}

fn format_failed_message(path: &std::path::Path, reason: Option<&str>) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let base = format!("Format failed: {name} via {}", format_command_display());
    match reason.map(str::trim).filter(|s| !s.is_empty()) {
        Some(reason) => format!("{base}: {reason}"),
        None => base,
    }
}

fn format_needs_file_message(ctx: &MuiContext) -> String {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "(scratch)".to_string());
    format!("Save {name} before formatting")
}

fn format_dirty_target_message(path: &std::path::Path) -> String {
    let name = basename(path);
    format!("Save or discard changes in {name} before formatting")
}

#[no_mangle]
pub extern "C" fn mui_format_current(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.file_path.clone() else {
        eprintln!("format: no file path configured");
        ctx.push_toast(crate::toast::Kind::Warn, format_needs_file_message(ctx));
        return -1;
    };
    if ctx.tabs.any_dirty_path(&path) {
        println!("format: {} -> skipped dirty open tab", path.display());
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format_dirty_target_message(&path),
        );
        return -1;
    }
    let resurrected_path = !path.is_file();
    match crate::format::run_fmt(&path) {
        crate::format::FmtOutcome::Formatted => {
            if resurrected_path && path.is_file() {
                record_recent_file(ctx, path.clone());
                refresh_workspace_file_views(ctx);
            }
            println!("format: {} -> ok", path.display());
            ctx.push_toast(crate::toast::Kind::Success, "Formatted document");
            1
        }
        crate::format::FmtOutcome::NotApplicable => {
            println!("format: {} -> skipped (only .mty supported)", path.display());
            ctx.push_toast(crate::toast::Kind::Info, "Format is available for Mighty files");
            0
        }
        crate::format::FmtOutcome::Failed(reason) => {
            println!("format: {} -> failed", path.display());
            ctx.push_toast(
                crate::toast::Kind::Error,
                format_failed_message(&path, Some(&reason)),
            );
            -1
        }
    }
}

/// Launch-test hook: with `MUI_HISTORY_PROBE` set, run a scripted edit -> undo
/// -> redo and a format over the active tab's buffer so a headless run proves
/// the undo/redo + format wiring (Ctrl+Z / Ctrl+Y / the format chord can't be
/// delivered non-interactively). Logs buffer lengths at each step. No effect
/// unless the env var is set.
#[no_mangle]
pub extern "C" fn mui_history_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if std::env::var_os("MUI_HISTORY_PROBE").is_none() {
        return;
    }
    // Build the active buffer bytes from the tab store.
    let active = ctx.tabs.active();
    let mut buf: Vec<u8> = Vec::new();
    let n = ctx.tabs.load_len(active);
    if n > 0 {
        for i in 0..(n as usize) {
            let b = ctx.tabs.load_byte(active, i);
            if (0..=255).contains(&b) {
                buf.push(b as u8);
            }
        }
    }
    let base_len = buf.len();

    // Seed the baseline (mirrors the Mighty load path).
    ctx.history.record_begin();
    for b in &buf {
        ctx.history.record_byte(*b);
    }
    ctx.history.seed_from_staging(0, 0);
    println!("history-probe: seed len={base_len} undo_depth={}", ctx.history.undo_depth());

    // Simulate typing two chars (a coalescing run) at EOF, recording after each.
    let mut edited = buf.clone();
    edited.push(b'/');
    ctx.history.break_run(); // first char after seed starts a fresh step
    ctx.history.record(edited.clone(), 0, edited.len() as i32);
    edited.push(b'/');
    ctx.history.record(edited.clone(), 0, edited.len() as i32);
    println!(
        "history-probe: after typing len={} undo_depth={}",
        edited.len(),
        ctx.history.undo_depth()
    );

    // Undo -> should return to the baseline length in one step (typing coalesced).
    match ctx.history.undo() {
        Some(s) => println!("history-probe: undo -> len={} (expect {base_len})", s.bytes.len()),
        None => println!("history-probe: undo -> nothing"),
    }
    // Redo -> back to the edited length.
    match ctx.history.redo() {
        Some(s) => println!("history-probe: redo -> len={} (expect {})", s.bytes.len(), edited.len()),
        None => println!("history-probe: redo -> nothing"),
    }

    // Format the on-disk active file (if any), logging the before/after lengths.
    if let Some(path) = ctx.file_path.clone() {
        let before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let outcome = crate::format::run_fmt(&path);
        let after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        println!("history-probe: format outcome={outcome:?} on-disk {before} -> {after} bytes");
    } else {
        println!("history-probe: format skipped (no file_path)");
    }
}

// ---------------------------------------------------------------------------
// Authoritative editor text model (shim-side; L28 workaround)
// ---------------------------------------------------------------------------
//
// Live editing under v0.36 native `mty build` was impossible: the Mighty
// `Vec[I32]` edit buffer comes back EMPTY (L28 codegen bug). So the editable
// buffer + cursor now live shim-side in the active tab's `TextModel`
// (`editor.rs`), and Mighty drives edits through these scalar ops. Editing is
// genuinely LIVE: `mui_ed_draw` renders directly from this mutated model each
// frame. Move the model back to Mighty once the codegen bug is fixed.

use crate::editor::TextModel;

/// The active tab's editable model (mutable). `None` on a null handle.
#[inline]
unsafe fn model_mut<'a>(handle: i64) -> Option<&'a mut TextModel> {
    ctx(handle).and_then(|c| {
        if c.tabs.active_read_only() {
            None
        } else {
            Some(c.tabs.active_model_mut())
        }
    })
}

fn reject_read_only_edit(ctx: &mut MuiContext) -> i32 {
    ctx.push_toast(crate::toast::Kind::Warn, "Edit is unavailable in read-only previews");
    0
}

fn apply_model_edit(handle: i64, edit: impl FnOnce(&mut TextModel)) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return reject_read_only_edit(ctx);
    }
    let before = ctx.tabs.active_model().as_text();
    edit(ctx.tabs.active_model_mut());
    i32::from(ctx.tabs.active_model().as_text() != before)
}

/// `1` when the active editor model can be edited.
/// Pure preflight: no toasts; stateful edit ABIs keep read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_edit(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(!ctx.tabs.active_read_only())
}

/// Owned snapshot of the model fields [`mui_ed_draw`] needs, taken so the borrow
/// on the model ends before the rect/text draw calls borrow the context again.
struct EdDrawSnapshot {
    total: usize,
    first: usize,
    cur_line: usize,
    cur_col: usize,
    sel: Option<((usize, usize), (usize, usize))>,
    /// The lines actually painted, in screen order: `(source_line, text)`. With
    /// folding active this SKIPS lines hidden inside a folded region, so the
    /// screen row of `lines_for_view[k]` is simply `k`.
    lines_for_view: Vec<(usize, String)>,
    /// Every caret's `(line, col)` (caret[0] = primary), for multi-cursor draw.
    carets: Vec<(usize, usize)>,
    /// Every caret's selection range (for multi-cursor selection highlights).
    selections: Vec<((usize, usize), (usize, usize))>,
    /// For each painted line, whether it STARTS a foldable region and (if so)
    /// whether it is currently folded + how many lines it hides. Drives the
    /// gutter chevron + the "⋯ N lines" indicator. Keyed by source line.
    fold_marks: std::collections::HashMap<usize, FoldMark>,
}

/// A painted line's fold decoration: it starts a region, is/ isn't folded, and
/// (when folded) hides `hidden` inner lines.
#[derive(Clone, Copy)]
struct FoldMark {
    folded: bool,
    hidden: usize,
}

/// Insert one Unicode scalar at the cursor (a `\n` codepoint splits the line).
#[no_mangle]
pub extern "C" fn mui_ed_insert_char(handle: i64, cp: i32) -> i32 {
    if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
        return apply_model_edit(handle, |m| m.insert_char(ch));
    }
    0
}

/// Delete the char before the cursor (joining lines at column 0).
#[no_mangle]
pub extern "C" fn mui_ed_backspace(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.backspace())
}

/// Delete the char at the cursor (joining the next line at end of line).
#[no_mangle]
pub extern "C" fn mui_ed_delete(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.delete())
}

/// Delete from the cursor back to the previous word boundary.
#[no_mangle]
pub extern "C" fn mui_ed_delete_word_left(handle: i64) -> i32 {
    apply_model_edit(handle, |m| {
        let _ = m.delete_word_left();
    })
}

/// Delete from the cursor forward to the next word boundary.
#[no_mangle]
pub extern "C" fn mui_ed_delete_word_right(handle: i64) -> i32 {
    apply_model_edit(handle, |m| {
        let _ = m.delete_word_right();
    })
}

fn cloned_model_edit_would_change(ctx: &MuiContext, edit: impl FnOnce(&mut TextModel)) -> i32 {
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let model = ctx.tabs.active_model();
    let before = model.as_text();
    let mut probe = model.clone();
    edit(&mut probe);
    i32::from(probe.as_text() != before)
}

/// `1` when Backspace can mutate the active model.
/// Pure preflight: no toasts; Backspace keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_backspace(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    cloned_model_edit_would_change(ctx, |m| m.backspace_multi())
}

/// `1` when Delete can mutate the active model.
/// Pure preflight: no toasts; Delete keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_delete(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    cloned_model_edit_would_change(ctx, |m| m.delete_multi())
}

/// `1` when Ctrl+Backspace can mutate the active model.
/// Pure preflight: no toasts; the delete command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_delete_word_left(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    cloned_model_edit_would_change(ctx, |m| m.delete_word_left_multi())
}

/// `1` when Ctrl+Delete can mutate the active model.
/// Pure preflight: no toasts; the delete command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_delete_word_right(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    cloned_model_edit_would_change(ctx, |m| m.delete_word_right_multi())
}

/// Delete the current logical line from the active editor model.
#[no_mangle]
pub extern "C" fn mui_ed_delete_current_line(handle: i64) -> i32 {
    apply_model_edit(handle, |m| {
        let _ = m.delete_current_line();
    })
}

/// `1` when Delete Line can mutate the active model.
/// Pure preflight: no toasts; the delete command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_delete_current_line(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let model = ctx.tabs.active_model();
    i32::from(model.line_count() > 1 || !model.line(0).is_empty())
}

/// Join the current line with the following line.
#[no_mangle]
pub extern "C" fn mui_ed_join_line(handle: i64) -> i32 {
    apply_model_edit(handle, |m| {
        let _ = m.join_line();
    })
}

/// `1` when Join Line can merge the current line with a following line.
/// Pure preflight: no toasts; the join command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_join_line(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let model = ctx.tabs.active_model();
    i32::from(model.cursor_line() + 1 < model.line_count())
}

/// Insert a newline at the cursor.
#[no_mangle]
pub extern "C" fn mui_ed_newline(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.newline())
}

/// Move the cursor one step in `dir` (0=L 1=R 2=Up 3=Down 4=Home 5=End).
#[no_mangle]
pub extern "C" fn mui_ed_move(handle: i64, dir: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_cursor(dir);
    }
}

/// Move the cursor to an explicit 0-based `(line, col)`, clamped.
#[no_mangle]
pub extern "C" fn mui_ed_move_to(handle: i64, line: i32, col: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_to(line, col);
    }
}

/// 0-based cursor line of the active model.
#[no_mangle]
pub extern "C" fn mui_ed_cursor_line(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().cursor_line() as i32)
}

/// 0-based cursor column of the active model.
#[no_mangle]
pub extern "C" fn mui_ed_cursor_col(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().cursor_col() as i32)
}

/// Number of lines in the active model (>= 1).
#[no_mangle]
pub extern "C" fn mui_ed_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| c.tabs.active_model().line_count() as i32)
}

/// Char length of line `line` (0-based) in the active model.
#[no_mangle]
pub extern "C" fn mui_ed_line_len(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().line_len(line as usize) as i32)
}

/// Set the top visible line (scroll offset) of the active model, clamped.
#[no_mangle]
pub extern "C" fn mui_ed_set_scroll(handle: i64, first: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.set_first_visible(first.max(0) as usize);
    }
}

/// The active model's top visible line (scroll offset).
#[no_mangle]
pub extern "C" fn mui_ed_first_visible(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().first_visible() as i32)
}

// ===========================================================================
// Code folding (per-tab; see `crate::fold`)
// ===========================================================================
//
// The fold state (foldable ranges + folded headers + the visible↔source line
// mapping) lives per-tab in the TabStore (L17/L21/L28: pure + shim-owned). The
// editor body draw ([`draw_editor_pane`]) consults it to skip folded lines and
// draw the gutter chevrons + the "⋯ N lines" indicator; the cursor/click paths
// use the visible↔source mapping. These scalar ops drive it from the Mighty
// side (which can't hold a Vec) and are exercised by the unit tests in
// `crate::fold`.

/// Recompute the active tab's foldable ranges from its current buffer (call
/// after edits / load). Folded headers that still open a region are preserved.
/// Returns the number of foldable regions.
#[no_mangle]
pub extern "C" fn mui_fold_recompute(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.tabs.recompute_active_fold();
    ctx.tabs.active_fold().ranges().len() as i32
}

/// Toggle the fold of the region whose HEADER is `line` (0-based). No-op if
/// `line` starts no foldable region. Returns `1` if a region was toggled.
#[no_mangle]
pub extern "C" fn mui_fold_toggle_at(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return 0;
    }
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.tabs.active_fold_mut().toggle(line as usize))
}

/// Toggle the fold of the INNERMOST region containing `line` (so "fold at the
/// cursor" works from a body line, not just the header). Returns the folded
/// header line (0-based), or `-1` if no region encloses `line`.
#[no_mangle]
pub extern "C" fn mui_fold_toggle_at_cursor(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return -1;
    }
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    ctx.tabs
        .active_fold_mut()
        .toggle_at_cursor(line as usize)
        .map(|h| h as i32)
        .unwrap_or(-1)
}

/// Fold EVERY foldable region in the active tab. Returns the folded region count.
#[no_mangle]
pub extern "C" fn mui_fold_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let f = ctx.tabs.active_fold_mut();
    f.fold_all();
    f.ranges().len() as i32
}

/// Unfold every region in the active tab.
#[no_mangle]
pub extern "C" fn mui_unfold_all(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.tabs.active_fold_mut().unfold_all();
    }
}

/// `1` if the region whose header is `line` is currently folded, else `0`.
#[no_mangle]
pub extern "C" fn mui_fold_is_folded(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return 0;
    }
    unsafe { ctx(handle) }
        .map_or(0, |c| i32::from(c.tabs.active_fold().is_folded(line as usize)))
}

/// `1` if `line` STARTS a foldable region (a gutter chevron is drawn there).
#[no_mangle]
pub extern "C" fn mui_fold_is_foldable(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return 0;
    }
    unsafe { ctx(handle) }
        .map_or(0, |c| i32::from(c.tabs.active_fold().is_foldable_start(line as usize)))
}

/// The END line (0-based) of the foldable region whose header is `line`, or
/// `-1` if `line` starts no region. (`mui_fold_region_at`.)
#[no_mangle]
pub extern "C" fn mui_fold_region_at(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.tabs
            .active_fold()
            .region_at(line as usize)
            .map(|r| r.end as i32)
            .unwrap_or(-1)
    })
}

/// The number of VISIBLE lines (buffer total minus lines hidden by folds).
#[no_mangle]
pub extern "C" fn mui_fold_visible_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        let total = c.tabs.active_model().line_count();
        c.tabs.active_fold().visible_count(total) as i32
    })
}

/// Map a 0-based VISIBLE row to the buffer line it shows (skipping folded
/// lines). Clamps past-the-end to the last line.
#[no_mangle]
pub extern "C" fn mui_fold_visible_to_source(handle: i64, row: i32) -> i32 {
    if row < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(row, |c| {
        let total = c.tabs.active_model().line_count();
        c.tabs.active_fold().visible_to_source(row as usize, total) as i32
    })
}

/// Map a buffer `line` to its VISIBLE row index. A hidden line maps to the row
/// of its enclosing (visible) fold header.
#[no_mangle]
pub extern "C" fn mui_fold_source_to_visible(handle: i64, line: i32) -> i32 {
    if line < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(line, |c| {
        let total = c.tabs.active_model().line_count();
        c.tabs.active_fold().source_to_visible(line as usize, total) as i32
    })
}

/// Dispatch a code-folding palette command (`CMD_FOLD_TOGGLE` / `CMD_FOLD_ALL`
/// / `CMD_UNFOLD_ALL`) — the single Mighty palette/quick-open arm-range routes
/// here so the ladder gains ONE arm, not three (L37/L38). Toggle acts on the
/// region enclosing the CURSOR line. Returns `1` when handled, `0` otherwise.
#[no_mangle]
pub extern "C" fn mui_fold_dispatch(handle: i64, cmd_id: i32) -> i32 {
    use crate::palette::*;
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let id = cmd_id as u32;
    if id == CMD_FOLD_TOGGLE {
        let line = ctx.tabs.active_model().cursor_line();
        if ctx.tabs.active_fold_mut().toggle_at_cursor(line).is_some() {
            1
        } else {
            ctx.push_toast(crate::toast::Kind::Info, "No foldable block at cursor");
            0
        }
    } else if id == CMD_FOLD_ALL {
        let fold = ctx.tabs.active_fold();
        if fold.ranges().is_empty() {
            ctx.push_toast(crate::toast::Kind::Info, "No foldable blocks");
            return 0;
        }
        if fold.ranges().iter().all(|r| fold.is_folded(r.start)) {
            ctx.push_toast(crate::toast::Kind::Info, "All foldable blocks already folded");
            return 0;
        }
        ctx.tabs.active_fold_mut().fold_all();
        1
    } else if id == CMD_UNFOLD_ALL {
        let fold = ctx.tabs.active_fold();
        if fold.ranges().is_empty() {
            ctx.push_toast(crate::toast::Kind::Info, "No foldable blocks");
            return 0;
        }
        if !fold.ranges().iter().any(|r| fold.is_folded(r.start)) {
            ctx.push_toast(crate::toast::Kind::Info, "No folded blocks to unfold");
            return 0;
        }
        ctx.tabs.active_fold_mut().unfold_all();
        1
    } else {
        0
    }
}

/// `1` if the active model has unsaved edits, else `0`.
#[no_mangle]
pub extern "C" fn mui_ed_dirty(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.tabs.is_dirty(c.tabs.active())))
}

/// Mark the active model clean (after a load) or dirty.
#[no_mangle]
pub extern "C" fn mui_ed_set_dirty(handle: i64, dirty: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if ctx.tabs.active_read_only() {
            return;
        }
        let dirty = dirty != 0;
        ctx.tabs.active_model_mut().set_dirty(dirty);
        let active = ctx.tabs.active();
        ctx.tabs.set_dirty(active, dirty);
        if !dirty {
            ctx.pending_dirty_close = None;
            ctx.pending_quit = None;
        }
    }
}

/// Load the active tab's file from disk into the active model (replacing it),
/// resetting the cursor to the top. Returns the byte length, or `-1` on error.
#[no_mangle]
pub extern "C" fn mui_ed_load(handle: i64) -> i64 {
    mui_ed_load_impl(handle, false)
}

/// Load the active tab's file from disk while keeping undo checkpoints. This is
/// for in-place editor transformations such as Format Document, where reload is
/// the post-edit state and the pre-edit checkpoint must stay undoable.
#[no_mangle]
pub extern "C" fn mui_ed_load_preserving_undo(handle: i64) -> i64 {
    mui_ed_load_impl(handle, true)
}

fn mui_ed_load_impl(handle: i64, preserve_undo: bool) -> i64 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // Edit-probe screenshot mode: preserve the scripted-edit model so a headless
    // capture shows the LIVE-edited buffer rather than the on-disk file.
    if ctx.edit_probe_lock {
        return ctx.tabs.active_model().to_bytes().len() as i64;
    }
    let Some(path) = ctx.tabs.active_path() else {
        // No file (scratch tab): keep the empty model.
        if preserve_undo {
            ctx.tabs.reload_active_preserving_history(b"");
        } else {
            ctx.tabs.reload_active(b"");
        }
        return 0;
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let n = bytes.len() as i64;
            if preserve_undo {
                ctx.tabs.reload_active_preserving_history(&bytes);
            } else {
                ctx.tabs.reload_active(&bytes);
            }
            let active = ctx.tabs.active();
            let _ = ctx
                .tabs
                .reload_all_clean_path_except(&path, &bytes, active);
            println!("mui_ed_load: {} ({} bytes)", path.display(), n);
            n
        }
        Err(e) => {
            eprintln!("mui_ed_load({}): {e}", path.display());
            refresh_workspace_file_views(ctx);
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Load", &path, &e),
            );
            -1
        }
    }
}

/// Compute the bytes to write for the active tab, applying the enabled on-save
/// transforms (trim trailing whitespace / ensure final newline) and updating the
/// in-memory model so the buffer matches disk (cursor preserved). Returns the
/// exact bytes that should be written.
fn mark_active_clean(ctx: &mut MuiContext) {
    let active = ctx.tabs.active();
    let bytes = ctx.tabs.active_model().to_bytes();
    if let Some(tab) = ctx.tabs.get_mut(active) {
        tab.bytes = bytes;
        tab.model.mark_clean();
        tab.dirty = false;
    }
    ctx.pending_dirty_close = None;
    ctx.pending_quit = None;
}

fn refresh_active_dirty_from_saved(ctx: &mut MuiContext) {
    let active = ctx.tabs.active();
    if let Some(tab) = ctx.tabs.get_mut(active) {
        let clean = tab.model.to_bytes() == tab.bytes;
        if clean {
            tab.model.mark_clean();
        }
        tab.dirty = !clean && !tab.read_only;
    }
}

fn reject_read_only_save(ctx: &mut MuiContext) -> i32 {
    let name = ctx
        .tabs
        .active_path()
        .as_deref()
        .map(basename)
        .unwrap_or_else(|| "binary file".to_string());
    ctx.autosave.disarm();
    ctx.push_toast(
        crate::toast::Kind::Warn,
        format!("{name} is read-only in the text editor"),
    );
    -1
}

fn save_bytes_for_active(ctx: &mut MuiContext) -> Vec<u8> {
    let trim = crate::settings::trim_ws();
    let final_nl = crate::settings::final_newline();
    let text = ctx.tabs.active_model().as_text();
    let out = crate::savefmt::apply(&text, trim, final_nl);
    if (trim || final_nl) && out != text {
        // Reflect the transform back into the live buffer (keeps the cursor) so
        // the trimmed whitespace doesn't reappear as an unsaved edit.
        ctx.tabs
            .active_model_mut()
            .set_text_preserving_cursor(&out);
    }
    out.into_bytes()
}

fn save_bytes_for_tab(tab: &mut crate::tabs::Tab) -> Vec<u8> {
    let trim = crate::settings::trim_ws();
    let final_nl = crate::settings::final_newline();
    let text = tab.model.as_text();
    let out = crate::savefmt::apply(&text, trim, final_nl);
    if (trim || final_nl) && out != text {
        tab.model.set_text_preserving_cursor(&out);
    }
    out.into_bytes()
}

/// A cheap content signature of the active buffer (FNV-1a over the bytes) used to
/// detect edits between auto-save ticks without per-op instrumentation.
fn autosave_signature(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Per-frame auto-save tick: when the `autosave` pref is ON and the active tab is
/// a real file-backed, dirty tab whose edit-idle window has elapsed, save it
/// (applying the same on-save transforms). Returns `1` if a save fired this
/// frame, else `0`. Safe on read-only/diff/preview/welcome/scratch states: those
/// have no file path, so `active_path()` is `None` and nothing is written.
#[no_mangle]
pub extern "C" fn mui_autosave_tick(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        ctx.autosave.disarm();
        ctx.autosave_sig = None;
        return 0;
    }
    if !crate::settings::autosave() {
        // Keep the clock disarmed so toggling autosave on doesn't immediately
        // fire on a stale timestamp. Forget the signature so the next enabled
        // tick re-baselines instead of treating accrued edits as one big change.
        ctx.autosave.disarm();
        ctx.autosave_sig = None;
        return 0;
    }
    // Detect edits by a cheap content signature of the active buffer; a change
    // (re)arms the debounce window. This avoids instrumenting every edit op while
    // still giving per-edit-idle debouncing.
    let sig = autosave_signature(&ctx.tabs.active_model().as_text());
    match ctx.autosave_sig {
        Some(prev) if prev == sig => {}
        _ => {
            // First observation or a real change since the last tick.
            if ctx.autosave_sig.is_some() {
                ctx.autosave.touch();
            }
            ctx.autosave_sig = Some(sig);
        }
    }
    // Only auto-save a real, file-backed, dirty tab.
    if !ctx.tabs.is_dirty(ctx.tabs.active()) {
        ctx.autosave.disarm();
        return 0;
    }
    let Some(path) = ctx.tabs.active_path() else {
        return 0;
    };
    if !ctx.autosave.due() {
        return 0;
    }
    if ctx.tabs.any_dirty_path_except(&path, ctx.tabs.active()) {
        println!(
            "mui_autosave: skipped dirty duplicate path={}",
            path.display()
        );
        return 0;
    }
    let bytes = save_bytes_for_active(ctx);
    let name = basename(&path);
    let resurrected_path = !path.is_file();
    trace(&format!("save path={} bytes={}", path.display(), bytes.len()));
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            mark_active_clean(ctx);
            let active = ctx.tabs.active();
            let _ = ctx
                .tabs
                .reload_all_clean_path_except(&path, &bytes, active);
            // Re-baseline the signature to the (possibly transformed) saved text
            // so the next tick doesn't see the transform as a fresh edit.
            ctx.autosave_sig = Some(autosave_signature(&ctx.tabs.active_model().as_text()));
            if resurrected_path {
                record_recent_file(ctx, path.clone());
            }
            refresh_workspace_file_views(ctx);
            println!("mui_autosave: {} ({} bytes)", path.display(), bytes.len());
            ctx.push_toast(crate::toast::Kind::Info, format!("Auto-saved {name}"));
            1
        }
        Err(e) => {
            eprintln!("mui_autosave({}): {e}", path.display());
            0
        }
    }
}

/// Record an edit for the auto-save debounce clock. The IDE calls this whenever
/// the active buffer changes (keystroke / paste / delete) so the idle window
/// restarts; auto-save fires ~1.2s after the last edit (see [`mui_autosave_tick`]).
#[no_mangle]
pub extern "C" fn mui_autosave_touch(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.autosave.touch();
    }
}

fn save_active_current_path(ctx: &mut MuiContext) -> i32 {
    if ctx.tabs.active_read_only() {
        return reject_read_only_save(ctx);
    }
    let Some(path) = ctx.tabs.active_path() else {
        let root = file_dialog_initial_dir(ctx);
        let target = match pick_save_file_native(&root, "untitled.mty", dialog_owner_hwnd(ctx)) {
            FileDialogPick::Picked(path) => path,
            FileDialogPick::Cancelled => {
                println!("mui_ed_save: native save dialog cancelled for untitled tab");
                ctx.push_toast(crate::toast::Kind::Info, "Save cancelled; tab is still open");
                return -2;
            }
            FileDialogPick::Unavailable => {
                eprintln!("mui_ed_save: no file path and native save dialog unavailable");
                ctx.push_toast(crate::toast::Kind::Warn, "Save dialog unavailable; use typed path");
                return -1;
            }
        };
        return save_active_to_path(ctx, target);
    };
    if ctx.tabs.any_dirty_path_except(&path, ctx.tabs.active()) {
        ctx.autosave.disarm();
        ctx.push_toast(
            crate::toast::Kind::Warn,
            "Save skipped: duplicate edits",
        );
        println!(
            "mui_ed_save: skipped dirty duplicate path={}",
            path.display()
        );
        return -1;
    }
    let bytes = save_bytes_for_active(ctx);
    let name = basename(&path);
    let resurrected_path = !path.is_file();
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            mark_active_clean(ctx);
            let active = ctx.tabs.active();
            let _ = ctx
                .tabs
                .reload_all_clean_path_except(&path, &bytes, active);
            ctx.autosave.disarm();
            if resurrected_path {
                record_recent_file(ctx, path.clone());
                refresh_workspace_file_views(ctx);
            }
            println!("mui_ed_save: {} ({} bytes)", path.display(), bytes.len());
            ctx.push_toast(crate::toast::Kind::Success, format!("Saved {name}"));
            0
        }
        Err(e) => {
            eprintln!("mui_ed_save({}): {e}", path.display());
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Save", &path, &e),
            );
            -1
        }
    }
}

fn save_confirm_tab(ctx: &mut MuiContext, idx: usize) -> bool {
    if idx >= ctx.tabs.count() {
        return false;
    }
    if let Some(path) = ctx.tabs.path(idx) {
        save_tab_to_path(ctx, idx, path, true) == 0
    } else {
        let root = file_dialog_initial_dir(ctx);
        let suggested = ctx
            .tabs
            .get(idx)
            .and_then(|t| t.path.as_ref())
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "untitled.mty".to_string());
        let target = match pick_save_file_native(&root, &suggested, dialog_owner_hwnd(ctx)) {
            FileDialogPick::Picked(path) => path,
            FileDialogPick::Cancelled => {
                ctx.push_toast(crate::toast::Kind::Info, "Save cancelled; tab is still open");
                return false;
            }
            FileDialogPick::Unavailable => {
                ctx.push_toast(crate::toast::Kind::Warn, "Save dialog unavailable; use Save As");
                return false;
            }
        };
        save_tab_to_path(ctx, idx, target, true) == 0
    }
}

fn save_tab_to_path(ctx: &mut MuiContext, idx: usize, path: PathBuf, toast_success: bool) -> i32 {
    if ctx.tabs.any_dirty_path_except(&path, idx) {
        ctx.autosave.disarm();
        ctx.push_toast(
            crate::toast::Kind::Warn,
            "Save skipped: duplicate edits",
        );
        println!(
            "mui_ed_save: skipped dirty duplicate path={}",
            path.display()
        );
        return -1;
    }
    let Some(tab) = ctx.tabs.get_mut(idx) else {
        return -1;
    };
    if tab.read_only {
        let name = tab
            .path
            .as_deref()
            .map(basename)
            .unwrap_or_else(|| "binary file".to_string());
        ctx.autosave.disarm();
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("{name} is read-only in the text editor"),
        );
        return -1;
    }
    let path_changed = tab.path.as_ref() != Some(&path);
    if path_changed {
        if let Err(e) = validate_save_target_basename(&path) {
            ctx.autosave.disarm();
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("mui_ed_save: invalid target filename: {e}");
            return -1;
        }
    }
    let bytes = save_bytes_for_tab(tab);
    let name = basename(&path);
    let resurrected_path = !path.is_file();
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            tab.path = Some(path.clone());
            tab.read_only = false;
            tab.dirty = false;
            tab.model.mark_clean();
            let _ = ctx.tabs.reload_all_clean_path_except(&path, &bytes, idx);
            if idx == ctx.tabs.active() {
                sync_active_path(ctx);
            }
            ctx.autosave.disarm();
            if path_changed || resurrected_path {
                record_recent_file(ctx, path.clone());
                refresh_workspace_file_views(ctx);
            }
            println!("mui_ed_save: {} ({} bytes)", path.display(), bytes.len());
            if toast_success {
                ctx.push_toast(crate::toast::Kind::Success, format!("Saved {name}"));
            }
            0
        }
        Err(e) => {
            eprintln!("mui_ed_save({}): {e}", path.display());
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Save", &path, &e),
            );
            -1
        }
    }
}

/// Write the active model to its tab's file path. Returns `0` on success, `-1`
/// on error (no path / IO failure). Marks the model clean on success.
#[no_mangle]
pub extern "C" fn mui_ed_save(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    save_active_current_path(ctx)
}

fn save_all_failed_phrase(failed: i32) -> String {
    let noun = if failed == 1 { "file" } else { "files" };
    format!("{failed} {noun} failed")
}

/// Save every dirty tab. File-backed tabs write in place; untitled tabs ask for
/// a native Save As path. Returns the number of tabs saved, or -1 when nothing
/// could be saved because every attempted write failed.
#[no_mangle]
pub extern "C" fn mui_save_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let dirty: Vec<usize> = (0..ctx.tabs.count()).filter(|i| ctx.tabs.is_dirty(*i)).collect();
    if dirty.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "No unsaved files");
        return 0;
    }
    let mut saved = 0_i32;
    let mut failed = 0_i32;
    let mut untitled = 0_i32;
    let mut untitled_cancelled = 0_i32;
    let mut untitled_unavailable = 0_i32;
    let mut read_only = 0_i32;
    let mut dirty_conflicts = 0_i32;
    let mut first_failed_message: Option<String> = None;
    let original_active = ctx.tabs.active();
    for idx in dirty {
        let path_conflict = ctx
            .tabs
            .path(idx)
            .is_some_and(|path| ctx.tabs.any_dirty_path_except(&path, idx));
        if path_conflict {
            dirty_conflicts += 1;
            continue;
        }
        let Some(tab) = ctx.tabs.get_mut(idx) else {
            continue;
        };
        if tab.read_only {
            read_only += 1;
            continue;
        }
        if let Some(path) = tab.path.clone() {
            let bytes = save_bytes_for_tab(tab);
            let resurrected_path = !path.is_file();
            match std::fs::write(&path, &bytes) {
                Ok(()) => {
                    tab.dirty = false;
                    tab.model.mark_clean();
                    let _ = ctx.tabs.reload_all_clean_path_except(&path, &bytes, idx);
                    if resurrected_path {
                        record_recent_file(ctx, path.clone());
                        refresh_workspace_file_views(ctx);
                    }
                    saved += 1;
                }
                Err(e) => {
                    failed += 1;
                    if first_failed_message.is_none() {
                        first_failed_message = Some(file_operation_failed_message("Save", &path, &e));
                    }
                    eprintln!("mui_save_all({}): {e}", path.display());
                }
            }
        } else {
            let root = file_dialog_initial_dir(ctx);
            let target = match pick_save_file_native(&root, "untitled.mty", dialog_owner_hwnd(ctx)) {
                FileDialogPick::Picked(path) => path,
                FileDialogPick::Cancelled => {
                    untitled += 1;
                    untitled_cancelled += 1;
                    continue;
                }
                FileDialogPick::Unavailable => {
                    untitled += 1;
                    untitled_unavailable += 1;
                    continue;
                }
            };
            trace(&format!("save_all_dialog path={}", target.display()));
            if save_tab_to_path(ctx, idx, target, false) == 0 {
                saved += 1;
            } else {
                failed += 1;
                if first_failed_message.is_none() {
                    first_failed_message = ctx.toasts.toasts().last().and_then(|toast| {
                        toast
                            .message
                            .strip_prefix("Save failed: ")
                            .map(|detail| format!("Save failed: {detail}"))
                    });
                }
            }
        }
    }
    if original_active < ctx.tabs.count() {
        ctx.tabs.switch(original_active);
        sync_active_path(ctx);
    }
    ctx.pending_dirty_close = None;
    if ctx.tabs.dirty_count() == 0 {
        ctx.pending_quit = None;
    }
    ctx.autosave.disarm();
    refresh_workspace_file_views(ctx);
    match (saved, failed, untitled, read_only + dirty_conflicts) {
        (0, 0, 0, skipped) if skipped > 0 => {
            let noun = if skipped == 1 { "file" } else { "files" };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!("{skipped} {noun} skipped"),
            );
            0
        }
        (0, 0, u, 0) if u > 0 && untitled_cancelled > 0 => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Save All cancelled; {u} {noun} still unsaved"),
            );
            0
        }
        (0, 0, u, 0) if u > 0 && untitled_unavailable > 0 => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!("Save dialog unavailable; {u} {noun} still unsaved"),
            );
            0
        }
        (0, 0, u, 0) if u > 0 => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            ctx.push_toast(crate::toast::Kind::Warn, format!("{u} {noun} need Save As"));
            0
        }
        (0, f, _, _) if f > 0 => {
            let message = if f == 1 {
                if let Some(detail) = first_failed_message.as_deref() {
                    format!("Save All failed: {}", detail.trim_start_matches("Save failed: "))
                } else {
                    format!("Save All failed: {}", save_all_failed_phrase(f))
                }
            } else {
                format!("Save All failed: {}", save_all_failed_phrase(f))
            };
            ctx.push_toast(
                crate::toast::Kind::Error,
                message,
            );
            -1
        }
        (s, 0, 0, 0) => {
            let noun = if s == 1 { "file" } else { "files" };
            ctx.push_toast(crate::toast::Kind::Success, format!("Saved {s} {noun}"));
            s
        }
        (s, 0, u, 0) if untitled_cancelled > 0 => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!("Saved {s}; Save All cancelled for {u} {noun}"),
            );
            s
        }
        (s, 0, u, 0) if untitled_unavailable > 0 => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!("Saved {s}; Save dialog unavailable for {u} {noun}"),
            );
            s
        }
        (s, 0, u, 0) => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            ctx.push_toast(crate::toast::Kind::Warn, format!("Saved {s}; {u} {noun} need Save As"));
            s
        }
        (s, 0, u, r) => {
            let noun = if u == 1 { "untitled file" } else { "untitled files" };
            let skipped = if r == 1 { "file" } else { "files" };
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!("Saved {s}; {u} {noun} need Save As; {r} {skipped} skipped"),
            );
            s
        }
        (s, f, _, r) => {
            let failed = if f == 1 {
                if let Some(detail) = first_failed_message.as_deref() {
                    detail.to_string()
                } else {
                    save_all_failed_phrase(f)
                }
            } else {
                save_all_failed_phrase(f)
            };
            if r > 0 {
                let skipped = if r == 1 { "file" } else { "files" };
                ctx.push_toast(
                    crate::toast::Kind::Warn,
                    format!("Saved {s}; {failed}; {r} {skipped} skipped"),
                );
            } else {
                ctx.push_toast(crate::toast::Kind::Warn, format!("Saved {s}; {failed}"));
            }
            s
        }
    }
}

/// `1` when the active tab is backed by a file path; `0` for an untitled buffer.
/// The IDE uses this to route Ctrl+S to a Save-As prompt for untitled buffers.
#[no_mangle]
pub extern "C" fn mui_active_has_path(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.tabs.active_has_path() { 1 } else { 0 })
}

/// Save-As: write the active (untitled) buffer to the path staged via
/// `mui_path_clear`/`mui_path_push` (resolved under the workspace root), bind the
/// tab to that path, mark it clean, and refresh the tree. Returns `0` on success.
#[no_mangle]
pub extern "C" fn mui_save_as(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let raw = String::from_utf8_lossy(&staged).into_owned();
    let raw = raw.trim();
    if raw.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "No save path entered");
        return -1;
    }
    let base = crate::wsabi::effective_root(ctx);
    let cand = std::path::Path::new(raw);
    let target = if cand.is_absolute() { cand.to_path_buf() } else { base.join(cand) };
    save_active_to_path(ctx, target)
}

/// Save-As through a native Windows save-file picker. Returns `0` on success,
/// `-2` when cancelled, or `-1` when unavailable/failed, letting Mighty keep a
/// fallback prompt only when the native picker could not run.
#[no_mangle]
pub extern "C" fn mui_save_as_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let root = file_dialog_initial_dir(ctx);
    let suggested = ctx
        .tabs
        .active_path()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "untitled.mty".to_string());
    let target = match pick_save_file_native(&root, &suggested, dialog_owner_hwnd(ctx)) {
        FileDialogPick::Picked(path) => path,
        FileDialogPick::Cancelled => {
            println!("mui_save_as_dialog: native save dialog cancelled");
            ctx.push_toast(crate::toast::Kind::Info, "Save cancelled; tab is still open");
            return -2;
        }
        FileDialogPick::Unavailable => {
            println!("mui_save_as_dialog: native save dialog unavailable");
            ctx.push_toast(crate::toast::Kind::Warn, "Save dialog unavailable; use typed path");
            return -1;
        }
    };
    save_active_to_path(ctx, target)
}

fn save_active_to_path(ctx: &mut MuiContext, target: PathBuf) -> i32 {
    if ctx.tabs.active_read_only() {
        return reject_read_only_save(ctx);
    }
    if let Err(e) = validate_save_target_basename(&target) {
        ctx.autosave.disarm();
        ctx.push_toast(crate::toast::Kind::Warn, e.clone());
        println!("mui_save_as: invalid target filename: {e}");
        return -1;
    }
    if ctx
        .tabs
        .find_by_path(&target)
        .is_some_and(|idx| idx != ctx.tabs.active())
    {
        ctx.push_toast(crate::toast::Kind::Warn, "Target file is already open");
        return -1;
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bytes = save_bytes_for_active(ctx);
    let name = basename(&target);
    trace(&format!("save_as path={} bytes={}", target.display(), bytes.len()));
    match std::fs::write(&target, &bytes) {
        Ok(()) => {
            ctx.tabs.set_active_path(target.clone());
            ctx.language = crate::langdetect::detect_path(&target);
            ctx.file_path = Some(target.clone());
            mark_active_clean(ctx);
            let active = ctx.tabs.active();
            if let Some(path) = ctx.file_path.clone() {
                let _ = ctx
                    .tabs
                    .reload_all_clean_path_except(&path, &bytes, active);
            }
            ctx.autosave.disarm();
            record_recent_file(ctx, target.clone());
            refresh_workspace_file_views(ctx);
            ctx.push_toast(crate::toast::Kind::Success, format!("Saved {name}"));
            0
        }
        Err(e) => {
            eprintln!("mui_save_as({}): {e}", target.display());
            ctx.push_toast(
                crate::toast::Kind::Error,
                file_operation_failed_message("Save", &target, &e),
            );
            -1
        }
    }
}

fn validate_save_target_basename(path: &std::path::Path) -> Result<(), String> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err("Choose a file name".to_string());
    };
    crate::newproj::validate_platform_segment(name)
}

enum FileDialogPick {
    Picked(PathBuf),
    Cancelled,
    Unavailable,
}

fn dialog_pick_from_raw_path(path: String) -> FileDialogPick {
    if path.trim().is_empty() {
        FileDialogPick::Cancelled
    } else {
        FileDialogPick::Picked(PathBuf::from(path))
    }
}

fn pick_open_file_native(initial_dir: &std::path::Path, owner_hwnd: Option<isize>) -> FileDialogPick {
    if let Ok(sequence) = std::env::var("MUI_OPEN_FILE_PICK_SEQUENCE") {
        let trimmed = sequence.trim();
        if !trimmed.is_empty() {
            return next_open_file_pick_from_sequence(trimmed);
        }
    }
    if let Ok(path) = std::env::var("MUI_OPEN_FILE_PICK") {
        return dialog_pick_from_raw_path(path);
    }
    if !cfg!(windows) {
        return FileDialogPick::Unavailable;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.OpenFileDialog
$d.Title = 'Open File'
$d.Filter = 'Mighty/code files (*.mty;*.rs;*.js;*.ts;*.tsx;*.jsx;*.py;*.go;*.md;*.toml;*.json)|*.mty;*.rs;*.js;*.ts;*.tsx;*.jsx;*.py;*.go;*.md;*.toml;*.json|All files (*.*)|*.*'
$dir = $env:MUI_DIALOG_DIR
if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) { $d.InitialDirectory = $dir }
$owner = $null
$ownerForm = $null
$ownerHwnd = 0L
$ownerText = $env:MUI_DIALOG_OWNER
if ($ownerText -and [Int64]::TryParse($ownerText, [ref]$ownerHwnd) -and $ownerHwnd -ne 0) {
  $owner = New-Object System.Windows.Forms.NativeWindow
  $owner.AssignHandle([IntPtr]$ownerHwnd)
} else {
  $ownerForm = New-Object System.Windows.Forms.Form
  $ownerForm.TopMost = $true
  $ownerForm.ShowInTaskbar = $false
  $ownerForm.StartPosition = 'CenterScreen'
  $ownerForm.Width = 1
  $ownerForm.Height = 1
  $owner = $ownerForm
}
try {
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }
} finally {
  if ($owner -is [System.Windows.Forms.NativeWindow]) { $owner.ReleaseHandle() }
  if ($ownerForm) { $ownerForm.Dispose() }
}
"#;
    run_file_dialog_script(script, initial_dir, None, owner_hwnd)
}

fn pick_save_file_native(
    initial_dir: &std::path::Path,
    suggested_name: &str,
    owner_hwnd: Option<isize>,
) -> FileDialogPick {
    #[cfg(test)]
    if std::env::var_os("MUI_SAVE_FILE_FORCE_UNAVAILABLE").is_some() {
        return FileDialogPick::Unavailable;
    }
    if let Ok(sequence) = std::env::var("MUI_SAVE_FILE_PICK_SEQUENCE") {
        return next_save_file_pick_from_sequence(&sequence);
    }
    if let Ok(path) = std::env::var("MUI_SAVE_FILE_PICK") {
        return dialog_pick_from_raw_path(path);
    }
    if !cfg!(windows) {
        return FileDialogPick::Unavailable;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.SaveFileDialog
$d.Title = 'Save As'
$d.Filter = 'Mighty files (*.mty)|*.mty|All files (*.*)|*.*'
$d.DefaultExt = 'mty'
$d.AddExtension = $true
$d.OverwritePrompt = $true
$dir = $env:MUI_DIALOG_DIR
if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) { $d.InitialDirectory = $dir }
$name = $env:MUI_DIALOG_FILE
if ($name) { $d.FileName = $name }
$owner = $null
$ownerForm = $null
$ownerHwnd = 0L
$ownerText = $env:MUI_DIALOG_OWNER
if ($ownerText -and [Int64]::TryParse($ownerText, [ref]$ownerHwnd) -and $ownerHwnd -ne 0) {
  $owner = New-Object System.Windows.Forms.NativeWindow
  $owner.AssignHandle([IntPtr]$ownerHwnd)
} else {
  $ownerForm = New-Object System.Windows.Forms.Form
  $ownerForm.TopMost = $true
  $ownerForm.ShowInTaskbar = $false
  $ownerForm.StartPosition = 'CenterScreen'
  $ownerForm.Width = 1
  $ownerForm.Height = 1
  $owner = $ownerForm
}
try {
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }
} finally {
  if ($owner -is [System.Windows.Forms.NativeWindow]) { $owner.ReleaseHandle() }
  if ($ownerForm) { $ownerForm.Dispose() }
}
"#;
    run_file_dialog_script(script, initial_dir, Some(suggested_name), owner_hwnd)
}

fn next_open_file_pick_from_sequence(sequence: &str) -> FileDialogPick {
    static NEXT_PICK: OnceLock<Mutex<(String, usize)>> = OnceLock::new();
    let mut state = NEXT_PICK
        .get_or_init(|| Mutex::new((String::new(), 0)))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state.0 != sequence {
        state.0 = sequence.to_string();
        state.1 = 0;
    }
    let idx = state.1;
    state.1 = state.1.saturating_add(1);
    dialog_pick_from_raw_path(sequence.split('|').nth(idx).unwrap_or("").to_string())
}

fn next_save_file_pick_from_sequence(sequence: &str) -> FileDialogPick {
    static NEXT_PICK: OnceLock<Mutex<(String, usize)>> = OnceLock::new();
    let mut state = NEXT_PICK
        .get_or_init(|| Mutex::new((String::new(), 0)))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state.0 != sequence {
        state.0 = sequence.to_string();
        state.1 = 0;
    }
    let idx = state.1;
    state.1 = state.1.saturating_add(1);
    dialog_pick_from_raw_path(sequence.split('|').nth(idx).unwrap_or("").to_string())
}

fn pick_new_file_native(initial_dir: &std::path::Path, owner_hwnd: Option<isize>) -> FileDialogPick {
    if let Ok(seq) = std::env::var("MUI_NEW_FILE_PICK_SEQUENCE") {
        let trimmed = seq.trim();
        if !trimmed.is_empty() {
            return next_new_file_pick_from_sequence(trimmed);
        }
    }
    if let Ok(path) = std::env::var("MUI_NEW_FILE_PICK") {
        return dialog_pick_from_raw_path(path);
    }
    if !cfg!(windows) {
        return FileDialogPick::Unavailable;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.SaveFileDialog
$d.Title = 'New File'
$d.Filter = 'Mighty files (*.mty)|*.mty|All files (*.*)|*.*'
$d.DefaultExt = 'mty'
$d.AddExtension = $true
$d.OverwritePrompt = $false
$d.CheckFileExists = $false
$d.CheckPathExists = $true
$dir = $env:MUI_DIALOG_DIR
if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) { $d.InitialDirectory = $dir }
$d.FileName = 'untitled.mty'
$owner = $null
$ownerForm = $null
$ownerHwnd = 0L
$ownerText = $env:MUI_DIALOG_OWNER
if ($ownerText -and [Int64]::TryParse($ownerText, [ref]$ownerHwnd) -and $ownerHwnd -ne 0) {
  $owner = New-Object System.Windows.Forms.NativeWindow
  $owner.AssignHandle([IntPtr]$ownerHwnd)
} else {
  $ownerForm = New-Object System.Windows.Forms.Form
  $ownerForm.TopMost = $true
  $ownerForm.ShowInTaskbar = $false
  $ownerForm.StartPosition = 'CenterScreen'
  $ownerForm.Width = 1
  $ownerForm.Height = 1
  $owner = $ownerForm
}
try {
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }
} finally {
  if ($owner -is [System.Windows.Forms.NativeWindow]) { $owner.ReleaseHandle() }
  if ($ownerForm) { $ownerForm.Dispose() }
}
"#;
    run_file_dialog_script(script, initial_dir, None, owner_hwnd)
}

fn pick_new_folder_native(initial_dir: &std::path::Path, owner_hwnd: Option<isize>) -> FileDialogPick {
    if let Ok(path) = std::env::var("MUI_NEW_FOLDER_PICK") {
        return dialog_pick_from_raw_path(path);
    }
    if !cfg!(windows) {
        return FileDialogPick::Unavailable;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.FolderBrowserDialog
$d.Description = 'Choose or create a folder'
$d.ShowNewFolderButton = $true
$dir = $env:MUI_DIALOG_DIR
if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) { $d.SelectedPath = $dir }
$owner = $null
$ownerForm = $null
$ownerHwnd = 0L
$ownerText = $env:MUI_DIALOG_OWNER
if ($ownerText -and [Int64]::TryParse($ownerText, [ref]$ownerHwnd) -and $ownerHwnd -ne 0) {
  $owner = New-Object System.Windows.Forms.NativeWindow
  $owner.AssignHandle([IntPtr]$ownerHwnd)
} else {
  $ownerForm = New-Object System.Windows.Forms.Form
  $ownerForm.TopMost = $true
  $ownerForm.ShowInTaskbar = $false
  $ownerForm.StartPosition = 'CenterScreen'
  $ownerForm.Width = 1
  $ownerForm.Height = 1
  $owner = $ownerForm
}
try {
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.SelectedPath) }
} finally {
  if ($owner -is [System.Windows.Forms.NativeWindow]) { $owner.ReleaseHandle() }
  if ($ownerForm) { $ownerForm.Dispose() }
}
"#;
    run_file_dialog_script(script, initial_dir, None, owner_hwnd)
}

fn pick_new_project_native(initial_dir: &std::path::Path, owner_hwnd: Option<isize>) -> FileDialogPick {
    if let Ok(path) = std::env::var("MUI_NEW_PROJECT_PICK") {
        return dialog_pick_from_raw_path(path);
    }
    if !cfg!(windows) {
        return FileDialogPick::Unavailable;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.FolderBrowserDialog
$d.Description = 'Choose or create the Mighty project folder'
$d.ShowNewFolderButton = $true
$dir = $env:MUI_DIALOG_DIR
if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) { $d.SelectedPath = $dir }
$owner = $null
$ownerForm = $null
$ownerHwnd = 0L
$ownerText = $env:MUI_DIALOG_OWNER
if ($ownerText -and [Int64]::TryParse($ownerText, [ref]$ownerHwnd) -and $ownerHwnd -ne 0) {
  $owner = New-Object System.Windows.Forms.NativeWindow
  $owner.AssignHandle([IntPtr]$ownerHwnd)
} else {
  $ownerForm = New-Object System.Windows.Forms.Form
  $ownerForm.TopMost = $true
  $ownerForm.ShowInTaskbar = $false
  $ownerForm.StartPosition = 'CenterScreen'
  $ownerForm.Width = 1
  $ownerForm.Height = 1
  $owner = $ownerForm
}
try {
  if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.SelectedPath) }
} finally {
  if ($owner -is [System.Windows.Forms.NativeWindow]) { $owner.ReleaseHandle() }
  if ($ownerForm) { $ownerForm.Dispose() }
}
"#;
    run_file_dialog_script(script, initial_dir, None, owner_hwnd)
}

fn next_new_file_pick_from_sequence(sequence: &str) -> FileDialogPick {
    static NEXT_PICK: OnceLock<Mutex<(String, usize)>> = OnceLock::new();
    let mut state = NEXT_PICK
        .get_or_init(|| Mutex::new((String::new(), 0)))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state.0 != sequence {
        state.0 = sequence.to_string();
        state.1 = 0;
    }
    let idx = state.1;
    state.1 = state.1.saturating_add(1);
    dialog_pick_from_raw_path(sequence.split('|').nth(idx).unwrap_or("").to_string())
}

fn run_file_dialog_script(
    script: &str,
    initial_dir: &std::path::Path,
    suggested_name: Option<&str>,
    owner_hwnd: Option<isize>,
) -> FileDialogPick {
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-STA", "-Command", script])
        .env("MUI_DIALOG_DIR", initial_dir)
        .stdin(std::process::Stdio::null());
    if let Some(name) = suggested_name {
        cmd.env("MUI_DIALOG_FILE", name);
    }
    if let Some(hwnd) = owner_hwnd {
        cmd.env("MUI_DIALOG_OWNER", hwnd.to_string());
    }
    let out = cmd.output();
    restore_dialog_owner_focus(owner_hwnd);
    let Ok(out) = out else {
        return FileDialogPick::Unavailable;
    };
    if !out.status.success() {
        return FileDialogPick::Unavailable;
    }
    dialog_pick_from_raw_path(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(target_os = "windows")]
pub(crate) fn restore_dialog_owner_focus(owner_hwnd: Option<isize>) {
    use std::ffi::c_void;

    let Some(hwnd) = owner_hwnd else {
        return;
    };
    if hwnd == 0 {
        return;
    }

    unsafe extern "system" {
        fn ShowWindow(hwnd: *mut c_void, n_cmd_show: i32) -> i32;
        fn BringWindowToTop(hwnd: *mut c_void) -> i32;
        fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
    }

    let hwnd = hwnd as *mut c_void;
    unsafe {
        // SW_SHOW keeps restored/maximized state intact while making sure the
        // parent IDE window receives the next real click after a child dialog.
        let _ = ShowWindow(hwnd, 5);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
    trace("dialog_focus_restore");
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn restore_dialog_owner_focus(_owner_hwnd: Option<isize>) {}

/// Stream the active model's bytes into the shim's find engine and run the
/// search using the active prompt's query. Replaces the Mighty byte-push loop —
/// the model is the source of truth. Returns the match count.
#[no_mangle]
pub extern "C" fn mui_ed_find_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let text = ctx.tabs.active_model().as_text();
    ctx.find.reset();
    for b in text.bytes() {
        ctx.find.push_byte(b as u32);
    }
    let needle = ctx.prompt.query_string();
    let count = ctx.find.run(&needle);
    if count == 0 {
        let message = if needle.is_empty() {
            "Enter text to find"
        } else {
            "No matches found"
        };
        ctx.push_toast(crate::toast::Kind::Info, message);
    }
    count
}

/// Stream the active model into the completion engine and request completion at
/// the cursor. Returns the candidate count. Replaces the Mighty byte-push loop.
#[no_mangle]
pub extern "C" fn mui_ed_complete_request(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (line, col) = {
        let m = ctx.tabs.active_model();
        (m.cursor_line() as i32, m.cursor_col() as i32)
    };
    let text = ctx.tabs.active_model().as_text();
    ctx.complete_buf = text.into_bytes();
    let cursor = line_col_to_offset(&ctx.complete_buf, line, col);
    let lsp_labels: Vec<String> = match ctx.file_path.clone() {
        Some(path) => {
            let source = String::from_utf8_lossy(&ctx.complete_buf).into_owned();
            lsp_semantic_labels(ctx.language, &path, &source, line.max(0) as u32, col.max(0) as u32)
        }
        None => Vec::new(),
    };
    ctx.complete
        .request(&ctx.complete_buf, cursor, &lsp_labels)
        .min(i32::MAX as usize) as i32
}

/// Accept the selected completion candidate into the active model: delete the
/// prefix chars before the cursor, then insert the accepted text. Returns the
/// accepted text's char length.
#[no_mangle]
pub extern "C" fn mui_ed_complete_accept(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return reject_read_only_edit(ctx);
    }
    if !ctx.complete.is_active() || ctx.complete.count() == 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No autocomplete suggestions open");
        return 0;
    }
    let accepted = ctx.complete.accepted_text().to_string();
    if accepted.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "No autocomplete suggestion selected");
        return 0;
    }
    let before = ctx.tabs.active_model().as_text();
    let prefix = ctx.complete.prefix_len();
    let m = ctx.tabs.active_model_mut();
    for _ in 0..prefix {
        m.backspace();
    }
    for ch in accepted.chars() {
        m.insert_char(ch);
    }
    if ctx.tabs.active_model().as_text() == before {
        ctx.push_toast(
            crate::toast::Kind::Info,
            "Autocomplete suggestion already inserted",
        );
        return 0;
    }
    accepted.chars().count() as i32
}

/// Stream the active model into the nav buffer (hover / go-to-definition).
#[no_mangle]
pub extern "C" fn mui_ed_nav_stream(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let text = ctx.tabs.active_model().as_text();
        ctx.nav_buf = text.into_bytes();
    }
}

/// Switch to tab `idx`, syncing the active path. Tab switching is now a plain
/// index change (each tab owns its model), so no byte-swap loop is needed.
/// Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_ed_tab_switch(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx >= 0 {
        ctx.tabs.switch(idx as usize);
        // The tab bar targets the FOCUSED pane: point it at the new active tab so
        // the split stays coherent (a no-op binding when unsplit).
        let f = ctx.panes.focused();
        ctx.panes.set_tab(f, ctx.tabs.active());
        sync_active_path(ctx);
        // Opening / switching to any tab leaves the forced Welcome landing.
        ctx.welcome.dismiss();
    }
    trace(&format!("tab_switch idx={idx} -> active={}", ctx.tabs.active()));
    ctx.tabs.active() as i32
}

/// Map the last mouse-click pixel to a buffer `(line, col)` and move the active
/// model's cursor there. Returns the resulting cursor line. Uses the gutter
/// sizing from the model's own line count.
#[no_mangle]
pub extern "C" fn mui_ed_click(handle: i64) -> i32 {
    // Fold gutter: a click on a chevron toggles that region instead of placing
    // the cursor. Done first (no new Mighty ladder arm — L37/L38). When a chevron
    // was toggled, leave the cursor where it is and return its line.
    if mui_fold_gutter_click(handle) >= 0 {
        return unsafe { ctx(handle) }.map_or(0, |c| c.tabs.active_model().cursor_line() as i32);
    }
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    // Interactive minimap: a click landing in the focused pane's minimap strip
    // jumps the editor so the clicked position maps to the corresponding source
    // line (scroll there + move the cursor to that line). Folded in here (no new
    // Mighty ladder arm — L37/L38 discipline). Falls through to normal cell
    // placement when the click is outside the strip / minimap is hidden.
    let (ex, ey) = (ctx.last_event.x, ctx.last_event.y);
    if let Some(g) = ctx.minimap_geom {
        let win_h = ctx.gpu.height as f32;
        let in_band = ey >= g.top - 4.0 && ey <= (win_h - 30.0);
        if g.contains_x(ex) && in_band {
            let line = g.line_at_y(ey);
            let rows = layout::visible_rows_in(
                layout::region(ctx.sidebar_visible),
                ctx.gpu.height,
                ctx.bottom_dock_open(),
            ) as usize;
            let first = g.scroll_to_center(line, rows);
            let m = ctx.tabs.active_model_mut();
            m.set_first_visible(first);
            m.move_to(line as i32, 0);
            return m.cursor_line() as i32;
        }
    }
    let mut region = layout::region(ctx.sidebar_visible);
    // When split, resolve the click against the FOCUSED pane's column (its left
    // edge), so the gutter/text math lines up with where that pane is drawn. The
    // focused pane's tab is the active tab (rebound on click→focus), so reading
    // the active model is correct. Unsplit: the full region, unchanged.
    let count = ctx.panes.count();
    if count > 1 {
        let win_w = ctx.gpu.width as f32;
        region = layout::pane_region(region, win_w, count, ctx.panes.focused());
    }
    let total = ctx.tabs.active_model().line_count() as u64;
    let first = ctx.tabs.active_model().first_visible() as u64;
    let (line, col) =
        layout::pixel_to_cell_in(region, ctx.last_event.x, ctx.last_event.y, first, total);
    // `pixel_to_cell_in` returns `line = first + screen_row` (it has no fold
    // awareness). Translate the screen row through the fold mapping to the SOURCE
    // line actually painted there, so a click below a folded region lands on the
    // right line. With no folds active this is identical (`src == line`).
    let screen_row = (line - first) as usize;
    let total_u = total as usize;
    let first_vis = ctx.tabs.active_fold().source_to_visible(first as usize, total_u);
    let src = ctx
        .tabs
        .active_fold()
        .visible_to_source(first_vis + screen_row, total_u);
    let m = ctx.tabs.active_model_mut();
    m.move_to(src as i32, col as i32);
    m.cursor_line() as i32
}

/// Hit-test the LAST mouse-click against the fold gutter (the chevron column to
/// the left of the line numbers) and, if it landed on a foldable region's
/// header row, toggle that fold. Returns the toggled header line (0-based), or
/// `-1` when the click wasn't on a chevron. The Mighty side calls this BEFORE
/// the normal editor click so a chevron click folds instead of moving the
/// cursor. Visible-row aware: the click y maps through the fold mapping to the
/// source line actually shown on that row.
#[no_mangle]
pub extern "C" fn mui_fold_gutter_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let (ex, ey) = (ctx.last_event.x, ctx.last_event.y);
    let mut region = layout::region(ctx.sidebar_visible);
    let count = ctx.panes.count();
    if count > 1 {
        let win_w = ctx.gpu.width as f32;
        region = layout::pane_region(region, win_w, count, ctx.panes.focused());
    }
    // The chevron sits in a narrow band at the LEFT of the gutter (before the
    // right-aligned line numbers). Accept a click anywhere in the first
    // `FOLD_GUTTER_W` px of the gutter column.
    let band_left = region.left;
    let band_right = region.left + FOLD_GUTTER_W;
    if ex < band_left || ex > band_right {
        return -1;
    }
    // Which VISIBLE row was clicked, then the source line shown there.
    let row_top = region.top + layout::PAD;
    if ey < row_top {
        return -1;
    }
    let vis_row = ((ey - row_top) / layout::LINE_H()).floor() as usize;
    let first = ctx.tabs.active_model().first_visible();
    let total = ctx.tabs.active_model().line_count();
    // `first` is a SOURCE line; convert it to a visible row, add the clicked row
    // offset, then back to the source line that is actually painted there.
    let first_vis = ctx.tabs.active_fold().source_to_visible(first, total);
    let src = ctx
        .tabs
        .active_fold()
        .visible_to_source(first_vis + vis_row, total);
    if ctx.tabs.active_fold().is_foldable_start(src) {
        ctx.tabs.active_fold_mut().toggle(src);
        return src as i32;
    }
    -1
}

/// Width (px) of the clickable fold-chevron band at the left of the gutter.
pub(crate) const FOLD_GUTTER_W: f32 = 14.0;

/// Map a minimap pixel `(x, y)` to the buffer line it represents, or `-1` if the
/// point is outside the focused pane's minimap strip (or the minimap is hidden).
/// Companion to the folded-in minimap jump in [`mui_ed_click`]; exposed for the
/// Mighty side / tests that want the mapping without moving the cursor.
#[no_mangle]
pub extern "C" fn mui_minimap_click(handle: i64, x: f32, y: f32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(g) = ctx.minimap_geom else {
        return -1;
    };
    let win_h = ctx.gpu.height as f32;
    let in_band = y >= g.top - 4.0 && y <= (win_h - 30.0);
    if g.contains_x(x) && in_band {
        g.line_at_y(y) as i32
    } else {
        -1
    }
}

/// `1` when the focused pane currently shows an interactive minimap strip (its
/// geometry is live), else `0`. Lets the Mighty side / tests probe presence.
#[no_mangle]
pub extern "C" fn mui_minimap_active(handle: i64) -> i32 {
    match unsafe { ctx(handle) } {
        Some(ctx) if ctx.minimap_geom.is_some() => 1,
        _ => 0,
    }
}

/// The minimap strip's left x (pixels), or `-1.0` when hidden.
#[no_mangle]
pub extern "C" fn mui_minimap_left(handle: i64) -> f32 {
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx.minimap_geom.map(|g| g.x).unwrap_or(-1.0),
        None => -1.0,
    }
}

/// The minimap strip's width (pixels), or `0.0` when hidden.
#[no_mangle]
pub extern "C" fn mui_minimap_width(handle: i64) -> f32 {
    match unsafe { ctx(handle) } {
        Some(ctx) => ctx.minimap_geom.map(|g| g.w).unwrap_or(0.0),
        None => 0.0,
    }
}

/// Draw the editor body from the authoritative model: the current-line band,
/// right-aligned gutter numbers (the cursor's line brighter), syntax-colored
/// source text, the translucent selection rect, and the 2px ember caret.
/// `rows` is the visible row count; the model owns the scroll offset.
///
/// Pane-aware: with ONE pane this is byte-identical to the historical single
/// editor (the full body region, the active tab, no divider / focus chrome). With
/// a split it draws every pane into its column via [`draw_editor_pane`], plus the
/// 1px dividers between them. See `crate::panes`.
pub(crate) const MINIMAP_W: f32 = 70.0;
pub(crate) const MINIMAP_COMPACT_W: f32 = 40.0;
pub(crate) const MINIMAP_COMPACT_PANE_W: f32 = 320.0;
pub(crate) const MINIMAP_MIN_PANE_W: f32 = 220.0;
pub(crate) const MINIMAP_SPLIT_MIN_PANE_W: f32 = 420.0;

pub(crate) fn should_show_minimap(pref_on: bool, split_chrome: bool, focused: bool, pane_w: f32) -> bool {
    if !pref_on || (split_chrome && !focused) {
        return false;
    }
    let min_w = if split_chrome {
        MINIMAP_SPLIT_MIN_PANE_W
    } else {
        MINIMAP_MIN_PANE_W
    };
    pane_w >= min_w
}

pub(crate) fn minimap_width_for_pane(pane_w: f32) -> f32 {
    if pane_w < MINIMAP_COMPACT_PANE_W {
        MINIMAP_COMPACT_W
    } else {
        MINIMAP_W
    }
}

#[no_mangle]
pub extern "C" fn mui_ed_draw(handle: i64, rows: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    // The inline-diff view owns the entire editor body region when active. Glyphs
    // are composited in a single pass after all rects, so the editor's body text
    // would otherwise show THROUGH the diff's opaque field background. Skip the
    // editor body draw entirely while the diff is up.
    if ctx.diff.is_active() {
        return;
    }
    // Keep the active tab's foldable ranges current with the live buffer. The
    // edit buffer lives shim-side (L28) and edits land between frames; recomputing
    // here (cheap linear scan, folds preserved where headers survive) means the
    // gutter chevrons + the visible↔source mapping always match what's on screen
    // without threading a recompute call through every Mighty edit site.
    ctx.tabs.recompute_active_fold();
    let count = ctx.panes.count();
    let region = layout::region(ctx.sidebar_visible);
    let win_w = visible_surface_size(ctx).0 as f32;
    if count <= 1 {
        // Unsplit: the full body span, the active tab, focused, no split chrome —
        // identical to the historical draw.
        let active = ctx.tabs.active();
        draw_editor_pane(ctx, handle, rows, region, win_w, active, true, false);
        return;
    }
    // Split: draw each pane into its column, then the dividers on top.
    let focused = ctx.panes.focused();
    for i in 0..count {
        let pr = layout::pane_region(region, win_w, count, i);
        let (_l, x_right) = layout::pane_bounds(region, win_w, count, i);
        // Markdown preview pane: render the rendered markdown of the OTHER pane's
        // `.md` buffer into this column instead of the editor body (L37/L38: no new
        // Mighty draw arm — `mui_ed_draw` already owns the per-pane loop).
        if ctx.md_pane == Some(i) && ctx.md_preview.is_open() {
            draw_md_preview_pane(ctx, pr, x_right, i, count);
            continue;
        }
        let tab = ctx.panes.tab_at(i).unwrap_or(0);
        draw_editor_pane(ctx, handle, rows, pr, x_right, tab, i == focused, true);
    }
    // 1px dividers between adjacent panes.
    {
        let win_h = ctx.gpu.height as f32;
        let div_top = region.top;
        let div_h = (win_h - 30.0 - div_top).max(0.0); // 30 = status bar
        for i in 0..count.saturating_sub(1) {
            let dx = layout::pane_divider_x(region, win_w, count, i);
            ctx.dl_rect(dx, div_top, layout::PANE_DIVIDER_W, div_h, theme::BORDER_SOFT());
        }
    }
}

/// Draw ONE editor pane (one tab) into `region` clipped to the right at
/// `x_right`. `focused` brightens the gutter / draws the caret as primary;
/// `split_chrome` adds the focus outline (only meaningful when there is more than
/// one pane). With a single full-width pane (`split_chrome == false`,
/// `x_right == window width`, `tab == active`) this reproduces the historical
/// `mui_ed_draw` body exactly.
#[allow(clippy::too_many_arguments)]
fn draw_editor_pane(
    ctx: &mut MuiContext,
    handle: i64,
    rows: i32,
    region: layout::Region,
    x_right: f32,
    tab_idx: usize,
    focused: bool,
    split_chrome: bool,
) {
    let handle_ptr = handle as usize as *mut MuiContext;
    let rows = rows.max(0) as usize;
    // The active file's detected language drives multi-language highlighting.
    let lang = ctx.language;
    // Glyph clip: the existing context clip when unsplit (byte-identical), else
    // this pane's column so a pane's text never bleeds across the divider.
    let clip = if split_chrome {
        let win_h = ctx.gpu.height as f32;
        let cy = region.top.max(0.0) as u32;
        let ch = (win_h - 30.0 - region.top).max(0.0) as u32; // above status bar
        let cx = region.left.max(0.0) as u32;
        let cw = (x_right - region.left).max(0.0) as u32;
        Some((cx, cy, cw, ch))
    } else {
        ctx.clip
    };

    // Snapshot what we need from the model (ends the borrow before text/rect).
    let snap = {
        let m = ctx
            .tabs
            .model_at(tab_idx)
            .unwrap_or_else(|| ctx.tabs.active_model());
        let fold = ctx
            .tabs
            .fold_at(tab_idx)
            .unwrap_or_else(|| ctx.tabs.active_fold());
        let total = m.line_count();
        let first = m.first_visible();
        let caret_n = m.caret_count();
        let carets: Vec<(usize, usize)> = (0..caret_n).filter_map(|i| m.caret_at(i)).collect();
        let selections: Vec<((usize, usize), (usize, usize))> =
            (0..caret_n).filter_map(|i| m.caret_selection(i)).collect();
        // FOLD-AWARE visible window: the next `rows` non-hidden source lines
        // starting at the scroll offset. Each painted line's screen row is its
        // index in this Vec. With no folds active this is exactly `first..last`.
        let vis_lines = fold.visible_lines_from(first, rows, total);
        let lines_for_view: Vec<(usize, String)> =
            vis_lines.iter().map(|&i| (i, m.line(i).to_string())).collect();
        let mut fold_marks = std::collections::HashMap::new();
        for &li in &vis_lines {
            if fold.is_foldable_start(li) {
                fold_marks.insert(
                    li,
                    FoldMark {
                        folded: fold.is_folded(li),
                        hidden: fold.hidden_count_at(li),
                    },
                );
            }
        }
        EdDrawSnapshot {
            total,
            first,
            cur_line: m.cursor_line(),
            cur_col: m.cursor_col(),
            sel: m.selection_range(),
            lines_for_view,
            carets,
            selections,
            fold_marks,
        }
    };
    let EdDrawSnapshot {
        total,
        first,
        cur_line,
        cur_col,
        sel,
        lines_for_view,
        carets,
        selections,
        fold_marks,
    } = snap;
    let _ = sel; // superseded by `selections` (still computed for back-compat).
    let _ = first; // superseded by the fold-aware `row_of` mapping below.

    // Source line -> painted screen row, for the visible window. With folding
    // this is NOT `li - first` (folded lines are skipped), so every per-line
    // draw (band/selection/caret/indent-guide/bracket) resolves its row here.
    let row_of: std::collections::HashMap<usize, i32> = lines_for_view
        .iter()
        .enumerate()
        .map(|(k, (li, _))| (*li, k as i32))
        .collect();
    // The last source line currently painted (for "is this on screen" tests that
    // previously used `first + rows`). When nothing is painted, treat as `first`.
    let last_src = lines_for_view.last().map(|(li, _)| *li).unwrap_or(first);

    let total_u64 = total.max(1) as u64;
    let text_x = layout::text_left_in(region, total_u64);
    let gutter_right = text_x - layout::GUTTER_GAP; // right edge for right-align
    let chrome = theme::CHROME_FONT_SIZE;
    // The pane's right edge: the full window width when unsplit, else this pane's
    // column right. Every right-anchored draw (field bg, minimap, current-line
    // band) clips to this so a pane never bleeds into its neighbor.
    let win_w = x_right;
    let win_h = ctx.gpu.height as f32;

    // 0) Editor field background (so the atmospheric glow doesn't wash the code).
    //    Spans from the body's left edge to the right, below the breadcrumb and
    //    above the status bar. Slightly translucent so a hint of glow remains.
    {
        let field_top = region.top;
        let field_h = (win_h - 30.0 - field_top).max(0.0); // 30 = status bar
        ctx.dl_rect(
            region.left,
            field_top,
            win_w - region.left,
            field_h,
            theme::BG_1(),
        );
    }

    // Minimap strip width (reserved on the right). Mockup `.minimap` ~76px. When
    // the minimap is disabled in Settings, reserve no strip (mm_w = 0) so the
    // current-line band + text run to the right edge. In a split, the minimap is
    // suppressed on UNFOCUSED panes and on narrow split columns where it would
    // cover source text instead of helping navigation.
    let pane_w = (x_right - region.left).max(0.0);
    let minimap_on = should_show_minimap(
        ctx.force_minimap_visible || crate::settings::minimap(),
        split_chrome,
        focused,
        pane_w,
    );
    let mm_w = if minimap_on { minimap_width_for_pane(pane_w) } else { 0.0_f32 };
    let mm_x = x_right - mm_w;
    let source_clip = Some((
        region.left.max(0.0) as u32,
        region.top.max(0.0) as u32,
        (mm_x - region.left).max(0.0) as u32,
        (win_h - 30.0 - region.top).max(0.0) as u32,
    ));

    // 1) Current-line highlight band (only when the cursor row is visible), with
    //    a soft indigo left→clear gradient glow + a 2px indigo left edge.
    //    Fold-aware: the row is resolved through `row_of` (folded lines skipped).
    if let Some(&row) = row_of.get(&cur_line) {
        let y = layout::row_y_in(region, row);
        let band_w = mm_x - region.left;
        // Nudge the band up 1px for optical centering on the glyph baseline, but
        // never above the editor field top — on row 0 that 1px would bleed into
        // the breadcrumb divider and show as a thin artifact at the very top.
        let band_top = (y - 1.0).max(region.top);
        let band_h = layout::LINE_H() - (band_top - (y - 1.0));
        ctx.dl_grad_h(region.left, band_top, band_w, band_h, 0.0, theme::accent_a(0.07), 0.6);
        ctx.dl_rect(region.left, band_top, 2.0, band_h, theme::ACCENT());
    }

    // 1b) Indent guides — faint vertical lines at each indent level inside the
    //     code body, the cursor block's level brightened. Drawn UNDER the text
    //     (after the band, before selections) so glyphs sit on top. Carries depth
    //     across blank lines from neighbors. Gated by the `indent_guides` pref.
    if crate::settings::indent_guides() {
        let tw = crate::settings::tab_width().max(1) as usize;
        // Scan the whole visible window plus a little context above/below so a
        // blank line at the window edge still carries its block's depth.
        let ctx_lines: Vec<String> = {
            let m = ctx
                .tabs
                .model_at(tab_idx)
                .unwrap_or_else(|| ctx.tabs.active_model());
            let lo = first.saturating_sub(4);
            let hi = (first + rows + 4).min(total);
            (lo..hi).map(|i| m.line(i).to_string()).collect()
        };
        let lo = first.saturating_sub(4);
        let refs: Vec<&str> = ctx_lines.iter().map(|s| s.as_str()).collect();
        let depths = crate::colorize::indent_depths(&refs, tw);
        let active = crate::colorize::active_indent_level(&refs, cur_line.saturating_sub(lo), tw);
        let guide_w = 1.0_f32;
        for (li, _line) in &lines_for_view {
            let idx = li.saturating_sub(lo);
            let Some(&cols) = depths.get(idx) else { continue };
            let levels = crate::colorize::guide_levels(cols, tw);
            if levels == 0 {
                continue;
            }
            let row = row_of.get(li).copied().unwrap_or(0);
            let y = layout::row_y_in(region, row);
            for lvl in 0..levels {
                let gx = text_x + (lvl * tw) as f32 * layout::CHAR_W();
                if gx >= mm_x {
                    break;
                }
                // The active rail (the cursor block's level) is brightened along
                // every line deep enough to contain it, so the whole "you are
                // here" column reads — not just the cursor row.
                let is_active = active == Some(lvl);
                let mut c = if is_active {
                    theme::accent_a(0.42)
                } else {
                    theme::accent_a(0.10)
                };
                if split_chrome && !focused {
                    c.a *= 0.5;
                }
                ctx.dl_rect(gx, y, guide_w, layout::LINE_H(), c);
            }
        }
    }

    // 2) Selection rects — one pass per caret's selection (multi-cursor). With a
    //    single caret this draws exactly the one primary selection as before.
    for ((l0, c0), (l1, c1)) in selections.iter().copied() {
        for (line_idx, line) in &lines_for_view {
            let li = *line_idx;
            if li < l0 || li > l1 {
                continue;
            }
            let line_chars = line.chars().count();
            let s = if li == l0 { c0 } else { 0 };
            // Extend one cell past EOL for multi-line selections to read as a
            // full-line highlight.
            let e = if li == l1 { c1 } else { line_chars + 1 };
            if e <= s {
                continue;
            }
            let row = row_of.get(&li).copied().unwrap_or(0);
            let x = layout::text_x_in(region, total_u64, s as i32);
            let w = (e - s) as f32 * layout::CHAR_W();
            let y = layout::row_y_in(region, row);
            unsafe {
                crate::mui_fill_rect(handle_ptr, x, y - 2.0, w, layout::LINE_H(), theme::SELECTION());
            }
        }
    }

    // 3) Gutter numbers + syntax-colored source text.
    for (row_idx, (line_idx, line)) in lines_for_view.iter().enumerate() {
        let li = *line_idx;
        let row = row_idx as i32;
        let y = layout::row_y_in(region, row);
        // Right-aligned gutter number; the cursor's line is brighter.
        let num = (li + 1).to_string();
        let num_w = gutter_number_width(&mut ctx.text, &num, chrome);
        let gx = (gutter_right - num_w).max(region.left + 2.0);
        let mut gcol = if li == cur_line {
            theme::GUTTER_ACTIVE()
        } else {
            theme::GUTTER()
        };
        // Split panes: dim an UNFOCUSED pane's gutter so the focused pane's
        // brighter active gutter reads as "where edits land". (No-op unsplit.)
        if split_chrome && !focused {
            gcol.a *= 0.55;
        }
        ctx.text.queue_sized(gx, y + 3.0, &num, gcol, chrome, clip);

        // Syntax spans for the line (language-aware).
        let spans = highlight_for(line, lang);
        if spans.is_empty() {
            // Nothing to draw (blank line) — still leave the band.
        } else {
            let chars: Vec<char> = line.chars().collect();
            let com_c = theme::SYN_COMMENT();
            let is_comment = |c: MuiColor| {
                (c.r - com_c.r).abs() < 0.004
                    && (c.g - com_c.g).abs() < 0.004
                    && (c.b - com_c.b).abs() < 0.004
            };
            for sp in spans {
                let frag: String = chars
                    .iter()
                    .skip(sp.start)
                    .take(sp.len)
                    .collect();
                if frag.trim().is_empty() {
                    continue;
                }
                let x = text_x + sp.start as f32 * layout::CHAR_W();
                // Comments render in the TRUE italic face (a tasteful editorial
                // touch); all other tokens stay in the regular code face.
                if is_comment(sp.color) {
                    ctx.text.queue_styled(
                        x,
                        y,
                        &frag,
                        sp.color,
                        theme::FONT_SIZE(),
                        crate::vello_ui::FontStyle::Italic,
                        source_clip,
                    );
                } else {
                    ctx.text.queue(x, y, &frag, sp.color, source_clip);
                }
            }
        }
    }

    // 3c) Fold gutter chevrons + the "⋯ N lines" folded indicator. A subtle
    //     ▾ (open) / ▸ (folded) glyph is drawn in the chevron band at the LEFT of
    //     the gutter next to every foldable region's header; a folded header also
    //     shows a faint pill "⋯ N lines" at the end of its text so the hidden span
    //     reads at a glance. Fold-state is per-tab (`fold_marks`, keyed by source
    //     line); rows resolve through the painted order so the marks land on the
    //     right screen rows even with nested folds active.
    if !fold_marks.is_empty() {
        for (row_idx, (line_idx, line)) in lines_for_view.iter().enumerate() {
            let Some(mark) = fold_marks.get(line_idx) else { continue };
            let row = row_idx as i32;
            let y = layout::row_y_in(region, row);
            // Chevron glyph in the left band of the gutter. Subtle by default,
            // brighter when folded (so a collapsed region stands out).
            let cev_x = region.left + 2.0;
            // Vector chevron (the embedded UI font lacks the ▸/▾ geometric glyphs,
            // which rendered as boxes): right when folded, down when open.
            let icon = if mark.folded { crate::icons::CHEVRON } else { crate::icons::CHEVRON_DOWN };
            let mut col = if mark.folded { theme::TEXT_3() } else { theme::GUTTER() };
            if split_chrome && !focused {
                col.a *= 0.6;
            }
            let icon_y = y + (layout::LINE_H() - 11.0) * 0.5;
            ctx.dl_icon(cev_x - 1.0, icon_y, 11.0, 11.0, icon, col, 1.6, false);

            // Folded indicator pill at the end of the header text: "⋯ N lines".
            if mark.folded {
                let end_col = line.chars().count();
                let px = text_x + (end_col as f32 + 1.0) * layout::CHAR_W();
                if px < mm_x - 40.0 {
                    let n = mark.hidden;
                    let label = if n == 1 {
                        "... 1 line".to_string()
                    } else {
                        format!("... {n} lines")
                    };
                    let pill_w = folded_indicator_width(&mut ctx.text, &label, chrome - 1.0);
                    let pill_h = layout::LINE_H() - 5.0;
                    let py = y - 1.0;
                    ctx.dl_round(px, py, pill_w.min(mm_x - px - 6.0), pill_h, 4.0, theme::accent_a(0.14));
                    let tcol = theme::TEXT_3();
                    ctx.text.queue_ui_sized(px + 6.0, py + (pill_h - (chrome - 1.0)) * 0.5, &label, tcol, chrome - 1.0, clip);
                }
            }
        }
    }

    // 3b) Bracket-pair colorization — re-draw each matched `()[]{}` glyph in a
    //     rainbow color by NESTING DEPTH, over-painting the punctuation glyph from
    //     step 3. Depth is tracked from the buffer start so brackets keep a stable
    //     color regardless of scroll; string/comment chars are masked out via the
    //     syntax spans. Unmatched/extra brackets get the error color. Gated by the
    //     `bracket_colors` pref (default ON). Lives alongside the cursor-adjacent
    //     bracket-match outline (step 4b).
    if crate::settings::bracket_colors() {
        let palette = crate::colorize::bracket_palette();
        let err_col = crate::colorize::bracket_error_color();
        // Scan from line 0 to the last visible line so depth is correct, but only
        // tag the visible window. Build the string/comment mask per line from its
        // syntax spans (a span colored as a string/comment masks its chars).
        // Scan from line 0 through the last PAINTED source line (folds can push
        // the painted window past `first + rows` source lines).
        let scan_hi = (last_src + 1).min(total);
        let scan_lines: Vec<String> = {
            let m = ctx
                .tabs
                .model_at(tab_idx)
                .unwrap_or_else(|| ctx.tabs.active_model());
            (0..scan_hi).map(|i| m.line(i).to_string()).collect()
        };
        let str_c = theme::SYN_STRING();
        let com_c = theme::SYN_COMMENT();
        let same = |a: MuiColor, b: MuiColor| {
            (a.r - b.r).abs() < 0.004 && (a.g - b.g).abs() < 0.004 && (a.b - b.b).abs() < 0.004
        };
        let line_refs: Vec<(usize, &str)> =
            scan_lines.iter().enumerate().map(|(i, s)| (i, s.as_str())).collect();
        let tags = crate::colorize::colorize_brackets(
            line_refs.iter().copied(),
            palette.len(),
            |line_no| {
                let line = &scan_lines[line_no];
                highlight_for(line, lang)
                    .iter()
                    .filter(|sp| same(sp.color, str_c) || same(sp.color, com_c))
                    .map(|sp| (sp.start, sp.len))
                    .collect()
            },
        );
        for t in tags {
            // Only re-paint brackets on PAINTED rows (fold-aware: a bracket on a
            // hidden line has no screen row).
            let Some(&row) = row_of.get(&t.line) else { continue };
            let line = &scan_lines[t.line];
            let Some(ch) = line.chars().nth(t.col) else { continue };
            let x = text_x + t.col as f32 * layout::CHAR_W();
            let y = layout::row_y_in(region, row);
            let mut c = if t.error { err_col } else { palette[t.color_index] };
            if split_chrome && !focused {
                c.a *= 0.7;
            }
            let mut s = [0u8; 4];
            ctx.text.queue(x, y, ch.encode_utf8(&mut s), c, clip);
        }
    }

    // 4) Carets — a 2px-wide indigo vertical bar with a soft glow behind each.
    //    The PRIMARY caret (carets[0]) is full-bright; secondary carets are drawn
    //    slightly dimmer so the primary stays distinguishable. With one caret this
    //    is identical to the historical single-caret draw (primary == cur_line).
    for (i, (cl, cc)) in carets.iter().copied().enumerate() {
        // Fold-aware: a caret on a hidden line has no painted row.
        let Some(&row) = row_of.get(&cl) else { continue };
        let cx = layout::text_x_in(region, total_u64, cc as i32);
        let cy = layout::row_y_in(region, row);
        // An UNFOCUSED split pane shows a faint, glow-less caret (it isn't where
        // typing lands). Unsplit panes are always focused -> unchanged.
        if split_chrome && !focused {
            if i == 0 {
                let mut bar = theme::ACCENT_BRIGHT();
                bar.a *= 0.35;
                ctx.dl_round(cx, cy - 1.0, 2.0, layout::LINE_H() - 2.0, 1.0, bar);
            }
            continue;
        }
        if i == 0 {
            ctx.dl_shadow(cx, cy + 1.0, 2.0, layout::LINE_H() - 6.0, 1.0, theme::ACCENT_GLOW(), 4.0);
            ctx.dl_round(cx, cy - 1.0, 2.0, layout::LINE_H() - 2.0, 1.0, theme::ACCENT_BRIGHT());
        } else {
            // Secondary caret: dimmer bar, lighter glow.
            let mut glow = theme::ACCENT_GLOW();
            glow.a *= 0.6;
            ctx.dl_shadow(cx, cy + 1.0, 2.0, layout::LINE_H() - 6.0, 1.0, glow, 3.0);
            let mut bar = theme::ACCENT_BRIGHT();
            bar.a *= 0.7;
            ctx.dl_round(cx, cy - 1.0, 2.0, layout::LINE_H() - 2.0, 1.0, bar);
        }
    }
    let _ = (cur_line, cur_col); // primary now drawn via the carets loop (i==0).

    // 4b) Bracket-match highlight — a thin outline box around the bracket the
    //     cursor is on/next to AND its depth-counted partner, when both are on
    //     visible rows. Subtle (1px accent stroke) so it reads as a pairing hint.
    {
        let pair = {
            let m = ctx
                .tabs
                .model_at(tab_idx)
                .unwrap_or_else(|| ctx.tabs.active_model());
            m.bracket_match().map(|(ml, mc)| {
                let (cl, cc) = bracket_source_cell(m);
                (cl as usize, cc as usize, ml, mc)
            })
        };
        if let Some((cl, cc, ml, mc)) = pair {
            let cw = layout::CHAR_W();
            for (li, co) in [(cl, cc), (ml, mc)] {
                if let Some(&row) = row_of.get(&li) {
                    let x = layout::text_x_in(region, total_u64, co as i32);
                    let y = layout::row_y_in(region, row);
                    ctx.dl_stroke(x - 1.0, y - 1.0, cw + 2.0, layout::LINE_H() - 2.0, 2.0, theme::ACCENT_LINE(), 1.0);
                }
            }
        }
    }

    // 5) Minimap — a faint right strip with one tiny colored bar per buffer line,
    //    sized by the line's first syntax span color + length, plus a clearer
    //    viewport rectangle over the currently-visible range. INTERACTIVE: the
    //    strip's geometry is stashed on the context so a click in the strip
    //    (`mui_ed_click`) jumps the editor to the matching source line. Tall files
    //    (more lines than fit) compress so the WHOLE file maps across the strip and
    //    a bottom click lands near EOF. Hidden when "Show Minimap" is off.
    //    `minimap_geom` is cleared on UNFOCUSED panes so clicks only hit the
    //    focused pane's strip.
    if minimap_on {
        let old_overlay = ctx.overlay;
        ctx.overlay = true;
        let field_top = region.top;
        let field_h = (win_h - 30.0 - field_top).max(0.0);
        // Left divider + a faint left→transparent shade.
        ctx.dl_rect(mm_x, field_top, 1.0, field_h, theme::BORDER_SOFT());
        ctx.dl_grad_h(mm_x, field_top, 24.0, field_h, 0.0, MuiColor::new(0.0, 0.0, 0.0, 0.18), 1.0);
        let mm_pad_x = mm_x + 10.0;
        let mm_inner_w = mm_w - 20.0;
        let mm_top = field_top + 10.0;
        let avail_h = (field_h - 20.0).max(0.0);
        // Per-line advance: 4px max, but compress to fit a tall file in the strip.
        let mm_line_h = if total > 0 {
            4.0_f32.min(avail_h / total as f32).max(0.5)
        } else {
            4.0
        };
        let shown_lines = total; // every line is represented (compressed if tall)
        let mm_lines: Vec<(usize, String)> = {
            let m = ctx
                .tabs
                .model_at(tab_idx)
                .unwrap_or_else(|| ctx.tabs.active_model());
            (0..shown_lines).map(|i| (i, m.line(i).to_string())).collect()
        };
        let bar_h = (mm_line_h - 1.5).clamp(1.0, 2.5);
        for (i, line) in &mm_lines {
            let yy = mm_top + (*i as f32) * mm_line_h;
            let trimmed_len = line.trim_start().chars().count();
            if trimmed_len == 0 {
                continue;
            }
            let indent = (line.chars().count() - trimmed_len) as f32;
            let spans = highlight_for(line, lang);
            let color = spans
                .iter()
                .find(|s| !line.chars().skip(s.start).take(s.len).collect::<String>().trim().is_empty())
                .map(|s| s.color)
                .unwrap_or(theme::DIM());
            // Bar length proportional to line length, clamped to the strip.
            let frac = ((trimmed_len as f32) / 48.0).min(1.0);
            let bx = mm_pad_x + (indent * 0.6).min(mm_inner_w * 0.4);
            let bw = (frac * mm_inner_w).max(2.0).min(mm_inner_w - (bx - mm_pad_x));
            let mut c = color;
            c.a = 0.55;
            ctx.dl_round(bx, yy, bw, bar_h, 1.0, c);
        }
        // Stash geometry for the click router (focused pane only). The vertical
        // field band is [field_top, field_top + field_h).
        let geom = crate::colorize::MinimapGeom {
            x: mm_x,
            w: mm_w,
            top: mm_top,
            line_h: mm_line_h,
            shown_lines,
            total,
        };
        if focused {
            ctx.minimap_geom = Some(geom);
        }
        // Viewport rectangle over the currently-visible range: a filled accent
        // wash + a brighter 1px border so the visible window reads clearly.
        let vp_y = mm_top + (first as f32) * mm_line_h;
        let vis = rows.min(total.saturating_sub(first)).max(1);
        let vp_h = (vis as f32 * mm_line_h).max(6.0);
        ctx.dl_round(mm_x + 4.0, vp_y - 1.0, mm_w - 8.0, vp_h + 2.0, 3.0, theme::accent_a(0.16));
        ctx.dl_stroke(mm_x + 4.0, vp_y - 1.0, mm_w - 8.0, vp_h + 2.0, 3.0, theme::ACCENT_LINE(), 1.2);
        ctx.overlay = old_overlay;
    } else if focused {
        ctx.minimap_geom = None;
    }

    // 6) Focus outline (split only): a subtle 2px accent stroke around the
    //    focused pane's column so it's clear where typing lands. Drawn last so it
    //    sits over the field/text. No-op unsplit (split_chrome == false).
    if split_chrome && focused {
        let outline_top = region.top;
        let outline_h = (win_h - 30.0 - outline_top).max(0.0);
        ctx.dl_stroke(
            region.left,
            outline_top,
            (x_right - region.left).max(0.0),
            outline_h,
            0.0,
            theme::ACCENT_LINE(),
            2.0,
        );
    }
    let _ = handle_ptr;
}

// ---------------------------------------------------------------------------
// Editor pane layout (side-by-side split) — see `crate::panes`.
// ---------------------------------------------------------------------------
//
// Panes are an ADDITIVE layer over the active-tab + per-tab `TextModel` state.
// The focused pane's tab IS the active tab, so every `mui_ed_*` op + every
// feature (completion/diag/nav/sticky/ghost/minimap) keeps operating on the
// focused pane with ZERO per-feature changes. With exactly one pane the layer is
// inert and the editor behaves byte-identically to before the split feature.

/// Re-bind the active tab + scroll to whatever pane `panes.focused()` now points
/// at: the CURRENT active model's scroll is stashed into the previously-focused
/// pane (done by the `panes` mutation that already saved it), then we switch the
/// tab store to the newly focused pane's tab and restore that pane's saved scroll
/// into its model, and resync the active file path. Call after any `panes`
/// mutation that may change the focused pane / its tab.
fn pane_rebind_focus(ctx: &mut MuiContext) {
    let f = ctx.panes.focused();
    let tab = ctx.panes.tab_at(f).unwrap_or_else(|| ctx.tabs.active());
    ctx.tabs.switch(tab);
    if let Some(scroll) = ctx.panes.scroll_at(f) {
        ctx.tabs.active_model_mut().set_first_visible(scroll);
    }
    sync_active_path(ctx);
}

/// The current live scroll of the active (focused) tab's model — saved into the
/// focused pane before a focus/split change so each pane keeps its own scroll.
#[inline]
fn active_scroll(ctx: &MuiContext) -> usize {
    ctx.tabs.active_model().first_visible()
}

/// Number of editor panes (>= 1). One means unsplit (identical to before).
#[no_mangle]
pub extern "C" fn mui_pane_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| c.panes.count() as i32)
}

/// The focused pane index (0-based).
#[no_mangle]
pub extern "C" fn mui_pane_focused(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.panes.focused() as i32)
}

/// The tab index shown in pane `i`, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_pane_tab(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }
        .and_then(|c| c.panes.tab_at(i as usize))
        .map_or(-1, |t| t as i32)
}

/// Set pane `i` to show tab `tab` (used when the tab bar opens a file into the
/// focused pane). If `i` is the focused pane, the active tab + scroll re-bind so
/// editing follows. Returns the focused tab index after the change.
#[no_mangle]
pub extern "C" fn mui_pane_set_tab(handle: i64, i: i32, tab: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i >= 0 && tab >= 0 {
        // Stash the focused pane's live scroll first so a focused retarget keeps
        // the other pane's position intact.
        let s = active_scroll(ctx);
        ctx.panes.save_focused_scroll(s);
        ctx.panes.set_tab(i as usize, tab as usize);
        if i as usize == ctx.panes.focused() {
            pane_rebind_focus(ctx);
        }
    }
    ctx.panes.focused_tab() as i32
}

/// Split the focused pane to the RIGHT, creating a second pane that shows the
/// SAME tab as the focused pane (so you immediately see two views of the file;
/// open a different file into it via the tab bar). Focuses the new pane. Caps at
/// 2 panes. Returns the new pane count.
#[no_mangle]
pub extern "C" fn mui_pane_split_right(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 1;
    };
    let before = ctx.panes.count();
    let cur_tab = ctx.panes.focused_tab();
    let s = active_scroll(ctx);
    ctx.panes.split_right(cur_tab, s);
    pane_rebind_focus(ctx);
    ctx.welcome.dismiss();
    if before > 1 {
        ctx.push_toast(crate::toast::Kind::Info, "Editor is already split");
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Split editor right");
    }
    ctx.panes.count() as i32
}

/// Cycle focus to the next pane (wraps); rebinds the active tab + restores the
/// newly focused pane's scroll. No-op with one pane. Returns the focused index.
#[no_mangle]
pub extern "C" fn mui_pane_focus_next(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.panes.count() <= 1 {
        ctx.push_toast(crate::toast::Kind::Info, "Only one editor pane");
        return ctx.panes.focused() as i32;
    }
    let s = active_scroll(ctx);
    ctx.panes.focus_next(s);
    pane_rebind_focus(ctx);
    ctx.push_toast(
        crate::toast::Kind::Info,
        format!("Focused editor pane {}", ctx.panes.focused() + 1),
    );
    ctx.panes.focused() as i32
}

/// Focus the pane the last mouse click landed in (for click→focus). Reads the
/// last click's pixel `x` (panes split into columns, so only x selects the pane),
/// rebinds the active tab + restores that pane's scroll. Returns the focused
/// index. The caller still positions the caret via `mui_ed_click` afterward (it
/// now resolves against the focused pane's column). No-op with one pane.
#[no_mangle]
pub extern "C" fn mui_pane_focus_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let count = ctx.panes.count();
    if count <= 1 {
        return 0;
    }
    let region = layout::region(ctx.sidebar_visible);
    let win_w = visible_surface_size(ctx).0 as f32;
    let target = layout::pane_at_x(region, win_w, count, ctx.last_event.x);
    if target != ctx.panes.focused() {
        let s = active_scroll(ctx);
        ctx.panes.focus(target, s);
        pane_rebind_focus(ctx);
    }
    ctx.panes.focused() as i32
}

/// Close the focused pane. If one remains, the layout returns to the unsplit
/// state (its tab/scroll restored). No-op with one pane. Returns the new count.
#[no_mangle]
pub extern "C" fn mui_pane_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 1;
    };
    if ctx.panes.count() <= 1 {
        ctx.push_toast(crate::toast::Kind::Info, "Only one editor pane");
        return ctx.panes.count() as i32;
    }
    let s = active_scroll(ctx);
    ctx.panes.save_focused_scroll(s);
    ctx.panes.close_focused();
    pane_rebind_focus(ctx);
    ctx.push_toast(crate::toast::Kind::Info, "Closed editor pane");
    ctx.panes.count() as i32
}

/// Pane `i`'s editor-column bounds in pixels, for the Mighty side / tests:
/// `mui_pane_region_left` / `_right`. With one pane these span the full editor
/// body (left edge .. window right).
#[no_mangle]
pub extern "C" fn mui_pane_region_left(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let region = layout::region(ctx.sidebar_visible);
    let win_w = ctx.gpu.width as f32;
    let (l, _r) = layout::pane_bounds(region, win_w, ctx.panes.count(), i.max(0) as usize);
    l as i32
}

#[no_mangle]
pub extern "C" fn mui_pane_region_right(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let region = layout::region(ctx.sidebar_visible);
    let win_w = ctx.gpu.width as f32;
    let (_l, r) = layout::pane_bounds(region, win_w, ctx.panes.count(), i.max(0) as usize);
    r as i32
}

/// Draw a single pane `i` into its column (the split render entry point). The
/// unsplit path uses `mui_ed_draw`, which already loops panes itself; this is
/// exposed so the Mighty side / a future per-pane drive can render one pane.
#[no_mangle]
pub extern "C" fn mui_pane_draw(handle: i64, i: i32, rows: i32) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if ctx.diff.is_active() {
        return;
    }
    let count = ctx.panes.count();
    let i = i.max(0) as usize;
    if i >= count {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let win_w = ctx.gpu.width as f32;
    let pr = layout::pane_region(region, win_w, count, i);
    let (_l, x_right) = layout::pane_bounds(region, win_w, count, i);
    let tab = ctx.panes.tab_at(i).unwrap_or(0);
    let focused = i == ctx.panes.focused();
    draw_editor_pane(ctx, handle, rows, pr, x_right, tab, focused, count > 1);
}

/// Dispatch a pane palette command (`CMD_SPLIT_RIGHT` / `CMD_FOCUS_NEXT_PANE` /
/// `CMD_CLOSE_PANE`) to the matching `mui_pane_*` op. Keeps the Mighty palette
/// ladder flat: all pane commands route through this one entry (the same pattern
/// `mui_git_dispatch` uses). Returns the resulting pane count.
#[no_mangle]
pub extern "C" fn mui_pane_dispatch(handle: i64, cmd: i32) -> i32 {
    let cmd = cmd as u32;
    // Only ids in the pane block route here (mirrors `mui_git_dispatch`'s gate);
    // anything else falls through to the resulting pane count unchanged.
    if !(crate::palette::CMD_PANE_FIRST..=crate::palette::CMD_PANE_LAST).contains(&cmd) {
        return mui_pane_count(handle);
    }
    if cmd == crate::palette::CMD_SPLIT_RIGHT {
        return mui_pane_split_right(handle);
    }
    if cmd == crate::palette::CMD_FOCUS_NEXT_PANE {
        let _ = mui_pane_focus_next(handle);
        return mui_pane_count(handle);
    }
    if cmd == crate::palette::CMD_CLOSE_PANE {
        return mui_pane_close(handle);
    }
    if cmd == crate::palette::CMD_MARKDOWN_PREVIEW {
        return mui_md_open(handle);
    }
    mui_pane_count(handle)
}

// ===========================================================================
// Live Markdown preview (split-pane rendered view of the active `.md` buffer)
// ===========================================================================

const MD_PREVIEW_MIN_READABLE_PANE_W: f32 = 220.0;

/// The tab index of the EDITOR pane that backs the preview pane `preview_i` (the
/// other pane in the split). Returns the source tab whose `.md` buffer is rendered.
fn md_source_tab(ctx: &MuiContext, preview_i: usize) -> usize {
    let count = ctx.panes.count();
    // The source is the other pane; with 2 panes that's `1 - preview_i`.
    let src = if count >= 2 { (preview_i + 1) % count } else { preview_i };
    ctx.panes.tab_at(src).unwrap_or_else(|| ctx.tabs.active())
}

/// Draw the markdown-preview body into pane `preview_i`'s column. The source text
/// is the live buffer of the editor pane beside it (re-parsed each frame so the
/// preview updates as you type). Used by [`mui_ed_draw`]'s split loop.
fn draw_md_preview_pane(
    ctx: &mut MuiContext,
    region: layout::Region,
    x_right: f32,
    preview_i: usize,
    _count: usize,
) {
    let src_tab = md_source_tab(ctx, preview_i);
    let source = ctx
        .tabs
        .model_at(src_tab)
        .map(|m| m.as_text())
        .unwrap_or_default();
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let win_w = visible_w as f32;
    let win_h = visible_h as f32;
    // Move the preview state out so we can borrow `ctx` mutably for drawing.
    let mut preview = std::mem::take(&mut ctx.md_preview);
    preview.draw(ctx, &source, region, x_right, win_w, win_h);
    ctx.md_preview = preview;
}

/// Open the Markdown preview: split the editor to the right (if not already) and
/// flag the right pane as the preview of the left pane's `.md` buffer. Idempotent
/// (re-opening just re-focuses / re-arms). Returns `1` on success, `0` if there is
/// no room to split. The preview re-renders live from the source buffer each frame.
#[no_mangle]
pub extern "C" fn mui_md_open(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.language != crate::langdetect::Language::Markdown {
        ctx.push_toast(crate::toast::Kind::Warn, "Markdown Preview is available for Markdown files");
        trace("md_open unavailable: non_markdown");
        return 0;
    }
    let (visible_w, _) = visible_surface_size(ctx);
    let body_w_with_sidebar = visible_w as f32 - layout::body_left(ctx.sidebar_visible);
    if ctx.sidebar_visible
        && body_w_with_sidebar / 2.0 < MD_PREVIEW_MIN_READABLE_PANE_W
        && !ctx.md_preview.is_open()
    {
        ctx.sidebar_visible = false;
        ctx.md_preview_hid_sidebar = true;
        trace("md_open compact: hide sidebar");
    }
    // Ensure a 2-pane split. The right pane becomes the preview; the left keeps
    // the editor on the source buffer. Reuse the existing pane machinery.
    if ctx.panes.count() < 2 {
        let cur_tab = ctx.panes.focused_tab();
        let s = active_scroll(ctx);
        ctx.panes.split_right(cur_tab, s);
        pane_rebind_focus(ctx);
        ctx.welcome.dismiss();
    }
    // The preview is the LAST (right) pane; keep editing focus on the left pane so
    // typing flows into the source buffer and the preview updates live.
    let preview_i = ctx.panes.count() - 1;
    ctx.md_pane = Some(preview_i);
    ctx.md_preview.open();
    // Focus the editor (left) pane so keystrokes edit the source.
    let s = active_scroll(ctx);
    ctx.panes.focus(0, s);
    pane_rebind_focus(ctx);
    trace("md_open");
    ctx.push_toast(crate::toast::Kind::Info, "Markdown preview opened");
    1
}

/// `1` if the markdown preview pane is currently open, else `0`.
#[no_mangle]
pub extern "C" fn mui_md_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.md_pane.is_some() && c.md_preview.is_open()))
}

/// Set the preview's SOURCE text explicitly (UTF-8 bytes at `ptr`/`len`). Normally
/// the preview reads the live `.md` buffer of the editor pane each frame, so this
/// is only needed for tests / headless rendering of a crafted sample. It seeds a
/// scratch buffer the preview parses; the next live frame supersedes it. Here it
/// simply parses + measures so callers can validate non-empty output. Returns the
/// parsed block count.
///
/// # Safety
/// `ptr` must point to `len` valid bytes (or be null with len 0).
#[no_mangle]
pub unsafe extern "C" fn mui_md_set_source(handle: i64, ptr: *const u8, len: usize) -> i32 {
    if (unsafe { ctx(handle) }).is_none() {
        return 0;
    }
    let src = if len == 0 || ptr.is_null() {
        String::new()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        String::from_utf8_lossy(slice).into_owned()
    };
    crate::markdown::parse(&src).len() as i32
}

/// Scroll the preview pane by `delta` lines (positive = down). Clamped to content.
#[no_mangle]
pub extern "C" fn mui_md_scroll(handle: i64, delta: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.md_preview.scroll_lines(delta);
    }
}

/// Hit-test the visible Markdown preview header close button. Returns `1` and
/// closes the preview when hit, else `0`.
#[no_mangle]
pub extern "C" fn mui_md_close_at_click(handle: i64) -> i32 {
    let hit = {
        let Some(ctx) = (unsafe { ctx(handle) }) else {
            return 0;
        };
        let Some(i) = ctx.md_pane else { return 0 };
        if !ctx.md_preview.is_open() || i >= ctx.panes.count() {
            return 0;
        }
        let region = layout::region(ctx.sidebar_visible);
        let win_w = visible_surface_size(ctx).0 as f32;
        let count = ctx.panes.count();
        let pr = layout::pane_region(region, win_w, count, i);
        let (_l, x_right) = layout::pane_bounds(region, win_w, count, i);
        let (x, y, w, h) = crate::mdpreview::close_rect(pr, x_right, win_w);
        let (px, py) = (ctx.last_event.x, ctx.last_event.y);
        px >= x && px <= x + w && py >= y && py <= y + h
    };
    if hit {
        let _ = mui_md_close(handle);
        1
    } else {
        0
    }
}

/// Draw the markdown preview into its pane column (the split render entry point,
/// mirroring `mui_pane_draw`). No-op when the preview is closed or not split.
/// `mui_ed_draw` already renders the preview inline in its pane loop, so this is
/// exposed for an explicit per-pane drive / tests.
#[no_mangle]
pub extern "C" fn mui_md_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(i) = ctx.md_pane else { return };
    if !ctx.md_preview.is_open() || i >= ctx.panes.count() {
        return;
    }
    let region = layout::region(ctx.sidebar_visible);
    let win_w = ctx.gpu.width as f32;
    let count = ctx.panes.count();
    let pr = layout::pane_region(region, win_w, count, i);
    let (_l, x_right) = layout::pane_bounds(region, win_w, count, i);
    draw_md_preview_pane(ctx, pr, x_right, i, count);
}

/// Close the markdown preview pane (collapse the split back to a single editor).
/// Returns `1` when it closed a preview, or `0` when already closed.
#[no_mangle]
pub extern "C" fn mui_md_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let was_open = ctx.md_preview.is_open() || ctx.md_pane.is_some();
    if !was_open {
        ctx.push_toast(crate::toast::Kind::Info, "Markdown preview is already closed");
        trace("md_close noop");
        return 0;
    }
    trace("md_close");
    ctx.md_preview.close();
    let restore_sidebar = ctx.md_preview_hid_sidebar;
    ctx.md_preview_hid_sidebar = false;
    // If the preview occupies the right pane, close that pane back to single.
    if let Some(i) = ctx.md_pane.take() {
        if ctx.panes.count() > 1 {
            let s = active_scroll(ctx);
            ctx.panes.save_focused_scroll(s);
            // Focus the preview pane, then close it (leaves the editor pane).
            ctx.panes.focus(i, s);
            ctx.panes.close_focused();
            pane_rebind_focus(ctx);
        }
    }
    if restore_sidebar {
        ctx.sidebar_visible = true;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Markdown preview closed");
    1
}

/// Launch-test hook: with `MUI_EDIT_PROBE` set, run a scripted insert, newline,
/// then backspace against the active model and log the resulting line count plus
/// a line's char length, proving the model mutates LIVE under native codegen
/// (where the old Mighty `Vec` buffer stayed empty, L28). The env value is the
/// text to type (default `hello`); the probe types it, inserts a newline, types
/// `world`, then backspaces once. No effect unless the var is set.
#[no_mangle]
pub extern "C" fn mui_edit_probe(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let Some(seed) = std::env::var_os("MUI_EDIT_PROBE") else {
        return;
    };
    let typed = seed.to_string_lossy();
    let typed = if typed.trim().is_empty() || typed == "1" {
        "hello".to_string()
    } else {
        typed.into_owned()
    };

    // Lock out the IDE's initial reload so the edited model is what renders.
    ctx.edit_probe_lock = true;

    let m = ctx.tabs.active_model_mut();
    let before_lines = m.line_count();
    // Move to end of document so the probe appends rather than splitting.
    let last = before_lines.saturating_sub(1);
    m.move_to(last as i32, m.line_len(last) as i32);
    for ch in typed.chars() {
        m.insert_char(ch);
    }
    let after_type_line = m.cursor_line();
    let after_type_len = m.line_len(after_type_line);
    m.newline();
    for ch in "world".chars() {
        m.insert_char(ch);
    }
    let nl_line = m.cursor_line();
    let nl_len_before_bs = m.line_len(nl_line);
    m.backspace();
    let nl_len_after_bs = m.line_len(nl_line);

    println!(
        "edit-probe: typed=\"{typed}\" lines {before_lines}->{} \
         typed_line_len={after_type_len} newline_line_len {nl_len_before_bs}->{nl_len_after_bs} \
         cursor=({},{}) dirty={}",
        m.line_count(),
        m.cursor_line(),
        m.cursor_col(),
        m.dirty()
    );

    // ---- power-feature probe: comment toggle, auto-close, auto-indent,
    //      duplicate, move-line, word-motion, bracket-match, in-file replace.
    //      Drives a fresh scratch model so the assertions are deterministic.
    {
        use crate::editor::TextModel;
        let p = ctx.tabs.active_model_mut();
        *p = TextModel::from_bytes(b"let x = 1\nlet y = 2");

        // 1) toggle comment on line 0.
        p.move_to(0, 0);
        p.toggle_line_comment();
        let commented = p.line(0).to_string();

        // 2) auto-close: type '(' -> "()".
        p.move_to(1, p.line_len(1) as i32);
        let smart_open = p.insert_char_smart('(');
        let autoclosed = p.line(1).to_string();

        // 3) auto-indent: after "{" Enter adds one level.
        let q = ctx.tabs.active_model_mut();
        *q = TextModel::from_bytes(b"fn f() {");
        q.move_to(0, 8);
        q.newline_auto_indent();
        let indent_len = q.line_len(1);

        // 4) duplicate the first line.
        let d = ctx.tabs.active_model_mut();
        *d = TextModel::from_bytes(b"dup_me");
        d.move_to(0, 0);
        d.duplicate();
        let dup_count = d.line_count();

        // 5) bracket match across the inserted pair.
        let b = ctx.tabs.active_model_mut();
        *b = TextModel::from_bytes(b"a(bc)d");
        b.move_to(0, 1);
        let bm = b.bracket_match();

        // 6) in-file replace all.
        let r = ctx.tabs.active_model_mut();
        *r = TextModel::from_bytes(b"x x x");
        let n_repl = r.replace_all("x", "yy");
        let replaced = r.line(0).to_string();

        // 7) word motion.
        let w = ctx.tabs.active_model_mut();
        *w = TextModel::from_bytes(b"alpha beta gamma");
        w.move_to(0, 0);
        w.move_word_right(false);
        let word_col = w.cursor_col();

        println!(
            "edit-probe[power]: comment=\"{commented}\" smart_open={smart_open} \
             autoclose=\"{autoclosed}\" indent_len={indent_len} dup_lines={dup_count} \
             bracket_match={bm:?} replace_all={n_repl} replaced=\"{replaced}\" \
             word_col={word_col}"
        );

        // Leave a representative buffer in place for the screenshot frame.
        let f = ctx.tabs.active_model_mut();
        *f = TextModel::from_bytes(
            b"fn main() {\n  // greet the world\n  let msg = greeting(\"world\")\n  print(msg)\n}",
        );
        f.move_to(0, 10);
    }
}

// ---- live-model undo / redo (shim-side snapshots; L28 workaround) ----

/// Cap the undo depth so a long session doesn't grow without bound.
const ED_UNDO_CAP: usize = 256;

/// Reset the editor undo/redo history (called on load / tab switch — history is
/// retained only as a compatibility hook; it no longer clears history. Real
/// load/reload paths clear the active tab stack.
#[no_mangle]
pub extern "C" fn mui_ed_undo_reset(handle: i64) {
    let _ = handle;
}

/// Push the CURRENT active model as an undo checkpoint (call before an edit
/// group). Clears the redo stack. Coalesces no-op duplicates.
#[no_mangle]
pub extern "C" fn mui_ed_undo_record(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let active = ctx.tabs.active();
        let Some(tab) = ctx.tabs.get_mut(active) else {
            return;
        };
        let snap = tab.model.clone();
        // Skip if identical to the most recent checkpoint.
        if let Some(last) = tab.undo.last() {
            if last.as_text() == snap.as_text() {
                return;
            }
        }
        tab.undo.push(snap);
        if tab.undo.len() > ED_UNDO_CAP {
            tab.undo.remove(0);
        }
        tab.redo.clear();
    }
}

/// Undo: restore the most recent checkpoint into the active model, pushing the
/// current state onto the redo stack. Returns `1` on success, `0` if nothing to
/// undo.
#[no_mangle]
pub extern "C" fn mui_ed_undo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        ctx.push_toast(crate::toast::Kind::Warn, "Undo is unavailable in read-only previews");
        return 0;
    }
    let active = ctx.tabs.active();
    let Some(tab) = ctx.tabs.get_mut(active) else {
        return 0;
    };
    match tab.undo.pop() {
        Some(prev) => {
            let current = tab.model.clone();
            tab.redo.push(current);
            tab.model = prev;
            refresh_active_dirty_from_saved(ctx);
            1
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "Nothing to undo");
            0
        }
    }
}

/// Redo: restore the most recent redo checkpoint, pushing the current state back
/// onto the undo stack. Returns `1` on success, `0` if nothing to redo.
#[no_mangle]
pub extern "C" fn mui_ed_redo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        ctx.push_toast(crate::toast::Kind::Warn, "Redo is unavailable in read-only previews");
        return 0;
    }
    let active = ctx.tabs.active();
    let Some(tab) = ctx.tabs.get_mut(active) else {
        return 0;
    };
    match tab.redo.pop() {
        Some(next) => {
            let current = tab.model.clone();
            tab.undo.push(current);
            tab.model = next;
            refresh_active_dirty_from_saved(ctx);
            1
        }
        None => {
            ctx.push_toast(crate::toast::Kind::Info, "Nothing to redo");
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Editor power-features (toggle comment, auto-indent, auto-close, bracket
// match, duplicate / move-line, word motion, select word/line, in-file
// replace) — all pure `TextModel` ops exposed as scalar `mui_ed_*` ABI.
// ---------------------------------------------------------------------------

// ---- Feature 1: toggle line comment (Ctrl+/) ----

/// Toggle a `// ` line comment on the cursor line or every selected line.
#[no_mangle]
pub extern "C" fn mui_ed_toggle_comment(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.toggle_line_comment())
}

/// `1` when Toggle Comment can mutate the active editor model.
/// Pure preflight: no toasts; the toggle command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_toggle_comment(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(!ctx.tabs.active_read_only())
}

/// Tab: insert configured spaces at a plain caret, or indent selected lines.
#[no_mangle]
pub extern "C" fn mui_ed_indent(handle: i64) -> i32 {
    apply_model_edit(handle, |m| {
        let _ = m.indent_or_insert_tab();
    })
}

/// Shift+Tab: outdent the current line or selected line range.
#[no_mangle]
pub extern "C" fn mui_ed_outdent(handle: i64) -> i32 {
    apply_model_edit(handle, |m| {
        let _ = m.outdent_lines();
    })
}

fn active_line_range(ctx: &MuiContext) -> Option<(usize, usize)> {
    let model = ctx.tabs.active_model();
    let line_count = model.line_count();
    if line_count == 0 {
        return None;
    }
    let (l0, l1) = model
        .selection_range()
        .map(|((start_line, _), (end_line, _))| (start_line, end_line))
        .unwrap_or_else(|| {
            let line = model.cursor_line();
            (line, line)
        });
    Some((l0.min(line_count - 1), l1.min(line_count - 1)))
}

/// `1` when Shift+Tab / Outdent can remove leading indentation.
/// Pure preflight: no toasts; the outdent command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_outdent(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let Some((l0, l1)) = active_line_range(ctx) else {
        return 0;
    };
    let model = ctx.tabs.active_model();
    for li in l0..=l1 {
        let line = model.line(li);
        if line.starts_with('\t') || line.starts_with(' ') {
            return 1;
        }
    }
    0
}

// ---- Feature 2: auto-indent on Enter ----

/// Insert a newline that copies the leading whitespace (and adds/removes one
/// indent level for `{` / `}`). The IDE routes Enter here instead of the plain
/// `mui_ed_newline`.
#[no_mangle]
pub extern "C" fn mui_ed_newline_indent(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.newline_auto_indent())
}

// ---- Feature 3: bracket / quote auto-close + skip-over + pair backspace ----

/// Smart char insert with bracket/quote auto-close + skip-over. Returns `1` if
/// smart handling applied (the IDE must NOT also insert the char), `0` to fall
/// back to a plain `mui_ed_insert_char`.
#[no_mangle]
pub extern "C" fn mui_ed_insert_smart(handle: i64, cp: i32) -> i32 {
    if let Some(m) = unsafe { model_mut(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            return i32::from(m.insert_char_smart(ch));
        }
    }
    0
}

/// Smart backspace that deletes a matching empty bracket/quote pair. Returns
/// `1` if a pair was removed, `0` to fall back to a plain `mui_ed_backspace`.
#[no_mangle]
pub extern "C" fn mui_ed_backspace_smart(handle: i64) -> i32 {
    if let Some(m) = unsafe { model_mut(handle) } {
        return i32::from(m.backspace_smart());
    }
    0
}

// ---- Feature 4: bracket match (renderer highlights both brackets) ----

/// `1` if the cursor is on/next to a bracket with a visible match, else `0`.
/// Caches the cursor-side bracket + its match for `mui_ed_bracket_*` readback.
#[no_mangle]
pub extern "C" fn mui_ed_bracket_match(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(ctx.tabs.active_model().bracket_match().is_some())
}

/// 0-based line of the cursor-side bracket being highlighted, or `-1`.
#[no_mangle]
pub extern "C" fn mui_ed_bracket_cur_line(handle: i64) -> i32 {
    bracket_field(handle, |c| c.0)
}

/// 0-based col of the cursor-side bracket being highlighted, or `-1`.
#[no_mangle]
pub extern "C" fn mui_ed_bracket_cur_col(handle: i64) -> i32 {
    bracket_field(handle, |c| c.1)
}

/// 0-based line of the MATCHING bracket, or `-1`.
#[no_mangle]
pub extern "C" fn mui_ed_bracket_match_line(handle: i64) -> i32 {
    bracket_field(handle, |c| c.2)
}

/// 0-based col of the MATCHING bracket, or `-1`.
#[no_mangle]
pub extern "C" fn mui_ed_bracket_match_col(handle: i64) -> i32 {
    bracket_field(handle, |c| c.3)
}

/// Resolve the cursor-side bracket cell + its match cell as `(cl,cc,ml,mc)` and
/// project a field; `-1` when there is no match. Recomputes per call (cheap).
fn bracket_field(handle: i64, f: impl Fn((i32, i32, i32, i32)) -> i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let m = ctx.tabs.active_model();
    let Some((ml, mc)) = m.bracket_match() else {
        return -1;
    };
    // Determine which cursor-side bracket produced the match (right then left).
    let (cl, cc) = bracket_source_cell(m);
    f((cl, cc, ml as i32, mc as i32))
}

/// The `(line, col)` of the bracket the cursor is highlighting — the char to
/// the right if it matches, else the char to the left.
fn bracket_source_cell(m: &TextModel) -> (i32, i32) {
    let line = m.cursor_line();
    let col = m.cursor_col();
    let is_bracket = |ch: Option<char>| matches!(ch, Some('(' | ')' | '[' | ']' | '{' | '}'));
    let right = m.line(line).chars().nth(col);
    if is_bracket(right) {
        // Confirm the right bracket is the one with a match.
        let mut probe = m.clone();
        probe.move_to(line as i32, col as i32);
        if probe.bracket_match().is_some() && is_bracket(right) {
            // bracket_match prefers the right char, so this is the source.
            return (line as i32, col as i32);
        }
    }
    (line as i32, (col as i32 - 1).max(0))
}

// ---- Feature 5: duplicate + move line ----

/// Duplicate the current line or selection (copy inserted below).
#[no_mangle]
pub extern "C" fn mui_ed_duplicate(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.duplicate())
}

/// `1` when Duplicate Line/Selection can mutate the active editor model.
/// Pure preflight: no toasts; Duplicate keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_duplicate(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(!ctx.tabs.active_read_only())
}

fn active_move_line_range(ctx: &MuiContext) -> Option<(usize, usize, usize)> {
    let model = ctx.tabs.active_model();
    let line_count = model.line_count();
    if line_count == 0 {
        return None;
    }
    let (l0, l1) = model
        .selection_range()
        .map(|((start_line, _), (end_line, _))| (start_line, end_line))
        .unwrap_or_else(|| {
            let line = model.cursor_line();
            (line, line)
        });
    Some((l0.min(line_count - 1), l1.min(line_count - 1), line_count))
}

/// `1` when moving the current/selected line range up can mutate the model.
/// Pure preflight: no toasts; the move command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_move_lines_up(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let Some((l0, _, _)) = active_move_line_range(ctx) else {
        return 0;
    };
    i32::from(l0 > 0)
}

/// `1` when moving the current/selected line range down can mutate the model.
/// Pure preflight: no toasts; the move command keeps read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_move_lines_down(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let Some((_, l1, line_count)) = active_move_line_range(ctx) else {
        return 0;
    };
    i32::from(l1 + 1 < line_count)
}

/// Move the current line / selected line range up by one.
#[no_mangle]
pub extern "C" fn mui_ed_move_lines_up(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.move_lines_up())
}

/// Move the current line / selected line range down by one.
#[no_mangle]
pub extern "C" fn mui_ed_move_lines_down(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.move_lines_down())
}

// ---- Feature 7: word motion + selection-extending motion + smart home ----

/// Extending/collapsing single-step motion: `dir` is a `DIR_*` constant,
/// `extend != 0` keeps/grows the selection (Shift held).
#[no_mangle]
pub extern "C" fn mui_ed_move_ext(handle: i64, dir: i32, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_cursor_ext(dir, extend != 0);
    }
}

/// Word-wise motion left/right; `extend != 0` grows the selection.
#[no_mangle]
pub extern "C" fn mui_ed_move_word(handle: i64, right: i32, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        if right != 0 {
            m.move_word_right(extend != 0);
        } else {
            m.move_word_left(extend != 0);
        }
    }
}

/// Smart Home (first-non-ws then col 0); `extend != 0` grows the selection.
#[no_mangle]
pub extern "C" fn mui_ed_home_smart(handle: i64, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.home_smart(extend != 0);
    }
}

/// Move to document start; `extend != 0` grows the selection.
#[no_mangle]
pub extern "C" fn mui_ed_document_start(handle: i64, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_document_start(extend != 0);
    }
}

/// Move to document end; `extend != 0` grows the selection.
#[no_mangle]
pub extern "C" fn mui_ed_document_end(handle: i64, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_document_end(extend != 0);
    }
}

/// Select the word under the cursor. Returns its char length.
#[no_mangle]
pub extern "C" fn mui_ed_select_word(handle: i64) -> i32 {
    if let Some(m) = unsafe { model_mut(handle) } {
        return m.select_word().chars().count() as i32;
    }
    0
}

/// Select the current line in the active document. Pure motion; does not mark dirty.
#[no_mangle]
pub extern "C" fn mui_ed_select_line(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.select_line();
    }
}

/// Select the entire active document. Pure motion; does not mark dirty.
#[no_mangle]
pub extern "C" fn mui_ed_select_all(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.select_all();
    }
}

/// Copy the active selection, or the current line when there is no selection.
#[no_mangle]
pub extern "C" fn mui_ed_copy(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (text, is_selection) = {
        let model = ctx.tabs.active_model();
        let selected = model.selected_text();
        if selected.is_empty() {
            (model.current_line_text_for_clipboard(), false)
        } else {
            (selected, true)
        }
    };
    if text.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "No text to copy");
        return 0;
    }
    match write_clipboard_text(&text) {
        Ok(()) => {
            ctx.push_toast(
                crate::toast::Kind::Success,
                if is_selection { "Copied selection" } else { "Copied line" },
            );
            1
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                clipboard_write_failure_message("copy", &e),
            );
            println!("editor-copy: failed: {e}");
            0
        }
    }
}

/// Cut the active selection, or the current line when there is no selection.
#[no_mangle]
pub extern "C" fn mui_ed_cut(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return reject_read_only_edit(ctx);
    }
    let (text, is_selection) = {
        let model = ctx.tabs.active_model();
        let selected = model.selected_text();
        if selected.is_empty() {
            (model.current_line_text_for_clipboard(), false)
        } else {
            (selected, true)
        }
    };
    if text.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Nothing to cut");
        return 0;
    }
    match write_clipboard_text(&text) {
        Ok(()) => {
            let changed = if is_selection {
                ctx.tabs.active_model_mut().delete_selection()
            } else {
                ctx.tabs.active_model_mut().delete_current_line()
            };
            if changed {
                ctx.push_toast(
                    crate::toast::Kind::Success,
                    if is_selection { "Cut selection" } else { "Cut line" },
                );
                1
            } else {
                ctx.push_toast(crate::toast::Kind::Info, "Nothing to cut");
                0
            }
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Error,
                clipboard_write_failure_message("cut", &e),
            );
            println!("editor-cut: failed: {e}");
            0
        }
    }
}

/// `1` when Cut can remove a selection or current line from the active model.
/// Pure preflight: no clipboard access and no toasts; Cut keeps user feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_cut(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    let model = ctx.tabs.active_model();
    if !model.selected_text().is_empty() {
        return 1;
    }
    i32::from(model.line_count() > 1 || !model.current_line_text_for_clipboard().is_empty())
}

/// Paste operating-system clipboard text at the primary caret.
#[no_mangle]
pub extern "C" fn mui_ed_paste(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return reject_read_only_edit(ctx);
    }
    let text = match read_clipboard_text() {
        Ok(text) => text,
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Error, clipboard_failure_message(&e));
            println!("editor-paste: failed to read clipboard: {e}");
            return 0;
        }
    };
    if text.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Clipboard is empty");
        return 0;
    }
    if ctx.tabs.active_model_mut().insert_text(&text) {
        ctx.push_toast(crate::toast::Kind::Success, "Pasted clipboard");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Clipboard is empty");
        0
    }
}

/// `1` when Paste can insert non-empty clipboard text into the active model.
/// Pure preflight: no toasts; Paste keeps clipboard/read-only feedback.
#[no_mangle]
pub extern "C" fn mui_ed_can_paste(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.tabs.active_read_only() {
        return 0;
    }
    match read_clipboard_text() {
        Ok(text) => i32::from(!text.is_empty()),
        Err(_) => 0,
    }
}

/// `1` if the active model has a non-empty selection, else `0`.
#[no_mangle]
pub extern "C" fn mui_ed_has_selection(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.tabs.active_model().has_selection()))
}

// ---------------------------------------------------------------------------
// Multi-cursor (multiple simultaneous carets / selections)
// ---------------------------------------------------------------------------
//
// The active model holds a list of carets with caret[0] = PRIMARY. Every
// existing `mui_ed_*` edit/motion op above now implicitly applies at ALL carets
// via the model's `*_multi` methods (the IDE routes edits through the `_multi`
// entry points below). With exactly one caret each op is byte-identical to the
// legacy single-cursor behavior, so all pre-existing tests/accessors are
// unaffected. Features that read the cursor (completion / diagnostics / nav /
// hover / sticky scroll) keep using the PRIMARY caret via `mui_ed_cursor_*`.

/// Number of carets in the active model (>= 1).
#[no_mangle]
pub extern "C" fn mui_ed_caret_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(1, |c| c.tabs.active_model().caret_count() as i32)
}

/// 0-based line of caret `i` (0 = primary), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_ed_caret_n_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }
        .and_then(|c| c.tabs.active_model().caret_at(i as usize))
        .map_or(-1, |(l, _)| l as i32)
}

/// 0-based col of caret `i` (0 = primary), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_ed_caret_n_col(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }
        .and_then(|c| c.tabs.active_model().caret_at(i as usize))
        .map_or(-1, |(_, c)| c as i32)
}

/// Ctrl+D: select the word at the primary caret (first press), or add a caret on
/// the next occurrence of the current selection (wrapping). Returns `1` if a
/// word was selected or a caret added, else `0` (no word / no other match).
#[no_mangle]
pub extern "C" fn mui_ed_add_caret_next(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        trace("multi_cursor add_next ok=0 count=0");
        return 0;
    };
    let m = ctx.tabs.active_model_mut();
    let ok = m.add_caret_next_occurrence();
    let count = m.caret_count();
    trace(&format!("multi_cursor add_next ok={} count={count}", i32::from(ok)));
    if ok {
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "No word or next occurrence for multi-cursor");
    0
}

/// Ctrl+Alt+Up: add a column-block caret on the line above the primary caret.
/// Returns `1` if added, `0` at the top edge.
#[no_mangle]
pub extern "C" fn mui_ed_add_caret_above(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        trace("multi_cursor add_above ok=0 count=0");
        return 0;
    };
    let m = ctx.tabs.active_model_mut();
    let ok = m.add_caret_vertical(-1);
    let count = m.caret_count();
    trace(&format!("multi_cursor add_above ok={} count={count}", i32::from(ok)));
    if ok {
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "No line above for another caret");
    0
}

/// Ctrl+Alt+Down: add a column-block caret on the line below the primary caret.
/// Returns `1` if added, `0` at the bottom edge.
#[no_mangle]
pub extern "C" fn mui_ed_add_caret_below(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        trace("multi_cursor add_below ok=0 count=0");
        return 0;
    };
    let m = ctx.tabs.active_model_mut();
    let ok = m.add_caret_vertical(1);
    let count = m.caret_count();
    trace(&format!("multi_cursor add_below ok={} count={count}", i32::from(ok)));
    if ok {
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "No line below for another caret");
    0
}

/// Esc: collapse to the primary caret only and clear its selection.
#[no_mangle]
pub extern "C" fn mui_ed_collapse_carets(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.collapse_carets();
        trace(&format!("multi_cursor collapse count={}", m.caret_count()));
    }
}

/// Alt+Click: toggle a caret at the last click's `(line, col)`.
#[no_mangle]
pub extern "C" fn mui_ed_toggle_caret_click(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let total = ctx.tabs.active_model().line_count() as u64;
    let first = ctx.tabs.active_model().first_visible() as u64;
    let (line, col) =
        layout::pixel_to_cell_in(region, ctx.last_event.x, ctx.last_event.y, first, total);
    ctx.tabs.active_model_mut().toggle_caret_at(line as i32, col as i32);
    let count = ctx.tabs.active_model().caret_count();
    trace(&format!("multi_cursor toggle_click line={line} col={col} count={count}"));
}

// ---- multi-caret edit / motion entry points (apply at EVERY caret) ----

/// Insert one scalar at every caret (a `\n` codepoint splits at each).
#[no_mangle]
pub extern "C" fn mui_ed_insert_char_multi(handle: i64, cp: i32) -> i32 {
    if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
        return apply_model_edit(handle, |m| m.insert_char_multi(ch));
    }
    0
}

/// Smart insert (auto-close/skip-over) at every caret, falling back to a plain
/// insert where the smart path declined. Replaces the Mighty-side
/// smart/plain branch when multiple carets are active.
#[no_mangle]
pub extern "C" fn mui_ed_insert_smart_multi(handle: i64, cp: i32) -> i32 {
    trace(&format!("ed_insert_smart_multi cp={cp}"));
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if c.tabs.active_read_only() {
        return reject_read_only_edit(c);
    }
    let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) else {
        return 0;
    };
    let before = c.tabs.active_model().as_text();
    c.tabs.active_model_mut().insert_char_smart_multi(ch);
    let changed = c.tabs.active_model().as_text() != before;
    if changed && c.snippet_session.is_active() {
        let session = &mut c.snippet_session;
        let model = c.tabs.active_model_mut();
        session.sync_mirrors_from_current(model);
    }
    i32::from(changed)
}

/// Backspace at every caret (smart pair-delete where applicable).
#[no_mangle]
pub extern "C" fn mui_ed_backspace_multi(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.backspace_multi())
}

/// Delete-forward at every caret.
#[no_mangle]
pub extern "C" fn mui_ed_delete_multi(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.delete_multi())
}

/// Delete previous word at every caret.
#[no_mangle]
pub extern "C" fn mui_ed_delete_word_left_multi(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.delete_word_left_multi())
}

/// Delete next word at every caret.
#[no_mangle]
pub extern "C" fn mui_ed_delete_word_right_multi(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.delete_word_right_multi())
}

/// Newline + auto-indent at every caret.
#[no_mangle]
pub extern "C" fn mui_ed_newline_indent_multi(handle: i64) -> i32 {
    apply_model_edit(handle, |m| m.newline_indent_multi())
}

/// Single-step motion at every caret; `extend != 0` grows each selection.
#[no_mangle]
pub extern "C" fn mui_ed_move_ext_multi(handle: i64, dir: i32, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_ext_multi(dir, extend != 0);
    }
}

/// Word motion at every caret; `right != 0` moves right, `extend != 0` grows.
#[no_mangle]
pub extern "C" fn mui_ed_move_word_multi(handle: i64, right: i32, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_word_multi(right != 0, extend != 0);
    }
}

/// Smart-home at every caret; `extend != 0` grows each selection.
#[no_mangle]
pub extern "C" fn mui_ed_home_smart_multi(handle: i64, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.home_smart_multi(extend != 0);
    }
}

/// Move every caret to document start; `extend != 0` grows each selection.
#[no_mangle]
pub extern "C" fn mui_ed_document_start_multi(handle: i64, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_document_start_multi(extend != 0);
    }
}

/// Move every caret to document end; `extend != 0` grows each selection.
#[no_mangle]
pub extern "C" fn mui_ed_document_end_multi(handle: i64, extend: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_document_end_multi(extend != 0);
    }
}

// ---------------------------------------------------------------------------
// Feature 6 — in-file find/replace bar (Ctrl+H)
// ---------------------------------------------------------------------------

/// Open the in-file replace bar, seeding the find field from the current find
/// prompt query (if any) or the selected word.
#[no_mangle]
pub extern "C" fn mui_replace_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        // Seed: prefer the existing find query, else the word under the cursor.
        let mut seed = ctx.prompt.query_string();
        if seed.is_empty() {
            seed = ctx.tabs.active_model_mut().select_word();
        }
        ctx.replace_bar.open(&seed);
    }
}

/// `1` if the replace bar is active.
#[no_mangle]
pub extern "C" fn mui_replace_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.replace_bar.is_active()))
}

/// Type a codepoint into the focused field.
#[no_mangle]
pub extern "C" fn mui_replace_push(handle: i64, cp: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if cp >= 0 {
            ctx.replace_bar.push(cp as u32);
        }
    }
}

/// Backspace the focused field.
#[no_mangle]
pub extern "C" fn mui_replace_backspace(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.replace_bar.backspace();
    }
}

/// Toggle focus between the find and replace fields (Tab). Returns `1` when the
/// replace field is now focused, else `0`.
#[no_mangle]
pub extern "C" fn mui_replace_toggle_focus(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.replace_bar.toggle_focus())
}

/// `1` if the replace field currently has focus.
#[no_mangle]
pub extern "C" fn mui_replace_focus(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.replace_bar.replace_focus())
}

/// Close the replace bar (clears its fields). Returns `1` when an active bar
/// was closed and `0` when there was nothing to close.
#[no_mangle]
pub extern "C" fn mui_replace_cancel(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.replace_bar.is_active() {
        ctx.replace_bar.cancel();
        ctx.push_toast(crate::toast::Kind::Info, "Find & Replace closed");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No Find & Replace bar open");
        0
    }
}

/// `1` when the latest mouse-down hit the replace bar's close button.
#[no_mangle]
pub extern "C" fn mui_replace_close_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.replace_bar.is_active()
        || ctx.last_event.tag != crate::ffi::MUI_EVENT_MOUSE_DOWN
        || ctx.last_event.button != crate::ffi::MUI_MOUSE_LEFT
    {
        return 0;
    }
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let w = visible_w as f32;
    let h = visible_h as f32;
    let bar_h = layout::LINE_H();
    let top = (h - 30.0 - 2.0 * bar_h).max(0.0);
    let (cx, cy, cw, ch) = replace_close_rect(w, top, bar_h);
    let px = ctx.last_event.x;
    let py = ctx.last_event.y;
    i32::from(px >= cx && px <= cx + cw && py >= cy && py <= cy + ch)
}

fn replace_can_current(ctx: &MuiContext) -> bool {
    let needle = ctx.replace_bar.find_string();
    !needle.is_empty()
        && !ctx.tabs.active_read_only()
        && ctx.tabs.active_model().as_text().contains(&needle)
}

/// Silent preflight for replace-next. Returns `1` only when Enter can mutate.
#[no_mangle]
pub extern "C" fn mui_replace_can_next(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(replace_can_current(c)))
}

/// Silent preflight for replace-all. Returns `1` only when Enter can mutate.
#[no_mangle]
pub extern "C" fn mui_replace_can_all(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(replace_can_current(c)))
}

/// Replace the next occurrence (at/after the cursor, wrapping) of the find
/// field with the replace field, in the active model. Returns `1` if a
/// replacement was made, else `0`.
#[no_mangle]
pub extern "C" fn mui_replace_next(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let needle = ctx.replace_bar.find_string();
    if needle.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Enter text to replace");
        return 0;
    }
    if ctx.tabs.active_read_only() {
        ctx.push_toast(crate::toast::Kind::Warn, "Replace is unavailable in read-only previews");
        return 0;
    }
    let repl = ctx.replace_bar.repl_string();
    if ctx.tabs.active_model_mut().replace_next(&needle, &repl) {
        ctx.push_toast(crate::toast::Kind::Success, "Replaced 1 occurrence");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No matches to replace");
        0
    }
}

/// Replace ALL occurrences of the find field with the replace field in the
/// active model. Returns the replacement count.
#[no_mangle]
pub extern "C" fn mui_replace_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let needle = ctx.replace_bar.find_string();
    if needle.is_empty() {
        ctx.push_toast(crate::toast::Kind::Info, "Enter text to replace");
        return 0;
    }
    if ctx.tabs.active_read_only() {
        ctx.push_toast(crate::toast::Kind::Warn, "Replace is unavailable in read-only previews");
        return 0;
    }
    let repl = ctx.replace_bar.repl_string();
    let n = ctx.tabs.active_model_mut().replace_all(&needle, &repl) as i32;
    if n > 0 {
        ctx.push_toast(
            crate::toast::Kind::Success,
            format!(
                "Replaced {n} {}",
                if n == 1 { "occurrence" } else { "occurrences" }
            ),
        );
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "No matches to replace");
    }
    n
}

/// Draw the in-file replace bar: two stacked input rows (find + replace) as a
/// band above the status bar, the focused field marked. No-op when inactive.
#[no_mangle]
pub extern "C" fn mui_replace_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.replace_bar.is_active() {
        return;
    }
    let (visible_w, visible_h) = visible_surface_size(ctx);
    let w = visible_w as f32;
    let h = visible_h as f32;
    let bar_h = layout::LINE_H();
    // Two rows above the 30px status bar.
    let top = (h - 30.0 - 2.0 * bar_h).max(0.0);
    let chrome = theme::CHROME_FONT_SIZE;
    let clip = ctx.clip;
    let left = layout::region(ctx.sidebar_visible).left;
    let text_x = left + layout::PAD + 12.0;
    let mut find_line = ctx.replace_bar.display_find();
    let mut repl_line = ctx.replace_bar.display_replace();
    let repl_focus = ctx.replace_bar.replace_focus() == 1;
    let (close_x, close_y, close_w, close_h) = replace_close_rect(w, top, bar_h);
    let hint = "Tab fields / Enter";
    let (hint_w, _) = ctx.text.measure_ui_sized(hint, 11.0);
    let hint_x = close_x - hint_w - 12.0;
    let show_hint = hint_x > text_x + 220.0;
    let max_right = if show_hint { hint_x - 14.0 } else { close_x - 10.0 };
    let max_line_w = (max_right - text_x).max(0.0);
    find_line = fit_prompt_tail(&mut ctx.text, &find_line, max_line_w, chrome);
    repl_line = fit_prompt_tail(&mut ctx.text, &repl_line, max_line_w, chrome);

    let handle_ptr = handle as usize as *mut MuiContext;
    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    unsafe {
        // Elevated two-row band + top divider + ember accent edge.
        crate::mui_fill_rect(handle_ptr, 0.0, top, w, 2.0 * bar_h, theme::ELEVATED());
        crate::mui_fill_rect(handle_ptr, 0.0, top, w, 1.0, theme::BORDER());
        crate::mui_fill_rect(handle_ptr, left, top, 3.0, 2.0 * bar_h, theme::EMBER());
    }
    // Focus highlight behind the active row.
    let focus_y = if repl_focus { top + bar_h } else { top };
    ctx.dl_rect(left + 3.0, focus_y, w - left - 3.0, bar_h, theme::accent_a(0.08));

    let fy = top + (bar_h - chrome) * 0.5 - 1.0;
    let ry = top + bar_h + (bar_h - chrome) * 0.5 - 1.0;
    if show_hint {
        ctx.text.queue_sized(hint_x, fy + 1.0, hint, theme::TEXT_3(), 11.0, clip);
    }
    ctx.dl_round(close_x, close_y, close_w, close_h, 6.0, theme::BG_4());
    ctx.dl_icon(
        close_x + 5.0,
        close_y + 5.0,
        close_w - 10.0,
        close_h - 10.0,
        crate::icons::CLOSE,
        theme::TEXT_1(),
        1.6,
        false,
    );
    ctx.text.queue_sized(text_x, fy, &find_line, theme::TEXT(), chrome, clip);
    ctx.text.queue_sized(text_x, ry, &repl_line, theme::TEXT(), chrome, clip);
    ctx.text.set_overlay(false);
    ctx.overlay = was_overlay;
}

fn replace_close_rect(w: f32, top: f32, bar_h: f32) -> (f32, f32, f32, f32) {
    let size = (bar_h - 6.0).clamp(18.0, 24.0);
    let x = (w - size - 8.0).max(0.0);
    (x, top + 4.0, size, size)
}

// ===========================================================================
// Welcome / first-impression screen
// ===========================================================================

/// `true` when the Welcome screen should occupy the editor body: either it was
/// forced open from the palette ("Welcome"), or no real file is open — the
/// active tab has no path AND its buffer is empty (a fresh scratch). The Mighty
/// side calls this each frame and, when set, draws the Welcome instead of the
/// editor body and routes clicks through [`mui_welcome_click`].
#[no_mangle]
pub extern "C" fn mui_welcome_active(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    i32::from(welcome_is_active(ctx))
}

fn welcome_is_active(ctx: &MuiContext) -> bool {
    if ctx.welcome.force_open {
        return true;
    }
    // "No file open": the active tab has no path and the buffer is empty.
    let no_path = ctx.tabs.active_path().is_none();
    let model = ctx.tabs.active_model();
    let empty = model.line_count() <= 1 && model.line_len(0) == 0;
    no_path && empty && !ctx.welcome.hides_empty_auto()
}

/// Force the Welcome screen open (the palette "Welcome" command).
#[no_mangle]
pub extern "C" fn mui_welcome_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.welcome.open();
    }
}

/// Force the focused Open Recent chooser open over the editor body.
#[no_mangle]
pub extern "C" fn mui_welcome_open_recent_picker(handle: i64) {
    trace("welcome_recent_picker_open");
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.welcome.open_recent_picker();
    }
}

/// Dismiss the forced Welcome screen (called after opening a file from it).
#[no_mangle]
pub extern "C" fn mui_welcome_dismiss(handle: i64) {
    trace("welcome_dismiss");
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.welcome.dismiss();
    }
}

/// Close the visible Welcome surface. Unlike `mui_welcome_dismiss`, this is the
/// explicit command/close-affordance path, so it hides both forced Welcome and
/// the automatic empty-buffer Welcome state and reports whether anything closed.
#[no_mangle]
pub extern "C" fn mui_welcome_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if welcome_is_active(ctx) {
        ctx.welcome.dismiss_empty_auto();
        ctx.push_toast(crate::toast::Kind::Info, "Welcome closed");
        trace("welcome_close");
        1
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Welcome is already closed");
        trace("welcome_close noop");
        0
    }
}

/// Draw the Welcome screen over the editor body region. No-op work is fine to
/// call unconditionally; the Mighty side only calls it when `mui_welcome_active`.
#[no_mangle]
pub extern "C" fn mui_welcome_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    prune_missing_recent_files(ctx);
    prune_missing_recent_workspaces(ctx);
    let region = layout::region(ctx.sidebar_visible);
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let visible_h = visible_surface_size(ctx).1;
    let welcome_h = if ctx.bottom_dock_open() {
        layout::term_panel_top(visible_h).floor().max(region.top + 1.0) as u32
    } else {
        h
    };
    // Take the recents snapshot out so we can borrow `ctx` mutably for the draw
    // (the MRU lives in the Quick-Open engine).
    let recents: Vec<std::path::PathBuf> = ctx.quickopen.recent_paths();
    let folders: Vec<std::path::PathBuf> = ctx.recent_workspaces.entries().to_vec();
    let mut welcome = std::mem::take(&mut ctx.welcome);
    welcome.draw(ctx, region.left, region.top, w, welcome_h, &recents, &folders);
    ctx.welcome = welcome;
}

/// Hit-test the LAST-POLLED mouse-down position against the Welcome layout
/// (mirrors the other `*_at_click` ABI fns, which read `ctx.last_event`).
/// Returns the action id (see `welcome.rs` `ACTION_*`), or -1 for none. For a
/// recents row the id is `ACTION_RECENT_BASE + index`; the chosen file is then
/// opened by [`mui_welcome_open_recent`].
#[no_mangle]
pub extern "C" fn mui_welcome_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return crate::welcome::ACTION_NONE;
    };
    let a = ctx.welcome.click(ctx.last_event.x, ctx.last_event.y);
    trace(&format!("welcome_click x={:.1} y={:.1} -> {a}", ctx.last_event.x, ctx.last_event.y));
    a
}

/// Open a Welcome recents row (`i = action - ACTION_RECENT_BASE`) as a new tab.
/// Returns the resulting active tab index, or -1 if the row/path is invalid.
#[no_mangle]
pub extern "C" fn mui_welcome_open_recent(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No recent file row selected");
        return -1;
    }
    let Some(path) = ctx.welcome.recent_path(i as usize).cloned() else {
        ctx.push_toast(crate::toast::Kind::Info, "Recent file row no longer listed");
        return -1;
    };
    if !path.is_file() {
        let removed = ctx.quickopen.remove_recent_path(&path);
        if removed {
            persist_recent_files(ctx);
        }
        ctx.welcome.clear_recent_file_hits();
        refresh_workspace_file_views(ctx);
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Recent file missing: {}", basename(&path)),
        );
        return -1;
    }
    let idx = ctx.tabs.open_path(path.clone());
    ctx.welcome.dismiss();
    sync_active_path(ctx);
    record_opened_file(ctx, &path);
    idx as i32
}

/// Open a Welcome RECENT-FOLDER row (`i = action - ACTION_RECENT_FOLDER_BASE`) as
/// the workspace (re-rooting the tree/index/search/git/agents). Returns `1` on
/// success, `0` if the row/path is invalid. Invalid selections report the same
/// feedback as the workspace-recents ABI; stale folders are handled by the
/// shared recent-folder opener.
#[no_mangle]
pub extern "C" fn mui_welcome_open_folder(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No recent folder row selected");
        return 0;
    }
    let Some(path) = ctx.welcome.recent_folder(i as usize).cloned() else {
        ctx.push_toast(crate::toast::Kind::Info, "Recent folder row no longer listed");
        return 0;
    };
    let opened = crate::wsabi::mui_ws_open_recent_path(ctx, &path);
    if opened == 1 {
        ctx.welcome.dismiss();
    } else if !path.is_dir() {
        ctx.welcome.clear_recent_folder_hits();
    }
    opened
}

// ===========================================================================
// Toast notifications
// ===========================================================================

/// Predefined toast message ids for Mighty-originated toasts (strings can't
/// cross the FFI, L17). Kept small + stable. `mui_toast(kind, msg_id)` looks up
/// the string here.
fn toast_message(msg_id: i32) -> &'static str {
    match msg_id {
        1 => "Saved",
        2 => "Formatted document",
        3 => "Committed changes",
        4 => "No definition found",
        5 => "Welcome to Mighty",
        6 => "Zen mode on",
        7 => "Zen mode off",
        8 => "Copied",
        9 => "Nothing to undo",
        10 => "No previous location",
        _ => "Done",
    }
}

/// Push a predefined toast from the Mighty side. `kind` is the severity scalar
/// (0=info, 1=success, 2=warn, 3=error); `msg_id` selects a predefined message.
#[no_mangle]
pub extern "C" fn mui_toast(handle: i64, kind: i32, msg_id: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.push_toast(crate::toast::Kind::from_scalar(kind), toast_message(msg_id));
    }
}

/// Advance the toast timers once per frame (drops expired toasts). Returns 1 if
/// the set changed (a toast expired) so the caller can request a redraw.
#[no_mangle]
pub extern "C" fn mui_toast_tick(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.toasts.tick() {
        1
    } else {
        0
    }
}

/// Clear all visible toast notifications. Returns 1 when anything was removed.
#[no_mangle]
pub extern "C" fn mui_toast_clear(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let cleared = ctx.toasts.clear();
    if !cleared {
        ctx.push_toast(crate::toast::Kind::Info, "No notifications to clear");
    }
    trace(&format!("toast_clear removed={}", if cleared { 1 } else { 0 }));
    i32::from(cleared)
}

/// Dismiss the toast under the last mouse-down position. Returns 1 on hit.
#[no_mangle]
pub extern "C" fn mui_toast_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if toast_suppressed_by_overlay(ctx) {
        return 0;
    }
    if ctx.last_event.tag != crate::ffi::MUI_EVENT_MOUSE_DOWN
        || ctx.last_event.button != crate::ffi::MUI_MOUSE_LEFT
    {
        return 0;
    }
    let (x, y) = (ctx.last_event.x, ctx.last_event.y);
    let (w, h) = visible_surface_size(ctx);
    let reserve_bottom = toast_bottom_reserve(ctx, h);
    let reserve_left = toast_left_reserve(ctx);
    let reserve_right = toast_right_reserve(ctx);
    let dismissed = ctx.toasts.dismiss_at_reserved_insets(
        w,
        h,
        reserve_bottom,
        reserve_left,
        reserve_right,
        x,
        y,
        std::time::Instant::now(),
    );
    trace(&format!(
        "toast_click x={:.1} y={:.1} hit={}",
        x,
        y,
        if dismissed { 1 } else { 0 }
    ));
    i32::from(dismissed)
}

/// Draw the bottom-right toast stack over everything (overlay layer). No-op when
/// empty.
#[no_mangle]
pub extern "C" fn mui_toast_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if ctx.toasts.is_empty() {
        return;
    }
    if toast_suppressed_by_overlay(ctx) {
        return;
    }
    let (w, h) = visible_surface_size(ctx);
    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    let toasts = std::mem::take(&mut ctx.toasts);
    let reserve_bottom = toast_bottom_reserve(ctx, h);
    let reserve_left = toast_left_reserve(ctx);
    let reserve_right = toast_right_reserve(ctx);
    toasts.draw_reserved_insets(
        ctx,
        w,
        h,
        reserve_bottom,
        reserve_left,
        reserve_right,
        std::time::Instant::now(),
    );
    ctx.toasts = toasts;
    ctx.overlay = was_overlay;
    ctx.text.set_overlay(was_overlay);
}

fn toast_bottom_reserve(ctx: &MuiContext, visible_h: u32) -> f32 {
    if !ctx.bottom_dock_open() {
        return 0.0;
    }
    (visible_h as f32 - theme::LINE_HEIGHT() - layout::term_panel_top(visible_h)).max(0.0)
}

fn toast_suppressed_by_overlay(ctx: &MuiContext) -> bool {
    dirty_confirm_active(ctx)
        || ctx.settings_panel.is_active()
        || ctx.shortcuts.is_active()
        || ctx.theme_picker.is_active()
        || ctx.palette.is_active()
        || ctx.quickopen.is_active()
        || ctx.branch_picker.is_active()
        || ctx.crumb_menu.is_active()
}

fn toast_left_reserve(ctx: &MuiContext) -> f32 {
    layout::region(ctx.sidebar_visible).left + 10.0
}

fn toast_right_reserve(ctx: &MuiContext) -> f32 {
    if ctx.ai.open {
        crate::ai::AI_PANEL_W + 10.0
    } else {
        0.0
    }
}

// ===========================================================================
// Zen / focus mode
// ===========================================================================

/// Toggle Zen / focus mode (hide rail + sidebar + tab bar + breadcrumb + status
/// bar; full-window centered editor). Returns the new state (1 = on). Pushes a
/// confirmation toast.
#[no_mangle]
pub extern "C" fn mui_zen_toggle(handle: i64) -> i32 {
    let now = !layout::zen_active();
    layout::set_zen(now);
    if let Some(ctx) = unsafe { ctx(handle) } {
        if now {
            ctx.push_toast(crate::toast::Kind::Info, "Zen mode on \u{2014} Alt+Z to exit");
        } else {
            ctx.push_toast(crate::toast::Kind::Info, "Zen mode off");
        }
    }
    if now {
        1
    } else {
        0
    }
}

/// `true` (1) when Zen / focus mode is active. The layout reads the same flag.
#[no_mangle]
pub extern "C" fn mui_zen_active(_handle: i64) -> i32 {
    if layout::zen_active() {
        1
    } else {
        0
    }
}

/// Perform a remappable command by its palette id (the cleanly router-
/// dispatchable subset — see [`crate::shortcuts::is_remappable`]). Both the
/// default chords and any remapped chords funnel through here so a command's
/// behavior is identical no matter which chord fired it. Returns `1` (consumed).
fn router_dispatch(handle: i64, cmd_id: u32) -> i32 {
    use crate::palette::*;
    match cmd_id {
        x if x == CMD_ZEN_MODE => {
            let _ = mui_zen_toggle(handle);
        }
        x if x == CMD_AGENTS => {
            let _ = crate::panels::mui_panel_set(handle, crate::PANEL_AGENTS_MTY);
            let _ = crate::agentsabi::mui_agents_refresh(handle);
        }
        x if x == CMD_GIT_TOGGLE_BLAME => {
            let _ = crate::featureabi::mui_blame_toggle(handle);
        }
        x if x == CMD_RUN_IN_BROWSER => {
            let _ = crate::webabi::mui_web_run(handle);
        }
        x if x == CMD_SPLIT_RIGHT => {
            let _ = mui_pane_split_right(handle);
        }
        x if x == CMD_MARKDOWN_PREVIEW => {
            let _ = mui_md_open(handle);
        }
        x if x == CMD_OPEN_FOLDER => {
            let _ = crate::wsabi::mui_ws_open_dialog(handle);
        }
        x if x == CMD_SIDEBAR_CYCLE_WIDTH => {
            let _ = mui_sidebar_layout_dispatch(handle, CMD_SIDEBAR_CYCLE_WIDTH as i32);
        }
        _ => return 0,
    }
    1
}

/// Centralized chord router for chords that must NOT each get their own
/// top-level `else if` arm in `src/main.mty`'s editor key ladder (the ladder is
/// at the mty v0.36 recursive-descent parse-stack ceiling — adding an arm
/// overflows `mty build`; see docs/mighty-language-lessons.md L37). New chords
/// are added HERE and the Mighty side calls `mui_chord` from a SINGLE existing
/// arm. Returns `1` if the chord was consumed, `0` to fall through.
///
/// Handled today:
///   * **Alt+Z** → toggle Zen / focus mode.
///   * **Alt+\\** → force an inline AI ghost completion (kept here so the Mighty
///     side's Alt arm is one call).
#[no_mangle]
pub extern "C" fn mui_chord(handle: i64, cp: i32, mods: i32) -> i32 {
    let alt = (mods & 4) != 0;
    let ctrl = (mods & 2) != 0;
    let shift = (mods & 1) != 0;

    // Ctrl+Shift+/ (Ctrl+?) : open the Keyboard Shortcuts reference overlay. Not
    // a remappable command itself (it's how you GET to remapping), so it stays a
    // literal arm here. Routed through `mui_chord` so the Mighty ladder gains NO
    // new top-level arm (L37/L38).
    if ctrl && shift && !alt && cp == 47 {
        mui_keys_open(handle);
        return 1;
    }

    // --- remapping: the override map wins over the hard-coded default chords. ---
    // Resolve the incoming chord to a remappable command id (an override, or the
    // command's default chord when it hasn't been remapped away) and dispatch it.
    // This is what makes a remapped command fire on its NEW chord and stop firing
    // on the freed default. The 7 remappable commands ALL go through here, so
    // their literal default arms no longer exist below.
    let resolved = unsafe { ctx(handle) }.and_then(|c| c.shortcuts.overrides().resolve(cp, mods));
    if let Some(id) = resolved {
        return router_dispatch(handle, id);
    }

    // Ctrl+Shift+[ : toggle the fold of the region enclosing the cursor.
    // Ctrl+Shift+] : unfold (toggle) it again. Both routed here (no new Mighty
    // ladder arm — L37/L38). `[`==91, `]`==93. The `{`/`}` codepoints (123/125)
    // are accepted too since some layouts deliver the shifted glyph.
    if ctrl && shift && !alt && (cp == 91 || cp == 123) {
        let _ = mui_fold_dispatch(handle, crate::palette::CMD_FOLD_TOGGLE as i32);
        return 1;
    }
    if ctrl && shift && !alt && (cp == 93 || cp == 125) {
        // Symmetric "fold/unfold" toggle at the cursor (same op; toggling twice
        // restores) so a single chord pair feels natural.
        let _ = mui_fold_dispatch(handle, crate::palette::CMD_FOLD_TOGGLE as i32);
        return 1;
    }

    // Alt+\ : force an inline AI ghost completion. (Not remappable — kept literal.)
    if alt && !ctrl && cp == 92 {
        let _ = crate::ghostabi::mui_ghost_force(handle);
        return 1;
    }
    // Ctrl+1 / Ctrl+2 : focus pane 1 / pane 2 (when split). Falls through (0) when
    // the target pane doesn't exist so normal handling continues.
    if ctrl && !alt && (cp == '1' as i32 || cp == '2' as i32) {
        let want = if cp == '1' as i32 { 0 } else { 1 };
        let Some(c) = (unsafe { ctx(handle) }) else {
            return 0;
        };
        if want < c.panes.count() && want != c.panes.focused() {
            let s = c.tabs.active_model().first_visible();
            c.panes.focus(want, s);
            pane_rebind_focus(c);
            return 1;
        }
        return 0;
    }
    0
}

/// Resolve a remappable chord to a palette command id without executing it.
/// Mighty feeds the returned id into its shared command dispatcher, so remapped
/// commands use the same path as palette Enter, Quick Open command mode, and
/// mouse activation. Returns `-2` for a freed default chord (the command was
/// remapped away, so the old shortcut must be consumed), or `-1` for fixed
/// router chords / unbound chords.
#[no_mangle]
pub extern "C" fn mui_chord_command_id(handle: i64, cp: i32, mods: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let overrides = ctx.shortcuts.overrides();
    if let Some(id) = overrides.resolve(cp, mods) {
        return id as i32;
    }
    if overrides.is_freed_default(cp, mods) {
        -2
    } else {
        -1
    }
}
