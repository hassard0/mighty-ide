//! "Mighty: New Project" — name validation + scaffolding via `mty new`.
//!
//! The IDE's New Project command collects a project NAME through the existing
//! bottom prompt (L17: strings can't cross the FFI, so the name is streamed in
//! char-by-char), then this module:
//!   * validates the name (a safe single path segment — no separators, no
//!     traversal, sane charset), so we never shell out with a hostile string;
//!   * resolves a PARENT directory to create the project under (the current
//!     workspace root when one is open, else the user's home dir, else cwd);
//!   * runs `mty new <name>` there and reports success / failure.
//!
//! The validation + parent-dir resolution are pure and unit-tested here; the
//! actual `mty new` invocation + workspace re-root + toast live in
//! [`crate::newprojabi`] (it needs the GPU-backed context).

use std::path::{Path, PathBuf};

/// Max length of a project name (keeps directory names sane across platforms).
pub const MAX_NAME_LEN: usize = 64;

/// Validate a typed project name. Returns the cleaned name on success, or an
/// error string suitable for a toast.
///
/// Rules: after trimming, the name must be non-empty, at most [`MAX_NAME_LEN`]
/// chars, a SINGLE path segment (no `/`, `\`, or `:` so it can't escape the
/// parent dir or name a drive), not a `.`/`..` traversal token, and built from
/// a conservative charset (ASCII alphanumerics plus `-`, `_`, `.`). The first
/// char must be alphanumeric or `_` so the result is a usable identifier-ish
/// directory name.
pub fn validate_name(input: &str) -> Result<String, String> {
    let name = input.trim();
    if name.is_empty() {
        return Err("Enter a project name".to_string());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!("Project name too long (max {MAX_NAME_LEN})"));
    }
    if name == "." || name == ".." {
        return Err("Invalid project name".to_string());
    }
    if name.contains('/') || name.contains('\\') || name.contains(':') {
        return Err("Name must not contain path separators".to_string());
    }
    let first = name.chars().next().unwrap();
    if !(first.is_ascii_alphanumeric() || first == '_') {
        return Err("Name must start with a letter, digit or underscore".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("Use letters, digits, '-', '_' or '.' only".to_string());
    }
    Ok(name.to_string())
}

/// Choose the directory to create the new project UNDER. Prefers `workspace`
/// (the currently-open folder) when it is a real, existing directory; otherwise
/// the user's home dir; otherwise the current working dir; otherwise `.`.
pub fn resolve_parent_dir(workspace: Option<&Path>) -> PathBuf {
    if let Some(ws) = workspace {
        if !ws.as_os_str().is_empty() && ws.is_dir() {
            return ws.to_path_buf();
        }
    }
    if let Some(home) = home_dir() {
        if home.is_dir() {
            return home;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The user's home directory from the platform env vars (no extra deps).
fn home_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var_os("HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_names() {
        assert_eq!(validate_name("hello").unwrap(), "hello");
        assert_eq!(validate_name("  my-app  ").unwrap(), "my-app");
        assert_eq!(validate_name("game_2").unwrap(), "game_2");
        assert_eq!(validate_name("_private").unwrap(), "_private");
    }

    #[test]
    fn rejects_blank() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
    }

    #[test]
    fn rejects_path_separators_and_traversal() {
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("C:foo").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("../escape").is_err());
    }

    #[test]
    fn rejects_bad_first_char_and_charset() {
        assert!(validate_name("-leading").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("emoji\u{1F600}").is_err());
        assert!(validate_name("semi;colon").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name(&long).is_err());
        let ok = "a".repeat(MAX_NAME_LEN);
        assert!(validate_name(&ok).is_ok());
    }

    #[test]
    fn parent_prefers_existing_workspace() {
        let tmp = std::env::temp_dir();
        let got = resolve_parent_dir(Some(&tmp));
        assert_eq!(got, tmp);
    }

    #[test]
    fn parent_falls_back_when_workspace_missing() {
        // An empty / nonexistent workspace path should not be used.
        let got = resolve_parent_dir(Some(Path::new("")));
        assert!(got.is_dir(), "fallback should be a real directory");
        let bogus = std::env::temp_dir().join("definitely-not-here-9988776655");
        let got2 = resolve_parent_dir(Some(&bogus));
        assert!(got2.is_dir());
        assert_ne!(got2, bogus);
    }
}
