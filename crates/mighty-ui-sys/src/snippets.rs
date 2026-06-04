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
use std::path::{Path, PathBuf};

/// One parsed piece of a snippet body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Literal text (may contain `\n` for multi-line bodies).
    Text(String),
    /// A VS Code-style snippet variable such as `$TM_FILENAME` or
    /// `${TM_FILENAME_BASE:default}`.
    Variable { name: String, default: Option<String>, braced: bool },
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
                if let Some((name, default, consumed)) = parse_braced_variable(&chars[i..]) {
                    flush(&mut segs, &mut text);
                    segs.push(Segment::Variable { name, default, braced: true });
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
        n = n.saturating_mul(10).saturating_add(chars[j] as u32 - '0' as u32);
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
    workspace_root: Option<PathBuf>,
    current_line: Option<String>,
    line_index: Option<usize>,
    current_word: String,
    date: Option<DateParts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateParts {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    weekday: u8,
}

impl DateParts {
    fn local_now() -> Self {
        local_date_parts()
    }
}

impl SnippetContext {
    pub fn from_path(path: Option<&Path>) -> Self {
        SnippetContext::from_path_selection_and_workspace(path, "", None)
    }

    pub fn from_path_and_selection(path: Option<&Path>, selected_text: &str) -> Self {
        SnippetContext::from_path_selection_and_workspace(path, selected_text, None)
    }

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
            workspace_root: workspace_root.map(Path::to_path_buf),
            current_line: current_line.map(str::to_string),
            line_index,
            current_word: current_word.to_string(),
            date,
        }
    }
}

/// Expand a snippet `body` inserted at document position `(cur_line, cur_col)`,
/// where `indent` is the leading whitespace of the call-site line (continuation
/// lines are prefixed with it). Returns the literal insert text + resolved stops.
///
/// Tab-stop positions are computed by walking the body segments and tracking the
/// running `(line, col)` offset from the insertion point, accounting for the
/// per-line indent added to continuation lines.
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
            Segment::Variable { name, default, braced } => {
                let value = resolve_variable_with_default(name, default.as_deref(), context)
                    .unwrap_or_else(|| unresolved_variable_literal(name, *braced));
                emit(&value, &mut text, &mut line, &mut col);
            }
            Segment::Stop { num, placeholder } => {
                let start = (line, col);
                let placeholder = resolve_variables_in_text(placeholder, context);
                emit(&placeholder, &mut text, &mut line, &mut col);
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

fn resolve_variables_in_text(text: &str, context: &SnippetContext) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
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

fn resolve_variable_with_default(
    name: &str,
    default: Option<&str>,
    context: &SnippetContext,
) -> Option<String> {
    match resolve_snippet_variable(name, context) {
        Some(value) if value.is_empty() => {
            default.map(|value| resolve_variables_in_text(value, context)).or(Some(value))
        }
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
        "TM_CURRENT_LINE" => context.current_line.clone(),
        "TM_CURRENT_WORD" => Some(context.current_word.clone()),
        "TM_LINE_INDEX" => context.line_index.map(|line| line.to_string()),
        "TM_LINE_NUMBER" => context.line_index.map(|line| (line + 1).to_string()),
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
            .or_else(|| context.workspace_root.as_ref().map(|_| "workspace".to_string())),
        "RELATIVE_FILEPATH" => relative_filepath(context),
        _ => None,
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
    if short { SHORT[idx] } else { LONG[idx] }
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
    if short { SHORT[idx] } else { LONG[idx] }
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
        weekday: t.wDayOfWeek as u8,
    }
}

#[cfg(not(windows))]
fn local_date_parts() -> DateParts {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
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
        weekday: ((days + 4).rem_euclid(7)) as u8,
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
        self.nav.get(self.cur).and_then(|i| self.stops.get(*i)).copied()
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
    chars[start.1.min(chars.len())..end.1.min(chars.len())].iter().collect()
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
    let root = serde_json::from_str::<serde_json::Value>(text).ok()?;
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
        let label = entry
            .get("description")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(name);
        for prefix in prefixes {
            out.push(SnippetDef::new(&prefix, label, &body));
        }
    }
    Some(out)
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
    let exp = if active_path.is_none() && selected_text.is_empty() && workspace_root.is_none() {
        expand(&def.body, &indent, cl, cc)
    } else if selected_text.is_empty() && workspace_root.is_none() {
        let context = SnippetContext::from_path(active_path);
        expand_with_context(&def.body, &indent, cl, cc, &context)
    } else if workspace_root.is_none() {
        let context = SnippetContext::from_path_and_selection(active_path, selected_text);
        expand_with_context(&def.body, &indent, cl, cc, &context)
    } else {
        let context = SnippetContext::from_editor_context(
            active_path,
            selected_text,
            workspace_root,
            Some(&current_line),
            Some(line),
            &word,
            None,
        );
        expand_with_context(&def.body, &indent, cl, cc, &context)
    };
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
    fn parse_choice_placeholder_uses_first_choice() {
        let segs = parse_body("color: ${1|red,green,blue|};");
        assert_eq!(
            segs,
            vec![
                Segment::Text("color: ".into()),
                Segment::Stop { num: 1, placeholder: "red".into() },
                Segment::Text(";".into()),
            ]
        );
    }

    #[test]
    fn parse_choice_placeholder_honors_escaped_separators() {
        let segs = parse_body("${1|one\\, two,pipe\\|value,slash\\\\value|}");
        assert_eq!(segs, vec![Segment::Stop { num: 1, placeholder: "one, two".into() }]);
    }

    #[test]
    fn parse_placeholder_honors_escaped_brace_dollar_and_backslash() {
        let segs = parse_body("${1:a\\}b \\$c \\\\ d}!");
        assert_eq!(
            segs,
            vec![
                Segment::Stop { num: 1, placeholder: "a}b $c \\ d".into() },
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
                Segment::Stop { num: 1, placeholder: "name = value".into() },
                Segment::Text(";".into()),
            ]
        );
    }

    #[test]
    fn parse_bare_nested_tab_stops_do_not_leak_into_placeholder_text() {
        let segs = parse_body("${1:call($2)}");
        assert_eq!(segs, vec![Segment::Stop { num: 1, placeholder: "call()".into() }]);
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
    fn expand_choice_placeholder_selects_inserted_first_choice() {
        let exp = expand("kind ${1|error,warning,info|} $0", "", 0, 0);
        assert_eq!(exp.text, "kind error ");
        assert_eq!(exp.stops[0], Stop { num: 1, start: (0, 5), end: (0, 10) });
        assert_eq!(exp.stops[1], Stop { num: 0, start: (0, 11), end: (0, 11) });
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
        assert_eq!(exp.stops[0], Stop { num: 1, start: (0, 18), end: (0, 26) });
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
        let ctx = SnippetContext::from_editor_context(
            None,
            "",
            None,
            Some("  "),
            Some(0),
            "",
            None,
        );
        let exp = expand_with_context("${TM_CURRENT_WORD:name}|$TM_CURRENT_WORD", "", 0, 0, &ctx);
        assert_eq!(exp.text, "name|");
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
                weekday: 4,
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
            "$UNKNOWN ${UNKNOWN} $TM_FILENAME ${TM_FILENAME} $TM_SELECTED_TEXT",
            "",
            0,
            0,
        );
        assert_eq!(exp.text, "$UNKNOWN ${UNKNOWN} $TM_FILENAME ${TM_FILENAME} ");
    }

    #[test]
    fn expand_variables_inside_placeholders_update_selection_range() {
        let path = std::path::Path::new("C:/work/src/main.mty");
        let ctx = SnippetContext::from_path(Some(path));
        let exp = expand_with_context("${1:${TM_FILENAME_BASE:default}}_test $0", "", 0, 0, &ctx);
        assert_eq!(exp.text, "main_test ");
        assert_eq!(exp.stops[0], Stop { num: 1, start: (0, 0), end: (0, 4) });
        assert_eq!(exp.stops[1], Stop { num: 0, start: (0, 10), end: (0, 10) });
    }

    #[test]
    fn expand_nested_placeholder_selects_flattened_default() {
        let exp = expand("let ${1:${2:name}: ${3:Type}} = $0", "", 0, 0);
        assert_eq!(exp.text, "let name: Type = ");
        assert_eq!(exp.stops[0], Stop { num: 1, start: (0, 4), end: (0, 14) });
        assert_eq!(exp.stops[1], Stop { num: 0, start: (0, 17), end: (0, 17) });
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
    fn session_skips_duplicate_mirror_stops_for_navigation() {
        let stops = vec![
            Stop { num: 1, start: (0, 0), end: (0, 1) },
            Stop { num: 1, start: (0, 5), end: (0, 6) },
            Stop { num: 2, start: (0, 10), end: (0, 11) },
            Stop { num: 0, start: (0, 12), end: (0, 12) },
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
    fn parse_user_snippets_invalid_json_is_empty() {
        assert!(parse_user_snippets("{not json").is_empty());
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
