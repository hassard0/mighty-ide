//! Language detection from a file's path (extension + a couple of well-known
//! filenames). Drives both the multi-language syntax highlighter
//! ([`crate::syntax`]) and the configurable LSP bridge ([`crate::lspregistry`]).
//!
//! Detection is dependency-free and purely lexical — no file content sniffing.
//! Everything maps to a [`Language`] enum; an unknown extension falls back to
//! [`Language::PlainText`]. The Mighty path stays [`Language::Mighty`] so the
//! existing highlighter/LSP/diagnostics behavior is unchanged for `.mty`.

/// A source language the IDE understands well enough to highlight (and, when a
/// server is installed, to drive over LSP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Mighty,
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Json,
    Toml,
    Markdown,
    C,
    Cpp,
    Go,
    Html,
    Css,
    Shell,
    Yaml,
    PlainText,
}

impl Language {
    /// Human-facing name shown in the status-bar language pill.
    pub fn display_name(self) -> &'static str {
        match self {
            Language::Mighty => "Mighty",
            Language::Rust => "Rust",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Json => "JSON",
            Language::Toml => "TOML",
            Language::Markdown => "Markdown",
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Go => "Go",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Shell => "Shell",
            Language::Yaml => "YAML",
            Language::PlainText => "Plain Text",
        }
    }

    /// The LSP `languageId` (the value sent in `didOpen`'s `languageId` field).
    /// These follow the LSP spec's canonical identifiers where one exists.
    pub fn lsp_id(self) -> &'static str {
        match self {
            Language::Mighty => "mighty",
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Json => "json",
            Language::Toml => "toml",
            Language::Markdown => "markdown",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
            Language::Html => "html",
            Language::Css => "css",
            Language::Shell => "shellscript",
            Language::Yaml => "yaml",
            Language::PlainText => "plaintext",
        }
    }

    /// A short, stable slug used as the config-file key for the LSP registry
    /// override (e.g. `rust = "rust-analyzer"`).
    #[allow(dead_code)]
    pub fn slug(self) -> &'static str {
        match self {
            Language::Mighty => "mighty",
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Json => "json",
            Language::Toml => "toml",
            Language::Markdown => "markdown",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
            Language::Html => "html",
            Language::Css => "css",
            Language::Shell => "shell",
            Language::Yaml => "yaml",
            Language::PlainText => "plaintext",
        }
    }

    /// Look up a language by its config slug ([`Language::slug`]).
    pub fn from_slug(slug: &str) -> Option<Language> {
        let s = slug.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "mighty" | "mty" => Language::Mighty,
            "rust" | "rs" => Language::Rust,
            "python" | "py" => Language::Python,
            "javascript" | "js" => Language::JavaScript,
            "typescript" | "ts" => Language::TypeScript,
            "json" => Language::Json,
            "toml" => Language::Toml,
            "markdown" | "md" => Language::Markdown,
            "c" => Language::C,
            "cpp" | "c++" => Language::Cpp,
            "go" => Language::Go,
            "html" => Language::Html,
            "css" => Language::Css,
            "shell" | "sh" | "bash" => Language::Shell,
            "yaml" | "yml" => Language::Yaml,
            "plaintext" | "text" | "txt" => Language::PlainText,
            _ => return None,
        })
    }
}

/// Lower-case the trailing extension of a path string (no leading dot), if any.
fn extension_of(path: &str) -> Option<String> {
    // Use the basename so a dotted directory (`/a.b/file`) doesn't confuse us.
    let base = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path);
    let dot = base.rfind('.')?;
    if dot == 0 {
        // Dotfile with no extension (e.g. `.gitignore`).
        return None;
    }
    Some(base[dot + 1..].to_ascii_lowercase())
}

/// The lower-case basename of a path string.
fn basename_of(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
}

/// Detect the [`Language`] for a path string from its extension, with a few
/// well-known exact filenames (so `Dockerfile`, `Makefile`, etc. still get a
/// sensible language even without an extension). Unknown → [`Language::PlainText`].
pub fn detect(path: &str) -> Language {
    // Exact-filename special cases first (these have no extension).
    match basename_of(path).as_str() {
        "cargo.toml" | "cargo.lock" => return Language::Toml,
        "dockerfile" => return Language::Shell, // close enough for shell-ish coloring
        "makefile" | "gnumakefile" => return Language::Shell,
        "go.mod" | "go.sum" => return Language::Go,
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => return Language::Shell,
        _ => {}
    }

    match extension_of(path).as_deref() {
        Some("mty") => Language::Mighty,
        Some("rs") => Language::Rust,
        Some("py") | Some("pyw") | Some("pyi") => Language::Python,
        Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Language::JavaScript,
        Some("ts") | Some("tsx") | Some("mts") | Some("cts") => Language::TypeScript,
        Some("json") | Some("jsonc") => Language::Json,
        Some("toml") => Language::Toml,
        Some("md") | Some("markdown") => Language::Markdown,
        Some("c") | Some("h") => Language::C,
        Some("cpp") | Some("cxx") | Some("cc") | Some("hpp") | Some("hh") | Some("hxx") => {
            Language::Cpp
        }
        Some("go") => Language::Go,
        Some("html") | Some("htm") | Some("xhtml") => Language::Html,
        Some("css") | Some("scss") | Some("sass") => Language::Css,
        Some("sh") | Some("bash") | Some("zsh") => Language::Shell,
        Some("yaml") | Some("yml") => Language::Yaml,
        _ => Language::PlainText,
    }
}

/// Convenience wrapper for a `std::path::Path`.
pub fn detect_path(path: &std::path::Path) -> Language {
    detect(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_extensions() {
        let cases: &[(&str, Language)] = &[
            ("src/main.mty", Language::Mighty),
            ("foo.rs", Language::Rust),
            ("a/b/c.py", Language::Python),
            ("app.js", Language::JavaScript),
            ("app.jsx", Language::JavaScript),
            ("mod.mjs", Language::JavaScript),
            ("app.ts", Language::TypeScript),
            ("comp.tsx", Language::TypeScript),
            ("data.json", Language::Json),
            ("Cargo.toml", Language::Toml),
            ("README.md", Language::Markdown),
            ("u.c", Language::C),
            ("u.h", Language::C),
            ("v.cpp", Language::Cpp),
            ("v.hpp", Language::Cpp),
            ("v.cc", Language::Cpp),
            ("main.go", Language::Go),
            ("index.html", Language::Html),
            ("style.css", Language::Css),
            ("run.sh", Language::Shell),
            ("ci.yaml", Language::Yaml),
            ("ci.yml", Language::Yaml),
            ("notes.txt", Language::PlainText),
            ("noext", Language::PlainText),
        ];
        for (p, want) in cases {
            assert_eq!(detect(p), *want, "path {p}");
        }
    }

    #[test]
    fn detects_windows_paths() {
        assert_eq!(detect(r"C:\Users\me\proj\lib.rs"), Language::Rust);
        assert_eq!(detect(r"C:\x\main.mty"), Language::Mighty);
    }

    #[test]
    fn exact_filenames() {
        assert_eq!(detect("Cargo.toml"), Language::Toml);
        assert_eq!(detect("/project/Makefile"), Language::Shell);
        assert_eq!(detect("go.mod"), Language::Go);
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(detect("FOO.RS"), Language::Rust);
        assert_eq!(detect("Data.JSON"), Language::Json);
    }

    #[test]
    fn dotfile_no_extension_is_plaintext_or_special() {
        assert_eq!(detect(".gitignore"), Language::PlainText);
        assert_eq!(detect(".bashrc"), Language::Shell);
    }

    #[test]
    fn slug_round_trips() {
        for lang in [
            Language::Mighty,
            Language::Rust,
            Language::Python,
            Language::JavaScript,
            Language::TypeScript,
            Language::Json,
            Language::Toml,
            Language::Markdown,
            Language::C,
            Language::Cpp,
            Language::Go,
            Language::Html,
            Language::Css,
            Language::Shell,
            Language::Yaml,
            Language::PlainText,
        ] {
            assert_eq!(Language::from_slug(lang.slug()), Some(lang), "{lang:?}");
        }
    }
}
