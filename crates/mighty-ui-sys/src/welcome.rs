//! The Welcome / first-impression screen (shim-side, scalar-driven from Mighty).
//!
//! Shown in the editor body when no real file is open (a fresh empty scratch
//! buffer), and reachable any time from the command palette ("Welcome"). It is a
//! branded landing: the big **Mighty wordmark** with the ember/indigo accent, a
//! tagline, a **Recently Opened** column (from the Quick-Open MRU — click to
//! open), a **Quick actions** column (Open File / Quick Open / Command Palette /
//! New File / New Project dialogs), and a small **tips / keybinding** cheat list, all
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
#[allow(dead_code)]
pub const ACTION_NEW_FOLDER: i32 = 7;
pub const ACTION_NEW_PROJECT: i32 = 8;
pub const ACTION_CLOSE: i32 = 9;
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
    QuickAction { icon: icons::NEW_FILE, label: "New File\u{2026}", key: "Ctrl+N", action: ACTION_NEW_FILE },
    QuickAction { icon: icons::NEW_FOLDER, label: "New Project\u{2026}", key: "", action: ACTION_NEW_PROJECT },
    QuickAction { icon: icons::EXPLORER, label: "Open File\u{2026}", key: "Ctrl+O", action: ACTION_OPEN_FILE },
    QuickAction { icon: icons::FOLDER, label: "Open Folder\u{2026}", key: "Ctrl+Shift+O", action: ACTION_OPEN_FOLDER },
    QuickAction { icon: icons::SEARCH, label: "Quick Open", key: "Ctrl+P", action: ACTION_QUICK_OPEN },
    QuickAction { icon: icons::TEST_BOX, label: "Command Palette", key: "Ctrl+Shift+P", action: ACTION_COMMAND_PALETTE },
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
    Tip { what: "Zen / Focus Mode", key: "Alt+Z" },
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

/// Return the x position for a quick-action shortcut hint, or `None` when the
/// label + minimum gap + shortcut cannot fit in the action column.
fn quick_action_key_x(
    col_x: f32,
    col_w: f32,
    label_x: f32,
    label_w: f32,
    key_w: f32,
    gap: f32,
    right_pad: f32,
) -> Option<f32> {
    let right_edge = col_x + col_w - right_pad;
    let after_label = label_x + label_w + gap;
    if after_label + key_w > right_edge {
        return None;
    }
    Some((right_edge - key_w).max(after_label))
}

fn recent_picker_close_rect(card_x: f32, card_y: f32, card_w: f32, pad: f32) -> (f32, f32, f32, f32) {
    (card_x + card_w - pad - 28.0, card_y + 20.0, 28.0, 28.0)
}

fn use_compact_layout(body_w: f32, body_h: f32, col_w: f32) -> bool {
    body_w < 760.0 || body_h < 420.0 || col_w < 640.0
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
    /// When `true`, the forced surface is a focused Open Recent picker rather
    /// than the branded first-run landing.
    recent_picker: bool,
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
        self.recent_picker = false;
        self.hide_empty_auto = false;
    }

    /// Force a focused Open Recent chooser over the editor body.
    pub fn open_recent_picker(&mut self) {
        self.force_open = true;
        self.recent_picker = true;
        self.hide_empty_auto = false;
    }

    /// Dismiss the forced Welcome screen (e.g. a file was opened).
    pub fn dismiss(&mut self) {
        self.force_open = false;
        self.recent_picker = false;
    }

    /// Hide the automatic empty-buffer Welcome state for an explicit New File.
    pub fn dismiss_empty_auto(&mut self) {
        self.force_open = false;
        self.recent_picker = false;
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

        if self.recent_picker {
            self.draw_recent_picker(ctx, bx, by, bw, bh, recents, folders, clip);
            return;
        }

        // Center column. Generous max width so it breathes on wide windows.
        let col_w = 720.0_f32.min(bw - 48.0).max(280.0);
        let compact = use_compact_layout(bw, bh, col_w);
        let tight_height = bh < 340.0;
        let cx = bx + (bw - col_w) * 0.5;
        // Vertical rhythm: start a bit above the optical center.
        let mut y = by
            + if tight_height {
                12.0
            } else if compact {
                8.0
            } else {
                (bh * 0.16).max(40.0)
            };

        // ---- Brand: teal/indigo logo tile + wordmark ----
        let tile = if tight_height {
            40.0_f32
        } else if compact {
            52.0_f32
        } else {
            64.0_f32
        };
        let tx = cx;
        // Rounded brand tile with a focused glow + the Mighty mark. Match the
        // simplified icon treatment instead of stacking nested strokes.
        ctx.dl_shadow(tx, y + 8.0, tile, tile, 9.0, MuiColor::new(0.35, 0.95, 0.90, 0.18), 28.0);
        let tile_r = if compact { 8.0 } else { 10.0 };
        ctx.dl_grad_v(
            tx,
            y,
            tile,
            tile,
            tile_r,
            MuiColor::new(0.08, 0.09, 0.15, 1.0),
            MuiColor::new(0.03, 0.04, 0.09, 1.0),
        );
        ctx.dl_stroke(tx, y, tile, tile, tile_r, MuiColor::new(0.56, 0.96, 0.94, 0.92), 1.3);
        // Centered Mighty mark. The old side-rail version read like a generic
        // app tile at small sizes; this keeps the first impression focused.
        let mark_ink = MuiColor::new(0.61, 1.0, 0.96, 0.98);
        let mark = if tight_height {
            30.0
        } else if compact {
            40.0
        } else {
            48.0
        };
        let mark_pad = (tile - mark) * 0.5;
        ctx.dl_icon(tx + mark_pad, y + mark_pad, mark, mark, icons::LANG_M_FILL, mark_ink, 0.0, true);

        // Wordmark to the right of the tile.
        let word_x = tx + tile + if compact { 16.0 } else { 22.0 };
        ctx.text.queue_ui_styled(
            word_x,
            y + if tight_height {
                2.0
            } else if compact {
                5.0
            } else {
                8.0
            },
            "Mighty",
            theme::TEXT(),
            if tight_height {
                28.0
            } else if compact {
                36.0
            } else {
                40.0
            },
            crate::vello_ui::FontStyle::Bold,
            clip,
        );
        if !tight_height {
            let tagline = if compact {
                "The agent-first IDE"
            } else {
                "The agent-first language IDE"
            };
            ctx.text.queue_ui_sized(
                word_x + 2.0,
                y + if compact { 44.0 } else { 50.0 },
                tagline,
                theme::DIM(),
                if compact { 13.5 } else { 14.5 },
                clip,
            );
        }

        y += tile
            + if tight_height {
                18.0
            } else if compact {
                18.0
            } else {
                44.0
            };

        // ---- Compact editor column: keep the welcome actions usable, without
        // forcing recent columns into the status bar.
        if compact {
            let left_x = cx;
            let row_h = if tight_height { 28.0_f32 } else { 32.0_f32 };
            ctx.text.queue_ui_styled(
                left_x, y, "START", theme::TEXT_3(), 11.5, crate::vello_ui::FontStyle::Bold, clip,
            );
            let rows_top = y + 20.0;
            let bottom_limit = by + bh - 34.0;
            let max_actions = (((bottom_limit - rows_top) / row_h).floor().max(0.0) as usize)
                .min(QUICK_ACTIONS.len());
            for (i, qa) in QUICK_ACTIONS.iter().take(max_actions).enumerate() {
                let ry = rows_top + i as f32 * row_h;
                ctx.dl_round(left_x, ry + 3.0, 24.0, 24.0, 7.0, theme::BG_4());
                ctx.dl_icon(left_x + 4.0, ry + 7.0, 16.0, 16.0, qa.icon, theme::ACCENT_BRIGHT(), 1.7, false);
                ctx.text
                    .queue_ui_sized(left_x + 36.0, ry + 6.0, qa.label, theme::TEXT_1(), 13.5, clip);
                self.hits.push(Hit {
                    x: left_x,
                    y: ry,
                    w: col_w,
                    h: row_h,
                    action: qa.action,
                });
            }
            let mut ry = rows_top + max_actions as f32 * row_h + 18.0;
            if ry + 58.0 < bottom_limit {
                ctx.dl_rect(left_x, ry - 10.0, col_w, 1.0, theme::BORDER());
                ctx.text.queue_ui_styled(
                    left_x,
                    ry,
                    "RECENT FOLDERS",
                    theme::TEXT_3(),
                    11.5,
                    crate::vello_ui::FontStyle::Bold,
                    clip,
                );
                ry += 20.0;
                if folders.is_empty() {
                    ctx.text.queue_ui_sized(left_x, ry + 7.0, "No recent folders yet", theme::TEXT_3(), 13.0, clip);
                    ry += 34.0;
                } else {
                    let max_folder_rows = (((bottom_limit - ry) / 34.0).floor().max(0.0) as usize).min(2);
                    for (i, path) in folders.iter().take(max_folder_rows).enumerate() {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.to_string_lossy().into_owned());
                        let dir = path.to_string_lossy().into_owned();
                        ctx.dl_icon(left_x, ry + 8.0, 15.0, 15.0, icons::FOLDER, theme::ACCENT_BRIGHT(), 1.6, false);
                        ctx.text.queue_ui_sized(left_x + 25.0, ry + 3.0, &name, theme::TEXT_1(), 13.0, clip);
                        let dir_short = shorten_dir(&dir, col_w - 30.0);
                        ctx.text.queue_ui_sized(left_x + 25.0, ry + 19.0, &dir_short, theme::TEXT_3(), 10.5, clip);
                        self.hits.push(Hit {
                            x: left_x,
                            y: ry,
                            w: col_w,
                            h: 34.0,
                            action: ACTION_RECENT_FOLDER_BASE + i as i32,
                        });
                        self.recent_folders.push(path.clone());
                        ry += 34.0;
                    }
                }
            }
            if ry + 58.0 < bottom_limit {
                ctx.text.queue_ui_styled(
                    left_x,
                    ry + 8.0,
                    "RECENT FILES",
                    theme::TEXT_3(),
                    11.5,
                    crate::vello_ui::FontStyle::Bold,
                    clip,
                );
                ry += 28.0;
                if recents.is_empty() {
                    ctx.text.queue_ui_sized(left_x, ry + 7.0, "No recent files yet", theme::TEXT_3(), 13.0, clip);
                } else {
                    let max_file_rows = (((bottom_limit - ry) / 34.0).floor().max(0.0) as usize).min(2);
                    for (i, path) in recents.iter().take(max_file_rows).enumerate() {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.to_string_lossy().into_owned());
                        let dir = path
                            .parent()
                            .map(|d| d.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        ctx.dl_icon(left_x, ry + 8.0, 15.0, 15.0, file_icon(&name), theme::ACCENT_BRIGHT(), 1.6, false);
                        ctx.text.queue_ui_sized(left_x + 25.0, ry + 3.0, &name, theme::TEXT_1(), 13.0, clip);
                        if !dir.is_empty() {
                            let dir_short = shorten_dir(&dir, col_w - 30.0);
                            ctx.text.queue_ui_sized(left_x + 25.0, ry + 19.0, &dir_short, theme::TEXT_3(), 10.5, clip);
                        }
                        self.hits.push(Hit {
                            x: left_x,
                            y: ry,
                            w: col_w,
                            h: 34.0,
                            action: ACTION_RECENT_BASE + i as i32,
                        });
                        self.recents.push(path.clone());
                        ry += 34.0;
                    }
                }
            }
            return;
        }

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
                let label_x = left_x + 40.0;
                let (label_w, _) = ctx.text.measure_ui_sized(qa.label, 14.0);
                let (key_w, _) = ctx.text.measure_ui_sized(qa.key, 11.5);
                if let Some(key_x) = quick_action_key_x(left_x, left_w, label_x, label_w, key_w, 16.0, 4.0) {
                    ctx.text.queue_ui_sized(key_x, ry + 11.0, qa.key, theme::TEXT_3(), 11.5, clip);
                }
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
        let tips_y = rows_top + (QUICK_ACTIONS.len() as f32) * row_h + 18.0;
        let file_rows_bottom = tips_y - 18.0;
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
            let fit_rows = ((file_rows_bottom - files_rows_top) / row_h).floor().max(0.0) as usize;
            let preferred_rows = QUICK_ACTIONS.len().saturating_sub(folder_rows).max(2);
            let max_rows = preferred_rows.min(fit_rows);
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
        ctx.dl_rect(left_x, tips_y - 14.0, col_w, 1.0, theme::BORDER());
        ctx.text
            .queue_ui_sized(left_x, tips_y, "TIPS", theme::TEXT_3(), 11.5, clip);
        let tip_top = tips_y + 22.0;
        // Three columns keep all tips above the status bar at the default
        // window height while leaving each keybinding pill readable.
        const TIP_COLS: usize = 3;
        let tip_col_w = col_w / TIP_COLS as f32;
        let tip_row_h = 38.0;
        for (i, tip) in TIPS.iter().enumerate() {
            let col = (i % TIP_COLS) as f32;
            let row = (i / TIP_COLS) as f32;
            let txx = left_x + col * tip_col_w;
            let tyy = tip_top + row * tip_row_h;
            ctx.text
                .queue_ui_sized(txx, tyy, tip.what, theme::DIM(), 12.5, clip);
            // Stack the keybinding under the label so long chords never collide
            // with labels in the compact footer grid.
            let (key_w, _) = ctx.text.measure_ui_sized(tip.key, 9.5);
            let kw = key_w + 18.0;
            let px = txx;
            let py = tyy + 18.0;
            ctx.dl_round(px, py, kw, 14.0, 5.0, theme::BG_4());
            ctx.dl_stroke(px, py, kw, 14.0, 5.0, theme::BORDER(), 1.0);
            ctx.text
                .queue_ui_sized(px + 7.0, py + 0.5, tip.key, theme::TEXT_3(), 9.5, clip);
        }
    }

    fn draw_recent_picker(
        &mut self,
        ctx: &mut crate::MuiContext,
        bx: f32,
        by: f32,
        bw: f32,
        bh: f32,
        recents: &[PathBuf],
        folders: &[PathBuf],
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let card_w = 680.0_f32.min(bw - 48.0).max(320.0);
        let card_h = 452.0_f32.min(bh - 44.0).max(260.0);
        let card_x = bx + (bw - card_w) * 0.5;
        let card_y = by + ((bh - card_h) * 0.34).max(18.0);
        let radius = 10.0;

        ctx.dl_rect(bx, by, bw, bh, MuiColor::new(0.0, 0.0, 0.0, 0.34));
        ctx.dl_shadow(card_x, card_y + 16.0, card_w, card_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.70), 36.0);
        ctx.dl_round(card_x, card_y, card_w, card_h, radius, theme::ELEVATED());
        ctx.dl_stroke(card_x, card_y, card_w, card_h, radius, theme::BORDER_STRONG(), 1.0);

        let pad = 22.0;
        let title_y = card_y + 22.0;
        ctx.dl_icon(card_x + pad, title_y + 2.0, 22.0, 22.0, icons::FOLDER, theme::ACCENT_BRIGHT(), 1.8, false);
        ctx.text.queue_ui_styled(
            card_x + pad + 34.0,
            title_y,
            "Open Recent",
            theme::TEXT(),
            22.0,
            crate::vello_ui::FontStyle::Bold,
            clip,
        );
        ctx.text.queue_ui_sized(
            card_x + pad + 34.0,
            title_y + 28.0,
            "Choose a recent workspace or file",
            theme::TEXT_3(),
            12.5,
            clip,
        );
        let (close_x, close_y, close_w, close_h) = recent_picker_close_rect(card_x, card_y, card_w, pad);
        ctx.dl_round(close_x, close_y, close_w, close_h, 7.0, theme::BG_2());
        ctx.dl_stroke(close_x, close_y, close_w, close_h, 7.0, theme::BORDER_STRONG(), 1.0);
        ctx.dl_icon(close_x + 7.0, close_y + 7.0, 14.0, 14.0, icons::CLOSE, theme::TEXT_1(), 1.7, false);
        self.hits.push(Hit {
            x: close_x,
            y: close_y,
            w: close_w,
            h: close_h,
            action: ACTION_CLOSE,
        });
        ctx.dl_rect(card_x + pad, card_y + 72.0, card_w - pad * 2.0, 1.0, theme::BORDER());

        let compact = card_w < 560.0;
        if compact {
            let list_x = card_x + pad;
            let list_w = card_w - pad * 2.0;
            let mut y = card_y + 91.0;
            y = self.draw_recent_section(ctx, "RECENT FOLDERS", icons::FOLDER, list_x, y, list_w, folders, true, 4, clip);
            y += 14.0;
            let _ = self.draw_recent_section(ctx, "RECENT FILES", icons::FILE_TXT, list_x, y, list_w, recents, false, 5, clip);
        } else {
            let gutter = 28.0;
            let col_w = (card_w - pad * 2.0 - gutter) * 0.5;
            let left_x = card_x + pad;
            let right_x = left_x + col_w + gutter;
            let list_y = card_y + 91.0;
            let _ = self.draw_recent_section(ctx, "RECENT FOLDERS", icons::FOLDER, left_x, list_y, col_w, folders, true, 7, clip);
            let _ = self.draw_recent_section(ctx, "RECENT FILES", icons::FILE_TXT, right_x, list_y, col_w, recents, false, 7, clip);
        }

        if folders.is_empty() && recents.is_empty() {
            let empty_y = card_y + card_h - 92.0;
            self.draw_fallback_action(ctx, card_x + pad, empty_y, card_w - pad * 2.0, "Open File...", icons::EXPLORER, ACTION_OPEN_FILE, clip);
            self.draw_fallback_action(ctx, card_x + pad, empty_y + 42.0, card_w - pad * 2.0, "Open Folder...", icons::FOLDER, ACTION_OPEN_FOLDER, clip);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_recent_section(
        &mut self,
        ctx: &mut crate::MuiContext,
        title: &str,
        fallback_icon: &'static str,
        x: f32,
        mut y: f32,
        w: f32,
        paths: &[PathBuf],
        folders: bool,
        max_rows: usize,
        clip: Option<(u32, u32, u32, u32)>,
    ) -> f32 {
        ctx.text.queue_ui_styled(x, y, title, theme::TEXT_3(), 11.5, crate::vello_ui::FontStyle::Bold, clip);
        y += 24.0;
        if paths.is_empty() {
            let msg = if folders { "No recent folders" } else { "No recent files" };
            ctx.text.queue_ui_sized(x, y + 8.0, msg, theme::TEXT_3(), 12.5, clip);
            return y + 38.0;
        }

        for (i, path) in paths.iter().take(max_rows).enumerate() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let dir = if folders {
                path.to_string_lossy().into_owned()
            } else {
                path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
            };
            ctx.dl_round(x, y, w, 42.0, 7.0, theme::BG_2());
            ctx.dl_stroke(x, y, w, 42.0, 7.0, theme::BORDER(), 1.0);
            let icon = if folders { fallback_icon } else { file_icon(&name) };
            ctx.dl_icon(x + 12.0, y + 11.0, 18.0, 18.0, icon, theme::ACCENT_BRIGHT(), 1.7, false);
            let name_short = shorten_dir(&name, w - 48.0);
            ctx.text.queue_ui_sized(x + 40.0, y + 7.0, &name_short, theme::TEXT_1(), 13.0, clip);
            if !dir.is_empty() {
                let dir_short = shorten_dir(&dir, w - 48.0);
                ctx.text.queue_ui_sized(x + 40.0, y + 24.0, &dir_short, theme::TEXT_3(), 10.5, clip);
            }
            self.hits.push(Hit {
                x,
                y,
                w,
                h: 42.0,
                action: if folders {
                    self.recent_folders.push(path.clone());
                    ACTION_RECENT_FOLDER_BASE + i as i32
                } else {
                    self.recents.push(path.clone());
                    ACTION_RECENT_BASE + i as i32
                },
            });
            y += 48.0;
        }
        y
    }

    fn draw_fallback_action(
        &mut self,
        ctx: &mut crate::MuiContext,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        icon: &'static str,
        action: i32,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        ctx.dl_round(x, y, w, 34.0, 7.0, theme::BG_2());
        ctx.dl_stroke(x, y, w, 34.0, 7.0, theme::BORDER(), 1.0);
        ctx.dl_icon(x + 12.0, y + 9.0, 16.0, 16.0, icon, theme::ACCENT_BRIGHT(), 1.7, false);
        ctx.text.queue_ui_sized(x + 40.0, y + 8.0, label, theme::TEXT_1(), 13.0, clip);
        self.hits.push(Hit { x, y, w, h: 34.0, action });
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
        assert!(!w.recent_picker);
        w.dismiss();
        assert!(!w.force_open);
        assert!(!w.recent_picker);
    }

    #[test]
    fn recent_picker_is_forced_without_brand_landing_mode() {
        let mut w = WelcomeState::new();
        w.open_recent_picker();
        assert!(w.force_open);
        assert!(w.recent_picker);
        w.open();
        assert!(w.force_open);
        assert!(!w.recent_picker);
        w.open_recent_picker();
        w.dismiss_empty_auto();
        assert!(!w.force_open);
        assert!(!w.recent_picker);
    }

    #[test]
    fn recent_picker_close_action_is_top_right_hit() {
        let (x, y, w, h) = recent_picker_close_rect(100.0, 80.0, 680.0, 22.0);
        assert_eq!((x, y, w, h), (730.0, 100.0, 28.0, 28.0));
        let hit = Hit { x, y, w, h, action: ACTION_CLOSE };
        assert!(hit.contains(744.0, 114.0));
        assert_eq!(hit.action, ACTION_CLOSE);
    }

    #[test]
    fn file_icon_by_ext() {
        assert_eq!(file_icon("main.mty"), icons::FILE_MTY);
        assert_eq!(file_icon("Cargo.toml"), icons::FILE_TOML);
        assert_eq!(file_icon("README.md"), icons::FILE_MD);
        assert_eq!(file_icon("notes.txt"), icons::FILE_TXT);
    }

    #[test]
    fn new_file_is_the_primary_welcome_action() {
        assert_eq!(QUICK_ACTIONS.first().unwrap().action, ACTION_NEW_FILE);
        assert_eq!(QUICK_ACTIONS.first().unwrap().label, "New File at Location\u{2026}");
    }

    #[test]
    fn new_project_is_exposed_from_welcome_start_actions() {
        let row = QUICK_ACTIONS
            .iter()
            .position(|qa| qa.action == ACTION_NEW_PROJECT)
            .expect("New Mighty Project should be discoverable from Welcome");
        assert_eq!(row, 1);
        assert_eq!(QUICK_ACTIONS[row].label, "New Mighty Project\u{2026}");
    }

    #[test]
    fn quick_action_shortcut_is_right_aligned_when_it_fits() {
        let x = quick_action_key_x(100.0, 240.0, 140.0, 74.0, 52.0, 16.0, 4.0).unwrap();
        assert_eq!(x, 284.0);
    }

    #[test]
    fn quick_action_shortcut_moves_after_long_label_when_space_remains() {
        let x = quick_action_key_x(100.0, 240.0, 140.0, 145.0, 35.0, 16.0, 4.0).unwrap();
        assert_eq!(x, 301.0);
        assert!(x + 35.0 <= 336.0);
    }

    #[test]
    fn quick_action_shortcut_hides_instead_of_overlapping() {
        assert!(quick_action_key_x(100.0, 180.0, 140.0, 112.0, 60.0, 16.0, 4.0).is_none());
    }

    #[test]
    fn explorer_narrowed_welcome_uses_single_column_layout() {
        assert!(use_compact_layout(612.0, 600.0, 564.0));
        assert!(use_compact_layout(740.0, 600.0, 692.0));
        assert!(use_compact_layout(954.0, 320.0, 720.0));
        assert!(!use_compact_layout(954.0, 600.0, 720.0));
    }
}
