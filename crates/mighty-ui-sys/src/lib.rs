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
mod ffi;
mod gpu;
mod layout;
mod text;
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

    let text = Text::new(&gpu.device, &gpu.queue, gpu.format);

    let ctx = Box::new(MuiContext {
        gpu,
        text,
        queue,
        host: Some(host),
        window: Some(window),
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
        })
    }

    fn read_pixels(&self) -> Vec<u8> {
        self.gpu.read_pixels().expect("offscreen read_pixels")
    }
}
