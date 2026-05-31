//! The Welcome / first-impression screen (shim-side, scalar-driven from Mighty).
//!
//! Shown in the editor body when no real file is open (a fresh empty scratch
//! buffer), and reachable any time from the command palette ("Welcome"). It is a
//! branded landing: the big **Mighty wordmark** with the ember/indigo accent, a
//! tagline, a **Recently Opened** column (from the Quick-Open MRU — click to
//! open), a **Quick actions** column (Open File / Quick Open / Command Palette /
//! New File / Toggle Theme), and a small **tips / keybinding** cheat list, all
//! centered over the theme's atmospheric background.
//!
//! Per L21 the layout + hit-testing live here; Mighty asks `mui_welcome_active`
//! each frame, draws via `mui_welcome_draw`, and routes clicks through
//! `mui_welcome_click(x,y) -> action`. Action ids map to existing IDE commands
//! (or an MRU recent index) on the Mighty side.

use std::path::PathBuf;

use crate::ffi::MuiColor;
use crate::{icons, theme};

/// Click-action ids returned by [`WelcomeState::click`]. Negative = none.
/// Quick-action ids are stable small integers the Mighty side maps to existing
/// command dispatch. MRU recents return `ACTION_RECENT_BASE + index`.
pub const ACTION_NONE: i32 = -1;
pub const ACTION_OPEN_FILE: i32 = 1;
pub const ACTION_QUICK_OPEN: i32 = 2;
pub const ACTION_COMMAND_PALETTE: i32 = 3;
pub const ACTION_NEW_FILE: i32 = 4;
#[allow(dead_code)]
pub const ACTION_TOGGLE_THEME: i32 = 5;
pub const ACTION_OPEN_FOLDER: i32 = 6;
/// MRU recents: returned id is `ACTION_RECENT_BASE + i` (i = row in the recents
/// list). The Mighty side reads the path back via [`WelcomeState::recent_path`].
pub const ACTION_RECENT_BASE: i32 = 1000;
/// Recent FOLDERS: returned id is `ACTION_RECENT_FOLDER_BASE + i`. The Mighty
/// side reads the folder back via [`WelcomeState::recent_folder`] and opens it
/// as the workspace.
pub const ACTION_RECENT_FOLDER_BASE: i32 = 2000;

/// One quick-action row: icon + label + keybinding hint + the action id.
struct QuickAction {
    icon: &'static str,
    label: &'static str,
    key: &'static str,
    action: i32,
}

const QUICK_ACTIONS: &[QuickAction] = &[
    QuickAction { icon: icons::EXPLORER, label: "Open File\u{2026}", key: "Ctrl+O", action: ACTION_OPEN_FILE },
    QuickAction { icon: icons::FOLDER, label: "Open Folder\u{2026}", key: "Ctrl+Shift+O", action: ACTION_OPEN_FOLDER },
    QuickAction { icon: icons::SEARCH, label: "Quick Open", key: "Ctrl+P", action: ACTION_QUICK_OPEN },
    QuickAction { icon: icons::TEST_BOX, label: "Command Palette", key: "Ctrl+Shift+P", action: ACTION_COMMAND_PALETTE },
    QuickAction { icon: icons::NEW_FILE, label: "New File", key: "", action: ACTION_NEW_FILE },
];

/// A small keybinding cheat row (label + chord).
struct Tip {
    what: &'static str,
    key: &'static str,
}

const TIPS: &[Tip] = &[
    Tip { what: "Go to Definition", key: "F12" },
    Tip { what: "Find in File", key: "Ctrl+F" },
    Tip { what: "Format Document", key: "Ctrl+Shift+I" },
    Tip { what: "Zen / Focus Mode", key: "Ctrl+K Z" },
    Tip { what: "Integrated Terminal", key: "Ctrl+`" },
];

/// Pixel rectangle for a clickable region (window space).
#[derive(Clone, Copy, Debug)]
struct Hit {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    action: i32,
}

impl Hit {
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

/// Shim-owned Welcome screen state. Holds the hit-test rectangles built during
/// the last draw (so a subsequent click maps to the right action), plus a
/// snapshot of the recents shown (paths) so the Mighty side can resolve a
/// recent-row click back to a path.
#[derive(Debug, Default)]
pub struct WelcomeState {
    /// When `true`, the Welcome screen is FORCED open (via the palette command)
    /// even though a file is loaded. Cleared when a file is opened.
    pub force_open: bool,
    /// When `true`, an intentionally-created empty untitled tab is allowed to
    /// show as a blank editor instead of being treated as the startup "no file"
    /// state. Cleared as soon as a real file becomes active or Welcome is forced.
    hide_empty_auto: bool,
    /// Hit rectangles from the last draw (action id per region).
    hits: Vec<Hit>,
    /// The recent file paths shown in the last draw (index = recents row).
    recents: Vec<PathBuf>,
    /// The recent FOLDER paths shown in the last draw (index = folder row).
    recent_folders: Vec<PathBuf>,
}

impl WelcomeState {
    pub fn new() -> Self {
        WelcomeState::default()
    }

    /// Force the Welcome screen open (palette "Welcome" command).
    pub fn open(&mut self) {
        self.force_open = true;
        self.hide_empty_auto = false;
    }

    /// Dismiss the forced Welcome screen (e.g. a file was opened).
    pub fn dismiss(&mut self) {
        self.force_open = false;
    }

    /// Hide the automatic empty-buffer Welcome state for an explicit New File.
    pub fn dismiss_empty_auto(&mut self) {
        self.force_open = false;
        self.hide_empty_auto = true;
    }

    /// Re-enable automatic Welcome when the active tab becomes file-backed.
    pub fn allow_empty_auto(&mut self) {
        self.hide_empty_auto = false;
    }

    pub fn hides_empty_auto(&self) -> bool {
        self.hide_empty_auto
    }

    /// Resolve a recents row to its path (for an `ACTION_RECENT_BASE + i` click).
    pub fn recent_path(&self, i: usize) -> Option<&PathBuf> {
        self.recents.get(i)
    }

    /// Resolve a recent-folder row to its path (for an
    /// `ACTION_RECENT_FOLDER_BASE + i` click).
    pub fn recent_folder(&self, i: usize) -> Option<&PathBuf> {
        self.recent_folders.get(i)
    }

    /// Hit-test a window-space click against the last drawn layout. Returns the
    /// action id, or [`ACTION_NONE`].
    pub fn click(&self, px: f32, py: f32) -> i32 {
        for hit in &self.hits {
            if hit.contains(px, py) {
                return hit.action;
            }
        }
        ACTION_NONE
    }

    /// Draw the Welcome screen filling the editor body region. `recents` is the
    /// MRU list (newest first) so the "Recently Opened" column can be clicked.
    /// Records hit rects + the recents snapshot for the next [`click`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        ctx: &mut crate::MuiContext,
        region_left: f32,
        region_top: f32,
        width: u32,
        height: u32,
        recents: &[PathBuf],
        folders: &[PathBuf],
    ) {
        self.hits.clear();
        self.recents.clear();
        self.recent_folders.clear();

        let clip = ctx.clip;
        let w = width as f32;
        let h = height as f32;
        let bx = region_left;
        let by = region_top;
        let bw = (w - bx).max(0.0);
        let bh = (h - by).max(0.0);

        // Paint the editor field over the body so it reads as a clean canvas
        // (the atmosphere already shows behind it; this keeps contrast).
        ctx.dl_rect(bx, by, bw, bh, theme::BG_EDIT());

        // Center column. Generous max width so it breathes on wide windows.
        let col_w = 720.0_f32.min(bw - 96.0).max(360.0);
        let cx = bx + (bw - col_w) * 0.5;
        // Vertical rhythm: start a bit above the optical center.
        let mut y = by + (bh * 0.16).max(40.0);

        // ---- Brand: ember/indigo logo tile + wordmark ----
        let tile = 64.0_f32;
        let tx = cx;
        // Rounded accent tile with a glow + the Mighty chevron mark.
        ctx.dl_shadow(tx, y + 8.0, tile, tile, 16.0, theme::ACCENT_GLOW(), 40.0);
        ctx.dl_grad_v(tx, y, tile, tile, 16.0, theme::ACCENT_BRIGHT(), theme::ACCENT());
        ctx.dl_stroke(tx, y, tile, tile, 16.0, theme::accent_a(0.5), 1.0);
        // The "M" mark, in on-accent ink (white reads on the saturated tile in
        // every theme).
        let mark_ink = MuiColor::new(1.0, 1.0, 1.0, 0.96);
        ctx.dl_icon(tx + 14.0, y + 14.0, 36.0, 36.0, icons::LANG_M, mark_ink, 2.6, false);

        // Wordmark to the right of the tile.
        let word_x = tx + tile + 22.0;
        ctx.text.queue_ui_styled(
            word_x,
            y + 8.0,
            "Mighty",
            theme::TEXT(),
            40.0,
            crate::vello_ui::FontStyle::Bold,
            clip,
        );
        ctx.text.queue_ui_sized(
            word_x + 2.0,
            y + 50.0,
            "The agent-first language IDE",
            theme::DIM(),
            14.5,
            clip,
        );

        y += tile + 44.0;

        // ---- Two columns: Quick actions (left) | Recently Opened (right) ----
        let gutter = 40.0_f32;
        // The START (quick actions) column carries the longest content (label +
        // chord) so it gets more room; RECENT (folders/files) takes the rest. This
        // keeps the chord from colliding with either the label or the right column.
        let left_w = (col_w - gutter) * 0.58;
        let half = (col_w - gutter) * 0.42; // RIGHT column (recents) width
        let left_x = cx;
        let right_x = cx + left_w + gutter;
        let row_h = 40.0_f32;

        // Section headers (bold UI face).
        ctx.text.queue_ui_styled(
            left_x, y, "START", theme::TEXT_3(), 11.5, crate::vello_ui::FontStyle::Bold, clip,
        );
        ctx.text.queue_ui_styled(
            right_x, y, "RECENT FOLDERS", theme::TEXT_3(), 11.5, crate::vello_ui::FontStyle::Bold, clip,
        );
        let rows_top = y + 22.0;

        // Quick actions (left column).
        for (i, qa) in QUICK_ACTIONS.iter().enumerate() {
            let ry = rows_top + i as f32 * row_h;
            // Icon chip.
            ctx.dl_round(left_x, ry + 4.0, 28.0, 28.0, 8.0, theme::BG_4());
            ctx.dl_icon(left_x + 6.0, ry + 10.0, 16.0, 16.0, qa.icon, theme::ACCENT_BRIGHT(), 1.7, false);
            // Label.
            ctx.text
                .queue_ui_sized(left_x + 40.0, ry + 9.0, qa.label, theme::TEXT_1(), 14.0, clip);
            // Keybinding hint: right-aligned within the column, BUT never closer
            // than 16px after the label (long labels like "Open Folder…" /
            // "Command Palette" used to collide with their chord). Push the hint
            // right past the label end when needed.
            if !qa.key.is_empty() {
                let kw = qa.key.chars().count() as f32 * 6.4;
                let label_w = qa.label.chars().count() as f32 * 7.8; // 14px UI advance (over-estimate)
                let right_aligned = left_x + left_w - kw - 4.0;
                let after_label = left_x + 40.0 + label_w + 16.0;
                let key_x = right_aligned.max(after_label);
                ctx.text.queue_ui_sized(key_x, ry + 11.0, qa.key, theme::TEXT_3(), 11.5, clip);
            }
            self.hits.push(Hit {
                x: left_x,
                y: ry,
                w: left_w,
                h: row_h,
                action: qa.action,
            });
        }

        // Recent FOLDERS (right column, top). Workspaces are the new emphasis of
        // the Open-Folder feature, so they lead; recent files follow below.
        let folder_rows = QUICK_ACTIONS.len().min(3);
        if folders.is_empty() {
            ctx.text.queue_ui_sized(
                right_x,
                rows_top + 9.0,
                "No recent folders yet",
                theme::TEXT_3(),
                13.0,
                clip,
            );
        } else {
            for (i, path) in folders.iter().take(folder_rows).enumerate() {
                let ry = rows_top + i as f32 * row_h;
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                let dir = path.to_string_lossy().into_owned();
                // Folder glyph + name + dim full path.
                ctx.dl_icon(right_x, ry + 10.0, 16.0, 16.0, icons::FOLDER, theme::ACCENT_BRIGHT(), 1.6, false);
                ctx.text
                    .queue_ui_sized(right_x + 26.0, ry + 6.0, &name, theme::TEXT_1(), 13.5, clip);
                let dir_short = shorten_dir(&dir, half - 30.0);
                ctx.text.queue_ui_sized(
                    right_x + 26.0,
                    ry + 23.0,
                    &dir_short,
                    theme::TEXT_3(),
                    11.0,
                    clip,
                );
                self.hits.push(Hit {
                    x: right_x,
                    y: ry,
                    w: half,
                    h: row_h,
                    action: ACTION_RECENT_FOLDER_BASE + i as i32,
                });
                self.recent_folders.push(path.clone());
            }
        }

        // Recent FILES (right column, below the folders).
        let files_top = rows_top + (folder_rows as f32) * row_h + 10.0;
        ctx.text.queue_ui_styled(
            right_x, files_top, "RECENT FILES", theme::TEXT_3(), 11.5, crate::vello_ui::FontStyle::Bold, clip,
        );
        let files_rows_top = files_top + 22.0;
        if recents.is_empty() {
            ctx.text.queue_ui_sized(
                right_x,
                files_rows_top + 9.0,
                "No recent files yet",
                theme::TEXT_3(),
                13.0,
                clip,
            );
        } else {
            let max_rows = QUICK_ACTIONS.len().saturating_sub(folder_rows).max(2);
            for (i, path) in recents.iter().take(max_rows).enumerate() {
                let ry = files_rows_top + i as f32 * row_h;
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                let dir = path
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ctx.dl_icon(right_x, ry + 10.0, 16.0, 16.0, file_icon(&name), theme::ACCENT_BRIGHT(), 1.6, false);
                ctx.text
                    .queue_ui_sized(right_x + 26.0, ry + 6.0, &name, theme::TEXT_1(), 13.5, clip);
                if !dir.is_empty() {
                    let dir_short = shorten_dir(&dir, half - 30.0);
                    ctx.text.queue_ui_sized(
                        right_x + 26.0,
                        ry + 23.0,
                        &dir_short,
                        theme::TEXT_3(),
                        11.0,
                        clip,
                    );
                }
                self.hits.push(Hit {
                    x: right_x,
                    y: ry,
                    w: half,
                    h: row_h,
                    action: ACTION_RECENT_BASE + i as i32,
                });
                self.recents.push(path.clone());
            }
        }

        // ---- Tips / keybinding cheat list (centered footer band) ----
        let tips_y = rows_top + (QUICK_ACTIONS.len() as f32) * row_h + 28.0;
        ctx.dl_rect(left_x, tips_y - 14.0, col_w, 1.0, theme::BORDER());
        ctx.text
            .queue_ui_sized(left_x, tips_y, "TIPS", theme::TEXT_3(), 11.5, clip);
        let tip_top = tips_y + 22.0;
        // Two tips per row to keep the footer compact.
        let tip_col_w = col_w * 0.5;
        for (i, tip) in TIPS.iter().enumerate() {
            let col = (i % 2) as f32;
            let row = (i / 2) as f32;
            let txx = left_x + col * tip_col_w;
            let tyy = tip_top + row * 26.0;
            ctx.text
                .queue_ui_sized(txx, tyy, tip.what, theme::DIM(), 12.5, clip);
            // Keybinding pill, right-aligned in its half.
            let kw = tip.key.chars().count() as f32 * 6.6 + 14.0;
            let px = txx + tip_col_w - kw - 18.0;
            ctx.dl_round(px, tyy - 2.0, kw, 18.0, 5.0, theme::BG_4());
            ctx.dl_stroke(px, tyy - 2.0, kw, 18.0, 5.0, theme::BORDER(), 1.0);
            ctx.text
                .queue_ui_sized(px + 7.0, tyy + 1.0, tip.key, theme::TEXT_3(), 10.5, clip);
        }
    }
}

/// Pick a file glyph by extension.
fn file_icon(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".mty") {
        icons::FILE_MTY
    } else if lower.ends_with(".toml") {
        icons::FILE_TOML
    } else if lower.ends_with(".md") {
        icons::FILE_MD
    } else {
        icons::FILE_TXT
    }
}

/// Shorten a directory path to roughly `max_px` from the LEFT, with a leading
/// ellipsis when truncated (so the meaningful tail stays visible).
fn shorten_dir(dir: &str, max_px: f32) -> String {
    let approx = 6.0_f32;
    let max_chars = (max_px / approx).floor().max(8.0) as usize;
    let count = dir.chars().count();
    if count <= max_chars {
        return dir.to_string();
    }
    let tail: String = dir
        .chars()
        .skip(count - max_chars.saturating_sub(1))
        .collect();
    format!("\u{2026}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal hit set the way `draw` does, then assert click mapping.
    fn synthetic() -> WelcomeState {
        let mut w = WelcomeState::new();
        // Mirror two rows of the left column + one recent.
        w.hits.push(Hit { x: 100.0, y: 200.0, w: 300.0, h: 40.0, action: ACTION_OPEN_FILE });
        w.hits.push(Hit { x: 100.0, y: 240.0, w: 300.0, h: 40.0, action: ACTION_QUICK_OPEN });
        w.hits.push(Hit { x: 500.0, y: 200.0, w: 300.0, h: 40.0, action: ACTION_RECENT_BASE });
        w.recents.push(PathBuf::from("/proj/src/main.mty"));
        // A recent folder row + its backing path.
        w.hits.push(Hit { x: 500.0, y: 300.0, w: 300.0, h: 40.0, action: ACTION_RECENT_FOLDER_BASE });
        w.recent_folders.push(PathBuf::from("/proj"));
        w
    }

    #[test]
    fn click_maps_to_action() {
        let w = synthetic();
        // Inside the Open File row.
        assert_eq!(w.click(150.0, 210.0), ACTION_OPEN_FILE);
        // Inside the Quick Open row.
        assert_eq!(w.click(150.0, 250.0), ACTION_QUICK_OPEN);
        // Inside the first recent row.
        assert_eq!(w.click(550.0, 210.0), ACTION_RECENT_BASE);
        // Outside any row.
        assert_eq!(w.click(50.0, 50.0), ACTION_NONE);
        assert_eq!(w.click(150.0, 900.0), ACTION_NONE);
    }

    #[test]
    fn recent_action_resolves_path() {
        let w = synthetic();
        let action = w.click(550.0, 210.0);
        assert!(action >= ACTION_RECENT_BASE);
        let idx = (action - ACTION_RECENT_BASE) as usize;
        assert_eq!(w.recent_path(idx).unwrap(), &PathBuf::from("/proj/src/main.mty"));
        assert!(w.recent_path(99).is_none());
    }

    #[test]
    fn recent_folder_action_resolves_path() {
        let w = synthetic();
        let action = w.click(550.0, 310.0);
        assert!(action >= ACTION_RECENT_FOLDER_BASE);
        let idx = (action - ACTION_RECENT_FOLDER_BASE) as usize;
        assert_eq!(w.recent_folder(idx).unwrap(), &PathBuf::from("/proj"));
        assert!(w.recent_folder(99).is_none());
    }

    #[test]
    fn force_open_toggles() {
        let mut w = WelcomeState::new();
        assert!(!w.force_open);
        w.open();
        assert!(w.force_open);
        w.dismiss();
        assert!(!w.force_open);
    }

    #[test]
    fn file_icon_by_ext() {
        assert_eq!(file_icon("main.mty"), icons::FILE_MTY);
        assert_eq!(file_icon("Cargo.toml"), icons::FILE_TOML);
        assert_eq!(file_icon("README.md"), icons::FILE_MD);
        assert_eq!(file_icon("notes.txt"), icons::FILE_TXT);
    }
}
