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

use crate::diagnostics::{self, Severity};
use crate::ffi::*;
use crate::langdetect::Language;
use crate::layout;
use crate::theme;
use crate::MuiContext;

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

/// Resolve the file to edit: `argv[1]` if given, else a scratch file in the
/// current directory. The scratch file is created empty if it does not exist
/// (so the editor never defaults to its own source — see deliverable 1).
///
/// The `bool` return is `true` when a file argument WAS supplied. On a no-arg
/// launch (`false`) the IDE forces the branded Welcome screen open so a
/// double-click lands on the landing page instead of an anonymous scratch
/// buffer (the path-backed scratch tab still exists underneath, so "New File" /
/// typing dismisses Welcome straight into an editable buffer).
fn resolve_target_path() -> (PathBuf, bool) {
    if let Some(arg) = std::env::args().nth(1) {
        return (PathBuf::from(arg), true);
    }
    let scratch = PathBuf::from("scratch.mty");
    if !scratch.exists() {
        if let Err(e) = std::fs::write(&scratch, b"") {
            eprintln!("mui_init_s: could not create scratch file: {e}");
        }
    }
    (scratch, false)
}

/// First-run onboarding: if the recent-folders MRU is empty AND a bundled
/// `samples/` directory exists beside the running exe, record it so the Welcome
/// screen surfaces a clickable "samples" recent folder. Idempotent — only seeds
/// when the MRU is empty, so a returning user's real history is never touched.
fn seed_first_run_samples(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.recent_workspaces.is_empty() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let samples = dir.join("samples");
    if samples.is_dir() {
        ctx.recent_workspaces.record(samples.clone());
        let _ = crate::config::save_recent_workspaces(&ctx.recent_workspaces.to_blob());
        println!("mui_init_s: first-run -> seeded samples folder {}", samples.display());
    }
}

/// Basename of `path` (file name component), or the whole path as a fallback.
fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
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

// ---------------------------------------------------------------------------
// init / shutdown
// ---------------------------------------------------------------------------

/// Open a window `width`x`height` and return an opaque `i64` handle, or `0` on
/// failure. Scalar mirror of [`crate::mui_init`] that additionally:
///   * resolves the target file from `argv[1]` (or a scratch file — never the
///     editor's own source);
///   * titles the window with the file's basename;
///   * eagerly loads the file so [`mui_load`] can report its length.
#[no_mangle]
pub extern "C" fn mui_init_s(width: u32, height: u32) -> i64 {
    let (path, had_file_arg) = resolve_target_path();
    let title = format!("{} — Mighty IDE", basename(&path));
    println!("mui_init_s: editing {}", path.display());

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

    let handle = crate::build_context(width, height, title, Some(path)) as usize as i64;

    // First-run onboarding: when there are no recent folders yet (a fresh
    // install) and a bundled `samples/` dir sits next to the exe, seed it into
    // the recents MRU so the Welcome screen's "Recent Folders" offers a
    // one-click "samples" entry to explore. Non-destructive + idempotent (it
    // only fires when the MRU is empty), so it never overrides a real history.
    seed_first_run_samples(handle);

    // No-arg launch (double-click): force the branded Welcome screen so the IDE
    // opens to its landing page, not an anonymous scratch buffer. The path-backed
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
            // Seed a signature directly (so the capture is deterministic even if
            // the LSP is slow): a representative `fn add` signature, active param 1.
            let ok = ctx.sig.set(Some(crate::language::ParsedSignature {
                label: "fn add(a: I32, b: I32) -> I32".to_string(),
                params: vec!["a: I32".to_string(), "b: I32".to_string()],
                active: 1,
                doc: "Adds two integers and returns the sum.".to_string(),
            }));
            ctx.sig_autoopen = Some((9, 16));
            println!("mui_init_s: MUI_SIG_AUTOOPEN -> signature active={ok}");
        }
    }
    if std::env::var_os("MUI_RENAME_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let seed = std::env::var("MUI_RENAME_AUTOOPEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty() && v != "1")
                .unwrap_or_else(|| "add".to_string());
            ctx.rename.open(&seed);
            // Type a fresh name so the field shows an edited value.
            ctx.rename.backspace();
            ctx.rename.backspace();
            ctx.rename.backspace();
            for ch in "compute_sum".chars() {
                ctx.rename.push(ch as u32);
            }
            ctx.rename_autoopen = true;
            println!("mui_init_s: MUI_RENAME_AUTOOPEN -> rename open for \"{seed}\"");
        }
    }
    if std::env::var_os("MUI_CODEACTION_AUTOOPEN").is_some() {
        if let Some(ctx) = unsafe { ctx(handle) } {
            let actions = vec![
                crate::language::CodeAction {
                    title: "Replace 'prnt' with 'print'".to_string(),
                    edit: None,
                    fix_all_mty: false,
                },
                crate::language::CodeAction {
                    title: "Import 'print' from std".to_string(),
                    edit: None,
                    fix_all_mty: false,
                },
                crate::language::CodeAction {
                    title: "Fix all (mty)".to_string(),
                    edit: None,
                    fix_all_mty: true,
                },
            ];
            let n = ctx.codeaction.set(actions);
            ctx.codeaction_autoopen = Some((9, 6));
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
            let raw = raw.trim();
            let (find, repl) = if raw.is_empty() || raw == "1" {
                ("world", "Mighty")
            } else {
                raw.split_once(':').unwrap_or((raw, ""))
            };
            ctx.replace_bar.open(find);
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
            let raw = seed.to_string_lossy();
            let raw = raw.trim();
            let suggestion = if raw.is_empty() || raw == "1" {
                ".push(item)\n  total = total + 1\n  log(\"added\")".to_string()
            } else {
                raw.replace("\\n", "\n")
            };
            // Anchor at the end of the first non-empty line (or 0,0).
            let (al, ac) = {
                let m = ctx.tabs.active_model();
                let mut anchor = (0usize, 0usize);
                for li in 0..m.line_count() {
                    if !m.line(li).trim().is_empty() {
                        anchor = (li, m.line_len(li));
                        break;
                    }
                }
                anchor
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
        let _ = crate::navsurfaces::mui_outline_refresh(handle);
        if let Some(ctx) = unsafe { ctx(handle) } {
            let which = which.to_string_lossy().to_lowercase();
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
                let anchor = layout::RAIL_W + layout::SIDEBAR_W + 90.0;
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
                let anchor = layout::RAIL_W + layout::SIDEBAR_W + 220.0;
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
            // Seed representative RECENT FOLDERS (newest first) so the Welcome
            // screen's "Recent Folders" column is populated for the capture, and
            // set an explicit workspace so the explorer header shows its name.
            for folder in ["C:\\Users\\you\\old-project", "C:\\Users\\you\\toy-lang", "C:\\Users\\you\\mighty-ide"] {
                ctx.recent_workspaces.record(std::path::PathBuf::from(folder));
            }
            ctx.workspace = crate::workspace::Workspace::new(std::path::PathBuf::from("C:\\Users\\you\\mighty-ide"));
            println!(
                "mui_init_s: MUI_WELCOME_AUTOOPEN -> welcome open, {} recents, {} folders",
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
        layout::visible_rows_in(region, c.gpu.height, c.term_open) as i32
    })
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
            let rest_x = text_x + (head.chars().count() as f32) * layout::CHAR_W();
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
            let rc = crate::titlebar::resize_code(ev.x, ev.y, w, h);
            if rc > 0 {
                if let (Some(dir), Some(host)) =
                    (crate::window::ResizeDir::from_code(rc), ctx.host.as_ref())
                {
                    host.drag_resize(dir);
                }
                return ShimAction::Consume;
            }
            // Drag strip last (caption row / rail header, not over a tab).
            if hit == Some(crate::titlebar::TitleHit::Drag) {
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
    layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 }
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
    match std::fs::write(&path, &ctx.save_buf) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mui_save_commit({}): {e}", path.display());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// live diagnostics (scalar getters over the parsed `mty check` result)
// ---------------------------------------------------------------------------

/// Re-run `mty check` on the currently-configured file path, parse the result,
/// store it in the context, and return the diagnostic count. Returns `0` (and
/// clears the stored set) if there is no configured path or the handle is null.
///
/// The IDE calls this after the initial load and after each Ctrl+S save (the
/// on-disk file is current after save), so the markers track the saved file.
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
        let source = std::fs::read_to_string(&path).unwrap_or_default();
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
    let scale = chrome / theme::FONT_SIZE();
    let advance = layout::CHAR_W() * scale;
    let text_w = |s: &str| s.chars().count() as f32 * advance;

    use crate::icons;
    // Status band (mockup linear-gradient near-black) + a thin top divider.
    ctx.dl_grad_v(0.0, y, w, bar_h, 0.0, theme::STATUS_TOP(), theme::STATUS_BOTTOM());
    ctx.dl_rect(0.0, y, w, 1.0, theme::BORDER());
    let ty = y + (bar_h - chrome) * 0.5 - 1.0;
    let icon_y = y + (bar_h - 13.0) * 0.5;

    let (line1, col1) = ctx.status_cursor;

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
    ctx.text.queue_sized(x, ty, &branch, theme::TEXT_1(), chrome, clip);
    x += text_w(&branch) + 6.0;
    ctx.text.queue_sized(x, ty, &ab, theme::TEXT_3(), chrome, clip);
    x += text_w(&ab) + 12.0;

    // Errors (red circle + N) and warnings (warn triangle + N). Prefer the
    // aggregated Problems counts when the Problems panel has run; otherwise fall
    // back to the per-file `error_count` the caller passed (active-file diags).
    let agg = ctx.problems.count() > 0 || ctx.problems.is_open();
    let n_err = if agg { ctx.problems.error_count() } else { error_count.max(0) };
    let n_warn = if agg { ctx.problems.warn_count() } else { 0 };
    ctx.dl_icon(x, icon_y, 13.0, 13.0, icons::ERROR_CIRCLE, theme::ERROR(), 1.5, false);
    x += 16.0;
    ctx.text.queue_sized(x, ty, &n_err.to_string(), if n_err > 0 { theme::ERROR() } else { theme::TEXT_1() }, chrome, clip);
    x += text_w(&n_err.to_string()) + 10.0;
    ctx.dl_icon(x, icon_y, 13.0, 13.0, icons::WARN_TRI, theme::WARNING(), 1.5, false);
    x += 16.0;
    ctx.text.queue_sized(x, ty, &n_warn.to_string(), if n_warn > 0 { theme::WARNING() } else { theme::TEXT_1() }, chrome, clip);

    // ---- right cluster (laid out right-to-left) ----
    let mut rx = w - 12.0;

    // Bell (notifications) at the far right.
    rx -= 16.0;
    ctx.dl_icon(rx, icon_y - 0.5, 14.0, 14.0, icons::BELL, theme::DIM(), 1.5, false);
    rx -= 10.0;

    // Language pill (detected from the active file) with an indigo gradient + an
    // M glyph. Falls back to "Mighty" only when the active file is Mighty.
    let lang = ctx.language.display_name();
    let lang_w = text_w(lang);
    let pill_w = lang_w + 30.0;
    let pill_h = 19.0;
    rx -= pill_w;
    let py = y + (bar_h - pill_h) * 0.5;
    ctx.dl_grad_v(rx, py, pill_w, pill_h, 6.0, theme::accent_a(0.22), theme::accent_a(0.10));
    ctx.dl_stroke(rx, py, pill_w, pill_h, 6.0, theme::ACCENT_LINE(), 1.0);
    ctx.dl_icon(rx + 8.0, py + (pill_h - 11.0) * 0.5, 11.0, 11.0, icons::LANG_M, theme::ACCENT_BRIGHT(), 1.8, false);
    ctx.text.queue_ui_sized(rx + 22.0, ty + 0.5, lang, theme::ACCENT_BRIGHT(), chrome - 1.5, clip);
    rx -= 12.0;

    // "UTF-8".
    let enc = "UTF-8";
    rx -= text_w(enc);
    ctx.text.queue_sized(rx, ty, enc, theme::DIM(), chrome, clip);
    rx -= 14.0;

    // "Spaces: 2".
    let sp = "Spaces: 2";
    rx -= text_w(sp);
    ctx.text.queue_sized(rx, ty, sp, theme::DIM(), chrome, clip);
    rx -= 14.0;

    // "Ln L, Col C".
    let lc = format!("Ln {line1}, Col {col1}");
    rx -= text_w(&lc);
    ctx.text.queue_sized(rx, ty, &lc, theme::DIM(), chrome, clip);
}

/// `1` if the last click landed on the status-bar problems chip (the
/// error/warning counters in the left cluster), else `0`. Lets Mighty open the
/// Problems panel when the chip is clicked. The chip spans the left band of the
/// status bar after the branch label (~x 96..200) on the bottom 30px row.
#[no_mangle]
pub extern "C" fn mui_status_problems_chip_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let h = ctx.gpu.height as f32;
    let y = ctx.last_event.y;
    let x = ctx.last_event.x;
    // Bottom status bar band.
    if y < h - 30.0 {
        return 0;
    }
    // The problems cluster (errors + warnings) sits after "main ↑2 ↓0".
    if (96.0..=210.0).contains(&x) {
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
pub extern "C" fn mui_prompt_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.prompt.cancel();
    }
}

/// `1` if a prompt is currently active, else `0`.
#[no_mangle]
pub extern "C" fn mui_prompt_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.prompt.is_active() { 1 } else { 0 })
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
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = layout::LINE_H();
    // Sit the prompt band one row above the status bar.
    let status_h = 30.0_f32;
    let y = (h - status_h - bar_h).max(0.0);
    let chrome = theme::CHROME_FONT_SIZE;
    let text = ctx.prompt.display_line();
    let text_y = y + (bar_h - chrome) * 0.5 - 1.0;
    let clip = ctx.clip;
    let handle_ptr = handle as usize as *mut MuiContext;
    let text_x = layout::region(ctx.sidebar_visible).left + layout::PAD + 12.0;
    unsafe {
        // Elevated band + top divider + an ember accent bar on the left edge.
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, bar_h, theme::ELEVATED());
        crate::mui_fill_rect(handle_ptr, 0.0, y, w, 1.0, theme::BORDER());
        crate::mui_fill_rect(handle_ptr, layout::region(ctx.sidebar_visible).left, y, 3.0, bar_h, theme::EMBER());
    }
    ctx.text.queue_sized(text_x, text_y, &text, theme::TEXT(), chrome, clip);
}

// ---------------------------------------------------------------------------
// Feature 3 — go-to-line: parse the goto query
// ---------------------------------------------------------------------------

/// Parse the active prompt's query as a 1-based line number, or `-1` if the
/// query is empty / not all digits / overflows. Mighty calls this on Enter.
#[no_mangle]
pub extern "C" fn mui_prompt_goto_target(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.prompt.goto_target())
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

/// Point the shim's file I/O (load / save / diagnostics) at the active tab's
/// path and update the status-bar basename. Called internally after any tab
/// open/switch/close so Ctrl+S and `mty check` follow the active file.
pub(crate) fn sync_active_path(ctx: &mut MuiContext) {
    let active = ctx.tabs.active();
    let path = ctx.tabs.path(active);
    ctx.file_name = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    ctx.language = path
        .as_ref()
        .map(|p| crate::langdetect::detect_path(p))
        .unwrap_or(crate::langdetect::Language::PlainText);
    if path.is_some() {
        ctx.welcome.allow_empty_auto();
    }
    ctx.file_path = path;
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
/// or -1 on a null handle. The staged path is resolved relative to the tree
/// root when not absolute, so Ctrl+O "foo.mty" opens beside the initial file.
#[no_mangle]
pub extern "C" fn mui_tab_open_path(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let raw = String::from_utf8_lossy(&ctx.path_stage).into_owned();
    let raw = raw.trim();
    if raw.is_empty() {
        return ctx.tabs.active() as i32;
    }
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        ctx.tree.root().join(&candidate)
    };
    let idx = ctx.tabs.open_path(resolved);
    sync_active_path(ctx);
    idx as i32
}

/// Open a native Windows file picker and open the selected file in a tab.
/// Returns the resulting tab index, or `-1` when the picker was cancelled or is
/// unavailable. The Mighty side then falls back to the typed-path prompt.
#[no_mangle]
pub extern "C" fn mui_open_file_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let root = crate::wsabi::effective_root(ctx);
    let Some(path) = pick_open_file_native(&root) else {
        println!("mui_open_file_dialog: native file dialog cancelled / unavailable");
        return -1;
    };
    let idx = ctx.tabs.open_path(path.clone());
    sync_active_path(ctx);
    ctx.quickopen.record_mru(path);
    idx as i32
}

/// Switch the active tab to `idx`. Returns the resulting active index.
#[no_mangle]
pub extern "C" fn mui_tab_switch(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx < 0 {
        return ctx.tabs.active() as i32;
    }
    let a = ctx.tabs.switch(idx as usize);
    sync_active_path(ctx);
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
    a as i32
}

/// Close tab `idx`, keeping at least one tab (last close -> empty scratch).
/// Returns the new active index.
#[no_mangle]
pub extern "C" fn mui_tab_close(handle: i64, idx: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if idx < 0 {
        return ctx.tabs.active() as i32;
    }
    let a = ctx.tabs.close(idx as usize);
    // Remap pane→tab indices so a pane never points past the end after a close.
    ctx.panes.on_tab_closed(idx as usize, ctx.tabs.count());
    sync_active_path(ctx);
    a as i32
}

/// Map the tab bar pixel x of the last click to a tab index, or -1 if the click
/// is past the last tab. Used to switch tabs by clicking.
#[no_mangle]
pub extern "C" fn mui_tab_index_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    // Only clicks within the tab-bar band (top) count.
    if ctx.last_event.y > layout::TAB_BAR_H {
        return -1;
    }
    // Tabs start right of the sidebar (when shown).
    let body_left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let lx = ctx.last_event.x;
    if lx < body_left {
        return -1;
    }
    let tab_right =
        (crate::titlebar::controls_x(ctx.gpu.width as f32) - crate::titlebar::ACTION_STRIP_W)
            .max(body_left);
    if lx >= tab_right {
        return -1;
    }
    let i = ((lx - body_left) / layout::TAB_W).floor() as usize;
    if i < ctx.tabs.count() {
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
    if ctx.last_event.y > layout::TAB_BAR_H {
        return -1;
    }
    let body_left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let lx = ctx.last_event.x;
    if lx < body_left {
        return -1;
    }
    let tab_right =
        (crate::titlebar::controls_x(ctx.gpu.width as f32) - crate::titlebar::ACTION_STRIP_W)
            .max(body_left);
    if lx >= tab_right {
        return -1;
    }
    for i in 0..ctx.tabs.count() {
        let x = body_left + i as f32 * layout::TAB_W;
        if x >= tab_right {
            break;
        }
        let tab_w = layout::TAB_W.min(tab_right - x);
        if tab_w < 48.0 {
            break;
        }
        if lx >= x + tab_w - 34.0 && lx <= x + tab_w - 8.0 {
            return i as i32;
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
            ctx.tabs.set_dirty(idx as usize, dirty != 0);
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

    // Brand mark: a small indigo logo glyph (the wordmark "M" path) near the top.
    let logo_sz = 20.0;
    let lx = (rw - logo_sz) * 0.5;
    ctx.dl_icon(
        lx,
        11.0,
        logo_sz,
        logo_sz,
        "M4 19V7.5a1 1 0 0 1 1.6-.8L12 12l6.4-5.3a1 1 0 0 1 1.6.8V19",
        theme::ACCENT_BRIGHT(),
        2.0,
        false,
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
    let icon_top = 52.0; // 12px pad + logo region
    let gap = 4.0;
    let cx = (rw - cell) * 0.5;
    // The active rail icon reflects the live sidebar panel: 0 Explorer,
    // 1 Search, 2 SourceControl (Run/Agents stay decorative).
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
            ctx.dl_grad_v(cx, cy, cell, cell, 8.0, theme::ACCENT_FAINT(), theme::accent_a(0.04));
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
    ctx.dl_icon(sx, h - 80.0, icon_sz, icon_sz, icons::USER, theme::DIM(), 1.5, false);
    ctx.dl_icon(sx, h - 42.0, icon_sz, icon_sz, icons::SETTINGS, theme::DIM(), 1.5, false);
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
    if !(0.0..=layout::RAIL_W).contains(&x) {
        return -1;
    }
    let h = ctx.gpu.height as f32;
    if y >= h - 84.0 && y <= h - 56.0 {
        return 1;
    }
    if y >= h - 46.0 && y <= h - 18.0 {
        return 2;
    }
    -1
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
    let left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
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
    let advance = chrome * 0.54; // UI-font proportional estimate
    let mut x = left + 16.0;
    let put = |ctx: &mut MuiContext, x: &mut f32, s: &str, color| {
        ctx.text.queue_ui_sized(*x, ty, s, color, chrome, clip);
        *x += s.chars().count() as f32 * advance;
    };
    let sep = |ctx: &mut MuiContext, x: &mut f32| {
        *x += 4.0;
        ctx.dl_icon(*x, icon_y, 12.0, 12.0, crate::icons::CHEVRON, theme::TEXT_4(), 1.5, false);
        *x += 12.0 + 4.0;
    };
    // Folder icon for the first segment.
    ctx.dl_icon(x, icon_y, 13.0, 13.0, crate::icons::FOLDER, theme::DIM(), 1.4, false);
    x += 13.0 + 6.0;
    put(ctx, &mut x, &parent, theme::DIM());
    sep(ctx, &mut x);
    put(ctx, &mut x, &file, theme::TEXT_1());
    sep(ctx, &mut x);
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
    ctx.dl_icon(x, icon_y, 13.0, 13.0, sym_icon, sym_color, 1.5, false);
    x += 13.0 + 5.0;
    put(ctx, &mut x, &sym_name, sym_color);

    // Right-aligned "Preview" pill — shown only when the active file is Markdown.
    // Clicking it (hit-tested by `mui_md_button_at_click`) opens the live preview.
    if ctx.language == crate::langdetect::Language::Markdown {
        let (bx, by, bw, bh) = md_button_rect(w, top, bar_h);
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

/// The screen rect `(x, y, w, h)` of the breadcrumb "Preview" pill for window
/// width `w`, breadcrumb `top`, and bar height `bar_h`. Right-aligned.
fn md_button_rect(w: f32, top: f32, bar_h: f32) -> (f32, f32, f32, f32) {
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
    let w = ctx.gpu.width as f32;
    let active = ctx.tabs.active();
    let count = ctx.tabs.count();
    let clip = ctx.clip;
    let bar_h = layout::TAB_BAR_H;
    let chrome = theme::CHROME_FONT_SIZE;
    // The tab bar lives over the editor column only — right of the rail AND the
    // sidebar (when shown), so it never overpaints the sidebar/header.
    let body_left = layout::RAIL_W + if ctx.sidebar_visible { layout::SIDEBAR_W } else { 0.0 };
    let tab_right = (crate::titlebar::controls_x(w) - crate::titlebar::ACTION_STRIP_W).max(body_left);

    use crate::icons;
    // Tab-bar background (panel) + a thin bottom divider.
    ctx.dl_rect(body_left, 0.0, tab_right - body_left, bar_h, theme::BG_2());
    ctx.dl_rect(body_left, bar_h - 1.0, tab_right - body_left, 1.0, theme::BORDER());

    for i in 0..count {
        let x = body_left + i as f32 * layout::TAB_W;
        if x >= tab_right {
            break;
        }
        let tab_w = layout::TAB_W.min(tab_right - x);
        if tab_w < 48.0 {
            break;
        }
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
            let dirty = tab.dirty;
            let (icon, icon_col) = file_icon_for(&base, is_active);
            let icon_y = (bar_h - 14.0) * 0.5;
            ctx.dl_icon(x + 14.0, icon_y, 14.0, 14.0, icon, icon_col, 1.4, false);
            let mut label = base;
            let max_chars = ((tab_w - 64.0).max(0.0) / layout::CHAR_W()).floor() as usize;
            if label.chars().count() > max_chars && max_chars > 1 {
                label = label.chars().take(max_chars - 1).collect::<String>() + "…";
            }
            let fg = if is_active { theme::TEXT() } else { theme::DIM() };
            let ty = (bar_h - chrome) * 0.5 - 1.0;
            // The ACTIVE tab's label reads in the bold UI face so the current
            // file stands out among the tabs.
            let style = if is_active {
                crate::vello_ui::FontStyle::Bold
            } else {
                crate::vello_ui::FontStyle::Regular
            };
            ctx.text.queue_ui_styled(x + 34.0, ty, &label, fg, chrome, style, clip);
            // Trailing affordance: always show close; dirty tabs keep a status dot.
            let tx = x + tab_w - 24.0;
            if dirty {
                ctx.dl_round(tx - 8.0, bar_h * 0.5 - 2.5, 5.0, 5.0, 2.5, theme::ACCENT_BRIGHT());
            }
            let close_col = if is_active { theme::TEXT_1() } else { theme::TEXT_3() };
            ctx.dl_icon(tx, (bar_h - 12.0) * 0.5, 12.0, 12.0, icons::CLOSE, close_col, 1.6, false);
        }
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
    let bar_h = layout::TAB_BAR_H;
    let btn_w = crate::titlebar::BTN_W;
    let controls_x = crate::titlebar::controls_x(w);
    let maximized = ctx.window_maximized;
    let icon_d = 14.0;
    let iy = (bar_h - icon_d) * 0.5;

    // A solid backing under the controls + run/dots so any panel drawn beneath
    // can't bleed through (the AI panel header used to show under the icons).
    let strip_x = controls_x - 60.0 - 8.0;
    ctx.dl_rect(strip_x, 0.0, w - strip_x, bar_h, theme::BG_RAIL());
    ctx.dl_rect(strip_x, 0.0, 1.0, bar_h, theme::BORDER_SOFT());

    // Run + more-actions (just left of the window controls).
    let ax = controls_x - 60.0;
    let ay = (bar_h - 16.0) * 0.5;
    ctx.dl_icon(ax, ay, 16.0, 16.0, icons::RUN, theme::GREEN(), 1.5, true);
    ctx.dl_icon(ax + 28.0, ay, 16.0, 16.0, icons::DOTS, theme::TEXT_3(), 0.0, true);

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

/// Hit-test the top-right run / more-actions icons (drawn by
/// `mui_window_controls_draw`). Returns 1 = run, 2 = more-actions (⋯), else 0.
/// Geometry mirrors the draw: run at `controls_x-60`, dots at `+28`, 16px wide,
/// within the title-bar row height.
#[no_mangle]
pub extern "C" fn mui_topbar_action_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if y < 0.0 || y >= layout::TAB_BAR_H {
        return 0;
    }
    let controls_x = crate::titlebar::controls_x(ctx.gpu.width as f32);
    let ax = controls_x - 60.0;
    if x >= ax && x < ax + 18.0 {
        return 1;
    }
    if x >= ax + 26.0 && x < ax + 46.0 {
        return 2;
    }
    0
}

/// Hit-test the Explorer header action icons (new file / new folder / collapse),
/// drawn in `mui_sidebar_draw` as three 15px icons at sx+sw-72/-50/-28 in the 40px
/// header band. Returns 1 = new file, 2 = new folder, 3 = collapse all, else 0.
/// (Only meaningful while the Explorer panel + sidebar are visible.)
#[no_mangle]
pub extern "C" fn mui_explorer_header_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !ctx.sidebar_visible {
        return 0;
    }
    let x = ctx.last_event.x;
    let y = ctx.last_event.y;
    if y < 0.0 || y >= 40.0 {
        return 0;
    }
    let right = layout::RAIL_W + layout::SIDEBAR_W;
    // Each icon is 15px wide; give a forgiving ~20px hit slot centered on it.
    let nf = right - 72.0;
    let nfo = right - 50.0;
    let col = right - 28.0;
    if x >= nf - 3.0 && x < nf + 18.0 {
        return 1;
    }
    if x >= nfo - 3.0 && x < nfo + 18.0 {
        return 2;
    }
    if x >= col - 3.0 && x < col + 18.0 {
        return 3;
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
    let raw = String::from_utf8_lossy(&ctx.path_stage).into_owned();
    let raw = raw.trim();
    if raw.is_empty() {
        return 0;
    }
    let base = crate::wsabi::effective_root(ctx);
    let cand = std::path::Path::new(raw);
    let target = if cand.is_absolute() { cand.to_path_buf() } else { base.join(cand) };
    match std::fs::create_dir_all(&target) {
        Ok(_) => {
            ctx.tree.refresh();
            1
        }
        Err(_) => 0,
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
        1
    } else {
        0
    }
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
    let sx1 = layout::RAIL_W + layout::SIDEBAR_W;
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

/// Open the file at tree row `i` as a tab (no-op for directories / out of
/// range). Returns the resulting tab index, or -1 if not a file.
#[no_mangle]
pub extern "C" fn mui_tree_open_row(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if i < 0 {
        return -1;
    }
    let Some(row) = ctx.tree.get(i as usize) else {
        return -1;
    };
    if row.is_dir {
        return -1;
    }
    let path = row.path.clone();
    let idx = ctx.tabs.open_path(path);
    sync_active_path(ctx);
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
    let sw = layout::SIDEBAR_W;
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
    // Letter-spaced uppercase header (insert thin spaces), UI family.
    let tracked: String = header.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ctx.text.queue_ui_styled(
        sx + 14.0,
        (head_h - (chrome - 2.0)) * 0.5 - 1.0,
        &tracked,
        theme::DIM(),
        chrome - 2.0,
        crate::vello_ui::FontStyle::Bold,
        clip,
    );
    // Header actions (new file / new folder / collapse) right-aligned.
    let act_y = (head_h - 15.0) * 0.5;
    ctx.dl_icon(sx + sw - 72.0, act_y, 15.0, 15.0, icons::NEW_FILE, theme::TEXT_3(), 1.5, false);
    ctx.dl_icon(sx + sw - 50.0, act_y, 15.0, 15.0, icons::NEW_FOLDER, theme::TEXT_3(), 1.5, false);
    ctx.dl_icon(sx + sw - 28.0, act_y, 15.0, 15.0, icons::COLLAPSE, theme::TEXT_3(), 1.5, false);

    // File rows. Mockup row height is 28px; we keep LINE_H rhythm but draw a
    // 28px-tall hover/selection capsule centered on the row baseline.
    let row_h = layout::LINE_H();
    let row_top = head_h + 6.0;
    let active_path = ctx.tabs.active_path();
    let count = ctx.tree.count();
    for i in 0..count {
        let (is_dir, expanded, depth, name, selected) = {
            let Some(row) = ctx.tree.get(i) else { continue };
            let selected = !row.is_dir
                && active_path.is_some()
                && row.path == *active_path.as_ref().unwrap();
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
        let avail = (((sx + sw - 28.0) - name_x) / layout::CHAR_W()).floor() as usize;
        let mut shown = name.clone();
        if shown.chars().count() > avail && avail > 1 {
            shown = shown.chars().take(avail - 1).collect::<String>() + "…";
        }
        let fg = if selected { theme::TEXT() } else { theme::TEXT_1() };
        ctx.text.queue_ui_sized(name_x, txt_y, &shown, fg, chrome, clip);
        // Git status letter, right-aligned (mockup `.row .git`): M/A/U.
        if let Some((gl, gc)) = git_status_for(&name) {
            ctx.text.queue_ui_sized(sx + sw - 22.0, txt_y, gl, gc, chrome - 2.0, clip);
        }
    }
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

/// One queued terminal text run: position, string, and resolved RGBA color.
type TermRun = (f32, f32, String, (f32, f32, f32, f32));

/// Grid dimensions for the terminal panel given the current window + sidebar.
fn term_dims(ctx: &MuiContext) -> (usize, usize) {
    let region = layout::region(ctx.sidebar_visible);
    let rows = layout::term_grid_rows(ctx.gpu.height);
    let cols = layout::term_grid_cols(ctx.gpu.width, region);
    (rows, cols)
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
    if ctx.terminal.is_none() {
        match crate::terminal::Terminal::spawn(rows, cols) {
            Ok(t) => {
                println!("mui_term_open: spawned shell, grid {rows}x{cols}");
                ctx.terminal = Some(t);
            }
            Err(e) => {
                eprintln!("mui_term_open: {e}");
                return 0;
            }
        }
    } else if let Some(t) = ctx.terminal.as_mut() {
        // Re-size to the current panel in case the window changed while closed.
        t.resize(rows, cols);
    }
    ctx.term_open = true;
    1
}

/// Close the terminal panel and tear down the shell (frees the PTY + grid).
/// Marks the panel closed.
#[no_mangle]
pub extern "C" fn mui_term_close(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.term_open = false;
        // Dropping the Terminal kills the child + joins nothing (reader thread
        // exits on EOF). Keep this explicit for clarity.
        ctx.terminal = None;
    }
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

/// Map a named key (`MUI_KEY_*`) + mods to terminal stdin bytes and write them
/// to the PTY. No-op if the terminal is not running. The key->byte mapping lives
/// shim-side (see [`crate::terminal::key_to_bytes`]).
#[no_mangle]
pub extern "C" fn mui_term_key(handle: i64, keycode: i32, mods: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(t) = ctx.terminal.as_mut() {
            if keycode >= 0 {
                if let Some(bytes) =
                    crate::terminal::key_to_bytes(keycode as u32, mods.max(0) as u32)
                {
                    t.send(&bytes);
                }
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
        if let Some(t) = ctx.terminal.as_mut() {
            t.pump();
        }
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
    let width = ctx.gpu.width;
    let height = ctx.gpu.height;
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
    // an ember top accent line + a dim "TERMINAL" header (UI family).
    ctx.dl_round(panel_left, panel_top, panel_w, panel_h + 12.0, 10.0, theme::ELEVATED());
    ctx.dl_rect(panel_left, panel_top, panel_w, 1.0, theme::BORDER());
    ctx.text.queue_ui_sized(
        panel_left + layout::PAD + 4.0,
        panel_top + 4.0,
        "TERMINAL",
        theme::DIM(),
        theme::CHROME_FONT_SIZE - 1.0,
        clip,
    );
    let _ = handle_ptr;

    // Snapshot the grid into owned data so the borrow on `ctx.terminal` ends
    // before we borrow `ctx.text`.
    let (rows, cols, cursor, glyphs) = {
        let t = ctx.terminal.as_ref().expect("terminal present");
        let g = t.grid();
        let rows = g.rows();
        let cols = g.cols();
        // Build one (x, y, string, color) run per row, splitting on color change
        // to keep the draw-call count modest while preserving per-cell color.
        let mut runs: Vec<TermRun> = Vec::new();
        for r in 0..rows {
            let y = layout::term_cell_y(height, r);
            let mut col = 0usize;
            while col < cols {
                let fg = g.cell(r, col).fg;
                let start = col;
                let mut s = String::new();
                while col < cols && g.cell(r, col).fg == fg {
                    s.push(g.cell(r, col).ch);
                    col += 1;
                }
                // Trim a trailing run of spaces (don't draw blank tails).
                if !s.trim_end().is_empty() {
                    let x = layout::term_cell_x(region, start);
                    runs.push((x, y, s, crate::terminal::palette_rgba(fg)));
                }
            }
        }
        (rows, cols, g.cursor(), runs)
    };

    for (x, y, s, (r, gc, b, a)) in &glyphs {
        ctx.text
            .queue(*x, *y, s, MuiColor::new(*r, *gc, *b, *a), clip);
    }

    // Block cursor at the grid cursor position (clamped into the panel).
    let (cr, cc) = cursor;
    if cr < rows && cc <= cols {
        let cx = layout::term_cell_x(region, cc);
        let cy = layout::term_cell_y(height, cr);
        unsafe {
            crate::mui_fill_rect(
                handle_ptr,
                cx,
                cy,
                layout::CHAR_W(),
                layout::LINE_H() - 2.0,
                MuiColor::new(0.486, 0.361, 1.0, 0.6),
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

/// Translate a 0-based `(line, col)` to a byte offset in `buf` (col is a byte
/// count from the line start, clamped to the line length). Shim-side because
/// Mighty already tracks the cursor as a byte offset, but the ABI is specified
/// as `(line, col)`; this keeps the two in agreement.
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
    // Walk `col` bytes into the line, stopping at its newline / EOF.
    let mut c = 0i32;
    while i < buf.len() && buf[i] != b'\n' && c < col.max(0) {
        i += 1;
        c += 1;
    }
    i
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

/// `1` if the dropdown is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_complete_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.complete.is_active()))
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
pub extern "C" fn mui_complete_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.complete.cancel();
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
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
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
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
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
    }
}

/// Append a typed char (codepoint) to the palette query and refilter. Ignores
/// non-printable / out-of-BMP-as-char values.
#[no_mangle]
pub extern "C" fn mui_palette_push_char(handle: i64, cp: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            ctx.palette.push_char(ch);
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
    ctx.palette
        .click_row(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height)
}

/// The command id of the current selection, or `-1` when nothing matches. Mighty
/// reads this on Enter and dispatches to the matching command helper.
#[no_mangle]
pub extern "C" fn mui_palette_selected_id(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(-1, |c| c.palette.selected_id())
}

/// `1` if the palette overlay is open, else `0`.
#[no_mangle]
pub extern "C" fn mui_palette_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.palette.is_active()))
}

/// Close the palette and clear its state (Escape, or after Enter dispatch).
#[no_mangle]
pub extern "C" fn mui_palette_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.palette.cancel();
    }
}

/// Draw the palette as a centered overlay box (query line + filtered commands
/// with right-aligned keybindings, selection highlighted). No-op when closed.
#[no_mangle]
pub extern "C" fn mui_palette_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    // Split the borrow: `draw` needs `&mut ctx` for both rects + text.
    let engine = std::mem::take(&mut ctx.palette);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
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
        ctx.shortcuts.reset_all();
    }
}

/// Cancel: while capturing, exit capture mode; else close the overlay.
#[no_mangle]
pub extern "C" fn mui_keys_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if ctx.shortcuts.is_capturing() {
            ctx.shortcuts.cancel_capture();
        } else {
            ctx.shortcuts.cancel();
        }
    }
}

/// Draw the shortcuts overlay (no-op unless active). Same borrow-split as the
/// palette draw.
#[no_mangle]
pub extern "C" fn mui_keys_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let engine = std::mem::take(&mut ctx.shortcuts);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    engine.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
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
    ctx.theme_picker
        .click(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height)
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
pub extern "C" fn mui_theme_picker_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.theme_picker.cancel();
    }
}

/// Draw the theme-picker overlay (no-op when inactive).
#[no_mangle]
pub extern "C" fn mui_theme_picker_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let picker = std::mem::take(&mut ctx.theme_picker);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    picker.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
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
            let cmds: Vec<(String, i32)> =
                crate::palette::filter_commands(crate::palette::COMMANDS, &q)
                    .into_iter()
                    .map(|c| (c.label.to_string(), c.id as i32))
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
    ctx.quickopen.ensure_index(&root, true) as i32
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
    ctx.quickopen
        .click_row(ctx.last_event.x, ctx.last_event.y, ctx.gpu.width, ctx.gpu.height)
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
pub extern "C" fn mui_qo_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.quickopen.cancel();
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
        return -1;
    }
    let mode = ctx.quickopen.mode();
    let result: i32 = match mode {
        crate::quickopen::Mode::Files => {
            match ctx.quickopen.accept_file_path(i) {
                Some(path) if path.exists() => {
                    let idx = ctx.tabs.open_path(path.clone());
                    sync_active_path(ctx);
                    ctx.quickopen.record_mru(path);
                    idx as i32
                }
                _ => -1,
            }
        }
        crate::quickopen::Mode::Symbols => {
            let sym = ctx.quickopen.accept_symbol(i);
            if sym < 0 {
                -1
            } else {
                let line = ctx.outline.line_of(sym as usize);
                if line < 0 {
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
    ctx.quickopen.cancel();
    result
}

/// In Commands mode (`>` query), the palette command id of the selected row
/// (`-1` = current), or `-1` when not in Commands mode / no match. Mighty reads
/// this on Enter and dispatches to the SAME helper a keybinding triggers, then
/// calls `mui_qo_cancel`.
#[no_mangle]
pub extern "C" fn mui_qo_command_id(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    if ctx.quickopen.mode() != crate::quickopen::Mode::Commands {
        return -1;
    }
    let idx = if i < 0 { ctx.quickopen.selection() } else { i as usize };
    ctx.quickopen.row(idx).map(|r| r.target).unwrap_or(-1)
}

/// Record `path`-by... — record the ACTIVE file as recently-opened. Called by
/// Mighty whenever a file opens via any path (tabs, tree, prompt) so the MRU
/// reflects real usage. No-op if there is no active file path.
#[no_mangle]
pub extern "C" fn mui_qo_record_active(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        if let Some(p) = ctx.tabs.active_path() {
            ctx.quickopen.record_mru(p);
        }
    }
}

/// Draw the Quick-Open overlay (no-op unless active). Centered card over the UI.
#[no_mangle]
pub extern "C" fn mui_qo_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let qo = std::mem::take(&mut ctx.quickopen);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    qo.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
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
        None => return 0,
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
        None => return 0,
    };
    let source = String::from_utf8_lossy(&ctx.nav_buf).into_owned();
    let raw = lsp_def_raw(ctx.language, &path, &source, line.max(0) as u32, col.max(0) as u32);
    let found = match crate::nav::parse_definition(&raw) {
        Some((uri, tline, tcol)) => match crate::nav::uri_to_path(&uri) {
            Some(tpath) => {
                ctx.def.set(Some(crate::nav::DefTarget {
                    path: tpath,
                    line: tline,
                    col: tcol,
                }));
                true
            }
            None => false,
        },
        None => false,
    };
    println!("def: line={line} col={col} found={found}");
    if !found {
        ctx.push_toast(crate::toast::Kind::Warn, "No definition found");
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
        None => return -1,
    };
    let idx = ctx.tabs.open_path(target_path);
    sync_active_path(ctx);
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

/// Extract the identifier (`[A-Za-z_][A-Za-z0-9_]*`) that contains or ends at the
/// char `col` on `line` of `text`. Returns `""` if the cursor isn't on an
/// identifier (used to prefill the rename input).
fn identifier_at(text: &str, line: u32, col: u32) -> String {
    let line_str = text.split('\n').nth(line as usize).unwrap_or("");
    let chars: Vec<char> = line_str.chars().collect();
    let is_id = |c: char| c == '_' || c.is_ascii_alphanumeric();
    let n = chars.len();
    let c = (col as usize).min(n);
    // Find an identifier covering the cursor: scan left for the start, right for
    // the end, allowing the cursor to sit just after the identifier too.
    let mut start = c;
    while start > 0 && is_id(chars[start - 1]) {
        start -= 1;
    }
    let mut end = c;
    while end < n && is_id(chars[end]) {
        end += 1;
    }
    if start == end {
        return String::new();
    }
    // Reject a leading digit (numeric literal).
    if chars[start].is_ascii_digit() {
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
        None => return 0,
    };
    let (source, _, _) = active_source_and_cursor(ctx);
    let raw = crate::language::lsp::request(
        &path,
        &source,
        crate::language::lsp::Req::SignatureHelp {
            line: line.max(0) as u32,
            col: col.max(0) as u32,
        },
    );
    let available = match crate::language::parse_signature_help(&raw) {
        Some(sig) => ctx.sig.set(Some(sig)),
        None => false,
    };
    println!("sig: line={line} col={col} available={available}");
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
    sig.draw(ctx, x, y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.sig = sig;
}

// ---- rename symbol (F2) ----

/// Prepare a rename at the cursor `(line, col)`: derive the symbol under the
/// cursor (preferring `prepareRename`'s range when the server provides one) and
/// open the inline rename input prefilled with it. Returns `1` if a renamable
/// symbol was found, else `0` (input not opened).
#[no_mangle]
pub extern "C" fn mui_rename_prepare(handle: i64, line: i32, col: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let (source, _, _) = active_source_and_cursor(ctx);
    // Prefer the identifier under the cursor in the live buffer (robust + matches
    // what prepareRename would return for mty-lsp). prepareRename is consulted
    // only to confirm renamability when the local scan is empty.
    let mut symbol = identifier_at(&source, line.max(0) as u32, col.max(0) as u32);
    if symbol.is_empty() {
        if let Some(path) = ctx.file_path.clone() {
            let raw = crate::language::lsp::request(
                &path,
                &source,
                crate::language::lsp::Req::PrepareRename {
                    line: line.max(0) as u32,
                    col: col.max(0) as u32,
                },
            );
            // prepareRename returns a range; re-derive the symbol from its start.
            if let Some((sl, sc)) = parse_prepare_rename_start(&raw) {
                symbol = identifier_at(&source, sl, sc);
            }
        }
    }
    if symbol.is_empty() {
        println!("rename: line={line} col={col} no-symbol");
        return 0;
    }
    ctx.rename.open(&symbol);
    println!("rename: prepare symbol=\"{symbol}\"");
    1
}

/// Parse the `prepareRename` result's start `(line, character)`. The result is a
/// `Range` `{"start":{"line":N,"character":N},"end":{...}}`.
fn parse_prepare_rename_start(json: &str) -> Option<(u32, u32)> {
    let bytes = json.as_bytes();
    let start_at = find_subslice(bytes, b"\"start\"")?;
    let region = &bytes[start_at..];
    let line = read_uint_in(region, b"\"line\"")?;
    let col = read_uint_in(region, b"\"character\"")?;
    Some((line, col))
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

/// Cancel the rename input (discard the buffer + any staged edit).
#[no_mangle]
pub extern "C" fn mui_rename_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.rename.cancel();
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
    let raw = crate::language::lsp::request(
        &path,
        &source,
        crate::language::lsp::Req::Rename {
            line: line.max(0) as u32,
            col: col.max(0) as u32,
            new_name: new_name.clone(),
        },
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

    let files_changed = apply_workspace_edit(ctx, &we, &new_name);
    ctx.rename.set_edit(Some(we));
    ctx.rename.cancel();
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
    let is_id = |c: char| c == '_' || c.is_ascii_alphanumeric();
    let sym_chars: Vec<char> = symbol.chars().collect();
    let slen = sym_chars.len();
    for (li, raw_line) in source.split('\n').enumerate() {
        let chars: Vec<char> = raw_line.chars().collect();
        let mut i = 0usize;
        while i + slen <= chars.len() {
            if chars[i..i + slen] == sym_chars[..] {
                let before_ok = i == 0 || !is_id(chars[i - 1]);
                let after_ok = i + slen == chars.len() || !is_id(chars[i + slen]);
                if before_ok && after_ok {
                    out.push(crate::language::TextEdit {
                        start_line: li as u32,
                        start_col: i as u32,
                        end_line: li as u32,
                        end_col: (i + slen) as u32,
                        new_text: String::new(), // filled by apply via new_name? no:
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
/// in-place + saved; other files are rewritten on disk and any open tab for them
/// is reloaded. Returns the count of files actually changed.
fn apply_workspace_edit(
    ctx: &mut MuiContext,
    we: &crate::language::WorkspaceEdit,
    new_name: &str,
) -> i32 {
    let current = ctx.file_path.clone();
    let mut changed = 0i32;
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
            // Apply to the active model in-place (preserves the live edit state),
            // then save it to disk.
            let m = ctx.tabs.active_model_mut();
            let text = m.as_text();
            let cl = m.cursor_line() as i32;
            let cc = m.cursor_col() as i32;
            let edited = crate::language::apply_text_edits(&text, &edits);
            *m = crate::editor::TextModel::from_bytes(edited.as_bytes());
            m.move_to(cl, cc);
            if let Some(p) = current.clone() {
                let _ = std::fs::write(&p, m.to_bytes());
                m.mark_clean();
            }
            changed += 1;
        } else {
            // Other file: read from disk, apply, write back; refresh an open tab.
            let disk = std::fs::read(&fpath).unwrap_or_default();
            let text = String::from_utf8_lossy(&disk).into_owned();
            let edited = crate::language::apply_text_edits(&text, &edits);
            if std::fs::write(&fpath, edited.as_bytes()).is_ok() {
                changed += 1;
                // If this file is open in a tab, reopen it to refresh its model.
                if ctx.tabs.find_by_path(&fpath).is_some() {
                    let _ = ctx.tabs.open_path(fpath.clone());
                }
            }
        }
    }
    // Restore active focus to the original file (open_path may have switched).
    if let Some(p) = current {
        let _ = ctx.tabs.open_path(p);
        sync_active_path(ctx);
    }
    changed
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
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    rename.draw(ctx, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
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
    let raw = crate::language::lsp::request(
        &path,
        &source,
        crate::language::lsp::Req::CodeAction {
            start_line: line0,
            start_col: 0,
            end_line: line0,
            end_col: line_len.max(col.max(0) as u32),
        },
    );
    let mut actions = crate::language::parse_code_actions(&raw);
    // Only offer "Fix all (mty)" when there's an actual fixable diagnostic on the
    // line (the LSP returned at least one action) — so the lightbulb never lights
    // every line just because the `mty fix` subcommand exists.
    if !actions.is_empty() && mty_fix_available() {
        actions.push(crate::language::CodeAction {
            title: "Fix all (mty)".to_string(),
            edit: None,
            fix_all_mty: true,
        });
    }
    actions
}

/// `1` if `mty fix --help` succeeds (the fixer subcommand exists).
pub(crate) fn mty_fix_available() -> bool {
    let mty = if let Ok(p) = std::env::var("MIGHTY_MTY") {
        if !p.trim().is_empty() {
            p
        } else {
            mty_default()
        }
    } else {
        mty_default()
    };
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
    const DEV: &str = r"C:\Users\ihass\stardust\target\debug\mty.exe";
    if std::path::Path::new(DEV).exists() {
        DEV.to_string()
    } else {
        "mty".to_string()
    }
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
    ctx.codeaction.click_row(
        ctx.last_event.x,
        ctx.last_event.y,
        cx,
        cy,
        ctx.gpu.width,
        ctx.gpu.height,
    )
}

/// Cancel/close the code-action menu.
#[no_mangle]
pub extern "C" fn mui_codeaction_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.codeaction.cancel();
    }
}

/// Apply the selected code action: apply its inline `WorkspaceEdit`, or run
/// `mty fix --apply` on the active file (the "Fix all (mty)" action) + reload.
/// Returns `1` if anything changed, `0` otherwise. Closes the menu.
#[no_mangle]
pub extern "C" fn mui_codeaction_apply(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let selected = ctx.codeaction.selected().cloned();
    ctx.codeaction.cancel();
    let Some(action) = selected else {
        return 0;
    };

    if action.fix_all_mty {
        // Save the live buffer, run `mty fix --apply`, reload.
        let path = match ctx.file_path.clone() {
            Some(p) => p,
            None => return 0,
        };
        let bytes = ctx.tabs.active_model().to_bytes();
        if std::fs::write(&path, &bytes).is_err() {
            return 0;
        }
        ctx.tabs.active_model_mut().mark_clean();
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
                ctx.tabs.reload_active(&reloaded);
            }
        }
        println!("codeaction: apply Fix-all-mty ok={ok}");
        return i32::from(ok);
    }

    // Inline-edit action.
    if let Some(we) = &action.edit {
        let we = we.clone();
        let changed = apply_workspace_edit(ctx, &we, "");
        println!("codeaction: apply edit files={changed}");
        return i32::from(changed > 0);
    }
    println!("codeaction: apply (command/no-edit) — no-op");
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
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let menu = std::mem::take(&mut ctx.codeaction);
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    menu.draw(ctx, x, y, w, h);
    ctx.overlay = false;
    ctx.text.set_overlay(false);
    ctx.codeaction = menu;
}

/// `read_uint_after` clone over an explicit region (avoids exporting the nav
/// helper). Reads the unsigned integer value of `key` in `region`.
fn read_uint_in(region: &[u8], key: &[u8]) -> Option<u32> {
    let p = find_subslice(region, key)?;
    let mut j = p + key.len();
    while j < region.len() && matches!(region[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    let start = j;
    let mut v: u32 = 0;
    while j < region.len() && region[j].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((region[j] - b'0') as u32);
        j += 1;
    }
    if j == start {
        None
    } else {
        Some(v)
    }
}

/// Find the first occurrence of `needle` in `hay` (byte substring search).
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
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
#[no_mangle]
pub extern "C" fn mui_format_current(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.file_path.clone() else {
        eprintln!("format: no file path configured");
        return -1;
    };
    match crate::format::run_fmt(&path) {
        crate::format::FmtOutcome::Formatted => {
            println!("format: {} -> ok", path.display());
            ctx.push_toast(crate::toast::Kind::Success, "Formatted document");
            1
        }
        crate::format::FmtOutcome::NotApplicable => {
            println!("format: {} -> skipped (only .mty supported)", path.display());
            0
        }
        crate::format::FmtOutcome::Failed => {
            println!("format: {} -> failed", path.display());
            ctx.push_toast(crate::toast::Kind::Error, "Format failed");
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
    ctx(handle).map(|c| c.tabs.active_model_mut())
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
pub extern "C" fn mui_ed_insert_char(handle: i64, cp: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            m.insert_char(ch);
        }
    }
}

/// Delete the char before the cursor (joining lines at column 0).
#[no_mangle]
pub extern "C" fn mui_ed_backspace(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.backspace();
    }
}

/// Delete the char at the cursor (joining the next line at end of line).
#[no_mangle]
pub extern "C" fn mui_ed_delete(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.delete();
    }
}

/// Insert a newline at the cursor.
#[no_mangle]
pub extern "C" fn mui_ed_newline(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.newline();
    }
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
        ctx.tabs.active_fold_mut().toggle_at_cursor(line);
        1
    } else if id == CMD_FOLD_ALL {
        ctx.tabs.active_fold_mut().fold_all();
        1
    } else if id == CMD_UNFOLD_ALL {
        ctx.tabs.active_fold_mut().unfold_all();
        1
    } else {
        0
    }
}

/// `1` if the active model has unsaved edits, else `0`.
#[no_mangle]
pub extern "C" fn mui_ed_dirty(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.tabs.active_model().dirty()))
}

/// Mark the active model clean (after a load) or dirty.
#[no_mangle]
pub extern "C" fn mui_ed_set_dirty(handle: i64, dirty: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.set_dirty(dirty != 0);
    }
}

/// Load the active tab's file from disk into the active model (replacing it),
/// resetting the cursor to the top. Returns the byte length, or `-1` on error.
#[no_mangle]
pub extern "C" fn mui_ed_load(handle: i64) -> i64 {
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
        ctx.tabs.reload_active(b"");
        return 0;
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let n = bytes.len() as i64;
            ctx.tabs.reload_active(&bytes);
            println!("mui_ed_load: {} ({} bytes)", path.display(), n);
            n
        }
        Err(e) => {
            eprintln!("mui_ed_load({}): {e}", path.display());
            ctx.tabs.reload_active(b"");
            -1
        }
    }
}

/// Compute the bytes to write for the active tab, applying the enabled on-save
/// transforms (trim trailing whitespace / ensure final newline) and updating the
/// in-memory model so the buffer matches disk (cursor preserved). Returns the
/// exact bytes that should be written.
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
    if !ctx.tabs.active_model().dirty() {
        ctx.autosave.disarm();
        return 0;
    }
    let Some(path) = ctx.tabs.active_path() else {
        return 0;
    };
    if !ctx.autosave.due() {
        return 0;
    }
    let bytes = save_bytes_for_active(ctx);
    let name = basename(&path);
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            ctx.tabs.active_model_mut().mark_clean();
            // Re-baseline the signature to the (possibly transformed) saved text
            // so the next tick doesn't see the transform as a fresh edit.
            ctx.autosave_sig = Some(autosave_signature(&ctx.tabs.active_model().as_text()));
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

/// Write the active model to its tab's file path. Returns `0` on success, `-1`
/// on error (no path / IO failure). Marks the model clean on success.
#[no_mangle]
pub extern "C" fn mui_ed_save(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let Some(path) = ctx.tabs.active_path() else {
        eprintln!("mui_ed_save: no file path for active tab");
        return -1;
    };
    let bytes = save_bytes_for_active(ctx);
    let name = basename(&path);
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            ctx.tabs.active_model_mut().mark_clean();
            ctx.autosave.disarm();
            println!("mui_ed_save: {} ({} bytes)", path.display(), bytes.len());
            ctx.push_toast(crate::toast::Kind::Success, format!("Saved {name}"));
            0
        }
        Err(e) => {
            eprintln!("mui_ed_save({}): {e}", path.display());
            ctx.push_toast(crate::toast::Kind::Error, format!("Save failed: {name}"));
            -1
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
    let raw = String::from_utf8_lossy(&ctx.path_stage).into_owned();
    let raw = raw.trim();
    if raw.is_empty() {
        return -1;
    }
    let base = crate::wsabi::effective_root(ctx);
    let cand = std::path::Path::new(raw);
    let target = if cand.is_absolute() { cand.to_path_buf() } else { base.join(cand) };
    save_active_to_path(ctx, target)
}

/// Save-As through a native Windows save-file picker. Returns `0` on success or
/// `-1` when cancelled/unavailable/failed, letting Mighty keep a fallback prompt.
#[no_mangle]
pub extern "C" fn mui_save_as_dialog(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let root = crate::wsabi::effective_root(ctx);
    let suggested = ctx
        .tabs
        .active_path()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "untitled.mty".to_string());
    let Some(target) = pick_save_file_native(&root, &suggested) else {
        println!("mui_save_as_dialog: native save dialog cancelled / unavailable");
        return -1;
    };
    save_active_to_path(ctx, target)
}

fn save_active_to_path(ctx: &mut MuiContext, target: PathBuf) -> i32 {
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bytes = save_bytes_for_active(ctx);
    let name = basename(&target);
    match std::fs::write(&target, &bytes) {
        Ok(()) => {
            ctx.tabs.set_active_path(target.clone());
            ctx.language = crate::langdetect::detect_path(&target);
            ctx.file_path = Some(target);
            ctx.tabs.active_model_mut().mark_clean();
            ctx.autosave.disarm();
            ctx.tree.refresh();
            ctx.push_toast(crate::toast::Kind::Success, format!("Saved {name}"));
            0
        }
        Err(e) => {
            eprintln!("mui_save_as({}): {e}", target.display());
            ctx.push_toast(crate::toast::Kind::Error, format!("Save failed: {name}"));
            -1
        }
    }
}

fn pick_open_file_native(initial_dir: &std::path::Path) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MUI_OPEN_FILE_PICK") {
        let trimmed = path.trim();
        return (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
    }
    if !cfg!(windows) {
        return None;
    }
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms | Out-Null
$d = New-Object System.Windows.Forms.OpenFileDialog
$d.Title = 'Open File'
$d.Filter = 'All files (*.*)|*.*'
$dir = $env:MUI_DIALOG_DIR
if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) { $d.InitialDirectory = $dir }
if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }
"#;
    run_file_dialog_script(script, initial_dir, None)
}

fn pick_save_file_native(initial_dir: &std::path::Path, suggested_name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MUI_SAVE_FILE_PICK") {
        let trimmed = path.trim();
        return (!trimmed.is_empty()).then(|| PathBuf::from(trimmed));
    }
    if !cfg!(windows) {
        return None;
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
if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }
"#;
    run_file_dialog_script(script, initial_dir, Some(suggested_name))
}

fn run_file_dialog_script(
    script: &str,
    initial_dir: &std::path::Path,
    suggested_name: Option<&str>,
) -> Option<PathBuf> {
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-STA", "-Command", script])
        .env("MUI_DIALOG_DIR", initial_dir)
        .stdin(std::process::Stdio::null());
    if let Some(name) = suggested_name {
        cmd.env("MUI_DIALOG_FILE", name);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

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
    ctx.find.run(&needle)
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
    let prefix = ctx.complete.prefix_len();
    let accepted = ctx.complete.accepted_text().to_string();
    let m = ctx.tabs.active_model_mut();
    for _ in 0..prefix {
        m.backspace();
    }
    for ch in accepted.chars() {
        m.insert_char(ch);
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
        // The quick-fix lightbulb tracked the OLD buffer's line; reset it so it
        // re-probes against the new active buffer rather than lingering.
        ctx.lightbulb.reset();
    }
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
                false,
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
    let win_w = ctx.gpu.width as f32;
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
    // suppressed on UNFOCUSED panes (focused-only is fine for v1) and on every
    // pane that is too narrow to host it.
    let pane_w = (x_right - region.left).max(0.0);
    let minimap_on = crate::settings::minimap()
        && (!split_chrome || focused)
        && pane_w > 200.0;
    let mm_w = if minimap_on { 70.0_f32 } else { 0.0_f32 };
    let mm_x = win_w - mm_w;

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
        let num_w = num.chars().count() as f32 * layout::CHAR_W() * (chrome / theme::FONT_SIZE());
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
                        clip,
                    );
                } else {
                    ctx.text.queue(x, y, &frag, sp.color, clip);
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
                    let pill_w = (label.chars().count() as f32) * (chrome * 0.55) + 12.0;
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
    let cur_tab = ctx.panes.focused_tab();
    let s = active_scroll(ctx);
    ctx.panes.split_right(cur_tab, s);
    pane_rebind_focus(ctx);
    ctx.welcome.dismiss();
    ctx.panes.count() as i32
}

/// Cycle focus to the next pane (wraps); rebinds the active tab + restores the
/// newly focused pane's scroll. No-op with one pane. Returns the focused index.
#[no_mangle]
pub extern "C" fn mui_pane_focus_next(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let s = active_scroll(ctx);
    ctx.panes.focus_next(s);
    pane_rebind_focus(ctx);
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
    let win_w = ctx.gpu.width as f32;
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
    let s = active_scroll(ctx);
    ctx.panes.save_focused_scroll(s);
    ctx.panes.close_focused();
    pane_rebind_focus(ctx);
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
    let win_h = ctx.gpu.height as f32;
    // Move the preview state out so we can borrow `ctx` mutably for drawing.
    let mut preview = std::mem::take(&mut ctx.md_preview);
    preview.draw(ctx, &source, region, x_right, win_h);
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
#[no_mangle]
pub extern "C" fn mui_md_close(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    ctx.md_preview.close();
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
/// per active buffer).
#[no_mangle]
pub extern "C" fn mui_ed_undo_reset(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.ed_undo.clear();
        ctx.ed_redo.clear();
    }
}

/// Push the CURRENT active model as an undo checkpoint (call before an edit
/// group). Clears the redo stack. Coalesces no-op duplicates.
#[no_mangle]
pub extern "C" fn mui_ed_undo_record(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        let snap = ctx.tabs.active_model().clone();
        // Skip if identical to the most recent checkpoint.
        if let Some(last) = ctx.ed_undo.last() {
            if last.as_text() == snap.as_text() {
                return;
            }
        }
        ctx.ed_undo.push(snap);
        if ctx.ed_undo.len() > ED_UNDO_CAP {
            ctx.ed_undo.remove(0);
        }
        ctx.ed_redo.clear();
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
    match ctx.ed_undo.pop() {
        Some(prev) => {
            let current = ctx.tabs.active_model().clone();
            ctx.ed_redo.push(current);
            *ctx.tabs.active_model_mut() = prev;
            1
        }
        None => 0,
    }
}

/// Redo: restore the most recent redo checkpoint, pushing the current state back
/// onto the undo stack. Returns `1` on success, `0` if nothing to redo.
#[no_mangle]
pub extern "C" fn mui_ed_redo(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    match ctx.ed_redo.pop() {
        Some(next) => {
            let current = ctx.tabs.active_model().clone();
            ctx.ed_undo.push(current);
            *ctx.tabs.active_model_mut() = next;
            1
        }
        None => 0,
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
pub extern "C" fn mui_ed_toggle_comment(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.toggle_line_comment();
    }
}

// ---- Feature 2: auto-indent on Enter ----

/// Insert a newline that copies the leading whitespace (and adds/removes one
/// indent level for `{` / `}`). The IDE routes Enter here instead of the plain
/// `mui_ed_newline`.
#[no_mangle]
pub extern "C" fn mui_ed_newline_indent(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.newline_auto_indent();
    }
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
pub extern "C" fn mui_ed_duplicate(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.duplicate();
    }
}

/// Move the current line / selected line range up by one.
#[no_mangle]
pub extern "C" fn mui_ed_move_lines_up(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_lines_up();
    }
}

/// Move the current line / selected line range down by one.
#[no_mangle]
pub extern "C" fn mui_ed_move_lines_down(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.move_lines_down();
    }
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

/// Select the word under the cursor. Returns its char length.
#[no_mangle]
pub extern "C" fn mui_ed_select_word(handle: i64) -> i32 {
    if let Some(m) = unsafe { model_mut(handle) } {
        return m.select_word().chars().count() as i32;
    }
    0
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
    if let Some(m) = unsafe { model_mut(handle) } {
        return i32::from(m.add_caret_next_occurrence());
    }
    0
}

/// Ctrl+Alt+Up: add a column-block caret on the line above the primary caret.
/// Returns `1` if added, `0` at the top edge.
#[no_mangle]
pub extern "C" fn mui_ed_add_caret_above(handle: i64) -> i32 {
    if let Some(m) = unsafe { model_mut(handle) } {
        return i32::from(m.add_caret_vertical(-1));
    }
    0
}

/// Ctrl+Alt+Down: add a column-block caret on the line below the primary caret.
/// Returns `1` if added, `0` at the bottom edge.
#[no_mangle]
pub extern "C" fn mui_ed_add_caret_below(handle: i64) -> i32 {
    if let Some(m) = unsafe { model_mut(handle) } {
        return i32::from(m.add_caret_vertical(1));
    }
    0
}

/// Esc: collapse to the primary caret only and clear its selection.
#[no_mangle]
pub extern "C" fn mui_ed_collapse_carets(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.collapse_carets();
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
}

// ---- multi-caret edit / motion entry points (apply at EVERY caret) ----

/// Insert one scalar at every caret (a `\n` codepoint splits at each).
#[no_mangle]
pub extern "C" fn mui_ed_insert_char_multi(handle: i64, cp: i32) {
    if let Some(m) = unsafe { model_mut(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            m.insert_char_multi(ch);
        }
    }
}

/// Smart insert (auto-close/skip-over) at every caret, falling back to a plain
/// insert where the smart path declined. Replaces the Mighty-side
/// smart/plain branch when multiple carets are active.
#[no_mangle]
pub extern "C" fn mui_ed_insert_smart_multi(handle: i64, cp: i32) {
    trace(&format!("ed_insert_smart_multi cp={cp}"));
    if let Some(m) = unsafe { model_mut(handle) } {
        if let Some(ch) = u32::try_from(cp).ok().and_then(char::from_u32) {
            m.insert_char_smart_multi(ch);
        }
    }
}

/// Backspace at every caret (smart pair-delete where applicable).
#[no_mangle]
pub extern "C" fn mui_ed_backspace_multi(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.backspace_multi();
    }
}

/// Delete-forward at every caret.
#[no_mangle]
pub extern "C" fn mui_ed_delete_multi(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.delete_multi();
    }
}

/// Newline + auto-indent at every caret.
#[no_mangle]
pub extern "C" fn mui_ed_newline_indent_multi(handle: i64) {
    if let Some(m) = unsafe { model_mut(handle) } {
        m.newline_indent_multi();
    }
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

/// Close the replace bar (clears its fields).
#[no_mangle]
pub extern "C" fn mui_replace_cancel(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.replace_bar.cancel();
    }
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
    let repl = ctx.replace_bar.repl_string();
    i32::from(ctx.tabs.active_model_mut().replace_next(&needle, &repl))
}

/// Replace ALL occurrences of the find field with the replace field in the
/// active model. Returns the replacement count.
#[no_mangle]
pub extern "C" fn mui_replace_all(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let needle = ctx.replace_bar.find_string();
    let repl = ctx.replace_bar.repl_string();
    ctx.tabs.active_model_mut().replace_all(&needle, &repl) as i32
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
    let w = ctx.gpu.width as f32;
    let h = ctx.gpu.height as f32;
    let bar_h = layout::LINE_H();
    // Two rows above the 30px status bar.
    let top = (h - 30.0 - 2.0 * bar_h).max(0.0);
    let chrome = theme::CHROME_FONT_SIZE;
    let clip = ctx.clip;
    let left = layout::region(ctx.sidebar_visible).left;
    let text_x = left + layout::PAD + 12.0;
    let find_line = ctx.replace_bar.display_find();
    let repl_line = ctx.replace_bar.display_replace();
    let repl_focus = ctx.replace_bar.replace_focus() == 1;

    let handle_ptr = handle as usize as *mut MuiContext;
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
    ctx.text.queue_sized(text_x, fy, &find_line, theme::TEXT(), chrome, clip);
    ctx.text.queue_sized(text_x, ry, &repl_line, theme::TEXT(), chrome, clip);
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
    if ctx.welcome.force_open {
        return 1;
    }
    // "No file open": the active tab has no path and the buffer is empty.
    let no_path = ctx.tabs.active_path().is_none();
    let model = ctx.tabs.active_model();
    let empty = model.line_count() <= 1 && model.line_len(0) == 0;
    if no_path && empty && !ctx.welcome.hides_empty_auto() {
        1
    } else {
        0
    }
}

/// Force the Welcome screen open (the palette "Welcome" command).
#[no_mangle]
pub extern "C" fn mui_welcome_open(handle: i64) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.welcome.open();
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

/// Draw the Welcome screen over the editor body region. No-op work is fine to
/// call unconditionally; the Mighty side only calls it when `mui_welcome_active`.
#[no_mangle]
pub extern "C" fn mui_welcome_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    let region = layout::region(ctx.sidebar_visible);
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    // Take the recents snapshot out so we can borrow `ctx` mutably for the draw
    // (the MRU lives in the Quick-Open engine).
    let recents: Vec<std::path::PathBuf> = ctx.quickopen.recent_paths();
    let folders: Vec<std::path::PathBuf> = ctx.recent_workspaces.entries().to_vec();
    let mut welcome = std::mem::take(&mut ctx.welcome);
    welcome.draw(ctx, region.left, region.top, w, h, &recents, &folders);
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
        return -1;
    }
    let Some(path) = ctx.welcome.recent_path(i as usize).cloned() else {
        return -1;
    };
    let idx = ctx.tabs.open_path(path.clone());
    ctx.welcome.dismiss();
    sync_active_path(ctx);
    ctx.quickopen.record_mru(path);
    idx as i32
}

/// Open a Welcome RECENT-FOLDER row (`i = action - ACTION_RECENT_FOLDER_BASE`) as
/// the workspace (re-rooting the tree/index/search/git/agents). Returns `1` on
/// success, `0` if the row/path is invalid.
#[no_mangle]
pub extern "C" fn mui_welcome_open_folder(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if i < 0 {
        return 0;
    }
    let Some(path) = ctx.welcome.recent_folder(i as usize).cloned() else {
        return 0;
    };
    ctx.welcome.dismiss();
    crate::wsabi::mui_ws_open_recent_path(ctx, &path)
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
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    let was_overlay = ctx.overlay;
    ctx.overlay = true;
    ctx.text.set_overlay(true);
    let toasts = std::mem::take(&mut ctx.toasts);
    toasts.draw(ctx, w, h);
    ctx.toasts = toasts;
    ctx.overlay = was_overlay;
    ctx.text.set_overlay(was_overlay);
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
            ctx.push_toast(crate::toast::Kind::Info, "Zen mode on \u{2014} Ctrl+K Z to exit");
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
