//! Configurable LSP server registry: maps a [`crate::langdetect::Language`] to
//! the launch command (program + args) for its language server, with built-in
//! defaults and a config-file override.
//!
//! ## Built-in defaults
//!
//! | language          | server                                    |
//! |-------------------|-------------------------------------------|
//! | mighty            | `mty lsp`                                 |
//! | rust              | `rust-analyzer`                           |
//! | python            | `pyright-langserver --stdio` (`pylsp` alt)|
//! | typescript/js     | `typescript-language-server --stdio`      |
//! | go                | `gopls`                                   |
//! | c / cpp           | `clangd`                                  |
//! | json              | `vscode-json-language-server --stdio`     |
//! | html              | `vscode-html-language-server --stdio`     |
//! | css               | `vscode-css-language-server --stdio`      |
//! | yaml              | `yaml-language-server --stdio`            |
//!
//! ## Override file
//!
//! A `lsp.toml`-style file (one `slug = "cmd arg1 arg2"` per line) in the IDE
//! config directory (`%APPDATA%/mighty-ide/lsp.toml`) lets users point at a
//! specific server binary / change args, e.g.:
//!
//! ```text
//! python = "pylsp"
//! rust   = "/opt/ra/rust-analyzer"
//! ```
//!
//! A value of `""` (empty) or `off`/`none` disables LSP for that language.
//!
//! ## Availability
//!
//! [`server_for`] returns the configured [`ServerSpec`] only when the program
//! is found on `PATH` (or is an existing absolute path) — so a language whose
//! server isn't installed silently has no LSP (highlighting + editing still
//! work; the caller must NEVER block/crash). The Mighty entry resolves through
//! the existing dev-path fallback so the `mty lsp` path is unchanged.

use std::path::{Path, PathBuf};

use crate::langdetect::Language;

/// A resolved language-server launch spec: the program (already verified to
/// exist) plus its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// The program to spawn (absolute path if resolved on PATH; bare name only
    /// for the Mighty `mty` fallback which the client resolves itself).
    pub program: String,
    /// Arguments passed to the program (e.g. `["lsp"]`, `["--stdio"]`).
    pub args: Vec<String>,
}

/// The built-in default command line for `lang` (program + args), before any
/// config override or PATH resolution. `None` means "no LSP for this language".
fn default_command(lang: Language) -> Option<(&'static str, &'static [&'static str])> {
    match lang {
        Language::Mighty => Some(("mty", &["lsp"])),
        Language::Rust => Some(("rust-analyzer", &[])),
        Language::Python => Some(("pyright-langserver", &["--stdio"])),
        Language::TypeScript | Language::JavaScript => {
            Some(("typescript-language-server", &["--stdio"]))
        }
        Language::Go => Some(("gopls", &[])),
        Language::C | Language::Cpp => Some(("clangd", &[])),
        Language::Json => Some(("vscode-json-language-server", &["--stdio"])),
        Language::Html => Some(("vscode-html-language-server", &["--stdio"])),
        Language::Css => Some(("vscode-css-language-server", &["--stdio"])),
        Language::Yaml => Some(("yaml-language-server", &["--stdio"])),
        Language::Shell => Some(("bash-language-server", &["start"])),
        Language::Toml | Language::Markdown | Language::PlainText => None,
    }
}

/// Parse one config override value into a command line. An empty / `off` / `none`
/// value disables the language. Returns `Some((program, args))` or `None` to
/// disable.
fn parse_command_value(value: &str) -> Option<CommandLine> {
    let v = value.trim().trim_matches('"').trim();
    if v.is_empty() || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("none") {
        return None;
    }
    // Simple whitespace tokenization (paths with spaces should be the whole
    // value if no args; we keep it simple — quote-aware splitting is overkill).
    let mut parts = v.split_whitespace();
    let program = parts.next()?.to_string();
    let args = parts.map(|s| s.to_string()).collect();
    Some((program, args))
}

/// A parsed command line: `(program, args)`. `None` (in an override) means the
/// language is explicitly disabled.
pub type CommandLine = (String, Vec<String>);

/// Parse a whole override blob (`slug = "command"` lines) into a list of
/// `(Language, Option<command>)` entries. Lines beginning with `#` and blank
/// lines are ignored. Unknown slugs are skipped. `None` for the command means
/// the language is explicitly disabled.
pub fn parse_overrides(text: &str) -> Vec<(Language, Option<CommandLine>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let Some(lang) = Language::from_slug(k.trim()) else {
            continue;
        };
        out.push((lang, parse_command_value(v)));
    }
    out
}

/// Path to the LSP override config file, if a config dir is available.
pub fn override_path() -> Option<PathBuf> {
    crate::config::config_path().and_then(|p| p.parent().map(|d| d.join("lsp.toml")))
}

/// Load the override for `lang` from the config file (if present). Returns
/// `Some(Some(cmd))` to override, `Some(None)` to explicitly disable, or `None`
/// when there is no entry for this language (use the default).
fn load_override(lang: Language) -> Option<Option<CommandLine>> {
    let path = override_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    parse_overrides(&text)
        .into_iter()
        .find(|(l, _)| *l == lang)
        .map(|(_, cmd)| cmd)
}

/// Find `program` on `PATH` (Windows: trying the `PATHEXT` extensions), or
/// accept it directly if it's an existing absolute/relative path. Returns the
/// resolved path string, or `None` if not found.
pub fn which(program: &str) -> Option<String> {
    // Direct path (absolute or contains a separator) — accept if it exists.
    let p = Path::new(program);
    if program.contains('/') || program.contains('\\') {
        if p.exists() {
            return Some(program.to_string());
        }
        // Also try with extensions on Windows.
        if cfg!(windows) {
            for ext in pathext() {
                let cand = PathBuf::from(format!("{program}{ext}"));
                if cand.exists() {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
        return None;
    }

    let path_var = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        pathext()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path_var) {
        for ext in &exts {
            let cand = dir.join(format!("{program}{ext}"));
            if cand.is_file() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// The executable extensions to try on Windows (from `PATHEXT`, lower-cased),
/// always including the empty string so an explicit `.exe` works too.
fn pathext() -> Vec<String> {
    let mut exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    exts.push(String::new());
    exts
}

/// Resolve the language-server [`ServerSpec`] for `lang`, applying any config
/// override and verifying the program is installed. Returns `None` if there's
/// no server configured for this language, it's explicitly disabled, or the
/// program isn't found on PATH.
///
/// Special-case Mighty: the program resolves through the existing
/// [`mty_resolved_path`] (honors `MIGHTY_MTY` + the dev-build fallback) so the
/// `mty lsp` path keeps working exactly as before even when `mty` isn't on PATH.
pub fn server_for(lang: Language) -> Option<ServerSpec> {
    resolve_spec(lang, load_override(lang))
}

/// The testable core of [`server_for`]: resolve a [`ServerSpec`] given an
/// explicit `override_cmd` (the result of [`load_override`] — `None` = no entry,
/// `Some(None)` = disabled, `Some(Some(cmd))` = override). Verifies the program
/// is on PATH (except Mighty, which resolves itself).
pub fn resolve_spec(
    lang: Language,
    override_cmd: Option<Option<CommandLine>>,
) -> Option<ServerSpec> {
    // Determine the command line: override, else default.
    let (program, args): (String, Vec<String>) = match override_cmd {
        Some(Some((p, a))) => (p, a),
        Some(None) => return None, // explicitly disabled
        None => {
            let (p, a) = default_command(lang)?;
            (p.to_string(), a.iter().map(|s| s.to_string()).collect())
        }
    };

    if lang == Language::Mighty {
        // The Mighty client resolves `mty` itself (MIGHTY_MTY / dev path / PATH);
        // hand back the program as-is so behavior is unchanged.
        let resolved = mty_resolved_path();
        return Some(ServerSpec {
            program: resolved,
            args,
        });
    }

    // For all other languages, require the program on PATH.
    let resolved = which(&program)?;
    Some(ServerSpec {
        program: resolved,
        args,
    })
}

/// The `mty` path used for the Mighty LSP/diagnostics (honors `MIGHTY_MTY`, then
/// the known dev build path, else bare `mty`). Mirrors the per-module helpers.
pub fn mty_resolved_path() -> String {
    if let Ok(p) = std::env::var("MIGHTY_MTY") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    const DEV: &str = r"C:\Users\ihass\stardust\target\debug\mty.exe";
    if Path::new(DEV).exists() {
        return DEV.to_string();
    }
    "mty".to_string()
}

/// Whether `lang` has any configured server at all (ignoring whether it's
/// installed). Used by tests and UI hints.
#[allow(dead_code)]
pub fn has_default_server(lang: Language) -> bool {
    default_command(lang).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_the_expected_languages() {
        assert_eq!(default_command(Language::Mighty), Some(("mty", &["lsp"][..])));
        assert_eq!(default_command(Language::Rust), Some(("rust-analyzer", &[][..])));
        assert!(default_command(Language::Python).is_some());
        assert!(default_command(Language::TypeScript).is_some());
        assert!(default_command(Language::JavaScript).is_some());
        assert!(default_command(Language::Go).is_some());
        assert!(default_command(Language::C).is_some());
        assert!(default_command(Language::Cpp).is_some());
        // Pure data/markup formats have no server.
        assert!(default_command(Language::Toml).is_none());
        assert!(default_command(Language::Markdown).is_none());
        assert!(default_command(Language::PlainText).is_none());
    }

    #[test]
    fn parse_command_value_splits_program_and_args() {
        assert_eq!(
            parse_command_value("pyright-langserver --stdio"),
            Some(("pyright-langserver".to_string(), vec!["--stdio".to_string()]))
        );
        assert_eq!(
            parse_command_value("  \"rust-analyzer\"  "),
            Some(("rust-analyzer".to_string(), vec![]))
        );
    }

    #[test]
    fn parse_command_value_disables_on_empty_or_off() {
        assert_eq!(parse_command_value(""), None);
        assert_eq!(parse_command_value("  "), None);
        assert_eq!(parse_command_value("off"), None);
        assert_eq!(parse_command_value("NONE"), None);
    }

    #[test]
    fn parse_overrides_reads_known_slugs() {
        let blob = "\
# servers
python = \"pylsp\"
rust = /opt/ra/rust-analyzer
typescript = off
boguslang = whatever
";
        let ovr = parse_overrides(blob);
        // python -> pylsp
        let py = ovr.iter().find(|(l, _)| *l == Language::Python).unwrap();
        assert_eq!(py.1, Some(("pylsp".to_string(), vec![])));
        // rust -> absolute path
        let rs = ovr.iter().find(|(l, _)| *l == Language::Rust).unwrap();
        assert_eq!(rs.1, Some(("/opt/ra/rust-analyzer".to_string(), vec![])));
        // typescript disabled
        let ts = ovr.iter().find(|(l, _)| *l == Language::TypeScript).unwrap();
        assert_eq!(ts.1, None);
        // unknown slug skipped
        assert_eq!(ovr.len(), 3);
    }

    #[test]
    fn which_finds_nonexistent_returns_none() {
        assert!(which("definitely-not-a-real-binary-xyz-12345").is_none());
    }

    #[test]
    fn resolve_spec_uninstalled_language_skips_gracefully() {
        // A language whose server isn't installed (no override) resolves to None
        // rather than panicking. Use a definitely-absent default by overriding to
        // a bogus binary so the result is deterministic regardless of CI tooling.
        let bogus = Some(Some(("definitely-not-a-real-binary-xyz-12345".to_string(), vec![])));
        assert_eq!(resolve_spec(Language::Go, bogus), None);
        // A language with no server at all -> always None.
        assert_eq!(resolve_spec(Language::Toml, None), None);
    }

    #[test]
    fn resolve_spec_override_disables_language() {
        // `Some(None)` is the "explicitly disabled" override.
        assert_eq!(resolve_spec(Language::Rust, Some(None)), None);
    }

    #[test]
    fn resolve_spec_mighty_always_available() {
        // Mighty resolves itself (MIGHTY_MTY / dev path / bare `mty`) without a
        // PATH check, so a spec is always produced with the `lsp` arg.
        let spec = resolve_spec(Language::Mighty, None).expect("mighty spec");
        assert_eq!(spec.args, vec!["lsp".to_string()]);
    }
}
