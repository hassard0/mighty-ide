//! Color-theme chooser overlay (shim-side, scalar-driven from Mighty).
//!
//! Opened by the "Preferences: Color Theme" command (or any future keybinding).
//! It lists the three themes; Up/Down PREVIEW the highlighted theme LIVE (the
//! whole IDE re-skins as you move), Enter COMMITS the selection and persists it
//! to config, Escape REVERTS to the theme that was active when the picker
//! opened. Mirrors [`crate::palette::PaletteEngine`]: all state lives here and
//! Mighty only opens / moves / reads / commits via the scalar `mui_theme_*` ABI.

use crate::ffi::MuiColor;
use crate::theme::{self, ThemeId};

/// Shim-owned theme-picker state.
#[derive(Debug, Default)]
pub struct ThemePicker {
    active: bool,
    /// Highlighted row (0-based index into [`ThemeId::ALL`]).
    sel: usize,
    /// The theme active when the picker opened, restored on cancel.
    original: Option<ThemeId>,
}

impl ThemePicker {
    pub fn new() -> Self {
        ThemePicker::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Open the picker, remembering the current theme and selecting its row so
    /// the highlight starts on the active theme.
    pub fn open(&mut self) {
        let cur = theme::active_id();
        self.original = Some(cur);
        self.sel = ThemeId::ALL.iter().position(|&t| t == cur).unwrap_or(0);
        self.active = true;
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        ThemeId::ALL.len()
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    pub fn selected_id(&self) -> ThemeId {
        ThemeId::ALL[self.sel.min(ThemeId::ALL.len() - 1)]
    }

    /// Move the highlight by `delta` (wrapping) AND preview that theme live.
    pub fn move_sel(&mut self, delta: i32) {
        let n = ThemeId::ALL.len() as i32;
        let mut s = self.sel as i32 + delta;
        s %= n;
        if s < 0 {
            s += n;
        }
        self.sel = s as usize;
        // Live preview: re-skin to the highlighted theme immediately.
        theme::set_active(self.selected_id());
    }

    /// Commit the highlighted theme: keep it active, persist to config, close.
    /// Returns the committed theme's index.
    pub fn commit(&mut self) -> i32 {
        let id = self.selected_id();
        theme::set_active(id);
        crate::config::save_theme(id);
        self.active = false;
        self.original = None;
        id.index()
    }

    /// Cancel: revert to the theme that was active when the picker opened.
    pub fn cancel(&mut self) {
        if let Some(orig) = self.original.take() {
            theme::set_active(orig);
        }
        self.active = false;
    }

    /// Draw the centered theme-chooser card: a dim scrim, a rounded elevated
    /// card titled "Color Theme", and one row per theme with a name, a short
    /// description, a swatch strip (bg / accent / a syntax color) and a check on
    /// the highlighted row. No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.active {
            return;
        }
        use crate::icons;
        let w = width as f32;
        let h = height as f32;
        let clip = ctx.clip;

        let rows = ThemeId::ALL.len();
        let head_h = 50.0_f32;
        let row_h = 64.0_f32;
        let foot_h = 34.0_f32;
        let box_w = 460.0_f32.min(w - 80.0);
        let box_h = head_h + rows as f32 * row_h + foot_h + 12.0;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = ((h - box_h) * 0.5).max(40.0);
        let radius = 12.0_f32;

        // Scrim (lighter on a light theme so it doesn't go muddy).
        let scrim_a = if theme::is_light() { 0.28 } else { 0.55 };
        ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, scrim_a));

        // Drop shadow + accent glow + card + border.
        ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, theme::SHADOW(), 40.0);
        ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW(), 36.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        // ---- header ----
        ctx.dl_icon(box_x + 18.0, box_y + (head_h - 18.0) * 0.5, 18.0, 18.0, icons::SETTINGS, theme::ACCENT_BRIGHT(), 1.7, false);
        ctx.text.queue_ui_sized(box_x + 46.0, box_y + (head_h - 16.0) * 0.5 - 1.0, "Color Theme", theme::TEXT(), 16.0, clip);
        ctx.dl_rect(box_x + 1.0, box_y + head_h - 1.0, box_w - 2.0, 1.0, theme::BORDER());

        // ---- rows ----
        let list_top = box_y + head_h;
        for (i, &id) in ThemeId::ALL.iter().enumerate() {
            let ry = list_top + i as f32 * row_h;
            let selected = i == self.sel;
            let preview = id.theme();
            if selected {
                ctx.dl_grad_h(box_x + 8.0, ry + 4.0, box_w - 16.0, row_h - 8.0, 8.0, theme::accent_a(0.20), 0.9);
                ctx.dl_stroke(box_x + 8.0, ry + 4.0, box_w - 16.0, row_h - 8.0, 8.0, theme::ACCENT_LINE(), 1.0);
            }

            // Swatch strip: a 36px rounded tile filled with the theme's bg, with
            // an accent bar + a syntax dot so each option reads at a glance.
            let sw = 40.0;
            let sx = box_x + 18.0;
            let sy = ry + (row_h - sw) * 0.5;
            ctx.dl_round(sx, sy, sw, sw, 8.0, preview.bg);
            ctx.dl_stroke(sx, sy, sw, sw, 8.0, preview.border_strong, 1.0);
            // accent chip (top-left), string-syntax chip (bottom-right).
            ctx.dl_round(sx + 6.0, sy + 6.0, 14.0, 14.0, 4.0, preview.accent);
            ctx.dl_round(sx + sw - 17.0, sy + sw - 17.0, 11.0, 11.0, 3.0, preview.syn_string);
            ctx.dl_round(sx + sw - 17.0, sy + 6.0, 11.0, 11.0, 3.0, preview.syn_keyword);

            // Name + description.
            let txt_x = box_x + 72.0;
            ctx.text.queue_ui_sized(txt_x, ry + 16.0, id.name(), theme::TEXT(), 14.0, clip);
            let desc = match id {
                ThemeId::Vivid => "Dark · electric indigo",
                ThemeId::Aurora => "Dark glass · aurora cyan",
                ThemeId::Warm => "Light · warm paper · ember",
            };
            ctx.text.queue_ui_sized(txt_x, ry + 36.0, desc, theme::TEXT_3(), 11.5, clip);

            // Check on the highlighted row (right edge).
            if selected {
                ctx.dl_icon(box_x + box_w - 40.0, ry + (row_h - 18.0) * 0.5, 18.0, 18.0, icons::PLUS, theme::ACCENT_BRIGHT(), 2.0, false);
            }
        }

        // ---- footer hint ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER());
        let fty = foot_y + (foot_h - 11.0) * 0.5;
        ctx.text.queue_ui_sized(box_x + 18.0, fty, "\u{2191}\u{2193} preview   \u{21B5} apply   esc revert", theme::TEXT_3(), 11.0, clip);
        let tag = "Mighty Themes";
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - tag.chars().count() as f32 * 6.3, fty, tag, theme::ACCENT_BRIGHT(), 11.0, clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        theme::set_active(ThemeId::Vivid);
    }

    #[test]
    fn open_selects_active_theme_row() {
        reset();
        theme::set_active(ThemeId::Aurora);
        let mut p = ThemePicker::new();
        p.open();
        assert!(p.is_active());
        assert_eq!(p.selected_id(), ThemeId::Aurora);
        reset();
    }

    #[test]
    fn move_previews_live() {
        reset();
        let mut p = ThemePicker::new();
        p.open(); // active vivid -> row 0
        assert_eq!(theme::active_id(), ThemeId::Vivid);
        p.move_sel(1);
        assert_eq!(p.selected_id(), ThemeId::Aurora);
        // Preview applied live.
        assert_eq!(theme::active_id(), ThemeId::Aurora);
        reset();
    }

    #[test]
    fn cancel_reverts_preview() {
        reset(); // vivid active
        let mut p = ThemePicker::new();
        p.open();
        p.move_sel(2); // preview warm
        assert_eq!(theme::active_id(), ThemeId::Warm);
        p.cancel();
        // Reverted to the originally-active theme.
        assert_eq!(theme::active_id(), ThemeId::Vivid);
        assert!(!p.is_active());
        reset();
    }

    #[test]
    fn commit_keeps_and_persists() {
        // Redirect config to a temp dir so commit's save is isolated.
        let tmp = std::env::temp_dir().join(format!("mighty-ide-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        reset();
        let mut p = ThemePicker::new();
        p.open();
        p.move_sel(1); // aurora
        let idx = p.commit();
        assert_eq!(idx, ThemeId::Aurora.index());
        assert_eq!(theme::active_id(), ThemeId::Aurora);
        assert!(!p.is_active());
        assert_eq!(crate::config::load_theme(), Some(ThemeId::Aurora));
        let _ = std::fs::remove_dir_all(&tmp);
        reset();
    }

    #[test]
    fn move_wraps() {
        reset();
        let mut p = ThemePicker::new();
        p.open(); // row 0
        p.move_sel(-1);
        assert_eq!(p.selection(), 2); // wrap to last
        p.move_sel(1);
        assert_eq!(p.selection(), 0);
        reset();
    }
}
