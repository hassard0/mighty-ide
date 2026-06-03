//! Interactive breadcrumb support (Feature 3): segment hit-testing + a quick
//! dropdown menu (file-of-folder list, or document symbols) styled like the
//! command palette.
//!
//! The breadcrumb draws `folder › file › symbol`. Clicking the **file** segment
//! opens a dropdown of the current folder's files (jump to open); clicking the
//! **symbol** segment opens a dropdown of the file's document symbols (the same
//! [`crate::outline`] data) to jump within the file. Hit-testing is a pure
//! geometry calc (unit-tested); the menu state mirrors the palette/completion
//! selection discipline and is drawn as a rounded card with an indigo selection.

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

/// Which breadcrumb segment a click landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Folder,
    File,
    Symbol,
    None,
}

/// The pixel widths of the three breadcrumb labels, so hit-testing can be
/// reproduced identically to the draw. All x in window pixels.
#[derive(Debug, Clone, Copy)]
pub struct CrumbLayout {
    pub left: f32,
    pub folder_w: f32,
    pub file_w: f32,
    pub symbol_w: f32,
}

impl CrumbLayout {
    /// Reproduce the breadcrumb x-advance math from `mui_breadcrumb_draw` and
    /// return the `[start, end)` x-range of each segment. The draw uses:
    /// `x = left + 16`, a 13px folder icon + 6px gap, then `name`, then a
    /// separator (`4 + 12 + 4`), then `file`, separator, a 13px fn icon + 5px
    /// gap, then `symbol`.
    pub fn segment_ranges(&self) -> [(Segment, f32, f32); 3] {
        let mut x = self.left + 16.0;
        // Folder: icon (13 + 6) + name.
        let folder_start = x;
        x += 13.0 + 6.0;
        x += self.folder_w;
        let folder_end = x;
        // separator: 4 + 12 + 4
        x += 4.0 + 12.0 + 4.0;
        // File: name.
        let file_start = x;
        x += self.file_w;
        let file_end = x;
        // separator
        x += 4.0 + 12.0 + 4.0;
        // Symbol: icon (13 + 5) + name.
        let sym_start = x;
        x += 13.0 + 5.0;
        x += self.symbol_w;
        let sym_end = x;
        [
            (Segment::Folder, folder_start, folder_end),
            (Segment::File, file_start, file_end),
            (Segment::Symbol, sym_start, sym_end),
        ]
    }

    /// The segment a click at window-x `cx` (already known to be on the
    /// breadcrumb row) lands on, or `Segment::None`.
    pub fn hit(&self, cx: f32) -> Segment {
        for (seg, s, e) in self.segment_ranges() {
            if cx >= s && cx < e {
                return seg;
            }
        }
        Segment::None
    }

    /// The left x where a dropdown for `seg` should be anchored.
    pub fn anchor_x(&self, seg: Segment) -> f32 {
        for (s, start, _e) in self.segment_ranges() {
            if s == seg {
                return start;
            }
        }
        self.left + 16.0
    }
}

/// One dropdown item: a display `label`, an optional per-kind icon (an SVG path
/// with a color), and a `target` the ABI uses to act (a file index, or a symbol
/// index, interpreted by the caller based on [`MenuKind`]).
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub icon: Option<&'static str>,
    pub icon_color: MuiColor,
    /// Extra indent steps (symbol depth) for nested symbols.
    pub depth: u32,
    /// The action target: file index (Files) or symbol index (Symbols).
    pub target: i32,
}

/// What the breadcrumb dropdown is listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Files,
    Symbols,
    None,
}

/// The breadcrumb dropdown state: kind + items + selection + anchor x.
#[derive(Debug)]
pub struct CrumbMenu {
    kind: MenuKind,
    items: Vec<MenuItem>,
    sel: usize,
    anchor_x: f32,
    active: bool,
}

impl Default for CrumbMenu {
    fn default() -> Self {
        CrumbMenu {
            kind: MenuKind::None,
            items: Vec::new(),
            sel: 0,
            anchor_x: 0.0,
            active: false,
        }
    }
}

impl CrumbMenu {
    pub fn new() -> Self {
        CrumbMenu::default()
    }

    /// Open the menu with `items` of `kind`, anchored at window-x `anchor_x`.
    /// Returns the item count (0 leaves it closed).
    pub fn open(&mut self, kind: MenuKind, items: Vec<MenuItem>, anchor_x: f32) -> usize {
        self.items = items;
        self.kind = kind;
        self.sel = 0;
        self.anchor_x = anchor_x;
        self.active = !self.items.is_empty() && kind != MenuKind::None;
        if !self.active {
            self.kind = MenuKind::None;
        }
        self.items.len()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn kind(&self) -> MenuKind {
        self.kind
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.items.len()
    }

    #[allow(dead_code)]
    pub fn selection(&self) -> usize {
        self.sel
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let mut s = self.sel as i32 + delta;
        let n_i = n as i32;
        s %= n_i;
        if s < 0 {
            s += n_i;
        }
        self.sel = s as usize;
    }

    /// The action target of the selected item (file/symbol index), or `-1`.
    pub fn selected_target(&self) -> i32 {
        if !self.active {
            return -1;
        }
        self.items.get(self.sel).map(|i| i.target).unwrap_or(-1)
    }

    /// The target of item `i` (for click hit-testing).
    pub fn target_at(&self, i: usize) -> i32 {
        self.items.get(i).map(|it| it.target).unwrap_or(-1)
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.items.clear();
        self.kind = MenuKind::None;
        self.sel = 0;
    }

    /// Map a window-y click to a menu row index (the card is drawn just below the
    /// breadcrumb at `anchor_x`). Returns `-1` if outside.
    pub fn row_at(&self, cy: f32) -> i32 {
        if !self.active {
            return -1;
        }
        let top = Self::card_top();
        let pad = 5.0;
        let row_h = layout::LINE_H();
        if cy < top + pad {
            return -1;
        }
        let i = ((cy - top - pad) / row_h).floor() as i32;
        if i >= 0 && (i as usize) < self.items.len() {
            i
        } else {
            -1
        }
    }

    /// The card's top y (just under the breadcrumb).
    fn card_top() -> f32 {
        layout::TAB_BAR_H + layout::BREADCRUMB_H + 2.0
    }

    /// Draw the dropdown card (palette-style: rounded elevated card, indigo
    /// selection, per-kind icons). No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext) {
        if !self.active || self.items.is_empty() {
            return;
        }
        let chrome = theme::CHROME_FONT_SIZE;
        let clip = ctx.clip;
        let w = ctx.gpu.width as f32;
        let h = ctx.gpu.height as f32;
        let row_h = layout::LINE_H();
        let pad = 5.0;

        let box_w = crumb_menu_card_width(
            w,
            crumb_menu_width(&mut ctx.text, &self.items, chrome),
        );
        // Clamp the visible rows so the card never runs off the bottom.
        let max_rows = (((h - 30.0) - Self::card_top() - 2.0 * pad) / row_h).floor() as usize;
        let shown = self.items.len().min(max_rows.max(1));
        let box_h = shown as f32 * row_h + 2.0 * pad;

        let box_x = crumb_menu_card_x(w, self.anchor_x, box_w);
        let box_y = Self::card_top();

        // Overlay so the card occludes editor glyphs underneath.
        let radius = 9.0_f32;
        ctx.dl_shadow(box_x, box_y + 6.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.7), 22.0);
        ctx.dl_grad_v(box_x, box_y, box_w, box_h, radius, theme::ELEVATED_2(), theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        for (i, it) in self.items.iter().take(shown).enumerate() {
            let row_y = box_y + pad + i as f32 * row_h;
            let selected = i == self.sel;
            if selected {
                ctx.dl_grad_h(box_x + 5.0, row_y + 2.0, box_w - 10.0, row_h - 4.0, 5.0, theme::accent_a(0.20), 0.9);
                ctx.dl_stroke(box_x + 5.0, row_y + 2.0, box_w - 10.0, row_h - 4.0, 5.0, theme::ACCENT_LINE(), 1.0);
            }
            let indent = it.depth as f32 * 12.0;
            let ix = box_x + 12.0 + indent;
            if let Some(icon) = it.icon {
                ctx.dl_icon(ix, row_y + (row_h - 14.0) * 0.5, 14.0, 14.0, icon, it.icon_color, 1.5, false);
            }
            let tx = ix + 20.0;
            let ty = row_y + (row_h - chrome) * 0.5 - 1.0;
            let fg = if selected { theme::TEXT() } else { theme::TEXT_1() };
            let label = fit_crumb_menu_label(&mut ctx.text, &it.label, ((box_x + box_w - 10.0) - tx).max(0.0), chrome);
            ctx.text.queue_ui_sized(tx, ty, &label, fg, chrome, clip);
        }
    }
}

fn crumb_menu_width(text: &mut crate::text::Text, items: &[MenuItem], size: f32) -> f32 {
    let content = items
        .iter()
        .map(|i| text.measure_ui_sized(&i.label, size).0 + i.depth as f32 * 12.0)
        .fold(0.0_f32, f32::max);
    (content + 64.0).clamp(220.0, 460.0)
}

fn crumb_menu_card_width(window_w: f32, wanted_w: f32) -> f32 {
    wanted_w.min(window_w.max(1.0))
}

fn crumb_menu_card_x(window_w: f32, anchor_x: f32, box_w: f32) -> f32 {
    let rail_x = layout::RAIL_W + 4.0;
    let min_x = if rail_x + box_w <= window_w { rail_x } else { 0.0 };
    let max_x = (window_w - box_w).max(min_x);
    (anchor_x - 6.0).clamp(min_x, max_x)
}

fn fit_crumb_menu_label(text: &mut crate::text::Text, label: &str, max_px: f32, size: f32) -> String {
    if max_px < 8.0 || label.is_empty() {
        return String::new();
    }
    if text.measure_ui_sized(label, size).0 <= max_px {
        return label.to_string();
    }
    let suffix = "\u{2026}";
    let suffix_w = text.measure_ui_sized(suffix, size).0;
    if suffix_w > max_px {
        return String::new();
    }
    let chars: Vec<char> = label.chars().collect();
    for keep in (1..chars.len()).rev() {
        let mut candidate: String = chars.iter().take(keep).collect();
        candidate.push_str(suffix);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            return candidate;
        }
    }
    suffix.to_string()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_fixture() -> CrumbLayout {
        CrumbLayout {
            left: 300.0, // rail + sidebar
            folder_w: 63.0, // "workspace" at 7px avg in this pure fixture
            file_w: 56.0,   // "main.mty"
            symbol_w: 28.0, // "main"
        }
    }

    #[test]
    fn segment_ranges_are_ordered_and_disjoint() {
        let l = layout_fixture();
        let r = l.segment_ranges();
        // folder before file before symbol, each non-empty.
        assert!(r[0].1 < r[0].2);
        assert!(r[0].2 <= r[1].1);
        assert!(r[1].1 < r[1].2);
        assert!(r[1].2 <= r[2].1);
        assert!(r[2].1 < r[2].2);
    }

    #[test]
    fn hit_picks_the_right_segment() {
        let l = layout_fixture();
        let r = l.segment_ranges();
        // Mid-point of each range hits that segment.
        let mid = |i: usize| (r[i].1 + r[i].2) * 0.5;
        assert_eq!(l.hit(mid(0)), Segment::Folder);
        assert_eq!(l.hit(mid(1)), Segment::File);
        assert_eq!(l.hit(mid(2)), Segment::Symbol);
        // Far left of the first segment -> None.
        assert_eq!(l.hit(l.left), Segment::None);
        // Far right past the symbol -> None.
        assert_eq!(l.hit(r[2].2 + 50.0), Segment::None);
    }

    #[test]
    fn anchor_x_matches_segment_start() {
        let l = layout_fixture();
        let r = l.segment_ranges();
        assert_eq!(l.anchor_x(Segment::Folder), r[0].1);
        assert_eq!(l.anchor_x(Segment::File), r[1].1);
        assert_eq!(l.anchor_x(Segment::Symbol), r[2].1);
    }

    #[test]
    fn menu_open_select_move() {
        let mut m = CrumbMenu::new();
        assert_eq!(m.open(MenuKind::Files, vec![], 100.0), 0);
        assert!(!m.is_active());
        let items = vec![
            MenuItem { label: "a.mty".into(), icon: None, icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0), depth: 0, target: 0 },
            MenuItem { label: "b.mty".into(), icon: None, icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0), depth: 0, target: 1 },
            MenuItem { label: "c.mty".into(), icon: None, icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0), depth: 0, target: 2 },
        ];
        assert_eq!(m.open(MenuKind::Files, items, 120.0), 3);
        assert!(m.is_active());
        assert_eq!(m.kind(), MenuKind::Files);
        assert_eq!(m.selection(), 0);
        assert_eq!(m.selected_target(), 0);
        m.move_sel(1);
        assert_eq!(m.selected_target(), 1);
        m.move_sel(2); // wrap: 1 -> 0
        assert_eq!(m.selection(), 0);
        m.move_sel(-1); // wrap to last
        assert_eq!(m.selection(), 2);
        assert_eq!(m.target_at(2), 2);
        m.cancel();
        assert!(!m.is_active());
        assert_eq!(m.selected_target(), -1);
    }

    #[test]
    fn symbol_menu_carries_depth() {
        let mut m = CrumbMenu::new();
        let items = vec![
            MenuItem { label: "Point".into(), icon: Some("x"), icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0), depth: 0, target: 0 },
            MenuItem { label: "x".into(), icon: Some("y"), icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0), depth: 1, target: 1 },
        ];
        assert_eq!(m.open(MenuKind::Symbols, items, 200.0), 2);
        assert_eq!(m.kind(), MenuKind::Symbols);
    }

    #[test]
    fn menu_width_uses_measured_label_width_and_depth() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(480, 240) else {
            return;
        };
        let shallow = vec![MenuItem {
            label: "component_navigation_controller.mty".into(),
            icon: None,
            icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0),
            depth: 0,
            target: 0,
        }];
        let deep = vec![MenuItem {
            label: "component_navigation_controller.mty".into(),
            icon: None,
            icon_color: MuiColor::new(1.0, 1.0, 1.0, 1.0),
            depth: 3,
            target: 0,
        }];

        assert!(crumb_menu_width(&mut ctx.text, &deep, theme::CHROME_FONT_SIZE)
            > crumb_menu_width(&mut ctx.text, &shallow, theme::CHROME_FONT_SIZE));
    }

    #[test]
    fn menu_card_width_clamps_inside_ultra_narrow_windows() {
        assert_eq!(crumb_menu_card_width(180.0, 220.0), 180.0);
        assert_eq!(crumb_menu_card_width(500.0, 220.0), 220.0);
    }

    #[test]
    fn menu_card_x_respects_rail_when_space_allows() {
        let x = crumb_menu_card_x(900.0, 20.0, 220.0);
        assert_eq!(x, layout::RAIL_W + 4.0);
    }

    #[test]
    fn menu_card_x_clamps_to_viewport_edges() {
        assert_eq!(crumb_menu_card_x(180.0, 300.0, 180.0), 0.0);
        let x = crumb_menu_card_x(500.0, 490.0, 220.0);
        assert!(x + 220.0 <= 500.0 + 0.5);
    }

    #[test]
    fn menu_label_fits_measured_budget() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(360, 240) else {
            return;
        };
        let shown = fit_crumb_menu_label(
            &mut ctx.text,
            "very_long_file_name_that_should_fit_before_the_menu_edge.mty",
            120.0,
            theme::CHROME_FONT_SIZE,
        );
        let (shown_w, _) = ctx.text.measure_ui_sized(&shown, theme::CHROME_FONT_SIZE);

        assert!(shown.ends_with('\u{2026}'));
        assert!(shown_w <= 120.0);
    }
}
