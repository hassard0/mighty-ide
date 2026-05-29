//! Command palette (shim-side, scalar-driven from Mighty).
//!
//! Mirrors [`crate::completion`]: the command registry + the query/filter/
//! selection state live here on the Rust side because the Mighty IDE drives the
//! shim through a scalar-only `extern c` ABI (L17) and keeps its own `Vec`
//! access flat (L21). Mighty opens the palette (Ctrl+Shift+P), feeds typed
//! chars / backspaces, moves the selection, then on Enter reads the selected
//! command id back and dispatches to the SAME code path the keybinding triggers.
//!
//! Filtering is a case-insensitive prefix-OR-subsequence (fuzzy) match against
//! each command's label, ranked so prefix matches sort ahead of looser fuzzy
//! matches. An empty query lists every command in registry order.

use crate::ffi::MuiColor;
use crate::theme;

/// A single editor command in the palette: a stable numeric `id` (the contract
/// with the Mighty dispatch switch), a human `label`, and the `keybinding`
/// string shown right-aligned. `id`s are stable so reordering the table or
/// filtering never changes what Enter dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub id: u32,
    pub label: &'static str,
    pub keybinding: &'static str,
}

// Command ids — kept in sync with the dispatch switch in `src/main.mty`
// (`fn cmd_*` helpers). Stable numeric contract; do not renumber casually.
pub const CMD_OPEN_FILE: u32 = 1;
pub const CMD_SAVE: u32 = 2;
pub const CMD_FIND: u32 = 3;
pub const CMD_GOTO_LINE: u32 = 4;
pub const CMD_GOTO_DEFINITION: u32 = 5;
pub const CMD_HOVER: u32 = 6;
pub const CMD_TOGGLE_TERMINAL: u32 = 7;
pub const CMD_TOGGLE_SIDEBAR: u32 = 8;
pub const CMD_NEXT_TAB: u32 = 9;
pub const CMD_PREV_TAB: u32 = 10;
pub const CMD_CLOSE_TAB: u32 = 11;
pub const CMD_FORMAT_DOCUMENT: u32 = 12;
pub const CMD_UNDO: u32 = 13;
pub const CMD_REDO: u32 = 14;
pub const CMD_AUTOCOMPLETE: u32 = 15;
pub const CMD_JUMP_BACK: u32 = 16;
pub const CMD_QUIT: u32 = 17;

/// The static command registry. Every action the editor exposes appears here
/// with its keybinding label. Registry order is the default (empty-query) order.
pub const COMMANDS: &[Command] = &[
    Command { id: CMD_OPEN_FILE,        label: "Open File",          keybinding: "Ctrl+O" },
    Command { id: CMD_SAVE,             label: "Save",               keybinding: "Ctrl+S" },
    Command { id: CMD_FIND,             label: "Find",               keybinding: "Ctrl+F" },
    Command { id: CMD_GOTO_LINE,        label: "Go to Line",         keybinding: "Ctrl+G" },
    Command { id: CMD_GOTO_DEFINITION,  label: "Go to Definition",   keybinding: "F12" },
    Command { id: CMD_HOVER,            label: "Show Hover",         keybinding: "Ctrl+K" },
    Command { id: CMD_TOGGLE_TERMINAL,  label: "Toggle Terminal",    keybinding: "Ctrl+`" },
    Command { id: CMD_TOGGLE_SIDEBAR,   label: "Toggle Sidebar",     keybinding: "Ctrl+B" },
    Command { id: CMD_NEXT_TAB,         label: "Next Tab",           keybinding: "Ctrl+Tab" },
    Command { id: CMD_PREV_TAB,         label: "Previous Tab",       keybinding: "Ctrl+Shift+Tab" },
    Command { id: CMD_CLOSE_TAB,        label: "Close Tab",          keybinding: "Ctrl+W" },
    Command { id: CMD_FORMAT_DOCUMENT,  label: "Format Document",    keybinding: "Ctrl+Shift+I" },
    Command { id: CMD_UNDO,             label: "Undo",               keybinding: "Ctrl+Z" },
    Command { id: CMD_REDO,             label: "Redo",               keybinding: "Ctrl+Y" },
    Command { id: CMD_AUTOCOMPLETE,     label: "Trigger Autocomplete", keybinding: "Ctrl+Space" },
    Command { id: CMD_JUMP_BACK,        label: "Jump Back",          keybinding: "Ctrl+-" },
    Command { id: CMD_QUIT,             label: "Quit",               keybinding: "Esc / close" },
];

/// Match quality for ranking. Lower sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// Label starts with the query (case-insensitive).
    Prefix = 0,
    /// Query is a contiguous substring of the label.
    Substring = 1,
    /// Query chars appear in order (subsequence / fuzzy).
    Fuzzy = 2,
}

/// Score `label` against the lowercased `query`. Returns `None` if it doesn't
/// match at all, else the rank (for sorting). An empty query matches everything
/// at [`Rank::Prefix`] (so registry order is preserved).
fn score(label: &str, query_lc: &str) -> Option<Rank> {
    if query_lc.is_empty() {
        return Some(Rank::Prefix);
    }
    let label_lc = label.to_ascii_lowercase();
    if label_lc.starts_with(query_lc) {
        return Some(Rank::Prefix);
    }
    if label_lc.contains(query_lc) {
        return Some(Rank::Substring);
    }
    // Subsequence test: every query char appears in order in the label.
    let mut q = query_lc.chars().peekable();
    for lc in label_lc.chars() {
        if let Some(&qc) = q.peek() {
            if lc == qc {
                q.next();
            }
        } else {
            break;
        }
    }
    if q.peek().is_none() {
        Some(Rank::Fuzzy)
    } else {
        None
    }
}

/// Filter + rank `commands` against `query`. Returns the matching commands in
/// rank order (prefix, then substring, then fuzzy), ties broken by original
/// registry index so the order is deterministic. Pure + unit-tested.
pub fn filter_commands(commands: &[Command], query: &str) -> Vec<Command> {
    let query_lc = query.to_ascii_lowercase();
    let mut scored: Vec<(Rank, usize, Command)> = commands
        .iter()
        .enumerate()
        .filter_map(|(i, c)| score(c.label, &query_lc).map(|r| (r, i, *c)))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, c)| c).collect()
}

/// Max rows drawn in the palette at once (the visible window).
const VISIBLE: usize = 12;

/// Shim-owned palette state: the typed query, the filtered command list, and
/// the selection. Mirrors [`crate::completion::CompletionEngine`].
#[derive(Debug, Default)]
pub struct PaletteEngine {
    /// `true` while the palette overlay is open.
    active: bool,
    /// The typed query (lowercased matching happens in [`score`]).
    query: String,
    /// The filtered command list for the current query (in rank order).
    filtered: Vec<Command>,
    /// Selected index into `filtered` (0-based).
    sel: usize,
}

impl PaletteEngine {
    pub fn new() -> Self {
        PaletteEngine::default()
    }

    /// Open the palette: clear the query, list all commands, select the first.
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.sel = 0;
        self.refilter();
    }

    /// Recompute the filtered list for the current query, clamping the selection.
    fn refilter(&mut self) {
        self.filtered = filter_commands(COMMANDS, &self.query);
        if self.sel >= self.filtered.len() {
            self.sel = self.filtered.len().saturating_sub(1);
        }
    }

    /// Append a typed char to the query and refilter (selection resets to top).
    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.sel = 0;
        self.refilter();
    }

    /// Delete the last query char and refilter (selection resets to top).
    pub fn backspace(&mut self) {
        self.query.pop();
        self.sel = 0;
        self.refilter();
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn count(&self) -> usize {
        self.filtered.len()
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Move the selection by `delta` (positive = down), wrapping around.
    pub fn move_sel(&mut self, delta: i32) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let n_i = n as i32;
        let mut s = self.sel as i32 + delta;
        s %= n_i;
        if s < 0 {
            s += n_i;
        }
        self.sel = s as usize;
    }

    /// The command id of the current selection, or `-1` when there is no match.
    pub fn selected_id(&self) -> i32 {
        self.filtered
            .get(self.sel)
            .map(|c| c.id as i32)
            .unwrap_or(-1)
    }

    /// Close the palette and clear its state.
    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
        self.filtered.clear();
        self.sel = 0;
    }

    /// First visible row index so the selected item stays within the window.
    pub fn scroll_top(&self) -> usize {
        if self.filtered.len() <= VISIBLE {
            return 0;
        }
        if self.sel < VISIBLE {
            0
        } else {
            (self.sel + 1).saturating_sub(VISIBLE)
        }
    }

    /// A vector icon + a short description for a command id, matching the
    /// mockup's rich palette rows.
    fn meta(id: u32) -> (&'static str, &'static str, bool) {
        use crate::icons;
        // (icon path, description, fill?)
        match id {
            CMD_OPEN_FILE => (icons::NEW_FILE, "Open a file from the workspace", false),
            CMD_SAVE => (icons::FILE_MTY, "Write the active file to disk", false),
            CMD_FIND => (icons::SEARCH, "Search within the current document", false),
            CMD_GOTO_LINE => (icons::CHEVRON, "Jump to a specific line number", false),
            CMD_GOTO_DEFINITION => (icons::FN_SYMBOL, "Navigate to the symbol definition", false),
            CMD_HOVER => (icons::INFO_I, "Show type & docs at the cursor", false),
            CMD_TOGGLE_TERMINAL => (icons::TEST_BOX, "Open the integrated terminal", false),
            CMD_TOGGLE_SIDEBAR => (icons::EXPLORER, "Show or hide the file explorer", false),
            CMD_NEXT_TAB => (icons::CHEVRON, "Switch to the next open tab", false),
            CMD_PREV_TAB => (icons::CHEVRON, "Switch to the previous open tab", false),
            CMD_CLOSE_TAB => (icons::CLOSE, "Close the active editor tab", false),
            CMD_FORMAT_DOCUMENT => (icons::PLUS, "Apply mightyfmt to active file", false),
            CMD_UNDO => (icons::CHEVRON, "Undo the last edit", false),
            CMD_REDO => (icons::CHEVRON, "Redo the last undone edit", false),
            CMD_AUTOCOMPLETE => (icons::AGENTS, "Suggest completions at the cursor", false),
            CMD_JUMP_BACK => (icons::CHEVRON, "Return to the previous location", false),
            CMD_QUIT => (icons::CLOSE, "Close the editor", false),
            _ => (icons::CHEVRON, "", false),
        }
    }

    /// Draw the rich command palette overlay (mockup `.palette`): a dim scrim, a
    /// rounded indigo-glow card with a search field (magnifier + caret + ⌘K
    /// pill), a "COMMANDS" category, rows with icon + title + dim description +
    /// right-aligned kbd pills (selected row indigo-tinted with a key-glow), and
    /// a footer hint line. No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.active {
            return;
        }
        use crate::icons;
        let w = width as f32;
        let h = height as f32;
        let chrome = theme::CHROME_FONT_SIZE;
        let clip = ctx.clip;

        let top = self.scroll_top();
        let shown = self.filtered.len().saturating_sub(top).min(VISIBLE).min(6);

        // Card geometry (mockup: 600px wide, search 56px, cat 25px, rows 50px,
        // footer ~37px).
        let box_w = 600.0_f32.min(w - 80.0);
        let search_h = 56.0;
        let cat_h = 25.0;
        let row_h = 50.0;
        let foot_h = 37.0;
        let box_h = search_h + cat_h + shown as f32 * row_h + 10.0 + foot_h;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = 96.0_f32.min((h - box_h).max(0.0));
        let radius = 12.0_f32;

        // Scrim: dim + a faint indigo top wash.
        ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, 0.55));
        ctx.dl_grad_v(0.0, 0.0, w, h * 0.5, 0.0, theme::hex(0x7c5cff, 0.05), theme::hex(0x7c5cff, 0.0));
        // Drop shadow + indigo glow + card + border.
        ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.85), 40.0);
        ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW, 40.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED);
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG, 1.0);

        // ---- search field ----
        ctx.dl_rect(box_x + 1.0, box_y + search_h - 1.0, box_w - 2.0, 1.0, theme::BORDER);
        ctx.dl_icon(box_x + 18.0, box_y + (search_h - 20.0) * 0.5, 20.0, 20.0, icons::SEARCH, theme::DIM, 1.7, false);
        let q_text_x = box_x + 50.0;
        let qy = box_y + (search_h - 16.0) * 0.5 - 1.0;
        let (q_str, q_color): (&str, _) = if self.query.is_empty() {
            ("Type a command\u{2026}", theme::TEXT_3)
        } else {
            (self.query.as_str(), theme::TEXT)
        };
        // Search font is larger (16px) per the mockup.
        ctx.text.queue_ui_sized(q_text_x, qy, q_str, q_color, 16.0, clip);
        let qadv = 16.0 * 0.52;
        let caret_x = q_text_x + self.query.chars().count() as f32 * qadv + 1.0;
        ctx.dl_round(caret_x, box_y + (search_h - 18.0) * 0.5, 2.0, 18.0, 1.0, theme::ACCENT_BRIGHT);
        // ⌘K mode pill (right).
        let pill_w = 40.0;
        let pill_x = box_x + box_w - pill_w - 18.0;
        let pill_y = box_y + (search_h - 22.0) * 0.5;
        ctx.dl_round(pill_x, pill_y, pill_w, 22.0, 5.0, theme::ACCENT_FAINT);
        ctx.dl_stroke(pill_x, pill_y, pill_w, 22.0, 5.0, theme::ACCENT_LINE, 1.0);
        ctx.text.queue_ui_sized(pill_x + 8.0, pill_y + 4.5, "\u{2318}K", theme::ACCENT_BRIGHT, 10.5, clip);

        // ---- category label ----
        let cat_y = box_y + search_h + 9.0;
        let cat: String = "COMMANDS".chars().flat_map(|c| [c, '\u{2009}']).collect();
        ctx.text.queue_ui_sized(box_x + 18.0, cat_y, &cat, theme::TEXT_3, chrome - 2.5, clip);

        // ---- rows ----
        let list_top = box_y + search_h + cat_h;
        for vis in 0..shown {
            let idx = top + vis;
            let cmd = &self.filtered[idx];
            let ry = list_top + vis as f32 * row_h;
            let selected = idx == self.sel;
            let (icon, desc, fill) = Self::meta(cmd.id);
            if selected {
                ctx.dl_grad_h(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::hex(0x7c5cff, 0.22), 0.9);
                ctx.dl_stroke(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_LINE, 1.0);
                ctx.dl_shadow(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_GLOW, 16.0);
            }
            // Leading icon tile (30px rounded, bordered).
            let tile = 30.0;
            let tile_x = box_x + 18.0;
            let tile_y = ry + (row_h - tile) * 0.5;
            if selected {
                ctx.dl_round(tile_x, tile_y, tile, tile, 7.0, theme::hex(0x7c5cff, 0.10));
                ctx.dl_stroke(tile_x, tile_y, tile, tile, 7.0, theme::ACCENT_LINE, 1.0);
            } else {
                ctx.dl_round(tile_x, tile_y, tile, tile, 7.0, theme::BG_2);
                ctx.dl_stroke(tile_x, tile_y, tile, tile, 7.0, theme::BORDER, 1.0);
            }
            let icon_col = if selected { theme::ACCENT_BRIGHT } else { theme::TEXT_1 };
            ctx.dl_icon(tile_x + 6.5, tile_y + 6.5, 17.0, 17.0, icon, icon_col, 1.6, fill);

            // Title + dim description (two lines).
            let txt_x = box_x + 60.0;
            ctx.text.queue_ui_sized(txt_x, ry + 11.0, cmd.label, theme::TEXT, 13.5, clip);
            if !desc.is_empty() {
                ctx.text.queue_ui_sized(txt_x, ry + 28.0, desc, theme::TEXT_3, 11.5, clip);
            }

            // Right-aligned kbd pills.
            let parts: Vec<&str> = cmd.keybinding.split('+').collect();
            let pill_pad = 7.0;
            let gap = 4.0;
            let kadv = 11.0 * 0.55;
            let widths: Vec<f32> = parts
                .iter()
                .map(|p| (p.chars().count() as f32 * kadv + 2.0 * pill_pad).max(22.0))
                .collect();
            let total_w: f32 = widths.iter().sum::<f32>() + gap * (parts.len().saturating_sub(1)) as f32;
            let mut px = box_x + box_w - 20.0 - total_w;
            let pill_h = 21.0;
            let py = ry + (row_h - pill_h) * 0.5;
            for (k, part) in parts.iter().enumerate() {
                let pw = widths[k];
                let (pbg, pborder, pfg) = if selected {
                    (theme::hex(0x7c5cff, 0.10), theme::ACCENT_LINE, theme::ACCENT_BRIGHT)
                } else {
                    (theme::BG_2, theme::BORDER_STRONG, theme::TEXT_1)
                };
                ctx.dl_round(px, py, pw, pill_h, 5.0, pbg);
                ctx.dl_stroke(px, py, pw, pill_h, 5.0, pborder, 1.0);
                let lbl_w = part.chars().count() as f32 * kadv;
                ctx.text.queue_ui_sized(px + (pw - lbl_w) * 0.5, py + 4.5, part, pfg, 11.0, clip);
                px += pw + gap;
            }
        }

        // ---- footer hint line ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER);
        ctx.dl_round(box_x + 1.0, foot_y, box_w - 2.0, foot_h - 1.0, 0.0, theme::BG_2);
        let fty = foot_y + (foot_h - chrome + 1.0) * 0.5 - 1.0;
        let mut fx = box_x + 18.0;
        let foot_seg = |ctx: &mut crate::MuiContext, key: &str, label: &str, fx: &mut f32| {
            let kw = (key.chars().count() as f32 * 6.0 + 10.0).max(20.0);
            ctx.dl_round(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BG_1);
            ctx.dl_stroke(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BORDER_STRONG, 1.0);
            ctx.text.queue_ui_sized(*fx + 5.0, foot_y + (foot_h - 10.0) * 0.5, key, theme::TEXT_1, 10.0, clip);
            *fx += kw + 6.0;
            ctx.text.queue_ui_sized(*fx, fty, label, theme::TEXT_3, 11.0, clip);
            *fx += label.chars().count() as f32 * 6.0 + 16.0;
        };
        foot_seg(ctx, "\u{2191}\u{2193}", "navigate", &mut fx);
        foot_seg(ctx, "\u{21B5}", "select", &mut fx);
        foot_seg(ctx, "esc", "dismiss", &mut fx);
        let tag = "Mighty Command Palette";
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - tag.chars().count() as f32 * 6.3, fty, tag, theme::ACCENT_BRIGHT, 11.0, clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_are_unique() {
        let mut ids: Vec<u32> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "command ids must be unique");
    }

    #[test]
    fn empty_query_lists_all_in_registry_order() {
        let got = filter_commands(COMMANDS, "");
        let ids: Vec<u32> = got.iter().map(|c| c.id).collect();
        let expected: Vec<u32> = COMMANDS.iter().map(|c| c.id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn prefix_match_ranks_first() {
        // "for" prefixes "Format Document"; should appear, prefix-ranked.
        let got = filter_commands(COMMANDS, "for");
        assert_eq!(got.first().map(|c| c.id), Some(CMD_FORMAT_DOCUMENT));
    }

    #[test]
    fn filter_is_case_insensitive() {
        let lower = filter_commands(COMMANDS, "save");
        let upper = filter_commands(COMMANDS, "SAVE");
        let lo: Vec<u32> = lower.iter().map(|c| c.id).collect();
        let up: Vec<u32> = upper.iter().map(|c| c.id).collect();
        assert_eq!(lo, up);
        assert_eq!(lo.first(), Some(&CMD_SAVE));
    }

    #[test]
    fn substring_and_fuzzy_match() {
        // "term" is a substring of "Toggle Terminal".
        let got = filter_commands(COMMANDS, "term");
        assert!(got.iter().any(|c| c.id == CMD_TOGGLE_TERMINAL));
        // "gtd" is a subsequence of "Go to Definition" (fuzzy).
        let fuzzy = filter_commands(COMMANDS, "gtd");
        assert!(
            fuzzy.iter().any(|c| c.id == CMD_GOTO_DEFINITION),
            "fuzzy subsequence should match: {fuzzy:?}"
        );
    }

    #[test]
    fn prefix_beats_substring_in_order() {
        // "ta": "Toggle Terminal"/"Toggle Sidebar"? No. Use "t": prefixes nothing
        // but matches many. Use a query where a prefix and a substring coexist.
        // "g" prefixes "Go to Line"/"Go to Definition" (Prefix) and is a substring
        // of "Toggle ..." (Substring) — prefixes must come first.
        let got = filter_commands(COMMANDS, "g");
        let first_two: Vec<u32> = got.iter().take(2).map(|c| c.id).collect();
        assert!(
            first_two.contains(&CMD_GOTO_LINE) && first_two.contains(&CMD_GOTO_DEFINITION),
            "prefix matches (Go to ...) should rank ahead of substring matches: {got:?}"
        );
    }

    #[test]
    fn no_match_returns_empty() {
        let got = filter_commands(COMMANDS, "zzqqxx");
        assert!(got.is_empty());
    }

    #[test]
    fn engine_open_lists_all_selects_first() {
        let mut e = PaletteEngine::new();
        assert!(!e.is_active());
        e.open();
        assert!(e.is_active());
        assert_eq!(e.count(), COMMANDS.len());
        assert_eq!(e.selection(), 0);
        assert_eq!(e.selected_id(), COMMANDS[0].id as i32);
    }

    #[test]
    fn engine_typing_filters_and_resets_selection() {
        let mut e = PaletteEngine::new();
        e.open();
        e.move_sel(3);
        assert_eq!(e.selection(), 3);
        // Type "sa" -> matches "Save"; selection resets to 0.
        e.push_char('s');
        e.push_char('a');
        assert_eq!(e.selection(), 0);
        assert_eq!(e.selected_id(), CMD_SAVE as i32);
        // Backspace back to "s".
        e.backspace();
        assert_eq!(e.query(), "s");
        assert!(e.count() > 1);
    }

    #[test]
    fn engine_move_wraps() {
        let mut e = PaletteEngine::new();
        e.open();
        let n = e.count();
        assert!(n >= 2);
        e.move_sel(-1);
        assert_eq!(e.selection(), n - 1); // wrap below 0 -> last
        e.move_sel(1);
        assert_eq!(e.selection(), 0); // wrap above end -> first
    }

    #[test]
    fn engine_selected_id_is_negative_when_no_match() {
        let mut e = PaletteEngine::new();
        e.open();
        for ch in "zzqqxx".chars() {
            e.push_char(ch);
        }
        assert_eq!(e.count(), 0);
        assert_eq!(e.selected_id(), -1);
    }

    #[test]
    fn engine_cancel_clears() {
        let mut e = PaletteEngine::new();
        e.open();
        e.push_char('s');
        e.cancel();
        assert!(!e.is_active());
        assert_eq!(e.count(), 0);
        assert_eq!(e.query(), "");
        assert_eq!(e.selected_id(), -1);
    }

    #[test]
    fn scroll_top_keeps_selection_visible() {
        let mut e = PaletteEngine::new();
        e.open(); // all commands, count > VISIBLE only if registry large enough
        if e.count() <= VISIBLE {
            // Registry smaller than the window: top is always 0.
            assert_eq!(e.scroll_top(), 0);
            return;
        }
        for _ in 0..(e.count() - 1) {
            e.move_sel(1);
        }
        let expected = (e.selection() + 1).saturating_sub(VISIBLE);
        assert_eq!(e.scroll_top(), expected);
    }
}
