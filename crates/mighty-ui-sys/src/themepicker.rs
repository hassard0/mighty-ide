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

    fn geometry(width: u32, height: u32) -> (f32, f32, f32, f32, f32, f32) {
        let w = width as f32;
        let h = height as f32;
        let rows = ThemeId::ALL.len();
        let head_h = 50.0_f32;
        let row_h = 64.0_f32;
        let foot_h = 34.0_f32;
        let box_w = theme_picker_width(w);
        let box_h = head_h + rows as f32 * row_h + foot_h + 12.0;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = ((h - box_h) * 0.5).max(40.0);
        let list_top = box_y + head_h;
        (box_x, box_y, box_w, box_h, list_top, row_h)
    }

    fn close_rect(width: u32, height: u32) -> (f32, f32, f32, f32) {
        let (box_x, box_y, box_w, _box_h, _list_top, _row_h) = Self::geometry(width, height);
        (box_x + box_w - 38.0, box_y + 13.0, 24.0, 24.0)
    }

    /// Preview the row under a click. Returns 1 when a theme row was selected,
    /// 2 when the close button was hit, and 0 for a miss.
    pub fn click(&mut self, x: f32, y: f32, width: u32, height: u32) -> i32 {
        if !self.active {
            return 0;
        }
        let (box_x, box_y, box_w, _box_h, list_top, row_h) = Self::geometry(width, height);
        if x < box_x || x > box_x + box_w || y < box_y {
            return 0;
        }
        let (cx, cy, cw, ch) = Self::close_rect(width, height);
        if (cx..=cx + cw).contains(&x) && (cy..=cy + ch).contains(&y) {
            return 2;
        }
        if y < list_top {
            return 0;
        }
        let row = ((y - list_top) / row_h).floor() as i32;
        if row < 0 || row as usize >= ThemeId::ALL.len() {
            return 0;
        }
        self.sel = row as usize;
        theme::set_active(self.selected_id());
        1
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
    /// Returns the committed theme's index and whether persistence succeeded.
    pub fn commit(&mut self) -> (i32, bool) {
        let id = self.selected_id();
        theme::set_active(id);
        let persisted = crate::config::save_theme(id);
        self.active = false;
        self.original = None;
        (id.index(), persisted)
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

        let head_h = 50.0_f32;
        let row_h = 64.0_f32;
        let foot_h = 34.0_f32;
        let (box_x, box_y, box_w, box_h, _list_top, _row_h) = Self::geometry(width, height);
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
        let (cx, cy, cw, ch) = Self::close_rect(width, height);
        ctx.dl_round(cx, cy, cw, ch, 6.0, theme::BG_2());
        ctx.dl_stroke(cx, cy, cw, ch, 6.0, theme::BORDER_STRONG(), 1.0);
        ctx.dl_icon(cx + 5.0, cy + 5.0, 14.0, 14.0, icons::CLOSE, theme::TEXT_1(), 1.6, false);
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
            let text_right = theme_row_text_right(box_x, box_w);
            let text_max = (text_right - txt_x).max(0.0);
            let name = fit_theme_text(&mut ctx.text, id.name(), text_max, 14.0);
            ctx.text.queue_ui_sized(txt_x, ry + 16.0, &name, theme::TEXT(), 14.0, clip);
            let desc = match id {
                ThemeId::Vivid => "Dark · electric indigo",
                ThemeId::Aurora => "Dark glass · aurora cyan",
                ThemeId::Warm => "Light · warm paper · ember",
            };
            let desc = fit_theme_text(&mut ctx.text, desc, text_max, 11.5);
            ctx.text.queue_ui_sized(txt_x, ry + 36.0, &desc, theme::TEXT_3(), 11.5, clip);

            // Check on the highlighted row (right edge).
            if selected {
                ctx.dl_round(box_x + box_w - 46.0, ry + (row_h - 26.0) * 0.5, 26.0, 26.0, 7.0, theme::accent_a(0.16));
                ctx.dl_stroke(box_x + box_w - 46.0, ry + (row_h - 26.0) * 0.5, 26.0, 26.0, 7.0, theme::ACCENT_LINE(), 1.0);
                ctx.dl_icon(box_x + box_w - 40.0, ry + (row_h - 14.0) * 0.5, 14.0, 14.0, selected_theme_icon(), theme::ACCENT_BRIGHT(), 1.8, false);
            }
        }

        // ---- footer hint ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER());
        let fty = foot_y + (foot_h - 11.0) * 0.5;
        let tag = "Mighty Themes";
        let (tag_w, _) = ctx.text.measure_ui_sized(tag, 11.0);
        let tag_x = box_x + box_w - 18.0 - tag_w;
        let hint_x = box_x + 18.0;
        let hint_max = (tag_x - 20.0 - hint_x).max(0.0);
        let hint = fit_theme_text(&mut ctx.text, "\u{2191}\u{2193} preview   Enter apply   esc revert", hint_max, 11.0);
        ctx.text.queue_ui_sized(hint_x, fty, &hint, theme::TEXT_3(), 11.0, clip);
        ctx.text.queue_ui_sized(tag_x, fty, tag, theme::ACCENT_BRIGHT(), 11.0, clip);
    }
}

fn theme_row_text_right(box_x: f32, box_w: f32) -> f32 {
    box_x + box_w - 56.0
}

fn theme_picker_width(window_w: f32) -> f32 {
    (window_w - 80.0).max(0.0).clamp(280.0, 460.0).min(window_w.max(1.0))
}

fn fit_theme_text(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if max_px < 8.0 || s.is_empty() {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let suffix = "...";
    let suffix_w = text.measure_ui_sized(suffix, size).0;
    if suffix_w > max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    for keep in (1..chars.len()).rev() {
        let mut candidate: String = chars.iter().take(keep).collect();
        candidate.push_str(suffix);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            return candidate;
        }
    }
    suffix.to_string()
}

fn selected_theme_icon() -> &'static str {
    crate::icons::CHECK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        theme::set_active(ThemeId::Vivid);
    }

    #[test]
    fn open_selects_active_theme_row() {
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        // Share the crate-wide test lock (settings/theme/config persistence all
        // mutate global APPDATA) so this can't race other persistence tests.
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Redirect config to a temp dir so commit's save is isolated.
        let tmp = std::env::temp_dir().join(format!("mighty-ide-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        reset();
        let mut p = ThemePicker::new();
        p.open();
        p.move_sel(1); // aurora
        let (idx, persisted) = p.commit();
        assert_eq!(idx, ThemeId::Aurora.index());
        assert!(persisted);
        assert_eq!(theme::active_id(), ThemeId::Aurora);
        assert!(!p.is_active());
        assert_eq!(crate::config::load_theme(), Some(ThemeId::Aurora));
        let _ = std::fs::remove_dir_all(&tmp);
        reset();
    }

    #[test]
    fn move_wraps() {
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let mut p = ThemePicker::new();
        p.open(); // row 0
        p.move_sel(-1);
        assert_eq!(p.selection(), 2); // wrap to last
        p.move_sel(1);
        assert_eq!(p.selection(), 0);
        reset();
    }

    #[test]
    fn mouse_click_previews_theme_row() {
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let mut p = ThemePicker::new();
        p.open();
        let (box_x, _box_y, _box_w, _box_h, list_top, row_h) = ThemePicker::geometry(900, 700);
        assert_eq!(p.click(box_x + 24.0, list_top + row_h + 8.0, 900, 700), 1);
        assert_eq!(p.selection(), 1);
        assert_eq!(theme::active_id(), ThemeId::Aurora);
        assert_eq!(p.click(box_x - 2.0, list_top + 8.0, 900, 700), 0);
        p.cancel();
        reset();
    }

    #[test]
    fn row_text_budget_stops_before_check_control() {
        let (box_x, _box_y, box_w, _box_h, _list_top, _row_h) = ThemePicker::geometry(900, 700);
        let txt_x = box_x + 72.0;
        let right = theme_row_text_right(box_x, box_w);

        assert!(right < box_x + box_w - 46.0);
        assert!(txt_x < right);
    }

    #[test]
    fn geometry_clamps_card_inside_ultra_narrow_windows() {
        let (box_x, _box_y, box_w, _box_h, _list_top, _row_h) = ThemePicker::geometry(180, 560);

        assert!(box_x >= 0.0);
        assert!(box_w <= 180.0);
        assert!(box_x + box_w <= 180.0 + 0.5);
    }

    #[test]
    fn picker_width_preserves_preferred_width_until_viewport_is_tiny() {
        assert_eq!(theme_picker_width(900.0), 460.0);
        assert_eq!(theme_picker_width(360.0), 280.0);
        assert_eq!(theme_picker_width(180.0), 180.0);
    }

    #[test]
    fn theme_text_fits_measured_budget() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 220) else {
            return;
        };
        let shown = fit_theme_text(
            &mut ctx.text,
            "Light · warm paper · ember with a very long suffix",
            96.0,
            11.5,
        );
        let (shown_w, _) = ctx.text.measure_ui_sized(&shown, 11.5);

        assert!(shown.ends_with("..."));
        assert!(shown_w <= 96.0);
    }

    #[test]
    fn close_rect_is_distinct_from_rows() {
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let mut p = ThemePicker::new();
        p.open();
        let (cx, cy, cw, ch) = ThemePicker::close_rect(900, 700);
        assert_eq!(p.click(cx + cw * 0.5, cy + ch * 0.5, 900, 700), 2);
        assert!(p.is_active());
        reset();
    }

    #[test]
    fn selected_theme_uses_check_affordance() {
        assert_eq!(selected_theme_icon(), crate::icons::CHECK);
        assert_ne!(selected_theme_icon(), crate::icons::PLUS);
    }
}
