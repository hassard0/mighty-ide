//! Offscreen screenshot mode (`MUI_SCREENSHOT=<out.png>`).
//!
//! When the env var is set, [`crate::build_context`] builds an **offscreen**
//! GPU context (an owned `wgpu::Texture` with `RENDER_ATTACHMENT | COPY_SRC`)
//! instead of a real winit window + surface, reusing the same render-to-texture
//! path the GPU tests use. The Mighty frame loop then draws its full UI into the
//! texture exactly as it would on screen — the screenshot comes from the SAME
//! draw calls the live app issues.
//!
//! On the configured target frame, [`crate::render_and_present`] reads the
//! texture back as RGBA8 (handling wgpu's 256-byte row padding in
//! [`crate::gpu::Gpu::read_pixels`]) and writes it out as a PNG here. The next
//! `mui_poll_event` then returns a `Close` event so the process exits promptly.
//!
//! Windowed (non-screenshot) behavior is completely unchanged: nothing in this
//! module runs unless `MUI_SCREENSHOT` is set at init.

use std::path::PathBuf;

/// Default target frame: small, so text/layout/diagnostics have settled but we
/// still exit fast. Overridable with `MUI_SCREENSHOT_FRAME`.
const DEFAULT_TARGET_FRAME: u32 = 3;

/// Per-context screenshot state, present only when `MUI_SCREENSHOT` is set.
pub struct ScreenshotState {
    /// Where the PNG is written.
    pub out_path: PathBuf,
    /// 0-based frame index to capture.
    pub target_frame: u32,
    /// Frames rendered so far.
    pub frame: u32,
    /// Set once the PNG has been written; the loop should then exit.
    pub captured: bool,
}

impl ScreenshotState {
    /// Build state from the environment, or `None` if `MUI_SCREENSHOT` is unset
    /// or empty.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var_os("MUI_SCREENSHOT")?;
        let s = raw.to_string_lossy();
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let target_frame = std::env::var("MUI_SCREENSHOT_FRAME")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_TARGET_FRAME);
        Some(ScreenshotState {
            out_path: PathBuf::from(s.to_string()),
            target_frame,
            frame: 0,
            captured: false,
        })
    }
}

/// Encode tightly-packed RGBA8 `pixels` (`width`x`height`) to a PNG at `path`.
/// Returns the number of bytes written on success.
pub fn write_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<u64, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let file = std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("png header: {e}"))?;
    writer
        .write_image_data(pixels)
        .map_err(|e| format!("png data: {e}"))?;
    drop(writer);
    let n = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(n)
}
