//! Theme system for the Mighty IDE — three live-switchable color themes.
//!
//! ALL colors and a handful of style parameters live behind a single active
//! [`Theme`] value so the entire renderer is theme-aware: the Mighty side never
//! names a color, every Rust draw site pulls its palette from this module, and
//! switching the active theme (see [`set_active`]) re-skins the IDE LIVE.
//!
//! ## How draw sites read the palette
//!
//! The historical surface was a set of `pub const NAME: MuiColor` items. To make
//! it runtime-switchable without touching ~280 call sites, each color is now a
//! zero-arg **function** of the same name returning the active theme's value
//! (`theme::ACCENT()`, `theme::BG_2()`, …). The layout *metrics*
//! (`FONT_SIZE`/`LINE_HEIGHT`/`CHAR_W`/`CHROME_FONT_SIZE`/`SPACE`) are
//! theme-independent and stay `const`.
//!
//! ## The three themes (exact hex pulled from each mockup's `:root`)
//!
//! * **Vivid Modern** (`design/option-b.html`, default): near-black #0b0b0f,
//!   electric-indigo #7c5cff accent, the 6-color dark syntax palette.
//! * **Aurora Glass** (`design/option-a.html`): dark glass over an aurora
//!   gradient, aurora-cyan #5fe3d0 accent, translucent panels, softer borders.
//! * **Warm Studio** (`design/option-c.html`): LIGHT — warm paper #FAF7F2,
//!   ember/terracotta #C0552E accent, soft dark drop-shadows, warm hairlines,
//!   dark ink text, a rich light syntax palette.
//!
//! On a light theme (`is_light = true`) elevation is read with soft *dark*
//! shadows and darker hairlines rather than the dark-mode white highlights, and
//! text is dark ink on paper — the renderer branches on [`Theme::is_light`] where
//! the visual logic differs between light and dark backgrounds.

#![allow(dead_code)]
#![allow(non_snake_case)]

use std::sync::RwLock;

use crate::ffi::MuiColor;

/// Build a [`MuiColor`] from 0xRRGGBB + alpha (0..=1). `const`-friendly.
pub const fn hex(rgb: u32, a: f32) -> MuiColor {
    let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
    let b = (rgb & 0xFF) as f32 / 255.0;
    MuiColor::new(r, g, b, a)
}

/// One radial atmosphere stop painted behind the whole window: a center
/// (fraction of width/height), a radius (fraction of width), and a color.
#[derive(Clone, Copy, Debug)]
pub struct AtmoStop {
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
    pub color: MuiColor,
}

/// A complete, self-contained color theme + the few style params that vary by
/// theme (light vs dark elevation, shadow strength, border alpha, glass
/// translucency, the atmosphere gradient stops, and the raw accent rgb used to
/// derive accent washes at arbitrary alpha).
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub id: ThemeId,
    /// `true` for a light background (Warm Studio): invert elevation/contrast.
    pub is_light: bool,
    /// `true` when panels are drawn translucent over the atmosphere (glass).
    pub glass: bool,

    // ---- surfaces ----
    pub bg: MuiColor,        // app void / GPU clear
    pub bg_1: MuiColor,      // primary surface
    pub bg_2: MuiColor,      // panels
    pub bg_edit: MuiColor,   // editor field
    pub bg_rail: MuiColor,   // activity rail
    pub bg_4: MuiColor,      // hover surface
    pub elevated: MuiColor,  // overlays / cards
    pub elevated_2: MuiColor,// top of card gradients
    pub current_line: MuiColor,

    // ---- hairlines ----
    pub border: MuiColor,
    pub border_soft: MuiColor,
    pub border_strong: MuiColor,
    /// Faux drop-shadow color (near-black on dark, soft brown on light).
    pub shadow: MuiColor,
    /// 1px top-edge highlight on panels/cards.
    pub highlight: MuiColor,

    // ---- text ----
    pub text: MuiColor,      // high-emphasis
    pub text_1: MuiColor,    // primary
    pub dim: MuiColor,       // secondary/muted
    pub text_3: MuiColor,    // tertiary / gutter
    pub text_4: MuiColor,    // faint
    pub gutter: MuiColor,
    pub gutter_active: MuiColor,

    // ---- accent ----
    /// Raw accent rgb (no alpha baked) so washes can be derived at any alpha.
    pub accent_rgb: u32,
    pub accent: MuiColor,
    pub accent_bright: MuiColor,
    pub accent_dim: MuiColor,
    pub accent_glow: MuiColor,
    pub accent_faint: MuiColor,
    pub accent_line: MuiColor,
    pub selection: MuiColor,
    pub find_highlight: MuiColor,

    // ---- syntax ----
    pub syn_keyword: MuiColor,
    pub syn_type: MuiColor,
    pub syn_string: MuiColor,
    pub syn_comment: MuiColor,
    pub syn_number: MuiColor,
    pub syn_function: MuiColor,
    pub syn_punct: MuiColor,
    pub syn_attr: MuiColor,
    pub syn_default: MuiColor,

    // ---- diagnostics / semantic ----
    pub error: MuiColor,
    pub warning: MuiColor,
    pub green: MuiColor,
    pub info: MuiColor,

    // ---- status bar band gradient ----
    pub status_top: MuiColor,
    pub status_bottom: MuiColor,

    // ---- atmosphere (painted behind the whole window) ----
    /// Up to 4 radial glow stops; unused stops carry alpha 0.
    pub atmosphere: [AtmoStop; 4],
}

/// Stable theme identity (also the persisted-config + ABI index order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeId {
    Vivid = 0,
    Aurora = 1,
    Warm = 2,
}

impl ThemeId {
    pub const ALL: [ThemeId; 3] = [ThemeId::Vivid, ThemeId::Aurora, ThemeId::Warm];

    pub fn from_index(i: i32) -> Option<ThemeId> {
        match i {
            0 => Some(ThemeId::Vivid),
            1 => Some(ThemeId::Aurora),
            2 => Some(ThemeId::Warm),
            _ => None,
        }
    }

    pub fn index(self) -> i32 {
        self as i32
    }

    /// Human-facing theme name (shown in the picker + status).
    pub fn name(self) -> &'static str {
        match self {
            ThemeId::Vivid => "Vivid Modern",
            ThemeId::Aurora => "Aurora Glass",
            ThemeId::Warm => "Warm Studio",
        }
    }

    /// Config-file slug + the `MUI_THEME` env override value.
    pub fn slug(self) -> &'static str {
        match self {
            ThemeId::Vivid => "vivid",
            ThemeId::Aurora => "aurora",
            ThemeId::Warm => "warm",
        }
    }

    /// Parse a slug ("vivid"/"aurora"/"warm", case-insensitive) into an id.
    pub fn from_slug(s: &str) -> Option<ThemeId> {
        match s.trim().to_ascii_lowercase().as_str() {
            "vivid" | "vivid modern" | "0" => Some(ThemeId::Vivid),
            "aurora" | "aurora glass" | "1" => Some(ThemeId::Aurora),
            "warm" | "warm studio" | "2" => Some(ThemeId::Warm),
            _ => None,
        }
    }

    pub fn theme(self) -> Theme {
        match self {
            ThemeId::Vivid => VIVID,
            ThemeId::Aurora => AURORA,
            ThemeId::Warm => WARM,
        }
    }
}

impl Theme {
    /// Derive an accent wash at arbitrary alpha from the theme's accent rgb.
    pub fn accent_a(&self, a: f32) -> MuiColor {
        hex(self.accent_rgb, a)
    }
}

// ===========================================================================
// VIVID MODERN (default) — design/option-b.html
// ===========================================================================
pub const VIVID: Theme = Theme {
    id: ThemeId::Vivid,
    is_light: false,
    glass: false,

    bg: hex(0x08080b, 1.0),
    bg_1: hex(0x0b0b0f, 1.0),
    bg_2: hex(0x0f0f15, 1.0),
    bg_edit: hex(0x0b0b0f, 1.0),
    bg_rail: hex(0x090910, 1.0),
    bg_4: hex(0x1a1a23, 1.0),
    elevated: hex(0x14141c, 1.0),
    elevated_2: hex(0x1a1a23, 1.0),
    current_line: hex(0x13131b, 1.0),

    border: hex(0x1c1c26, 1.0),
    border_soft: hex(0x15151d, 1.0),
    border_strong: hex(0x26262f, 1.0),
    shadow: hex(0x000000, 0.7),
    highlight: MuiColor::new(1.0, 1.0, 1.0, 0.045),

    text: hex(0xf4f4f8, 1.0),
    text_1: hex(0xb9b9c6, 1.0),
    dim: hex(0x797986, 1.0),
    text_3: hex(0x54545f, 1.0),
    text_4: hex(0x3a3a44, 1.0),
    gutter: hex(0x54545f, 1.0),
    gutter_active: hex(0xf4f4f8, 1.0),

    accent_rgb: 0x7c5cff,
    accent: hex(0x7c5cff, 1.0),
    accent_bright: hex(0x9d83ff, 1.0),
    accent_dim: hex(0x5d43c8, 1.0),
    accent_glow: hex(0x7c5cff, 0.45),
    accent_faint: hex(0x7c5cff, 0.12),
    accent_line: hex(0x7c5cff, 0.30),
    selection: hex(0x7c5cff, 0.22),
    find_highlight: hex(0x7c5cff, 0.22),

    syn_keyword: hex(0xb794ff, 1.0),
    syn_type: hex(0x5ad1c4, 1.0),
    syn_string: hex(0x8ce99a, 1.0),
    syn_comment: hex(0x4a4a56, 1.0),
    syn_number: hex(0xff9e64, 1.0),
    syn_function: hex(0xffd27a, 1.0),
    syn_punct: hex(0x8b8b99, 1.0),
    syn_attr: hex(0xff7eb6, 1.0),
    syn_default: hex(0xd7d7e3, 1.0),

    error: hex(0xff5e7a, 1.0),
    warning: hex(0xffb84d, 1.0),
    green: hex(0x38d39f, 1.0),
    info: hex(0x4ec5ff, 1.0),

    status_top: hex(0x0d0d13, 1.0),
    status_bottom: hex(0x090910, 1.0),

    atmosphere: [
        AtmoStop { cx: 0.12, cy: -0.08, radius: 0.78, color: hex(0x1f2f4e, 1.0) },
        AtmoStop { cx: 1.0, cy: 0.0, radius: 0.62, color: hex(0x322138, 1.0) },
        AtmoStop { cx: 0.6, cy: 1.2, radius: 0.85, color: hex(0x122632, 1.0) },
        AtmoStop { cx: 0.0, cy: 0.0, radius: 0.0, color: hex(0x000000, 0.0) },
    ],
};

// ===========================================================================
// AURORA GLASS — design/option-a.html
// ===========================================================================
// glass rgba(20,27,41,.55) → opaque base #141b29; glass-strong #182030;
// glass-faint #1c2638; hairline rgba(150,180,230,.14) ≈ #2c3852 over void.
pub const AURORA: Theme = Theme {
    id: ThemeId::Aurora,
    is_light: false,
    glass: true,

    bg: hex(0x05070d, 1.0),
    bg_1: hex(0x070b14, 1.0),
    bg_2: hex(0x141b29, 0.78),   // translucent glass panel
    bg_edit: hex(0x0a0f1a, 0.62), // editor field — slightly more transparent
    bg_rail: hex(0x0a0f1a, 0.80),
    bg_4: hex(0x1c2638, 0.85),   // hover surface
    elevated: hex(0x182030, 0.92), // overlay cards — mostly opaque so text reads
    elevated_2: hex(0x1f2a40, 0.92),
    current_line: hex(0x16203a, 0.55),

    border: hex(0x2c3852, 1.0),       // ≈ hairline rgba(150,180,230,.14)
    border_soft: hex(0x222d42, 1.0),  // ≈ hairline-soft .08
    border_strong: hex(0x3a4a68, 1.0),
    shadow: hex(0x01030a, 0.7),
    highlight: MuiColor::new(0.70, 0.82, 1.0, 0.10), // inner-hi rgba(180,210,255,.10)

    text: hex(0xe8eefb, 1.0),
    text_1: hex(0xb8c4dc, 1.0),
    dim: hex(0x7e8aa4, 1.0),
    text_3: hex(0x525d76, 1.0),
    text_4: hex(0x3c465c, 1.0),
    gutter: hex(0x525d76, 1.0),
    gutter_active: hex(0xe8eefb, 1.0),

    accent_rgb: 0x5fe3d0,
    accent: hex(0x5fe3d0, 1.0),
    accent_bright: hex(0x8ff0e3, 1.0),
    accent_dim: hex(0x3fa99a, 1.0),
    accent_glow: hex(0x5fe3d0, 0.45),
    accent_faint: hex(0x5fe3d0, 0.16),
    accent_line: hex(0x5fe3d0, 0.30),
    selection: hex(0x57b6ff, 0.22),     // .seltext rgba(87,182,255,.22)
    find_highlight: hex(0x57b6ff, 0.22),

    syn_keyword: hex(0xc79bff, 1.0),
    syn_type: hex(0x5fe3d0, 1.0),
    syn_string: hex(0x9fe6a0, 1.0),
    syn_comment: hex(0x5a6885, 1.0),
    syn_number: hex(0xffc98a, 1.0),
    syn_function: hex(0x7db4ff, 1.0),
    syn_punct: hex(0x8593b0, 1.0),
    syn_attr: hex(0xff9bb8, 1.0),
    syn_default: hex(0xd7e0f4, 1.0),

    error: hex(0xff7a85, 1.0),
    warning: hex(0xffc98a, 1.0),
    green: hex(0x9fe6a0, 1.0),
    info: hex(0x57b6ff, 1.0),

    status_top: hex(0x0c1320, 0.92),
    status_bottom: hex(0x070d18, 0.92),

    atmosphere: [
        AtmoStop { cx: 0.18, cy: -0.08, radius: 0.95, color: hex(0x22788c, 1.0) },
        AtmoStop { cx: 0.92, cy: 0.08, radius: 0.88, color: hex(0x4a3aaa, 1.0) },
        AtmoStop { cx: 0.70, cy: 1.08, radius: 0.78, color: hex(0x7e3c96, 1.0) },
        AtmoStop { cx: 0.08, cy: 1.05, radius: 0.62, color: hex(0x1e5a78, 1.0) },
    ],
};

// ===========================================================================
// WARM STUDIO (LIGHT) — design/option-c.html
// ===========================================================================
pub const WARM: Theme = Theme {
    id: ThemeId::Warm,
    is_light: true,
    glass: false,

    bg: hex(0xFAF7F2, 1.0),      // paper — also the GPU clear
    bg_1: hex(0xFCFAF6, 1.0),    // panel
    bg_2: hex(0xF4EFE7, 1.0),    // paper-2 (panels/sidebar)
    bg_edit: hex(0xFCFAF6, 1.0), // panel — editor field reads as the lightest paper
    bg_rail: hex(0xF1EADE, 1.0), // rail gradient bottom
    bg_4: hex(0xEFE9DF, 1.0),    // paper-3 hover
    elevated: hex(0xFFFDFA, 1.0),// panel-raised — overlay cards
    elevated_2: hex(0xFCFAF6, 1.0),
    current_line: hex(0xF1EADD, 1.0), // a touch warmer than paper for the band

    border: hex(0xE7DFD2, 1.0),       // hair
    border_soft: hex(0xEFE8DC, 1.0),  // hair-soft
    border_strong: hex(0xDDD2C0, 1.0),// hair-strong
    // Soft realistic drop shadow: dark warm brown at low alpha (shadow-3 base
    // rgba(74,60,40,…)). On light bg shadows must be DARK to read as elevation.
    shadow: MuiColor::new(0.29, 0.235, 0.157, 0.20),
    // On light bg the "top highlight" is a subtle darker hairline, not white.
    highlight: MuiColor::new(0.0, 0.0, 0.0, 0.04),

    text: hex(0x2A2622, 1.0),    // ink
    text_1: hex(0x4A443C, 1.0),  // ink-2
    dim: hex(0x6E665B, 1.0),     // ink-3
    text_3: hex(0x968C7D, 1.0),  // ink-4 (gutter)
    text_4: hex(0xB6AB99, 1.0),  // ink-faint
    gutter: hex(0x968C7D, 1.0),
    gutter_active: hex(0x2A2622, 1.0),

    accent_rgb: 0xC0552E,
    accent: hex(0xC0552E, 1.0),   // ember
    accent_bright: hex(0xA2421F, 1.0), // ember-deep (a *darker* "bright" reads on light)
    accent_dim: hex(0xE9C9B7, 1.0),    // ember-soft
    accent_glow: hex(0xC0552E, 0.28),  // softer glow on light bg
    accent_faint: hex(0xC0552E, 0.10), // ember-tint
    accent_line: hex(0xC0552E, 0.30),
    selection: hex(0x41608A, 0.16),    // .selh rgba(65,96,138,.16)
    find_highlight: hex(0xC0552E, 0.18),

    syn_keyword: hex(0xB0492B, 1.0),  // ember
    syn_type: hex(0x41608A, 1.0),     // ink-blue
    syn_string: hex(0x5E7A52, 1.0),   // sage
    syn_comment: hex(0xA79C8B, 1.0),  // warm gray
    syn_number: hex(0xB58634, 1.0),   // gold
    syn_function: hex(0x8A5A6E, 1.0), // plum
    syn_punct: hex(0x7C7163, 1.0),
    syn_attr: hex(0x8A5A6E, 1.0),
    syn_default: hex(0x3B352E, 1.0),

    error: hex(0xC0392B, 1.0),
    warning: hex(0xB58634, 1.0),
    green: hex(0x5E7A52, 1.0),
    info: hex(0x41608A, 1.0),

    status_top: hex(0xF6F1E8, 1.0),
    status_bottom: hex(0xF1EADE, 1.0),

    // Light-paper warm radial washes (body radial-gradient stops).
    atmosphere: [
        AtmoStop { cx: 0.18, cy: -0.10, radius: 0.95, color: hex(0xFFFBF4, 1.0) },
        AtmoStop { cx: 0.95, cy: 1.10, radius: 0.85, color: hex(0xF3ECE0, 1.0) },
        AtmoStop { cx: 0.0, cy: 0.0, radius: 0.0, color: hex(0x000000, 0.0) },
        AtmoStop { cx: 0.0, cy: 0.0, radius: 0.0, color: hex(0x000000, 0.0) },
    ],
};

// ===========================================================================
// Active theme — global, swappable at runtime (live re-skin).
// ===========================================================================

static ACTIVE: RwLock<Theme> = RwLock::new(VIVID);

/// The currently-active theme (by value; `Theme` is `Copy`).
#[inline]
pub fn active() -> Theme {
    *ACTIVE.read().unwrap()
}

/// The active theme's id.
#[inline]
pub fn active_id() -> ThemeId {
    active().id
}

/// Switch the active theme to `id`. Effective on the next frame (live re-skin).
pub fn set_active(id: ThemeId) {
    *ACTIVE.write().unwrap() = id.theme();
}

/// `true` when the active theme is a light (paper) theme.
#[inline]
pub fn is_light() -> bool {
    active().is_light
}

/// `true` when panels should be drawn translucent (glass).
#[inline]
pub fn is_glass() -> bool {
    active().glass
}

/// Derive an accent wash at arbitrary alpha from the active theme's accent.
#[inline]
pub fn accent_a(a: f32) -> MuiColor {
    active().accent_a(a)
}

/// A green wash at alpha `a` (the active theme's green color) — added-line tint
/// in the inline diff view.
#[inline]
pub fn green_wash(a: f32) -> MuiColor {
    let g = active().green;
    MuiColor::new(g.r, g.g, g.b, a)
}

/// A red/error wash at alpha `a` (the active theme's error color) — removed-line
/// tint in the inline diff view.
#[inline]
pub fn error_wash(a: f32) -> MuiColor {
    let e = active().error;
    MuiColor::new(e.r, e.g, e.b, a)
}

// ---------------------------------------------------------------------------
// Color accessors — same names the old `pub const`s used, now reading `active()`.
// Draw sites call them as `theme::ACCENT()`, `theme::BG_2()`, etc.
// ---------------------------------------------------------------------------

macro_rules! color_fn {
    ($name:ident, $field:ident) => {
        #[inline]
        pub fn $name() -> MuiColor {
            active().$field
        }
    };
}

color_fn!(BG, bg);
color_fn!(BG_1, bg_1);
color_fn!(BG_2, bg_2);
color_fn!(BG_EDIT, bg_edit);
color_fn!(PANEL, bg_2);
color_fn!(BG_RAIL, bg_rail);
color_fn!(ELEVATED, elevated);
color_fn!(BG_4, bg_4);
color_fn!(ELEVATED_2, elevated_2);
color_fn!(CURRENT_LINE, current_line);
color_fn!(BORDER, border);
color_fn!(BORDER_SOFT, border_soft);
color_fn!(BORDER_STRONG, border_strong);
color_fn!(SHADOW, shadow);
color_fn!(HIGHLIGHT, highlight);

color_fn!(TEXT, text);
color_fn!(TEXT_1, text_1);
color_fn!(DIM, dim);
color_fn!(TEXT_3, text_3);
color_fn!(TEXT_4, text_4);
color_fn!(GUTTER, gutter);
color_fn!(GUTTER_ACTIVE, gutter_active);

color_fn!(ACCENT, accent);
color_fn!(ACCENT_BRIGHT, accent_bright);
color_fn!(ACCENT_DIM, accent_dim);
color_fn!(ACCENT_GLOW, accent_glow);
color_fn!(ACCENT_FAINT, accent_faint);
color_fn!(ACCENT_LINE, accent_line);

// Back-compat aliases retained for any remaining draw sites.
color_fn!(EMBER, accent);
color_fn!(EMBER_SOFT, accent_faint);
color_fn!(EMBER_TINT, accent_faint);
color_fn!(TEAL, syn_type);
color_fn!(SELECTION, selection);
color_fn!(FIND_HIGHLIGHT, find_highlight);

color_fn!(SYN_KEYWORD, syn_keyword);
color_fn!(SYN_TYPE, syn_type);
color_fn!(SYN_STRING, syn_string);
color_fn!(SYN_COMMENT, syn_comment);
color_fn!(SYN_NUMBER, syn_number);
color_fn!(SYN_FUNCTION, syn_function);
color_fn!(SYN_PUNCT, syn_punct);
color_fn!(SYN_ATTR, syn_attr);
color_fn!(SYN_DEFAULT, syn_default);

color_fn!(ERROR, error);
color_fn!(WARNING, warning);
color_fn!(GREEN, green);
color_fn!(INFO, info);

color_fn!(STATUS_TOP, status_top);
color_fn!(STATUS_BOTTOM, status_bottom);

// ---------------------------------------------------------------------------
// Metrics (px) — theme-independent, stay `const`.
// ---------------------------------------------------------------------------

// NOTE: the editor metrics (font size / line height / cell advance) are now
// LIVE preferences read from `crate::settings` (the Settings panel), so they are
// functions rather than `const`. Chrome font size + the 8px spacing unit stay
// constant (they are theme/layout rhythm, not user prefs).

/// Editor font size (px) — live from the active settings.
#[inline]
pub fn FONT_SIZE() -> f32 {
    crate::settings::font_size()
}
/// Editor line-height / row advance (px) — live from the active settings.
#[inline]
pub fn LINE_HEIGHT() -> f32 {
    crate::settings::line_height()
}
/// Monospace cell advance for the editor font at the active size (px) — live.
#[inline]
pub fn CHAR_W() -> f32 {
    crate::settings::char_w()
}
/// Chrome (tabs / sidebar / status) font size (px).
pub const CHROME_FONT_SIZE: f32 = 12.5;
/// Base 8px spacing unit.
pub const SPACE: f32 = 8.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_active_is_vivid() {
        set_active(ThemeId::Vivid);
        assert_eq!(active_id(), ThemeId::Vivid);
        assert!(!is_light());
        // Vivid accent is electric indigo.
        let a = ACCENT();
        assert!(a.r > 0.4 && a.b > 0.9, "vivid accent should be indigo: {a:?}");
    }

    #[test]
    fn set_active_switches_live() {
        set_active(ThemeId::Aurora);
        assert_eq!(active_id(), ThemeId::Aurora);
        assert!(is_glass());
        let a = ACCENT();
        // Aurora accent is cyan (high g+b, lower r).
        assert!(a.g > 0.7 && a.b > 0.7 && a.r < 0.5, "aurora accent cyan: {a:?}");
        set_active(ThemeId::Vivid); // restore for other tests
    }

    #[test]
    fn warm_is_light_with_dark_text_and_dark_shadow() {
        set_active(ThemeId::Warm);
        assert!(is_light());
        // Light background: bg is bright.
        let bg = BG();
        assert!(bg.r > 0.9 && bg.g > 0.9, "warm bg should be light paper: {bg:?}");
        // Text is dark ink.
        let t = TEXT();
        assert!(t.r < 0.3 && t.g < 0.3, "warm text should be dark ink: {t:?}");
        // Shadow is a DARK soft color (so elevation reads on light bg), unlike
        // dark themes where HIGHLIGHT is a white top edge.
        let sh = SHADOW();
        assert!(sh.r < 0.5 && sh.a > 0.0, "warm shadow should be dark + soft: {sh:?}");
        // The dark-mode top highlight is white; the light theme's is dark.
        let hi = HIGHLIGHT();
        assert!(hi.r < 0.2, "warm highlight should be dark hairline, not white: {hi:?}");
        set_active(ThemeId::Vivid); // restore
    }

    #[test]
    fn light_and_dark_derive_differently() {
        set_active(ThemeId::Vivid);
        let dark_hi = HIGHLIGHT();
        set_active(ThemeId::Warm);
        let light_hi = HIGHLIGHT();
        // Dark theme's top highlight is white (bright); light theme's is dark.
        assert!(dark_hi.r > 0.9, "dark highlight white");
        assert!(light_hi.r < 0.2, "light highlight dark");
        set_active(ThemeId::Vivid);
    }

    #[test]
    fn ids_round_trip_through_index_and_slug() {
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::from_index(id.index()), Some(id));
            assert_eq!(ThemeId::from_slug(id.slug()), Some(id));
        }
        assert_eq!(ThemeId::from_index(99), None);
        assert_eq!(ThemeId::from_slug("nope"), None);
    }

    #[test]
    fn all_three_names_distinct() {
        let names: Vec<&str> = ThemeId::ALL.iter().map(|i| i.name()).collect();
        assert_eq!(names, ["Vivid Modern", "Aurora Glass", "Warm Studio"]);
    }

    #[test]
    fn accent_a_uses_active_accent_rgb() {
        set_active(ThemeId::Warm);
        let wash = accent_a(0.5);
        let ember = WARM.accent;
        assert!((wash.r - ember.r).abs() < 0.01, "accent_a should use ember rgb");
        assert!((wash.a - 0.5).abs() < 0.001);
        set_active(ThemeId::Vivid);
    }
}
