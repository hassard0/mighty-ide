//! Global UI scale (OS DPI scale × user zoom).
//!
//! The whole IDE lays out in **logical pixels** (the `layout`/`theme` metrics are
//! all in logical px). The window surface, however, is sized in **physical
//! pixels** (`window.inner_size()` is physical, e.g. 1920×1248 at 150% scaling).
//!
//! Rather than thread a scale factor through ~40 draw/click sites, we keep one
//! process-global factor here and apply it in exactly THREE places:
//!
//! * the rect projection (`gpu`): the screen-size uniform is the **logical** size
//!   (`physical / scale`), so logical-pixel rect coords fill the physical surface
//!   → the whole chrome scales up uniformly;
//! * the text pass (`text`): each glyphon `TextArea` is given `scale = ui_scale`
//!   over a physical-resolution viewport, so glyphs render crisply at the scaled
//!   size while their (logical) positions land in the same place as the rects;
//! * mouse input (`window`): incoming physical cursor coords are divided by the
//!   scale before they enter the (logical) hit-testing math, so clicks still hit.
//!
//! `ui_scale = os_scale_factor × user_zoom`. The OS factor comes from winit
//! (`window.scale_factor()` / `ScaleFactorChanged`); the user zoom is driven by
//! Ctrl+=/Ctrl+-/Ctrl+0 and Ctrl+mouse-wheel and is clamped + persisted.

use std::sync::atomic::{AtomicU32, Ordering};

/// Minimum / maximum user zoom (independent of the OS DPI factor).
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 3.0;
/// One zoom step for Ctrl+= / Ctrl+-.
pub const ZOOM_STEP: f32 = 0.1;

// Stored as bit-patterns so they can live in a `static` with no lock.
static OS_SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0
static USER_ZOOM: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}
fn store(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

/// The OS DPI scale factor (winit `scale_factor`), e.g. 1.0 / 1.5 / 2.0.
pub fn os_scale() -> f32 {
    load(&OS_SCALE)
}

/// Set the OS DPI scale factor (clamped to a sane positive range).
pub fn set_os_scale(s: f32) {
    store(&OS_SCALE, clamp_os(s));
}

/// The user zoom multiplier (Ctrl+=/-/0, Ctrl+wheel), clamped.
pub fn user_zoom() -> f32 {
    load(&USER_ZOOM)
}

/// Set the user zoom (clamped to `ZOOM_MIN..=ZOOM_MAX`).
pub fn set_user_zoom(z: f32) {
    store(&USER_ZOOM, clamp_zoom(z));
}

/// The combined UI scale used everywhere: `os_scale × user_zoom`.
#[inline]
pub fn ui_scale() -> f32 {
    (os_scale() * user_zoom()).max(0.25)
}

fn clamp_os(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(0.25, 8.0)
    } else {
        1.0
    }
}

/// Clamp + snap a zoom value to `ZOOM_MIN..=ZOOM_MAX`, rounded to one step.
pub fn clamp_zoom(z: f32) -> f32 {
    if !z.is_finite() {
        return 1.0;
    }
    let snapped = (z / ZOOM_STEP).round() * ZOOM_STEP;
    snapped.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Zoom commands mapped from the Ctrl chords. Returns the new clamped zoom.
/// (The live ABI in `abi.rs` drives zoom directly via `clamp_zoom`/`set_user_zoom`
/// so it can persist + rescale in one step; these remain the unit-tested core.)
#[allow(dead_code)]
pub fn zoom_in() -> f32 {
    let z = clamp_zoom(user_zoom() + ZOOM_STEP);
    set_user_zoom(z);
    z
}
#[allow(dead_code)]
pub fn zoom_out() -> f32 {
    let z = clamp_zoom(user_zoom() - ZOOM_STEP);
    set_user_zoom(z);
    z
}
#[allow(dead_code)]
pub fn zoom_reset() -> f32 {
    set_user_zoom(1.0);
    1.0
}

/// Convert a physical pixel coordinate (from winit) to the logical pixel space
/// the layout/hit-testing math operates in.
#[inline]
pub fn phys_to_logical(v: f32) -> f32 {
    v / ui_scale()
}

/// Convert a logical pixel length to physical (e.g. for the GPU scissor rect).
#[inline]
#[allow(dead_code)]
pub fn logical_to_phys(v: f32) -> f32 {
    v * ui_scale()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_clamps_to_range() {
        assert_eq!(clamp_zoom(10.0), ZOOM_MAX);
        assert_eq!(clamp_zoom(0.0), ZOOM_MIN);
        assert_eq!(clamp_zoom(f32::NAN), 1.0);
        // snaps to a step
        assert!((clamp_zoom(1.04) - 1.0).abs() < 0.001);
        assert!((clamp_zoom(1.06) - 1.1).abs() < 0.001);
    }

    #[test]
    fn zoom_in_out_reset_round_trip() {
        set_user_zoom(1.0);
        let a = zoom_in();
        assert!((a - 1.1).abs() < 0.001);
        let b = zoom_out();
        assert!((b - 1.0).abs() < 0.001);
        // reset always returns to 1.0
        zoom_in();
        zoom_in();
        assert!((zoom_reset() - 1.0).abs() < 0.001);
        set_user_zoom(1.0);
    }

    #[test]
    fn zoom_in_saturates_at_max() {
        set_user_zoom(ZOOM_MAX);
        assert_eq!(zoom_in(), ZOOM_MAX);
        set_user_zoom(ZOOM_MIN);
        assert_eq!(zoom_out(), ZOOM_MIN);
        set_user_zoom(1.0);
    }

    #[test]
    fn ui_scale_is_product() {
        set_os_scale(1.5);
        set_user_zoom(2.0);
        assert!((ui_scale() - 3.0).abs() < 0.001);
        // logical<->physical invert
        assert!((phys_to_logical(300.0) - 100.0).abs() < 0.01);
        assert!((logical_to_phys(100.0) - 300.0).abs() < 0.01);
        set_os_scale(1.0);
        set_user_zoom(1.0);
    }

    #[test]
    fn os_scale_rejects_garbage() {
        set_os_scale(f32::INFINITY);
        assert_eq!(os_scale(), 1.0);
        set_os_scale(1.0);
    }
}
