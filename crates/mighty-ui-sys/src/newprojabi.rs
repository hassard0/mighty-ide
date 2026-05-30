//! "Mighty: New Project" ABI — the scalar veneer over [`crate::newproj`].
//!
//! Flow (Mighty side): the New Project palette command opens the bottom prompt
//! for a name; on Enter the IDE stages the typed name into the shared byte
//! buffer (`mui_path_push`, the same one Open-File / Open-Folder use) and calls
//! [`mui_newproj_create`]. This validates the name, picks a parent directory
//! (the open workspace, else home), runs `mty new <name>` there, opens the new
//! project folder as the workspace, and toasts the outcome.
//!
//! All string handling stays Rust-side (L17). `mty` discovery mirrors the other
//! shim call sites (`MIGHTY_MTY` env → dev path → `mty` on PATH); if `mty` can't
//! be run we toast a clear "needs the Mighty compiler" message and return -1 so
//! the feature degrades gracefully instead of failing silently.

use std::path::Path;
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

/// Resolve the `mty` compiler path: `MIGHTY_MTY` env, else the dev build path,
/// else bare `mty` (found on PATH). Shared shape with the other shim sites.
fn mty_path() -> String {
    if let Ok(p) = std::env::var("MIGHTY_MTY") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    const DEV: &str = r"C:\Users\ihass\stardust\target\debug\mty.exe";
    if Path::new(DEV).exists() {
        return DEV.to_string();
    }
    "mty".to_string()
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
    let target = parent.join(&name);

    if target.exists() {
        let msg = format!("'{name}' already exists in {}", parent.display());
        ctx.push_toast(crate::toast::Kind::Warn, msg.clone());
        println!("newproj: {msg}");
        return 0;
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
