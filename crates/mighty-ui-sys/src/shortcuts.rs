//! Keyboard Shortcuts reference overlay + remapping (shim-owned, scalar ABI).
//!
//! Two features in one module:
//!
//! 1. **Reference overlay** — a searchable, scrollable list of EVERY command the
//!    IDE exposes with its current key binding. Opened by the palette command
//!    "Help: Keyboard Shortcuts" and a chord (Ctrl+Shift+/, routed through
//!    [`crate::abi::mui_chord`]). Styled exactly like the command palette
//!    (Vivid-Modern card, kbd pills). The list is assembled from the palette
//!    [`crate::palette::COMMANDS`] registry (the remappable subset) PLUS a static
//!    table of ladder-fixed chords (shown read-only / "(fixed)").
//!
//! 2. **Remapping** — for the cleanly-remappable subset (the commands whose chord
//!    is BOTH detected and fully dispatched inside [`crate::abi::mui_chord`], so a
//!    new chord can fire them with no `src/main.mty` ladder change — L37/L38):
//!    select a row → capture mode ("press the new shortcut") → record the chord
//!    (cp + mods) → save an override to `%APPDATA%/mighty-ide/keybindings.toml`.
//!    The chord router consults [`Overrides::resolve`] FIRST so a remapped command
//!    fires on its new chord. Conflicts (another command already on that chord)
//!    are detected and surfaced. "Reset to default" (one) + "Reset all" clear
//!    overrides. Overrides load at startup.
//!
//! The module is pure logic + a draw method; all I/O is best-effort and never
//! fails the IDE (mirrors [`crate::config`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ffi::MuiColor;
use crate::palette::{Command, COMMANDS};
use crate::theme;

/// Modifier bits, matching the IDE event `mods` encoding (mirrors the
/// `shift_held`/`ctrl_held`/`alt_held` helpers in `src/main.mty` and the bit
/// tests in [`crate::abi::mui_chord`]).
pub const MOD_SHIFT: i32 = 1;
pub const MOD_CTRL: i32 = 2;
pub const MOD_ALT: i32 = 4;

/// A normalized chord: a base codepoint (lowercased ASCII letter, or the literal
/// key for symbols/digits) plus a modifier mask. The "cp" is stored lowercased so
/// `Ctrl+B` and `Ctrl+b` are the same chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Chord {
    pub cp: i32,
    pub mods: i32,
}

impl Chord {
    pub fn new(cp: i32, mods: i32) -> Self {
        // Lowercase ASCII letters so case never affects matching.
        let cp = if (b'A' as i32..=b'Z' as i32).contains(&cp) {
            cp + 32
        } else {
            cp
        };
        // Keep only the three modifier bits we recognize.
        let mods = mods & (MOD_SHIFT | MOD_CTRL | MOD_ALT);
        Chord { cp, mods }
    }

    /// Render the chord as a human "Ctrl+Shift+B" style string.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.mods & MOD_CTRL != 0 {
            parts.push("Ctrl".to_string());
        }
        if self.mods & MOD_ALT != 0 {
            parts.push("Alt".to_string());
        }
        if self.mods & MOD_SHIFT != 0 {
            parts.push("Shift".to_string());
        }
        parts.push(key_name(self.cp));
        parts.join("+")
    }

    /// Serialize to the `keybindings.toml` value form: `mods:cp` (decimal). Stable
    /// + trivially parseable; the human label is recomputed on load.
    fn to_token(self) -> String {
        format!("{}:{}", self.mods, self.cp)
    }

    /// Parse a `mods:cp` token. Returns `None` on any malformed input.
    fn from_token(s: &str) -> Option<Chord> {
        let (m, c) = s.split_once(':')?;
        let mods = m.trim().parse::<i32>().ok()?;
        let cp = c.trim().parse::<i32>().ok()?;
        Some(Chord::new(cp, mods))
    }
}

/// Display name for a base codepoint in a chord label.
fn key_name(cp: i32) -> String {
    match cp {
        92 => "\\".to_string(),
        96 => "`".to_string(),
        44 => ",".to_string(),
        45 => "-".to_string(),
        47 => "/".to_string(),
        32 => "Space".to_string(),
        _ => {
            if let Some(c) = char::from_u32(cp as u32) {
                c.to_ascii_uppercase().to_string()
            } else {
                format!("U+{cp:X}")
            }
        }
    }
}

/// One row in the shortcuts reference. `cmd_id` is the palette command id for
/// registry-backed rows, or a synthetic negative id for ladder-fixed entries
/// (so they never collide with a remapping override).
#[derive(Debug, Clone)]
pub struct ShortcutRow {
    pub cmd_id: i32,
    pub name: String,
    /// The CURRENTLY-bound keys (default, or the override label when remapped).
    pub keys: String,
    /// The DEFAULT keys (for "is this remapped?" + reset display).
    #[allow(dead_code)]
    pub default_keys: String,
    /// `true` when this command can be remapped through the chord router.
    pub remappable: bool,
}

/// The set of palette command ids that can be rebound to an Alt+letter chord.
/// The router resolves to a palette id and Mighty executes it through the shared
/// command dispatcher, so every palette command is eligible.
pub fn is_remappable(cmd_id: u32) -> bool {
    crate::palette::COMMANDS.iter().any(|cmd| cmd.id == cmd_id)
}

/// The DEFAULT chord for a remappable command (what the router fires on out of
/// the box). `None` for commands with no single default chord. Kept in sync with
/// the hard-coded arms in [`crate::abi::mui_chord`].
pub fn default_chord(cmd_id: u32) -> Option<Chord> {
    use crate::palette::*;
    let c = |cp: i32, mods: i32| Some(Chord::new(cp, mods));
    match cmd_id {
        x if x == CMD_ZEN_MODE => c('z' as i32, MOD_ALT),
        x if x == CMD_AGENTS => c('g' as i32, MOD_ALT),
        x if x == CMD_GIT_TOGGLE_BLAME => c('b' as i32, MOD_ALT),
        x if x == CMD_RUN_IN_BROWSER => c('w' as i32, MOD_ALT),
        x if x == CMD_SPLIT_RIGHT => c(92, MOD_CTRL),
        x if x == CMD_MARKDOWN_PREVIEW => c('v' as i32, MOD_CTRL | MOD_SHIFT),
        x if x == CMD_OPEN_FOLDER => c('o' as i32, MOD_CTRL | MOD_SHIFT),
        x if x == CMD_OPEN_FILE => c('o' as i32, MOD_CTRL),
        x if x == CMD_SAVE => c('s' as i32, MOD_CTRL),
        x if x == CMD_FIND => c('f' as i32, MOD_CTRL),
        x if x == CMD_GOTO_LINE => c('g' as i32, MOD_CTRL),
        x if x == CMD_HOVER => c('k' as i32, MOD_CTRL),
        x if x == CMD_TOGGLE_TERMINAL => c('`' as i32, MOD_CTRL),
        x if x == CMD_TOGGLE_SIDEBAR => c('b' as i32, MOD_CTRL),
        x if x == CMD_CLOSE_TAB => c('w' as i32, MOD_CTRL),
        x if x == CMD_FORMAT_DOCUMENT => c('i' as i32, MOD_CTRL | MOD_SHIFT),
        x if x == CMD_UNDO => c('z' as i32, MOD_CTRL),
        x if x == CMD_REDO => c('y' as i32, MOD_CTRL),
        x if x == CMD_AUTOCOMPLETE => c(' ' as i32, MOD_CTRL),
        x if x == CMD_JUMP_BACK => c('-' as i32, MOD_CTRL),
        x if x == CMD_RUN_FILE => c('r' as i32, MOD_CTRL | MOD_SHIFT),
        x if x == CMD_SETTINGS => c(',' as i32, MOD_CTRL),
        x if x == CMD_RUN_TESTS => c('t' as i32, MOD_CTRL | MOD_SHIFT),
        x if x == CMD_KEYBOARD_SHORTCUTS => c('/' as i32, MOD_CTRL | MOD_SHIFT),
        x if x == CMD_FOLD_TOGGLE => c('[' as i32, MOD_CTRL | MOD_SHIFT),
        _ => None,
    }
}

/// Ladder-fixed chords handled directly in `src/main.mty` (NOT through the
/// router) — shown read-only with a "(fixed)" affordance. Synthetic negative ids.
const FIXED: &[(&str, &str)] = &[
    ("Redo Alternate", "Ctrl+Shift+Z"),
    ("Toggle Line Comment", "Ctrl+/"),
    ("Duplicate Line / Selection", "Ctrl+Shift+D"),
    ("Add Caret at Next Match", "Ctrl+D"),
    ("Move Line Up / Down", "Alt+Up / Alt+Down"),
    ("Find & Replace", "Ctrl+H"),
    ("Command Palette", "Ctrl+Shift+P"),
    ("Quick Open", "Ctrl+P"),
    ("Signature Help", "Ctrl+Shift+Space"),
    ("Rename Symbol", "F2"),
    ("Code Actions / Quick Fix", "Ctrl+."),
    ("AI Copilot Panel", "Ctrl+Shift+A"),
    ("Inline Ask", "Ctrl+I"),
    ("Source Control Panel", "Ctrl+Shift+G"),
    ("Project Search Panel", "Ctrl+Shift+F"),
    ("Debugger: Start / Continue", "F5"),
    ("Debugger: Step Over", "F10"),
    ("Force Ghost Completion", "Alt+\\"),
    ("Help: Keyboard Shortcuts", "Ctrl+Shift+/"),
];

/// Persisted remapping overrides: command id → new chord. The chord router
/// resolves an incoming chord through [`Overrides::resolve`] (override map wins
/// over the hard-coded defaults) so a remapped command fires on its new chord.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    map: BTreeMap<u32, Chord>,
}

impl Overrides {
    pub fn new() -> Self {
        Overrides::default()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// The override chord for `cmd_id`, if remapped.
    pub fn get(&self, cmd_id: u32) -> Option<Chord> {
        self.map.get(&cmd_id).copied()
    }

    /// Set (or replace) the override for `cmd_id`. Returns the command id that was
    /// previously bound to `chord` (default OR override) if it conflicts with a
    /// DIFFERENT command — the caller surfaces the conflict.
    pub fn set(&mut self, cmd_id: u32, chord: Chord) -> Option<u32> {
        let conflict = self.command_for_chord(chord).filter(|&c| c != cmd_id);
        self.map.insert(cmd_id, chord);
        conflict
    }

    /// Clear the override for one command (back to its default chord).
    pub fn reset(&mut self, cmd_id: u32) -> bool {
        self.map.remove(&cmd_id).is_some()
    }

    /// Clear every override.
    pub fn reset_all(&mut self) {
        self.map.clear();
    }

    /// Which command (if any) currently owns `chord`, considering overrides first
    /// then the hard-coded defaults. An override that moves a command OFF its
    /// default frees that default for another command.
    pub fn command_for_chord(&self, chord: Chord) -> Option<u32> {
        // 1. An explicit override binding to this chord wins.
        if let Some((&id, _)) = self.map.iter().find(|(_, &c)| c == chord) {
            return Some(id);
        }
        // 2. Else a default-chord command, UNLESS that command has been remapped
        //    away (its default is then free).
        for cmd in COMMANDS {
            if self.map.contains_key(&cmd.id) {
                continue; // remapped away; its default no longer fires.
            }
            if default_chord(cmd.id) == Some(chord) {
                return Some(cmd.id);
            }
        }
        None
    }

    /// Resolve an incoming `(cp, mods)` to a command id, if the chord is bound to
    /// a router-dispatchable command (override OR default). The router calls this
    /// FIRST so remapped chords fire the right command.
    pub fn resolve(&self, cp: i32, mods: i32) -> Option<u32> {
        let chord = Chord::new(cp, mods);
        self.command_for_chord(chord)
            .filter(|&id| is_remappable(id))
    }

    /// Return true when `chord` used to be a command's default chord, but that
    /// command now has an override. The event loop consumes these freed defaults
    /// so a remapped command does not continue firing from its old hard-coded
    /// branch.
    pub fn is_freed_default(&self, cp: i32, mods: i32) -> bool {
        let chord = Chord::new(cp, mods);
        COMMANDS.iter().any(|cmd| {
            self.map.contains_key(&cmd.id)
                && is_remappable(cmd.id)
                && default_chord(cmd.id) == Some(chord)
        })
    }

    /// Serialize to the `keybindings.toml` blob. One `[overrides]` table with
    /// `cmd_<id> = "mods:cp"` lines. Human-readable comment header.
    pub fn render(&self) -> String {
        let mut s = String::from(
            "# Mighty IDE keybinding overrides.\n\
             # Each line remaps a command to a new chord; delete a line to restore\n\
             # its default. Values are `modifiers:codepoint` (shift=1 ctrl=2 alt=4).\n\
             [overrides]\n",
        );
        for (id, chord) in &self.map {
            s.push_str(&format!("cmd_{id} = \"{}\"\n", chord.to_token()));
        }
        s
    }

    /// Parse a `keybindings.toml` blob. Tolerant: unknown/malformed lines are
    /// skipped (best-effort, never fails the IDE).
    pub fn parse(text: &str) -> Overrides {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let Some(id_str) = key.strip_prefix("cmd_") else {
                continue;
            };
            let Ok(id) = id_str.trim().parse::<u32>() else {
                continue;
            };
            let v = v.trim().trim_matches('"');
            if let Some(chord) = Chord::from_token(v) {
                map.insert(id, chord);
            }
        }
        Overrides { map }
    }
}

/// Path to the keybinding overrides file (`%APPDATA%/mighty-ide/keybindings.toml`
/// on Windows; XDG/HOME `.config` otherwise). Mirrors [`crate::config`].
pub fn keybindings_path() -> Option<PathBuf> {
    let dir = if let Some(appdata) = std::env::var_os("APPDATA") {
        PathBuf::from(appdata).join("mighty-ide")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("mighty-ide")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config").join("mighty-ide")
    } else {
        return None;
    };
    Some(dir.join("keybindings.toml"))
}

/// Load overrides from disk, or empty when unset/unreadable. Best-effort.
pub fn load_overrides() -> Overrides {
    let Some(path) = keybindings_path() else {
        return Overrides::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Overrides::parse(&text),
        Err(_) => Overrides::new(),
    }
}

/// Persist `overrides` to disk (creating the directory). Best-effort: returns
/// `false` (and logs) on any I/O error.
pub fn save_overrides(overrides: &Overrides) -> bool {
    let Some(path) = keybindings_path() else {
        eprintln!("shortcuts: no config directory; keybindings not persisted");
        return false;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("shortcuts: create_dir_all {}: {e}", parent.display());
            return false;
        }
    }
    match std::fs::write(&path, overrides.render()) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("shortcuts: write {}: {e}", path.display());
            false
        }
    }
}

/// Build the full shortcuts reference list: every palette command (with its
/// current binding, applying any override) followed by the ladder-fixed entries
/// (read-only). Pure + unit-tested.
pub fn build_rows(overrides: &Overrides) -> Vec<ShortcutRow> {
    let mut rows: Vec<ShortcutRow> = Vec::new();
    for cmd in COMMANDS {
        let remappable = is_remappable(cmd.id);
        let default_keys = cmd.keybinding.to_string();
        let keys = if let Some(ov) = overrides.get(cmd.id) {
            ov.label()
        } else {
            default_keys.clone()
        };
        rows.push(ShortcutRow {
            cmd_id: cmd.id as i32,
            name: cmd.label.to_string(),
            keys,
            default_keys,
            remappable,
        });
    }
    // Ladder-fixed read-only entries get synthetic negative ids.
    for (i, (name, keys)) in FIXED.iter().enumerate() {
        rows.push(ShortcutRow {
            cmd_id: -(i as i32) - 1,
            name: name.to_string(),
            keys: keys.to_string(),
            default_keys: keys.to_string(),
            remappable: false,
        });
    }
    rows
}

/// Filter rows by a case-insensitive substring match against the command name OR
/// its key binding. Empty query returns all rows (in build order). Pure.
pub fn filter_rows(rows: &[ShortcutRow], query: &str) -> Vec<ShortcutRow> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|r| {
            r.name.to_ascii_lowercase().contains(&q) || r.keys.to_ascii_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

/// Max rows drawn at once (the visible window).
const VISIBLE: usize = 9;

/// Shim-owned shortcuts overlay state: the typed filter, the (filtered) rows, the
/// selection, the override map, and capture mode.
pub struct ShortcutsEngine {
    active: bool,
    query: String,
    rows: Vec<ShortcutRow>,
    sel: usize,
    overrides: Overrides,
    /// `true` while waiting for the user to press the new chord for `capture_id`.
    capturing: bool,
    capture_id: u32,
    /// A transient status line (conflict warning / saved / reset).
    status: String,
}

impl Default for ShortcutsEngine {
    /// A bare engine with NO overrides loaded. Cheap (no disk I/O) so the
    /// `std::mem::take` borrow-splitting in the draw ABI is free; production
    /// construction goes through [`ShortcutsEngine::new`] which loads from disk.
    fn default() -> Self {
        ShortcutsEngine {
            active: false,
            query: String::new(),
            rows: Vec::new(),
            sel: 0,
            overrides: Overrides::new(),
            capturing: false,
            capture_id: 0,
            status: String::new(),
        }
    }
}

impl ShortcutsEngine {
    /// Build the engine and load any persisted overrides from disk.
    pub fn new() -> Self {
        ShortcutsEngine {
            overrides: load_overrides(),
            ..ShortcutsEngine::default()
        }
    }

    /// Borrow the override map (so the chord router can resolve remaps).
    pub fn overrides(&self) -> &Overrides {
        &self.overrides
    }

    #[cfg(test)]
    pub(crate) fn overrides_mut(&mut self) -> &mut Overrides {
        &mut self.overrides
    }

    /// Open the overlay: clear the filter, rebuild rows, select the first.
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.sel = 0;
        self.capturing = false;
        self.status.clear();
        self.refilter();
    }

    fn all_rows(&self) -> Vec<ShortcutRow> {
        build_rows(&self.overrides)
    }

    fn refilter(&mut self) {
        let all = self.all_rows();
        self.rows = filter_rows(&all, &self.query);
        if self.sel >= self.rows.len() {
            self.sel = self.rows.len().saturating_sub(1);
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing
    }

    pub fn count(&self) -> usize {
        self.rows.len()
    }

    #[allow(dead_code)]
    pub fn selection(&self) -> usize {
        self.sel
    }

    #[allow(dead_code)]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[allow(dead_code)]
    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn push_char(&mut self, ch: char) {
        if self.capturing {
            return; // typing is captured as a chord, not a filter, while capturing.
        }
        self.query.push(ch);
        self.sel = 0;
        self.refilter();
    }

    pub fn backspace(&mut self) {
        if self.capturing {
            return;
        }
        self.query.pop();
        self.sel = 0;
        self.refilter();
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.rows.len();
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

    pub fn select(&mut self, idx: usize) -> bool {
        if idx < self.rows.len() {
            self.sel = idx;
            true
        } else {
            false
        }
    }

    /// The selected row's command id (`< 0` for fixed / no selection).
    pub fn selected_id(&self) -> i32 {
        self.rows.get(self.sel).map(|r| r.cmd_id).unwrap_or(-1)
    }

    pub fn selected_remappable(&self) -> bool {
        self.rows.get(self.sel).map(|r| r.remappable).unwrap_or(false)
    }

    /// The row name + keys for the selection (for the ABI string reads).
    pub fn row_name(&self, idx: usize) -> &str {
        self.rows.get(idx).map(|r| r.name.as_str()).unwrap_or("")
    }

    pub fn row_keys(&self, idx: usize) -> &str {
        self.rows.get(idx).map(|r| r.keys.as_str()).unwrap_or("")
    }

    pub fn row_remappable(&self, idx: usize) -> bool {
        self.rows.get(idx).map(|r| r.remappable).unwrap_or(false)
    }

    /// Enter capture mode for the selected row (only if it's remappable).
    /// Returns `true` if capture started.
    pub fn begin_capture(&mut self) -> bool {
        if !self.selected_remappable() {
            self.status = "This shortcut is fixed and cannot be remapped".to_string();
            return false;
        }
        let id = self.selected_id();
        if id < 0 {
            return false;
        }
        self.capturing = true;
        self.capture_id = id as u32;
        self.status = "Press the new shortcut: Alt + a letter (Esc to cancel)".to_string();
        true
    }

    /// Record a captured chord as the override for the command in capture mode.
    /// Detects + warns on conflicts; persists on success. Returns `1` saved,
    /// `2` saved-with-conflict-warning, `0` ignored (not capturing / bad chord).
    pub fn capture_chord(&mut self, cp: i32, mods: i32) -> i32 {
        if !self.capturing {
            return 0;
        }
        let m = mods & (MOD_SHIFT | MOD_CTRL | MOD_ALT);
        // Remap TARGETS must be an `Alt+<letter>` chord (no Ctrl). This is the one
        // modifier class the Mighty key ladder forwards wholesale to the chord
        // router (`is_router_chord`: `alt && !ctrl`), so a remapped command's new
        // chord is guaranteed to reach the router without growing the ladder
        // (L37/L38). The capture UI states this constraint.
        let is_letter = (b'a' as i32..=b'z' as i32).contains(&Chord::new(cp, 0).cp);
        if m != MOD_ALT || !is_letter {
            self.status = "Use Alt + a letter for the new shortcut".to_string();
            return 0;
        }
        let chord = Chord::new(cp, m);
        let id = self.capture_id;
        let conflict = self.overrides.set(id, chord);
        let _ = save_overrides(&self.overrides);
        self.capturing = false;
        if let Some(other) = conflict {
            let other_name = command_name(other);
            self.status = format!(
                "Bound to {} \u{2014} also used by \"{}\"",
                chord.label(),
                other_name
            );
            self.refilter();
            return 2;
        }
        self.status = format!("Bound to {}", chord.label());
        self.refilter();
        1
    }

    /// Reset the selected row to its default chord (clears the override).
    pub fn reset_selected(&mut self) -> bool {
        let id = self.selected_id();
        if id < 0 {
            return false;
        }
        let did = self.overrides.reset(id as u32);
        if did {
            let _ = save_overrides(&self.overrides);
            self.status = "Reset to default".to_string();
            self.refilter();
        }
        did
    }

    /// Clear every override and persist.
    pub fn reset_all(&mut self) {
        self.overrides.reset_all();
        let _ = save_overrides(&self.overrides);
        self.status = "All shortcuts reset to defaults".to_string();
        self.refilter();
    }

    /// Cancel capture mode (keeps the overlay open).
    pub fn cancel_capture(&mut self) {
        if self.capturing {
            self.capturing = false;
            self.status = "Capture cancelled".to_string();
        }
    }

    /// Close the overlay and clear transient state (keeps overrides in memory).
    pub fn cancel(&mut self) {
        self.active = false;
        self.capturing = false;
        self.query.clear();
        self.rows.clear();
        self.sel = 0;
        self.status.clear();
    }

    /// First visible row index so the selection stays within the window.
    fn scroll_top(&self) -> usize {
        if self.rows.len() <= VISIBLE {
            return 0;
        }
        if self.sel < VISIBLE {
            0
        } else {
            (self.sel + 1).saturating_sub(VISIBLE)
        }
    }

    fn geometry(&self, width: u32, height: u32) -> (f32, f32, f32, f32, f32, f32, usize, usize) {
        let w = width as f32;
        let h = height as f32;
        let top = self.scroll_top();
        let shown = self.rows.len().saturating_sub(top).min(VISIBLE);
        let box_w = 640.0_f32.min(w - 80.0);
        let search_h = 56.0;
        let cat_h = 26.0;
        let row_h = 44.0;
        let foot_h = 38.0;
        let box_h = search_h + cat_h + shown as f32 * row_h + 10.0 + foot_h;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = 80.0_f32.min((h - box_h).max(0.0));
        let list_top = box_y + search_h + cat_h;
        (box_x, box_y, box_w, list_top, row_h, box_h, top, shown)
    }

    /// Select the shortcut row under a click. Returns the selected filtered row
    /// index, or -1 when the click missed the visible row list.
    pub fn click_row(&mut self, x: f32, y: f32, width: u32, height: u32) -> i32 {
        if !self.active || self.rows.is_empty() {
            return -1;
        }
        let (box_x, _box_y, box_w, list_top, row_h, _box_h, top, shown) = self.geometry(width, height);
        if x < box_x || x > box_x + box_w || y < list_top {
            return -1;
        }
        let vis = ((y - list_top) / row_h).floor() as usize;
        if vis >= shown {
            return -1;
        }
        let idx = top + vis;
        if self.select(idx) {
            idx as i32
        } else {
            -1
        }
    }

    /// Draw the shortcuts overlay (Vivid-Modern card, kbd pills, remap affordance).
    /// No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.active {
            return;
        }
        use crate::icons;
        let w = width as f32;
        let h = height as f32;
        let chrome = theme::CHROME_FONT_SIZE;
        let clip = ctx.clip;

        let search_h = 56.0;
        let cat_h = 26.0;
        let row_h = 44.0;
        let foot_h = 38.0;
        let (box_x, box_y, box_w, _list_top, _row_h, box_h, top, shown) = self.geometry(width, height);
        let radius = 12.0_f32;

        // Scrim + indigo wash.
        ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, 0.55));
        ctx.dl_grad_v(0.0, 0.0, w, h * 0.5, 0.0, theme::accent_a(0.05), theme::accent_a(0.0));
        ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.85), 40.0);
        ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW(), 40.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        // ---- search / filter field ----
        ctx.dl_rect(box_x + 1.0, box_y + search_h - 1.0, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_icon(box_x + 18.0, box_y + (search_h - 20.0) * 0.5, 20.0, 20.0, icons::SEARCH, theme::DIM(), 1.7, false);
        let q_text_x = box_x + 50.0;
        let qy = box_y + (search_h - 16.0) * 0.5 - 1.0;
        let (q_str, q_color): (&str, _) = if self.query.is_empty() {
            ("Search keyboard shortcuts\u{2026}", theme::TEXT_3())
        } else {
            (self.query.as_str(), theme::TEXT())
        };
        ctx.text.queue_ui_sized(q_text_x, qy, q_str, q_color, 16.0, clip);
        let qadv = 16.0 * 0.52;
        let caret_x = q_text_x + self.query.chars().count() as f32 * qadv + 1.0;
        ctx.dl_round(caret_x, box_y + (search_h - 18.0) * 0.5, 2.0, 18.0, 1.0, theme::ACCENT_BRIGHT());

        // ---- category / title ----
        let cat_y = box_y + search_h + 8.0;
        ctx.text.queue_ui_sized(box_x + 18.0, cat_y, "KEYBOARD SHORTCUTS", theme::TEXT_3(), chrome - 2.5, clip);

        // ---- rows ----
        let list_top = box_y + search_h + cat_h;
        for vis in 0..shown {
            let idx = top + vis;
            let row = &self.rows[idx];
            let ry = list_top + vis as f32 * row_h;
            let selected = idx == self.sel;
            if selected {
                ctx.dl_grad_h(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::accent_a(0.22), 0.9);
                ctx.dl_stroke(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_LINE(), 1.0);
                ctx.dl_shadow(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_GLOW(), 16.0);
            }

            // Title (left).
            let txt_x = box_x + 22.0;
            ctx.text.queue_ui_sized(txt_x, ry + (row_h - 14.0) * 0.5, &row.name, theme::TEXT(), 13.5, clip);

            // Right-aligned kbd pills.
            let right_edge = box_x + box_w - 20.0;
            let parts: Vec<&str> = if row.keys.is_empty() {
                Vec::new()
            } else {
                row.keys.split(['+', ' ']).filter(|s| !s.is_empty() && *s != "/").collect()
            };
            let pill_pad = 7.0;
            let gap = 4.0;
            let kadv = 11.0 * 0.55;
            let widths: Vec<f32> = parts
                .iter()
                .map(|p| (p.chars().count() as f32 * kadv + 2.0 * pill_pad).max(22.0))
                .collect();
            let total_w: f32 = widths.iter().sum::<f32>() + gap * (parts.len().saturating_sub(1)) as f32;
            let px = right_edge - total_w;
            let pill_h = 21.0;
            let py = ry + (row_h - pill_h) * 0.5;
            let mut draw_x = px;
            for (k, part) in parts.iter().enumerate() {
                let pw = widths[k];
                let (pbg, pborder, pfg) = if selected {
                    (theme::accent_a(0.10), theme::ACCENT_LINE(), theme::ACCENT_BRIGHT())
                } else {
                    (theme::BG_2(), theme::BORDER_STRONG(), theme::TEXT_1())
                };
                ctx.dl_round(draw_x, py, pw, pill_h, 5.0, pbg);
                ctx.dl_stroke(draw_x, py, pw, pill_h, 5.0, pborder, 1.0);
                let lbl_w = part.chars().count() as f32 * kadv;
                ctx.text.queue_ui_sized(draw_x + (pw - lbl_w) * 0.5, py + 4.5, part, pfg, 11.0, clip);
                draw_x += pw + gap;
            }

            // Remappable affordance: a small "remap" / "fixed" label left of pills.
            let tag = if !row.remappable {
                "fixed"
            } else if selected {
                "Enter to remap"
            } else {
                ""
            };
            if !tag.is_empty() {
                let tag_w = tag.chars().count() as f32 * 5.6;
                let tag_x = px - 12.0 - tag_w;
                let tcol = if row.remappable { theme::ACCENT_BRIGHT() } else { theme::TEXT_3() };
                ctx.text.queue_ui_sized(tag_x, ry + (row_h - 10.0) * 0.5, tag, tcol, 10.0, clip);
            }
        }

        // ---- footer / status line ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_round(box_x + 1.0, foot_y, box_w - 2.0, foot_h - 1.0, 0.0, theme::BG_2());
        let fty = foot_y + (foot_h - chrome + 1.0) * 0.5 - 1.0;
        if self.capturing {
            ctx.text.queue_ui_sized(box_x + 18.0, fty, &self.status, theme::ACCENT_BRIGHT(), 11.5, clip);
        } else if !self.status.is_empty() {
            ctx.text.queue_ui_sized(box_x + 18.0, fty, &self.status, theme::TEXT_1(), 11.5, clip);
        } else {
            let hint = "\u{2191}\u{2193} navigate   Enter remap   Ctrl+R reset   Ctrl+Shift+R reset all   esc close";
            ctx.text.queue_ui_sized(box_x + 18.0, fty, hint, theme::TEXT_3(), 11.0, clip);
        }
        let tag = "Mighty Shortcuts";
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - tag.chars().count() as f32 * 6.3, fty, tag, theme::ACCENT_BRIGHT(), 11.0, clip);
    }
}

/// Look up a command's display name by id (for conflict messages).
fn command_name(cmd_id: u32) -> String {
    COMMANDS
        .iter()
        .find(|c| c.id == cmd_id)
        .map(|c: &Command| c.label.to_string())
        .unwrap_or_else(|| format!("command {cmd_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::*;

    #[test]
    fn chord_normalizes_case_and_mods() {
        let a = Chord::new('B' as i32, MOD_ALT);
        let b = Chord::new('b' as i32, MOD_ALT);
        assert_eq!(a, b);
        // Extra (unknown) bits are stripped.
        let c = Chord::new('b' as i32, MOD_ALT | 8 | 16);
        assert_eq!(c, b);
    }

    #[test]
    fn chord_label_orders_modifiers() {
        let c = Chord::new('v' as i32, MOD_CTRL | MOD_SHIFT);
        assert_eq!(c.label(), "Ctrl+Shift+V");
        assert_eq!(Chord::new(92, MOD_CTRL).label(), "Ctrl+\\");
        assert_eq!(Chord::new('z' as i32, MOD_ALT).label(), "Alt+Z");
    }

    #[test]
    fn chord_token_round_trips() {
        for c in [
            Chord::new('z' as i32, MOD_ALT),
            Chord::new(92, MOD_CTRL),
            Chord::new('v' as i32, MOD_CTRL | MOD_SHIFT),
        ] {
            let tok = c.to_token();
            assert_eq!(Chord::from_token(&tok), Some(c), "round-trip {tok}");
        }
        assert_eq!(Chord::from_token("garbage"), None);
        assert_eq!(Chord::from_token("x:y"), None);
    }

    #[test]
    fn build_rows_lists_commands_then_fixed() {
        let ov = Overrides::new();
        let rows = build_rows(&ov);
        // First COMMANDS.len() rows are the palette commands, in order.
        assert!(rows.len() >= COMMANDS.len() + FIXED.len());
        assert_eq!(rows[0].name, COMMANDS[0].label);
        assert_eq!(rows[0].cmd_id, COMMANDS[0].id as i32);
        // Every palette command can be rebound to an Alt+letter chord.
        assert!(rows[..COMMANDS.len()].iter().all(|r| r.remappable));
        let save = rows.iter().find(|r| r.cmd_id == CMD_SAVE as i32).unwrap();
        assert!(save.remappable);
        // Fixed rows are read-only with negative ids.
        let fixed = rows.iter().filter(|r| r.cmd_id < 0).count();
        assert_eq!(fixed, FIXED.len());
        assert!(rows.iter().filter(|r| r.cmd_id < 0).all(|r| !r.remappable));
    }

    #[test]
    fn all_palette_commands_are_alt_remappable() {
        for cmd in COMMANDS {
            assert!(is_remappable(cmd.id), "{} should be remappable", cmd.label);
        }
    }

    #[test]
    fn direct_default_chords_resolve_for_common_commands() {
        let ov = Overrides::new();
        assert_eq!(ov.resolve('s' as i32, MOD_CTRL), Some(CMD_SAVE));
        assert_eq!(ov.resolve('f' as i32, MOD_CTRL), Some(CMD_FIND));
        assert_eq!(ov.resolve('z' as i32, MOD_CTRL), Some(CMD_UNDO));
        assert_eq!(ov.resolve('y' as i32, MOD_CTRL), Some(CMD_REDO));
        assert_eq!(ov.resolve('r' as i32, MOD_CTRL | MOD_SHIFT), Some(CMD_RUN_FILE));
        assert_eq!(
            ov.resolve('t' as i32, MOD_CTRL | MOD_SHIFT),
            Some(CMD_RUN_TESTS)
        );
    }

    #[test]
    fn filter_matches_name_or_keys() {
        let ov = Overrides::new();
        let rows = build_rows(&ov);
        // By name.
        let by_name = filter_rows(&rows, "zen");
        assert!(by_name.iter().any(|r| r.cmd_id == CMD_ZEN_MODE as i32));
        // By key.
        let by_key = filter_rows(&rows, "alt+z");
        assert!(by_key.iter().any(|r| r.cmd_id == CMD_ZEN_MODE as i32));
        // Empty -> all.
        assert_eq!(filter_rows(&rows, "").len(), rows.len());
        // No match.
        assert!(filter_rows(&rows, "zzqqxx").is_empty());
    }

    #[test]
    fn override_set_and_resolve() {
        let mut ov = Overrides::new();
        // Default: Alt+Z resolves to Zen.
        assert_eq!(ov.resolve('z' as i32, MOD_ALT), Some(CMD_ZEN_MODE));
        // Remap Zen to Alt+K.
        let conflict = ov.set(CMD_ZEN_MODE, Chord::new('k' as i32, MOD_ALT));
        assert_eq!(conflict, None);
        // New chord resolves; old default no longer fires.
        assert_eq!(ov.resolve('k' as i32, MOD_ALT), Some(CMD_ZEN_MODE));
        assert_eq!(ov.resolve('z' as i32, MOD_ALT), None);
        assert!(ov.is_freed_default('z' as i32, MOD_ALT));
        assert!(!ov.is_freed_default('k' as i32, MOD_ALT));
    }

    #[test]
    fn override_conflict_detection() {
        let mut ov = Overrides::new();
        // Bind Zen onto Alt+B which defaults to Toggle Blame -> conflict reported.
        let conflict = ov.set(CMD_ZEN_MODE, Chord::new('b' as i32, MOD_ALT));
        assert_eq!(conflict, Some(CMD_GIT_TOGGLE_BLAME));
    }

    #[test]
    fn override_reset_one_and_all() {
        let mut ov = Overrides::new();
        ov.set(CMD_ZEN_MODE, Chord::new('k' as i32, MOD_ALT));
        ov.set(CMD_AGENTS, Chord::new('j' as i32, MOD_ALT));
        assert_eq!(ov.len(), 2);
        assert!(ov.reset(CMD_ZEN_MODE));
        assert!(!ov.reset(CMD_ZEN_MODE)); // already cleared
        assert_eq!(ov.len(), 1);
        // Zen back to default.
        assert_eq!(ov.resolve('z' as i32, MOD_ALT), Some(CMD_ZEN_MODE));
        ov.reset_all();
        assert!(ov.is_empty());
        assert_eq!(ov.resolve('j' as i32, MOD_ALT), None);
    }

    #[test]
    fn override_render_parse_round_trip() {
        let mut ov = Overrides::new();
        ov.set(CMD_ZEN_MODE, Chord::new('k' as i32, MOD_ALT));
        ov.set(CMD_OPEN_FOLDER, Chord::new('o' as i32, MOD_CTRL | MOD_ALT));
        let blob = ov.render();
        let back = Overrides::parse(&blob);
        assert_eq!(back.get(CMD_ZEN_MODE), ov.get(CMD_ZEN_MODE));
        assert_eq!(back.get(CMD_OPEN_FOLDER), ov.get(CMD_OPEN_FOLDER));
        // Tolerant of junk.
        let junk = Overrides::parse("# c\n[overrides]\nbad line\ncmd_x = \"1:2\"\ncmd_24 = \"4:107\"\n");
        assert_eq!(junk.get(CMD_ZEN_MODE), Some(Chord::new('k' as i32, MOD_ALT)));
    }

    #[test]
    fn save_load_round_trip() {
        // Serialize with the crate-wide settings lock since we mutate APPDATA.
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("mighty-ide-kbtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let mut ov = Overrides::new();
        ov.set(CMD_ZEN_MODE, Chord::new('k' as i32, MOD_ALT));
        assert!(save_overrides(&ov));
        let loaded = load_overrides();
        assert_eq!(loaded.get(CMD_ZEN_MODE), Some(Chord::new('k' as i32, MOD_ALT)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn engine_capture_records_override() {
        let mut e = ShortcutsEngine::new();
        // Start from a clean override set (don't depend on disk).
        e.overrides = Overrides::new();
        e.open();
        // Select the Zen row (first remappable in registry order is Zen).
        // Find its index in the filtered rows and move there.
        let idx = e
            .rows
            .iter()
            .position(|r| r.cmd_id == CMD_ZEN_MODE as i32)
            .unwrap();
        e.sel = idx;
        assert!(e.selected_remappable());
        assert!(e.begin_capture());
        assert!(e.is_capturing());
        // A modifier-less chord is rejected.
        assert_eq!(e.capture_chord('k' as i32, 0), 0);
        assert!(e.is_capturing());
        // Alt+K records.
        let rc = e.capture_chord('k' as i32, MOD_ALT);
        assert_eq!(rc, 1);
        assert!(!e.is_capturing());
        assert_eq!(e.overrides.get(CMD_ZEN_MODE), Some(Chord::new('k' as i32, MOD_ALT)));
    }

    #[test]
    fn engine_rejects_remap_of_fixed_row() {
        let mut e = ShortcutsEngine::new();
        e.overrides = Overrides::new();
        e.open();
        // Move to a fixed (negative-id) row.
        let idx = e.rows.iter().position(|r| r.cmd_id < 0).unwrap();
        e.sel = idx;
        assert!(!e.selected_remappable());
        assert!(!e.begin_capture());
        assert!(!e.is_capturing());
    }

    #[test]
    fn click_row_selects_shortcut_row() {
        let mut e = ShortcutsEngine::new();
        e.overrides = Overrides::new();
        e.open();
        let (box_x, _box_y, _box_w, list_top, row_h, _box_h, _top, _shown) = e.geometry(900, 700);
        let idx = e.click_row(box_x + 24.0, list_top + row_h + 3.0, 900, 700);
        assert_eq!(idx, 1);
        assert_eq!(e.selection(), 1);
        assert_eq!(e.click_row(box_x - 2.0, list_top + 3.0, 900, 700), -1);
    }
}
