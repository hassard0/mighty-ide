//! Live, persisted editor preferences (the Settings panel model).
//!
//! Mirrors the [`crate::theme`] active-value pattern: a single global
//! [`Settings`] value behind an `RwLock`, read by the layout metrics
//! ([`crate::layout`]), the text renderer ([`crate::text`]) and the editor draw
//! so changing a preference re-skins the IDE LIVE (next frame). The five
//! preferences are:
//!
//! * **font size** (editor px) — drives the editor glyph size, line height and
//!   the monospace cell advance (so the gutter/cursor/click math stays aligned);
//! * **tab width** (spaces) — the indent unit (auto-indent + the Tab key) and
//!   the display width of a literal tab;
//! * **word wrap** (on/off) — a stored pref the editor reads (soft-wrap is left
//!   to the editor; today it gates the horizontal-overflow behavior);
//! * **minimap** (on/off) — hides the editor minimap strip when off;
//! * **theme** — reuses the existing [`crate::theme`] picker (stored in the same
//!   config file).
//!
//! Preferences persist to the SAME `key=value` config file the theme uses
//! (`crate::config`), so a restart restores them. Both load + save are
//! best-effort; a missing/corrupt config never fails the IDE.

#![allow(dead_code)]

use std::sync::RwLock;

/// Clamp bounds for the editable numeric preferences (kept readable on screen).
pub const FONT_MIN: f32 = 9.0;
pub const FONT_MAX: f32 = 28.0;
pub const TAB_MIN: i32 = 1;
pub const TAB_MAX: i32 = 8;

/// The default editor font size (px) — the historical `theme::FONT_SIZE`.
pub const DEFAULT_FONT_SIZE: f32 = 13.5;
/// The reference monospace cell advance at [`DEFAULT_FONT_SIZE`] — the historical
/// `theme::CHAR_W`. Scaled linearly with the active font size.
pub const REF_CHAR_W: f32 = 8.1;
/// The reference line height at [`DEFAULT_FONT_SIZE`] — historical
/// `theme::LINE_HEIGHT` (≈1.63× the font size).
pub const REF_LINE_HEIGHT: f32 = 22.0;

/// The complete set of live editor preferences (all `Copy`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Editor font size in px (clamped to `FONT_MIN..=FONT_MAX`).
    pub font_size: f32,
    /// Indent / tab width in spaces (clamped to `TAB_MIN..=TAB_MAX`).
    pub tab_width: i32,
    /// Soft word-wrap on/off (stored pref read by the editor).
    pub word_wrap: bool,
    /// Show the editor minimap strip.
    pub minimap: bool,
    /// Inline AI ghost-text completions (Copilot-style). Default ON, but
    /// effectively off without an `ANTHROPIC_API_KEY` (the engine never fires).
    pub inline_ai: bool,
    /// Sticky scroll: pin the enclosing-scope headers at the editor top while
    /// scrolled inside a nested scope. Default ON.
    pub sticky_scroll: bool,
    /// Bracket-pair colorization: color matched `()[]{}` by nesting depth using
    /// a theme-derived rainbow palette. Default ON.
    pub bracket_colors: bool,
    /// Indent guides: faint vertical lines at each indentation level, the active
    /// (cursor block) level brightened. Default ON.
    pub indent_guides: bool,
    /// On save: strip trailing whitespace from every line. Default ON.
    pub trim_ws: bool,
    /// On save: ensure the file ends with exactly one trailing newline. Default ON.
    pub final_newline: bool,
    /// Auto-save the active file after a short edit-idle window (debounced).
    /// Default OFF. Only ever saves real file-backed, dirty tabs.
    pub autosave: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            font_size: DEFAULT_FONT_SIZE,
            tab_width: 2,
            word_wrap: false,
            minimap: true,
            inline_ai: true,
            sticky_scroll: true,
            bracket_colors: true,
            indent_guides: true,
            trim_ws: true,
            final_newline: true,
            autosave: false,
        }
    }
}

impl Settings {
    /// Clamp every numeric field into its valid range.
    pub fn clamped(mut self) -> Self {
        self.font_size = self.font_size.clamp(FONT_MIN, FONT_MAX);
        self.tab_width = self.tab_width.clamp(TAB_MIN, TAB_MAX);
        self
    }

    /// Line height (px) for the active font size, preserving the reference ratio.
    pub fn line_height(&self) -> f32 {
        self.font_size * (REF_LINE_HEIGHT / DEFAULT_FONT_SIZE)
    }

    /// Monospace cell advance (px) for the active font size (linear with size).
    pub fn char_w(&self) -> f32 {
        self.font_size * (REF_CHAR_W / DEFAULT_FONT_SIZE)
    }
}

static ACTIVE: RwLock<Settings> = RwLock::new(Settings {
    font_size: DEFAULT_FONT_SIZE,
    tab_width: 2,
    word_wrap: false,
    minimap: true,
    inline_ai: true,
    sticky_scroll: true,
    bracket_colors: true,
    indent_guides: true,
    trim_ws: true,
    final_newline: true,
    autosave: false,
});

/// The currently-active settings (by value; `Settings` is `Copy`).
#[inline]
pub fn active() -> Settings {
    *ACTIVE.read().unwrap()
}

/// Replace the active settings (clamped). Effective next frame (live re-skin).
pub fn set_active(s: Settings) {
    *ACTIVE.write().unwrap() = s.clamped();
}

/// Mutate the active settings in place via `f`, re-clamping after.
pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut g = ACTIVE.write().unwrap();
    f(&mut g);
    *g = g.clamped();
}

// ---- convenience accessors (read by layout / text / editor each frame) ----

#[inline]
pub fn font_size() -> f32 {
    active().font_size
}
#[inline]
pub fn line_height() -> f32 {
    active().line_height()
}
#[inline]
pub fn char_w() -> f32 {
    active().char_w()
}
#[inline]
pub fn tab_width() -> i32 {
    active().tab_width
}
#[inline]
pub fn word_wrap() -> bool {
    active().word_wrap
}
#[inline]
pub fn minimap() -> bool {
    active().minimap
}
#[inline]
pub fn inline_ai() -> bool {
    active().inline_ai
}
#[inline]
pub fn sticky_scroll() -> bool {
    active().sticky_scroll
}
#[inline]
pub fn bracket_colors() -> bool {
    active().bracket_colors
}
#[inline]
pub fn indent_guides() -> bool {
    active().indent_guides
}
#[inline]
pub fn trim_ws() -> bool {
    active().trim_ws
}
#[inline]
pub fn final_newline() -> bool {
    active().final_newline
}
#[inline]
pub fn autosave() -> bool {
    active().autosave
}

// ---------------------------------------------------------------------------
// Config persistence — extends the shared `key=value` config file used for the
// theme (`crate::config`). We parse/append the `font_size` / `tab_width` /
// `word_wrap` / `minimap` lines alongside the existing `theme=` line.
// ---------------------------------------------------------------------------

/// Parse a `key=value` config blob into a [`Settings`], filling unset keys with
/// the default. Tolerant: unknown keys + malformed values are skipped.
pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "font_size" => {
                if let Ok(n) = v.parse::<f32>() {
                    s.font_size = n;
                }
            }
            "tab_width" => {
                if let Ok(n) = v.parse::<i32>() {
                    s.tab_width = n;
                }
            }
            "word_wrap" => s.word_wrap = parse_bool(v),
            "minimap" => s.minimap = parse_bool(v),
            "inline_ai" => s.inline_ai = parse_bool(v),
            "sticky_scroll" => s.sticky_scroll = parse_bool(v),
            "bracket_colors" => s.bracket_colors = parse_bool(v),
            "indent_guides" => s.indent_guides = parse_bool(v),
            "trim_ws" => s.trim_ws = parse_bool(v),
            "final_newline" => s.final_newline = parse_bool(v),
            "autosave" => s.autosave = parse_bool(v),
            _ => {}
        }
    }
    s.clamped()
}

fn parse_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes")
}

/// Render the settings as config lines (the theme line is rendered separately by
/// [`crate::config`]).
pub fn render(s: &Settings) -> String {
    format!(
        "font_size={}\ntab_width={}\nword_wrap={}\nminimap={}\ninline_ai={}\nsticky_scroll={}\nbracket_colors={}\nindent_guides={}\ntrim_ws={}\nfinal_newline={}\nautosave={}\n",
        s.font_size,
        s.tab_width,
        if s.word_wrap { "true" } else { "false" },
        if s.minimap { "true" } else { "false" },
        if s.inline_ai { "true" } else { "false" },
        if s.sticky_scroll { "true" } else { "false" },
        if s.bracket_colors { "true" } else { "false" },
        if s.indent_guides { "true" } else { "false" },
        if s.trim_ws { "true" } else { "false" },
        if s.final_newline { "true" } else { "false" },
        if s.autosave { "true" } else { "false" },
    )
}

/// Load the persisted settings from the shared config file into the active
/// global. Best-effort: leaves the defaults active if the file is absent.
pub fn load_into_active() {
    if let Some(path) = crate::config::config_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            set_active(parse(&text));
        }
    }
}

/// Persist the active settings (and the active theme) to the shared config file.
/// Best-effort; returns `false` on any I/O error.
pub fn save() -> bool {
    crate::config::save_all()
}

/// A process-wide lock serializing tests that mutate the global settings / theme
/// (both are shared statics; parallel tests would otherwise race). Any test that
/// asserts on `active()` / `theme::active_id()` should hold this for its body.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard global-state tests in this module too.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.tab_width, 2);
        assert!(s.minimap);
        assert!(!s.word_wrap);
        assert!((s.font_size - DEFAULT_FONT_SIZE).abs() < 0.001);
    }

    #[test]
    fn clamp_bounds_numeric_fields() {
        let s = Settings {
            font_size: 99.0,
            tab_width: 99,
            ..Default::default()
        }
        .clamped();
        assert_eq!(s.font_size, FONT_MAX);
        assert_eq!(s.tab_width, TAB_MAX);
        let s2 = Settings {
            font_size: 1.0,
            tab_width: -3,
            ..Default::default()
        }
        .clamped();
        assert_eq!(s2.font_size, FONT_MIN);
        assert_eq!(s2.tab_width, TAB_MIN);
    }

    #[test]
    fn metrics_scale_with_font_size() {
        let small = Settings { font_size: DEFAULT_FONT_SIZE, ..Default::default() };
        assert!((small.char_w() - REF_CHAR_W).abs() < 0.001);
        assert!((small.line_height() - REF_LINE_HEIGHT).abs() < 0.001);
        // Doubling the font size doubles the cell advance + line height.
        let big = Settings { font_size: DEFAULT_FONT_SIZE * 2.0, ..Default::default() };
        assert!((big.char_w() - REF_CHAR_W * 2.0).abs() < 0.01);
        assert!((big.line_height() - REF_LINE_HEIGHT * 2.0).abs() < 0.01);
    }

    #[test]
    fn parse_round_trips_through_render() {
        let s = Settings {
            font_size: 16.0,
            tab_width: 4,
            word_wrap: true,
            minimap: false,
            inline_ai: false,
            sticky_scroll: false,
            bracket_colors: false,
            indent_guides: false,
            trim_ws: false,
            final_newline: false,
            autosave: true,
        };
        let blob = render(&s);
        let parsed = parse(&blob);
        assert_eq!(parsed, s);
    }

    #[test]
    fn parse_tolerates_noise_and_fills_defaults() {
        let s = parse("# comment\ntheme=aurora\nfont_size=15\ngarbage\nminimap=off\ninline_ai=off\n");
        assert!((s.font_size - 15.0).abs() < 0.001);
        // Unset keys keep defaults.
        assert_eq!(s.tab_width, 2);
        assert!(!s.minimap); // "off" -> false
        assert!(!s.word_wrap);
        assert!(!s.inline_ai); // "off" -> false
        // inline_ai defaults ON when unset.
        assert!(parse("font_size=15\n").inline_ai);
    }

    #[test]
    fn sticky_scroll_defaults_on_and_round_trips() {
        // Default ON.
        assert!(Settings::default().sticky_scroll);
        // Persists through render/parse.
        let s = Settings { sticky_scroll: false, ..Default::default() };
        assert!(!parse(&render(&s)).sticky_scroll);
        // Unset key keeps the ON default.
        assert!(parse("font_size=15\n").sticky_scroll);
        // Parses "off"/"on".
        assert!(!parse("sticky_scroll=off\n").sticky_scroll);
        assert!(parse("sticky_scroll=on\n").sticky_scroll);
    }

    #[test]
    fn bracket_colors_and_indent_guides_default_on_and_round_trip() {
        // Both default ON.
        assert!(Settings::default().bracket_colors);
        assert!(Settings::default().indent_guides);
        // Persist through render/parse.
        let s = Settings { bracket_colors: false, indent_guides: false, ..Default::default() };
        let p = parse(&render(&s));
        assert!(!p.bracket_colors);
        assert!(!p.indent_guides);
        // Unset keys keep the ON default.
        assert!(parse("font_size=15\n").bracket_colors);
        assert!(parse("font_size=15\n").indent_guides);
        // Parse "off"/"on".
        assert!(!parse("bracket_colors=off\n").bracket_colors);
        assert!(parse("bracket_colors=on\n").bracket_colors);
        assert!(!parse("indent_guides=off\n").indent_guides);
        assert!(parse("indent_guides=on\n").indent_guides);
    }

    #[test]
    fn sticky_scroll_accessor_reads_active() {
        let _g = guard();
        set_active(Settings { sticky_scroll: false, ..Default::default() });
        assert!(!sticky_scroll());
        set_active(Settings::default());
        assert!(sticky_scroll());
    }

    #[test]
    fn set_active_clamps_and_reads_back() {
        let _g = guard();
        set_active(Settings { font_size: 50.0, tab_width: 0, word_wrap: true, minimap: false, inline_ai: false, ..Default::default() });
        assert_eq!(font_size(), FONT_MAX);
        assert_eq!(tab_width(), TAB_MIN);
        assert!(word_wrap());
        assert!(!minimap());
        assert!(!inline_ai());
        // Restore defaults for other tests.
        set_active(Settings::default());
    }
}
