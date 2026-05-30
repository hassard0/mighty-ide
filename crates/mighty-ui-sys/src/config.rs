//! Tiny persisted IDE config — currently just the selected color theme.
//!
//! The config is a one-line `key=value` text file at
//! `%APPDATA%/mighty-ide/config` (Windows) / `$XDG_CONFIG_HOME` or
//! `~/.config/mighty-ide/config` (else), e.g.:
//!
//! ```text
//! theme=warm
//! ```
//!
//! Load order at startup ([`resolve_startup_theme`]):
//!   1. the `MUI_THEME` env override (`vivid`/`aurora`/`warm`) — used by the
//!      screenshot-capture path to force a theme without writing config;
//!   2. the persisted config file;
//!   3. the default ([`theme::ThemeId::Vivid`]).
//!
//! [`save_theme`] writes the choice so the picker's selection survives a
//! restart. Both are best-effort: a missing/corrupt config never fails the IDE.

use std::path::PathBuf;

use crate::theme::ThemeId;

/// Directory that holds the config file (created on save if absent).
fn config_dir() -> Option<PathBuf> {
    // Windows: %APPDATA%\mighty-ide. Else: $XDG_CONFIG_HOME or ~/.config.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(appdata).join("mighty-ide"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("mighty-ide"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home).join(".config").join("mighty-ide"));
    }
    None
}

/// Full path to the config file.
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config"))
}

/// Parse a `key=value` config blob for the `theme=` line, returning the id.
fn parse_theme(text: &str) -> Option<ThemeId> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("theme") {
                return ThemeId::from_slug(v.trim());
            }
        }
    }
    None
}

/// Render the config blob for a given theme. (Retained for the round-trip test;
/// production writes go through [`render_all`].)
#[allow(dead_code)]
fn render(theme: ThemeId) -> String {
    format!(
        "# Mighty IDE config\n# theme = vivid | aurora | warm\ntheme={}\n",
        theme.slug()
    )
}

/// Render the full config blob: the theme line plus the editor-preference lines
/// ([`crate::settings`]). Written by [`save_all`] so theme + settings persist
/// together in one file.
fn render_all(theme: ThemeId, settings: &crate::settings::Settings) -> String {
    format!(
        "# Mighty IDE config\n# theme = vivid | aurora | warm\ntheme={}\n{}",
        theme.slug(),
        crate::settings::render(settings),
    )
}

/// Read the persisted theme from the config file, or `None` if unset/unreadable.
pub fn load_theme() -> Option<ThemeId> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    parse_theme(&text)
}

/// Persist `theme` to the config file (creating the directory). Best-effort:
/// returns `false` (and logs) on any I/O error. Activates `theme` first so the
/// shared writer ([`save_all`]) also preserves the current editor settings — the
/// theme picker calls `theme::set_active(id)` before this, so the activation is
/// idempotent.
pub fn save_theme(theme: ThemeId) -> bool {
    crate::theme::set_active(theme);
    save_all()
}

/// Persist BOTH the active theme and the active editor settings to the config
/// file (so the Settings panel + theme picker share one file). Best-effort.
pub fn save_all() -> bool {
    let Some(path) = config_path() else {
        eprintln!("config: no config directory available; settings not persisted");
        return false;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("config: create_dir_all {}: {e}", parent.display());
            return false;
        }
    }
    let blob = render_all(crate::theme::active_id(), &crate::settings::active());
    match std::fs::write(&path, blob) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("config: write {}: {e}", path.display());
            false
        }
    }
}

/// The theme to activate at startup: `MUI_THEME` env override, else the
/// persisted config, else the default (Vivid).
pub fn resolve_startup_theme() -> ThemeId {
    if let Some(v) = std::env::var_os("MUI_THEME") {
        if let Some(id) = ThemeId::from_slug(&v.to_string_lossy()) {
            return id;
        }
    }
    load_theme().unwrap_or(ThemeId::Vivid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_picks_theme_line() {
        assert_eq!(parse_theme("theme=aurora\n"), Some(ThemeId::Aurora));
        assert_eq!(parse_theme("# c\ntheme = warm \n"), Some(ThemeId::Warm));
        assert_eq!(parse_theme("theme=vivid"), Some(ThemeId::Vivid));
        assert_eq!(parse_theme("nothing=here"), None);
        assert_eq!(parse_theme(""), None);
        assert_eq!(parse_theme("theme=bogus"), None);
    }

    #[test]
    fn render_round_trips_through_parse() {
        for id in ThemeId::ALL {
            let blob = render(id);
            assert_eq!(parse_theme(&blob), Some(id), "round-trip {id:?}");
        }
    }

    #[test]
    fn save_then_load_round_trip() {
        // Share the crate-wide settings/theme test lock so this serializes with
        // the theme-picker / settings-panel tests that also mutate global APPDATA.
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Point the config dir at a temp location for this test.
        let tmp = std::env::temp_dir().join(format!("mighty-ide-cfgtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        // Save aurora, load it back.
        assert!(save_theme(ThemeId::Aurora));
        assert_eq!(load_theme(), Some(ThemeId::Aurora));
        // Overwrite with warm.
        assert!(save_theme(ThemeId::Warm));
        assert_eq!(load_theme(), Some(ThemeId::Warm));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
