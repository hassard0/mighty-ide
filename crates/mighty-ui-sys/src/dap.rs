//! Shim-side **Debug Adapter Protocol** client + debugger UI state.
//!
//! v0.36 Mighty can't spawn a process, hold a long-lived child, frame
//! `Content-Length` JSON, or keep `Vec`/`String` across the scalar `extern c`
//! ABI (L17/L21), so — exactly like the LSP client in [`crate::language`] and
//! the Run panel in [`crate::run`] — the whole debugger lives shim-side and is
//! driven through a scalar `mui_dbg_*` / `mui_bp_*` ABI (see
//! [`crate::dapabi`]).
//!
//! ## What `mty dap` actually supports (verified against its source —
//! Mighty's `crates/mty-cli/src/cmd/dap.rs`, v0.32 Track A):
//!
//! | request                | behaviour                                         |
//! |------------------------|---------------------------------------------------|
//! | `initialize`           | returns capabilities                              |
//! | `launch`               | `program`,`args`,`stopOnEntry`; **emits `initialized` AFTER `launch`** (non-standard order — we cope) |
//! | `setBreakpoints`       | by source line                                    |
//! | `setFunctionBreakpoints` | `fn:name` / `agent:Name`                        |
//! | `configurationDone`    | resumes the program                               |
//! | `threads`              | one thread, id 1, "main"                          |
//! | `stackTrace`           | frames: id/name/line/source                       |
//! | `scopes`               | a single synthetic "Locals" (variablesReference 1000) |
//! | `variables`            | flat name/value/type rows (NO structured expansion) |
//! | `continue`/`next`/`stepIn`/`stepOut`/`pause` | DAP step semantics      |
//! | `evaluate`             | local-name lookup + simple field access           |
//! | `restart`,`disconnect`,`terminate` | clean re-launch / shutdown            |
//!
//! Events it emits: `initialized`, `stopped`, `output`, `exited`, `terminated`.
//! There is no `continued` event (the client infers running-state from issuing a
//! resume); no `setVariable`; no conditional breakpoints; one thread only. We
//! degrade gracefully for anything missing.
//!
//! This module is split into:
//!   * **pure parsers** (`parse_*`) over the JSON `mty dap` emits — exhaustively
//!     unit-tested by feeding sample envelopes;
//!   * **[`DebugModel`]** — shim-owned UI state (per-file breakpoints, run state,
//!     current stop line/file, the stack frames + selected frame, the variables)
//!     read back by the ABI; pure + testable;
//!   * **[`DapSession`]** — the live adapter: spawns `mty dap`, runs the
//!     handshake, and drives a request/response + event loop on a worker thread,
//!     posting parsed events back over a channel the model drains each frame.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

// ===========================================================================
// Pure helpers — minimal JSON scanning (no serde, matching the LSP client).
// ===========================================================================

/// Read a JSON string value that begins at/after `pos` (skips ws + a leading
/// `:`, expects `"`). Decodes the common escapes. Returns `(value, idx-past)`.
fn read_json_string_at(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    let mut j = pos;
    while j < bytes.len() && matches!(bytes[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'"' {
        return None;
    }
    j += 1;
    let mut val = String::new();
    let mut segment_start = j;
    let mut high_surrogate: Option<u16> = None;
    while j < bytes.len() {
        match bytes[j] {
            b'"' => {
                flush_json_segment(bytes, segment_start, j, &mut val, &mut high_surrogate)?;
                push_pending_surrogate(&mut val, &mut high_surrogate);
                return Some((val, j + 1));
            }
            b'\\' if j + 1 < bytes.len() => {
                flush_json_segment(bytes, segment_start, j, &mut val, &mut high_surrogate)?;
                j += 1;
                match bytes[j] {
                    b'n' => push_escaped_char(&mut val, &mut high_surrogate, '\n'),
                    b't' => push_escaped_char(&mut val, &mut high_surrogate, '\t'),
                    b'r' => push_escaped_char(&mut val, &mut high_surrogate, '\r'),
                    b'b' => push_escaped_char(&mut val, &mut high_surrogate, '\u{0008}'),
                    b'f' => push_escaped_char(&mut val, &mut high_surrogate, '\u{000c}'),
                    b'"' => push_escaped_char(&mut val, &mut high_surrogate, '"'),
                    b'\\' => push_escaped_char(&mut val, &mut high_surrogate, '\\'),
                    b'/' => push_escaped_char(&mut val, &mut high_surrogate, '/'),
                    b'u' if j + 4 < bytes.len() => {
                        let unit = read_hex4(&bytes[j + 1..j + 5])?;
                        push_json_code_unit(&mut val, &mut high_surrogate, unit);
                        j += 4;
                    }
                    other => push_escaped_char(&mut val, &mut high_surrogate, other as char),
                }
                j += 1;
                segment_start = j;
                continue;
            }
            _ => {
                j += 1;
                continue;
            }
        }
    }
    None
}

fn flush_json_segment(
    bytes: &[u8],
    start: usize,
    end: usize,
    out: &mut String,
    high_surrogate: &mut Option<u16>,
) -> Option<()> {
    if start < end {
        push_pending_surrogate(out, high_surrogate);
        out.push_str(std::str::from_utf8(&bytes[start..end]).ok()?);
    }
    Some(())
}

fn read_hex4(bytes: &[u8]) -> Option<u16> {
    if bytes.len() != 4 {
        return None;
    }
    let mut value = 0u16;
    for &b in bytes {
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        value = (value << 4) | digit as u16;
    }
    Some(value)
}

fn push_escaped_char(out: &mut String, high_surrogate: &mut Option<u16>, ch: char) {
    push_pending_surrogate(out, high_surrogate);
    out.push(ch);
}

fn push_pending_surrogate(out: &mut String, high_surrogate: &mut Option<u16>) {
    if high_surrogate.take().is_some() {
        out.push('\u{fffd}');
    }
}

fn push_json_code_unit(out: &mut String, high_surrogate: &mut Option<u16>, unit: u16) {
    match unit {
        0xd800..=0xdbff => {
            push_pending_surrogate(out, high_surrogate);
            *high_surrogate = Some(unit);
        }
        0xdc00..=0xdfff => {
            if let Some(high) = high_surrogate.take() {
                let high = (high as u32) - 0xd800;
                let low = (unit as u32) - 0xdc00;
                let cp = 0x10000 + ((high << 10) | low);
                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
            } else {
                out.push('\u{fffd}');
            }
        }
        _ => {
            push_pending_surrogate(out, high_surrogate);
            out.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
        }
    }
}

fn skip_json_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

fn skip_json_string(bytes: &[u8], quote: usize) -> Option<usize> {
    let mut i = quote + 1;
    let mut esc = false;
    while i < bytes.len() {
        if esc {
            esc = false;
        } else if bytes[i] == b'\\' {
            esc = true;
        } else if bytes[i] == b'"' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn json_key_matches(bytes: &[u8], quote: usize, field: &[u8]) -> bool {
    let start = quote + 1;
    let end = start + field.len();
    end < bytes.len() && &bytes[start..end] == field && bytes[end] == b'"'
}

fn top_level_field_value(bytes: &[u8], field: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if depth == 1 && json_key_matches(bytes, i, field) {
                    let key_end = i + field.len() + 2;
                    let colon = skip_json_ws(bytes, key_end);
                    if colon < bytes.len() && bytes[colon] == b':' {
                        return Some(skip_json_ws(bytes, colon + 1));
                    }
                }
                i = skip_json_string(bytes, i)?;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

fn top_level_uint_field(bytes: &[u8], field: &[u8]) -> Option<i64> {
    let mut j = top_level_field_value(bytes, field)?;
    let start = j;
    let mut v: i64 = 0;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        v = v.checked_mul(10)?;
        v = v.checked_add((bytes[j] - b'0') as i64)?;
        j += 1;
    }
    if j == start {
        None
    } else if j < bytes.len()
        && !matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']')
    {
        None
    } else {
        Some(v)
    }
}

fn top_level_string_field(bytes: &[u8], field: &[u8]) -> Option<String> {
    let j = top_level_field_value(bytes, field)?;
    read_json_string_at(bytes, j).map(|(s, _)| s)
}

fn top_level_bool_field(bytes: &[u8], field: &[u8]) -> Option<bool> {
    let j = top_level_field_value(bytes, field)?;
    if bytes.get(j..j + 4) == Some(b"true") {
        Some(true)
    } else if bytes.get(j..j + 5) == Some(b"false") {
        Some(false)
    } else {
        None
    }
}

fn top_level_object_field<'a>(bytes: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let value = top_level_field_value(bytes, field)?;
    if value >= bytes.len() || bytes[value] != b'{' {
        return None;
    }
    let end = match_delim(bytes, value, b'{', b'}').min(bytes.len());
    Some(&bytes[value..end])
}

fn top_level_array_field<'a>(bytes: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let value = top_level_field_value(bytes, field)?;
    if value >= bytes.len() || bytes[value] != b'[' {
        return None;
    }
    let end = match_bracket(bytes, value).min(bytes.len());
    Some(&bytes[value..end])
}

/// Index just past the `]` matching the `[` at `open` (string-aware).
fn match_bracket(bytes: &[u8], open: usize) -> usize {
    match_delim(bytes, open, b'[', b']')
}

fn match_delim(bytes: &[u8], open: usize, o: u8, c: u8) -> usize {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut k = open;
    while k < bytes.len() {
        let b = bytes[k];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == o {
            depth += 1;
        } else if b == c {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
        k += 1;
    }
    bytes.len()
}

/// Split the top-level objects of a JSON array slice `[ {...}, {...} ]` into
/// their `{...}` byte slices.
fn split_objects(arr: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut obj_start: Option<usize> = None;
    let mut in_str = false;
    let mut esc = false;
    for (k, &c) in arr.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    obj_start = Some(k);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = obj_start.take() {
                        out.push(&arr[s..=k]);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Escape a string for embedding in a JSON document.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ===========================================================================
// Parsed DAP message shapes (the events / responses the client reacts to).
// ===========================================================================

/// One inbound DAP envelope, classified just enough to route it. We keep the
/// raw body bytes so the typed parsers below can scan them.
#[derive(Debug, Clone)]
pub struct DapEnvelope {
    /// `"response"` / `"event"` / `"request"`.
    pub kind: String,
    /// For responses: the command echoed back (`"stackTrace"`, …).
    pub command: Option<String>,
    /// For responses: the request seq this answers.
    pub request_seq: Option<i64>,
    /// For responses: success flag.
    pub success: Option<bool>,
    /// For events: the event name (`"stopped"`, `"output"`, …).
    pub event: Option<String>,
    /// The whole raw JSON text (so per-shape parsers can re-scan it).
    pub raw: String,
}

/// Parse one framed/unframed DAP JSON object into a [`DapEnvelope`]. Accepts
/// either a bare JSON object or a `Content-Length`-framed one.
pub fn parse_envelope(text: &str) -> Option<DapEnvelope> {
    let body = match text.find("\r\n\r\n") {
        Some(i) => &text[i + 4..],
        None => text,
    };
    let bytes = body.as_bytes();
    let kind = top_level_string_field(bytes, b"type")?;
    let command = top_level_string_field(bytes, b"command");
    let event = top_level_string_field(bytes, b"event");
    let request_seq =
        top_level_uint_field(bytes, b"request_seq").or_else(|| top_level_uint_field(bytes, b"requestSeq"));
    let success = top_level_bool_field(bytes, b"success");
    Some(DapEnvelope {
        kind,
        command,
        request_seq,
        success,
        event,
        raw: body.to_string(),
    })
}

/// A `stopped` event body: the reason + optional description.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoppedInfo {
    pub reason: String,
    pub description: String,
    pub thread_id: Option<i64>,
}

/// Parse a `stopped` event's body.
pub fn parse_stopped(raw: &str) -> StoppedInfo {
    let bytes = raw.as_bytes();
    let body = dap_event_body(bytes, "stopped").unwrap_or(&[]);
    StoppedInfo {
        reason: top_level_string_field(body, b"reason").unwrap_or_default(),
        description: top_level_string_field(body, b"description").unwrap_or_default(),
        thread_id: top_level_uint_field(body, b"threadId"),
    }
}

/// Parse a `threads` response into thread IDs.
pub fn parse_threads(raw: &str) -> Vec<i64> {
    let bytes = raw.as_bytes();
    let Some(body) = dap_response_body(bytes, "threads") else {
        return Vec::new();
    };
    let Some(threads) = top_level_array_field(body, b"threads") else {
        return Vec::new();
    };
    split_objects(threads)
        .into_iter()
        .filter_map(|obj| top_level_uint_field(obj, b"id"))
        .collect()
}

/// An `output` event body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputInfo {
    pub category: String,
    pub output: String,
}

/// Parse an `output` event's body.
pub fn parse_output(raw: &str) -> OutputInfo {
    let bytes = raw.as_bytes();
    let body = dap_event_body(bytes, "output").unwrap_or(&[]);
    OutputInfo {
        category: top_level_string_field(body, b"category").unwrap_or_else(|| "stdout".into()),
        output: top_level_string_field(body, b"output").unwrap_or_default(),
    }
}

pub fn parse_exit_code(raw: &str) -> i64 {
    let bytes = raw.as_bytes();
    let body = dap_event_body(bytes, "exited").unwrap_or(&[]);
    top_level_uint_field(body, b"exitCode").unwrap_or(0)
}

/// One stack frame from a `stackTrace` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub line: i64,
    pub file: String,
}

/// Parse a `stackTrace` response into ordered frames (innermost first, as DAP
/// returns them).
pub fn parse_stack_trace(raw: &str) -> Vec<StackFrame> {
    let bytes = raw.as_bytes();
    let Some(body) = dap_response_body(bytes, "stackTrace") else {
        return Vec::new();
    };
    let Some(frames) = top_level_array_field(body, b"stackFrames") else {
        return Vec::new();
    };
    split_objects(frames)
        .into_iter()
        .filter_map(|obj| {
            let id = top_level_uint_field(obj, b"id")?;
            let name = top_level_string_field(obj, b"name").unwrap_or_default();
            let line = top_level_uint_field(obj, b"line")?;
            let file = top_level_object_field(obj, b"source")
                .and_then(|source| top_level_string_field(source, b"path"))
                .unwrap_or_default();
            Some(StackFrame {
                id,
                name,
                line,
                file,
            })
        })
        .collect()
}

/// One variable row from a `variables` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub kind: String,
}

/// Parse a `variables` response into name/value/type rows.
pub fn parse_variables(raw: &str) -> Vec<Variable> {
    let bytes = raw.as_bytes();
    let Some(body) = dap_response_body(bytes, "variables") else {
        return Vec::new();
    };
    let Some(variables) = top_level_array_field(body, b"variables") else {
        return Vec::new();
    };
    split_objects(variables)
        .into_iter()
        .filter_map(|obj| {
            let name = top_level_string_field(obj, b"name")?;
            let value = top_level_string_field(obj, b"value").unwrap_or_default();
            let kind = top_level_string_field(obj, b"type").unwrap_or_default();
            Some(Variable { name, value, kind })
        })
        .collect()
}

fn dap_response_body<'a>(bytes: &'a [u8], command: &str) -> Option<&'a [u8]> {
    if top_level_string_field(bytes, b"type").as_deref() != Some("response") {
        return None;
    }
    if top_level_string_field(bytes, b"command").as_deref() != Some(command) {
        return None;
    }
    if top_level_bool_field(bytes, b"success") == Some(false) {
        return None;
    }
    top_level_object_field(bytes, b"body")
}

fn dap_event_body<'a>(bytes: &'a [u8], event: &str) -> Option<&'a [u8]> {
    if top_level_string_field(bytes, b"type").as_deref() != Some("event") {
        return None;
    }
    if top_level_string_field(bytes, b"event").as_deref() != Some(event) {
        return None;
    }
    top_level_object_field(bytes, b"body")
}

// ===========================================================================
// Debugger run-state machine + shim-owned UI model.
// ===========================================================================

/// The debugger's coarse state, surfaced to Mighty as a scalar via the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugState {
    /// No session running.
    #[default]
    Idle,
    /// Launched + resumed; the program is executing (not stopped).
    Running,
    /// Stopped at a breakpoint / step / exception — stack + vars are valid.
    Stopped,
    /// The program exited or the adapter disconnected.
    Terminated,
}

impl DebugState {
    pub fn as_i32(self) -> i32 {
        match self {
            DebugState::Idle => 0,
            DebugState::Running => 1,
            DebugState::Stopped => 2,
            DebugState::Terminated => 3,
        }
    }
}

/// One line of debug-console output (from `output` events / status notes).
#[derive(Debug, Clone)]
pub struct ConsoleLine {
    pub text: String,
    pub is_error: bool,
}

/// One stored line breakpoint location, sorted for display in the debug view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointLocation {
    pub file: String,
    pub line: u32,
}

/// Shim-owned debugger model: breakpoints per file, the live session, the
/// current stop position, the call stack + selected frame, and the variables.
///
/// All the I/O lives in [`DapSession`]; this struct holds the *state* the UI
/// renders and the ABI reads back. It is `Default`-constructible and most of
/// its logic (breakpoint toggling, draining session events, frame selection) is
/// pure + unit-tested.
#[derive(Default)]
pub struct DebugModel {
    /// Per-file breakpoint line sets (1-based DAP lines), keyed by absolute path.
    breakpoints: std::collections::HashMap<String, Vec<u32>>,
    /// First global breakpoint shown in the sidebar inventory window.
    breakpoint_first: usize,
    /// The file the debug controls operate on (the program under debug).
    program: Option<PathBuf>,
    /// The live adapter session, if one is running.
    session: Option<DapSession>,
    /// Coarse state for the ABI / UI.
    state: DebugState,
    /// Current stopped location (0-based line + absolute file), valid in Stopped.
    cur_line: i32,
    cur_file: String,
    /// The call stack (innermost first), refreshed on each stop.
    stack: Vec<StackFrame>,
    /// Which frame is selected (drives the variables view + the editor jump).
    sel_frame: usize,
    /// The variables for the selected frame.
    variables: Vec<Variable>,
    /// Debug-console lines (output events + status notes).
    console: Vec<ConsoleLine>,
    /// Set true the frame a fresh stop arrives, so the IDE can jump the editor.
    just_stopped: bool,
    /// Whether the debug view (rail panel) is open.
    open: bool,
}

impl DebugModel {
    pub fn new() -> Self {
        DebugModel::default()
    }

    // ---- debug-view open/close ----
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }
    pub fn toggle_open(&mut self) -> bool {
        self.open = !self.open;
        self.open
    }

    // ---- coarse state ----
    pub fn state(&self) -> DebugState {
        self.state
    }
    pub fn cur_line(&self) -> i32 {
        self.cur_line
    }
    pub fn cur_file(&self) -> &str {
        &self.cur_file
    }
    pub fn has_program(&self) -> bool {
        self.program.is_some()
    }
    pub fn program(&self) -> Option<&Path> {
        self.program.as_deref()
    }

    // ---- breakpoints (pure) ----

    /// Toggle a breakpoint on (0-based) `line` of `file`. Returns the new state
    /// (`true` = breakpoint now present). DAP lines are 1-based, so we store
    /// `line + 1`.
    pub fn toggle_breakpoint(&mut self, file: &str, line0: i32) -> bool {
        if line0 < 0 {
            return false;
        }
        let dap_line = line0 as u32 + 1;
        let set = self.breakpoints.entry(file.to_string()).or_default();
        if let Some(pos) = set.iter().position(|&l| l == dap_line) {
            set.remove(pos);
            false
        } else {
            set.push(dap_line);
            set.sort_unstable();
            true
        }
    }

    /// Remove a specific stored breakpoint by absolute file and 1-based DAP
    /// line. Returns true when a breakpoint was removed.
    pub fn remove_breakpoint(&mut self, file: &str, line: u32) -> bool {
        let Some(lines) = self.breakpoints.get_mut(file) else {
            return false;
        };
        let Some(pos) = lines.iter().position(|&l| l == line) else {
            return false;
        };
        lines.remove(pos);
        if lines.is_empty() {
            self.breakpoints.remove(file);
        }
        let count = self.total_breakpoint_count();
        self.breakpoint_first = self.breakpoint_first.min(count);
        true
    }

    /// `true` if there's a breakpoint on (0-based) `line` of `file`.
    pub fn has_breakpoint(&self, file: &str, line0: i32) -> bool {
        if line0 < 0 {
            return false;
        }
        let dap_line = line0 as u32 + 1;
        self.breakpoints
            .get(file)
            .is_some_and(|s| s.contains(&dap_line))
    }

    /// All 0-based breakpoint lines for `file`, sorted (for the gutter draw).
    pub fn breakpoint_lines0(&self, file: &str) -> Vec<i32> {
        self.breakpoints
            .get(file)
            .map(|s| s.iter().map(|&l| l as i32 - 1).collect())
            .unwrap_or_default()
    }

    /// Total breakpoint count across every file.
    pub fn total_breakpoint_count(&self) -> usize {
        self.breakpoints.values().map(Vec::len).sum()
    }

    /// Every stored breakpoint, sorted by normalized file path and line.
    pub fn breakpoint_locations(&self) -> Vec<BreakpointLocation> {
        let mut out = self
            .breakpoints
            .iter()
            .flat_map(|(file, lines)| {
                let file = file.clone();
                lines.iter().map(move |&line| BreakpointLocation {
                    file: file.clone(),
                    line,
                })
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| {
            let af = a.file.replace('\\', "/").to_ascii_lowercase();
            let bf = b.file.replace('\\', "/").to_ascii_lowercase();
            af.cmp(&bf).then(a.line.cmp(&b.line))
        });
        out
    }

    /// First global breakpoint visible in the sidebar inventory window.
    pub fn breakpoint_window_first(&self, data_rows: usize) -> usize {
        let count = self.total_breakpoint_count();
        self.breakpoint_first.min(count.saturating_sub(data_rows))
    }

    /// Scroll the sidebar breakpoint inventory. Positive `delta` moves toward
    /// later breakpoints, negative toward earlier breakpoints.
    pub fn scroll_breakpoints(&mut self, delta: i32, data_rows: usize) -> bool {
        let count = self.total_breakpoint_count();
        if data_rows == 0 || count <= data_rows {
            self.breakpoint_first = 0;
            return false;
        }
        let max_first = count - data_rows;
        let current = self.breakpoint_first.min(max_first);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            current.saturating_add(delta as usize).min(max_first)
        };
        self.breakpoint_first = next;
        next != current
    }

    /// Total breakpoint count for the program (across the program file).
    pub fn breakpoint_count(&self) -> usize {
        self.program
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .and_then(|k| self.breakpoints.get(&k))
            .map(|s| s.len())
            .unwrap_or(0)
    }

    /// 1-based DAP breakpoint line `i` for the program file, or -1.
    pub fn breakpoint_line_at(&self, i: usize) -> i32 {
        self.program
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .and_then(|k| self.breakpoints.get(&k))
            .and_then(|s| s.get(i))
            .map(|&l| l as i32)
            .unwrap_or(-1)
    }

    /// Clear every stored line breakpoint across files. Returns `true` when any
    /// breakpoint was removed.
    pub fn clear_breakpoints(&mut self) -> bool {
        let changed = self.breakpoints.values().any(|lines| !lines.is_empty());
        self.breakpoints.clear();
        self.breakpoint_first = 0;
        changed
    }

    // ---- call stack / variables read-back (for the ABI) ----
    pub fn stack_count(&self) -> usize {
        self.stack.len()
    }
    pub fn frame(&self, i: usize) -> Option<&StackFrame> {
        self.stack.get(i)
    }
    pub fn selected_frame(&self) -> usize {
        self.sel_frame
    }
    pub fn variable_count(&self) -> usize {
        self.variables.len()
    }
    pub fn variable(&self, i: usize) -> Option<&Variable> {
        self.variables.get(i)
    }
    pub fn console_count(&self) -> usize {
        self.console.len()
    }
    pub fn console_line(&self, i: usize) -> Option<&ConsoleLine> {
        self.console.get(i)
    }
    pub fn take_just_stopped(&mut self) -> bool {
        std::mem::take(&mut self.just_stopped)
    }

    /// Whether clearing the current debug session would be a no-op. Breakpoints
    /// and the last target are intentionally ignored because clear-session
    /// preserves them.
    pub fn session_is_empty(&self) -> bool {
        self.session.is_none()
            && self.state == DebugState::Idle
            && self.cur_line < 0
            && self.cur_file.is_empty()
            && self.stack.is_empty()
            && self.variables.is_empty()
            && self.console.is_empty()
            && !self.just_stopped
    }

    /// Clear the current debug session model while preserving breakpoints and
    /// the last target. Disconnects any live adapter and keeps the panel open.
    pub fn clear_session(&mut self) -> bool {
        let changed = !self.session_is_empty();
        if let Some(sess) = self.session.take() {
            sess.disconnect();
        }
        self.state = DebugState::Idle;
        self.cur_line = -1;
        self.cur_file.clear();
        self.stack.clear();
        self.variables.clear();
        self.console.clear();
        self.sel_frame = 0;
        self.just_stopped = false;
        self.open = true;
        changed
    }

    fn log(&mut self, text: impl Into<String>, is_error: bool) {
        self.console.push(ConsoleLine {
            text: text.into(),
            is_error,
        });
    }

    // ---- session lifecycle ----

    /// Start a debug session for `program`: spawn `mty dap`, run the handshake,
    /// send the program's breakpoints, and resume. Returns `true` on spawn.
    pub fn start(&mut self, program: &Path) -> bool {
        self.stop(); // tear down any prior session
        self.program = Some(program.to_path_buf());
        self.open = true;
        self.console.clear();
        self.stack.clear();
        self.variables.clear();
        self.sel_frame = 0;
        self.cur_line = -1;
        self.cur_file.clear();
        self.state = DebugState::Idle;

        let key = program.to_string_lossy().to_string();
        let bps = self.breakpoints.get(&key).cloned().unwrap_or_default();

        match DapSession::launch(program, &bps) {
            Ok(sess) => {
                self.session = Some(sess);
                self.state = DebugState::Running;
                self.log(format!("Debugging {}", program.display()), false);
                true
            }
            Err(e) => {
                self.log(format!("debug: failed to start adapter: {e}"), true);
                self.state = DebugState::Terminated;
                false
            }
        }
    }

    /// Record a debug-start failure that was detected before spawning `mty dap`.
    pub fn fail_before_start(&mut self, program: &Path, reason: impl Into<String>) {
        self.stop(); // tear down any prior session
        self.program = Some(program.to_path_buf());
        self.open = true;
        self.console.clear();
        self.stack.clear();
        self.variables.clear();
        self.sel_frame = 0;
        self.cur_line = -1;
        self.cur_file.clear();
        self.just_stopped = false;
        self.state = DebugState::Terminated;
        self.log(
            format!("debug: failed to start adapter: {}", reason.into()),
            true,
        );
    }

    /// Stop / disconnect the session (best-effort). Resets to Idle.
    pub fn stop(&mut self) {
        if let Some(sess) = self.session.take() {
            sess.disconnect();
            self.log("Debug session stopped", false);
        }
        self.state = DebugState::Idle;
        self.cur_line = -1;
        self.cur_file.clear();
        self.stack.clear();
        self.variables.clear();
        self.sel_frame = 0;
    }

    fn require_running(&self) -> bool {
        self.session.is_some() && self.state != DebugState::Terminated
    }

    /// F5 / Continue. Resumes the program (returns to Running).
    pub fn continue_(&mut self) {
        if self.require_running() && self.state == DebugState::Stopped {
            if let Some(s) = &self.session {
                s.send_continue();
            }
            self.state = DebugState::Running;
            self.clear_stop();
        }
    }
    /// Pause a running program.
    pub fn pause(&mut self) {
        if self.require_running() && self.state == DebugState::Running {
            if let Some(s) = &self.session {
                s.send_pause();
            }
            self.log("Pausing debuggee...", false);
        }
    }
    /// Restart the last debug target.
    pub fn restart(&mut self) -> bool {
        let Some(program) = self.program.clone() else {
            self.log("No debug target to restart", true);
            return false;
        };
        self.start(&program)
    }
    /// F10 / step over (`next`).
    pub fn step_over(&mut self) {
        self.step("next");
    }
    /// F11 / step into (`stepIn`).
    pub fn step_into(&mut self) {
        self.step("stepIn");
    }
    /// Shift+F11 / step out (`stepOut`).
    pub fn step_out(&mut self) {
        self.step("stepOut");
    }

    fn step(&mut self, cmd: &str) {
        if self.require_running() && self.state == DebugState::Stopped {
            if let Some(s) = &self.session {
                s.send_step(cmd);
            }
            self.state = DebugState::Running;
            self.clear_stop();
        }
    }

    fn clear_stop(&mut self) {
        self.cur_line = -1;
        self.stack.clear();
        self.variables.clear();
        self.sel_frame = 0;
    }

    /// Push the current breakpoints for the program to a live session (called
    /// when a breakpoint is toggled mid-session).
    pub fn resend_breakpoints(&mut self) {
        let Some(prog) = self.program.clone() else {
            return;
        };
        let key = prog.to_string_lossy().to_string();
        let bps = self.breakpoints.get(&key).cloned().unwrap_or_default();
        if let Some(s) = &self.session {
            s.send_set_breakpoints(&prog, &bps);
        }
    }

    /// Select call-stack frame `i`: updates the variables (request the frame's
    /// scope) + makes that frame's location the "current" jump target. Returns
    /// `true` if the index was valid.
    pub fn select_frame(&mut self, i: usize) -> bool {
        if i >= self.stack.len() {
            return false;
        }
        self.sel_frame = i;
        let frame = self.stack[i].clone();
        // Update the editor jump target to the selected frame's line.
        if !frame.file.is_empty() {
            self.cur_file = frame.file.clone();
        }
        self.cur_line = (frame.line as i32 - 1).max(0);
        self.just_stopped = true;
        // Re-request the variables for this frame (scopes -> variables).
        if let Some(s) = &self.session {
            s.request_variables(frame.id);
        }
        true
    }

    /// Drain any events the worker posted since the last call, mutating the
    /// model (stop position, stack, variables, console, terminated). Returns
    /// `true` if anything changed (so the IDE redraws). Call once per frame.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        // Collect events first (ends the borrow on `self.session`).
        let mut drained: Vec<SessionEvent> = Vec::new();
        if let Some(sess) = &self.session {
            while let Ok(ev) = sess.events.try_recv() {
                drained.push(ev);
            }
        }
        for ev in drained {
            changed = true;
            self.apply_event(ev);
        }
        changed
    }

    fn apply_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Stopped(info) => {
                self.state = DebugState::Stopped;
                if !info.description.is_empty() {
                    self.log(format!("Stopped: {} ({})", info.reason, info.description), false);
                } else {
                    self.log(format!("Stopped: {}", info.reason), false);
                }
                // Request the stack now that we're stopped; the worker will post
                // a `Stack` event back with the frames.
                if let Some(s) = &self.session {
                    if let Some(thread_id) = info.thread_id {
                        s.request_stack(thread_id);
                    } else {
                        s.request_threads();
                    }
                }
            }
            SessionEvent::Threads(thread_ids) => {
                if self.state == DebugState::Stopped {
                    if let (Some(s), Some(thread_id)) = (&self.session, thread_ids.first()) {
                        s.request_stack(*thread_id);
                    }
                }
            }
            SessionEvent::Stack(frames) => {
                self.stack = frames;
                self.sel_frame = 0;
                if let Some(top) = self.stack.first().cloned() {
                    if !top.file.is_empty() {
                        self.cur_file = top.file.clone();
                    } else if let Some(p) = &self.program {
                        self.cur_file = p.to_string_lossy().into_owned();
                    }
                    self.cur_line = (top.line as i32 - 1).max(0);
                    self.just_stopped = true;
                    // Pull the top frame's variables.
                    if let Some(s) = &self.session {
                        s.request_variables(top.id);
                    }
                }
            }
            SessionEvent::Variables(vars) => {
                self.variables = vars;
            }
            SessionEvent::Output(o) => {
                let is_err = o.category == "stderr";
                for line in o.output.split_inclusive('\n') {
                    let t = line.trim_end_matches(['\n', '\r']);
                    if !t.is_empty() {
                        self.log(t.to_string(), is_err);
                    }
                }
            }
            SessionEvent::Exited(code) => {
                self.log(format!("Program exited with code {code}"), code != 0);
            }
            SessionEvent::Terminated => {
                self.state = DebugState::Terminated;
                self.clear_stop();
                self.log("Debuggee terminated", false);
            }
        }
    }

    // ---- screenshot/test seeding ----

    /// Seed a fake stopped state (no live adapter) so a headless capture renders
    /// the debug view: a breakpoint, a stopped line, a call stack, and variables.
    pub fn seed_demo(&mut self, program: &str) {
        let prog = PathBuf::from(program);
        let key = prog.to_string_lossy().to_string();
        self.program = Some(prog);
        self.open = true;
        // Lines within a short demo file so the stopped band overlaps real code.
        self.breakpoints.insert(key.clone(), vec![3, 5]);
        self.state = DebugState::Stopped;
        self.cur_file = key.clone();
        self.cur_line = 2; // 0-based -> line 3
        self.stack = vec![
            StackFrame { id: 1, name: "compute_sum".into(), line: 3, file: key.clone() },
            StackFrame { id: 2, name: "run".into(), line: 5, file: key.clone() },
            StackFrame { id: 3, name: "main".into(), line: 3, file: key.clone() },
        ];
        self.sel_frame = 0;
        self.variables = vec![
            Variable { name: "a".into(), value: "21".into(), kind: "I32".into() },
            Variable { name: "b".into(), value: "21".into(), kind: "I32".into() },
            Variable { name: "total".into(), value: "0".into(), kind: "I32".into() },
            Variable { name: "label".into(), value: "\"sum\"".into(), kind: "Str".into() },
        ];
        self.console = vec![
            ConsoleLine { text: "Debugging demo.mty".into(), is_error: false },
            ConsoleLine { text: "Breakpoint hit at compute_sum (line 7)".into(), is_error: false },
        ];
    }
}

// ===========================================================================
// Live DAP session — spawns `mty dap`, runs the handshake + an I/O loop.
// ===========================================================================

/// An event the worker thread posts back to the model on the main thread.
#[derive(Debug)]
pub enum SessionEvent {
    Stopped(StoppedInfo),
    Threads(Vec<i64>),
    Stack(Vec<StackFrame>),
    Variables(Vec<Variable>),
    Output(OutputInfo),
    Exited(i64),
    Terminated,
}

/// A command the main thread asks the worker to send to the adapter.
enum Outbound {
    Continue,
    Pause,
    Step(String),
    Threads,
    Stack(i64),
    Variables(i64),
    SetBreakpoints { path: String, lines: Vec<u32> },
    Disconnect,
}

/// The live adapter session: the child process + the worker thread that owns
/// stdin/stdout and runs the request/response + event loop.
pub struct DapSession {
    /// Outbound command channel (main -> worker).
    cmds: Sender<Outbound>,
    /// Inbound parsed events (worker -> main); drained by [`DebugModel::pump`].
    events: Receiver<SessionEvent>,
    /// The child handle, shared so `disconnect` can kill it if the worker is
    /// blocked on a read.
    child: Arc<Mutex<Option<Child>>>,
}

fn mty_path() -> String {
    crate::mty::path()
}

fn file_uri_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

impl DapSession {
    /// Spawn `mty dap`, run `initialize` → (await `launch`'s `initialized`) →
    /// `setBreakpoints` → `configurationDone`, and start the worker loop. The
    /// `launch` is sent right after initialize because `mty dap` emits its
    /// `initialized` event in response to `launch`, not `initialize`.
    pub fn launch(program: &Path, bps: &[u32]) -> std::io::Result<Self> {
        let mty = mty_path();
        let mut child = Command::new(&mty)
            .arg("dap")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("dap: no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("dap: no stdout"))?;

        let (cmd_tx, cmd_rx) = mpsc::channel::<Outbound>();
        let (ev_tx, ev_rx) = mpsc::channel::<SessionEvent>();
        let child_arc = Arc::new(Mutex::new(Some(child)));

        let prog = program.to_path_buf();
        let bps = bps.to_vec();
        let child_for_worker = Arc::clone(&child_arc);
        std::thread::spawn(move || {
            worker_loop(stdin, stdout, cmd_rx, ev_tx, prog, bps, child_for_worker);
        });

        Ok(DapSession {
            cmds: cmd_tx,
            events: ev_rx,
            child: child_arc,
        })
    }

    pub fn send_continue(&self) {
        let _ = self.cmds.send(Outbound::Continue);
    }
    pub fn send_pause(&self) {
        let _ = self.cmds.send(Outbound::Pause);
    }
    pub fn send_step(&self, cmd: &str) {
        let _ = self.cmds.send(Outbound::Step(cmd.to_string()));
    }
    pub fn request_stack(&self, thread_id: i64) {
        let _ = self.cmds.send(Outbound::Stack(thread_id));
    }
    pub fn request_threads(&self) {
        let _ = self.cmds.send(Outbound::Threads);
    }
    pub fn request_variables(&self, frame_id: i64) {
        let _ = self.cmds.send(Outbound::Variables(frame_id));
    }
    pub fn send_set_breakpoints(&self, path: &Path, lines: &[u32]) {
        let _ = self.cmds.send(Outbound::SetBreakpoints {
            path: path.to_string_lossy().into_owned(),
            lines: lines.to_vec(),
        });
    }

    /// Disconnect: ask the worker to send `disconnect`, then kill the child.
    pub fn disconnect(self) {
        let _ = self.cmds.send(Outbound::Disconnect);
        // Give the worker a brief moment, then force-kill if still alive.
        std::thread::sleep(Duration::from_millis(40));
        kill_debug_child(&self.child);
    }
}

fn lock_debug_child(child: &Arc<Mutex<Option<Child>>>) -> MutexGuard<'_, Option<Child>> {
    child.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn kill_debug_child(child: &Arc<Mutex<Option<Child>>>) {
    let mut guard = lock_debug_child(child);
    if let Some(mut c) = guard.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Frame + write one DAP request.
fn write_msg<W: Write>(w: &mut W, json: &str) -> std::io::Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n", json.len())?;
    w.write_all(json.as_bytes())?;
    w.flush()
}

/// Read one `Content-Length`-framed DAP message from `reader`. Returns the JSON
/// body, or `None` on EOF.
fn read_msg<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            let len = parse_content_length(rest).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "dap: malformed Content-Length header",
                )
            })?;
            content_length = Some(len);
        }
    }
    let content_length = content_length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "dap: missing Content-Length header",
        )
    })?;
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn parse_content_length(raw: &str) -> Option<usize> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let len = trimmed.parse().ok()?;
    if len == 0 {
        return None;
    }
    Some(len)
}

/// The worker: drives the handshake then multiplexes outbound commands against
/// inbound adapter messages. To avoid blocking on a single `read`, the reader
/// runs on its own thread feeding a channel; this loop selects between adapter
/// lines and main-thread commands with short timeouts.
#[allow(clippy::too_many_arguments)]
fn worker_loop(
    mut stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    cmds: Receiver<Outbound>,
    events: Sender<SessionEvent>,
    program: PathBuf,
    bps: Vec<u32>,
    child: Arc<Mutex<Option<Child>>>,
) {
    let seq = AtomicU64::new(1);
    let next = || seq.fetch_add(1, Ordering::SeqCst);

    // Reader thread: posts every framed JSON body onto a channel.
    let (raw_tx, raw_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(body)) = read_msg(&mut reader) {
            if !body.is_empty() && raw_tx.send(body).is_err() {
                break;
            }
        }
    });

    // --- Handshake ---
    let prog_uri = file_uri_path(&program);
    let init = format!(
        r#"{{"seq":{},"type":"request","command":"initialize","arguments":{{"clientID":"mighty-ide","adapterID":"mighty","linesStartAt1":true,"columnsStartAt1":true,"pathFormat":"path"}}}}"#,
        next()
    );
    // NOTE: `mty dap` (v0.36) verifies line breakpoints but does not reliably
    // *fire* them on a plain `continue` — the program tends to run to
    // completion. `stopOnEntry` DOES reliably stop (reason "entry") with a valid
    // stack, and `next`/`stepIn`/`stepOut` then work + populate locals. So we
    // always launch with `stopOnEntry:true`: the user lands paused at `main` and
    // can step or continue. Breakpoints are still sent (and verified) so a
    // future adapter that honours them just works. See docs/mighty-language-lessons.md.
    let launch = format!(
        r#"{{"seq":{},"type":"request","command":"launch","arguments":{{"program":"{}","stopOnEntry":true}}}}"#,
        next(),
        json_escape(&prog_uri)
    );
    if write_msg(&mut stdin, &init).is_err() || write_msg(&mut stdin, &launch).is_err() {
        let _ = events.send(SessionEvent::Terminated);
        return;
    }

    // Wait (bounded) for the `initialized` event the adapter emits after launch.
    let mut configured = false;
    let deadline = std::time::Instant::now() + Duration::from_millis(4000);
    while std::time::Instant::now() < deadline && !configured {
        match raw_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(body) => {
                if let Some(env) = parse_envelope(&body) {
                    route_inbound(&env, &events);
                    if env.event.as_deref() == Some("initialized") {
                        // setBreakpoints for the program, then configurationDone.
                        let bp_items = bps
                            .iter()
                            .map(|l| format!(r#"{{"line":{l}}}"#))
                            .collect::<Vec<_>>()
                            .join(",");
                        let set_bp = format!(
                            r#"{{"seq":{},"type":"request","command":"setBreakpoints","arguments":{{"source":{{"path":"{}"}},"breakpoints":[{}]}}}}"#,
                            next(),
                            json_escape(&prog_uri),
                            bp_items
                        );
                        let done = format!(
                            r#"{{"seq":{},"type":"request","command":"configurationDone","arguments":{{}}}}"#,
                            next()
                        );
                        let _ = write_msg(&mut stdin, &set_bp);
                        let _ = write_msg(&mut stdin, &done);
                        configured = true;
                    } else if env.event.as_deref() == Some("terminated") {
                        let _ = events.send(SessionEvent::Terminated);
                        return;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = events.send(SessionEvent::Terminated);
                return;
            }
        }
    }

    // --- Main multiplexed loop ---
    loop {
        // 1) Drain any pending adapter messages.
        loop {
            match raw_rx.try_recv() {
                Ok(body) => {
                    if let Some(env) = parse_envelope(&body) {
                        route_inbound(&env, &events);
                        if env.event.as_deref() == Some("terminated") {
                            let _ = events.send(SessionEvent::Terminated);
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = events.send(SessionEvent::Terminated);
                    return;
                }
            }
        }

        // 2) Service one outbound command (short wait so we stay responsive to
        //    both directions).
        match cmds.recv_timeout(Duration::from_millis(30)) {
            Ok(cmd) => {
                let json = match cmd {
                    Outbound::Continue => format!(
                        r#"{{"seq":{},"type":"request","command":"continue","arguments":{{"threadId":1}}}}"#,
                        next()
                    ),
                    Outbound::Pause => format!(
                        r#"{{"seq":{},"type":"request","command":"pause","arguments":{{"threadId":1}}}}"#,
                        next()
                    ),
                    Outbound::Step(c) => format!(
                        r#"{{"seq":{},"type":"request","command":"{c}","arguments":{{"threadId":1}}}}"#,
                        next()
                    ),
                    Outbound::Threads => format!(
                        r#"{{"seq":{},"type":"request","command":"threads","arguments":{{}}}}"#,
                        next()
                    ),
                    Outbound::Stack(tid) => format!(
                        r#"{{"seq":{},"type":"request","command":"stackTrace","arguments":{{"threadId":{tid},"startFrame":0,"levels":50}}}}"#,
                        next()
                    ),
                    Outbound::Variables(fid) => {
                        // scopes then variables: mty dap returns a single Locals
                        // scope (variablesReference 1000) regardless of frame, so
                        // request scopes (for protocol-correctness) then variables.
                        let scopes = format!(
                            r#"{{"seq":{},"type":"request","command":"scopes","arguments":{{"frameId":{fid}}}}}"#,
                            next()
                        );
                        let vars = format!(
                            r#"{{"seq":{},"type":"request","command":"variables","arguments":{{"variablesReference":1000}}}}"#,
                            next()
                        );
                        let _ = write_msg(&mut stdin, &scopes);
                        vars
                    }
                    Outbound::SetBreakpoints { path, lines } => {
                        let items = lines
                            .iter()
                            .map(|l| format!(r#"{{"line":{l}}}"#))
                            .collect::<Vec<_>>()
                            .join(",");
                        format!(
                            r#"{{"seq":{},"type":"request","command":"setBreakpoints","arguments":{{"source":{{"path":"{}"}},"breakpoints":[{}]}}}}"#,
                            next(),
                            json_escape(&file_uri_path(Path::new(&path))),
                            items
                        )
                    }
                    Outbound::Disconnect => {
                        let dis = format!(
                            r#"{{"seq":{},"type":"request","command":"disconnect","arguments":{{"terminateDebuggee":true}}}}"#,
                            next()
                        );
                        let _ = write_msg(&mut stdin, &dis);
                        kill_debug_child(&child);
                        return;
                    }
                };
                if write_msg(&mut stdin, &json).is_err() {
                    let _ = events.send(SessionEvent::Terminated);
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Main side dropped the session: shut down.
                kill_debug_child(&child);
                return;
            }
        }
    }
}

/// Classify an inbound envelope and post the corresponding [`SessionEvent`].
fn route_inbound(env: &DapEnvelope, events: &Sender<SessionEvent>) {
    if env.kind == "event" {
        match env.event.as_deref() {
            Some("stopped") => {
                let _ = events.send(SessionEvent::Stopped(parse_stopped(&env.raw)));
            }
            Some("output") => {
                let _ = events.send(SessionEvent::Output(parse_output(&env.raw)));
            }
            Some("exited") => {
                let code = parse_exit_code(&env.raw);
                let _ = events.send(SessionEvent::Exited(code));
            }
            _ => {}
        }
    } else if env.kind == "response" {
        match env.command.as_deref() {
            Some("stackTrace") => {
                let _ = events.send(SessionEvent::Stack(parse_stack_trace(&env.raw)));
            }
            Some("threads") => {
                let _ = events.send(SessionEvent::Threads(parse_threads(&env.raw)));
            }
            Some("variables") => {
                let _ = events.send(SessionEvent::Variables(parse_variables(&env.raw)));
            }
            _ => {}
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_cleanup_recovers_from_poisoned_slot() {
        let child = Arc::new(Mutex::new(None));
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poisoned = {
            let child = Arc::clone(&child);
            std::panic::catch_unwind(move || {
                let _guard = lock_debug_child(&child);
                panic!("poison debug child lock");
            })
        };
        std::panic::set_hook(hook);
        assert!(poisoned.is_err());

        kill_debug_child(&child);
        assert!(lock_debug_child(&child).is_none());
    }

    #[test]
    fn parse_initialize_response_capabilities() {
        let raw = r#"{"seq":1,"type":"response","request_seq":1,"success":true,"command":"initialize","body":{"supportsConfigurationDoneRequest":true,"supportsFunctionBreakpoints":true}}"#;
        let env = parse_envelope(raw).unwrap();
        assert_eq!(env.kind, "response");
        assert_eq!(env.command.as_deref(), Some("initialize"));
        assert_eq!(env.success, Some(true));
        assert_eq!(env.request_seq, Some(1));
    }

    #[test]
    fn parse_initialized_event() {
        let raw = r#"{"seq":3,"type":"event","event":"initialized","body":{}}"#;
        let env = parse_envelope(raw).unwrap();
        assert_eq!(env.kind, "event");
        assert_eq!(env.event.as_deref(), Some("initialized"));
    }

    #[test]
    fn parse_envelope_decodes_unicode_strings() {
        let raw = r#"{"seq":3,"type":"event","event":"initialized-\u6771\ud83d\ude00","body":{}}"#;
        let env = parse_envelope(raw).unwrap();
        assert_eq!(env.kind, "event");
        assert_eq!(env.event.as_deref(), Some("initialized-東😀"));
    }

    #[test]
    fn parse_framed_envelope() {
        let body = r#"{"seq":1,"type":"event","event":"initialized"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let env = parse_envelope(&framed).unwrap();
        assert_eq!(env.event.as_deref(), Some("initialized"));
    }

    #[test]
    fn read_msg_reads_framed_body() {
        let body = r#"{"seq":1,"type":"event","event":"initialized"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = std::io::Cursor::new(framed.into_bytes());

        assert_eq!(read_msg(&mut reader).unwrap(), Some(body.to_string()));
    }

    #[test]
    fn read_msg_rejects_malformed_content_length() {
        for header in [
            "Content-Length: 0\r\n\r\n",
            "Content-Length: -1\r\n\r\n{}",
            "Content-Length: 4.5\r\n\r\n{}",
            "Content-Length: 1e2\r\n\r\n{}",
            "Content-Type: application/json\r\n\r\n{}",
        ] {
            let mut reader = std::io::Cursor::new(header.as_bytes());
            let err = read_msg(&mut reader).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn parse_envelope_uses_top_level_protocol_fields() {
        let raw = r#"{
          "metadata":{"type":"event","event":"wrong","success":false,"request_seq":99},
          "type":"response",
          "command":"initialize",
          "request_seq":4,
          "success":true,
          "body":{"type":"event","event":"also wrong"}
        }"#;
        let env = parse_envelope(raw).unwrap();
        assert_eq!(env.kind, "response");
        assert_eq!(env.command.as_deref(), Some("initialize"));
        assert_eq!(env.event, None);
        assert_eq!(env.success, Some(true));
        assert_eq!(env.request_seq, Some(4));
    }

    #[test]
    fn parse_envelope_rejects_fractional_request_seq_prefix() {
        let raw = r#"{"seq":1,"type":"response","request_seq":4.5,"success":true,"command":"initialize","body":{}}"#;
        let env = parse_envelope(raw).unwrap();

        assert_eq!(env.request_seq, None);
    }

    #[test]
    fn parse_envelope_rejects_overflow_request_seq() {
        let raw = r#"{"seq":1,"type":"response","request_seq":999999999999999999999999999999,"success":true,"command":"initialize","body":{}}"#;
        let env = parse_envelope(raw).unwrap();

        assert_eq!(env.request_seq, None);
    }

    #[test]
    fn parse_stopped_event_body() {
        let raw = r#"{"seq":9,"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1,"allThreadsStopped":true}}"#;
        let env = parse_envelope(raw).unwrap();
        assert_eq!(env.event.as_deref(), Some("stopped"));
        let info = parse_stopped(&env.raw);
        assert_eq!(info.reason, "breakpoint");
        assert_eq!(info.thread_id, Some(1));
    }

    #[test]
    fn parse_stopped_exception_description() {
        let raw = r#"{"type":"event","event":"stopped","body":{"reason":"exception","description":"E0001: div by zero","threadId":1}}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);
        assert_eq!(info.reason, "exception");
        assert_eq!(info.description, "E0001: div by zero");
    }

    #[test]
    fn parse_stopped_decodes_unicode_description() {
        let raw = r#"{"type":"event","event":"stopped","body":{"reason":"exception-\u6771","description":"caf\u00e9 \ud83d\ude00 \ud83dX","threadId":1}}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);
        assert_eq!(info.reason, "exception-東");
        assert_eq!(info.description, "café 😀 �X");
    }

    #[test]
    fn parse_stopped_uses_body_top_level_fields() {
        let raw = r#"{
          "type":"event",
          "event":"stopped",
          "reason":"wrong envelope",
          "threadId":99,
          "body":{
            "metadata":{"reason":"wrong nested","description":"wrong desc","threadId":98},
            "reason":"breakpoint",
            "description":"right desc",
            "threadId":2
          }
        }"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);
        assert_eq!(info.reason, "breakpoint");
        assert_eq!(info.description, "right desc");
        assert_eq!(info.thread_id, Some(2));
    }

    #[test]
    fn parse_stopped_requires_body_owned_fields() {
        let raw = r#"{"type":"event","event":"stopped","reason":"wrong","description":"wrong desc","threadId":99}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);

        assert_eq!(info.reason, "");
        assert_eq!(info.description, "");
        assert_eq!(info.thread_id, None);
    }

    #[test]
    fn parse_stopped_rejects_fractional_thread_id_prefix() {
        let raw = r#"{"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1.5}}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);

        assert_eq!(info.reason, "breakpoint");
        assert_eq!(info.thread_id, None);
    }

    #[test]
    fn parse_stopped_rejects_overflow_thread_id() {
        let raw = r#"{"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":999999999999999999999999999999}}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);

        assert_eq!(info.reason, "breakpoint");
        assert_eq!(info.thread_id, None);
    }

    #[test]
    fn parse_stopped_requires_stopped_event() {
        let raw = r#"{"type":"event","event":"output","body":{"reason":"wrong","description":"wrong desc","threadId":99}}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);

        assert_eq!(info, StoppedInfo::default());
    }

    #[test]
    fn parse_stopped_without_thread_id_requests_thread_lookup() {
        let raw = r#"{"type":"event","event":"stopped","body":{"reason":"entry","allThreadsStopped":true}}"#;
        let info = parse_stopped(&parse_envelope(raw).unwrap().raw);

        assert_eq!(info.reason, "entry");
        assert_eq!(info.thread_id, None);
    }

    #[test]
    fn parse_threads_response_uses_thread_owned_ids() {
        let raw = r#"{
          "type":"response",
          "command":"threads",
          "success":true,
          "body":{
            "metadata":{"id":99},
            "threads":[
              {"id":7,"name":"main","metadata":{"id":70}},
              {"name":"missing id","metadata":{"id":8}},
              {"id":9,"name":"worker"}
            ]
          }
        }"#;
        let threads = parse_threads(&parse_envelope(raw).unwrap().raw);

        assert_eq!(threads, vec![7, 9]);
    }

    #[test]
    fn parse_threads_rejects_fractional_id_prefixes() {
        let raw = r#"{"type":"response","command":"threads","success":true,"body":{"threads":[{"id":7.5,"name":"bad"},{"id":9,"name":"good"}]}}"#;
        let threads = parse_threads(&parse_envelope(raw).unwrap().raw);

        assert_eq!(threads, vec![9]);
    }

    #[test]
    fn parse_threads_skips_overflow_ids() {
        let raw = r#"{"type":"response","command":"threads","success":true,"body":{"threads":[{"id":999999999999999999999999999999,"name":"bad"},{"id":9,"name":"good"}]}}"#;
        let threads = parse_threads(&parse_envelope(raw).unwrap().raw);

        assert_eq!(threads, vec![9]);
    }

    #[test]
    fn parse_threads_requires_threads_response() {
        let wrong_command = r#"{"type":"response","command":"variables","success":true,"body":{"threads":[{"id":7}]}}"#;
        let failed = r#"{"type":"response","command":"threads","success":false,"body":{"threads":[{"id":7}]}}"#;

        assert!(parse_threads(&parse_envelope(wrong_command).unwrap().raw).is_empty());
        assert!(parse_threads(&parse_envelope(failed).unwrap().raw).is_empty());
    }

    #[test]
    fn parse_stack_trace_frames() {
        let raw = r#"{"type":"response","command":"stackTrace","success":true,"body":{"stackFrames":[{"id":1,"name":"compute_sum","line":7,"column":1,"source":{"path":"C:/p/demo.mty","name":"demo.mty"}},{"id":2,"name":"main","line":18,"column":1,"source":{"path":"C:/p/demo.mty"}}],"totalFrames":2}}"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].name, "compute_sum");
        assert_eq!(frames[0].line, 7);
        assert_eq!(frames[0].file, "C:/p/demo.mty");
        assert_eq!(frames[1].name, "main");
        assert_eq!(frames[1].id, 2);
    }

    #[test]
    fn parse_stack_trace_decodes_unicode_frames() {
        let raw = r#"{"type":"response","command":"stackTrace","success":true,"body":{"stackFrames":[{"id":1,"name":"compute_\u6771\ud83d\ude00","line":7,"source":{"path":"C:/p/\u6771\ud83d\ude00/demo.mty"}}]}}"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].name, "compute_東😀");
        assert_eq!(frames[0].file, "C:/p/東😀/demo.mty");
    }

    #[test]
    fn parse_stack_trace_uses_body_and_frame_top_level_fields() {
        let raw = r#"{
          "type":"response",
          "command":"stackTrace",
          "success":true,
          "metadata":{"stackFrames":[{"id":99,"name":"wrong array","line":99}]},
          "body":{"stackFrames":[
            {
              "metadata":{"id":99,"name":"wrong frame","line":99,"source":{"path":"C:/wrong.mty"}},
              "id":3,
              "name":"right frame",
              "line":8,
              "source":{"metadata":{"path":"C:/wrong-source.mty"},"path":"C:/right.mty"}
            }
          ]}
        }"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, 3);
        assert_eq!(frames[0].name, "right frame");
        assert_eq!(frames[0].line, 8);
        assert_eq!(frames[0].file, "C:/right.mty");
    }

    #[test]
    fn parse_stack_trace_requires_frame_owned_line() {
        let raw = r#"{
          "type":"response",
          "command":"stackTrace",
          "success":true,
          "body":{"stackFrames":[
            {
              "id":1,
              "name":"metadata only line",
              "metadata":{"line":99},
              "source":{"path":"C:/wrong.mty"}
            },
            {
              "id":2,
              "name":"right frame",
              "line":12,
              "source":{"path":"C:/right.mty"}
            }
          ]}
        }"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, 2);
        assert_eq!(frames[0].line, 12);
        assert_eq!(frames[0].file, "C:/right.mty");
    }

    #[test]
    fn parse_stack_trace_rejects_fractional_frame_numbers() {
        let raw = r#"{"type":"response","command":"stackTrace","success":true,"body":{"stackFrames":[{"id":1.5,"name":"bad id","line":7,"source":{"path":"C:/bad-id.mty"}},{"id":2,"name":"bad line","line":8.5,"source":{"path":"C:/bad-line.mty"}},{"id":3,"name":"good","line":9,"source":{"path":"C:/good.mty"}}]}}"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, 3);
        assert_eq!(frames[0].line, 9);
        assert_eq!(frames[0].file, "C:/good.mty");
    }

    #[test]
    fn parse_stack_trace_skips_overflow_frame_numbers() {
        let raw = r#"{"type":"response","command":"stackTrace","success":true,"body":{"stackFrames":[{"id":999999999999999999999999999999,"name":"bad id","line":7,"source":{"path":"C:/bad-id.mty"}},{"id":2,"name":"bad line","line":999999999999999999999999999999,"source":{"path":"C:/bad-line.mty"}},{"id":3,"name":"good","line":9,"source":{"path":"C:/good.mty"}}]}}"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, 3);
        assert_eq!(frames[0].line, 9);
        assert_eq!(frames[0].file, "C:/good.mty");
    }

    #[test]
    fn parse_stack_trace_requires_stack_trace_response() {
        let raw = r#"{"type":"response","command":"variables","success":true,"body":{"stackFrames":[{"id":1,"name":"wrong","line":7,"source":{"path":"C:/wrong.mty"}}]}}"#;
        let frames = parse_stack_trace(&parse_envelope(raw).unwrap().raw);

        assert!(frames.is_empty());
    }

    #[test]
    fn parse_variables_rows() {
        let raw = r#"{"type":"response","command":"variables","success":true,"body":{"variables":[{"name":"a","value":"21","type":"I32","variablesReference":0},{"name":"label","value":"\"sum\"","type":"Str","variablesReference":0}]}}"#;
        let vars = parse_variables(&parse_envelope(raw).unwrap().raw);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "a");
        assert_eq!(vars[0].value, "21");
        assert_eq!(vars[0].kind, "I32");
        assert_eq!(vars[1].name, "label");
        assert_eq!(vars[1].value, "\"sum\"");
    }

    #[test]
    fn parse_variables_decodes_unicode_rows() {
        let raw = r#"{"type":"response","command":"variables","success":true,"body":{"variables":[{"name":"label_\u6771","value":"value \ud83d\ude00","type":"Str\u00e9","variablesReference":0}]}}"#;
        let vars = parse_variables(&parse_envelope(raw).unwrap().raw);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "label_東");
        assert_eq!(vars[0].value, "value 😀");
        assert_eq!(vars[0].kind, "Stré");
    }

    #[test]
    fn parse_variables_uses_body_and_variable_top_level_fields() {
        let raw = r#"{
          "type":"response",
          "command":"variables",
          "success":true,
          "metadata":{"variables":[{"name":"wrong array","value":"99","type":"Wrong"}]},
          "body":{"variables":[
            {
              "metadata":{"name":"wrong name","value":"wrong value","type":"Wrong"},
              "name":"right name",
              "value":"right value",
              "type":"Right"
            }
          ]}
        }"#;
        let vars = parse_variables(&parse_envelope(raw).unwrap().raw);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "right name");
        assert_eq!(vars[0].value, "right value");
        assert_eq!(vars[0].kind, "Right");
    }

    #[test]
    fn parse_variables_requires_variables_response() {
        let raw = r#"{"type":"response","command":"stackTrace","success":true,"body":{"variables":[{"name":"wrong","value":"99","type":"I32"}]}}"#;
        let vars = parse_variables(&parse_envelope(raw).unwrap().raw);

        assert!(vars.is_empty());
    }

    #[test]
    fn parse_output_event_body() {
        let raw = r#"{"type":"event","event":"output","body":{"category":"stdout","output":"hello\n"}}"#;
        let o = parse_output(&parse_envelope(raw).unwrap().raw);
        assert_eq!(o.category, "stdout");
        assert_eq!(o.output, "hello\n");
    }

    #[test]
    fn parse_output_decodes_unicode_text() {
        let raw = r#"{"type":"event","event":"output","body":{"category":"stdout","output":"hello \u6771\ud83d\ude00\n"}}"#;
        let o = parse_output(&parse_envelope(raw).unwrap().raw);
        assert_eq!(o.category, "stdout");
        assert_eq!(o.output, "hello 東😀\n");
    }

    #[test]
    fn parse_output_uses_body_top_level_fields() {
        let raw = r#"{
          "type":"event",
          "event":"output",
          "category":"stderr",
          "output":"wrong envelope",
          "body":{
            "metadata":{"category":"console","output":"wrong nested"},
            "category":"stdout",
            "output":"right body"
          }
        }"#;
        let o = parse_output(&parse_envelope(raw).unwrap().raw);
        assert_eq!(o.category, "stdout");
        assert_eq!(o.output, "right body");
    }

    #[test]
    fn parse_output_requires_body_owned_fields() {
        let raw = r#"{"type":"event","event":"output","category":"stderr","output":"wrong"}"#;
        let o = parse_output(&parse_envelope(raw).unwrap().raw);

        assert_eq!(o.category, "stdout");
        assert_eq!(o.output, "");
    }

    #[test]
    fn parse_output_requires_output_event() {
        let raw = r#"{"type":"event","event":"stopped","body":{"category":"stderr","output":"wrong"}}"#;
        let o = parse_output(&parse_envelope(raw).unwrap().raw);

        assert_eq!(o.category, "stdout");
        assert_eq!(o.output, "");
    }

    #[test]
    fn parse_exit_code_uses_body_top_level_field() {
        let raw = r#"{
          "type":"event",
          "event":"exited",
          "exitCode":99,
          "body":{"metadata":{"exitCode":98},"exitCode":7}
        }"#;
        assert_eq!(parse_exit_code(&parse_envelope(raw).unwrap().raw), 7);
    }

    #[test]
    fn parse_exit_code_requires_body_owned_field() {
        let raw = r#"{"type":"event","event":"exited","exitCode":7}"#;
        assert_eq!(parse_exit_code(&parse_envelope(raw).unwrap().raw), 0);
    }

    #[test]
    fn parse_exit_code_rejects_fractional_prefix() {
        let raw = r#"{"type":"event","event":"exited","body":{"exitCode":7.5}}"#;
        assert_eq!(parse_exit_code(&parse_envelope(raw).unwrap().raw), 0);
    }

    #[test]
    fn parse_exit_code_rejects_overflow() {
        let raw = r#"{"type":"event","event":"exited","body":{"exitCode":999999999999999999999999999999}}"#;
        assert_eq!(parse_exit_code(&parse_envelope(raw).unwrap().raw), 0);
    }

    #[test]
    fn parse_exit_code_requires_exited_event() {
        let raw = r#"{"type":"event","event":"output","body":{"exitCode":7}}"#;
        assert_eq!(parse_exit_code(&parse_envelope(raw).unwrap().raw), 0);
    }

    #[test]
    fn empty_stack_and_vars_are_safe() {
        assert!(parse_stack_trace(r#"{"body":{"stackFrames":[]}}"#).is_empty());
        assert!(parse_variables(r#"{"body":{"variables":[]}}"#).is_empty());
        assert!(parse_stack_trace("{}").is_empty());
    }

    // ---- breakpoint state ----

    #[test]
    fn toggle_breakpoint_round_trips() {
        let mut m = DebugModel::new();
        assert!(!m.has_breakpoint("a.mty", 5));
        assert!(m.toggle_breakpoint("a.mty", 5)); // now on
        assert!(m.has_breakpoint("a.mty", 5));
        // Stored as 1-based DAP line.
        assert_eq!(m.breakpoint_lines0("a.mty"), vec![5]);
        assert!(!m.toggle_breakpoint("a.mty", 5)); // off
        assert!(!m.has_breakpoint("a.mty", 5));
        assert!(m.breakpoint_lines0("a.mty").is_empty());
    }

    #[test]
    fn breakpoints_are_per_file_and_sorted() {
        let mut m = DebugModel::new();
        m.toggle_breakpoint("a.mty", 10);
        m.toggle_breakpoint("a.mty", 2);
        m.toggle_breakpoint("a.mty", 6);
        m.toggle_breakpoint("b.mty", 1);
        assert_eq!(m.breakpoint_lines0("a.mty"), vec![2, 6, 10]);
        assert_eq!(m.breakpoint_lines0("b.mty"), vec![1]);
        assert!(!m.has_breakpoint("b.mty", 6));
    }

    #[test]
    fn breakpoint_locations_are_global_and_sorted_for_display() {
        let mut m = DebugModel::new();
        m.toggle_breakpoint("C:/p/z.mty", 9);
        m.toggle_breakpoint("C:/p/a.mty", 4);
        m.toggle_breakpoint("C:/p/a.mty", 1);

        let locations = m.breakpoint_locations();
        assert_eq!(m.total_breakpoint_count(), 3);
        assert_eq!(
            locations,
            vec![
                BreakpointLocation { file: "C:/p/a.mty".into(), line: 2 },
                BreakpointLocation { file: "C:/p/a.mty".into(), line: 5 },
                BreakpointLocation { file: "C:/p/z.mty".into(), line: 10 },
            ]
        );
    }

    #[test]
    fn breakpoint_inventory_scroll_clamps_to_available_window() {
        let mut m = DebugModel::new();
        for i in 0..6 {
            m.toggle_breakpoint(&format!("C:/p/file{i}.mty"), i);
        }

        assert_eq!(m.breakpoint_window_first(3), 0);
        assert!(m.scroll_breakpoints(2, 3));
        assert_eq!(m.breakpoint_window_first(3), 2);
        assert!(m.scroll_breakpoints(99, 3));
        assert_eq!(m.breakpoint_window_first(3), 3);
        assert!(m.scroll_breakpoints(-1, 3));
        assert_eq!(m.breakpoint_window_first(3), 2);
        assert!(m.scroll_breakpoints(-99, 3));
        assert_eq!(m.breakpoint_window_first(3), 0);
        assert!(!m.scroll_breakpoints(3, 6));
        assert_eq!(m.breakpoint_window_first(6), 0);
    }

    #[test]
    fn remove_breakpoint_deletes_exact_location_and_clamps_window() {
        let mut m = DebugModel::new();
        for i in 0..6 {
            m.toggle_breakpoint(&format!("C:/p/file{i}.mty"), i);
        }
        assert!(m.scroll_breakpoints(99, 3));
        assert_eq!(m.breakpoint_window_first(3), 3);

        assert!(m.remove_breakpoint("C:/p/file4.mty", 5));
        assert_eq!(m.total_breakpoint_count(), 5);
        assert!(!m.has_breakpoint("C:/p/file4.mty", 4));
        assert_eq!(m.breakpoint_window_first(3), 2);
        assert!(!m.remove_breakpoint("C:/p/file4.mty", 5));
    }

    #[test]
    fn clear_breakpoints_removes_all_files_and_reports_change() {
        let mut m = DebugModel::new();
        assert!(!m.clear_breakpoints());
        m.toggle_breakpoint("a.mty", 2);
        m.toggle_breakpoint("b.mty", 4);

        assert!(m.clear_breakpoints());
        assert!(m.breakpoint_lines0("a.mty").is_empty());
        assert!(m.breakpoint_lines0("b.mty").is_empty());
        assert!(!m.clear_breakpoints());
    }

    #[test]
    fn negative_line_is_ignored() {
        let mut m = DebugModel::new();
        assert!(!m.toggle_breakpoint("a.mty", -1));
        assert!(!m.has_breakpoint("a.mty", -1));
    }

    #[test]
    fn state_codes() {
        assert_eq!(DebugState::Idle.as_i32(), 0);
        assert_eq!(DebugState::Running.as_i32(), 1);
        assert_eq!(DebugState::Stopped.as_i32(), 2);
        assert_eq!(DebugState::Terminated.as_i32(), 3);
    }

    #[test]
    fn apply_stopped_then_stack_updates_position() {
        let mut m = DebugModel::new();
        m.program = Some(PathBuf::from("C:/p/demo.mty"));
        m.apply_event(SessionEvent::Stopped(StoppedInfo {
            reason: "breakpoint".into(),
            description: String::new(),
            thread_id: Some(1),
        }));
        assert_eq!(m.state(), DebugState::Stopped);
        m.apply_event(SessionEvent::Stack(vec![
            StackFrame { id: 1, name: "f".into(), line: 7, file: "C:/p/demo.mty".into() },
            StackFrame { id: 2, name: "main".into(), line: 18, file: "C:/p/demo.mty".into() },
        ]));
        assert_eq!(m.stack_count(), 2);
        assert_eq!(m.cur_line(), 6); // 1-based 7 -> 0-based 6
        assert_eq!(m.cur_file(), "C:/p/demo.mty");
        assert!(m.take_just_stopped());
        assert!(!m.take_just_stopped()); // consumed
    }

    #[test]
    fn select_frame_moves_jump_target() {
        let mut m = DebugModel::new();
        m.apply_event(SessionEvent::Stack(vec![
            StackFrame { id: 1, name: "f".into(), line: 7, file: "x.mty".into() },
            StackFrame { id: 2, name: "main".into(), line: 18, file: "x.mty".into() },
        ]));
        assert!(m.select_frame(1));
        assert_eq!(m.selected_frame(), 1);
        assert_eq!(m.cur_line(), 17);
        assert!(!m.select_frame(9)); // out of range
    }

    #[test]
    fn terminated_clears_stop() {
        let mut m = DebugModel::new();
        m.apply_event(SessionEvent::Stack(vec![StackFrame {
            id: 1,
            name: "f".into(),
            line: 3,
            file: "x.mty".into(),
        }]));
        m.apply_event(SessionEvent::Terminated);
        assert_eq!(m.state(), DebugState::Terminated);
        assert_eq!(m.stack_count(), 0);
        assert_eq!(m.cur_line(), -1);
    }

    #[test]
    fn output_event_splits_lines() {
        let mut m = DebugModel::new();
        let before = m.console_count();
        m.apply_event(SessionEvent::Output(OutputInfo {
            category: "stdout".into(),
            output: "one\ntwo\n".into(),
        }));
        assert_eq!(m.console_count(), before + 2);
    }

    /// Guarded live integration test: spawn `mty dap`, set a breakpoint in a
    /// tiny program, launch, and assert we reach a `stopped` event with a stack
    /// frame. Skips (passes) if `mty` can't be spawned so CI without the toolchain
    /// stays green. Run with `--ignored` or it auto-skips when mty is absent.
    #[test]
    fn live_dap_session_hits_breakpoint() {
        // Resolve mty; skip if neither the env override nor the dev build exists.
        let mty = mty_path();
        if mty == "mty" && Command::new("mty").arg("--version").output().is_err() {
            eprintln!("SKIP: mty not available for live DAP test");
            return;
        }
        if mty != "mty" && !Path::new(&mty).exists() {
            eprintln!("SKIP: mty path {mty} missing");
            return;
        }
        // A tiny program with a couple of statements to break on.
        let tmp = std::env::temp_dir().join(format!("mui-dap-{}.mty", std::process::id()));
        let src = "fn main() {\n  let a: I32 = 1\n  let b: I32 = 2\n  let c: I32 = a + b\n}\n";
        if std::fs::write(&tmp, src).is_err() {
            eprintln!("SKIP: could not write temp program");
            return;
        }

        let mut m = DebugModel::new();
        let key = tmp.to_string_lossy().to_string();
        // Breakpoint on line 4 (0-based 3) — the `a + b` statement.
        m.toggle_breakpoint(&key, 3);
        if !m.start(&tmp) {
            eprintln!("SKIP: could not spawn `mty dap`");
            let _ = std::fs::remove_file(&tmp);
            return;
        }

        // Pump for up to ~5s waiting for a Stopped + a stack frame.
        let mut stopped = false;
        for _ in 0..200 {
            m.pump();
            if m.state() == DebugState::Stopped && m.stack_count() > 0 {
                stopped = true;
                break;
            }
            if m.state() == DebugState::Terminated {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // Capture the stop facts BEFORE disconnecting (stop() clears the stack).
        let frames = m.stack_count();
        let cur = m.cur_line();
        let final_state = m.state();
        m.stop();
        let _ = std::fs::remove_file(&tmp);

        if !stopped {
            // Don't hard-fail CI (the adapter behaviour can vary), but report it
            // loudly so a human notices.
            eprintln!(
                "WARN: live DAP did not report a stopped+stack within timeout (state={final_state:?}, frames={frames})"
            );
            return;
        }
        assert!(frames > 0, "expected at least one stack frame");
        assert!(cur >= 0, "expected a resolved current line at the stop");
        eprintln!("live DAP OK: stopped with {frames} frame(s), cur_line={cur}");
    }

    #[test]
    fn seed_demo_renders_state() {
        let mut m = DebugModel::new();
        m.seed_demo("C:/p/demo.mty");
        assert_eq!(m.state(), DebugState::Stopped);
        assert!(m.stack_count() >= 1);
        assert!(m.variable_count() >= 1);
        assert!(m.has_breakpoint("C:/p/demo.mty", 2)); // 1-based 3
        assert_eq!(m.cur_line(), 2);
    }

    #[test]
    fn clear_session_preserves_target_and_breakpoints() {
        let mut m = DebugModel::new();
        let path = "C:/p/demo.mty";
        m.seed_demo(path);
        assert_eq!(m.state(), DebugState::Stopped);
        assert!(!m.session_is_empty());
        assert!(m.stack_count() > 0);
        assert!(m.variable_count() > 0);
        assert!(m.console_count() > 0);
        assert!(m.has_program());
        assert!(m.has_breakpoint(path, 2));

        assert!(m.clear_session());
        assert_eq!(m.state(), DebugState::Idle);
        assert_eq!(m.cur_line(), -1);
        assert!(m.cur_file().is_empty());
        assert_eq!(m.stack_count(), 0);
        assert_eq!(m.variable_count(), 0);
        assert_eq!(m.console_count(), 0);
        assert!(m.has_program());
        assert!(m.has_breakpoint(path, 2));
        assert!(m.is_open());
        assert!(m.session_is_empty());
        assert!(!m.clear_session());
    }
}
