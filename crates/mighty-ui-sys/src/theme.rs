//! "Vivid Modern" — the centralized dark design system for the Mighty IDE,
//! matching `design/option-b.html` `:root` (Linear/Raycast-grade).
//!
//! ALL colors and visual metrics live here so the restyle is shim-only: the
//! Mighty side never names a color, and every Rust draw site pulls its palette
//! from this one module. To re-theme the IDE, edit this file.
//!
//! Palette — Vivid Modern (hex), from `option-b.html`:
//!   bg-0 #08080b  bg-1 #0b0b0f  bg-2 #0f0f15  bg-3 #14141c  bg-4 #1a1a23
//!   rail #090910   line #1c1c26  line-soft #15151d  line-strong #26262f
//!   tx-0 #f4f4f8  tx-1 #b9b9c6  tx-2 #797986  tx-3 #54545f  tx-4 #3a3a44
//!   acc #7c5cff   acc-bright #9d83ff  acc-dim #5d43c8
//!   ok #38d39f  warn #ffb84d  err #ff5e7a  info #4ec5ff
//!   sx-kw #b794ff  sx-type #5ad1c4  sx-fn #ffd27a  sx-str #8ce99a
//!   sx-num #ff9e64  sx-com #4a4a56  sx-op #8b8b99  sx-var #d7d7e3  sx-mut #ff7eb6

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

/// App void (`#08080b`). Also the GPU clear color.
pub const BG: MuiColor = hex(0x08080b, 1.0);
/// Primary surface (`#0b0b0f`).
pub const BG_1: MuiColor = hex(0x0b0b0f, 1.0);
/// Panels (`#0f0f15`).
pub const BG_2: MuiColor = hex(0x0f0f15, 1.0);
/// Editor field background (`#0b0b0f`).
pub const BG_EDIT: MuiColor = hex(0x0b0b0f, 1.0);
/// Sidebar / panel background — alias for `BG_2` draw sites.
pub const PANEL: MuiColor = hex(0x0f0f15, 1.0);
/// Activity-rail background (`#090910`).
pub const BG_RAIL: MuiColor = hex(0x090910, 1.0);
/// Raised surfaces — overlays / cards (`#14141c`).
pub const ELEVATED: MuiColor = hex(0x14141c, 1.0);
/// Hover surface (`#1a1a23`).
pub const BG_4: MuiColor = hex(0x1a1a23, 1.0);
/// Elevated-2 — top of card gradients (`#1a1a23`).
pub const ELEVATED_2: MuiColor = hex(0x1a1a23, 1.0);
/// The current-line highlight band (`#13131b`).
pub const CURRENT_LINE: MuiColor = hex(0x13131b, 1.0);
/// Borders / dividers — hairline (`#1c1c26`).
pub const BORDER: MuiColor = hex(0x1c1c26, 1.0);
/// Softer hairline divider (`#15151d`).
pub const BORDER_SOFT: MuiColor = hex(0x15151d, 1.0);
/// Stronger hairline (`#26262f`) — card borders.
pub const BORDER_STRONG: MuiColor = hex(0x26262f, 1.0);
/// A faux drop-shadow color (near-black), used behind overlays.
pub const SHADOW: MuiColor = hex(0x000000, 0.7);
/// A 1px top-edge highlight on panels/cards (white at low alpha).
pub const HIGHLIGHT: MuiColor = MuiColor::new(1.0, 1.0, 1.0, 0.045);

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// High-emphasis text (`#f4f4f8`).
pub const TEXT: MuiColor = hex(0xf4f4f8, 1.0);
/// Primary text (`#b9b9c6`).
pub const TEXT_1: MuiColor = hex(0xb9b9c6, 1.0);
/// Secondary / muted text (`#797986`).
pub const DIM: MuiColor = hex(0x797986, 1.0);
/// Tertiary text / gutter numbers (`#54545f`).
pub const TEXT_3: MuiColor = hex(0x54545f, 1.0);
/// Faint text — dividers / glyph spacers (`#3a3a44`).
pub const TEXT_4: MuiColor = hex(0x3a3a44, 1.0);
/// Gutter line numbers — tertiary (`#54545f`).
pub const GUTTER: MuiColor = hex(0x54545f, 1.0);
/// The active (cursor) line's gutter number — high text.
pub const GUTTER_ACTIVE: MuiColor = hex(0xf4f4f8, 1.0);

// ---------------------------------------------------------------------------
// Accent — electric indigo / violet
// ---------------------------------------------------------------------------

/// Primary accent — electric indigo (`#7c5cff`). Caret, active bars, selection.
pub const ACCENT: MuiColor = hex(0x7c5cff, 1.0);
/// Bright accent (`#9d83ff`) — selected text / glow highlights.
pub const ACCENT_BRIGHT: MuiColor = hex(0x9d83ff, 1.0);
/// Dim accent (`#5d43c8`).
pub const ACCENT_DIM: MuiColor = hex(0x5d43c8, 1.0);
/// Accent glow (indigo @ 45%).
pub const ACCENT_GLOW: MuiColor = hex(0x7c5cff, 0.45);
/// Faint accent wash (indigo @ 12%) — selected-row tints.
pub const ACCENT_FAINT: MuiColor = hex(0x7c5cff, 0.12);
/// Accent hairline (indigo @ 30%).
pub const ACCENT_LINE: MuiColor = hex(0x7c5cff, 0.30);

/// Back-compat alias retained for any remaining draw sites: "EMBER" now maps to
/// the indigo accent so the whole UI shifts palette in one place.
pub const EMBER: MuiColor = ACCENT;
pub const EMBER_SOFT: MuiColor = ACCENT_FAINT;
pub const EMBER_TINT: MuiColor = ACCENT_FAINT;
/// Secondary accent — teal (kept for type syntax).
pub const TEAL: MuiColor = hex(0x5ad1c4, 1.0);
/// Selection fill — indigo at low alpha.
pub const SELECTION: MuiColor = hex(0x7c5cff, 0.22);
/// Find-match highlight — indigo at low alpha.
pub const FIND_HIGHLIGHT: MuiColor = hex(0x7c5cff, 0.22);

// ---------------------------------------------------------------------------
// Syntax — from the mockup `--sx-*`
// ---------------------------------------------------------------------------

/// `keyword` — violet (`#b794ff`).
pub const SYN_KEYWORD: MuiColor = hex(0xb794ff, 1.0);
/// `type` — teal (`#5ad1c4`).
pub const SYN_TYPE: MuiColor = hex(0x5ad1c4, 1.0);
/// `string` — green (`#8ce99a`).
pub const SYN_STRING: MuiColor = hex(0x8ce99a, 1.0);
/// `comment` — dim (`#4a4a56`).
pub const SYN_COMMENT: MuiColor = hex(0x4a4a56, 1.0);
/// `number` — orange (`#ff9e64`).
pub const SYN_NUMBER: MuiColor = hex(0xff9e64, 1.0);
/// `function`/call — gold (`#ffd27a`).
pub const SYN_FUNCTION: MuiColor = hex(0xffd27a, 1.0);
/// `punctuation`/operator — op grey (`#8b8b99`).
pub const SYN_PUNCT: MuiColor = hex(0x8b8b99, 1.0);
/// `mut`/special — pink (`#ff7eb6`).
pub const SYN_ATTR: MuiColor = hex(0xff7eb6, 1.0);
/// identifier text (`#d7d7e3`).
pub const SYN_DEFAULT: MuiColor = hex(0xd7d7e3, 1.0);

// ---------------------------------------------------------------------------
// Diagnostics / semantic
// ---------------------------------------------------------------------------

/// Error (`#ff5e7a`).
pub const ERROR: MuiColor = hex(0xff5e7a, 1.0);
/// Warning (`#ffb84d`).
pub const WARNING: MuiColor = hex(0xffb84d, 1.0);
/// OK green (`#38d39f`).
pub const GREEN: MuiColor = hex(0x38d39f, 1.0);
/// Info (`#4ec5ff`).
pub const INFO: MuiColor = hex(0x4ec5ff, 1.0);

// ---------------------------------------------------------------------------
// Metrics (px). Editor line-height 22 to match the mockup; chrome 12.5px.
// ---------------------------------------------------------------------------

/// Editor font size (px).
pub const FONT_SIZE: f32 = 13.5;
/// Editor line-height / row advance (px) — matches the mockup's 22px.
pub const LINE_HEIGHT: f32 = 22.0;
/// Chrome (tabs / sidebar / status) font size (px).
pub const CHROME_FONT_SIZE: f32 = 12.5;
/// Monospace cell advance for the editor font at [`FONT_SIZE`] (px).
/// JetBrains Mono advance ≈ 0.6 em → 13.5 × 0.6 = 8.1.
pub const CHAR_W: f32 = 8.1;
/// Base 8px spacing unit.
pub const SPACE: f32 = 8.0;
