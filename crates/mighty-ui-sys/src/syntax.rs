//! A small, dependency-free syntax highlighter for Mighty source lines.
//!
//! Produces a list of `(start_char, len_chars, color)` spans for one line so
//! the editor body draw can color tokens (keywords, types, strings, comments,
//! numbers, function calls, punctuation). It is intentionally line-local and
//! approximate — good enough for a premium-looking editor without a full
//! parser. Colors come from [`crate::theme`].

use crate::ffi::MuiColor;
use crate::theme;

/// Mighty keywords (control flow, declarations, effects).
const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "while", "if", "else", "return", "match", "struct", "enum", "extern",
    "effect", "import", "pub", "for", "in", "type", "true", "false", "alloc", "use", "const",
    "impl", "as", "self", "Some", "None", "Ok", "Err",
];

/// Primitive / common type names (PascalCase also colored as types).
const TYPES: &[&str] = &[
    "I32", "I64", "U8", "U16", "U32", "U64", "USize", "F32", "F64", "Bool", "Str", "String",
    "Vec", "Option", "Result", "Unit",
];

/// A colored span within a single line: char offset, char length, and color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub color: MuiColor,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Tokenize `line` into colored spans (char-indexed). Whitespace is left
/// uncolored (no span). A `//` comment colors the rest of the line.
pub fn highlight_line(line: &str) -> Vec<Span> {
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

        // Line comment `// ...` to end of line.
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            spans.push(Span {
                start: i,
                len: n - i,
                color: theme::SYN_COMMENT,
            });
            break;
        }

        // String literal "..." (single line; unterminated runs to EOL).
        if c == '"' {
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span {
                start,
                len: i - start,
                color: theme::SYN_STRING,
            });
            continue;
        }

        // Char literal 'x' (kept simple — colored as a string).
        if c == '\'' {
            let start = i;
            i += 1;
            while i < n && chars[i] != '\'' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 1;
                }
                i += 1;
            }
            if i < n {
                i += 1;
            }
            spans.push(Span {
                start,
                len: i - start,
                color: theme::SYN_STRING,
            });
            continue;
        }

        // Number literal (decimal, with optional `_suffix`).
        if c.is_ascii_digit() {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            spans.push(Span {
                start,
                len: i - start,
                color: theme::SYN_NUMBER,
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

            let color = if KEYWORDS.contains(&word.as_str()) {
                theme::SYN_KEYWORD
            } else if TYPES.contains(&word.as_str())
                || word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
            {
                theme::SYN_TYPE
            } else if is_call {
                theme::SYN_FUNCTION
            } else {
                theme::SYN_DEFAULT
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
        while i < n && !chars[i].is_whitespace() && !is_ident_start(chars[i]) && !chars[i].is_ascii_digit()
            && chars[i] != '"' && chars[i] != '\''
        {
            i += 1;
        }
        if i == start {
            i += 1; // ensure progress
        }
        spans.push(Span {
            start,
            len: i - start,
            color: theme::SYN_PUNCT,
        });
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_and_type_and_call() {
        let spans = highlight_line("fn add(a: I32) -> I32 {");
        // First span is the keyword "fn".
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].len, 2);
        assert_eq!(spans[0].color, theme::SYN_KEYWORD);
        // "add" before '(' is a function call.
        let add = spans.iter().find(|s| s.start == 3).unwrap();
        assert_eq!(add.color, theme::SYN_FUNCTION);
        // "I32" is a type.
        assert!(spans.iter().any(|s| s.color == theme::SYN_TYPE));
    }

    #[test]
    fn comment_runs_to_eol() {
        let spans = highlight_line("let x = 1 // trailing");
        let c = spans.last().unwrap();
        assert_eq!(c.color, theme::SYN_COMMENT);
    }

    #[test]
    fn string_and_number() {
        let spans = highlight_line(r#"log("hi", 42)"#);
        assert!(spans.iter().any(|s| s.color == theme::SYN_STRING));
        assert!(spans.iter().any(|s| s.color == theme::SYN_NUMBER));
    }

    #[test]
    fn empty_line_no_spans() {
        assert!(highlight_line("").is_empty());
        assert!(highlight_line("    ").is_empty());
    }
}
