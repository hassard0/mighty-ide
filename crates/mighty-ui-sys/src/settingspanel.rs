//! Settings panel (Preferences: Settings) — a centered Vivid-Modern card listing
//! the live editor preferences with inline controls.
//!
//! Rows (each a [`Row`]):
//!   * **Font Size** (px) — numeric, adjusted with `-`/`+`;
//!   * **Tab Width** (spaces) — numeric, `-`/`+`;
//!   * **Word Wrap** — on/off toggle;
//!   * **Show Minimap** — on/off toggle;
//!   * **Color Theme** — cycles Vivid → Aurora → Warm (reuses the theme system).
//!
//! Up/Down move the highlighted row; `-`/`+` (or Left/Right) adjust a numeric
//! row; Space/Enter toggle a boolean row or cycle the theme. Every change is
//! applied LIVE to [`crate::settings`] / [`crate::theme`] and persisted to the
//! shared config file immediately, so the editor re-skins next frame and the
//! choice survives a restart. Mirrors [`crate::themepicker`]: all state lives
//! here and Mighty drives it through the scalar `mui_settings_*` ABI.

use crate::ffi::MuiColor;
use crate::settings::{self, Settings};
use crate::theme::{self, ThemeId};

/// Which preference a settings row edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowId {
    FontSize,
    TabWidth,
    WordWrap,
    Minimap,
    BracketColors,
    IndentGuides,
    InlineAi,
    TrimWhitespace,
    FinalNewline,
    AutoSave,
    Theme,
}

impl RowId {
    pub const ALL: [RowId; 11] = [
        RowId::FontSize,
        RowId::TabWidth,
        RowId::WordWrap,
        RowId::Minimap,
        RowId::BracketColors,
        RowId::IndentGuides,
        RowId::InlineAi,
        RowId::TrimWhitespace,
        RowId::FinalNewline,
        RowId::AutoSave,
        RowId::Theme,
    ];

    fn label(self) -> &'static str {
        match self {
            RowId::FontSize => "Font Size",
            RowId::TabWidth => "Tab Width",
            RowId::WordWrap => "Word Wrap",
            RowId::Minimap => "Show Minimap",
            RowId::BracketColors => "Bracket Colors",
            RowId::IndentGuides => "Indent Guides",
            RowId::InlineAi => "Inline AI",
            RowId::TrimWhitespace => "Trim Whitespace",
            RowId::FinalNewline => "Final Newline",
            RowId::AutoSave => "Auto Save",
            RowId::Theme => "Color Theme",
        }
    }

    fn desc(self) -> &'static str {
        match self {
            RowId::FontSize => "Editor text size, in pixels",
            RowId::TabWidth => "Spaces per indent level",
            RowId::WordWrap => "Soft-wrap long lines",
            RowId::Minimap => "Show the code minimap strip",
            RowId::BracketColors => "Rainbow-color matched brackets by depth",
            RowId::IndentGuides => "Show vertical indent guide lines",
            RowId::InlineAi => "AI ghost-text completions (needs API key)",
            RowId::TrimWhitespace => "Strip trailing whitespace on save",
            RowId::FinalNewline => "Ensure a trailing newline on save",
            RowId::AutoSave => "Save the file shortly after you stop typing",
            RowId::Theme => "Switch the editor color theme",
        }
    }

    /// `true` for numeric (±) rows; `false` for toggle / cycle rows.
    fn is_numeric(self) -> bool {
        matches!(self, RowId::FontSize | RowId::TabWidth)
    }
}

/// Shim-owned Settings panel state.
#[derive(Debug, Default)]
pub struct SettingsPanel {
    active: bool,
    /// Highlighted row index into [`RowId::ALL`].
    sel: usize,
}

impl SettingsPanel {
    pub fn new() -> Self {
        SettingsPanel::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn open(&mut self) {
        self.active = true;
        self.sel = 0;
    }

    pub fn close(&mut self) {
        self.active = false;
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        RowId::ALL.len()
    }

    fn selected(&self) -> RowId {
        RowId::ALL[self.sel.min(RowId::ALL.len() - 1)]
    }

    /// Move the highlight by `delta`, wrapping.
    pub fn move_sel(&mut self, delta: i32) {
        let n = RowId::ALL.len() as i32;
        let mut s = self.sel as i32 + delta;
        s %= n;
        if s < 0 {
            s += n;
        }
        self.sel = s as usize;
    }

    /// Adjust the selected NUMERIC row by `delta` (font px or tab spaces). For
    /// the Theme row, `delta` cycles the theme. No-op for boolean rows. Applies
    /// live + persists.
    pub fn adjust(&mut self, delta: i32) {
        match self.selected() {
            RowId::FontSize => {
                settings::update(|s| s.font_size += delta as f32);
                settings::save();
            }
            RowId::TabWidth => {
                settings::update(|s| s.tab_width += delta);
                settings::save();
            }
            RowId::Theme => self.cycle_theme(delta),
            _ => {}
        }
    }

    /// Toggle / activate the selected row: flips a boolean row, cycles the theme
    /// forward, or (for a numeric row) bumps it up by one. Applies live + saves.
    pub fn toggle(&mut self) {
        match self.selected() {
            RowId::WordWrap => {
                settings::update(|s| s.word_wrap = !s.word_wrap);
                settings::save();
            }
            RowId::Minimap => {
                settings::update(|s| s.minimap = !s.minimap);
                settings::save();
            }
            RowId::BracketColors => {
                settings::update(|s| s.bracket_colors = !s.bracket_colors);
                settings::save();
            }
            RowId::IndentGuides => {
                settings::update(|s| s.indent_guides = !s.indent_guides);
                settings::save();
            }
            RowId::InlineAi => {
                settings::update(|s| s.inline_ai = !s.inline_ai);
                settings::save();
            }
            RowId::TrimWhitespace => {
                settings::update(|s| s.trim_ws = !s.trim_ws);
                settings::save();
            }
            RowId::FinalNewline => {
                settings::update(|s| s.final_newline = !s.final_newline);
                settings::save();
            }
            RowId::AutoSave => {
                settings::update(|s| s.autosave = !s.autosave);
                settings::save();
            }
            RowId::Theme => self.cycle_theme(1),
            RowId::FontSize => self.adjust(1),
            RowId::TabWidth => self.adjust(1),
        }
    }

    /// Cycle the active theme by `delta` (wrapping) and persist.
    fn cycle_theme(&self, delta: i32) {
        let cur = theme::active_id();
        let n = ThemeId::ALL.len() as i32;
        let i = ThemeId::ALL.iter().position(|&t| t == cur).unwrap_or(0) as i32;
        let mut j = (i + delta) % n;
        if j < 0 {
            j += n;
        }
        let next = ThemeId::ALL[j as usize];
        theme::set_active(next);
        crate::config::save_all();
    }

    /// The display value string for a row (read live from settings/theme).
    fn value_str(s: &Settings, row: RowId) -> String {
        match row {
            RowId::FontSize => format!("{:.0} px", s.font_size),
            RowId::TabWidth => format!("{}", s.tab_width),
            RowId::WordWrap => on_off(s.word_wrap),
            RowId::Minimap => on_off(s.minimap),
            RowId::BracketColors => on_off(s.bracket_colors),
            RowId::IndentGuides => on_off(s.indent_guides),
            RowId::InlineAi => on_off(s.inline_ai),
            RowId::TrimWhitespace => on_off(s.trim_ws),
            RowId::FinalNewline => on_off(s.final_newline),
            RowId::AutoSave => on_off(s.autosave),
            RowId::Theme => theme::active_id().name().to_string(),
        }
    }

    /// Draw the centered Settings card: a dim scrim, a rounded elevated card
    /// titled "Settings", one row per preference with its label + description on
    /// the left and a value + control (± stepper or on/off pill) on the right
    /// (the highlighted row tinted with the accent), and a footer hint. No-op
    /// when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.active {
            return;
        }
        use crate::icons;
        let w = width as f32;
        let h = height as f32;
        let clip = ctx.clip;
        let cur = settings::active();

        let rows = RowId::ALL.len();
        let head_h = 50.0_f32;
        let row_h = 56.0_f32;
        let foot_h = 34.0_f32;
        let box_w = 500.0_f32.min(w - 80.0);
        let box_h = head_h + rows as f32 * row_h + foot_h + 12.0;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = ((h - box_h) * 0.5).max(40.0);
        let radius = 12.0_f32;

        // Scrim (lighter on a light theme).
        let scrim_a = if theme::is_light() { 0.28 } else { 0.55 };
        ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, scrim_a));

        // Shadow + accent glow + card + border.
        ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, theme::SHADOW(), 40.0);
        ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW(), 36.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        // ---- header ----
        ctx.dl_icon(box_x + 18.0, box_y + (head_h - 18.0) * 0.5, 18.0, 18.0, icons::SETTINGS, theme::ACCENT_BRIGHT(), 1.7, false);
        ctx.text.queue_ui_sized(box_x + 46.0, box_y + (head_h - 16.0) * 0.5 - 1.0, "Settings", theme::TEXT(), 16.0, clip);
        ctx.dl_rect(box_x + 1.0, box_y + head_h - 1.0, box_w - 2.0, 1.0, theme::BORDER());

        // ---- rows ----
        let list_top = box_y + head_h;
        for (i, &row) in RowId::ALL.iter().enumerate() {
            let ry = list_top + i as f32 * row_h;
            let selected = i == self.sel;
            if selected {
                ctx.dl_grad_h(box_x + 8.0, ry + 4.0, box_w - 16.0, row_h - 8.0, 8.0, theme::accent_a(0.18), 0.9);
                ctx.dl_stroke(box_x + 8.0, ry + 4.0, box_w - 16.0, row_h - 8.0, 8.0, theme::ACCENT_LINE(), 1.0);
            }

            // Label + description (left).
            let txt_x = box_x + 22.0;
            ctx.text.queue_ui_sized(txt_x, ry + 12.0, row.label(), theme::TEXT(), 14.0, clip);
            ctx.text.queue_ui_sized(txt_x, ry + 32.0, row.desc(), theme::TEXT_3(), 11.5, clip);

            // Control (right): a stepper for numeric rows, an on/off pill for
            // toggles, a value chip + cycle hint for the theme.
            let val = Self::value_str(&cur, row);
            let ctrl_right = box_x + box_w - 22.0;
            let val_col = if selected { theme::ACCENT_BRIGHT() } else { theme::TEXT_1() };

            if row.is_numeric() {
                // [ - ]  value  [ + ]   laid out right-to-left.
                let step = 22.0;
                let plus_x = ctrl_right - step;
                let py = ry + (row_h - step) * 0.5;
                ctx.dl_round(plus_x, py, step, step, 6.0, theme::BG_2());
                ctx.dl_stroke(plus_x, py, step, step, 6.0, theme::BORDER_STRONG(), 1.0);
                ctx.dl_icon(plus_x + 4.0, py + 4.0, 14.0, 14.0, icons::STAGE_PLUS, val_col, 1.7, false);

                let val_w = val.chars().count() as f32 * 7.5;
                let val_x = plus_x - 14.0 - val_w;
                ctx.text.queue_ui_sized(val_x, ry + (row_h - 14.0) * 0.5 - 1.0, &val, val_col, 14.0, clip);

                let minus_x = val_x - 14.0 - step;
                ctx.dl_round(minus_x, py, step, step, 6.0, theme::BG_2());
                ctx.dl_stroke(minus_x, py, step, step, 6.0, theme::BORDER_STRONG(), 1.0);
                ctx.dl_icon(minus_x + 4.0, py + 4.0, 14.0, 14.0, icons::UNSTAGE_MINUS, val_col, 1.7, false);
            } else if row == RowId::Theme {
                // A value chip showing the theme name (cycle on Enter/±).
                let chip_w = (val.chars().count() as f32 * 7.2 + 24.0).max(60.0);
                let chip_x = ctrl_right - chip_w;
                let cy = ry + (row_h - 24.0) * 0.5;
                ctx.dl_round(chip_x, cy, chip_w, 24.0, 7.0, theme::accent_a(0.12));
                ctx.dl_stroke(chip_x, cy, chip_w, 24.0, 7.0, theme::ACCENT_LINE(), 1.0);
                ctx.text.queue_ui_sized(chip_x + 12.0, cy + 5.0, &val, val_col, 12.5, clip);
            } else {
                // On/off pill toggle. Track + knob; accent when ON.
                let on = val == "On";
                let track_w = 42.0;
                let track_h = 22.0;
                let tx = ctrl_right - track_w;
                let ty = ry + (row_h - track_h) * 0.5;
                let (track, knob_off) = if on {
                    (theme::accent_a(0.40), track_w - track_h + 2.0)
                } else {
                    (theme::BG_4(), 2.0)
                };
                ctx.dl_round(tx, ty, track_w, track_h, track_h * 0.5, track);
                ctx.dl_stroke(tx, ty, track_w, track_h, track_h * 0.5, theme::BORDER_STRONG(), 1.0);
                let knob = track_h - 4.0;
                let knob_col = if on { theme::ACCENT_BRIGHT() } else { theme::TEXT_3() };
                ctx.dl_round(tx + knob_off, ty + 2.0, knob, knob, knob * 0.5, knob_col);
            }
        }

        // ---- footer hint ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER());
        let fty = foot_y + (foot_h - 11.0) * 0.5;
        ctx.text.queue_ui_sized(
            box_x + 18.0,
            fty,
            "\u{2191}\u{2193} move   \u{2212}/+ adjust   space toggle   esc close",
            theme::TEXT_3(),
            11.0,
            clip,
        );
        let tag = "Mighty Settings";
        ctx.text.queue_ui_sized(
            box_x + box_w - 18.0 - tag.chars().count() as f32 * 6.3,
            fty,
            tag,
            theme::ACCENT_BRIGHT(),
            11.0,
            clip,
        );
    }
}

fn on_off(b: bool) -> String {
    if b { "On".to_string() } else { "Off".to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acquire the process-wide settings/theme test lock AND reset both globals,
    /// returning the guard so the caller holds it for the whole test body (the
    /// globals are shared statics; parallel tests would otherwise race).
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = settings::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        settings::set_active(Settings::default());
        theme::set_active(ThemeId::Vivid);
        g
    }

    #[test]
    fn open_selects_first_row() {
        let _g = guard();
        let mut p = SettingsPanel::new();
        assert!(!p.is_active());
        p.open();
        assert!(p.is_active());
        assert_eq!(p.selection(), 0);
        assert_eq!(p.count(), 11);
    }

    #[test]
    fn move_wraps() {
        let _g = guard();
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(-1);
        assert_eq!(p.selection(), 10);
        p.move_sel(1);
        assert_eq!(p.selection(), 0);
    }

    #[test]
    fn toggle_inline_ai() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-iai-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(6); // InlineAi
        assert!(settings::inline_ai()); // default on
        p.toggle();
        assert!(!settings::inline_ai());
        p.toggle();
        assert!(settings::inline_ai());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn adjust_font_size_changes_live_metrics() {
        let _g = guard();
        // Redirect config to a temp dir so save() is isolated.
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let before = crate::layout::CHAR_W();
        let mut p = SettingsPanel::new();
        p.open(); // row 0 = FontSize
        p.adjust(2);
        // Font size went up -> cell advance (CHAR_W) and line height grow.
        assert!((settings::font_size() - (settings::DEFAULT_FONT_SIZE + 2.0)).abs() < 0.001);
        assert!(crate::layout::CHAR_W() > before, "CHAR_W should grow with font size");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn adjust_tab_width_propagates() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-tab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(1); // TabWidth
        p.adjust(2);
        assert_eq!(settings::tab_width(), 4);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn toggle_word_wrap_and_minimap() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-tog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(2); // WordWrap
        assert!(!settings::word_wrap());
        p.toggle();
        assert!(settings::word_wrap());
        p.move_sel(1); // Minimap
        assert!(settings::minimap());
        p.toggle();
        assert!(!settings::minimap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn toggle_bracket_colors_and_indent_guides() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-bg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(4); // BracketColors
        assert!(settings::bracket_colors()); // default on
        p.toggle();
        assert!(!settings::bracket_colors());
        p.move_sel(1); // IndentGuides
        assert!(settings::indent_guides()); // default on
        p.toggle();
        assert!(!settings::indent_guides());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn toggle_save_conveniences() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(7); // TrimWhitespace
        assert!(settings::trim_ws()); // default on
        p.toggle();
        assert!(!settings::trim_ws());
        p.move_sel(1); // FinalNewline
        assert!(settings::final_newline()); // default on
        p.toggle();
        assert!(!settings::final_newline());
        p.move_sel(1); // AutoSave
        assert!(!settings::autosave()); // default off
        p.toggle();
        assert!(settings::autosave());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn theme_row_cycles_and_persists() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-theme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        p.move_sel(10); // Theme
        assert_eq!(theme::active_id(), ThemeId::Vivid);
        p.toggle(); // cycle forward
        assert_eq!(theme::active_id(), ThemeId::Aurora);
        // Persisted to config.
        assert_eq!(crate::config::load_theme(), Some(ThemeId::Aurora));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn font_size_clamps_at_bounds() {
        let _g = guard();
        let tmp = std::env::temp_dir().join(format!("mui-setpanel-clamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut p = SettingsPanel::new();
        p.open();
        for _ in 0..100 {
            p.adjust(1);
        }
        assert_eq!(settings::font_size(), settings::FONT_MAX);
        for _ in 0..200 {
            p.adjust(-1);
        }
        assert_eq!(settings::font_size(), settings::FONT_MIN);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
