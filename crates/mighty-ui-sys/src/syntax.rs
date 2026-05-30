//! A small, dependency-free, **multi-language** syntax highlighter.
//!
//! Produces a list of `(start_char, len_chars, color)` spans for one line so
//! the editor body draw can color tokens (keywords, types, strings, comments,
//! numbers, function calls, punctuation). It is intentionally line-local and
//! approximate — good enough for a premium-looking editor without a full
//! parser. Colors come from [`crate::theme`].
//!
//! ## Multi-language design
//!
//! Highlighting is driven by a per-language [`SyntaxConfig`] (keyword set,
//! line-/block-comment delimiters, string delimiters, a number rule, and a
//! "treat PascalCase as a type" flag). The generic scanner
//! [`highlight_line_lang`] takes a [`crate::langdetect::Language`] and looks up
//! its config. [`highlight_line`] is the Mighty-default entry point (unchanged
//! behavior) used where no language context is available.
//!
//! This is config-driven, not a full grammar — that is by design. Block
//! comments are detected only when they open and close on the same line (the
//! editor is line-local); JSON/TOML/Markdown get light tailored handling via
//! their configs.

use crate::ffi::MuiColor;
use crate::langdetect::Language;
use crate::theme;

/// A colored span within a single line: char offset, char length, and color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub color: MuiColor,
}

/// How string literals behave in a language.
#[derive(Debug, Clone, Copy)]
pub struct StringRule {
    /// `"` double-quoted strings are present (almost all languages).
    pub double: bool,
    /// `'` single-quoted strings (Python/JS/shell); for C/C++/Rust/Go the `'`
    /// is a char literal but we color it the same.
    pub single: bool,
    /// `` ` `` template / raw strings (JS/TS template literals, Go raw strings).
    pub backtick: bool,
    /// Backslash escapes are honored inside strings (so `\"` doesn't close).
    pub escapes: bool,
}

impl StringRule {
    const fn common() -> Self {
        StringRule { double: true, single: true, backtick: false, escapes: true }
    }
}

/// A per-language highlighting configuration.
pub struct SyntaxConfig {
    /// Reserved words colored as keywords.
    pub keywords: &'static [&'static str],
    /// Builtin/primitive type names colored as types.
    pub types: &'static [&'static str],
    /// Line-comment delimiters (e.g. `//`, `#`, `--`). Empty = none.
    pub line_comments: &'static [&'static str],
    /// Same-line block-comment delimiter pair (`/* */`), if any.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// String-literal behavior.
    pub strings: StringRule,
    /// Treat a leading-uppercase identifier as a type (Mighty/Rust/Go style).
    pub pascal_is_type: bool,
    /// Highlight numeric literals.
    pub numbers: bool,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ---------------------------------------------------------------------------
// Per-language keyword / type tables
// ---------------------------------------------------------------------------

const MIGHTY_KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "while", "if", "else", "return", "match", "struct", "enum", "extern",
    "effect", "import", "pub", "for", "in", "type", "true", "false", "alloc", "use", "const",
    "impl", "as", "self", "Some", "None", "Ok", "Err",
];
const MIGHTY_TYPES: &[&str] = &[
    "I32", "I64", "U8", "U16", "U32", "U64", "USize", "F32", "F64", "Bool", "Str", "String",
    "Vec", "Option", "Result", "Unit",
];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "union",
];
const RUST_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "Rc", "Arc",
    "Cell", "RefCell", "HashMap", "BTreeMap",
];

const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
    "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try",
    "while", "with", "yield", "self", "match", "case",
];
const PY_TYPES: &[&str] = &[
    "int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object", "type",
];

const JS_KEYWORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "export", "extends", "finally", "for", "function", "if",
    "import", "in", "instanceof", "let", "new", "of", "return", "super", "switch", "this", "throw",
    "try", "typeof", "var", "void", "while", "with", "yield", "true", "false", "null", "undefined",
    "static", "get", "set",
];
const TS_KEYWORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "enum", "export", "extends", "finally", "for", "function",
    "if", "implements", "import", "in", "instanceof", "interface", "let", "namespace", "new", "of",
    "private", "protected", "public", "readonly", "return", "super", "switch", "this", "throw",
    "try", "type", "typeof", "var", "void", "while", "yield", "true", "false", "null", "undefined",
    "as", "is", "keyof", "abstract", "declare", "static",
];
const TS_TYPES: &[&str] = &[
    "string", "number", "boolean", "any", "unknown", "never", "void", "object", "symbol", "bigint",
];

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern", "for",
    "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static", "struct",
    "switch", "typedef", "union", "volatile", "while",
];
const C_TYPES: &[&str] = &[
    "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "bool",
    "size_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t",
    "uint64_t",
];
const CPP_KEYWORDS: &[&str] = &[
    "alignas", "alignof", "and", "auto", "break", "case", "catch", "class", "const", "constexpr",
    "continue", "decltype", "default", "delete", "do", "else", "enum", "explicit", "export",
    "extern", "for", "friend", "goto", "if", "inline", "mutable", "namespace", "new", "noexcept",
    "nullptr", "operator", "private", "protected", "public", "return", "sizeof", "static",
    "struct", "switch", "template", "this", "throw", "try", "typedef", "typename", "union",
    "using", "virtual", "volatile", "while", "true", "false",
];

const GO_KEYWORDS: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for",
    "func", "go", "goto", "if", "import", "interface", "map", "package", "range", "return",
    "select", "struct", "switch", "type", "var", "nil", "true", "false", "iota",
];
const GO_TYPES: &[&str] = &[
    "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8",
    "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64",
    "uintptr",
];

const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "case", "esac", "for", "while", "until", "do", "done",
    "in", "function", "select", "time", "return", "export", "local", "readonly", "declare", "set",
    "unset", "echo", "cd", "exit",
];

const CSS_KEYWORDS: &[&str] = &[
    "important", "inherit", "initial", "unset", "none", "auto", "block", "inline", "flex", "grid",
    "absolute", "relative", "fixed", "static", "sticky",
];

const YAML_KEYWORDS: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];
const TOML_KEYWORDS: &[&str] = &["true", "false"];
const JSON_KEYWORDS: &[&str] = &["true", "false", "null"];

const EMPTY: &[&str] = &[];

/// The [`SyntaxConfig`] for `lang`. Languages that share a shape reuse tables.
pub fn config_for(lang: Language) -> SyntaxConfig {
    match lang {
        Language::Mighty => SyntaxConfig {
            keywords: MIGHTY_KEYWORDS,
            types: MIGHTY_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule::common(),
            pascal_is_type: true,
            numbers: true,
        },
        Language::Rust => SyntaxConfig {
            keywords: RUST_KEYWORDS,
            types: RUST_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule::common(),
            pascal_is_type: true,
            numbers: true,
        },
        Language::Python => SyntaxConfig {
            keywords: PY_KEYWORDS,
            types: PY_TYPES,
            line_comments: &["#"],
            block_comment: None,
            strings: StringRule::common(),
            pascal_is_type: true,
            numbers: true,
        },
        Language::JavaScript => SyntaxConfig {
            keywords: JS_KEYWORDS,
            types: EMPTY,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule { double: true, single: true, backtick: true, escapes: true },
            pascal_is_type: true,
            numbers: true,
        },
        Language::TypeScript => SyntaxConfig {
            keywords: TS_KEYWORDS,
            types: TS_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule { double: true, single: true, backtick: true, escapes: true },
            pascal_is_type: true,
            numbers: true,
        },
        Language::C => SyntaxConfig {
            keywords: C_KEYWORDS,
            types: C_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule::common(),
            pascal_is_type: false,
            numbers: true,
        },
        Language::Cpp => SyntaxConfig {
            keywords: CPP_KEYWORDS,
            types: C_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule::common(),
            pascal_is_type: true,
            numbers: true,
        },
        Language::Go => SyntaxConfig {
            keywords: GO_KEYWORDS,
            types: GO_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            strings: StringRule { double: true, single: true, backtick: true, escapes: true },
            pascal_is_type: true,
            numbers: true,
        },
        Language::Json => SyntaxConfig {
            keywords: JSON_KEYWORDS,
            types: EMPTY,
            line_comments: EMPTY,
            block_comment: None,
            strings: StringRule { double: true, single: false, backtick: false, escapes: true },
            pascal_is_type: false,
            numbers: true,
        },
        Language::Toml => SyntaxConfig {
            keywords: TOML_KEYWORDS,
            types: EMPTY,
            line_comments: &["#"],
            block_comment: None,
            strings: StringRule { double: true, single: true, backtick: false, escapes: true },
            pascal_is_type: false,
            numbers: true,
        },
        Language::Markdown => SyntaxConfig {
            keywords: EMPTY,
            types: EMPTY,
            line_comments: EMPTY,
            block_comment: None,
            strings: StringRule { double: false, single: false, backtick: true, escapes: false },
            pascal_is_type: false,
            numbers: false,
        },
        Language::Html => SyntaxConfig {
            keywords: EMPTY,
            types: EMPTY,
            line_comments: EMPTY,
            block_comment: Some(("<!--", "-->")),
            strings: StringRule { double: true, single: true, backtick: false, escapes: false },
            pascal_is_type: false,
            numbers: false,
        },
        Language::Css => SyntaxConfig {
            keywords: CSS_KEYWORDS,
            types: EMPTY,
            line_comments: EMPTY,
            block_comment: Some(("/*", "*/")),
            strings: StringRule { double: true, single: true, backtick: false, escapes: true },
            pascal_is_type: false,
            numbers: true,
        },
        Language::Shell => SyntaxConfig {
            keywords: SHELL_KEYWORDS,
            types: EMPTY,
            line_comments: &["#"],
            block_comment: None,
            strings: StringRule { double: true, single: true, backtick: true, escapes: true },
            pascal_is_type: false,
            numbers: true,
        },
        Language::Yaml => SyntaxConfig {
            keywords: YAML_KEYWORDS,
            types: EMPTY,
            line_comments: &["#"],
            block_comment: None,
            strings: StringRule { double: true, single: true, backtick: false, escapes: true },
            pascal_is_type: false,
            numbers: true,
        },
        Language::PlainText => SyntaxConfig {
            keywords: EMPTY,
            types: EMPTY,
            line_comments: EMPTY,
            block_comment: None,
            strings: StringRule { double: false, single: false, backtick: false, escapes: false },
            pascal_is_type: false,
            numbers: false,
        },
    }
}

/// Tokenize `line` into colored spans (char-indexed) for the Mighty language.
/// Kept as the default entry point so callers without language context get the
/// original behavior unchanged.
#[allow(dead_code)]
pub fn highlight_line(line: &str) -> Vec<Span> {
    highlight_line_lang(line, Language::Mighty)
}

/// Tokenize `line` into colored spans (char-indexed) for `lang`'s
/// [`SyntaxConfig`]. Whitespace is left uncolored (no span).
pub fn highlight_line_lang(line: &str, lang: Language) -> Vec<Span> {
    let cfg = config_for(lang);
    highlight_with(line, &cfg)
}

/// Markdown gets light, line-local tailored handling: headings, list bullets,
/// blockquotes, and inline `code` spans. Other tokens fall through to the
/// generic scanner (which colors backtick spans as strings).
fn highlight_markdown(line: &str) -> Option<Vec<Span>> {
    let chars: Vec<char> = line.chars().collect();
    let trimmed = line.trim_start();
    let indent = chars.len() - trimmed.chars().count();
    // ATX heading: a run of leading '#'.
    if trimmed.starts_with('#') {
        return Some(vec![Span {
            start: 0,
            len: chars.len(),
            color: theme::SYN_KEYWORD(),
        }]);
    }
    // Blockquote.
    if trimmed.starts_with('>') {
        return Some(vec![Span {
            start: 0,
            len: chars.len(),
            color: theme::SYN_COMMENT(),
        }]);
    }
    // List bullet: -, *, + then space.
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        let _ = rest;
        let mut spans = vec![Span {
            start: indent,
            len: 1,
            color: theme::SYN_FUNCTION(),
        }];
        // Color inline code spans in the remainder via the generic scanner.
        spans.extend(highlight_with(line, &config_for(Language::Markdown)));
        return Some(spans);
    }
    None
}

/// The core generic scanner. Produces spans from `cfg`'s rules.
pub fn highlight_with(line: &str, cfg: &SyntaxConfig) -> Vec<Span> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0usize;

    while i < n {
        let c = chars[i];

        // Whitespace — skip.
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Line comment — to end of line. Match the longest configured delim.
        if let Some(delim) = cfg
            .line_comments
            .iter()
            .find(|d| starts_with_at(&chars, i, d))
        {
            let _ = delim;
            spans.push(Span {
                start: i,
                len: n - i,
                color: theme::SYN_COMMENT(),
            });
            break;
        }

        // Same-line block comment `/* ... */` (line-local: if the close isn't on
        // this line, color to EOL).
        if let Some((open, close)) = cfg.block_comment {
            if starts_with_at(&chars, i, open) {
                let start = i;
                i += open.chars().count();
                let close_chars: Vec<char> = close.chars().collect();
                while i < n && !starts_with_at(&chars, i, close) {
                    i += 1;
                }
                if i < n {
                    i += close_chars.len();
                }
                spans.push(Span {
                    start,
                    len: i - start,
                    color: theme::SYN_COMMENT(),
                });
                continue;
            }
        }

        // String / char / template literals.
        let is_str_open = (c == '"' && cfg.strings.double)
            || (c == '\'' && cfg.strings.single)
            || (c == '`' && cfg.strings.backtick);
        if is_str_open {
            let quote = c;
            let start = i;
            i += 1;
            while i < n {
                if cfg.strings.escapes && chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span {
                start,
                len: i - start,
                color: theme::SYN_STRING(),
            });
            continue;
        }

        // Number literal (decimal/hex/float, with optional `_suffix`).
        if cfg.numbers && c.is_ascii_digit() {
            let start = i;
            while i < n
                && (chars[i].is_alphanumeric()
                    || chars[i] == '.'
                    || chars[i] == '_'
                    || chars[i] == 'x'
                    || chars[i] == 'X')
            {
                i += 1;
            }
            spans.push(Span {
                start,
                len: i - start,
                color: theme::SYN_NUMBER(),
            });
            continue;
        }

        // Identifier / keyword / type / function call.
        if is_ident_start(c) {
            let start = i;
            i += 1;
            while i < n && is_ident_continue(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // Look ahead past spaces for a '(' → function call.
            let mut j = i;
            while j < n && chars[j] == ' ' {
                j += 1;
            }
            let is_call = j < n && chars[j] == '(';

            let color = if cfg.keywords.contains(&word.as_str()) {
                theme::SYN_KEYWORD()
            } else if cfg.types.contains(&word.as_str())
                || (cfg.pascal_is_type
                    && word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
            {
                theme::SYN_TYPE()
            } else if is_call {
                theme::SYN_FUNCTION()
            } else {
                theme::SYN_DEFAULT()
            };
            spans.push(Span {
                start,
                len: i - start,
                color,
            });
            continue;
        }

        // Punctuation / operator: a run of symbol chars.
        let start = i;
        while i < n
            && !chars[i].is_whitespace()
            && !is_ident_start(chars[i])
            && !chars[i].is_ascii_digit()
            && chars[i] != '"'
            && chars[i] != '\''
            && chars[i] != '`'
        {
            i += 1;
        }
        if i == start {
            i += 1; // ensure progress
        }
        spans.push(Span {
            start,
            len: i - start,
            color: theme::SYN_PUNCT(),
        });
    }

    spans
}

/// True if `chars[i..]` begins with `pat`.
fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    for (k, pc) in (i..).zip(pat.chars()) {
        if k >= chars.len() || chars[k] != pc {
            return false;
        }
    }
    true
}

// Markdown wrapper that prefers the tailored handling, then the generic scanner.
#[allow(dead_code)]
pub fn highlight_markdown_line(line: &str) -> Vec<Span> {
    if let Some(spans) = highlight_markdown(line) {
        return spans;
    }
    highlight_with(line, &config_for(Language::Markdown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_and_type_and_call() {
        let spans = highlight_line("fn add(a: I32) -> I32 {");
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].len, 2);
        assert_eq!(spans[0].color, theme::SYN_KEYWORD());
        let add = spans.iter().find(|s| s.start == 3).unwrap();
        assert_eq!(add.color, theme::SYN_FUNCTION());
        assert!(spans.iter().any(|s| s.color == theme::SYN_TYPE()));
    }

    #[test]
    fn comment_runs_to_eol() {
        let spans = highlight_line("let x = 1 // trailing");
        let c = spans.last().unwrap();
        assert_eq!(c.color, theme::SYN_COMMENT());
    }

    #[test]
    fn string_and_number() {
        let spans = highlight_line(r#"log("hi", 42)"#);
        assert!(spans.iter().any(|s| s.color == theme::SYN_STRING()));
        assert!(spans.iter().any(|s| s.color == theme::SYN_NUMBER()));
    }

    #[test]
    fn empty_line_no_spans() {
        assert!(highlight_line("").is_empty());
        assert!(highlight_line("    ").is_empty());
    }

    // ---- multi-language ----

    #[test]
    fn rust_line_tokens() {
        // `let mut x: u32 = 5; // c` — kw `let`/`mut`, type `u32`, number `5`, comment.
        let spans = highlight_line_lang("let mut x: u32 = 5 // c", Language::Rust);
        assert_eq!(spans[0].color, theme::SYN_KEYWORD(), "let is keyword");
        assert!(
            spans.iter().any(|s| s.color == theme::SYN_TYPE()),
            "u32 is a type"
        );
        assert!(spans.iter().any(|s| s.color == theme::SYN_NUMBER()), "5 is a number");
        assert_eq!(spans.last().unwrap().color, theme::SYN_COMMENT(), "// c is a comment");
    }

    #[test]
    fn python_line_tokens() {
        // `def f(): # hi` — kw `def`, call `f`, `#` comment.
        let spans = highlight_line_lang("def f(): # hi", Language::Python);
        assert_eq!(spans[0].color, theme::SYN_KEYWORD(), "def is keyword");
        assert!(
            spans.iter().any(|s| s.color == theme::SYN_FUNCTION()),
            "f( is a call"
        );
        assert_eq!(
            spans.last().unwrap().color,
            theme::SYN_COMMENT(),
            "# hi is a python comment"
        );
        // A double-slash is NOT a comment in Python.
        let spans2 = highlight_line_lang("x = 1 // 2", Language::Python);
        assert!(
            !spans2.iter().any(|s| s.color == theme::SYN_COMMENT()),
            "// is not a python comment"
        );
    }

    #[test]
    fn python_single_quote_string() {
        let spans = highlight_line_lang("s = 'hello'", Language::Python);
        assert!(spans.iter().any(|s| s.color == theme::SYN_STRING()));
    }

    #[test]
    fn json_string_and_number() {
        let spans = highlight_line_lang(r#"  "key": 42,"#, Language::Json);
        assert!(spans.iter().any(|s| s.color == theme::SYN_STRING()), "key string");
        assert!(spans.iter().any(|s| s.color == theme::SYN_NUMBER()), "42 number");
        // JSON has no single-quote strings.
        let spans2 = highlight_line_lang("'x'", Language::Json);
        assert!(
            !spans2.iter().any(|s| s.color == theme::SYN_STRING()),
            "json has no single-quote strings"
        );
    }

    #[test]
    fn json_true_is_keyword() {
        let spans = highlight_line_lang(r#"  "ok": true"#, Language::Json);
        assert!(spans.iter().any(|s| s.color == theme::SYN_KEYWORD()));
    }

    #[test]
    fn toml_comment_is_hash() {
        let spans = highlight_line_lang("name = \"x\" # comment", Language::Toml);
        assert_eq!(spans.last().unwrap().color, theme::SYN_COMMENT());
    }

    #[test]
    fn js_template_literal() {
        let spans = highlight_line_lang("const s = `hi ${x}`", Language::JavaScript);
        assert_eq!(spans[0].color, theme::SYN_KEYWORD(), "const is keyword");
        assert!(spans.iter().any(|s| s.color == theme::SYN_STRING()), "template literal");
    }

    #[test]
    fn go_keyword_and_type() {
        let spans = highlight_line_lang("func main() string", Language::Go);
        assert_eq!(spans[0].color, theme::SYN_KEYWORD());
        assert!(spans.iter().any(|s| s.color == theme::SYN_TYPE()));
    }

    #[test]
    fn c_block_comment_same_line() {
        let spans = highlight_line_lang("int x = 1; /* note */ y", Language::C);
        assert!(spans.iter().any(|s| s.color == theme::SYN_COMMENT()), "block comment");
        // After the closed block comment, `y` is still colored (default).
        assert!(spans.iter().any(|s| s.color == theme::SYN_DEFAULT() || s.color == theme::SYN_TYPE()));
    }

    #[test]
    fn markdown_heading() {
        let spans = highlight_markdown_line("# Title");
        assert_eq!(spans[0].color, theme::SYN_KEYWORD());
    }

    #[test]
    fn plaintext_has_no_keywords() {
        let spans = highlight_line_lang("fn let if while", Language::PlainText);
        assert!(!spans.iter().any(|s| s.color == theme::SYN_KEYWORD()));
    }
}
