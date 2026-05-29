//! Mighty IDE native render shim — flat C ABI (`mui_*`).
//!
//! The shim owns the window, GPU surface, and text rendering; the Mighty IDE
//! owns the main loop and drives the shim each frame via these `extern "C"`
//! entry points. The shim NEVER calls back into Mighty (poll/pump model).
//!
//! Module layout:
//! * [`ffi`]    — `#[repr(C)]` types ([`MuiColor`], [`MuiEvent`], key/mouse codes)
//! * [`gpu`]    — wgpu device/surface + solid-rect pipeline (+ offscreen target)
//! * [`text`]   — glyphon font system, atlas, renderer, measurement
//! * [`window`] — winit window + `pump_events` + event-queue translation

mod abi;
mod completion;
mod diagnostics;
mod editor;
mod ffi;
mod format;
mod gpu;
mod history;
mod layout;
mod nav;
mod palette;
mod prompt;
mod screenshot;
mod syntax;
mod tabs;
mod terminal;
mod text;
mod theme;
mod tree;
mod window;

pub use abi::*;
pub use ffi::*;

use std::path::PathBuf;

use std::sync::Arc;

use gpu::{Gpu, RectInstance, RenderTarget};
use text::Text;
use window::{EventQueue, WindowHost};

/// Smoke export retained from the spike: proves the staticlib links.
#[no_mangle]
pub extern "C" fn mui_smoke_add(a: i32, b: i32) -> i32 {
    a + b
}

/// Opaque handle returned to the IDE. Layout is private to Rust.
pub struct MuiContext {
    gpu: Gpu,
    text: Text,
    queue: Box<EventQueue>,
    /// `None` in offscreen/headless mode.
    host: Option<WindowHost>,
    #[allow(dead_code)]
    window: Option<Arc<winit::window::Window>>,

    // ---- per-frame state ----
    rects: Vec<RectInstance>,
    clip: Option<(u32, u32, u32, u32)>,
    /// Surface frame held between begin/end in windowed mode.
    frame: Option<wgpu::SurfaceTexture>,
    frame_view: Option<wgpu::TextureView>,
    in_frame: bool,

    // ---- scalar-ABI staging state (see abi.rs) ----
    /// Text accumulated codepoint-by-codepoint before a `mui_text_draw`.
    text_stage: String,
    /// Last event delivered by `mui_poll_event_s`, read by scalar accessors.
    last_event: MuiEvent,
    /// Configured source/target file path (shim owns file I/O).
    file_path: Option<PathBuf>,
    /// Path bytes staged before `mui_path_commit`.
    path_stage: Vec<u8>,
    /// Bytes of the most recently loaded file (read by index).
    load_buf: Vec<u8>,
    /// Bytes staged for `mui_save_commit`.
    save_buf: Vec<u8>,
    /// Latest parsed diagnostics from `mty check` (refreshed on demand).
    diags: Vec<diagnostics::Diag>,

    // ---- editor-feature state (status bar, prompt, find) ----
    /// Basename of the edited file, drawn in the status bar.
    file_name: String,
    /// 1-based cursor (line, col) fed each frame for the status bar.
    status_cursor: (i32, i32),
    /// Bottom one-line prompt buffer (goto / find), shim-owned (L17).
    prompt: prompt::PromptState,
    /// Find-search engine over the buffer streamed in from Mighty.
    find: prompt::FindState,

    // ---- multi-file workspace state (tabs + file tree) ----
    /// Open tabs + per-tab cursor/scroll/dirty state (shim-owned, L17).
    tabs: tabs::TabStore,
    /// File-tree sidebar model.
    tree: tree::FileTree,
    /// Whether the sidebar is currently shown (toggled by Ctrl+B).
    sidebar_visible: bool,

    // ---- integrated terminal state ----
    /// The PTY-backed terminal, lazily spawned on first open (`mui_term_open`).
    /// `None` until opened or after the shell exits + the panel is closed.
    terminal: Option<terminal::Terminal>,
    /// Whether the terminal panel is currently shown (toggled by Ctrl+`).
    term_open: bool,

    // ---- autocomplete state ----
    /// The completion dropdown engine (candidate list + selection), shim-owned.
    complete: completion::CompletionEngine,
    /// Editor buffer bytes streamed in from Mighty for a completion request
    /// (mirrors the find streaming path — Mighty can't pass a buffer, L17).
    complete_buf: Vec<u8>,

    // ---- hover + go-to-definition state ----
    /// The hover popup (wrapped text + active flag), shim-owned.
    hover: nav::HoverState,
    /// The most recent resolved definition target (path + 0-based line/col).
    def: nav::DefState,
    /// Editor buffer bytes streamed in from Mighty for a hover/def request
    /// (same shape as `complete_buf`; the live unsaved source is the doc text).
    nav_buf: Vec<u8>,

    // ---- undo / redo history (shim-owned, L21) ----
    /// Undo + redo stacks of full buffer snapshots for the active tab. Mighty
    /// streams its post-edit buffer in (reusing the byte-streaming path) and the
    /// shim coalesces typing runs / decides whether to push. Re-seeded on load /
    /// tab switch so history is per-active-buffer.
    history: history::HistoryStore,
    /// Restored cursor from the most recent `mui_undo` / `mui_redo`, read back by
    /// Mighty via `mui_undo_cursor_line` / `_col` after pulling the bytes.
    restored_cursor: (i32, i32),

    // ---- command palette (Ctrl+Shift+P) ----
    /// The command palette overlay (registry + query/filter + selection),
    /// shim-owned. Mighty opens it, feeds chars, moves the selection, and reads
    /// the selected command id back to dispatch.
    palette: palette::PaletteEngine,

    // ---- offscreen screenshot mode (MUI_SCREENSHOT) ----
    /// When `Some`, the context renders into an offscreen texture (no window)
    /// and writes a PNG of the configured frame, then asks the loop to exit.
    /// `None` for normal windowed runs (behavior unchanged).
    screenshot: Option<screenshot::ScreenshotState>,

    // ---- live editor model undo/redo (shim-side; L28 workaround) ----
    /// Undo/redo of full [`editor::TextModel`] snapshots for the ACTIVE tab.
    /// Since the editable buffer now lives shim-side (L28), undo also lives
    /// here: `mui_ed_undo_record` pushes the current model, `mui_ed_undo`/`_redo`
    /// restore one. Reset on load / tab switch (history is per active buffer).
    ed_undo: Vec<editor::TextModel>,
    ed_redo: Vec<editor::TextModel>,
    /// When set by `MUI_EDIT_PROBE`, [`mui_ed_load`] becomes a no-op so the
    /// scripted-edit model survives the IDE's initial load — letting a headless
    /// screenshot capture the LIVE-edited buffer (screenshots/06-edit.png).
    pub(crate) edit_probe_lock: bool,
}

// ---------------------------------------------------------------------------
// 2.1 — init / shutdown
// ---------------------------------------------------------------------------

/// Initialize the shim: open a window of `width`x`height` titled by the UTF-8
/// bytes at `title_ptr`/`title_len`, and set up GPU + text. Returns an opaque
/// context pointer, or null on failure.
///
/// # Safety
/// `title_ptr` must point to `title_len` valid bytes (or be null with len 0).
#[no_mangle]
pub unsafe extern "C" fn mui_init(
    width: u32,
    height: u32,
    title_ptr: *const u8,
    title_len: usize,
) -> *mut MuiContext {
    let title = read_utf8(title_ptr, title_len).unwrap_or_else(|| "Mighty IDE".to_string());
    build_context(width, height, title, None)
}

/// Build a windowed context with an explicit window `title` and an optional
/// pre-resolved `file_path`. Shared by [`mui_init`] and [`abi::mui_init_s`].
/// Returns null on window/GPU failure.
pub(crate) fn build_context(
    width: u32,
    height: u32,
    title: String,
    file_path: Option<PathBuf>,
) -> *mut MuiContext {
    let mut queue = Box::new(EventQueue::default());
    let queue_ptr: *mut EventQueue = queue.as_mut();

    // Screenshot mode: render headless into an offscreen texture (no window /
    // surface) and capture a PNG. The window dimensions can be overridden via
    // MUI_SCREENSHOT_W / MUI_SCREENSHOT_H so a faithful large frame is captured
    // regardless of the dimensions the Mighty side passes to mui_init_s.
    let screenshot = screenshot::ScreenshotState::from_env();

    let (host, window, gpu) = if screenshot.is_some() {
        let sw = std::env::var("MUI_SCREENSHOT_W")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(width);
        let sh = std::env::var("MUI_SCREENSHOT_H")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(height);
        let gpu = match Gpu::new_offscreen(sw, sh) {
            Ok(Some(g)) => g,
            Ok(None) => {
                eprintln!("mui_init: MUI_SCREENSHOT set but no GPU adapter available");
                return std::ptr::null_mut();
            }
            Err(e) => {
                eprintln!("mui_init: offscreen init failed: {e}");
                return std::ptr::null_mut();
            }
        };
        (None, None, gpu)
    } else {
        let (host, window) = match WindowHost::create(width, height, title, queue_ptr) {
            Ok((h, w)) => (h, w),
            Err(e) => {
                eprintln!("mui_init: {e}");
                return std::ptr::null_mut();
            }
        };
        let gpu = match Gpu::new_windowed(window.clone()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("mui_init: {e}");
                return std::ptr::null_mut();
            }
        };
        (Some(host), Some(window), gpu)
    };

    let text = Text::new(&gpu.device, &gpu.queue, gpu.format);

    let file_name = file_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Seed the tab store with the initial file as tab 0 (or a scratch tab), and
    // root the file tree at that file's directory (or the cwd).
    let mut tab_store = tabs::TabStore::new();
    if let Some(p) = file_path.clone() {
        tab_store.open_path(p);
    } else {
        tab_store.ensure_scratch();
    }
    let tree_root = file_path
        .as_ref()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .filter(|d| !d.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let mut file_tree = tree::FileTree::new();
    file_tree.set_root(tree_root);

    let ctx = Box::new(MuiContext {
        gpu,
        text,
        queue,
        host,
        window,
        rects: Vec::new(),
        clip: None,
        frame: None,
        frame_view: None,
        in_frame: false,
        text_stage: String::new(),
        last_event: MuiEvent::none(),
        file_path,
        path_stage: Vec::new(),
        load_buf: Vec::new(),
        save_buf: Vec::new(),
        diags: Vec::new(),
        file_name,
        status_cursor: (1, 1),
        prompt: prompt::PromptState::new(),
        find: prompt::FindState::new(),
        tabs: tab_store,
        tree: file_tree,
        sidebar_visible: true,
        terminal: None,
        term_open: false,
        complete: completion::CompletionEngine::new(),
        complete_buf: Vec::new(),
        hover: nav::HoverState::new(),
        def: nav::DefState::new(),
        nav_buf: Vec::new(),
        history: history::HistoryStore::new(),
        restored_cursor: (0, 0),
        palette: palette::PaletteEngine::new(),
        screenshot,
        ed_undo: Vec::new(),
        ed_redo: Vec::new(),
        edit_probe_lock: false,
    });
    Box::into_raw(ctx)
}

/// Tear down the shim and free the context.
///
/// # Safety
/// `ctx` must be a pointer previously returned by `mui_init` and not freed.
#[no_mangle]
pub unsafe extern "C" fn mui_shutdown(ctx: *mut MuiContext) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

// ---------------------------------------------------------------------------
// 2.2 — rects
// ---------------------------------------------------------------------------

/// Queue a solid-color rectangle (pixel space) for the current frame.
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_fill_rect(
    ctx: *mut MuiContext,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: MuiColor,
) {
    let Some(ctx) = ctx.as_mut() else { return };
    ctx.rects.push(RectInstance {
        pos: [x, y],
        size: [w, h],
        color: [color.r, color.g, color.b, color.a],
    });
}

// ---------------------------------------------------------------------------
// 2.3 — text
// ---------------------------------------------------------------------------

/// Queue UTF-8 text at (`x`,`y`) in the given color for the current frame.
///
/// # Safety
/// `ctx` valid; `utf8_ptr` points to `len` valid bytes.
#[no_mangle]
pub unsafe extern "C" fn mui_draw_text(
    ctx: *mut MuiContext,
    x: f32,
    y: f32,
    utf8_ptr: *const u8,
    len: usize,
    color: MuiColor,
) {
    let Some(ctx) = ctx.as_mut() else { return };
    let Some(text) = read_utf8(utf8_ptr, len) else {
        return;
    };
    let clip = ctx.clip;
    ctx.text.queue(x, y, &text, color, clip);
}

/// Measure UTF-8 text, writing pixel `width`/`height` to the out-params.
/// Returns `true` on success.
///
/// # Safety
/// `ctx` valid; `utf8_ptr` points to `len` valid bytes; `out_w`/`out_h` valid.
#[no_mangle]
pub unsafe extern "C" fn mui_text_measure(
    ctx: *mut MuiContext,
    utf8_ptr: *const u8,
    len: usize,
    out_w: *mut f32,
    out_h: *mut f32,
) -> bool {
    let Some(ctx) = ctx.as_mut() else { return false };
    let Some(text) = read_utf8(utf8_ptr, len) else {
        return false;
    };
    let (w, h) = ctx.text.measure(&text);
    if !out_w.is_null() {
        *out_w = w;
    }
    if !out_h.is_null() {
        *out_h = h;
    }
    true
}

// ---------------------------------------------------------------------------
// 2.4 — frame lifecycle + clip
// ---------------------------------------------------------------------------

/// Begin a frame: acquire the surface texture (windowed) and reset draw state.
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_begin_frame(ctx: *mut MuiContext) {
    let Some(ctx) = ctx.as_mut() else { return };
    ctx.rects.clear();
    ctx.clip = None;
    ctx.text.begin();
    ctx.in_frame = true;

    if let RenderTarget::Surface(surface) = &ctx.gpu.target {
        match surface.get_current_texture() {
            Ok(frame) => {
                let view = frame.texture.create_view(&Default::default());
                ctx.frame = Some(frame);
                ctx.frame_view = Some(view);
            }
            Err(_) => {
                // Surface lost/outdated: reconfigure and try once more.
                let (w, h) = (ctx.gpu.width, ctx.gpu.height);
                ctx.gpu.resize(w, h);
                if let RenderTarget::Surface(surface) = &ctx.gpu.target {
                    if let Ok(frame) = surface.get_current_texture() {
                        let view = frame.texture.create_view(&Default::default());
                        ctx.frame = Some(frame);
                        ctx.frame_view = Some(view);
                    }
                }
            }
        }
    }
}

/// Set a scissor clip rect (pixels) applied to subsequent draws this frame.
/// (Width/height of 0 are allowed and clip everything.)
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_set_clip(ctx: *mut MuiContext, x: u32, y: u32, w: u32, h: u32) {
    let Some(ctx) = ctx.as_mut() else { return };
    ctx.clip = Some((x, y, w, h));
}

/// End the frame: submit rects then text, and present (windowed).
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_end_frame(ctx: *mut MuiContext) {
    let Some(ctx) = ctx.as_mut() else { return };
    if !ctx.in_frame {
        return;
    }
    render_and_present(ctx);
    ctx.in_frame = false;
}

/// Shared by `mui_end_frame` and tests: encode rects + text and submit.
fn render_and_present(ctx: &mut MuiContext) {
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    // Determine the view to render into. Both the surface frame view and the
    // offscreen view are owned elsewhere (ctx.frame_view / ctx.gpu.target), so
    // a borrow suffices.
    let view: &wgpu::TextureView = match (&ctx.frame_view, &ctx.gpu.target) {
        (Some(v), _) => v,
        (None, RenderTarget::Offscreen { view, .. }) => view,
        (None, RenderTarget::Surface(_)) => return, // no frame acquired
    };

    let mut encoder = ctx
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mui frame encoder"),
        });

    // Rect pass (clears the target).
    ctx.gpu
        .render_rects(&mut encoder, view, &ctx.rects, true, ctx.clip);

    // Text pass (loads, draws on top). Errors are logged, not fatal.
    if let Err(e) = ctx
        .text
        .render(&ctx.gpu.device, &ctx.gpu.queue, &mut encoder, view, w, h)
    {
        eprintln!("mui_end_frame: {e}");
    }

    ctx.gpu.queue.submit([encoder.finish()]);

    if let Some(frame) = ctx.frame.take() {
        frame.present();
    }
    ctx.frame_view = None;

    // Screenshot mode: on the configured frame, read the offscreen texture back
    // and write a PNG. Done after every other draw call this frame, so the PNG
    // is a faithful capture of the full UI. The next poll returns Close.
    maybe_capture_screenshot(ctx);
}

/// In screenshot mode, count frames and capture the configured one to a PNG.
/// No-op in windowed mode (`ctx.screenshot` is `None`).
fn maybe_capture_screenshot(ctx: &mut MuiContext) {
    let Some(shot) = ctx.screenshot.as_mut() else {
        return;
    };
    if shot.captured {
        return;
    }
    let this_frame = shot.frame;
    shot.frame += 1;
    if this_frame != shot.target_frame {
        return;
    }

    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    match ctx.gpu.read_pixels() {
        Some(pixels) => {
            let shot = ctx.screenshot.as_mut().unwrap();
            match screenshot::write_png(&shot.out_path, w, h, &pixels) {
                Ok(bytes) => println!(
                    "mui_screenshot: wrote {} ({w}x{h}, {bytes} bytes, frame {})",
                    shot.out_path.display(),
                    shot.target_frame
                ),
                Err(e) => eprintln!("mui_screenshot: {e}"),
            }
            shot.captured = true;
        }
        None => {
            eprintln!("mui_screenshot: read_pixels returned None (not offscreen?)");
            ctx.screenshot.as_mut().unwrap().captured = true;
        }
    }
}

// ---------------------------------------------------------------------------
// 2.5 — event pump
// ---------------------------------------------------------------------------

/// Pump OS events (windowed) then pop one queued event into `*out_ev`.
/// Returns `true` if an event was written, `false` when the queue is empty.
///
/// # Safety
/// `ctx` valid; `out_ev` points to a writable `MuiEvent`.
#[no_mangle]
pub unsafe extern "C" fn mui_poll_event(ctx: *mut MuiContext, out_ev: *mut MuiEvent) -> bool {
    let Some(ctx) = ctx.as_mut() else { return false };

    // Screenshot mode: once the target frame has been captured, deliver a single
    // Close event so the Mighty frame loop exits promptly. Until then, report
    // "no event" so the loop keeps drawing full frames into the offscreen target.
    if let Some(shot) = ctx.screenshot.as_ref() {
        if shot.captured {
            if !out_ev.is_null() {
                *out_ev = MuiEvent::close();
            }
            return true;
        }
        return false;
    }

    // Pump the OS event loop only when there is nothing buffered, so a backlog
    // is drained FIFO before new OS events are folded in.
    if ctx.queue.is_empty() {
        if let Some(host) = ctx.host.as_mut() {
            host.pump();
        }
    }

    // Apply any pending resize (reconfigure the surface) before delivery.
    if let Some((w, h)) = ctx.queue.pending_resize.take() {
        ctx.gpu.resize(w, h);
    }

    match ctx.queue.pop() {
        Some(ev) => {
            if !out_ev.is_null() {
                *out_ev = ev;
            }
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Read `len` bytes at `ptr` as a UTF-8 `String` (lossy). `None` if invalid.
unsafe fn read_utf8(ptr: *const u8, len: usize) -> Option<String> {
    if len == 0 {
        return Some(String::new());
    }
    if ptr.is_null() {
        return None;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    Some(String::from_utf8_lossy(slice).into_owned())
}

// ---------------------------------------------------------------------------
// test-only offscreen helpers (no winit window/surface)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
impl MuiContext {
    /// Build a headless context backed by an offscreen texture. Returns `None`
    /// when no GPU adapter is available (so tests can skip).
    fn new_offscreen(width: u32, height: u32) -> Option<Self> {
        let gpu = match Gpu::new_offscreen(width, height) {
            Ok(Some(g)) => g,
            Ok(None) => return None,
            Err(e) => {
                eprintln!("offscreen init failed: {e}");
                return None;
            }
        };
        let text = Text::new(&gpu.device, &gpu.queue, gpu.format);
        // Seed a scratch tab so the active editor model is always present (the
        // real `build_context` does this; tests build the context directly).
        let mut tabs = tabs::TabStore::new();
        tabs.ensure_scratch();
        Some(MuiContext {
            gpu,
            text,
            queue: Box::new(EventQueue::default()),
            host: None,
            window: None,
            rects: Vec::new(),
            clip: None,
            frame: None,
            frame_view: None,
            in_frame: false,
            text_stage: String::new(),
            last_event: MuiEvent::none(),
            file_path: None,
            path_stage: Vec::new(),
            load_buf: Vec::new(),
            save_buf: Vec::new(),
            diags: Vec::new(),
            file_name: String::new(),
            status_cursor: (1, 1),
            prompt: prompt::PromptState::new(),
            find: prompt::FindState::new(),
            tabs,
            tree: tree::FileTree::new(),
            sidebar_visible: true,
            terminal: None,
            term_open: false,
            complete: completion::CompletionEngine::new(),
            complete_buf: Vec::new(),
            hover: nav::HoverState::new(),
            def: nav::DefState::new(),
            nav_buf: Vec::new(),
            history: history::HistoryStore::new(),
            restored_cursor: (0, 0),
            palette: palette::PaletteEngine::new(),
            screenshot: None,
            ed_undo: Vec::new(),
            ed_redo: Vec::new(),
            edit_probe_lock: false,
        })
    }

    fn read_pixels(&self) -> Vec<u8> {
        self.gpu.read_pixels().expect("offscreen read_pixels")
    }
}
