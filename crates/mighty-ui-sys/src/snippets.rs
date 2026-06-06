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
//! Two equal-numbered stops share one navigation target: the first is the
//! editable primary placeholder; later equal stops mirror its text.
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
use regex::RegexBuilder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// One parsed piece of a snippet body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Literal text (may contain `\n` for multi-line bodies).
    Text(String),
    /// A VS Code-style snippet variable such as `$TM_FILENAME` or
    /// `${TM_FILENAME_BASE:default}`.
    Variable {
        name: String,
        default: Option<String>,
        braced: bool,
    },
    /// A focused VS Code-style variable transform, such as
    /// `${TM_FILENAME_BASE/(.*)/${1:/pascalcase}/}`.
    VariableTransform {
        name: String,
        pattern: String,
        format: String,
        options: String,
    },
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
    scope: Vec<Language>,
}

impl SnippetDef {
    fn new(prefix: &str, label: &str, body: &str) -> Self {
        SnippetDef {
            prefix: prefix.to_string(),
            label: label.to_string(),
            body: body.to_string(),
            scope: Vec::new(),
        }
    }

    fn with_scope(mut self, scope: Vec<Language>) -> Self {
        self.scope = scope;
        self
    }

    fn applies_to(&self, lang: Language) -> bool {
        self.scope.is_empty() || self.scope.contains(&lang)
    }
}

/// Parse a snippet `body` into an ordered list of [`Segment`]s.
///
/// Recognizes `$N`, `${N:placeholder}`, `${N|one,two|}`, `$0`, and snippet
/// variables such as `$TM_FILENAME` / `${TM_FILENAME_BASE:default}`. Nested
/// placeholder defaults are flattened into the outer placeholder text. A literal
/// dollar sign is written `\$`. Anything else is literal text (newlines
/// preserved).
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
            // `${N:placeholder}` / `${NAME:default}`
            if chars[i + 1] == '{' {
                if let Some((num, placeholder, consumed)) = parse_braced(&chars[i..]) {
                    flush(&mut segs, &mut text);
                    segs.push(Segment::Stop { num, placeholder });
                    i += consumed;
                    continue;
                }
                if let Some((name, pattern, format, options, consumed)) =
                    parse_braced_variable_transform(&chars[i..])
                {
                    flush(&mut segs, &mut text);
                    segs.push(Segment::VariableTransform {
                        name,
                        pattern,
                        format,
                        options,
                    });
                    i += consumed;
                    continue;
                }
                if let Some((name, default, consumed)) = parse_braced_variable(&chars[i..]) {
                    flush(&mut segs, &mut text);
                    segs.push(Segment::Variable {
                        name,
                        default,
                        braced: true,
                    });
                    i += consumed;
                    continue;
                }
            }
            // `$N`
            if chars[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                let mut n = 0u32;
                let mut overflowed = false;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    let digit = chars[j] as u32 - '0' as u32;
                    if let Some(next) = n.checked_mul(10).and_then(|v| v.checked_add(digit)) {
                        n = next;
                    } else {
                        overflowed = true;
                    }
                    j += 1;
                }
                if overflowed {
                    text.push(c);
                    i += 1;
                    continue;
                }
                flush(&mut segs, &mut text);
                segs.push(Segment::Stop {
                    num: n,
                    placeholder: String::new(),
                });
                i = j;
                continue;
            }
            if is_variable_start(chars[i + 1]) {
                let mut j = i + 2;
                while j < chars.len() && is_variable_char(chars[j]) {
                    j += 1;
                }
                flush(&mut segs, &mut text);
                segs.push(Segment::Variable {
                    name: chars[i + 1..j].iter().collect(),
                    default: None,
                    braced: false,
                });
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
        let digit = chars[j] as u32 - '0' as u32;
        n = n.checked_mul(10)?.checked_add(digit)?;
        j += 1;
    }
    if j == start_digits {
        return None; // no digits -> not a tab-stop
    }
    if j < chars.len() && chars[j] == ':' {
        j += 1;
        let (placeholder, consumed) = parse_placeholder_text(&chars[j..])?;
        j += consumed;
        return Some((n, placeholder, j));
    }
    if j < chars.len() && chars[j] == '|' {
        j += 1;
        let (placeholder, consumed) = parse_choice_text(&chars[j..])?;
        j += consumed;
        return Some((n, placeholder, j));
    }
    if j < chars.len() && chars[j] == '}' {
        j += 1;
        Some((n, String::new(), j))
    } else {
        None
    }
}

/// Parse a braced variable starting at `chars[0] == '$'`, such as
/// `${TM_FILENAME}` or `${TM_FILENAME_BASE:default}`.
fn parse_braced_variable(chars: &[char]) -> Option<(String, Option<String>, usize)> {
    debug_assert!(chars[0] == '$' && chars.get(1) == Some(&'{'));
    let mut j = 2;
    if j >= chars.len() || !is_variable_start(chars[j]) {
        return None;
    }
    j += 1;
    while j < chars.len() && is_variable_char(chars[j]) {
        j += 1;
    }
    let name: String = chars[2..j].iter().collect();
    if j < chars.len() && chars[j] == ':' {
        j += 1;
        let (default, consumed) = parse_placeholder_text(&chars[j..])?;
        j += consumed;
        return Some((name, Some(default), j));
    }
    if j < chars.len() && chars[j] == '}' {
        j += 1;
        Some((name, None, j))
    } else {
        None
    }
}

fn parse_braced_variable_transform(
    chars: &[char],
) -> Option<(String, String, String, String, usize)> {
    debug_assert!(chars[0] == '$' && chars.get(1) == Some(&'{'));
    let mut j = 2;
    if j >= chars.len() || !is_variable_start(chars[j]) {
        return None;
    }
    j += 1;
    while j < chars.len() && is_variable_char(chars[j]) {
        j += 1;
    }
    let name: String = chars[2..j].iter().collect();
    if chars.get(j) != Some(&'/') {
        return None;
    }
    j += 1;
    let (pattern, consumed) = parse_transform_section(&chars[j..], false)?;
    j += consumed;
    let (format, consumed) = parse_transform_section(&chars[j..], true)?;
    j += consumed;
    let option_start = j;
    while j < chars.len() && chars[j] != '}' {
        j += 1;
    }
    if chars.get(j) != Some(&'}') {
        return None;
    }
    let options: String = chars[option_start..j].iter().collect();
    Some((name, pattern, format, options, j + 1))
}

fn parse_transform_section(chars: &[char], allow_braced_format: bool) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut j = 0;
    let mut braced_depth = 0usize;
    while j < chars.len() {
        if chars[j] == '\\' && j + 1 < chars.len() {
            if chars[j + 1] == '/' {
                out.push('/');
            } else {
                out.push(chars[j]);
                out.push(chars[j + 1]);
            }
            j += 2;
            continue;
        }
        if allow_braced_format && chars[j] == '$' && chars.get(j + 1) == Some(&'{') {
            braced_depth = braced_depth.saturating_add(1);
            out.push(chars[j]);
            out.push(chars[j + 1]);
            j += 2;
            continue;
        }
        if allow_braced_format && chars[j] == '}' && braced_depth > 0 {
            braced_depth -= 1;
            out.push(chars[j]);
            j += 1;
            continue;
        }
        if chars[j] == '/' && braced_depth == 0 {
            return Some((out, j + 1));
        }
        out.push(chars[j]);
        j += 1;
    }
    None
}

fn parse_placeholder_text(chars: &[char]) -> Option<(String, usize)> {
    let mut text = String::new();
    let mut j = 0;
    while j < chars.len() {
        match chars[j] {
            '}' => return Some((text, j + 1)),
            '$' if chars.get(j + 1) == Some(&'{') => {
                if let Some((_, placeholder, consumed)) = parse_braced(&chars[j..]) {
                    text.push_str(&placeholder);
                    j += consumed;
                } else if let Some((_, _, _, _, consumed)) =
                    parse_braced_variable_transform(&chars[j..])
                {
                    text.extend(chars[j..j + consumed].iter());
                    j += consumed;
                } else if let Some((_, _, consumed)) = parse_braced_variable(&chars[j..]) {
                    text.extend(chars[j..j + consumed].iter());
                    j += consumed;
                } else {
                    text.push(chars[j]);
                    j += 1;
                }
            }
            '$' if j + 1 < chars.len() && chars[j + 1].is_ascii_digit() => {
                let mut k = j + 1;
                while k < chars.len() && chars[k].is_ascii_digit() {
                    k += 1;
                }
                j = k;
            }
            '\\' if j + 1 < chars.len() && matches!(chars[j + 1], '}' | '$' | '\\') => {
                text.push(chars[j + 1]);
                j += 2;
            }
            ch => {
                text.push(ch);
                j += 1;
            }
        }
    }
    None
}

fn parse_choice_text(chars: &[char]) -> Option<(String, usize)> {
    let mut choices = vec![String::new()];
    let mut j = 0;
    while j < chars.len() {
        match chars[j] {
            '|' if chars.get(j + 1) == Some(&'}') => {
                let first = choices.into_iter().next().unwrap_or_default();
                return Some((first, j + 2));
            }
            ',' => {
                choices.push(String::new());
                j += 1;
            }
            '\\' if j + 1 < chars.len() && matches!(chars[j + 1], ',' | '|' | '\\') => {
                choices.last_mut().unwrap().push(chars[j + 1]);
                j += 2;
            }
            ch => {
                choices.last_mut().unwrap().push(ch);
                j += 1;
            }
        }
    }
    None
}

fn is_variable_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_variable_char(ch: char) -> bool {
    is_variable_start(ch) || ch.is_ascii_digit()
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetContext {
    active_path: Option<PathBuf>,
    selected_text: String,
    clipboard_text: Option<String>,
    workspace_root: Option<PathBuf>,
    current_line: Option<String>,
    line_index: Option<usize>,
    current_word: String,
    date: Option<DateParts>,
    language: Option<Language>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateParts {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u16,
    weekday: u8,
    unix_millis: i64,
}

impl DateParts {
    fn local_now() -> Self {
        local_date_parts()
    }
}

impl SnippetContext {
    #[allow(dead_code)]
    pub fn from_path(path: Option<&Path>) -> Self {
        SnippetContext::from_path_selection_and_workspace(path, "", None)
    }

    #[allow(dead_code)]
    pub fn from_path_and_selection(path: Option<&Path>, selected_text: &str) -> Self {
        SnippetContext::from_path_selection_and_workspace(path, selected_text, None)
    }

    #[allow(dead_code)]
    pub fn from_path_selection_and_workspace(
        path: Option<&Path>,
        selected_text: &str,
        workspace_root: Option<&Path>,
    ) -> Self {
        SnippetContext::from_editor_context(
            path,
            selected_text,
            workspace_root,
            None,
            None,
            "",
            None,
        )
    }

    pub fn from_editor_context(
        path: Option<&Path>,
        selected_text: &str,
        workspace_root: Option<&Path>,
        current_line: Option<&str>,
        line_index: Option<usize>,
        current_word: &str,
        date: Option<DateParts>,
    ) -> Self {
        SnippetContext {
            active_path: path.map(Path::to_path_buf),
            selected_text: selected_text.to_string(),
            clipboard_text: None,
            workspace_root: workspace_root.map(Path::to_path_buf),
            current_line: current_line.map(str::to_string),
            line_index,
            current_word: current_word.to_string(),
            date,
            language: None,
        }
    }

    pub fn from_editor_context_with_language(
        path: Option<&Path>,
        selected_text: &str,
        workspace_root: Option<&Path>,
        current_line: Option<&str>,
        line_index: Option<usize>,
        current_word: &str,
        date: Option<DateParts>,
        language: Language,
    ) -> Self {
        let mut context = SnippetContext::from_editor_context(
            path,
            selected_text,
            workspace_root,
            current_line,
            line_index,
            current_word,
            date,
        );
        context.language = Some(language);
        context
    }

    pub fn with_clipboard_text(mut self, clipboard_text: Option<&str>) -> Self {
        self.clipboard_text = clipboard_text.map(str::to_string);
        self
    }
}

/// Expand a snippet `body` inserted at document position `(cur_line, cur_col)`,
/// where `indent` is the leading whitespace of the call-site line (continuation
/// lines are prefixed with it). Returns the literal insert text + resolved stops.
///
/// Tab-stop positions are computed by walking the body segments and tracking the
/// running `(line, col)` offset from the insertion point, accounting for the
/// per-line indent added to continuation lines.
#[allow(dead_code)]
pub fn expand(body: &str, indent: &str, cur_line: usize, cur_col: usize) -> Expansion {
    expand_with_context(body, indent, cur_line, cur_col, &SnippetContext::default())
}

pub fn expand_with_context(
    body: &str,
    indent: &str,
    cur_line: usize,
    cur_col: usize,
    context: &SnippetContext,
) -> Expansion {
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
            Segment::Variable {
                name,
                default,
                braced,
            } => {
                let value = resolve_variable_with_default(name, default.as_deref(), context)
                    .unwrap_or_else(|| unresolved_variable_literal(name, *braced));
                emit(&value, &mut text, &mut line, &mut col);
            }
            Segment::VariableTransform {
                name,
                pattern,
                format,
                options,
            } => {
                let value = resolve_snippet_variable(name, context)
                    .map(|value| apply_variable_transform(&value, pattern, format, options))
                    .unwrap_or_else(|| unresolved_variable_literal(name, true));
                emit(&value, &mut text, &mut line, &mut col);
            }
            Segment::Stop { num, placeholder } => {
                let start = (line, col);
                let placeholder = resolve_variables_in_text(placeholder, context);
                emit(&placeholder, &mut text, &mut line, &mut col);
                let end = (line, col);
                stops.push(Stop {
                    num: *num,
                    start,
                    end,
                });
            }
        }
    }

    // Navigation order: ascending number, but $0 (final cursor) goes LAST.
    // Stable so equal-numbered stops keep body order.
    stops.sort_by_key(|s| if s.num == 0 { u32::MAX } else { s.num });
    Expansion { text, stops }
}

fn resolve_variables_in_text(text: &str, context: &SnippetContext) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            if let Some((name, pattern, format, options, consumed)) =
                parse_braced_variable_transform(&chars[i..])
            {
                if let Some(value) = resolve_snippet_variable(&name, context) {
                    out.push_str(&apply_variable_transform(
                        &value, &pattern, &format, &options,
                    ));
                } else {
                    out.push_str(&unresolved_variable_literal(&name, true));
                }
                i += consumed;
                continue;
            }
            if let Some((name, default, consumed)) = parse_braced_variable(&chars[i..]) {
                if let Some(value) =
                    resolve_variable_with_default(&name, default.as_deref(), context)
                {
                    out.push_str(&value);
                } else {
                    out.push_str(&unresolved_variable_literal(&name, true));
                }
                i += consumed;
                continue;
            }
        }
        if chars[i] == '$' && i + 1 < chars.len() && is_variable_start(chars[i + 1]) {
            let mut j = i + 2;
            while j < chars.len() && is_variable_char(chars[j]) {
                j += 1;
            }
            let name: String = chars[i + 1..j].iter().collect();
            if let Some(value) = resolve_snippet_variable(&name, context) {
                out.push_str(&value);
            } else {
                out.extend(chars[i..j].iter());
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn apply_variable_modifier(value: &str, modifier: &str) -> String {
    match modifier {
        "upcase" => value.to_uppercase(),
        "downcase" => value.to_lowercase(),
        "capitalize" => capitalize(value),
        "camelcase" => camel_case(value, false),
        "pascalcase" => camel_case(value, true),
        _ => value.to_string(),
    }
}

fn apply_variable_transform(value: &str, pattern: &str, format: &str, options: &str) -> String {
    let mut builder = RegexBuilder::new(pattern);
    builder.case_insensitive(options.contains('i'));
    builder.multi_line(options.contains('m'));
    builder.dot_matches_new_line(options.contains('s'));
    let Ok(regex) = builder.build() else {
        return value.to_string();
    };
    let apply = |caps: &regex::Captures<'_>| expand_transform_format(format, caps);
    if options.contains('g') {
        regex.replace_all(value, apply).into_owned()
    } else {
        regex.replace(value, apply).into_owned()
    }
}

fn expand_transform_format(format: &str, caps: &regex::Captures<'_>) -> String {
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if chars[i] == '$' {
            if chars.get(i + 1).is_some_and(|ch| ch.is_ascii_digit()) {
                let mut j = i + 1;
                let mut n = Some(0usize);
                while j < chars.len() && chars[j].is_ascii_digit() {
                    let digit = chars[j] as usize - '0' as usize;
                    n = n.and_then(|v| v.checked_mul(10).and_then(|v| v.checked_add(digit)));
                    j += 1;
                }
                out.push_str(
                    n.and_then(|n| caps.get(n))
                        .map(|m| m.as_str())
                        .unwrap_or(""),
                );
                i = j;
                continue;
            }
            if chars.get(i + 1) == Some(&'{') {
                if let Some((capture, format, consumed)) = parse_transform_capture(&chars[i..]) {
                    let value = caps.get(capture).map(|m| m.as_str()).unwrap_or("");
                    match format {
                        TransformCaptureFormat::Plain => out.push_str(value),
                        TransformCaptureFormat::Modifier(modifier) => {
                            out.push_str(&apply_variable_modifier(value, &modifier))
                        }
                        TransformCaptureFormat::IfPresent(text) => {
                            if !value.is_empty() {
                                out.push_str(&text);
                            }
                        }
                        TransformCaptureFormat::IfAbsent(text) => {
                            if value.is_empty() {
                                out.push_str(&text);
                            }
                        }
                        TransformCaptureFormat::IfElse { present, absent } => {
                            out.push_str(if value.is_empty() { &absent } else { &present });
                        }
                    }
                    i += consumed;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransformCaptureFormat {
    Plain,
    Modifier(String),
    IfPresent(String),
    IfAbsent(String),
    IfElse { present: String, absent: String },
}

fn parse_transform_capture(chars: &[char]) -> Option<(usize, TransformCaptureFormat, usize)> {
    if chars.first() != Some(&'$') || chars.get(1) != Some(&'{') {
        return None;
    }
    let mut j = 2;
    let digit_start = j;
    let mut capture = 0usize;
    while j < chars.len() && chars[j].is_ascii_digit() {
        let digit = chars[j] as usize - '0' as usize;
        capture = capture.checked_mul(10)?.checked_add(digit)?;
        j += 1;
    }
    if j == digit_start {
        return None;
    }
    let format = if chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&'/') {
        j += 2;
        let modifier_start = j;
        while j < chars.len() && chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        if j == modifier_start {
            return None;
        }
        TransformCaptureFormat::Modifier(chars[modifier_start..j].iter().collect())
    } else if chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&'+') {
        j += 2;
        let (text, consumed) = parse_transform_capture_text(&chars[j..], None)?;
        j += consumed;
        TransformCaptureFormat::IfPresent(text)
    } else if chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&'-') {
        j += 2;
        let (text, consumed) = parse_transform_capture_text(&chars[j..], None)?;
        j += consumed;
        TransformCaptureFormat::IfAbsent(text)
    } else if chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&'?') {
        j += 2;
        let (present, consumed) = parse_transform_capture_text(&chars[j..], Some(':'))?;
        j += consumed;
        let (absent, consumed) = parse_transform_capture_text(&chars[j..], None)?;
        j += consumed;
        TransformCaptureFormat::IfElse { present, absent }
    } else if chars.get(j) == Some(&':') {
        j += 1;
        let (text, consumed) = parse_transform_capture_text(&chars[j..], None)?;
        j += consumed;
        TransformCaptureFormat::IfAbsent(text)
    } else {
        TransformCaptureFormat::Plain
    };
    if chars.get(j) != Some(&'}') {
        return None;
    }
    Some((capture, format, j + 1))
}

fn parse_transform_capture_text(
    chars: &[char],
    stop: Option<char>,
) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut j = 0;
    while j < chars.len() {
        if Some(chars[j]) == stop {
            return Some((out, j + 1));
        }
        if chars[j] == '}' {
            return Some((out, j));
        }
        if chars[j] == '\\' && j + 1 < chars.len() {
            out.push(chars[j + 1]);
            j += 2;
            continue;
        }
        out.push(chars[j]);
        j += 1;
    }
    None
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = first.to_uppercase().collect::<String>();
    out.push_str(chars.as_str());
    out
}

fn camel_case(value: &str, pascal: bool) -> String {
    let mut out = String::new();
    let mut capitalize_next = pascal;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if out.is_empty() && !pascal {
                out.push(ch.to_ascii_lowercase());
            } else if capitalize_next {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
            capitalize_next = false;
        } else {
            capitalize_next = !out.is_empty();
        }
    }
    out
}

fn resolve_variable_with_default(
    name: &str,
    default: Option<&str>,
    context: &SnippetContext,
) -> Option<String> {
    match resolve_snippet_variable(name, context) {
        Some(value) if value.is_empty() => default
            .map(|value| resolve_variables_in_text(value, context))
            .or(Some(value)),
        Some(value) => Some(value),
        None => default.map(|value| resolve_variables_in_text(value, context)),
    }
}

fn unresolved_variable_literal(name: &str, braced: bool) -> String {
    if braced {
        format!("${{{name}}}")
    } else {
        format!("${name}")
    }
}

fn resolve_snippet_variable(name: &str, context: &SnippetContext) -> Option<String> {
    match name {
        "TM_SELECTED_TEXT" => Some(context.selected_text.clone()),
        "CLIPBOARD" => context.clipboard_text.clone(),
        "TM_CURRENT_LINE" => context.current_line.clone(),
        "TM_CURRENT_WORD" => Some(context.current_word.clone()),
        "TM_LINE_INDEX" => context.line_index.map(|line| line.to_string()),
        "TM_LINE_NUMBER" => context.line_index.map(|line| (line + 1).to_string()),
        "LINE_COMMENT" => Some(comment_tokens(context).line.to_string()),
        "BLOCK_COMMENT_START" => Some(comment_tokens(context).block_start.to_string()),
        "BLOCK_COMMENT_END" => Some(comment_tokens(context).block_end.to_string()),
        "CURRENT_YEAR" => Some(date_parts(context).year.to_string()),
        "CURRENT_YEAR_SHORT" => Some(format!("{:02}", date_parts(context).year % 100)),
        "CURRENT_MONTH" => Some(format!("{:02}", date_parts(context).month)),
        "CURRENT_MONTH_NAME" => Some(month_name(date_parts(context).month, false).to_string()),
        "CURRENT_MONTH_NAME_SHORT" => Some(month_name(date_parts(context).month, true).to_string()),
        "CURRENT_DATE" => Some(format!("{:02}", date_parts(context).day)),
        "CURRENT_DAY_NAME" => Some(day_name(date_parts(context).weekday, false).to_string()),
        "CURRENT_DAY_NAME_SHORT" => Some(day_name(date_parts(context).weekday, true).to_string()),
        "CURRENT_HOUR" => Some(format!("{:02}", date_parts(context).hour)),
        "CURRENT_MINUTE" => Some(format!("{:02}", date_parts(context).minute)),
        "CURRENT_SECOND" => Some(format!("{:02}", date_parts(context).second)),
        "CURRENT_MILLISECOND" => Some(format!("{:03}", date_parts(context).millisecond)),
        "CURRENT_SECONDS_UNIX" => Some((date_parts(context).unix_millis / 1000).to_string()),
        "CURRENT_MILLISECONDS_UNIX" => Some(date_parts(context).unix_millis.to_string()),
        "RANDOM" => Some(random_digits(6)),
        "RANDOM_HEX" => Some(format!("{:06x}", random_u64() & 0x00ff_ffff)),
        "UUID" => Some(random_uuid_v4()),
        "TM_FILENAME" => context
            .active_path
            .as_deref()
            .and_then(|path| path.file_name().map(|s| s.to_string_lossy().into_owned())),
        "TM_FILENAME_BASE" => context
            .active_path
            .as_deref()
            .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned())),
        "TM_DIRECTORY" => context
            .active_path
            .as_deref()
            .and_then(|path| path.parent().map(|p| p.to_string_lossy().into_owned())),
        "TM_FILEPATH" => context
            .active_path
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned()),
        "WORKSPACE_FOLDER" => context
            .workspace_root
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned()),
        "WORKSPACE_NAME" => context
            .workspace_root
            .as_deref()
            .and_then(|path| path.file_name().map(|s| s.to_string_lossy().into_owned()))
            .or_else(|| {
                context
                    .workspace_root
                    .as_ref()
                    .map(|_| "workspace".to_string())
            }),
        "RELATIVE_FILEPATH" => relative_filepath(context),
        _ => None,
    }
}

static RANDOM_COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0);
    let counter = RANDOM_COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    nanos ^ counter ^ ((std::process::id() as u64) << 32)
}

fn random_u64() -> u64 {
    let mut x = random_seed();
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn random_digits(width: usize) -> String {
    let modulo = 10u64.saturating_pow(width.min(18) as u32);
    format!("{:0width$}", random_u64() % modulo, width = width)
}

fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    let a = random_u64().to_be_bytes();
    let b = random_u64().to_be_bytes();
    bytes[..8].copy_from_slice(&a);
    bytes[8..].copy_from_slice(&b);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommentTokens {
    line: &'static str,
    block_start: &'static str,
    block_end: &'static str,
}

fn comment_tokens(context: &SnippetContext) -> CommentTokens {
    let Some(language) = context.language else {
        return CommentTokens {
            line: "",
            block_start: "",
            block_end: "",
        };
    };
    let cfg = crate::syntax::config_for(language);
    let (block_start, block_end) = cfg.block_comment.unwrap_or(("", ""));
    CommentTokens {
        line: cfg.line_comments.first().copied().unwrap_or(""),
        block_start,
        block_end,
    }
}

fn relative_filepath(context: &SnippetContext) -> Option<String> {
    let path = context.active_path.as_deref()?;
    let root = context.workspace_root.as_deref()?;
    path.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn date_parts(context: &SnippetContext) -> DateParts {
    context.date.unwrap_or_else(DateParts::local_now)
}

fn month_name(month: u8, short: bool) -> &'static str {
    const LONG: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const SHORT: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let idx = month.saturating_sub(1).min(11) as usize;
    if short {
        SHORT[idx]
    } else {
        LONG[idx]
    }
}

fn day_name(weekday: u8, short: bool) -> &'static str {
    const LONG: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let idx = weekday.min(6) as usize;
    if short {
        SHORT[idx]
    } else {
        LONG[idx]
    }
}

fn unix_millis_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn local_date_parts() -> DateParts {
    let mut t = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    DateParts {
        year: t.wYear,
        month: t.wMonth as u8,
        day: t.wDay as u8,
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
        second: t.wSecond as u8,
        millisecond: t.wMilliseconds,
        weekday: t.wDayOfWeek as u8,
        unix_millis: unix_millis_now(),
    }
}

#[cfg(not(windows))]
fn local_date_parts() -> DateParts {
    let unix_millis = unix_millis_now();
    let seconds = unix_millis.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    DateParts {
        year: year as u16,
        month,
        day,
        hour: (seconds_of_day / 3600) as u8,
        minute: ((seconds_of_day % 3600) / 60) as u8,
        second: (seconds_of_day % 60) as u8,
        millisecond: unix_millis.rem_euclid(1000) as u16,
        weekday: ((days + 4).rem_euclid(7)) as u8,
        unix_millis,
    }
}

#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let d = doy - (153 * mp + 2).div_euclid(5) + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    ((y + i64::from(m <= 2)) as i32, m as u8, d as u8)
}

/// An active tab-stop navigation session over an expanded snippet.
///
/// Holds the ordered stops + the index of the current stop. The model's cursor /
/// selection is driven to the current stop by the ABI; navigation just advances
/// or rewinds the index. Reaching past the last stop ends the session.
#[derive(Debug, Clone, Default)]
pub struct SnippetSession {
    stops: Vec<Stop>,
    /// Navigation indexes into `stops`. Equal-numbered stops appear once here;
    /// later equal stops are mirrors, not extra Tab destinations.
    nav: Vec<usize>,
    /// Index into `nav`; `nav.len()` means done.
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
            self.stops.clear();
            self.nav.clear();
            return false;
        }
        self.stops = stops;
        self.nav.clear();
        let mut seen = std::collections::HashSet::new();
        for (i, stop) in self.stops.iter().enumerate() {
            if seen.insert(stop.num) {
                self.nav.push(i);
            }
        }
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
        self.nav
            .get(self.cur)
            .and_then(|i| self.stops.get(*i))
            .copied()
    }

    /// Advance to the next stop. Returns the new current stop, or `None` (and ends
    /// the session) when the last stop has been passed.
    pub fn next_stop(&mut self) -> Option<Stop> {
        if !self.active {
            return None;
        }
        if self.cur + 1 >= self.nav.len() {
            // Past the final stop -> session over.
            self.cur = self.nav.len();
            self.active = false;
            return None;
        }
        self.cur += 1;
        self.current()
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
        self.current()
    }

    /// End the session (Esc / typing past the end / cursor leaving the region).
    pub fn cancel(&mut self) {
        self.active = false;
        self.stops.clear();
        self.nav.clear();
        self.cur = 0;
    }

    /// Delete the current primary placeholder before typing over it. The current
    /// stop's range collapses to its start; mirrors keep their old placeholder
    /// ranges until `sync_mirrors_from_current`.
    pub fn replace_current_selection(&mut self, model: &mut TextModel) -> bool {
        let Some(stop_idx) = self.nav.get(self.cur).copied() else {
            return false;
        };
        let Some(stop) = self.stops.get_mut(stop_idx) else {
            return false;
        };
        let removed = model.delete_selection();
        if removed {
            stop.end = stop.start;
        }
        removed
    }

    /// Copy the current primary placeholder text into all same-numbered mirror
    /// stops, keeping later tab-stop positions aligned after each replacement.
    pub fn sync_mirrors_from_current(&mut self, model: &mut TextModel) {
        let Some(primary_idx) = self.nav.get(self.cur).copied() else {
            return;
        };
        let Some(primary) = self.stops.get(primary_idx).copied() else {
            return;
        };
        if primary.num == 0 {
            return;
        }
        let cursor = (model.cursor_line(), model.cursor_col());
        if cursor.0 != primary.start.0 || cursor.1 < primary.start.1 {
            return;
        }
        let replacement = text_between(model, primary.start, cursor);
        if replacement.contains('\n') {
            return;
        }
        self.stops[primary_idx].end = cursor;

        let mut mirrors: Vec<usize> = self
            .stops
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != primary_idx && s.num == primary.num)
            .map(|(i, _)| i)
            .collect();
        mirrors.sort_by_key(|i| self.stops[*i].start);
        mirrors.reverse();

        for i in mirrors {
            let old = self.stops[i];
            if old.start.0 != old.end.0 {
                continue;
            }
            replace_range(model, old.start, old.end, &replacement);
            let new_end = (old.start.0, old.start.1 + replacement.chars().count());
            self.stops[i].end = new_end;
            self.shift_after_single_line_edit(i, old.start, old.end, new_end);
        }
        model.set_selection(self.stops[primary_idx].end, self.stops[primary_idx].end);
    }

    fn shift_after_single_line_edit(
        &mut self,
        edited_idx: usize,
        start: (usize, usize),
        old_end: (usize, usize),
        new_end: (usize, usize),
    ) {
        if start.0 != old_end.0 || start.0 != new_end.0 {
            return;
        }
        let old_len = old_end.1.saturating_sub(start.1);
        let new_len = new_end.1.saturating_sub(start.1);
        let delta = new_len as isize - old_len as isize;
        if delta == 0 {
            return;
        }
        let shift = |col: usize| -> usize {
            if delta >= 0 {
                col.saturating_add(delta as usize)
            } else {
                col.saturating_sub((-delta) as usize)
            }
        };
        for (i, stop) in self.stops.iter_mut().enumerate() {
            if i == edited_idx || stop.start.0 != start.0 {
                continue;
            }
            if stop.start.1 >= old_end.1 {
                stop.start.1 = shift(stop.start.1);
            }
            if stop.end.1 >= old_end.1 {
                stop.end.1 = shift(stop.end.1);
            }
        }
    }
}

fn text_between(model: &TextModel, start: (usize, usize), end: (usize, usize)) -> String {
    if start.0 != end.0 {
        return String::new();
    }
    let line = model.line(start.0);
    let chars: Vec<char> = line.chars().collect();
    chars[start.1.min(chars.len())..end.1.min(chars.len())]
        .iter()
        .collect()
}

fn replace_range(model: &mut TextModel, start: (usize, usize), end: (usize, usize), text: &str) {
    model.set_selection(start, end);
    let _ = model.delete_selection();
    for ch in text.chars() {
        model.insert_char(ch);
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
        SnippetDef::new("enum", "enum", "enum ${1:Name} {\n  ${2:Variant},\n}$0"),
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
            SnippetDef::new(
                "fn",
                "function",
                "fn ${1:name}(${2:args}) -> ${3:()} {\n    $0\n}",
            ),
            SnippetDef::new(
                "struct",
                "struct",
                "struct ${1:Name} {\n    ${2:field}: ${3:T},\n}$0",
            ),
            SnippetDef::new("if", "if", "if ${1:cond} {\n    $0\n}"),
            SnippetDef::new("for", "for", "for ${1:i} in ${2:iter} {\n    $0\n}"),
            SnippetDef::new(
                "match",
                "match",
                "match ${1:value} {\n    ${2:pat} => $0,\n}",
            ),
            SnippetDef::new(
                "test",
                "test",
                "#[test]\nfn ${1:name}() {\n    assert_eq!(${2:a}, ${3:b});\n    $0\n}",
            ),
        ],
        Language::Python => vec![
            SnippetDef::new("def", "def", "def ${1:name}(${2:args}):\n    $0"),
            SnippetDef::new(
                "class",
                "class",
                "class ${1:Name}:\n    def __init__(self${2:, args}):\n        $0",
            ),
            SnippetDef::new("if", "if", "if ${1:cond}:\n    $0"),
            SnippetDef::new("for", "for", "for ${1:item} in ${2:iterable}:\n    $0"),
            SnippetDef::new("while", "while", "while ${1:cond}:\n    $0"),
        ],
        Language::JavaScript | Language::TypeScript => vec![
            SnippetDef::new("fn", "function", "function ${1:name}(${2:args}) {\n  $0\n}"),
            SnippetDef::new("if", "if", "if (${1:cond}) {\n  $0\n}"),
            SnippetDef::new(
                "for",
                "for",
                "for (let ${1:i} = 0; ${1:i} < ${2:n}; ${1:i}++) {\n  $0\n}",
            ),
            SnippetDef::new("log", "console.log", "console.log($0)"),
        ],
        Language::Go => vec![
            SnippetDef::new(
                "fn",
                "func",
                "func ${1:name}(${2:args}) ${3:error} {\n\t$0\n}",
            ),
            SnippetDef::new("if", "if", "if ${1:cond} {\n\t$0\n}"),
            SnippetDef::new(
                "for",
                "for",
                "for ${1:i} := 0; ${1:i} < ${2:n}; ${1:i}++ {\n\t$0\n}",
            ),
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
    for u in user.into_iter().filter(|u| u.applies_to(lang)) {
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
// User snippets (optional): loaded from config-side snippet files
// ---------------------------------------------------------------------------

/// Candidate user snippet files in the IDE config dir.
///
/// Load order is stable: the legacy TSV `snippets` file first, then common
/// single-file JSON names, then copied VS Code `*.code-snippets` files sorted by
/// filename. Later files can override earlier same-prefix definitions.
fn user_snippets_paths() -> Vec<std::path::PathBuf> {
    let Some(dir) = crate::config::config_path().and_then(|p| p.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    user_snippets_paths_in_dir(&dir)
}

fn user_snippets_paths_in_dir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = vec![
        dir.join("snippets"),
        dir.join("snippets.json"),
        dir.join("user-snippets.json"),
    ];
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut code_snippets: Vec<std::path::PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("code-snippets"))
            })
            .collect();
        code_snippets.sort_by_key(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        });
        paths.extend(code_snippets);
    }
    paths
}

/// Parse the user-snippet blob: each non-comment line is
/// `prefix\tlabel\tbody` (body uses `\n` for newlines, `\t` for tabs), or a
/// VS Code-style JSON snippet object.
pub fn parse_user_snippets(text: &str) -> Vec<SnippetDef> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') {
        return parse_vscode_snippets(trimmed).unwrap_or_default();
    }
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

fn parse_vscode_snippets(text: &str) -> Option<Vec<SnippetDef>> {
    let root = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .or_else(|| {
            let normalized = normalize_jsonc_snippets(text);
            serde_json::from_str::<serde_json::Value>(&normalized).ok()
        })?;
    let object = root.as_object()?;
    let mut out = Vec::new();
    for (name, value) in object {
        let Some(entry) = value.as_object() else {
            continue;
        };
        let prefixes = snippet_prefixes(entry.get("prefix"));
        if prefixes.is_empty() {
            continue;
        }
        let Some(body) = snippet_body(entry.get("body")) else {
            continue;
        };
        let scope = snippet_scope(entry.get("scope"));
        let label = entry
            .get("description")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(name);
        for prefix in prefixes {
            out.push(SnippetDef::new(&prefix, label, &body).with_scope(scope.clone()));
        }
    }
    Some(out)
}

fn normalize_jsonc_snippets(text: &str) -> String {
    remove_json_trailing_commas(&strip_json_comments(text))
}

fn strip_json_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '/' && chars.get(i + 1) == Some(&'/') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() {
                if chars[i] == '\n' {
                    out.push('\n');
                    i += 1;
                    continue;
                }
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn remove_json_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if matches!(chars.get(j), Some('}' | ']')) {
                i += 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn snippet_prefixes(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(prefix)) if !prefix.trim().is_empty() => {
            vec![prefix.to_string()]
        }
        Some(serde_json::Value::Array(prefixes)) => prefixes
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|prefix| !prefix.trim().is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn snippet_body(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(body)) => Some(body.to_string()),
        Some(serde_json::Value::Array(lines)) => {
            let mut out = Vec::new();
            for line in lines {
                out.push(line.as_str()?.to_string());
            }
            Some(out.join("\n"))
        }
        _ => None,
    }
}

fn snippet_scope(value: Option<&serde_json::Value>) -> Vec<Language> {
    match value {
        Some(serde_json::Value::String(scope)) => scope
            .split(',')
            .filter_map(snippet_scope_language)
            .collect(),
        Some(serde_json::Value::Array(scopes)) => scopes
            .iter()
            .filter_map(serde_json::Value::as_str)
            .flat_map(|scope| scope.split(','))
            .filter_map(snippet_scope_language)
            .collect(),
        _ => Vec::new(),
    }
}

fn snippet_scope_language(raw: &str) -> Option<Language> {
    let scope = raw.trim().to_ascii_lowercase();
    match scope.as_str() {
        "javascriptreact" | "jsx" => Some(Language::JavaScript),
        "typescriptreact" | "tsx" => Some(Language::TypeScript),
        "shellscript" => Some(Language::Shell),
        "plaintext" | "text" => Some(Language::PlainText),
        _ => Language::from_slug(&scope),
    }
}

/// Load user snippets from config-side files (best-effort; empty on any error).
pub fn load_user_snippets() -> Vec<SnippetDef> {
    load_user_snippets_from_paths(user_snippets_paths())
}

fn load_user_snippets_from_paths(paths: Vec<std::path::PathBuf>) -> Vec<SnippetDef> {
    let mut out = Vec::new();
    for path in paths {
        if let Ok(text) = crate::config::read_config_text(&path) {
            out.extend(parse_user_snippets(&text));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Driving the model: expand at the cursor + drive a session
// ---------------------------------------------------------------------------

/// Try to expand the snippet whose prefix is the identifier-word immediately
/// before the model's cursor, for `lang`. On success: deletes the prefix word,
/// inserts the expanded body (indented to the call-site line), begins the
/// `session`, selects the first tab-stop's placeholder, and returns `true`.
/// Returns `false` (model untouched) when there's no snippet for the word.
pub fn can_expand(model: &TextModel, lang: Language) -> bool {
    let line = model.cursor_line();
    let col = model.cursor_col();
    let word = prefix_word(model.line(line), col);
    find_snippet(lang, &word).is_some()
}

pub fn try_expand(model: &mut TextModel, session: &mut SnippetSession, lang: Language) -> bool {
    try_expand_with_path(model, session, lang, None)
}

pub fn try_expand_with_path(
    model: &mut TextModel,
    session: &mut SnippetSession,
    lang: Language,
    active_path: Option<&Path>,
) -> bool {
    try_expand_with_path_and_selection(model, session, lang, active_path, "")
}

pub fn try_expand_with_path_and_selection(
    model: &mut TextModel,
    session: &mut SnippetSession,
    lang: Language,
    active_path: Option<&Path>,
    selected_text: &str,
) -> bool {
    try_expand_with_context(model, session, lang, active_path, selected_text, None)
}

pub fn try_expand_with_context(
    model: &mut TextModel,
    session: &mut SnippetSession,
    lang: Language,
    active_path: Option<&Path>,
    selected_text: &str,
    workspace_root: Option<&Path>,
) -> bool {
    try_expand_with_context_and_clipboard(
        model,
        session,
        lang,
        active_path,
        selected_text,
        workspace_root,
        None,
    )
}

pub fn try_expand_with_context_and_clipboard(
    model: &mut TextModel,
    session: &mut SnippetSession,
    lang: Language,
    active_path: Option<&Path>,
    selected_text: &str,
    workspace_root: Option<&Path>,
    clipboard_text: Option<&str>,
) -> bool {
    let line = model.cursor_line();
    let col = model.cursor_col();
    let word = prefix_word(model.line(line), col);
    let current_line = model.line(line).to_string();
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
    let context = SnippetContext::from_editor_context_with_language(
        active_path,
        selected_text,
        workspace_root,
        Some(&current_line),
        Some(line),
        &word,
        None,
        lang,
    )
    .with_clipboard_text(clipboard_text);
    let exp = expand_with_context(&def.body, &indent, cl, cc, &context);
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

    fn assert_uuid_v4(value: &str) {
        assert_eq!(value.len(), 36, "{value}");
        let chars: Vec<char> = value.chars().collect();
        for idx in [8, 13, 18, 23] {
            assert_eq!(chars[idx], '-', "{value}");
        }
        assert!(
            chars
                .iter()
                .enumerate()
                .filter(|(idx, _)| ![8, 13, 18, 23].contains(idx))
                .all(|(_, ch)| ch.is_ascii_hexdigit()),
            "{value}"
        );
        assert_eq!(chars[14], '4', "{value}");
        assert!(
            matches!(chars[19], '8' | '9' | 'a' | 'b' | 'A' | 'B'),
            "{value}"
        );
    }

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
                Segment::Stop {
                    num: 1,
                    placeholder: String::new()
                },
                Segment::Text("b".into()),
                Segment::Stop {
                    num: 2,
                    placeholder: String::new()
                },
                Segment::Text("c".into()),
                Segment::Stop {
                    num: 0,
                    placeholder: String::new()
                },
            ]
        );
    }

    #[test]
    fn parse_overflow_bare_tab_stop_as_literal_text() {
        let body = "a$999999999999999999999999999999b";
        assert_eq!(parse_body(body), vec![Segment::Text(body.into())]);
    }

    #[test]
    fn parse_placeholders() {
        let segs = parse_body("fn ${1:name}(${2:args})");
        assert_eq!(
            segs,
            vec![
                Segment::Text("fn ".into()),
                Segment::Stop {
                    num: 1,
                    placeholder: "name".into()
                },
                Segment::Text("(".into()),
                Segment::Stop {
                    num: 2,
                    placeholder: "args".into()
                },
                Segment::Text(")".into()),
            ]
        );
    }

    #[test]
    fn parse_overflow_braced_tab_stop_as_literal_text() {
        let body = "fn ${999999999999999999999999999999:name}()";
        assert_eq!(parse_body(body), vec![Segment::Text(body.into())]);
    }

    #[test]
    fn parse_variables() {
        let segs = parse_body("file $TM_FILENAME base $TM_FILENAME_BASE");
        assert_eq!(
            segs,
            vec![
                Segment::Text("file ".into()),
                Segment::Variable {
                    name: "TM_FILENAME".into(),
                    default: None,
                    braced: false,
                },
                Segment::Text(" base ".into()),
                Segment::Variable {
                    name: "TM_FILENAME_BASE".into(),
                    default: None,
                    braced: false,
                },
            ]
        );
    }

    #[test]
    fn parse_braced_variables() {
        let segs = parse_body("file ${TM_FILENAME} base ${TM_FILENAME_BASE:main}");
        assert_eq!(
            segs,
            vec![
                Segment::Text("file ".into()),
                Segment::Variable {
                    name: "TM_FILENAME".into(),
                    default: None,
                    braced: true,
                },
                Segment::Text(" base ".into()),
                Segment::Variable {
                    name: "TM_FILENAME_BASE".into(),
                    default: Some("main".into()),
                    braced: true,
                },
            ]
        );
    }

    #[test]
    fn parse_braced_variable_transform() {
        let segs = parse_body("class ${TM_FILENAME_BASE/(.*)/${1:/pascalcase}/} {}");
        assert_eq!(
            segs,
            vec![
                Segment::Text("class ".into()),
                Segment::VariableTransform {
                    name: "TM_FILENAME_BASE".into(),
                    pattern: "(.*)".into(),
                    format: "${1:/pascalcase}".into(),
                    options: "".into(),
                },
                Segment::Text(" {}".into()),
            ]
        );
    }

    #[test]
    fn parse_choice_placeholder_uses_first_choice() {
        let segs = parse_body("color: ${1|red,green,blue|};");
        assert_eq!(
            segs,
            vec![
                Segment::Text("color: ".into()),
                Segment::Stop {
                    num: 1,
                    placeholder: "red".into()
                },
                Segment::Text(";".into()),
            ]
        );
    }

    #[test]
    fn parse_choice_placeholder_honors_escaped_separators() {
        let segs = parse_body("${1|one\\, two,pipe\\|value,slash\\\\value|}");
        assert_eq!(
            segs,
            vec![Segment::Stop {
                num: 1,
                placeholder: "one, two".into()
            }]
        );
    }

    #[test]
    fn parse_placeholder_honors_escaped_brace_dollar_and_backslash() {
        let segs = parse_body("${1:a\\}b \\$c \\\\ d}!");
        assert_eq!(
            segs,
            vec![
                Segment::Stop {
                    num: 1,
                    placeholder: "a}b $c \\ d".into()
                },
                Segment::Text("!".into()),
            ]
        );
    }

    #[test]
    fn parse_nested_placeholder_defaults_are_flattened() {
        let segs = parse_body("${1:${2:name} = ${3:value}};");
        assert_eq!(
            segs,
            vec![
                Segment::Stop {
                    num: 1,
                    placeholder: "name = value".into()
                },
                Segment::Text(";".into()),
            ]
        );
    }

    #[test]
    fn parse_bare_nested_tab_stops_do_not_leak_into_placeholder_text() {
        let segs = parse_body("${1:call($2)}");
        assert_eq!(
            segs,
            vec![Segment::Stop {
                num: 1,
                placeholder: "call()".into()
            }]
        );
    }

    #[test]
    fn parse_escaped_dollar_is_literal() {
        assert_eq!(
            parse_body("cost \\$5"),
            vec![Segment::Text("cost $5".into())]
        );
    }

    #[test]
    fn parse_multidigit_stop() {
        let segs = parse_body("$10");
        assert_eq!(
            segs,
            vec![Segment::Stop {
                num: 10,
                placeholder: String::new()
            }]
        );
    }

    #[test]
    fn parse_braced_without_placeholder() {
        // `${3}` form (digits, no colon) is a bare stop.
        let segs = parse_body("x${3}y");
        assert_eq!(
            segs,
            vec![
                Segment::Text("x".into()),
                Segment::Stop {
                    num: 3,
                    placeholder: String::new()
                },
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
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (0, 4),
                end: (0, 8)
            }
        );
        assert_eq!(
            exp.stops[1],
            Stop {
                num: 0,
                start: (0, 11),
                end: (0, 16)
            }
        );
    }

    #[test]
    fn expand_choice_placeholder_selects_inserted_first_choice() {
        let exp = expand("kind ${1|error,warning,info|} $0", "", 0, 0);
        assert_eq!(exp.text, "kind error ");
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (0, 5),
                end: (0, 10)
            }
        );
        assert_eq!(
            exp.stops[1],
            Stop {
                num: 0,
                start: (0, 11),
                end: (0, 11)
            }
        );
    }

    #[test]
    fn expand_file_variables_from_context() {
        let path = std::path::Path::new("C:/work/src/main.test.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context(
            "$TM_FILENAME|$TM_FILENAME_BASE|$TM_DIRECTORY|$TM_FILEPATH",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(
            exp.text,
            "main.test.mty|main.test|C:/work/src|C:/work/src/main.test.mty"
        );
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_braced_file_variables_from_context() {
        let path = std::path::Path::new("C:/work/src/main.test.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context(
            "${TM_FILENAME}|${TM_FILENAME_BASE}|${TM_DIRECTORY}|${TM_FILEPATH}",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(
            exp.text,
            "main.test.mty|main.test|C:/work/src|C:/work/src/main.test.mty"
        );
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_common_variable_transform_modifiers() {
        let path = std::path::Path::new("C:/work/src/my-component.test.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context(
            concat!(
                "${TM_FILENAME_BASE/(.*)/${1:/pascalcase}/}|",
                "${TM_FILENAME_BASE/(.*)/${1:/camelcase}/}|",
                "${TM_FILENAME_BASE/(.*)/${1:/upcase}/}|",
                "${TM_FILENAME_BASE/(.*)/${1:/downcase}/}|",
                "${TM_FILENAME_BASE/(.*)/${1:/capitalize}/}"
            ),
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(
            exp.text,
            "MyComponentTest|myComponentTest|MY-COMPONENT.TEST|my-component.test|My-component.test"
        );
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_capture_replacements() {
        let path = std::path::Path::new("C:/work/src/my-component.test.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context(
            concat!(
                "${TM_FILENAME/(.*)\\..+$/$1/}|",
                "${TM_FILENAME_BASE/(my)-(component).*/${1}_${2}/}|",
                "${TM_FILENAME/(.*)\\..+$/${1:/pascalcase}/}"
            ),
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "my-component.test|my_component|MyComponentTest");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_rejects_overflow_capture_indexes() {
        let ctx = SnippetContext::from_path_and_selection(None, "alpha");
        let exp = expand_with_context(
            concat!(
                "${TM_SELECTED_TEXT/^(alpha)$/$999999999999999999999999999999/}|",
                "${TM_SELECTED_TEXT/^(alpha)$/${999999999999999999999999999999:/upcase}/}"
            ),
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "|${999999999999999999999999999999:/upcase}");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_unescapes_slash_delimiters() {
        let ctx = SnippetContext::from_path_and_selection(None, "src/my-component.test");
        let exp = expand_with_context(
            concat!(
                "${TM_SELECTED_TEXT/^src\\/(.+)$/$1/}|",
                "${TM_SELECTED_TEXT/^(src)\\/(my)-(component).*$/$1\\/${2}_${3}/}"
            ),
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "my-component.test|src/my_component");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_honors_global_and_invalid_regex_fallback() {
        let path = std::path::Path::new("C:/work/src/my-component.test.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context(
            "${TM_FILENAME_BASE/[-.]/_/g}|${TM_FILENAME_BASE/[/$1/}",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "my_component_test|my-component.test");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_honors_multiline_regex_option() {
        let ctx = SnippetContext::from_path_and_selection(None, "alpha\nbeta\ngamma");
        let exp = expand_with_context(
            "${TM_SELECTED_TEXT/^(.+)$/> $1/gm}",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "> alpha\n> beta\n> gamma");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_honors_dotall_regex_option() {
        let ctx = SnippetContext::from_path_and_selection(None, "alpha\nbeta");
        let exp = expand_with_context(
            "${TM_SELECTED_TEXT/^alpha.*beta$/joined/s}",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "joined");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_variable_transform_conditional_replacements() {
        let path = std::path::Path::new("C:/work/src/my-component.test.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context(
            concat!(
                "${TM_FILENAME_BASE/^(my)?-(component)(?:\\.(test))?/${1:+has-my}-${4:-missing}-${3:?test:prod}/}|",
                "${TM_FILENAME_BASE/^my-(component)(?:\\.(test))?(missing)?$/${3:plain-fallback}/}"
            ),
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "has-my-missing-test|plain-fallback");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_braced_variable_defaults_without_context() {
        let exp = expand("${TM_FILENAME_BASE:main}|${UNKNOWN:fallback}", "", 0, 0);
        assert_eq!(exp.text, "main|fallback");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_selected_text_variables_from_context() {
        let ctx = SnippetContext::from_path_and_selection(None, "selected");
        let exp = expand_with_context(
            "$TM_SELECTED_TEXT|${TM_SELECTED_TEXT}|${1:$TM_SELECTED_TEXT}",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "selected|selected|selected");
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (0, 18),
                end: (0, 26)
            }
        );
    }

    #[test]
    fn expand_clipboard_variable_from_context() {
        let ctx = SnippetContext::default().with_clipboard_text(Some("clip\ntext"));
        let exp = expand_with_context("$CLIPBOARD|${CLIPBOARD}|${1:$CLIPBOARD}", "", 0, 0, &ctx);
        assert_eq!(exp.text, "clip\ntext|clip\ntext|clip\ntext");
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (2, 5),
                end: (3, 4)
            }
        );
    }

    #[test]
    fn expand_empty_clipboard_uses_default() {
        let ctx = SnippetContext::default().with_clipboard_text(Some(""));
        let exp = expand_with_context("${CLIPBOARD:fallback}|$CLIPBOARD", "", 0, 0, &ctx);
        assert_eq!(exp.text, "fallback|");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_workspace_variables_from_context() {
        let path = std::path::Path::new("C:/work/app/src/main.mty");
        let root = std::path::Path::new("C:/work/app");
        let ctx = SnippetContext::from_path_selection_and_workspace(Some(path), "", Some(root));
        let exp = expand_with_context(
            "$WORKSPACE_NAME|${WORKSPACE_FOLDER}|$RELATIVE_FILEPATH",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "app|C:/work/app|src/main.mty");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_current_line_variables_from_context() {
        let ctx = SnippetContext::from_editor_context(
            None,
            "",
            None,
            Some("  guard"),
            Some(6),
            "guard",
            None,
        );
        let exp = expand_with_context(
            "$TM_CURRENT_LINE|$TM_CURRENT_WORD|$TM_LINE_INDEX|$TM_LINE_NUMBER",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "  guard|guard|6|7");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_empty_current_word_uses_default() {
        let ctx =
            SnippetContext::from_editor_context(None, "", None, Some("  "), Some(0), "", None);
        let exp = expand_with_context("${TM_CURRENT_WORD:name}|$TM_CURRENT_WORD", "", 0, 0, &ctx);
        assert_eq!(exp.text, "name|");
    }

    #[test]
    fn expand_comment_variables_from_language_context() {
        let rust_ctx = SnippetContext::from_editor_context_with_language(
            None,
            "",
            None,
            None,
            None,
            "",
            None,
            Language::Rust,
        );
        let rust = expand_with_context(
            "$LINE_COMMENT ${1:todo}\n$BLOCK_COMMENT_START ${2:note} $BLOCK_COMMENT_END",
            "",
            0,
            0,
            &rust_ctx,
        );
        assert_eq!(rust.text, "// todo\n/* note */");

        let python_ctx = SnippetContext::from_editor_context_with_language(
            None,
            "",
            None,
            None,
            None,
            "",
            None,
            Language::Python,
        );
        let python = expand_with_context(
            "$LINE_COMMENT ${1:todo}|${BLOCK_COMMENT_START:#}|$BLOCK_COMMENT_START",
            "",
            0,
            0,
            &python_ctx,
        );
        assert_eq!(python.text, "# todo|#|");
    }

    #[test]
    fn expand_current_date_variables_from_context() {
        let ctx = SnippetContext::from_editor_context(
            None,
            "",
            None,
            None,
            None,
            "",
            Some(DateParts {
                year: 2026,
                month: 6,
                day: 4,
                hour: 9,
                minute: 5,
                second: 7,
                millisecond: 78,
                weekday: 4,
                unix_millis: 1_717_489_107_078,
            }),
        );
        let exp = expand_with_context(
            concat!(
                "$CURRENT_YEAR|$CURRENT_YEAR_SHORT|$CURRENT_MONTH|",
                "$CURRENT_MONTH_NAME|$CURRENT_MONTH_NAME_SHORT|$CURRENT_DATE|",
                "$CURRENT_DAY_NAME|$CURRENT_DAY_NAME_SHORT|",
                "$CURRENT_HOUR|$CURRENT_MINUTE|$CURRENT_SECOND"
            ),
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "2026|26|06|June|Jun|04|Thursday|Thu|09|05|07");
    }

    #[test]
    fn expand_current_timestamp_variables_from_context() {
        let ctx = SnippetContext::from_editor_context(
            None,
            "",
            None,
            None,
            None,
            "",
            Some(DateParts {
                year: 2026,
                month: 6,
                day: 4,
                hour: 9,
                minute: 5,
                second: 7,
                millisecond: 78,
                weekday: 4,
                unix_millis: 1_717_489_107_078,
            }),
        );
        let exp = expand_with_context(
            "$CURRENT_MILLISECOND|$CURRENT_SECONDS_UNIX|$CURRENT_MILLISECONDS_UNIX",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "078|1717489107|1717489107078");
    }

    #[test]
    fn random_variables_expand_with_vscode_shapes() {
        let exp = expand("$RANDOM|$RANDOM_HEX|$UUID", "", 0, 0);
        let parts: Vec<&str> = exp.text.split('|').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 6);
        assert!(
            parts[0].chars().all(|ch| ch.is_ascii_digit()),
            "{:?}",
            parts[0]
        );
        assert_eq!(parts[1].len(), 6);
        assert!(
            parts[1].chars().all(|ch| ch.is_ascii_hexdigit()),
            "{:?}",
            parts[1]
        );
        assert_uuid_v4(parts[2]);
    }

    #[test]
    fn random_uuid_helper_sets_version_and_variant_bits() {
        assert_uuid_v4(&random_uuid_v4());
    }

    #[test]
    fn expand_relative_filepath_preserves_unmatched_workspace_literal() {
        let path = std::path::Path::new("C:/other/main.mty");
        let root = std::path::Path::new("C:/work/app");
        let ctx = SnippetContext::from_path_selection_and_workspace(Some(path), "", Some(root));
        let exp = expand_with_context(
            "${RELATIVE_FILEPATH:fallback}|$RELATIVE_FILEPATH",
            "",
            0,
            0,
            &ctx,
        );
        assert_eq!(exp.text, "fallback|$RELATIVE_FILEPATH");
    }

    #[test]
    fn expand_empty_selected_text_uses_default() {
        let exp = expand("${TM_SELECTED_TEXT:fallback}|$TM_SELECTED_TEXT", "", 0, 0);
        assert_eq!(exp.text, "fallback|");
        assert!(exp.stops.is_empty());
    }

    #[test]
    fn expand_unknown_variables_are_preserved() {
        let exp = expand(
            "$UNKNOWN ${UNKNOWN} $TM_FILENAME ${TM_FILENAME} $TM_SELECTED_TEXT $CLIPBOARD ${CLIPBOARD}",
            "",
            0,
            0,
        );
        assert_eq!(
            exp.text,
            "$UNKNOWN ${UNKNOWN} $TM_FILENAME ${TM_FILENAME}  $CLIPBOARD ${CLIPBOARD}"
        );
    }

    #[test]
    fn expand_variables_inside_placeholders_update_selection_range() {
        let path = std::path::Path::new("C:/work/src/main.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context("${1:${TM_FILENAME_BASE:default}}_test $0", "", 0, 0, &ctx);
        assert_eq!(exp.text, "main_test ");
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (0, 0),
                end: (0, 4)
            }
        );
        assert_eq!(
            exp.stops[1],
            Stop {
                num: 0,
                start: (0, 10),
                end: (0, 10)
            }
        );
    }

    #[test]
    fn expand_nested_placeholder_selects_flattened_default() {
        let exp = expand("let ${1:${2:name}: ${3:Type}} = $0", "", 0, 0);
        assert_eq!(exp.text, "let name: Type = ");
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (0, 4),
                end: (0, 14)
            }
        );
        assert_eq!(
            exp.stops[1],
            Stop {
                num: 0,
                start: (0, 17),
                end: (0, 17)
            }
        );
    }

    #[test]
    fn expand_multiline_indents_continuations() {
        // Called at line 2, col 4 (inside a 4-space indent).
        let exp = expand("if ${1:cond} {\n  $0\n}", "    ", 2, 4);
        // Continuation lines get the call-site indent prepended.
        assert_eq!(exp.text, "if cond {\n      \n    }");
        // $1 = "cond" on line 2 cols 7..11.
        assert_eq!(
            exp.stops[0],
            Stop {
                num: 1,
                start: (2, 7),
                end: (2, 11)
            }
        );
        // $0 = zero-length on line 3; col = indent(4) + body "  " (2) = 6.
        assert_eq!(
            exp.stops[1],
            Stop {
                num: 0,
                start: (3, 6),
                end: (3, 6)
            }
        );
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
            Stop {
                num: 1,
                start: (0, 0),
                end: (0, 1),
            },
            Stop {
                num: 2,
                start: (0, 2),
                end: (0, 3),
            },
            Stop {
                num: 0,
                start: (0, 4),
                end: (0, 4),
            },
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
            Stop {
                num: 1,
                start: (0, 0),
                end: (0, 1),
            },
            Stop {
                num: 0,
                start: (0, 2),
                end: (0, 2),
            },
        ];
        let mut s = SnippetSession::new();
        s.begin(stops);
        s.next_stop(); // now at $0
        assert_eq!(s.prev_stop().unwrap().num, 1);
        // Prev at the first stays at the first.
        assert_eq!(s.prev_stop().unwrap().num, 1);
    }

    #[test]
    fn session_skips_duplicate_mirror_stops_for_navigation() {
        let stops = vec![
            Stop {
                num: 1,
                start: (0, 0),
                end: (0, 1),
            },
            Stop {
                num: 1,
                start: (0, 5),
                end: (0, 6),
            },
            Stop {
                num: 2,
                start: (0, 10),
                end: (0, 11),
            },
            Stop {
                num: 0,
                start: (0, 12),
                end: (0, 12),
            },
        ];
        let mut s = SnippetSession::new();
        assert!(s.begin(stops));
        assert_eq!(s.current().unwrap().num, 1);
        assert_eq!(s.next_stop().unwrap().num, 2);
        assert_eq!(s.next_stop().unwrap().num, 0);
        assert_eq!(s.next_stop(), None);
    }

    #[test]
    fn session_mirrors_placeholder_typing() {
        let mut m = TextModel::from_bytes(b"for");
        m.move_to(0, 3);
        let mut s = SnippetSession::new();
        assert!(try_expand(&mut m, &mut s, Language::JavaScript));
        assert_eq!(m.selected_text(), "i");
        assert!(s.replace_current_selection(&mut m));
        m.insert_char('j');
        s.sync_mirrors_from_current(&mut m);
        assert_eq!(m.line(0), "for (let j = 0; j < n; j++) {");
        assert_eq!((m.cursor_line(), m.cursor_col()), (0, 10));
        let next = s.next_stop().unwrap();
        assert_eq!(next.num, 2);
        m.set_selection(next.start, next.end);
        assert_eq!(m.selected_text(), "n");
    }

    #[test]
    fn session_cancel_deactivates() {
        let mut s = SnippetSession::new();
        s.begin(vec![Stop {
            num: 1,
            start: (0, 0),
            end: (0, 1),
        }]);
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
        for want in [
            "fn", "struct", "enum", "agent", "protocol", "match", "if", "ifelse", "for", "while",
            "let", "test", "log", "main",
        ] {
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
        let blob =
            "# my snippets\nguard\tguard clause\tif ${1:cond} {\\n  return\\n}$0\n\nbad line\n";
        let defs = parse_user_snippets(blob);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].prefix, "guard");
        assert_eq!(defs[0].label, "guard clause");
        assert!(defs[0].body.contains('\n'));
    }

    #[test]
    fn parse_user_snippets_vscode_json() {
        let blob = r#"{
            "Console Log": {
                "prefix": ["log", "clog"],
                "body": ["console.log(${1:value});", "$0"],
                "description": "Log to console"
            },
            "Guard": {
                "prefix": "guard",
                "body": "if (!${1:cond}) return $0"
            }
        }"#;
        let defs = parse_user_snippets(blob);
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[0].prefix, "log");
        assert_eq!(defs[0].label, "Log to console");
        assert_eq!(defs[0].body, "console.log(${1:value});\n$0");
        assert_eq!(defs[1].prefix, "clog");
        assert_eq!(defs[1].body, "console.log(${1:value});\n$0");
        assert_eq!(defs[2].prefix, "guard");
        assert_eq!(defs[2].label, "Guard");
    }

    #[test]
    fn parse_user_snippets_vscode_jsonc_comments_and_trailing_commas() {
        let blob = r#"{
            // Existing VS Code snippet files commonly include comments.
            "Fetch": {
                "prefix": ["fetch",],
                "body": [
                    "let url = \"https://example.com/${1:path}\";",
                    "/* keep this literal block marker */",
                    "$0",
                ],
                "description": "Fetch URL",
            },
        }"#;
        let defs = parse_user_snippets(blob);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].prefix, "fetch");
        assert_eq!(defs[0].label, "Fetch URL");
        assert_eq!(
            defs[0].body,
            "let url = \"https://example.com/${1:path}\";\n/* keep this literal block marker */\n$0"
        );
    }

    #[test]
    fn parse_user_snippets_vscode_scope_limits_languages() {
        let blob = r##"{
            "Console": {
                "scope": "javascript, typescriptreact",
                "prefix": "log",
                "body": "console.log($0)"
            },
            "Shell": {
                "scope": ["shellscript", "plaintext"],
                "prefix": "bang",
                "body": "#! /usr/bin/env bash\n$0"
            },
            "Global": {
                "prefix": "todo",
                "body": "TODO: $0"
            }
        }"##;
        let defs = parse_user_snippets(blob);
        let log = defs.iter().find(|d| d.prefix == "log").unwrap();
        assert!(log.applies_to(Language::JavaScript));
        assert!(log.applies_to(Language::TypeScript));
        assert!(!log.applies_to(Language::Python));

        let bang = defs.iter().find(|d| d.prefix == "bang").unwrap();
        assert!(bang.applies_to(Language::Shell));
        assert!(bang.applies_to(Language::PlainText));
        assert!(!bang.applies_to(Language::Rust));

        let todo = defs.iter().find(|d| d.prefix == "todo").unwrap();
        assert!(todo.applies_to(Language::Mighty));
        assert!(todo.applies_to(Language::Python));
    }

    #[test]
    fn parse_user_snippets_invalid_json_is_empty() {
        assert!(parse_user_snippets("{not json").is_empty());
    }

    #[test]
    fn user_snippets_paths_include_vscode_files_in_stable_order() {
        let dir = std::env::temp_dir().join(format!("mui-snippet-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("z.code-snippets"), "{}").unwrap();
        std::fs::write(dir.join("a.code-snippets"), "{}").unwrap();
        std::fs::write(dir.join("ignore.txt"), "{}").unwrap();

        let files: Vec<String> = user_snippets_paths_in_dir(&dir)
            .into_iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        assert_eq!(
            files,
            vec![
                "snippets",
                "snippets.json",
                "user-snippets.json",
                "a.code-snippets",
                "z.code-snippets",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_user_snippets_merges_legacy_json_and_code_snippets() {
        let dir = std::env::temp_dir().join(format!("mui-snippet-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("snippets");
        let json = dir.join("snippets.json");
        let vscode = dir.join("react.code-snippets");
        std::fs::write(&legacy, "guard\tguard\tif ${1:cond} {\\n  return\\n}$0\n").unwrap();
        std::fs::write(
            &json,
            r#"{"Console": {"prefix": "log", "body": "console.log($0)"}}"#,
        )
        .unwrap();
        std::fs::write(
            &vscode,
            r#"{"Component": {"prefix": "comp", "body": "class $0"}}"#,
        )
        .unwrap();

        let defs = load_user_snippets_from_paths(vec![legacy, json, vscode]);
        let prefixes: Vec<&str> = defs.iter().map(|def| def.prefix.as_str()).collect();
        assert_eq!(prefixes, vec!["guard", "log", "comp"]);
        let _ = std::fs::remove_dir_all(&dir);
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
