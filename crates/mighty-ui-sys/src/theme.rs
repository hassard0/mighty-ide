//! "Aurora Noir" — the centralized dark design system for the Mighty IDE.
//!
//! ALL colors and visual metrics live here so the restyle is shim-only: the
//! Mighty side never names a color (it draws the editor body / chrome through
//! shim entry points that read these constants), and every Rust draw site pulls
//! its palette from this one module. To re-theme the IDE, edit this file.
//!
//! Palette — Aurora Noir (hex), matching `design/mockup.html` `:root`:
//!   bg            #0c0e13   bg-2 (rail/side)  #0a0c10
//!   bg-edit       #0e1016   panel             #13161e   panel-2  #171b25
//!   line          #1d222d   line-soft         #161a22   current-line #13161d
//!   text          #ECE6DA   text-2 #9aa3b2    text-3 #5c6675   text-4 #3b424f
//!   ember         #F4A259   aurora #56C7C0    violet #B99CF5
//!   rose          #E5897B   sage  #A9C77E     sky    #7FB0E8
//!   red           #F2545B   green #5BD6A0

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

/// Window base background (`#0c0e13`). Also the GPU clear color.
pub const BG: MuiColor = hex(0x0c0e13, 1.0);
/// Deeper background — activity rail / sidebar (`#0a0c10`).
pub const BG_2: MuiColor = hex(0x0a0c10, 1.0);
/// Editor field background (`#0e1016`).
pub const BG_EDIT: MuiColor = hex(0x0e1016, 1.0);
/// Sidebar / panel background — kept as an alias for `BG_2` draw sites.
pub const PANEL: MuiColor = hex(0x0a0c10, 1.0);
/// Elevated surfaces — tabs / status / overlays (`#13161e`).
pub const ELEVATED: MuiColor = hex(0x13161e, 1.0);
/// Elevated-2 — top of overlay-card gradients (`#171b25`).
pub const ELEVATED_2: MuiColor = hex(0x171b25, 1.0);
/// The current-line highlight band (`#13161d`).
pub const CURRENT_LINE: MuiColor = hex(0x13161d, 1.0);
/// Borders / dividers — hairline (`#1d222d`).
pub const BORDER: MuiColor = hex(0x1d222d, 1.0);
/// Softer hairline divider (`#161a22`).
pub const BORDER_SOFT: MuiColor = hex(0x161a22, 1.0);
/// A faux drop-shadow color (darker than any surface), used behind overlays.
pub const SHADOW: MuiColor = hex(0x050608, 0.62);
/// A 1px top-edge highlight on panels/cards (white at low alpha).
pub const HIGHLIGHT: MuiColor = MuiColor::new(1.0, 1.0, 1.0, 0.055);

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Default foreground text — warm off-white (`#ECE6DA`).
pub const TEXT: MuiColor = hex(0xECE6DA, 1.0);
/// Secondary text (`#9aa3b2`).
pub const DIM: MuiColor = hex(0x9aa3b2, 1.0);
/// Tertiary text / gutter numbers (`#5c6675`).
pub const TEXT_3: MuiColor = hex(0x5c6675, 1.0);
/// Faint text — dividers / glyph spacers (`#3b424f`).
pub const TEXT_4: MuiColor = hex(0x3b424f, 1.0);
/// Gutter line numbers — tertiary (`#5c6675`), VISIBLE (was the broken void).
pub const GUTTER: MuiColor = hex(0x5c6675, 1.0);
/// The active (cursor) line's gutter number — ember.
pub const GUTTER_ACTIVE: MuiColor = hex(0xF4A259, 1.0);

// ---------------------------------------------------------------------------
// Accents
// ---------------------------------------------------------------------------

/// Primary accent — ember (`#F4A259`). Caret, active-tab underline, selection.
pub const EMBER: MuiColor = hex(0xF4A259, 1.0);
/// A soft ember wash used for selected-row tints / left glows (`#f4a259` @ ~13%).
pub const EMBER_SOFT: MuiColor = hex(0xF4A259, 0.13);
/// Secondary accent — aurora teal (`#56C7C0`).
pub const TEAL: MuiColor = hex(0x56C7C0, 1.0);
/// Selection fill — aurora teal at low alpha (matches the mockup `.sel`).
pub const SELECTION: MuiColor = hex(0x56C7C0, 0.20);
/// Palette selected-row tint — ember at low alpha.
pub const EMBER_TINT: MuiColor = hex(0xF4A259, 0.13);
/// Find-match highlight — ember at low alpha.
pub const FIND_HIGHLIGHT: MuiColor = hex(0xF4A259, 0.22);

// ---------------------------------------------------------------------------
// Syntax — from the mockup `.k/.ty/.s/.n/.f/.c/.p` rules
// ---------------------------------------------------------------------------

/// `keyword` — violet (`#B99CF5`).
pub const SYN_KEYWORD: MuiColor = hex(0xB99CF5, 1.0);
/// `type` — aurora teal (`#56C7C0`).
pub const SYN_TYPE: MuiColor = hex(0x56C7C0, 1.0);
/// `string` — sage (`#A9C77E`).
pub const SYN_STRING: MuiColor = hex(0xA9C77E, 1.0);
/// `comment` — dim tertiary (`#5c6675`).
pub const SYN_COMMENT: MuiColor = hex(0x5c6675, 1.0);
/// `number` — ember (`#F4A259`).
pub const SYN_NUMBER: MuiColor = hex(0xF4A259, 1.0);
/// `function`/call — sky (`#7FB0E8`).
pub const SYN_FUNCTION: MuiColor = hex(0x7FB0E8, 1.0);
/// `punctuation`/operator — secondary (`#9aa3b2`).
pub const SYN_PUNCT: MuiColor = hex(0x9aa3b2, 1.0);
/// `attribute`/rose — used for decorators (`#E5897B`).
pub const SYN_ATTR: MuiColor = hex(0xE5897B, 1.0);
/// default identifier text.
pub const SYN_DEFAULT: MuiColor = TEXT;

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Error (`#F2545B`).
pub const ERROR: MuiColor = hex(0xF2545B, 1.0);
/// Warning — sage/amber (`#E5B567`).
pub const WARNING: MuiColor = hex(0xE5B567, 1.0);
/// Diagnostics-OK green (`#5BD6A0`).
pub const GREEN: MuiColor = hex(0x5BD6A0, 1.0);

// ---------------------------------------------------------------------------
// Metrics (px). Editor line-height 23 to match the mockup; chrome 12.5px.
// ---------------------------------------------------------------------------

/// Editor font size (px).
pub const FONT_SIZE: f32 = 14.0;
/// Editor line-height / row advance (px) — matches the mockup's 23px.
pub const LINE_HEIGHT: f32 = 23.0;
/// Chrome (tabs / sidebar / status) font size (px).
pub const CHROME_FONT_SIZE: f32 = 12.5;
/// Monospace cell advance for the editor font at [`FONT_SIZE`] (px).
/// JetBrains Mono advance ≈ 0.6 em → 14 × 0.6 = 8.4.
pub const CHAR_W: f32 = 8.4;
/// Base 8px spacing unit.
pub const SPACE: f32 = 8.0;
