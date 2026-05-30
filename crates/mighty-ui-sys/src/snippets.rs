//! Snippet engine (shim-side): prefix → template expansion with navigable
//! tab-stops.
//!
//! Like every other capability in this IDE, ALL the logic lives shim-side and is
//! driven from the Mighty loop through a scalar `mui_snippet_*` ABI (see
//! [`crate::snippetsabi`]). The editor text model
//! ([`crate::editor::TextModel`]) is the source of truth.
//!
//! ## Snippet definitions
//!
//! A snippet is a `prefix` (the trigger word) plus a `body`. The body is plain
//! text with VS Code-style tab-stop markers:
//!
//!   * `$1`, `$2`, … — ordered tab-stops (the cursor jumps to each in turn).
//!   * `${1:label}` — a tab-stop with placeholder text pre-selected.
//!   * `$0` — the FINAL cursor position (jumped to last; ends the session).
//!
//! Two equal-numbered stops both appear in the session (the first is the primary
//! navigation target; the rest are recorded so a future mirror pass can update
//! them — single-stop is fully supported, mirroring is a nice-to-have).
//!
//! ## Expansion
//!
//! [`expand`] takes the body, the current line's indent, and the cursor's
//! document position, and produces:
//!   * the literal text to insert at the cursor (continuation lines re-indented
//!     to the call site), and
//!   * the resolved tab-stops as absolute `(line, col)` ranges.
//!
//! Everything here is pure + GPU-free so it is exhaustively unit-testable.

use crate::editor::TextModel;
use crate::langdetect::Language;

/// One parsed piece of a snippet body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Literal text (may contain `\n` for multi-line bodies).
    Text(String),
    /// A tab-stop: its number (`0` is the final cursor) and placeholder text
    /// (empty when the body used the bare `$N` form).
    Stop { num: u32, placeholder: String },
}

/// A snippet definition: the trigger prefix + the raw body template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetDef {
    pub prefix: String,
    pub body: String,
    /// A short human label shown in the completion dropdown / docs.
    pub label: String,
}

impl SnippetDef {
    fn new(prefix: &str, label: &str, body: &str) -> Self {
        SnippetDef {
            prefix: prefix.to_string(),
            label: label.to_string(),
            body: body.to_string(),
        }
    }
}

/// Parse a snippet `body` into an ordered list of [`Segment`]s.
///
/// Recognizes `$N`, `${N:placeholder}`, and `$0`. A literal dollar sign is
/// written `\$`. Anything else is literal text (newlines preserved).
pub fn parse_body(body: &str) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    let mut text = String::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let flush = |segs: &mut Vec<Segment>, text: &mut String| {
        if !text.is_empty() {
            segs.push(Segment::Text(std::mem::take(text)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '$' {
            // Escaped dollar -> literal `$`.
            text.push('$');
            i += 2;
            continue;
        }
        if c == '$' && i + 1 < chars.len() {
            // `${N:placeholder}`
            if chars[i + 1] == '{' {
                if let Some((num, placeholder, consumed)) = parse_braced(&chars[i..]) {
                    flush(&mut segs, &mut text);
                    segs.push(Segment::Stop { num, placeholder });
                    i += consumed;
                    continue;
                }
            }
            // `$N`
            if chars[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                let mut n = 0u32;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    n = n.saturating_mul(10).saturating_add(chars[j] as u32 - '0' as u32);
                    j += 1;
                }
                flush(&mut segs, &mut text);
                segs.push(Segment::Stop { num: n, placeholder: String::new() });
                i = j;
                continue;
            }
        }
        text.push(c);
        i += 1;
    }
    flush(&mut segs, &mut text);
    segs
}

/// Parse a `${N:placeholder}` starting at `chars[0] == '$'`. Returns
/// `(num, placeholder, chars_consumed)` or `None` if it isn't well-formed.
fn parse_braced(chars: &[char]) -> Option<(u32, String, usize)> {
    // chars[0]='$', chars[1]='{'
    debug_assert!(chars[0] == '$' && chars.get(1) == Some(&'{'));
    let mut j = 2;
    let mut n = 0u32;
    let start_digits = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(chars[j] as u32 - '0' as u32);
        j += 1;
    }
    if j == start_digits {
        return None; // no digits -> not a tab-stop
    }
    let mut placeholder = String::new();
    if j < chars.len() && chars[j] == ':' {
        j += 1;
        while j < chars.len() && chars[j] != '}' {
            placeholder.push(chars[j]);
            j += 1;
        }
    }
    if j < chars.len() && chars[j] == '}' {
        j += 1;
        Some((n, placeholder, j))
    } else {
        None
    }
}

/// A resolved tab-stop: its number + the absolute selection range in the
/// document `((line,col),(line,col))` (start..end). `$0` (num 0) is the final
/// cursor; placeholder-less stops have a zero-length range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop {
    pub num: u32,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

/// The result of expanding a snippet at a cursor: the literal text to insert and
/// the navigation-ordered tab-stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// Literal text to insert at the cursor (continuation lines already indented).
    pub text: String,
    /// Tab-stops in NAVIGATION order: ascending by number, with `$0` last. Equal
    /// numbers keep body order (mirror candidates).
    pub stops: Vec<Stop>,
}

/// Expand a snippet `body` inserted at document position `(cur_line, cur_col)`,
/// where `indent` is the leading whitespace of the call-site line (continuation
/// lines are prefixed with it). Returns the literal insert text + resolved stops.
///
/// Tab-stop positions are computed by walking the body segments and tracking the
/// running `(line, col)` offset from the insertion point, accounting for the
/// per-line indent added to continuation lines.
pub fn expand(body: &str, indent: &str, cur_line: usize, cur_col: usize) -> Expansion {
    let segs = parse_body(body);
    let indent_chars = indent.chars().count();
    let mut text = String::new();
    let mut stops: Vec<Stop> = Vec::new();

    // Track the cursor as we emit text. `line` is the absolute document line,
    // `col` the absolute char column. The first body line continues from
    // `cur_col`; later lines start at `indent_chars` (the indent we prepend).
    let mut line = cur_line;
    let mut col = cur_col;

    // Emit a literal string, re-indenting after each newline, updating line/col.
    let emit = |s: &str, text: &mut String, line: &mut usize, col: &mut usize| {
        for ch in s.chars() {
            if ch == '\n' {
                text.push('\n');
                text.push_str(indent);
                *line += 1;
                *col = indent_chars;
            } else {
                text.push(ch);
                *col += 1;
            }
        }
    };

    for seg in &segs {
        match seg {
            Segment::Text(s) => emit(s, &mut text, &mut line, &mut col),
            Segment::Stop { num, placeholder } => {
                let start = (line, col);
                emit(placeholder, &mut text, &mut line, &mut col);
                let end = (line, col);
                stops.push(Stop { num: *num, start, end });
            }
        }
    }

    // Navigation order: ascending number, but $0 (final cursor) goes LAST.
    // Stable so equal-numbered stops keep body order.
    stops.sort_by_key(|s| if s.num == 0 { u32::MAX } else { s.num });
    Expansion { text, stops }
}

/// An active tab-stop navigation session over an expanded snippet.
///
/// Holds the ordered stops + the index of the current stop. The model's cursor /
/// selection is driven to the current stop by the ABI; navigation just advances
/// or rewinds the index. Reaching past the last stop ends the session.
#[derive(Debug, Clone, Default)]
pub struct SnippetSession {
    stops: Vec<Stop>,
    /// Index of the current stop (`0..stops.len()`); `stops.len()` means done.
    cur: usize,
    active: bool,
}

impl SnippetSession {
    pub fn new() -> Self {
        SnippetSession::default()
    }

    /// Begin a session over `stops`. Inactive (no-op) when there are fewer than
    /// two stops AND the single stop is `$0` only — i.e. nothing to navigate; in
    /// that common "one placeholder + final" case we still activate so the first
    /// placeholder is selected and Tab jumps to the end.
    pub fn begin(&mut self, stops: Vec<Stop>) -> bool {
        // Drop nothing — but only activate if there's at least one stop to land on.
        if stops.is_empty() {
            self.active = false;
            return false;
        }
        self.stops = stops;
        self.cur = 0;
        self.active = true;
        true
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The current stop, or `None` when inactive / past the end.
    pub fn current(&self) -> Option<Stop> {
        if !self.active {
            return None;
        }
        self.stops.get(self.cur).copied()
    }

    /// Advance to the next stop. Returns the new current stop, or `None` (and ends
    /// the session) when the last stop has been passed.
    pub fn next_stop(&mut self) -> Option<Stop> {
        if !self.active {
            return None;
        }
        if self.cur + 1 >= self.stops.len() {
            // Past the final stop -> session over.
            self.cur = self.stops.len();
            self.active = false;
            return None;
        }
        self.cur += 1;
        self.stops.get(self.cur).copied()
    }

    /// Step back to the previous stop. Returns the new current stop (clamped at
    /// the first). No-op at the first stop.
    pub fn prev_stop(&mut self) -> Option<Stop> {
        if !self.active {
            return None;
        }
        if self.cur > 0 {
            self.cur -= 1;
        }
        self.stops.get(self.cur).copied()
    }

    /// End the session (Esc / typing past the end / cursor leaving the region).
    pub fn cancel(&mut self) {
        self.active = false;
        self.stops.clear();
        self.cur = 0;
    }
}

// ===========================================================================
// Built-in + user snippet sets
// ===========================================================================

/// The built-in Mighty snippets (REAL Mighty syntax — see `examples/`).
pub fn mighty_snippets() -> Vec<SnippetDef> {
    vec![
        SnippetDef::new(
            "fn",
            "function",
            "fn ${1:name}(${2:args}) -> ${3:I32} {\n  $0\n}",
        ),
        SnippetDef::new(
            "struct",
            "struct",
            "struct ${1:Name} {\n  ${2:field}: ${3:I32},\n}$0",
        ),
        SnippetDef::new(
            "enum",
            "enum",
            "enum ${1:Name} {\n  ${2:Variant},\n}$0",
        ),
        SnippetDef::new(
            "agent",
            "agent",
            "agent ${1:Name}: ${2:Protocol} {\n  on ${3:Msg}(${4:arg}) -> {\n    $0\n  }\n}",
        ),
        SnippetDef::new(
            "protocol",
            "protocol",
            "protocol ${1:Name} {\n  ${2:Msg}(${3:arg}: ${4:Str}) -> ${5:U8}\n}$0",
        ),
        SnippetDef::new(
            "match",
            "match",
            "match ${1:value} {\n  ${2:pattern} -> $0\n}",
        ),
        SnippetDef::new("if", "if", "if ${1:cond} {\n  $0\n}"),
        SnippetDef::new(
            "ifelse",
            "if / else",
            "if ${1:cond} {\n  $2\n} else {\n  $0\n}",
        ),
        SnippetDef::new("for", "for", "for ${1:i} in ${2:0..n} {\n  $0\n}"),
        SnippetDef::new("while", "while", "while ${1:cond} {\n  $0\n}"),
        SnippetDef::new("let", "let", "let ${1:name} = ${0:value}"),
        SnippetDef::new(
            "test",
            "test function",
            "fn test_${1:name}() -> I32 {\n  assert_eq(${2:actual}, ${3:expected})\n  $0\n}",
        ),
        SnippetDef::new("log", "log", "log(${0:\"message\"})"),
        SnippetDef::new("main", "main", "fn main() {\n  $0\n}"),
    ]
}

/// Cheap language-agnostic snippets for non-Mighty files (keyed by language).
pub fn generic_snippets(lang: Language) -> Vec<SnippetDef> {
    match lang {
        Language::Rust => vec![
            SnippetDef::new("fn", "function", "fn ${1:name}(${2:args}) -> ${3:()} {\n    $0\n}"),
            SnippetDef::new("struct", "struct", "struct ${1:Name} {\n    ${2:field}: ${3:T},\n}$0"),
            SnippetDef::new("if", "if", "if ${1:cond} {\n    $0\n}"),
            SnippetDef::new("for", "for", "for ${1:i} in ${2:iter} {\n    $0\n}"),
            SnippetDef::new("match", "match", "match ${1:value} {\n    ${2:pat} => $0,\n}"),
            SnippetDef::new("test", "test", "#[test]\nfn ${1:name}() {\n    assert_eq!(${2:a}, ${3:b});\n    $0\n}"),
        ],
        Language::Python => vec![
            SnippetDef::new("def", "def", "def ${1:name}(${2:args}):\n    $0"),
            SnippetDef::new("class", "class", "class ${1:Name}:\n    def __init__(self${2:, args}):\n        $0"),
            SnippetDef::new("if", "if", "if ${1:cond}:\n    $0"),
            SnippetDef::new("for", "for", "for ${1:item} in ${2:iterable}:\n    $0"),
            SnippetDef::new("while", "while", "while ${1:cond}:\n    $0"),
        ],
        Language::JavaScript | Language::TypeScript => vec![
            SnippetDef::new("fn", "function", "function ${1:name}(${2:args}) {\n  $0\n}"),
            SnippetDef::new("if", "if", "if (${1:cond}) {\n  $0\n}"),
            SnippetDef::new("for", "for", "for (let ${1:i} = 0; ${1:i} < ${2:n}; ${1:i}++) {\n  $0\n}"),
            SnippetDef::new("log", "console.log", "console.log($0)"),
        ],
        Language::Go => vec![
            SnippetDef::new("fn", "func", "func ${1:name}(${2:args}) ${3:error} {\n\t$0\n}"),
            SnippetDef::new("if", "if", "if ${1:cond} {\n\t$0\n}"),
            SnippetDef::new("for", "for", "for ${1:i} := 0; ${1:i} < ${2:n}; ${1:i}++ {\n\t$0\n}"),
        ],
        _ => Vec::new(),
    }
}

/// The active snippet set for `lang`: Mighty's rich set for Mighty files, else a
/// language-agnostic set, plus any user-defined snippets loaded from config.
pub fn snippets_for(lang: Language) -> Vec<SnippetDef> {
    let mut defs = if lang == Language::Mighty {
        mighty_snippets()
    } else {
        generic_snippets(lang)
    };
    // User snippets override / extend the built-ins (same-prefix wins last).
    let user = load_user_snippets();
    for u in user {
        if let Some(existing) = defs.iter_mut().find(|d| d.prefix == u.prefix) {
            *existing = u;
        } else {
            defs.push(u);
        }
    }
    defs
}

/// Find the snippet whose prefix exactly equals `word` for `lang`, if any.
pub fn find_snippet(lang: Language, word: &str) -> Option<SnippetDef> {
    if word.is_empty() {
        return None;
    }
    snippets_for(lang).into_iter().find(|d| d.prefix == word)
}

// ---------------------------------------------------------------------------
// User snippets (optional): loaded from a tiny config file
// ---------------------------------------------------------------------------

/// Path to the user snippet file (same dir as the IDE config): `snippets`.
/// Format: one snippet per stanza, `prefix<TAB>label<TAB>body` with literal `\n`
/// in the body for newlines. Blank lines and `#` comments are ignored.
fn user_snippets_path() -> Option<std::path::PathBuf> {
    crate::config::config_path().and_then(|p| p.parent().map(|d| d.join("snippets")))
}

/// Parse the user-snippet blob: each non-comment line is
/// `prefix\tlabel\tbody` (body uses `\n` for newlines, `\t` for tabs).
pub fn parse_user_snippets(text: &str) -> Vec<SnippetDef> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let prefix = parts[0].trim();
        if prefix.is_empty() {
            continue;
        }
        let body = parts[2].replace("\\n", "\n").replace("\\t", "\t");
        out.push(SnippetDef::new(prefix, parts[1].trim(), &body));
    }
    out
}

/// Load user snippets from the config file (best-effort; empty on any error).
pub fn load_user_snippets() -> Vec<SnippetDef> {
    let Some(path) = user_snippets_path() else {
        return Vec::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_user_snippets(&text),
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Driving the model: expand at the cursor + drive a session
// ---------------------------------------------------------------------------

/// Try to expand the snippet whose prefix is the identifier-word immediately
/// before the model's cursor, for `lang`. On success: deletes the prefix word,
/// inserts the expanded body (indented to the call-site line), begins the
/// `session`, selects the first tab-stop's placeholder, and returns `true`.
/// Returns `false` (model untouched) when there's no snippet for the word.
pub fn try_expand(model: &mut TextModel, session: &mut SnippetSession, lang: Language) -> bool {
    let line = model.cursor_line();
    let col = model.cursor_col();
    let word = prefix_word(model.line(line), col);
    let Some(def) = find_snippet(lang, &word) else {
        return false;
    };
    let prefix_len = word.chars().count();
    // The call-site indent = leading whitespace of the current line.
    let indent: String = model
        .line(line)
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    // Delete the typed prefix word (cursor sits just after it).
    for _ in 0..prefix_len {
        model.backspace();
    }
    let (cl, cc) = (model.cursor_line(), model.cursor_col());
    let exp = expand(&def.body, &indent, cl, cc);
    for ch in exp.text.chars() {
        model.insert_char(ch);
    }
    // Begin the navigation session + select the first stop.
    if session.begin(exp.stops) {
        if let Some(stop) = session.current() {
            model.set_selection(stop.start, stop.end);
        }
    } else {
        // No stops at all (shouldn't happen for our bodies) — leave cursor at end.
    }
    true
}

/// The identifier word ending at char column `col` on `line` (the snippet prefix
/// candidate). Empty if the char before the cursor isn't an identifier char.
pub fn prefix_word(line: &str, col: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    let end = col.min(chars.len());
    let mut start = end;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    chars[start..end].iter().collect()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- body parsing ----

    #[test]
    fn parse_plain_text() {
        assert_eq!(parse_body("hello"), vec![Segment::Text("hello".into())]);
    }

    #[test]
    fn parse_numbered_stops() {
        let segs = parse_body("a$1b$2c$0");
        assert_eq!(
            segs,
            vec![
                Segment::Text("a".into()),
                Segment::Stop { num: 1, placeholder: String::new() },
                Segment::Text("b".into()),
                Segment::Stop { num: 2, placeholder: String::new() },
                Segment::Text("c".into()),
                Segment::Stop { num: 0, placeholder: String::new() },
            ]
        );
    }

    #[test]
    fn parse_placeholders() {
        let segs = parse_body("fn ${1:name}(${2:args})");
        assert_eq!(
            segs,
            vec![
                Segment::Text("fn ".into()),
                Segment::Stop { num: 1, placeholder: "name".into() },
                Segment::Text("(".into()),
                Segment::Stop { num: 2, placeholder: "args".into() },
                Segment::Text(")".into()),
            ]
        );
    }

    #[test]
    fn parse_escaped_dollar_is_literal() {
        assert_eq!(parse_body("cost \\$5"), vec![Segment::Text("cost $5".into())]);
    }

    #[test]
    fn parse_multidigit_stop() {
        let segs = parse_body("$10");
        assert_eq!(segs, vec![Segment::Stop { num: 10, placeholder: String::new() }]);
    }

    #[test]
    fn parse_braced_without_placeholder() {
        // `${3}` form (digits, no colon) is a bare stop.
        let segs = parse_body("x${3}y");
        assert_eq!(
            segs,
            vec![
                Segment::Text("x".into()),
                Segment::Stop { num: 3, placeholder: String::new() },
                Segment::Text("y".into()),
            ]
        );
    }

    // ---- expansion + indentation ----

    #[test]
    fn expand_single_line_stops_positions() {
        // "let $1 = $0" at line 0, col 0, no indent.
        let exp = expand("let ${1:name} = ${0:value}", "", 0, 0);
        assert_eq!(exp.text, "let name = value");
        // $1 selects "name" at cols 4..8; $0 is "value" at 11..16, ordered last.
        assert_eq!(exp.stops.len(), 2);
        assert_eq!(exp.stops[0], Stop { num: 1, start: (0, 4), end: (0, 8) });
        assert_eq!(exp.stops[1], Stop { num: 0, start: (0, 11), end: (0, 16) });
    }

    #[test]
    fn expand_multiline_indents_continuations() {
        // Called at line 2, col 4 (inside a 4-space indent).
        let exp = expand("if ${1:cond} {\n  $0\n}", "    ", 2, 4);
        // Continuation lines get the call-site indent prepended.
        assert_eq!(exp.text, "if cond {\n      \n    }");
        // $1 = "cond" on line 2 cols 7..11.
        assert_eq!(exp.stops[0], Stop { num: 1, start: (2, 7), end: (2, 11) });
        // $0 = zero-length on line 3; col = indent(4) + body "  " (2) = 6.
        assert_eq!(exp.stops[1], Stop { num: 0, start: (3, 6), end: (3, 6) });
    }

    #[test]
    fn expand_orders_zero_last_and_numbers_ascending() {
        let exp = expand("$2 $1 $0 $3", "", 0, 0);
        let nums: Vec<u32> = exp.stops.iter().map(|s| s.num).collect();
        assert_eq!(nums, vec![1, 2, 3, 0]);
    }

    // ---- session navigation ----

    #[test]
    fn session_navigates_next_prev_and_ends() {
        let stops = vec![
            Stop { num: 1, start: (0, 0), end: (0, 1) },
            Stop { num: 2, start: (0, 2), end: (0, 3) },
            Stop { num: 0, start: (0, 4), end: (0, 4) },
        ];
        let mut s = SnippetSession::new();
        assert!(s.begin(stops));
        assert_eq!(s.current().unwrap().num, 1);
        assert_eq!(s.next_stop().unwrap().num, 2);
        assert_eq!(s.next_stop().unwrap().num, 0); // $0 last
        // Past the last stop -> session ends.
        assert_eq!(s.next_stop(), None);
        assert!(!s.is_active());
    }

    #[test]
    fn session_prev_clamps_at_first() {
        let stops = vec![
            Stop { num: 1, start: (0, 0), end: (0, 1) },
            Stop { num: 0, start: (0, 2), end: (0, 2) },
        ];
        let mut s = SnippetSession::new();
        s.begin(stops);
        s.next_stop(); // now at $0
        assert_eq!(s.prev_stop().unwrap().num, 1);
        // Prev at the first stays at the first.
        assert_eq!(s.prev_stop().unwrap().num, 1);
    }

    #[test]
    fn session_cancel_deactivates() {
        let mut s = SnippetSession::new();
        s.begin(vec![Stop { num: 1, start: (0, 0), end: (0, 1) }]);
        assert!(s.is_active());
        s.cancel();
        assert!(!s.is_active());
        assert_eq!(s.current(), None);
    }

    // ---- prefix word at cursor ----

    #[test]
    fn prefix_word_reads_trigger() {
        assert_eq!(prefix_word("  fn", 4), "fn");
        assert_eq!(prefix_word("let x", 5), "x");
        assert_eq!(prefix_word("a.fn", 4), "fn"); // stops at the dot
        assert_eq!(prefix_word("fn ", 3), ""); // space before cursor
    }

    // ---- built-in set sanity ----

    #[test]
    fn mighty_snippets_present_and_valid() {
        let defs = mighty_snippets();
        let prefixes: Vec<&str> = defs.iter().map(|d| d.prefix.as_str()).collect();
        for want in ["fn", "struct", "enum", "agent", "protocol", "match", "if", "ifelse", "for", "while", "let", "test", "log", "main"] {
            assert!(prefixes.contains(&want), "missing snippet `{want}`");
        }
        // Every body must parse to at least one stop (so a session can begin).
        for d in &defs {
            let exp = expand(&d.body, "", 0, 0);
            assert!(!exp.stops.is_empty(), "`{}` has no tab-stops", d.prefix);
        }
    }

    #[test]
    fn find_snippet_exact_match_only() {
        assert!(find_snippet(Language::Mighty, "fn").is_some());
        assert!(find_snippet(Language::Mighty, "fnx").is_none());
        assert!(find_snippet(Language::Mighty, "").is_none());
        // Python gets generic set, not Mighty's.
        assert!(find_snippet(Language::Python, "def").is_some());
        assert!(find_snippet(Language::Python, "agent").is_none());
    }

    // ---- user snippet parsing ----

    #[test]
    fn parse_user_snippets_tab_separated() {
        let blob = "# my snippets\nguard\tguard clause\tif ${1:cond} {\\n  return\\n}$0\n\nbad line\n";
        let defs = parse_user_snippets(blob);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].prefix, "guard");
        assert_eq!(defs[0].label, "guard clause");
        assert!(defs[0].body.contains('\n'));
    }

    // ---- end-to-end expansion against the editor model ----

    #[test]
    fn try_expand_inserts_and_selects_first_stop() {
        let mut m = TextModel::from_bytes(b"  fn");
        m.move_to(0, 4); // cursor after "fn"
        let mut s = SnippetSession::new();
        assert!(try_expand(&mut m, &mut s, Language::Mighty));
        // The prefix "fn" was replaced by the expanded body, indented to "  ".
        assert_eq!(m.line(0), "  fn name(args) -> I32 {");
        assert_eq!(m.line(1), "    "); // 2-space call indent + 2-space body
        assert_eq!(m.line(2), "  }");
        // First stop ($1 = "name") is selected.
        assert!(s.is_active());
        assert_eq!(s.current().unwrap().num, 1);
        assert_eq!(m.selected_text(), "name");
    }

    #[test]
    fn try_expand_no_match_leaves_model() {
        let mut m = TextModel::from_bytes(b"zzz");
        m.move_to(0, 3);
        let mut s = SnippetSession::new();
        assert!(!try_expand(&mut m, &mut s, Language::Mighty));
        assert_eq!(m.line(0), "zzz");
        assert!(!s.is_active());
    }
}
