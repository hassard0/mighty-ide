//! "Mighty: New Project" ABI — the scalar veneer over [`crate::newproj`].
//!
//! Flow: the preferred command path chooses the final project folder through a
//! native dialog, then calls [`create_project_at`]. The bottom prompt remains as
//! a fallback when native dialogs are unavailable; it stages the typed name into
//! the shared byte buffer and calls [`mui_newproj_create`].
//!
//! All string handling stays Rust-side (L17). `mty` discovery mirrors the other
//! shim call sites (`MIGHTY_MTY` env → dev path → `mty` on PATH); if `mty` can't
//! be run we toast a clear "needs the Mighty compiler" message and return -1 so
//! the feature degrades gracefully instead of failing silently.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::MuiContext;

/// Cast an opaque `i64` handle back to a context reference (mirrors `abi::ctx`).
#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

/// Resolve the `mty` compiler path through the shared resolver.
fn mty_path() -> String {
    crate::mty::path()
}

fn new_project_parent_missing_message(parent: &Path) -> String {
    let label = parent
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| parent.to_string_lossy().into_owned());
    format!("New project parent missing: {label}")
}

fn new_project_parent_not_folder_message(parent: &Path) -> String {
    let label = parent
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| parent.to_string_lossy().into_owned());
    format!("New project parent is not a folder: {label}")
}

/// Create a new Mighty project from the NAME staged in the shared path buffer.
///
/// Returns:
///   * `1`  — project created + opened as the workspace;
///   * `0`  — the name was invalid, or `mty new` ran but failed (a warn toast
///     explains; the prompt's caller just closes);
///   * `-1` — `mty` is not available on PATH (a warn toast explains).
///
/// The staged buffer is consumed (taken) regardless of outcome.
#[no_mangle]
pub extern "C" fn mui_newproj_create(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let staged = std::mem::take(&mut ctx.path_stage);
    let typed = String::from_utf8_lossy(&staged).into_owned();
    let name = match crate::newproj::validate_name(&typed) {
        Ok(n) => n,
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("newproj: invalid name: {e}");
            return 0;
        }
    };

    // Parent dir: the open workspace when set, else home / cwd.
    let ws_root = ctx.workspace.root().to_path_buf();
    let ws_opt: Option<&Path> = if ws_root.as_os_str().is_empty() {
        None
    } else {
        Some(ws_root.as_path())
    };
    let parent = crate::newproj::resolve_parent_dir(ws_opt);
    create_project_at(ctx, parent.join(name))
}

/// Create a Mighty project at the exact selected folder path.
///
/// The selected path is interpreted as the final project directory, not merely a
/// parent. A non-existing path is ideal. An existing empty directory is accepted
/// and removed before `mty new` so the compiler can scaffold it. Non-empty
/// folders and files are rejected rather than overwritten.
pub(crate) fn create_project_at(ctx: &mut MuiContext, target: PathBuf) -> i32 {
    let Some(raw_name) = target.file_name().and_then(|n| n.to_str()) else {
        ctx.push_toast(crate::toast::Kind::Warn, "Choose a project folder name");
        println!("newproj: selected path has no folder name: {}", target.display());
        return 0;
    };
    let name = match crate::newproj::validate_name(raw_name) {
        Ok(n) => n,
        Err(e) => {
            ctx.push_toast(crate::toast::Kind::Warn, e.clone());
            println!("newproj: invalid selected folder name: {e}");
            return 0;
        }
    };
    let Some(parent) = target.parent().map(|p| p.to_path_buf()) else {
        ctx.push_toast(crate::toast::Kind::Warn, "Choose a parent folder");
        println!("newproj: selected path has no parent: {}", target.display());
        return 0;
    };
    match std::fs::metadata(&parent) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                new_project_parent_not_folder_message(&parent),
            );
            println!("newproj: parent is not a folder: {}", parent.display());
            return 0;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                new_project_parent_missing_message(&parent),
            );
            println!("newproj: parent missing: {}", parent.display());
            return 0;
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                format!("New project parent unavailable: {}", parent.display()),
            );
            println!("newproj: parent unavailable {}: {e}", parent.display());
            return 0;
        }
    }

    if target.exists() {
        if !target.is_dir() {
            ctx.push_toast(crate::toast::Kind::Warn, format!("File already exists: {name}"));
            println!("newproj: target is a file: {}", target.display());
            return 0;
        }
        match target.read_dir() {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    ctx.push_toast(crate::toast::Kind::Warn, format!("Choose an empty folder for {name}"));
                    println!("newproj: selected folder is not empty: {}", target.display());
                    return 0;
                }
                if let Err(e) = std::fs::remove_dir(&target) {
                    ctx.push_toast(crate::toast::Kind::Warn, format!("Could not prepare folder: {name}"));
                    println!("newproj: could not remove empty folder {}: {e}", target.display());
                    return 0;
                }
            }
            Err(e) => {
                ctx.push_toast(crate::toast::Kind::Warn, format!("Could not inspect folder: {name}"));
                println!("newproj: could not inspect {}: {e}", target.display());
                return 0;
            }
        }
    }

    let mty = mty_path();
    // `mty new <name>` scaffolds a default-template project as a subdir of the
    // working directory. We run it WITH cwd = parent so the project lands there.
    let result = Command::new(&mty)
        .arg("new")
        .arg(&name)
        .current_dir(&parent)
        .output();

    match result {
        Ok(out) if out.status.success() => {
            // Re-root the workspace to the new project (rebuilds tree / index /
            // git / agents) + record it in recents, then toast success.
            let opened = open_new_project(ctx, &target);
            ctx.push_toast(
                crate::toast::Kind::Success,
                format!("Created project: {name}"),
            );
            println!(
                "newproj: created {} via `{mty} new {name}` (opened_ws={opened})",
                target.display()
            );
            1
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let detail = stderr.lines().last().unwrap_or("mty new failed").trim();
            let msg = if detail.is_empty() {
                "Could not create project".to_string()
            } else {
                format!("New project failed: {detail}")
            };
            ctx.push_toast(crate::toast::Kind::Warn, msg.clone());
            println!("newproj: `{mty} new {name}` exited non-zero: {stderr}");
            0
        }
        Err(e) => {
            ctx.push_toast(
                crate::toast::Kind::Warn,
                "New Project needs the Mighty compiler `mty` on PATH".to_string(),
            );
            println!("newproj: could not run `{mty} new`: {e}");
            -1
        }
    }
}

/// Re-root the workspace to a freshly-created project directory. Mirrors the
/// open-folder worker but takes a `PathBuf` directly (we just created it, so it
/// exists). Returns `1` when the re-root applied, else `0`.
fn open_new_project(ctx: &mut MuiContext, target: &Path) -> i32 {
    crate::wsabi::mui_ws_open_recent_path(ctx, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mty_path` honors the `MIGHTY_MTY` override.
    #[test]
    fn mty_path_uses_env_override() {
        // SAFETY: single-threaded test mutating a process env var it owns.
        std::env::set_var("MIGHTY_MTY", "C:/custom/mty.exe");
        assert_eq!(mty_path(), "C:/custom/mty.exe");
        std::env::remove_var("MIGHTY_MTY");
    }

    /// A null handle is a safe no-op returning 0 (mirrors the other ABI guards).
    #[test]
    fn null_handle_is_safe() {
        assert_eq!(mui_newproj_create(0), 0);
    }
}
