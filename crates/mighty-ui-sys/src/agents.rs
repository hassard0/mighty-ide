//! Mighty **Agents** panel — static discovery + topology model of an
//! agent-first Mighty program, plus a best-effort live-inspect parser.
//!
//! ## Why this panel exists
//!
//! Mighty is an *agent-first* language: source declares `protocol P { Msg(..)
//! -> Ret }`, `agent A: P { on Msg(..) -> .. }`, `supervisor S { child .. }`,
//! `@tool("..")`-annotated fns, and LLM-backed agents (an agent whose handler
//! calls an LLM client). No other IDE understands this shape; the Agents panel
//! renders the whole message-passing topology of the workspace as a clear tree
//! and lets you jump to any agent / protocol / handler / tool definition, run a
//! program, and (on platforms where the runtime control socket exists) attach a
//! live inspector.
//!
//! ## Discovery is STATIC and reliable
//!
//! [`scan_project`] walks the workspace's `.mty` files and, reusing the same
//! line-oriented, brace-balance, string/comment-aware discipline as
//! [`crate::outline`], extracts:
//!   * **protocols** + their message signatures,
//!   * **agents** + which protocol they implement (`agent Name: Proto` or
//!     `agent Name(ctor): Proto`) + their `on <Msg>` handlers + whether they are
//!     LLM-backed (a handler that calls an LLM client — `.messages(` /
//!     `AnthropicClient` / `std.llm` / a `with llm` marker),
//!   * **supervisors** + their `child` declarations,
//!   * **tools** (`@tool(..)` immediately preceding an `fn`),
//!
//! and the **agent→protocol** relationships.
//!
//! ## Live inspect is best-effort and platform-gated
//!
//! `mty inspect` connects to a runtime control socket (opt-in via
//! `MTY_RUNTIME_CONTROL_SOCK`). On **Windows the named-pipe backend is NOT
//! implemented** (v0.16 Unix-only) — the runtime binds no socket and `mty
//! inspect --sock ...` returns the stub error
//! *"the Windows named-pipe control socket is not yet implemented"*. So on this
//! platform the panel ships static topology + run only; the [`parse_snapshot`]
//! JSON parser is implemented + unit-tested so live inspect lights up for free
//! on Unix / once the Windows backend lands. See `docs/mighty-language-lessons.md`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

// ===========================================================================
// Project model (the result of static discovery)
// ===========================================================================

/// A protocol message signature (the line inside a `protocol { .. }` body),
/// e.g. `Submit(text: Str) -> U8`. We keep the raw `sig` for display and the
/// bare `name` (`Submit`) for matching against agent `on <Msg>` handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub name: String,
    pub sig: String,
    pub line: u32,
}

/// A `protocol Name { .. }` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
    pub name: String,
    pub line: u32,
    pub messages: Vec<Message>,
}

/// An `on <Msg>` handler inside an agent body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handler {
    pub name: String,
    pub line: u32,
}

/// An `agent Name[: Proto] { .. }` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub line: u32,
    /// The protocol it implements (`agent Name: Proto`), if any.
    pub protocol: Option<String>,
    /// `on <Msg>` handlers in its body.
    pub handlers: Vec<Handler>,
    /// `true` when a handler calls an LLM client (heuristic; see module docs).
    pub llm: bool,
}

/// A `child <name> = spawn <Type>(..)` line inside a supervisor body. `agent_ty`
/// is the spawned agent type (used to link the supervisor to an agent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub local: String,
    pub agent_ty: String,
    pub line: u32,
}

/// A `supervisor Name[(..)] { .. }` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervisor {
    pub name: String,
    pub line: u32,
    pub children: Vec<Child>,
}

/// A `@tool(..)`-annotated `fn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub line: u32,
}

/// The discovered agent system. Each item carries its source `file` so the
/// panel can jump cross-file; within a single-file scan `file` is that file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentModel {
    pub agents: Vec<(PathBuf, Agent)>,
    pub protocols: Vec<(PathBuf, Protocol)>,
    pub supervisors: Vec<(PathBuf, Supervisor)>,
    pub tools: Vec<(PathBuf, Tool)>,
}

impl AgentModel {
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
            && self.protocols.is_empty()
            && self.supervisors.is_empty()
            && self.tools.is_empty()
    }

    /// Merge another single-file model into this one (used by the workspace
    /// scan). Order is preserved (file order, then in-file order).
    fn extend(&mut self, other: AgentModel) {
        self.agents.extend(other.agents);
        self.protocols.extend(other.protocols);
        self.supervisors.extend(other.supervisors);
        self.tools.extend(other.tools);
    }
}

// ===========================================================================
// Single-file scanner
// ===========================================================================

/// Scan one file's `source` for the agent system, attributing every item to
/// `file`. Pure (no I/O), so it is exhaustively unit-testable.
pub fn scan_file(file: &Path, source: &str) -> AgentModel {
    let mut model = AgentModel::default();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0usize;
    // Tracks a pending `@tool(..)` seen on a preceding line (it may span the
    // attribute line then the `fn` line).
    let mut pending_tool = false;

    while i < lines.len() {
        let raw = lines[i];
        let code = strip_line_noise(raw);
        let trimmed = code.trim_start();

        // `@tool(...)` attribute — the next `fn` is a tool. (The attribute can
        // be multi-line; we just latch the flag until the `fn`.)
        if trimmed.starts_with("@tool") {
            pending_tool = true;
            i += 1;
            continue;
        }

        if let Some((kw, rest)) = leading_keyword(trimmed) {
            match kw {
                "fn" => {
                    if pending_tool {
                        if let Some(name) = first_ident(rest) {
                            model.tools.push((
                                file.to_path_buf(),
                                Tool { name, line: i as u32 },
                            ));
                        }
                    }
                    pending_tool = false;
                }
                "protocol" => {
                    pending_tool = false;
                    let (proto, next) = scan_protocol(&lines, i, rest);
                    model.protocols.push((file.to_path_buf(), proto));
                    i = next;
                    continue;
                }
                "agent" => {
                    pending_tool = false;
                    let (agent, next) = scan_agent(&lines, i, rest);
                    model.agents.push((file.to_path_buf(), agent));
                    i = next;
                    continue;
                }
                "supervisor" => {
                    pending_tool = false;
                    let (sup, next) = scan_supervisor(&lines, i, rest);
                    model.supervisors.push((file.to_path_buf(), sup));
                    i = next;
                    continue;
                }
                _ => {
                    pending_tool = false;
                }
            }
        }
        i += 1;
    }
    model
}

/// Scan a `protocol Name { Msg(..) -> Ret ... }` block starting at line
/// `start`. `rest` is the text after the `protocol` keyword on the start line.
/// Returns the parsed protocol + the index of the line AFTER its closing `}`.
fn scan_protocol(lines: &[&str], start: usize, rest: &str) -> (Protocol, usize) {
    let name = first_ident(rest).unwrap_or_default();
    let mut messages = Vec::new();
    let (body_start, body_end) = block_span(lines, start);
    for (li, raw) in lines.iter().enumerate().take(body_end).skip(body_start) {
        let code = strip_line_noise(raw);
        let t = code.trim();
        if t.is_empty() {
            continue;
        }
        // A message is `Ident( ... )` (optionally `-> Ret`). Skip nested braces.
        if let Some(mname) = message_name(t) {
            messages.push(Message {
                name: mname,
                sig: t.to_string(),
                line: li as u32,
            });
        }
    }
    (
        Protocol {
            name,
            line: start as u32,
            messages,
        },
        body_end + 1,
    )
}

/// Scan an `agent Name[(ctor)]: Proto { on Msg ... }` block. Returns the agent
/// + the index of the line after the closing `}`.
fn scan_agent(lines: &[&str], start: usize, rest: &str) -> (Agent, usize) {
    let name = first_ident(rest).unwrap_or_default();
    // The implemented protocol is the identifier after the FIRST top-level `:`
    // on the header line (after skipping a `(ctor)` group).
    let protocol = agent_protocol(rest);
    let (body_start, body_end) = block_span(lines, start);
    let mut handlers = Vec::new();
    let mut llm = false;
    for (li, raw) in lines.iter().enumerate().take(body_end).skip(body_start) {
        let code = strip_line_noise(raw);
        let t = code.trim_start();
        if let Some(after) = t.strip_prefix("on ") {
            if let Some(hname) = first_ident(after.trim_start()) {
                handlers.push(Handler {
                    name: hname,
                    line: li as u32,
                });
            }
        }
        // LLM-backed heuristic: a handler body that drives an LLM client.
        if is_llm_signal(&code) {
            llm = true;
        }
    }
    // A header-level `with llm` marker also counts (forward-compat).
    if rest.contains("with llm") {
        llm = true;
    }
    (
        Agent {
            name,
            line: start as u32,
            protocol,
            handlers,
            llm,
        },
        body_end + 1,
    )
}

/// Scan a `supervisor Name[(..)] { child x = spawn T(..) ... }` block.
fn scan_supervisor(lines: &[&str], start: usize, rest: &str) -> (Supervisor, usize) {
    let name = first_ident(rest).unwrap_or_default();
    let (body_start, body_end) = block_span(lines, start);
    let mut children = Vec::new();
    for (li, raw) in lines.iter().enumerate().take(body_end).skip(body_start) {
        let code = strip_line_noise(raw);
        let t = code.trim_start();
        if let Some(after) = t.strip_prefix("child ") {
            // `child <local> = spawn <Type>(..)` or `child <Type>` (bare form).
            let after = after.trim_start();
            if let Some(local) = first_ident(after) {
                let agent_ty = spawn_type(after).unwrap_or_else(|| local.clone());
                children.push(Child {
                    local,
                    agent_ty,
                    line: li as u32,
                });
            }
        }
    }
    (
        Supervisor {
            name,
            line: start as u32,
            children,
        },
        body_end + 1,
    )
}

// ---------------------------------------------------------------------------
// Workspace scan (multi-file)
// ---------------------------------------------------------------------------

/// Walk `root` for `.mty` files (skipping `target/` and hidden dirs) and merge
/// each file's discovery into one model. Bounded so a huge tree can't stall the
/// frame thread: at most `MAX_FILES` files, `MAX_BYTES` per file.
pub fn scan_project(root: &Path) -> AgentModel {
    const MAX_FILES: usize = 400;
    const MAX_BYTES: u64 = 512 * 1024;
    let mut model = AgentModel::default();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_mty_files(root, &mut files, MAX_FILES);
    files.sort();
    for f in files {
        let meta = std::fs::metadata(&f).ok();
        if meta.map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            continue;
        }
        if let Ok(src) = std::fs::read_to_string(&f) {
            model.extend(scan_file(&f, &src));
        }
    }
    model
}

fn collect_mty_files(dir: &Path, out: &mut Vec<PathBuf>, max: usize) {
    if out.len() >= max {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        if out.len() >= max {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_mty_files(&path, out, max);
        } else if path.extension().and_then(|e| e.to_str()) == Some("mty") {
            out.push(path);
        }
    }
}

// ===========================================================================
// Lexical helpers (shared discipline with crate::outline)
// ===========================================================================

/// Remove a trailing `//` line comment and blank out string contents so braces
/// / keywords inside strings or comments don't skew scanning.
fn strip_line_noise(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_str = false;
    let mut str_ch = b'"';
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                out.push(' ');
                out.push(' ');
                continue;
            }
            if b == str_ch {
                in_str = false;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = true;
                str_ch = b;
                out.push(' ');
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => break,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => break,
            _ => out.push(b as char),
        }
        i += 1;
    }
    out
}

/// If `s` begins with an identifier token, return `(token, rest_after_token)`.
/// Skips a leading `pub ` visibility modifier.
fn leading_keyword(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix("pub ") {
        return leading_keyword(rest);
    }
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

/// The first identifier in `s` (e.g. the declared name after a keyword), or
/// `None` if `s` doesn't start with an identifier char (after trimming).
fn first_ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(s[..end].to_string())
    }
}

/// The implemented protocol from an agent header `rest` (text after `agent`):
/// the identifier after the first top-level `:` (after any `(ctor)` group).
fn agent_protocol(rest: &str) -> Option<String> {
    let bytes = rest.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' | b'>' => depth -= 1,
            b':' if depth == 0 => {
                // `::` is a path separator, not the impl colon.
                if i + 1 < bytes.len() && bytes[i + 1] == b':' {
                    i += 2;
                    continue;
                }
                return first_ident(&rest[i + 1..]);
            }
            b'{' if depth == 0 => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// The protocol-message name from a body line `Ident( ... )`. Returns `None`
/// for lines that aren't a message signature (e.g. blank / nested braces).
fn message_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let name = first_ident(t)?;
    // Must be followed (after the ident) by a `(` to be a message signature.
    let after = t[name.len()..].trim_start();
    if after.starts_with('(') {
        Some(name)
    } else {
        None
    }
}

/// The spawned agent type from a `child` line tail: the identifier after
/// `spawn `, e.g. `local = spawn Collector()` → `Collector`.
fn spawn_type(after_child: &str) -> Option<String> {
    let pos = after_child.find("spawn ")?;
    first_ident(after_child[pos + 6..].trim_start())
}

/// `true` if a (noise-stripped) line carries an LLM-client signal.
fn is_llm_signal(code: &str) -> bool {
    code.contains(".messages(")
        || code.contains("AnthropicClient")
        || code.contains("std.llm")
        || code.contains("use std.llm")
        || code.contains("Member.anthropic")
        || code.contains("Member.openai")
}

/// Find the brace-delimited body of a declaration starting at line `start`.
/// Returns `(first_body_line, closing_brace_line)` — the half-open range
/// `first_body_line..closing_brace_line` covers the body's interior lines.
/// If no `{` is found on/after the start line within a small window, the body
/// is treated as empty (`(start+1, start+1)`).
fn block_span(lines: &[&str], start: usize) -> (usize, usize) {
    // Find the opening brace (it may be on the start line or a following one).
    let mut open_line = None;
    let scan_to = (start + 8).min(lines.len());
    for (li, l) in lines.iter().enumerate().take(scan_to).skip(start) {
        if strip_line_noise(l).contains('{') {
            open_line = Some(li);
            break;
        }
    }
    let Some(open_line) = open_line else {
        return (start + 1, start + 1);
    };
    // Walk brace depth from the opening line to its match.
    let mut depth = 0i32;
    for (li, l) in lines.iter().enumerate().skip(open_line) {
        let code = strip_line_noise(l);
        for b in code.bytes() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        // Body interior is (open_line+1 .. li); closing line is li.
                        return (open_line + 1, li);
                    }
                }
                _ => {}
            }
        }
    }
    // Unterminated — body runs to EOF.
    (open_line + 1, lines.len())
}

// ===========================================================================
// Live-inspect (`mty inspect --json`) snapshot parsing — best-effort
// ===========================================================================

/// One live agent snapshot from a `RuntimeSnapshot` (the `mty inspect --json`
/// wire shape, version 1). Only the fields the panel surfaces are kept.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentSnapshot {
    pub agent_id: u64,
    pub agent_type: String,
    pub mailbox_depth: u64,
    pub mailbox_high_water: u64,
    pub in_flight_handler: String,
}

/// A parsed runtime snapshot (worker count + per-agent rows).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeSnapshot {
    pub worker_count: u64,
    pub agents: Vec<AgentSnapshot>,
}

/// Parse a `mty inspect --json` payload (the documented `RuntimeSnapshot` v1
/// shape). Tolerant scalar scan (no serde dep): finds `worker_count` then each
/// `{ ... }` object inside the `agents` array. Returns `None` on a clearly
/// non-snapshot / error payload.
///
/// This is implemented + tested even though the **Windows** runtime currently
/// binds no control socket (so `mty inspect` can't attach here): on Unix / once
/// the named-pipe backend lands, live inspect lights up with no further work.
pub fn parse_snapshot(json: &str) -> Option<RuntimeSnapshot> {
    let bytes = json.as_bytes();
    // Locate the agents array (also the presence gate for "is this a snapshot").
    let arr_at = find_sub(bytes, b"\"agents\"")?;
    let worker_count = read_uint_after(bytes, b"\"worker_count\"").unwrap_or(0);
    let mut i = arr_at + b"\"agents\"".len();
    while i < bytes.len() && matches!(bytes[i], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return Some(RuntimeSnapshot {
            worker_count,
            agents: Vec::new(),
        });
    }
    let mut agents = Vec::new();
    parse_agent_array(&bytes[i..], &mut agents);
    Some(RuntimeSnapshot {
        worker_count,
        agents,
    })
}

fn parse_agent_array(arr: &[u8], out: &mut Vec<AgentSnapshot>) {
    let mut brace = 0i32;
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
                if brace == 0 {
                    obj_start = Some(k);
                }
                brace += 1;
            }
            b'}' => {
                brace -= 1;
                if brace == 0 {
                    if let Some(s) = obj_start.take() {
                        if let Some(a) = parse_one_agent(&arr[s..=k]) {
                            out.push(a);
                        }
                    }
                }
            }
            b']' if brace == 0 => break,
            _ => {}
        }
    }
}

fn parse_one_agent(obj: &[u8]) -> Option<AgentSnapshot> {
    let agent_type = read_str_after(obj, b"\"agent_type\"")?;
    Some(AgentSnapshot {
        agent_id: read_uint_after(obj, b"\"agent_id\"").unwrap_or(0),
        agent_type,
        mailbox_depth: read_uint_after(obj, b"\"mailbox_depth\"").unwrap_or(0),
        mailbox_high_water: read_uint_after(obj, b"\"mailbox_high_water\"").unwrap_or(0),
        in_flight_handler: read_str_after(obj, b"\"in_flight_handler\"").unwrap_or_default(),
    })
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_uint_after(region: &[u8], key: &[u8]) -> Option<u64> {
    let p = find_sub(region, key)?;
    let mut j = p + key.len();
    while j < region.len() && matches!(region[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    let start = j;
    let mut v: u64 = 0;
    while j < region.len() && region[j].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((region[j] - b'0') as u64);
        j += 1;
    }
    if j == start {
        None
    } else {
        Some(v)
    }
}

fn read_str_after(region: &[u8], key: &[u8]) -> Option<String> {
    let p = find_sub(region, key)?;
    let mut j = p + key.len();
    while j < region.len() && matches!(region[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    if j >= region.len() || region[j] != b'"' {
        return None;
    }
    j += 1;
    let mut s = String::new();
    while j < region.len() && region[j] != b'"' {
        if region[j] == b'\\' && j + 1 < region.len() {
            j += 1;
        }
        s.push(region[j] as char);
        j += 1;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> PathBuf {
        PathBuf::from("agents.mty")
    }

    const SAMPLE: &str = r#"
package agents_demo
use std.llm

protocol Ingest {
  Submit(text: Str) -> U8
  Flush() -> U8
}

protocol Summarize {
  Ask(doc: Str) -> Str
}

@tool("Count the words", cap: fs.read)
fn word_count(text: Str) -> I32 {
  let mut n = 0
  n
}

agent Collector: Ingest {
  on Submit(text) -> {
    let _w = word_count(text)
    1
  }
  on Flush() -> 0
}

agent Summarizer(client): Summarize {
  on Ask(doc) -> {
    let reply = client.messages("m", "s", doc, 10)
    reply
  }
}

supervisor Pipeline(strategy: one_for_one) {
  child collector = spawn Collector()
  child summarizer = spawn Summarizer(client)
  on_fail(collector) { restart up_to 3 in 30s }
}

fn main() {
  log("ok")
}
"#;

    #[test]
    fn discovers_protocols_with_messages() {
        let m = scan_file(&f(), SAMPLE);
        let protos: Vec<&str> = m.protocols.iter().map(|(_, p)| p.name.as_str()).collect();
        assert_eq!(protos, vec!["Ingest", "Summarize"]);
        let ingest = &m.protocols[0].1;
        let msgs: Vec<&str> = ingest.messages.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(msgs, vec!["Submit", "Flush"]);
        let summ = &m.protocols[1].1;
        assert_eq!(summ.messages.len(), 1);
        assert_eq!(summ.messages[0].name, "Ask");
    }

    #[test]
    fn discovers_agents_with_protocol_and_handlers() {
        let m = scan_file(&f(), SAMPLE);
        let names: Vec<&str> = m.agents.iter().map(|(_, a)| a.name.as_str()).collect();
        assert_eq!(names, vec!["Collector", "Summarizer"]);

        let collector = &m.agents[0].1;
        assert_eq!(collector.protocol.as_deref(), Some("Ingest"));
        let handlers: Vec<&str> = collector.handlers.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(handlers, vec!["Submit", "Flush"]);
        assert!(!collector.llm, "Collector is not LLM-backed");

        let summarizer = &m.agents[1].1;
        assert_eq!(summarizer.protocol.as_deref(), Some("Summarize"));
        assert_eq!(
            summarizer.handlers.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["Ask"]
        );
        assert!(summarizer.llm, "Summarizer calls client.messages() -> LLM-backed");
    }

    #[test]
    fn discovers_tools() {
        let m = scan_file(&f(), SAMPLE);
        let tools: Vec<&str> = m.tools.iter().map(|(_, t)| t.name.as_str()).collect();
        assert_eq!(tools, vec!["word_count"]);
    }

    #[test]
    fn discovers_supervisor_with_children() {
        let m = scan_file(&f(), SAMPLE);
        assert_eq!(m.supervisors.len(), 1);
        let sup = &m.supervisors[0].1;
        assert_eq!(sup.name, "Pipeline");
        let kids: Vec<(&str, &str)> = sup
            .children
            .iter()
            .map(|c| (c.local.as_str(), c.agent_ty.as_str()))
            .collect();
        assert_eq!(
            kids,
            vec![("collector", "Collector"), ("summarizer", "Summarizer")]
        );
    }

    #[test]
    fn agent_protocol_skips_ctor_and_path_colons() {
        // ctor group before the impl colon.
        assert_eq!(agent_protocol("Foo(a, b): Bar {").as_deref(), Some("Bar"));
        // path-style `::` is not the impl colon.
        assert_eq!(agent_protocol("Foo: pkg::Proto {").as_deref(), Some("pkg"));
        // no protocol.
        assert_eq!(agent_protocol("Foo {"), None);
        assert_eq!(agent_protocol("Foo(x) {"), None);
    }

    #[test]
    fn braces_in_strings_dont_break_block_span() {
        let src = "agent A: P {\n  on M() -> {\n    let s = \"a } b { c\"\n    1\n  }\n}\nagent B: Q {\n}\n";
        let m = scan_file(&f(), src);
        let names: Vec<&str> = m.agents.iter().map(|(_, a)| a.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B"], "string braces must not merge agents");
    }

    #[test]
    fn empty_source_yields_empty_model() {
        let m = scan_file(&f(), "fn main() { log(\"hi\") }\n");
        assert!(m.is_empty());
    }

    #[test]
    fn with_llm_header_marker_counts() {
        let src = "agent Bot: Chat with llm {\n  on Say(m) -> m\n}\n";
        let m = scan_file(&f(), src);
        assert!(m.agents[0].1.llm);
    }

    // ---- snapshot parser ----

    const SNAP: &str = r#"{"version":1,"worker_count":4,"timestamp_ms":1779580800000,
      "agents":[
        {"version":1,"agent_id":1,"agent_type":"agents_demo::Collector","supervisor_parent":3,
         "mailbox_depth":2,"mailbox_high_water":7,"in_flight_handler":"Submit","in_flight_elapsed_ms":12},
        {"version":1,"agent_id":2,"agent_type":"agents_demo::Summarizer","supervisor_parent":3,
         "mailbox_depth":0,"mailbox_high_water":3,"in_flight_handler":"","in_flight_elapsed_ms":0}
      ]}"#;

    #[test]
    fn parse_snapshot_reads_agents() {
        let s = parse_snapshot(SNAP).expect("snapshot");
        assert_eq!(s.worker_count, 4);
        assert_eq!(s.agents.len(), 2);
        assert_eq!(s.agents[0].agent_id, 1);
        assert_eq!(s.agents[0].agent_type, "agents_demo::Collector");
        assert_eq!(s.agents[0].mailbox_depth, 2);
        assert_eq!(s.agents[0].mailbox_high_water, 7);
        assert_eq!(s.agents[0].in_flight_handler, "Submit");
        assert_eq!(s.agents[1].agent_type, "agents_demo::Summarizer");
        assert_eq!(s.agents[1].in_flight_handler, "");
    }

    #[test]
    fn parse_snapshot_empty_agents() {
        let s = parse_snapshot(r#"{"worker_count":1,"agents":[]}"#).expect("snapshot");
        assert_eq!(s.worker_count, 1);
        assert!(s.agents.is_empty());
    }

    #[test]
    fn parse_snapshot_rejects_non_snapshot() {
        assert!(parse_snapshot(r#"{"error":"no socket"}"#).is_none());
    }
}
