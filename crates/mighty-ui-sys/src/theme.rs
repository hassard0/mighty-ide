//! "Ember Graphite" — the centralized dark design system for the Mighty IDE.
//!
//! ALL colors and visual metrics live here so the restyle is shim-only: the
//! Mighty side never names a color (it draws the editor body / chrome through
//! shim entry points that read these constants), and every Rust draw site pulls
//! its palette from this one module. To re-theme the IDE, edit this file.
//!
//! Palette — Ember Graphite (hex):
//!   editor bg        #14161B   sidebar/panels   #0F1115
//!   elevated         #1A1D24   current-line     #1C2029
//!   border/divider   #262A33   text             #E6E1D6
//!   dim text         #6B7280   gutter number    #4A5160
//!   active gutter    #C9C4B8   accent (ember)   #F2A65A
//!   accent-2 (teal)  #5BC8C0   selection        ember @ ~20% alpha

// The theme is a complete design palette: a few entries (e.g. diagnostic
// colors passed to Mighty as float args, or BG mirrored by the GPU clear color)
// have no direct Rust draw site, so suppress dead-code on the module.
#![allow(dead_code)]

use crate::ffi::MuiColor;

/// Build a [`MuiColor`] from 0xRRGGBB + alpha (0..=1). `const`-friendly.
pub const fn hex(rgb: u32, a: f32) -> MuiColor {
    let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
    let b = (rgb & 0xFF) as f32 / 255.0;
    MuiColor::new(r, g, b, a)
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Editor body background (`#14161B`). Also the GPU clear color.
pub const BG: MuiColor = hex(0x14161B, 1.0);
/// Sidebar / panel background (`#0F1115`).
pub const PANEL: MuiColor = hex(0x0F1115, 1.0);
/// Elevated surfaces — tabs / status / overlays (`#1A1D24`).
pub const ELEVATED: MuiColor = hex(0x1A1D24, 1.0);
/// The current-line highlight band (`#1C2029`).
pub const CURRENT_LINE: MuiColor = hex(0x1C2029, 1.0);
/// Borders / dividers (`#262A33`).
pub const BORDER: MuiColor = hex(0x262A33, 1.0);
/// A faux drop-shadow color (darker than any surface), used behind overlays.
pub const SHADOW: MuiColor = hex(0x05060A, 0.55);

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Default foreground text (`#E6E1D6`).
pub const TEXT: MuiColor = hex(0xE6E1D6, 1.0);
/// Dim / secondary text (`#6B7280`).
pub const DIM: MuiColor = hex(0x6B7280, 1.0);
/// Gutter line numbers (`#4A5160`).
pub const GUTTER: MuiColor = hex(0x4A5160, 1.0);
/// The active (cursor) line's gutter number (`#C9C4B8`).
pub const GUTTER_ACTIVE: MuiColor = hex(0xC9C4B8, 1.0);

// ---------------------------------------------------------------------------
// Accents
// ---------------------------------------------------------------------------

/// Primary accent — ember (`#F2A65A`). Caret, active-tab underline, selection.
pub const EMBER: MuiColor = hex(0xF2A65A, 1.0);
/// Secondary accent — teal (`#5BC8C0`).
pub const TEAL: MuiColor = hex(0x5BC8C0, 1.0);
/// Selection fill — ember at ~20% alpha.
pub const SELECTION: MuiColor = hex(0xF2A65A, 0.20);
/// Palette selected-row tint — ember at low alpha.
pub const EMBER_TINT: MuiColor = hex(0xF2A65A, 0.16);
/// Find-match highlight — ember at low alpha.
pub const FIND_HIGHLIGHT: MuiColor = hex(0xF2A65A, 0.22);

// ---------------------------------------------------------------------------
// Syntax
// ---------------------------------------------------------------------------

/// `keyword` (`#C792EA`).
pub const SYN_KEYWORD: MuiColor = hex(0xC792EA, 1.0);
/// `type` (`#5BC8C0`).
pub const SYN_TYPE: MuiColor = hex(0x5BC8C0, 1.0);
/// `string` (`#B9D77E`).
pub const SYN_STRING: MuiColor = hex(0xB9D77E, 1.0);
/// `comment` (`#5A6172`).
pub const SYN_COMMENT: MuiColor = hex(0x5A6172, 1.0);
/// `number` (`#F2A65A`).
pub const SYN_NUMBER: MuiColor = hex(0xF2A65A, 1.0);
/// `function`/call (`#82AAFF`).
pub const SYN_FUNCTION: MuiColor = hex(0x82AAFF, 1.0);
/// `punctuation`/operator (`#9AA0AB`).
pub const SYN_PUNCT: MuiColor = hex(0x9AA0AB, 1.0);
/// default identifier text.
pub const SYN_DEFAULT: MuiColor = TEXT;

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Error (`#F2545B`).
pub const ERROR: MuiColor = hex(0xF2545B, 1.0);
/// Warning (`#E5B567`).
pub const WARNING: MuiColor = hex(0xE5B567, 1.0);

// ---------------------------------------------------------------------------
// Metrics (px). The 8px spacing rhythm: PAD = 8, rows = 22 (≈1.5 line-height).
// ---------------------------------------------------------------------------

/// Editor font size (px).
pub const FONT_SIZE: f32 = 15.0;
/// Editor line-height / row advance (px) ≈ 1.5 × font size.
pub const LINE_HEIGHT: f32 = 22.0;
/// Chrome (tabs / sidebar / status) font size (px).
pub const CHROME_FONT_SIZE: f32 = 12.5;
/// Monospace cell advance for the editor font at [`FONT_SIZE`] (px).
/// JetBrains Mono advance ≈ 0.6 em → 15 × 0.6 = 9.0.
pub const CHAR_W: f32 = 9.0;
/// Base 8px spacing unit.
pub const SPACE: f32 = 8.0;
