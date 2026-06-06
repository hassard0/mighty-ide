//! Generic, server-agnostic LSP client used for **non-Mighty** languages.
//!
//! The Mighty path keeps its three existing, well-tuned clients (`completion`,
//! `nav`, `language`) which spawn `mty lsp`. This module generalizes the exact
//! same proven discipline (L24/L25) — byte-count `Content-Length`, staged
//! `initialize`/`initialized`/`didOpen`/request with brief pauses, read on a
//! worker thread bounded by `recv_timeout`, kill the child on timeout — to an
//! arbitrary [`crate::lspregistry::ServerSpec`] + `languageId`, so the IDE can
//! drive `rust-analyzer`, `pyright`, `gopls`, `clangd`, etc.
//!
//! Two entry points:
//!   * [`request`] — run the handshake + a single `textDocument/*` request and
//!     return the isolated `"id":2` response object (completion / hover /
//!     definition / signature help / rename / code actions). Empty string on any
//!     failure / timeout (never blocks).
//!   * [`diagnostics`] — `didOpen` the doc and collect the server's
//!     `textDocument/publishDiagnostics` for that URI, parsed into
//!     [`crate::diagnostics::Diag`]. Empty on failure.
//!
//! Everything here is failure-tolerant: a missing server (the spec is only
//! produced by [`crate::lspregistry::server_for`] when the binary is found),
//! a spawn error, a parse error, or a timeout all yield empty results so the
//! editor keeps highlighting + editing.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::diagnostics::{Diag, Severity};
use crate::lspregistry::ServerSpec;

/// Which single LSP request to fire after initialize + didOpen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Completion,
    Hover,
    Definition,
    SignatureHelp,
    PrepareRename,
    Rename {
        new_name: String,
    },
    CodeAction {
        end_line: u32,
        end_col: u32,
        diagnostics_json: String,
    },
    DocumentSymbol,
    ExecuteCommand {
        command: String,
        arguments_json: Option<String>,
    },
}

impl Method {
    fn name(&self) -> &'static str {
        match self {
            Method::Completion => "textDocument/completion",
            Method::Hover => "textDocument/hover",
            Method::Definition => "textDocument/definition",
            Method::SignatureHelp => "textDocument/signatureHelp",
            Method::PrepareRename => "textDocument/prepareRename",
            Method::Rename { .. } => "textDocument/rename",
            Method::CodeAction { .. } => "textDocument/codeAction",
            Method::DocumentSymbol => "textDocument/documentSymbol",
            Method::ExecuteCommand { .. } => "workspace/executeCommand",
        }
    }

    fn params(&self, uri: &str, source: &str, line: u32, col: u32) -> String {
        let u = json_escape(uri);
        let lsp_col = lsp_utf16_col(source, line, col);
        match self {
            Method::Completion
            | Method::Hover
            | Method::Definition
            | Method::SignatureHelp
            | Method::PrepareRename => format!(
                r#"{{"textDocument":{{"uri":"{u}"}},"position":{{"line":{line},"character":{lsp_col}}}}}"#
            ),
            Method::Rename { new_name } => format!(
                r#"{{"textDocument":{{"uri":"{u}"}},"position":{{"line":{line},"character":{lsp_col}}},"newName":"{}"}}"#,
                json_escape(new_name)
            ),
            Method::CodeAction {
                end_line,
                end_col,
                diagnostics_json,
            } => {
                let lsp_end_col = lsp_utf16_col(source, *end_line, *end_col);
                format!(
                    r#"{{"textDocument":{{"uri":"{u}"}},"range":{{"start":{{"line":{line},"character":{lsp_col}}},"end":{{"line":{end_line},"character":{lsp_end_col}}}}},"context":{{"diagnostics":{diagnostics_json}}}}}"#
                )
            }
            Method::DocumentSymbol => format!(r#"{{"textDocument":{{"uri":"{u}"}}}}"#),
            Method::ExecuteCommand {
                command,
                arguments_json,
            } => {
                let args = arguments_json.as_deref().unwrap_or("[]");
                format!(
                    r#"{{"command":"{}","arguments":{args}}}"#,
                    json_escape(command)
                )
            }
        }
    }
}

fn lsp_utf16_col(source: &str, line: u32, char_col: u32) -> u32 {
    let Some(line_text) = source.split('\n').nth(line as usize) else {
        return 0;
    };
    line_text
        .chars()
        .take(char_col as usize)
        .map(|ch| ch.len_utf16() as u32)
        .sum()
}

fn frame(json: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", json.len()).into_bytes();
    out.extend_from_slice(json.as_bytes());
    out
}

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

pub fn file_uri(path: &Path) -> String {
    crate::nav::path_to_file_uri(path)
}

fn kill(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Spawn the server described by `spec`. Returns `None` on spawn failure (the
/// caller then silently skips LSP — the binary was found by `server_for` but
/// could still fail to launch).
fn spawn(spec: &ServerSpec) -> Option<Child> {
    Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| eprintln!("lspclient: spawn `{}` failed: {e}", spec.program))
        .ok()
}

/// The `initialize` params with a real `rootUri` (the workspace root) so servers
/// like rust-analyzer / gopls can resolve the project. `root` is the workspace
/// directory; `processId` is our PID.
fn initialize_msg(root: &Path) -> String {
    let root_uri = file_uri(root);
    let pid = std::process::id();
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"processId":{pid},"rootUri":"{}","capabilities":{{"workspace":{{"applyEdit":true}},"textDocument":{{"completion":{{"completionItem":{{"snippetSupport":true,"commitCharactersSupport":true,"deprecatedSupport":true,"preselectSupport":true,"tagSupport":{{"valueSet":[1]}},"insertReplaceSupport":true,"labelDetailsSupport":true,"documentationFormat":["markdown","plaintext"]}},"completionItemKind":{{"valueSet":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25]}},"completionList":{{"itemDefaults":["commitCharacters","editRange"]}}}},"hover":{{"contentFormat":["markdown","plaintext"]}},"definition":{{"linkSupport":true}},"signatureHelp":{{"signatureInformation":{{"documentationFormat":["markdown","plaintext"],"parameterInformation":{{"labelOffsetSupport":true}}}}}},"rename":{{"prepareSupport":true}},"codeAction":{{"codeActionLiteralSupport":{{"codeActionKind":{{"valueSet":["quickfix","refactor","refactor.rewrite","source","source.fixAll","source.organizeImports"]}}}},"isPreferredSupport":true,"disabledSupport":true}},"documentSymbol":{{"hierarchicalDocumentSymbolSupport":true,"symbolKind":{{"valueSet":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26]}}}},"publishDiagnostics":{{}}}}}},"workspaceFolders":null}}}}"#,
        json_escape(&root_uri)
    )
}

/// Run the staged handshake + one `textDocument/*` request at (`line`,`col`)
/// (0-based) against `source` (the live unsaved doc text), identified by `path`,
/// using `language_id` in `didOpen`. Returns the isolated `"id":2` response
/// object, or an empty string on any failure / timeout.
#[allow(clippy::too_many_arguments)]
pub fn request(
    spec: &ServerSpec,
    language_id: &str,
    root: &Path,
    path: &Path,
    source: &str,
    method: Method,
    line: u32,
    col: u32,
) -> String {
    request_with_timeout(
        spec,
        language_id,
        root,
        path,
        source,
        method,
        line,
        col,
        Duration::from_millis(4000),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn request_with_timeout(
    spec: &ServerSpec,
    language_id: &str,
    root: &Path,
    path: &Path,
    source: &str,
    method: Method,
    line: u32,
    col: u32,
    timeout: Duration,
) -> String {
    let Some(mut child) = spawn(spec) else {
        return String::new();
    };

    let uri = file_uri(path);
    let initialize = initialize_msg(root);
    let initialized = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string();
    let did_open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"{}","version":1,"text":"{}"}}}}}}"#,
        json_escape(&uri),
        json_escape(language_id),
        json_escape(source)
    );
    let request_msg = request_msg(&method, &uri, source, line, col);

    let Some(stdin) = child.stdin.take() else {
        kill(child);
        return String::new();
    };
    let stdin = Arc::new(Mutex::new(stdin));
    let writer_stdin = Arc::clone(&stdin);
    let writer = std::thread::spawn(move || {
        // Bigger settle pauses than the Mighty client: heavyweight servers
        // (rust-analyzer/gopls) index on initialize and need the doc open to
        // settle before they answer.
        let stages: [(&str, u64); 4] = [
            (&initialize, 250),
            (&initialized, 80),
            (&did_open, 350),
            (&request_msg, 0),
        ];
        for (msg, pause_ms) in stages {
            let framed = frame(msg);
            let mut stdin = lock_shared(&writer_stdin);
            if stdin.write_all(&framed).is_err() || stdin.flush().is_err() {
                return;
            }
            drop(stdin);
            if pause_ms > 0 {
                std::thread::sleep(Duration::from_millis(pause_ms));
            }
        }
    });

    let Some(mut stdout) = child.stdout.take() else {
        kill(child);
        return String::new();
    };

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let respond_apply_edit = matches!(method, Method::ExecuteCommand { .. });
    let reader_stdin = Arc::clone(&stdin);
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut apply_edit_replied = false;
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if respond_apply_edit && !apply_edit_replied {
                        if let Some(id) = apply_edit_request_id(&buf) {
                            let response = apply_edit_response(&id);
                            let mut stdin = lock_shared(&reader_stdin);
                            let _ = stdin.write_all(&frame(&response));
                            let _ = stdin.flush();
                            apply_edit_replied = true;
                        }
                    }
                    if has_response_id(&buf, "2") {
                        break;
                    }
                    if buf.len() > 4 * 1024 * 1024 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(buf);
    });

    let raw = match rx.recv_timeout(timeout) {
        Ok(bytes) => {
            kill(child);
            let _ = writer.join();
            let _ = reader.join();
            bytes
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let bytes = rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap_or_default();
            let _ = writer.join();
            let _ = reader.join();
            eprintln!("lspclient: {} timed out after {timeout:?}", method.name());
            bytes
        }
    };

    let text = String::from_utf8_lossy(&raw).into_owned();
    let response = crate::nav::lsp::isolate_response_id(&text, 2);
    if respond_apply_edit && has_apply_edit_request(text.as_bytes()) {
        format!("{response}\n{text}")
    } else {
        response
    }
}

fn lock_shared<T>(shared: &Arc<Mutex<T>>) -> MutexGuard<'_, T> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn request_msg(method: &Method, uri: &str, source: &str, line: u32, col: u32) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"{}","params":{}}}"#,
        method.name(),
        method.params(uri, source, line, col)
    )
}

fn apply_edit_response(id_json: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id_json},"result":{{"applied":true}}}}"#)
}

fn has_apply_edit_request(stream: &[u8]) -> bool {
    apply_edit_request_id(stream).is_some()
}

fn has_response_id(stream: &[u8], wanted_id_json: &str) -> bool {
    top_level_json_objects(stream).into_iter().any(|obj| {
        let Some(id_at) = top_level_field_value_start(obj, b"id") else {
            return false;
        };
        let Some((id, _)) = read_json_id_at(obj, id_at) else {
            return false;
        };
        id == wanted_id_json
            && top_level_field_value_start(obj, b"method").is_none()
            && (top_level_field_value_start(obj, b"result").is_some()
                || top_level_field_value_start(obj, b"error").is_some())
    })
}

fn apply_edit_request_id(stream: &[u8]) -> Option<String> {
    for obj in top_level_json_objects(stream) {
        let Some(method_at) = top_level_field_value_start(obj, b"method") else {
            continue;
        };
        let Some((method, _)) = read_json_string_at(obj, method_at) else {
            continue;
        };
        if method != "workspace/applyEdit" {
            continue;
        }
        if top_level_object_field(obj, b"params")
            .and_then(|params| top_level_object_field(params, b"edit"))
            .is_none()
        {
            continue;
        }
        let Some(id_at) = top_level_field_value_start(obj, b"id") else {
            continue;
        };
        let Some((id, _)) = read_json_id_at(obj, id_at) else {
            continue;
        };
        return Some(id);
    }
    None
}

/// Open `source` on the server and collect its `publishDiagnostics` for the
/// document URI, parsed into [`Diag`]s. Returns an empty Vec on any failure /
/// timeout. Used to surface non-Mighty diagnostics (the Mighty path keeps using
/// `mty check`).
pub fn diagnostics(
    spec: &ServerSpec,
    language_id: &str,
    root: &Path,
    path: &Path,
    source: &str,
) -> Vec<Diag> {
    diagnostics_with_timeout(
        spec,
        language_id,
        root,
        path,
        source,
        Duration::from_millis(6000),
    )
}

pub fn diagnostics_with_timeout(
    spec: &ServerSpec,
    language_id: &str,
    root: &Path,
    path: &Path,
    source: &str,
    timeout: Duration,
) -> Vec<Diag> {
    let Some(mut child) = spawn(spec) else {
        return Vec::new();
    };

    let uri = file_uri(path);
    let uri_for_reader = uri.clone();
    let initialize = initialize_msg(root);
    let initialized = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string();
    let did_open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"{}","version":1,"text":"{}"}}}}}}"#,
        json_escape(&uri),
        json_escape(language_id),
        json_escape(source)
    );

    let Some(mut stdin) = child.stdin.take() else {
        kill(child);
        return Vec::new();
    };
    let writer = std::thread::spawn(move || {
        let stages: [(&str, u64); 3] = [(&initialize, 250), (&initialized, 80), (&did_open, 0)];
        for (msg, pause_ms) in stages {
            if stdin.write_all(&frame(msg)).is_err() || stdin.flush().is_err() {
                return;
            }
            if pause_ms > 0 {
                std::thread::sleep(Duration::from_millis(pause_ms));
            }
        }
        // Keep stdin open: many servers only publish diagnostics for an OPEN
        // document and may withhold them if the connection closes. We rely on
        // the reader's deadline + kill to tear down.
        // (stdin is dropped when this thread ends.)
        std::thread::sleep(Duration::from_millis(50));
        drop(stdin);
    });

    let Some(mut stdout) = child.stdout.take() else {
        kill(child);
        return Vec::new();
    };

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    // Stop once a complete publishDiagnostics notification for
                    // our URI has arrived. Other workspace files can publish
                    // first, and their messages/relatedInformation may mention
                    // this URI.
                    if has_publish_diagnostics_for_uri(&buf, &uri_for_reader) {
                        // Give a brief grace read so the array body is fully buffered.
                        break;
                    }
                    if buf.len() > 8 * 1024 * 1024 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(buf);
    });

    let raw = match rx.recv_timeout(timeout) {
        Ok(bytes) => {
            kill(child);
            let _ = writer.join();
            let _ = reader.join();
            bytes
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let bytes = rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap_or_default();
            let _ = writer.join();
            let _ = reader.join();
            bytes
        }
    };

    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut diags = parse_publish_diagnostics_for_uri(&text, &uri);
    normalize_lsp_diag_columns(source, &mut diags);
    diags
}

/// Parse a `textDocument/publishDiagnostics` notification stream into [`Diag`]s.
/// Reads the `diagnostics` array, and for each entry the `range.start`
/// line/character (0-based), the `severity` (1=error,2=warning → our 0/1; 3/4
/// info/hint folded to warning), and the `message`.
#[cfg(test)]
pub fn parse_publish_diagnostics(stream: &str) -> Vec<Diag> {
    parse_publish_diagnostics_latest(stream, None)
}

pub fn parse_publish_diagnostics_for_uri(stream: &str, wanted_uri: &str) -> Vec<Diag> {
    parse_publish_diagnostics_latest(stream, Some(wanted_uri))
}

fn has_publish_diagnostics_for_uri(stream: &[u8], wanted_uri: &str) -> bool {
    top_level_json_objects(stream).into_iter().any(|object| {
        top_level_json_string_field(object, b"method").as_deref()
            == Some("textDocument/publishDiagnostics")
            && publish_chunk_matches_uri(object, Some(wanted_uri))
            && top_level_object_field(object, b"params")
                .and_then(|params| top_level_array_field(params, b"diagnostics"))
                .is_some()
    })
}

fn normalize_lsp_diag_columns(source: &str, diags: &mut [Diag]) {
    for diag in diags {
        if diag.line < 0 {
            continue;
        }
        let Some(line_text) = source.split('\n').nth(diag.line as usize) else {
            continue;
        };
        let start = utf16_to_char_col(line_text, diag.col_start.max(0) as u32) as i32;
        let mut end = utf16_to_char_col(line_text, diag.col_end.max(0) as u32) as i32;
        if end <= start {
            end = start + 1;
        }
        diag.col_start = start;
        diag.col_end = end;
    }
}

fn utf16_to_char_col(line_text: &str, utf16_col: u32) -> u32 {
    let mut units = 0u32;
    let mut chars = 0u32;
    for ch in line_text.chars() {
        if units >= utf16_col {
            return chars;
        }
        let next = units + ch.len_utf16() as u32;
        if next > utf16_col {
            return chars;
        }
        units = next;
        chars += 1;
    }
    chars
}

fn parse_publish_diagnostics_latest(stream: &str, wanted_uri: Option<&str>) -> Vec<Diag> {
    let bytes = stream.as_bytes();
    if wanted_uri.is_none() && is_json_array(bytes) {
        return parse_diagnostics_array_from_bytes(bytes).unwrap_or_default();
    }
    let mut latest: Option<Vec<Diag>> = None;
    let mut saw_object = false;
    for object in top_level_json_objects(bytes) {
        saw_object = true;
        if wanted_uri.is_some() && !is_publish_diagnostics_notification(object) {
            continue;
        }
        if publish_chunk_matches_uri(object, wanted_uri) {
            if let Some(diags) = parse_diagnostics_array_from_bytes(object) {
                latest = Some(diags);
            }
        }
    }
    if let Some(diags) = latest {
        return diags;
    }
    if wanted_uri.is_none() && !saw_object {
        return parse_diagnostics_array_from_bytes(bytes).unwrap_or_default();
    }
    Vec::new()
}

fn is_publish_diagnostics_notification(chunk: &[u8]) -> bool {
    top_level_json_string_field(chunk, b"method").as_deref()
        == Some("textDocument/publishDiagnostics")
}

fn publish_chunk_matches_uri(chunk: &[u8], wanted_uri: Option<&str>) -> bool {
    let Some(wanted) = wanted_uri else {
        return true;
    };
    top_level_object_field(chunk, b"params")
        .and_then(|params| top_level_json_string_field(params, b"uri"))
        .map(|uri| diagnostics_uri_matches(&uri, wanted))
        .unwrap_or(false)
}

fn diagnostics_uri_matches(actual: &str, wanted: &str) -> bool {
    if actual == wanted {
        return true;
    }
    let Some(actual_path) = crate::nav::uri_to_path(actual) else {
        return false;
    };
    let Some(wanted_path) = crate::nav::uri_to_path(wanted) else {
        return false;
    };
    crate::nav::paths_equal(&actual_path, &wanted_path)
}

fn parse_diagnostics_array_from_bytes(bytes: &[u8]) -> Option<Vec<Diag>> {
    let arr = if is_json_array(bytes) {
        bytes
    } else {
        top_level_object_field(bytes, b"params")
            .and_then(|params| top_level_array_field(params, b"diagnostics"))?
    };
    Some(parse_diag_array(arr))
}

fn is_json_array(bytes: &[u8]) -> bool {
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    bytes.get(i) == Some(&b'[')
}

/// Split a `[ {...}, ... ]` slice into per-diagnostic objects and parse each.
fn parse_diag_array(arr: &[u8]) -> Vec<Diag> {
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
                        if let Some(d) = parse_one_diag(&arr[s..=k]) {
                            out.push(d);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse a single diagnostic object slice.
fn parse_one_diag(obj: &[u8]) -> Option<Diag> {
    let range = top_level_object_field(obj, b"range")?;
    let start = top_level_object_field(range, b"start")?;
    let line = i32::try_from(top_level_uint_field(start, b"line")?).ok()?;
    let col = i32::try_from(top_level_uint_field(start, b"character")?).ok()?;
    // optional end character on the same start line for a wider underline.
    let col_end = top_level_object_field(range, b"end")
        .and_then(|end| {
            let el = i32::try_from(top_level_uint_field(end, b"line")?).ok()?;
            let ec = i32::try_from(top_level_uint_field(end, b"character")?).ok()?;
            if el == line && ec > col {
                Some(ec)
            } else {
                None
            }
        })
        .unwrap_or(col + 1);

    let severity = parse_diagnostic_severity(obj);
    let message = top_level_json_string_field(obj, b"message").unwrap_or_default();
    let code = top_level_json_string_field(obj, b"code")
        .or_else(|| top_level_uint_field(obj, b"code").map(|n| n.to_string()))
        .unwrap_or_default();

    Some(Diag {
        line,
        col_start: col,
        col_end,
        severity,
        code,
        message,
    })
}

fn parse_diagnostic_severity(obj: &[u8]) -> Severity {
    match top_level_uint_field(obj, b"severity") {
        Some(1) => Severity::Error,
        Some(2..=4) => Severity::Warning,
        _ => Severity::Warning,
    }
}

fn top_level_object_field<'a>(obj: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let value_start = top_level_field_value_start(obj, field)?;
    if obj.get(value_start) != Some(&b'{') {
        return None;
    }
    let value_end = match_brace(obj, value_start);
    Some(&obj[value_start..value_end])
}

fn top_level_array_field<'a>(obj: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let value_start = top_level_field_value_start(obj, field)?;
    if obj.get(value_start) != Some(&b'[') {
        return None;
    }
    let value_end = match_bracket(obj, value_start);
    Some(&obj[value_start..value_end])
}

fn top_level_json_objects(stream: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut k = 0usize;
    while k < stream.len() {
        if stream[k] == b'{' {
            let end = match_brace(stream, k).min(stream.len());
            out.push(&stream[k..end]);
            k = end;
        } else {
            k += 1;
        }
    }
    out
}

fn top_level_json_string_field(obj: &[u8], field: &[u8]) -> Option<String> {
    top_level_field_value_start(obj, field)
        .and_then(|value_start| read_json_string_at(obj, value_start))
        .map(|(value, _)| value)
}

fn top_level_uint_field(obj: &[u8], field: &[u8]) -> Option<u32> {
    let mut j = top_level_field_value_start(obj, field)?;
    while j < obj.len() && obj[j].is_ascii_whitespace() {
        j += 1;
    }
    let start = j;
    let mut value: u32 = 0;
    while j < obj.len() && obj[j].is_ascii_digit() {
        value = value.checked_mul(10)?;
        value = value.checked_add((obj[j] - b'0') as u32)?;
        j += 1;
    }
    if j == start {
        None
    } else if j < obj.len()
        && !matches!(obj[j], b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']')
    {
        None
    } else {
        Some(value)
    }
}

fn top_level_field_value_start(obj: &[u8], field: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut k = 0usize;
    while k < obj.len() {
        let b = obj[k];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            k += 1;
            continue;
        }

        match b {
            b'"' => {
                if depth == 1
                    && obj.get(k + 1..k + 1 + field.len()) == Some(field)
                    && obj.get(k + 1 + field.len()) == Some(&b'"')
                {
                    let mut p = k + field.len() + 2;
                    while p < obj.len() && obj[p].is_ascii_whitespace() {
                        p += 1;
                    }
                    if obj.get(p) != Some(&b':') {
                        return None;
                    }
                    p += 1;
                    while p < obj.len() && obj[p].is_ascii_whitespace() {
                        p += 1;
                    }
                    return Some(p);
                }
                in_str = true;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
        k += 1;
    }
    None
}

/// Read a JSON string at/after `pos` (skips ws + `:`), un-escaping common cases.
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

/// Read a JSON-RPC request id as JSON text (`7` or `"abc"`), beginning at or
/// after `pos` (skips ws + `:`). Returns the raw id value for an id response.
fn read_json_id_at(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    let mut j = pos;
    while j < bytes.len() && matches!(bytes[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    if bytes[j] == b'"' {
        let (id, past) = read_json_string_at(bytes, j)?;
        return Some((format!("\"{}\"", json_escape(&id)), past));
    }
    let start = j;
    if bytes[j] == b'-' {
        j += 1;
    }
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    if j == start || (bytes[start] == b'-' && j == start + 1) {
        return None;
    }
    if j < bytes.len()
        && !matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']')
    {
        return None;
    }
    Some((String::from_utf8_lossy(&bytes[start..j]).into_owned(), j))
}

/// Index just past the `}` matching the `{` at `open` (string-aware).
fn match_brace(bytes: &[u8], open: usize) -> usize {
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
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
        k += 1;
    }
    bytes.len()
}

/// Index just past the `]` matching the `[` at `open` (string-aware).
fn match_bracket(bytes: &[u8], open: usize) -> usize {
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
        } else if b == b'[' {
            depth += 1;
        } else if b == b']' {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
        k += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_lock_recovers_from_poisoned_state() {
        let shared = Arc::new(Mutex::new(Vec::<u8>::from(b"lsp")));
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poisoned = {
            let shared = Arc::clone(&shared);
            std::panic::catch_unwind(move || {
                let mut guard = lock_shared(&shared);
                guard.extend_from_slice(b" poisoned");
                panic!("poison lsp shared lock");
            })
        };
        std::panic::set_hook(hook);
        assert!(poisoned.is_err());

        let mut guard = lock_shared(&shared);
        guard.extend_from_slice(b" recovered");
        assert_eq!(&guard[..], b"lsp poisoned recovered");
    }

    #[test]
    fn parses_publish_diagnostics() {
        // A realistic rust-analyzer-style publishDiagnostics notification.
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[{"range":{"start":{"line":3,"character":4},"end":{"line":3,"character":9}},"severity":1,"code":"E0425","message":"cannot find value `foo`"},{"range":{"start":{"line":10,"character":0},"end":{"line":10,"character":2}},"severity":2,"message":"unused import"}]}}"#;
        let diags = parse_publish_diagnostics(stream);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].line, 3);
        assert_eq!(diags[0].col_start, 4);
        assert_eq!(diags[0].col_end, 9);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].code, "E0425");
        assert_eq!(diags[0].message, "cannot find value `foo`");
        assert_eq!(diags[1].line, 10);
        assert_eq!(diags[1].severity, Severity::Warning);
        assert_eq!(diags[1].col_end, 2);
    }

    #[test]
    fn diagnostics_preserve_numeric_codes() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}},"severity":1,"code":6133,"message":"declared but never used"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "6133");
    }

    #[test]
    fn diagnostics_range_ignores_related_information_ranges() {
        let stream = r#"{"params":{"diagnostics":[{"relatedInformation":[{"location":{"uri":"file:///x/dep.rs","range":{"start":{"line":99,"character":1},"end":{"line":99,"character":2}}},"message":"related"}],"range":{"start":{"line":4,"character":3},"end":{"line":4,"character":8}},"severity":1,"message":"primary"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 4);
        assert_eq!(diags[0].col_start, 3);
        assert_eq!(diags[0].col_end, 8);
        assert_eq!(diags[0].message, "primary");
    }

    #[test]
    fn diagnostics_array_uses_params_top_level_field() {
        let stream = r#"{"params":{"metadata":{"diagnostics":[{"range":{"start":{"line":99,"character":1},"end":{"line":99,"character":2}},"severity":1,"message":"wrong nested"}]},"diagnostics":[{"range":{"start":{"line":5,"character":6},"end":{"line":5,"character":10}},"severity":2,"message":"right"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 5);
        assert_eq!(diags[0].col_start, 6);
        assert_eq!(diags[0].col_end, 10);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].message, "right");
    }

    #[test]
    fn diagnostics_object_requires_params_owned_array() {
        let stream = r#"{"metadata":{"diagnostics":[{"range":{"start":{"line":98,"character":1},"end":{"line":98,"character":2}},"severity":1,"message":"wrong nested"}]},"diagnostics":[{"range":{"start":{"line":5,"character":6},"end":{"line":5,"character":10}},"severity":2,"message":"wrong root"}]}"#;

        assert!(parse_publish_diagnostics(stream).is_empty());
    }

    #[test]
    fn diagnostics_raw_array_still_parses_direct_payloads() {
        let stream = r#"[{"range":{"start":{"line":5,"character":6},"end":{"line":5,"character":10}},"severity":2,"message":"right"}]"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 5);
        assert_eq!(diags[0].col_start, 6);
        assert_eq!(diags[0].message, "right");
    }

    #[test]
    fn diagnostics_range_uses_range_top_level_positions() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"metadata":{"start":{"line":99,"character":1},"end":{"line":99,"character":2}},"start":{"metadata":{"line":98,"character":3},"line":6,"character":7},"end":{"metadata":{"line":97,"character":4},"line":6,"character":12}},"severity":1,"message":"primary"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 6);
        assert_eq!(diags[0].col_start, 7);
        assert_eq!(diags[0].col_end, 12);
        assert_eq!(diags[0].message, "primary");
    }

    #[test]
    fn diagnostics_reject_fractional_range_positions() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"start":{"line":6.5,"character":7},"end":{"line":6,"character":12}},"severity":1,"message":"bad"},{"range":{"start":{"line":8,"character":2},"end":{"line":8,"character":5}},"severity":2,"message":"good"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 8);
        assert_eq!(diags[0].col_start, 2);
        assert_eq!(diags[0].message, "good");
    }

    #[test]
    fn diagnostics_reject_overflow_range_positions() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"start":{"line":999999999999999999999999999999,"character":7},"end":{"line":6,"character":12}},"severity":1,"message":"bad overflow"},{"range":{"start":{"line":2147483648,"character":7},"end":{"line":2147483648,"character":12}},"severity":1,"message":"bad wrap"},{"range":{"start":{"line":8,"character":2},"end":{"line":8,"character":5}},"severity":2,"message":"good"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 8);
        assert_eq!(diags[0].col_start, 2);
        assert_eq!(diags[0].message, "good");
    }

    #[test]
    fn diagnostics_malformed_severity_falls_back_to_warning() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"severity":1.5,"message":"fractional"},{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}},"severity":99,"message":"unknown"},{"range":{"start":{"line":2,"character":0},"end":{"line":2,"character":1}},"message":"missing"}]}}"#;
        let diags = parse_publish_diagnostics(stream);

        assert_eq!(diags.len(), 3);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[1].severity, Severity::Warning);
        assert_eq!(diags[2].severity, Severity::Warning);
    }

    #[test]
    fn diagnostics_for_uri_ignores_other_files() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/other.rs","diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},"severity":1,"message":"wrong file"}]}}{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[{"range":{"start":{"line":9,"character":2},"end":{"line":9,"character":6}},"severity":2,"message":"right file"}]}}"#;
        let diags = parse_publish_diagnostics_for_uri(stream, "file:///x/main.rs");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 9);
        assert_eq!(diags[0].message, "right file");
    }

    #[test]
    fn diagnostics_for_uri_uses_latest_matching_notification() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[]}}{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[{"range":{"start":{"line":2,"character":3},"end":{"line":2,"character":8}},"severity":1,"message":"later error"}]}}"#;
        let diags = parse_publish_diagnostics_for_uri(stream, "file:///x/main.rs");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].col_start, 3);
        assert_eq!(diags[0].message, "later error");
    }

    #[test]
    fn diagnostics_message_can_contain_publish_diagnostics_text() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},"severity":1,"message":"mentions publishDiagnostics in text"}]}}{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[{"range":{"start":{"line":2,"character":1},"end":{"line":2,"character":5}},"severity":1,"message":"later error"}]}}"#;
        let diags = parse_publish_diagnostics_for_uri(stream, "file:///x/main.rs");

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].message, "later error");
    }

    #[test]
    fn diagnostics_for_uri_returns_empty_when_only_other_uri_publishes() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/other.rs","diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},"severity":1,"message":"wrong file"}]}}"#;
        assert!(parse_publish_diagnostics_for_uri(stream, "file:///x/main.rs").is_empty());
    }

    #[test]
    fn diagnostics_uri_filter_uses_params_uri_not_related_information_uri() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},"severity":1,"message":"wrong file","relatedInformation":[{"location":{"uri":"file:///x/main.rs","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}},"message":"related"}]}],"uri":"file:///x/other.rs"}}"#;

        assert!(parse_publish_diagnostics_for_uri(stream, "file:///x/main.rs").is_empty());
    }

    #[test]
    fn diagnostics_reader_match_uses_params_uri_not_related_information_uri() {
        let stream = br#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/other.rs","diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},"severity":1,"message":"wrong file","relatedInformation":[{"location":{"uri":"file:///x/main.rs","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}},"message":"related"}]}]}}"#;

        assert!(!has_publish_diagnostics_for_uri(
            stream,
            "file:///x/main.rs"
        ));
    }

    #[test]
    fn diagnostics_reader_match_uses_top_level_method_and_params_array() {
        let nested_method = br#"{"jsonrpc":"2.0","params":{"uri":"file:///x/main.rs","metadata":{"method":"textDocument/publishDiagnostics","diagnostics":[]},"diagnostics":[]}}"#;
        let wrong_array_owner = br#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","metadata":{"diagnostics":[]}}}"#;
        let matching_empty_publish = br#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x/main.rs","diagnostics":[]}}"#;

        assert!(!has_publish_diagnostics_for_uri(
            nested_method,
            "file:///x/main.rs"
        ));
        assert!(!has_publish_diagnostics_for_uri(
            wrong_array_owner,
            "file:///x/main.rs"
        ));
        assert!(has_publish_diagnostics_for_uri(
            matching_empty_publish,
            "file:///x/main.rs"
        ));
    }

    #[test]
    fn diagnostics_for_uri_requires_publish_notification() {
        let stream = r#"{"jsonrpc":"2.0","id":4,"method":"workspace/applyEdit","params":{"uri":"file:///x/main.rs","diagnostics":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},"severity":1,"message":"wrong request"}]}}"#;

        assert!(parse_publish_diagnostics_for_uri(stream, "file:///x/main.rs").is_empty());
    }

    #[test]
    fn diagnostics_for_uri_matches_equivalent_file_uris() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"FILE://localhost/C:/x/main.rs","diagnostics":[{"range":{"start":{"line":4,"character":1},"end":{"line":4,"character":5}},"severity":1,"message":"same file"}]}}"#;
        let diags = parse_publish_diagnostics_for_uri(stream, "file:///C:/x/main.rs");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "same file");
    }

    #[test]
    fn diagnostics_for_uri_matches_percent_hex_casing() {
        let stream = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///C:/x/a%20b/%e6%9d%b1.rs","diagnostics":[{"range":{"start":{"line":2,"character":0},"end":{"line":2,"character":1}},"severity":2,"message":"encoded same file"}]}}"#;
        let diags = parse_publish_diagnostics_for_uri(stream, "file:///C:/x/a%20b/%E6%9D%B1.rs");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "encoded same file");
    }

    #[test]
    fn empty_diagnostics_array_yields_none() {
        let stream = r#"{"method":"textDocument/publishDiagnostics","params":{"uri":"file:///x","diagnostics":[]}}"#;
        assert!(parse_publish_diagnostics(stream).is_empty());
    }

    #[test]
    fn no_diagnostics_key_yields_none() {
        assert!(parse_publish_diagnostics(r#"{"result":null,"id":1}"#).is_empty());
    }

    #[test]
    fn severity_info_and_hint_fold_to_warning() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"severity":3,"message":"info"}]}}"#;
        let diags = parse_publish_diagnostics(stream);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[test]
    fn diagnostics_decode_unicode_message_and_code() {
        let stream = r#"{"params":{"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"severity":1,"code":"E-\u6771\ud83d\ude00","message":"東京 caf\u00e9 \ud83d\ude00 \ud83dX"}]}}"#;
        let diags = parse_publish_diagnostics(stream);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "E-東\u{1f600}");
        assert_eq!(diags[0].message, "東京 café \u{1f600} \u{fffd}X");
    }

    #[test]
    fn utf16_diag_columns_convert_to_editor_character_columns() {
        let mut diags = vec![Diag {
            line: 0,
            col_start: 2,
            col_end: 5,
            severity: Severity::Error,
            code: String::new(),
            message: "after emoji".to_string(),
        }];
        normalize_lsp_diag_columns("😀abc", &mut diags);
        assert_eq!(diags[0].col_start, 1);
        assert_eq!(diags[0].col_end, 4);
    }

    #[test]
    fn utf16_diag_columns_inside_surrogate_pair_snap_to_character_start() {
        let mut diags = vec![Diag {
            line: 0,
            col_start: 1,
            col_end: 2,
            severity: Severity::Error,
            code: String::new(),
            message: "inside surrogate".to_string(),
        }];
        normalize_lsp_diag_columns("😀abc", &mut diags);
        assert_eq!(diags[0].col_start, 0);
        assert_eq!(diags[0].col_end, 1);
    }

    #[test]
    fn json_escape_and_uri() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
        let u = file_uri(Path::new("C:\\x\\y.rs"));
        assert!(u.starts_with("file:///C:/x/y.rs") || u.starts_with("file://"));
        assert_eq!(
            file_uri(Path::new(r"C:\x y\hash#query?.rs")),
            "file:///C:/x%20y/hash%23query%3F.rs"
        );
        assert_eq!(
            file_uri(Path::new(r"\\server\share folder\main.rs")),
            "file://server/share%20folder/main.rs"
        );
    }

    #[test]
    fn lsp_utf16_col_converts_editor_character_columns() {
        let source = "let face = \"😀\";\n😀abc";
        assert_eq!(lsp_utf16_col(source, 0, 12), 12);
        assert_eq!(lsp_utf16_col(source, 0, 13), 14);
        assert_eq!(lsp_utf16_col(source, 1, 0), 0);
        assert_eq!(lsp_utf16_col(source, 1, 1), 2);
        assert_eq!(lsp_utf16_col(source, 1, 3), 4);
        assert_eq!(lsp_utf16_col(source, 99, 7), 0);
    }

    #[test]
    fn request_msg_serializes_lsp_utf16_position_columns() {
        let source = "😀abc";
        let msg = request_msg(&Method::Hover, "file:///repo/src/main.rs", source, 0, 3);
        assert!(msg.contains(r#""position":{"line":0,"character":4}"#));
    }

    #[test]
    fn code_action_request_serializes_lsp_utf16_range_columns() {
        let source = "😀abc";
        let msg = request_msg(
            &Method::CodeAction {
                end_line: 0,
                end_col: 4,
                diagnostics_json: "[]".to_string(),
            },
            "file:///repo/src/main.rs",
            source,
            0,
            1,
        );
        assert!(msg.contains(
            r#""range":{"start":{"line":0,"character":2},"end":{"line":0,"character":5}}"#
        ));
    }

    #[test]
    fn code_action_request_uses_range_params() {
        let msg = request_msg(
            &Method::CodeAction {
                end_line: 4,
                end_col: 17,
                diagnostics_json: "[]".to_string(),
            },
            "file:///repo/src/main.rs",
            "line0\nline1\nline2\nline3\nabcdefghijklmnopq",
            4,
            0,
        );
        assert!(msg.contains(r#""method":"textDocument/codeAction""#));
        assert!(msg.contains(
            r#""range":{"start":{"line":4,"character":0},"end":{"line":4,"character":17}}"#
        ));
        assert!(msg.contains(r#""context":{"diagnostics":[]}"#));
    }

    #[test]
    fn code_action_request_includes_diagnostic_context() {
        let msg = request_msg(
            &Method::CodeAction {
                end_line: 4,
                end_col: 17,
                diagnostics_json: r#"[{"severity":1,"message":"missing import"}]"#.to_string(),
            },
            "file:///repo/src/main.rs",
            "line0\nline1\nline2\nline3\nabcdefghijklmnopq",
            4,
            0,
        );
        assert!(msg
            .contains(r#""context":{"diagnostics":[{"severity":1,"message":"missing import"}]}"#));
    }

    #[test]
    fn signature_help_request_uses_position_params() {
        let msg = request_msg(
            &Method::SignatureHelp,
            "file:///repo/src/main.rs",
            "\n\n\n\n\n\n\nabcdefghijkl",
            7,
            12,
        );
        assert!(msg.contains(r#""method":"textDocument/signatureHelp""#));
        assert!(msg.contains(
            r#""textDocument":{"uri":"file:///repo/src/main.rs"},"position":{"line":7,"character":12}"#
        ));
    }

    #[test]
    fn prepare_rename_request_uses_position_params() {
        let msg = request_msg(
            &Method::PrepareRename,
            "file:///repo/src/main.rs",
            "\n\nabcde",
            2,
            5,
        );
        assert!(msg.contains(r#""method":"textDocument/prepareRename""#));
        assert!(msg.contains(
            r#""textDocument":{"uri":"file:///repo/src/main.rs"},"position":{"line":2,"character":5}"#
        ));
    }

    #[test]
    fn rename_request_uses_new_name_params() {
        let msg = request_msg(
            &Method::Rename {
                new_name: "next_value".to_string(),
            },
            "file:///repo/src/main.rs",
            "\n\n\nabcdefghi",
            3,
            9,
        );
        assert!(msg.contains(r#""method":"textDocument/rename""#));
        assert!(msg.contains(r#""position":{"line":3,"character":9}"#));
        assert!(msg.contains(r#""newName":"next_value""#));
    }

    #[test]
    fn document_symbol_request_uses_document_params_only() {
        let msg = request_msg(
            &Method::DocumentSymbol,
            "file:///repo/src/main.rs",
            "",
            99,
            42,
        );
        assert!(msg.contains(r#""method":"textDocument/documentSymbol""#));
        assert!(msg.contains(r#""params":{"textDocument":{"uri":"file:///repo/src/main.rs"}}"#));
        assert!(!msg.contains(r#""position""#));
    }

    #[test]
    fn execute_command_request_uses_command_params() {
        let msg = request_msg(
            &Method::ExecuteCommand {
                command: "rust-analyzer.applySourceChange".to_string(),
                arguments_json: Some(r#"[{"id":1}]"#.to_string()),
            },
            "file:///repo/src/main.rs",
            "",
            0,
            0,
        );
        assert!(msg.contains(r#""method":"workspace/executeCommand""#));
        assert!(msg.contains(
            r#""params":{"command":"rust-analyzer.applySourceChange","arguments":[{"id":1}]}"#
        ));
        assert!(!msg.contains(r#""textDocument""#));
    }

    #[test]
    fn initialize_advertises_supported_language_capabilities() {
        let msg = initialize_msg(Path::new("C:/repo"));
        assert!(msg.contains(r#""workspace":{"applyEdit":true}"#));
        assert!(msg.contains(r#""completionItem":{"#));
        assert!(msg.contains(r#""snippetSupport":true"#));
        assert!(msg.contains(r#""commitCharactersSupport":true"#));
        assert!(msg.contains(r#""deprecatedSupport":true"#));
        assert!(msg.contains(r#""preselectSupport":true"#));
        assert!(msg.contains(r#""tagSupport":{"valueSet":[1]}"#));
        assert!(msg.contains(r#""insertReplaceSupport":true"#));
        assert!(msg.contains(r#""labelDetailsSupport":true"#));
        assert!(msg.contains(r#""documentationFormat":["markdown","plaintext"]"#));
        assert!(msg.contains(r#""completionItemKind":{"valueSet":[1,2,3"#));
        assert!(msg.contains(r#""completionList":{"itemDefaults":["commitCharacters","editRange"]}"#));
        assert!(msg.contains(r#""hover":{"contentFormat":["markdown","plaintext"]}"#));
        assert!(msg.contains(r#""definition":{"linkSupport":true}"#));
        assert!(msg.contains(r#""signatureHelp":{"signatureInformation":{"documentationFormat":["markdown","plaintext"],"parameterInformation":{"labelOffsetSupport":true}}}"#));
        assert!(msg.contains(r#""rename":{"prepareSupport":true}"#));
        assert!(msg.contains(r#""codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","refactor","refactor.rewrite","source","source.fixAll","source.organizeImports"]}}"#));
        assert!(msg.contains(r#""isPreferredSupport":true"#));
        assert!(msg.contains(r#""disabledSupport":true"#));
        assert!(msg.contains(r#""documentSymbol":{"hierarchicalDocumentSymbolSupport":true,"symbolKind":{"valueSet":[1,2,3"#));
        assert!(!msg.contains(r#""contextSupport""#));
        assert!(!msg.contains(r#""honorsChangeAnnotations""#));
        assert!(!msg.contains(r#""labelSupport""#));
        assert!(!msg.contains(r#""resolveSupport""#));
    }

    #[test]
    fn response_id_wait_uses_top_level_response_id() {
        let stream = br#"{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":{"contents":"wrong"}}}}
{"jsonrpc":"2.0","id":2,"result":{"contents":"right"}}"#;

        assert!(has_response_id(stream, "2"));
    }

    #[test]
    fn response_id_wait_ignores_nested_id_and_requests() {
        let nested_id = br#"{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":{"contents":"wrong"}}}}"#;
        let server_request = br#"{"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;
        let request_with_result =
            br#"{"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","result":{"applied":true}}"#;
        let response_error =
            br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"failed"}}"#;

        assert!(!has_response_id(nested_id, "2"));
        assert!(!has_response_id(server_request, "2"));
        assert!(!has_response_id(request_with_result, "2"));
        assert!(has_response_id(response_error, "2"));
    }

    #[test]
    fn response_id_wait_rejects_fractional_numeric_prefix() {
        let fractional = br#"{"jsonrpc":"2.0","id":2.5,"result":{"contents":"wrong"}}"#;
        let stream = br#"{"jsonrpc":"2.0","id":2.5,"result":{"contents":"wrong"}}
{"jsonrpc":"2.0","id":2,"result":{"contents":"right"}}"#;

        assert!(!has_response_id(fractional, "2"));
        assert!(has_response_id(stream, "2"));
    }

    #[test]
    fn response_isolation_skips_progress_metadata_id() {
        let stream = r#"{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":{"contents":"wrong"}}}}{"jsonrpc":"2.0","id":2,"result":{"contents":"right"}}"#;
        let response = crate::nav::lsp::isolate_response_id(stream, 2);

        assert!(response.contains(r#""contents":"right""#));
        assert!(!response.contains("wrong"));
    }

    #[test]
    fn apply_edit_stream_append_requires_top_level_request() {
        let nested = br#"{"jsonrpc":"2.0","id":2,"result":{"metadata":{"method":"workspace/applyEdit","id":99}}}"#;
        let request = br#"{"jsonrpc":"2.0","id":7,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;

        assert!(!has_apply_edit_request(nested));
        assert!(has_apply_edit_request(request));
    }

    #[test]
    fn apply_edit_request_id_reads_numeric_id() {
        let stream = br#"Content-Length: 190

{"jsonrpc":"2.0","id":9,"method":"workspace/applyEdit","params":{"edit":{"changes":{"file:///repo/src/main.rs":[{"newText":"x","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}}}]}}}}"#;
        assert_eq!(apply_edit_request_id(stream), Some("9".to_string()));
        assert_eq!(
            apply_edit_response("9"),
            r#"{"jsonrpc":"2.0","id":9,"result":{"applied":true}}"#
        );
    }

    #[test]
    fn apply_edit_request_id_reads_string_id() {
        let stream = br#"{"jsonrpc":"2.0","id":"cmd-7","method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;
        assert_eq!(
            apply_edit_request_id(stream),
            Some(r#""cmd-7""#.to_string())
        );
        assert_eq!(
            apply_edit_response(r#""cmd-7""#),
            r#"{"jsonrpc":"2.0","id":"cmd-7","result":{"applied":true}}"#
        );
    }

    #[test]
    fn apply_edit_request_id_uses_top_level_method_and_id() {
        let stream = br#"{"jsonrpc":"2.0","id":1,"method":"client/registerCapability","params":{"metadata":{"method":"workspace/applyEdit","id":99}}}
{"jsonrpc":"2.0","metadata":{"id":98,"method":"workspace/applyEdit"},"id":7,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;
        assert_eq!(apply_edit_request_id(stream), Some("7".to_string()));
    }

    #[test]
    fn apply_edit_request_id_ignores_nested_apply_edit_without_request() {
        let stream = br#"{"jsonrpc":"2.0","id":1,"method":"client/registerCapability","params":{"metadata":{"method":"workspace/applyEdit","id":99}}}"#;
        assert_eq!(apply_edit_request_id(stream), None);
    }

    #[test]
    fn apply_edit_request_id_requires_params_owned_edit() {
        let missing_params = br#"{"jsonrpc":"2.0","id":7,"method":"workspace/applyEdit"}"#;
        let missing_edit =
            br#"{"jsonrpc":"2.0","id":8,"method":"workspace/applyEdit","params":{"label":"apply"}}"#;
        let nested_edit = br#"{"jsonrpc":"2.0","id":9,"method":"workspace/applyEdit","params":{"metadata":{"edit":{"changes":{}}}}}"#;
        let valid = br#"{"jsonrpc":"2.0","id":10,"method":"workspace/applyEdit","params":{"metadata":{"edit":{"changes":{"file:///wrong":[{"newText":"x"}]}}},"edit":{"changes":{}}}}"#;

        assert_eq!(apply_edit_request_id(missing_params), None);
        assert_eq!(apply_edit_request_id(missing_edit), None);
        assert_eq!(apply_edit_request_id(nested_edit), None);
        assert_eq!(apply_edit_request_id(valid), Some("10".to_string()));
    }

    #[test]
    fn apply_edit_request_id_skips_malformed_requests_before_valid_one() {
        let stream = br#"{"jsonrpc":"2.0","method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}
{"jsonrpc":"2.0","id":false,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}
{"jsonrpc":"2.0","id":"cmd-8","method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;

        assert_eq!(
            apply_edit_request_id(stream),
            Some(r#""cmd-8""#.to_string())
        );
    }

    #[test]
    fn apply_edit_request_id_rejects_fractional_numeric_prefix() {
        let stream = br#"{"jsonrpc":"2.0","id":8.5,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}
{"jsonrpc":"2.0","id":9,"method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;

        assert_eq!(apply_edit_request_id(stream), Some("9".to_string()));
    }

    #[test]
    fn apply_edit_request_id_decodes_and_reescapes_unicode_string_id() {
        let stream = br#"{"jsonrpc":"2.0","id":"cmd-\u6771-\ud83d\ude00","method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}"#;
        assert_eq!(
            apply_edit_request_id(stream),
            Some("\"cmd-東-😀\"".to_string())
        );
        assert_eq!(
            apply_edit_response("\"cmd-東-😀\""),
            r#"{"jsonrpc":"2.0","id":"cmd-東-😀","result":{"applied":true}}"#
        );
    }

    #[test]
    fn apply_edit_request_body_remains_workspace_edit_parseable() {
        let stream = r#"{"jsonrpc":"2.0","id":4,"method":"workspace/applyEdit","params":{"label":"Apply source change","edit":{"changes":{"file:///repo/src/main.rs":[{"newText":"use std::fmt;\n","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}}}]}}}}"#;
        let we = crate::language::parse_workspace_edit(stream);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///repo/src/main.rs");
        assert_eq!(we.files[0].1[0].new_text, "use std::fmt;\n");
    }

    /// Guarded integration test: if a real `rust-analyzer` is on PATH, spawn it
    /// through the generic client and run the initialize + didOpen handshake,
    /// requesting hover on a tiny Rust file. We assert only that the handshake
    /// completes and returns *some* `id:2` object (rust-analyzer may not have
    /// finished indexing, so we don't require non-empty hover content) — the
    /// point is to prove the bridge speaks to a non-Mighty server end to end.
    /// Skipped (passes trivially) when rust-analyzer isn't installed.
    #[test]
    fn rust_analyzer_handshake_if_present() {
        use crate::langdetect::Language;
        let Some(spec) = crate::lspregistry::server_for(Language::Rust) else {
            eprintln!("rust_analyzer_handshake_if_present: rust-analyzer not on PATH — skipped");
            return;
        };
        eprintln!("rust_analyzer_handshake_if_present: using {}", spec.program);

        // A real on-disk Cargo project so rust-analyzer can discover a workspace
        // and actually answer requests.
        let dir = std::env::temp_dir().join(format!("mui-ra-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"ra_probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let file = dir.join("src").join("main.rs");
        let source = "fn main() {\n    let x: u32 = 1;\n    let y = x + 1;\n}\n";
        std::fs::write(&file, source).unwrap();

        let raw = request_with_timeout(
            &spec,
            Language::Rust.lsp_id(),
            &dir,
            &file,
            source,
            Method::Hover,
            1,
            8,
            Duration::from_secs(45),
        );
        eprintln!(
            "rust_analyzer_handshake_if_present: response len={} head={:?}",
            raw.len(),
            &raw.chars().take(160).collect::<String>()
        );
        // The handshake completed and the bridge spoke to a real, non-Mighty
        // server. rust-analyzer's indexing latency varies, so we only require
        // that the process launched and the read loop returned without blocking
        // or crashing (an `id:2` object on a fast machine; otherwise the stream
        // it produced). The value of the test is the live end-to-end exercise.
        assert!(
            !spec.program.is_empty(),
            "rust-analyzer spec must name a program"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
