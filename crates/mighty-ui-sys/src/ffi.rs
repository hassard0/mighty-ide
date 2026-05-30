//! Flat C ABI types shared across the shim.
//!
//! Everything here is `#[repr(C)]` so the Mighty side can mirror the layout
//! with `#[repr]` structs and pass values across the FFI boundary. No Rust
//! types with non-trivial layout (enums with data, `String`, `Vec`) ever cross
//! the boundary; events are flattened into a single tagged struct.

/// An RGBA color with components in the `0.0..=1.0` range.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MuiColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl MuiColor {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

// ---------------------------------------------------------------------------
// Event tags
// ---------------------------------------------------------------------------

/// No event (used as the zero value / empty slot).
pub const MUI_EVENT_NONE: u32 = 0;
/// A printable character was typed. `codepoint` holds the Unicode scalar value.
pub const MUI_EVENT_CHAR: u32 = 1;
/// A named (non-text) key was pressed. `key` holds a `MUI_KEY_*` code.
pub const MUI_EVENT_KEY: u32 = 2;
/// A mouse button went down at (`x`, `y`). `button` holds a `MUI_MOUSE_*` code.
pub const MUI_EVENT_MOUSE_DOWN: u32 = 3;
/// A mouse button went up at (`x`, `y`). `button` holds a `MUI_MOUSE_*` code.
pub const MUI_EVENT_MOUSE_UP: u32 = 4;
/// The mouse wheel scrolled. `scroll_x`/`scroll_y` hold the delta.
pub const MUI_EVENT_SCROLL: u32 = 5;
/// The window was resized. `width`/`height` hold the new size in pixels.
pub const MUI_EVENT_RESIZE: u32 = 6;
/// The window was asked to close.
pub const MUI_EVENT_CLOSE: u32 = 7;

// ---------------------------------------------------------------------------
// Named key codes (only used when tag == MUI_EVENT_KEY)
// ---------------------------------------------------------------------------

pub const MUI_KEY_UNKNOWN: u32 = 0;
pub const MUI_KEY_LEFT: u32 = 1;
pub const MUI_KEY_RIGHT: u32 = 2;
pub const MUI_KEY_UP: u32 = 3;
pub const MUI_KEY_DOWN: u32 = 4;
pub const MUI_KEY_BACKSPACE: u32 = 5;
pub const MUI_KEY_ENTER: u32 = 6;
pub const MUI_KEY_TAB: u32 = 7;
pub const MUI_KEY_ESCAPE: u32 = 8;
pub const MUI_KEY_DELETE: u32 = 9;
pub const MUI_KEY_HOME: u32 = 10;
pub const MUI_KEY_END: u32 = 11;
pub const MUI_KEY_PAGE_UP: u32 = 12;
pub const MUI_KEY_PAGE_DOWN: u32 = 13;
/// F12 — bound to go-to-definition in the IDE (sub-project 7).
pub const MUI_KEY_F12: u32 = 14;
/// F2 — bound to rename symbol in the IDE.
pub const MUI_KEY_F2: u32 = 15;
/// F5 — start / continue debugging.
pub const MUI_KEY_F5: u32 = 16;
/// F10 — step over (debug).
pub const MUI_KEY_F10: u32 = 17;
/// F11 — step into (debug); Shift+F11 steps out.
pub const MUI_KEY_F11: u32 = 18;

// ---------------------------------------------------------------------------
// Mouse button codes (only used when tag == MUI_EVENT_MOUSE_DOWN/UP)
// ---------------------------------------------------------------------------

pub const MUI_MOUSE_LEFT: u32 = 0;
pub const MUI_MOUSE_RIGHT: u32 = 1;
pub const MUI_MOUSE_MIDDLE: u32 = 2;
pub const MUI_MOUSE_OTHER: u32 = 3;

// ---------------------------------------------------------------------------
// Modifier bitflags (applied to the `mods` field on Char/Key/Mouse events)
// ---------------------------------------------------------------------------

pub const MUI_MOD_SHIFT: u32 = 1 << 0;
pub const MUI_MOD_CTRL: u32 = 1 << 1;
pub const MUI_MOD_ALT: u32 = 1 << 2;
pub const MUI_MOD_SUPER: u32 = 1 << 3;

/// A flattened input event. Which fields are meaningful depends on `tag`:
///
/// | tag           | meaningful fields                  |
/// |---------------|------------------------------------|
/// | `CHAR`        | `codepoint`, `mods`                |
/// | `KEY`         | `key`, `mods`                      |
/// | `MOUSE_DOWN`  | `button`, `x`, `y`, `mods`         |
/// | `MOUSE_UP`    | `button`, `x`, `y`, `mods`         |
/// | `SCROLL`      | `scroll_x`, `scroll_y`, `mods`     |
/// | `RESIZE`      | `width`, `height`                  |
/// | `CLOSE`/`NONE`| (none)                             |
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MuiEvent {
    /// One of the `MUI_EVENT_*` tags.
    pub tag: u32,
    /// Unicode scalar value for `CHAR` events.
    pub codepoint: u32,
    /// `MUI_KEY_*` code for `KEY` events.
    pub key: u32,
    /// `MUI_MOUSE_*` code for mouse events.
    pub button: u32,
    /// Bitwise OR of `MUI_MOD_*` flags active at event time.
    pub mods: u32,
    /// Cursor x in pixels (mouse events).
    pub x: f32,
    /// Cursor y in pixels (mouse events).
    pub y: f32,
    /// Horizontal scroll delta (scroll events).
    pub scroll_x: f32,
    /// Vertical scroll delta (scroll events).
    pub scroll_y: f32,
    /// New width in pixels (resize events).
    pub width: u32,
    /// New height in pixels (resize events).
    pub height: u32,
}

impl MuiEvent {
    /// The empty / no-op event.
    pub const fn none() -> Self {
        Self {
            tag: MUI_EVENT_NONE,
            codepoint: 0,
            key: 0,
            button: 0,
            mods: 0,
            x: 0.0,
            y: 0.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            width: 0,
            height: 0,
        }
    }

    pub fn char(codepoint: u32, mods: u32) -> Self {
        Self {
            tag: MUI_EVENT_CHAR,
            codepoint,
            mods,
            ..Self::none()
        }
    }

    pub fn key(key: u32, mods: u32) -> Self {
        Self {
            tag: MUI_EVENT_KEY,
            key,
            mods,
            ..Self::none()
        }
    }

    pub fn mouse(tag: u32, button: u32, x: f32, y: f32, mods: u32) -> Self {
        Self {
            tag,
            button,
            x,
            y,
            mods,
            ..Self::none()
        }
    }

    pub fn scroll(scroll_x: f32, scroll_y: f32, mods: u32) -> Self {
        Self {
            tag: MUI_EVENT_SCROLL,
            scroll_x,
            scroll_y,
            mods,
            ..Self::none()
        }
    }

    pub fn resize(width: u32, height: u32) -> Self {
        Self {
            tag: MUI_EVENT_RESIZE,
            width,
            height,
            ..Self::none()
        }
    }

    pub fn close() -> Self {
        Self {
            tag: MUI_EVENT_CLOSE,
            ..Self::none()
        }
    }
}
