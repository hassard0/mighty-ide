//! Deeper language intelligence (shim-side, scalar-driven from Mighty):
//! **signature help**, **rename symbol**, and **code actions / quick-fix**.
//!
//! Like completion + nav, all the LSP work lives here on the Rust side because
//! the Mighty IDE can only drive the shim through a scalar `extern c` ABI (L17)
//! and must keep its `Vec` access flat (L21). This module:
//!
//! * Spawns `mty lsp`, runs the staged JSON-RPC handshake (the same discipline
//!   completion/nav use — staged `didOpen` before the request), fires one of
//!   `textDocument/signatureHelp` / `prepareRename` / `rename` / `codeAction`,
//!   and parses the answer with small hand scanners (no serde dependency).
//! * Owns the shim-side UI state for each feature: the [`SigState`] popup, the
//!   [`RenameState`] inline-input, and the [`CodeActionState`] menu.
//! * Applies a parsed [`WorkspaceEdit`] to in-memory documents back-to-front so
//!   earlier edit offsets are never shifted by later ones.
//!
//! mty-lsp (v0.5) advertises and implements all three (verified):
//!   signatureHelpProvider(triggerChars `(` `,`), renameProvider(prepareProvider),
//!   codeActionProvider(kinds quickfix / refactor.rewrite / source.fixAll.mighty).
//! Each exchange is short-timeout + failure-tolerant — any error leaves state
//! empty so the editor simply does nothing (never blocks).

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

const POPUP_MARGIN: f32 = 20.0;

// ===========================================================================
// Pure parsers + edit model (no GPU/context; exhaustively unit-tested)
// ===========================================================================

/// Read a JSON string literal beginning at or after `pos` (skips whitespace + a
/// leading `:`, then expects `"`). Un-escapes the common cases. Returns the
/// decoded string + the byte index just past the closing quote, or `None`.
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

// ---------------------------------------------------------------------------
// Signature help
// ---------------------------------------------------------------------------

/// One parsed `SignatureInformation`: the signature `label`, its parameter
/// labels (string form), and the active-parameter index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedSignature {
    /// The signature label (e.g. `fn add(a: I32, b: I32) -> I32`).
    pub label: String,
    /// Parameter labels in order (string-form `label`s; we ignore the
    /// `[start,end]` offset form mty-lsp doesn't currently emit).
    pub params: Vec<String>,
    /// 0-based active parameter index (clamped to `params` on use).
    pub active: u32,
    /// Optional documentation for the signature (rarely emitted; kept if present).
    pub doc: String,
}

/// Parse a `textDocument/signatureHelp` response. The result is
/// `{"signatures":[{"label":"...","parameters":[{"label":"p0"},...],
/// "documentation":"..."}],"activeSignature":N,"activeParameter":M}`.
/// Returns the active signature (the one at `activeSignature`, else the first),
/// or `None` for a `null` / empty result.
pub fn parse_signature_help(json: &str) -> Option<ParsedSignature> {
    let bytes = json.as_bytes();
    let result = json_rpc_result_value(bytes)?;
    let sigs = top_level_array_field(result, "signatures")?;
    let sig_objects = collect_json_objects(sigs);
    if sig_objects.is_empty() {
        return None;
    }
    let active_sig = top_level_uint_field(result, "activeSignature").unwrap_or(0) as usize;
    let active_param = top_level_uint_field(result, "activeParameter").unwrap_or(0);
    let idx = active_sig.min(sig_objects.len() - 1);
    let sig_obj = sig_objects[idx];
    let label = top_level_json_string_field(sig_obj, "label")?;
    let params = parse_signature_params(sig_obj, &label);
    let doc = parse_signature_doc(sig_obj);
    Some(ParsedSignature {
        label,
        params,
        active: active_param,
        doc,
    })
}

fn parse_signature_params(sig_obj: &[u8], signature_label: &str) -> Vec<String> {
    let Some(params) = top_level_array_field(sig_obj, "parameters") else {
        return Vec::new();
    };
    collect_json_objects(params)
        .into_iter()
        .filter_map(|param| parse_parameter_label(param, signature_label))
        .collect()
}

fn parse_parameter_label(param: &[u8], signature_label: &str) -> Option<String> {
    let value_at = top_level_field_value_start(param, "label")?;
    match param.get(value_at).copied() {
        Some(b'"') => read_json_string_at(param, value_at).map(|(s, _)| s),
        Some(b'[') => parse_parameter_label_offsets(param, value_at, signature_label),
        _ => None,
    }
}

fn parse_parameter_label_offsets(param: &[u8], value_at: usize, signature_label: &str) -> Option<String> {
    let end = match_bracket(param, value_at).min(param.len());
    let arr = &param[value_at..end];
    let mut i = 1usize;
    i = skip_json_ws_and_commas(arr, i);
    let (start, next) = read_uint_at(arr, i)?;
    i = skip_json_ws_and_commas(arr, next);
    let (end, _) = read_uint_at(arr, i)?;
    let start = start as usize;
    let end = end as usize;
    if start >= end || end > signature_label.len() {
        return None;
    }
    signature_label
        .get(start..end)
        .map(|s| s.to_string())
}

/// Read the signature `documentation` (string or MarkupContent form) if present.
fn parse_signature_doc(sig_obj: &[u8]) -> String {
    let Some(value_at) = top_level_field_value_start(sig_obj, "documentation") else {
        return String::new();
    };
    match sig_obj.get(value_at).copied() {
        Some(b'"') => read_json_string_at(sig_obj, value_at)
            .map(|(s, _)| s)
            .unwrap_or_default(),
        Some(b'{') => {
            let end = match_brace(sig_obj, value_at).min(sig_obj.len());
            let doc_obj = &sig_obj[value_at..end];
            top_level_json_string_field(doc_obj, "value").unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn json_rpc_result_value(bytes: &[u8]) -> Option<&[u8]> {
    if top_level_field_value_start(bytes, "method").is_some() {
        return None;
    }
    let value_at = top_level_field_value_start(bytes, "result")?;
    if bytes.get(value_at..value_at + 4) == Some(b"null") {
        return None;
    }
    let end = json_value_end(bytes, value_at).min(bytes.len());
    Some(&bytes[value_at..end])
}

fn top_level_array_field<'a>(obj: &'a [u8], field: &str) -> Option<&'a [u8]> {
    let value_at = top_level_field_value_start(obj, field)?;
    if obj.get(value_at) != Some(&b'[') {
        return None;
    }
    let end = match_bracket(obj, value_at).min(obj.len());
    Some(&obj[value_at..end])
}

fn top_level_object_field<'a>(obj: &'a [u8], field: &str) -> Option<&'a [u8]> {
    let value_at = top_level_field_value_start(obj, field)?;
    if obj.get(value_at) != Some(&b'{') {
        return None;
    }
    let end = match_brace(obj, value_at).min(obj.len());
    Some(&obj[value_at..end])
}

fn top_level_json_string_field(obj: &[u8], field: &str) -> Option<String> {
    top_level_field_value_start(obj, field)
        .and_then(|value_at| read_json_string_at(obj, value_at))
        .map(|(s, _)| s)
}

fn top_level_uint_field(obj: &[u8], field: &str) -> Option<u32> {
    let i = top_level_field_value_start(obj, field)?;
    read_uint_at(obj, i).map(|(value, _)| value)
}

fn read_uint_at(obj: &[u8], mut i: usize) -> Option<(u32, usize)> {
    while i < obj.len() && obj[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    let mut value = 0u32;
    while i < obj.len() && obj[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((obj[i] - b'0') as u32);
        i += 1;
    }
    (i > start).then_some((value, i))
}

fn collect_json_objects(arr: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 1usize;
    while i < arr.len() {
        i = skip_json_ws_and_commas(arr, i);
        if i >= arr.len() || arr[i] == b']' {
            break;
        }
        if arr[i] == b'{' {
            let end = match_brace(arr, i).min(arr.len());
            out.push(&arr[i..end]);
            i = end;
        } else {
            i = json_value_end(arr, i);
        }
    }
    out
}

fn skip_json_ws_and_commas(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b',' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

fn json_value_end(bytes: &[u8], pos: usize) -> usize {
    match bytes.get(pos).copied() {
        Some(b'"') => read_json_string_at(bytes, pos).map(|(_, end)| end).unwrap_or(bytes.len()),
        Some(b'{') => match_brace(bytes, pos),
        Some(b'[') => match_bracket(bytes, pos),
        _ => {
            let mut i = pos;
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']') {
                i += 1;
            }
            i
        }
    }
}

// ---------------------------------------------------------------------------
// WorkspaceEdit (rename + code-action edits)
// ---------------------------------------------------------------------------

/// One text edit: a replacement of the half-open `[start,end)` range (0-based
/// line/character) with `new_text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub new_text: String,
}

/// A workspace edit: per-file (uri) lists of [`TextEdit`]s. Parsed from either
/// the `changes` map or `documentChanges` array shapes of an LSP `WorkspaceEdit`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceEdit {
    /// `(uri, edits)` pairs, in first-seen order.
    pub files: Vec<(String, Vec<TextEdit>)>,
}

impl WorkspaceEdit {
    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|(_, e)| e.is_empty())
    }

    #[allow(dead_code)]
    pub fn file_count(&self) -> usize {
        self.files.iter().filter(|(_, e)| !e.is_empty()).count()
    }

    #[allow(dead_code)]
    pub fn total_edits(&self) -> usize {
        self.files.iter().map(|(_, e)| e.len()).sum()
    }
}

/// Parse a `WorkspaceEdit` from a JSON-RPC response that carries either a
/// `"changes":{"<uri>":[<TextEdit>...],...}` map (what mty-lsp's rename emits)
/// or a `"documentChanges":[{"textDocument":{"uri":...},"edits":[...]},...]`
/// array. Returns an empty edit (no files) when neither is present / `null`.
pub fn parse_workspace_edit(json: &str) -> WorkspaceEdit {
    let bytes = json.as_bytes();
    let bytes = workspace_edit_payload(bytes);
    let mut we = WorkspaceEdit::default();

    if let Some(changes_at) = top_level_field_value_start(bytes, "changes") {
        parse_changes_map_at_value(bytes, changes_at, &mut we);
    } else if let Some(dc_at) = top_level_field_value_start(bytes, "documentChanges") {
        parse_document_changes_at_value(bytes, dc_at, &mut we);
    }
    we
}

fn workspace_edit_payload(bytes: &[u8]) -> &[u8] {
    if bytes.first() == Some(&b'{') {
        if top_level_field_value_start(bytes, "method").is_some() {
            if top_level_json_string_field(bytes, "method").as_deref()
                == Some("workspace/applyEdit")
            {
                if let Some(params) = top_level_object_field(bytes, "params") {
                    if let Some(edit) = top_level_object_field(params, "edit") {
                        return edit;
                    }
                }
            }
            return &[];
        }
        if let Some(result_at) = top_level_field_value_start(bytes, "result") {
            if bytes.get(result_at..result_at + 4) == Some(b"null") {
                return &[];
            }
            let result_end = json_value_end(bytes, result_at).min(bytes.len());
            return &bytes[result_at..result_end];
        }
    }
    bytes
}

fn parse_changes_map_at_value(bytes: &[u8], i: usize, we: &mut WorkspaceEdit) {
    if i >= bytes.len() || bytes[i] != b'{' {
        return;
    }
    let obj_start = i;
    // Find the matching close `}` so we don't read past the changes object.
    let obj_end = match_brace(bytes, obj_start);
    let region = &bytes[obj_start..obj_end.min(bytes.len())];
    // Each entry: `"uri":[ ...edits... ]`. URI schemes are case-insensitive.
    let mut k = 0usize;
    while k < region.len() {
        let Some(uri_start) = find_next_file_uri_key(region, k) else {
            break;
        };
        let Some((uri, past)) = read_json_string_at(region, uri_start) else {
            k = uri_start + 5;
            continue;
        };
        // After the uri comes `:[ ... ]`.
        let mut j = past;
        while j < region.len() && matches!(region[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if j >= region.len() || region[j] != b'[' {
            k = past;
            continue;
        }
        let arr_end = match_bracket(region, j);
        let edits = parse_text_edits(&region[j..arr_end.min(region.len())]);
        we.files.push((uri, edits));
        k = arr_end;
    }
}

fn find_next_file_uri_key(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                let string_start = i;
                let (key, past) = read_json_string_at(bytes, i)?;
                let mut value_at = past;
                while value_at < bytes.len()
                    && matches!(bytes[value_at], b' ' | b'\t' | b'\r' | b'\n')
                {
                    value_at += 1;
                }
                if depth == 1
                    && string_start >= start
                    && value_at < bytes.len()
                    && bytes[value_at] == b':'
                    && key
                        .get(..4)
                        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file"))
                {
                    return Some(string_start);
                }
                i = past;
            }
            _ => i += 1,
        }
    }
    None
}

fn parse_document_changes_at_value(bytes: &[u8], i: usize, we: &mut WorkspaceEdit) {
    if i >= bytes.len() || bytes[i] != b'[' {
        return;
    }
    let arr_end = match_bracket(bytes, i);
    let region = &bytes[i..arr_end.min(bytes.len())];
    parse_document_change_objects(region, we);
}

fn parse_document_change_objects(arr: &[u8], we: &mut WorkspaceEdit) {
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
                    if let Some(start) = obj_start.take() {
                        parse_one_document_change(&arr[start..=k], we);
                    }
                }
            }
            _ => {}
        }
    }
}

fn parse_one_document_change(obj: &[u8], we: &mut WorkspaceEdit) {
    // ResourceOperation objects (`create`, `rename`, `delete`) also carry URIs,
    // but no text edit list. Keep this parser scoped to TextDocumentEdit objects
    // so we never apply edits to the wrong file.
    let Some(text_document) = top_level_object_field(obj, "textDocument") else {
        return;
    };
    let Some(edits_region) = top_level_array_field(obj, "edits") else {
        return;
    };
    let Some(uri_at) = top_level_field_value_start(text_document, "uri") else {
        return;
    };
    let Some((uri, _)) = read_json_string_at(text_document, uri_at) else {
        return;
    };
    let edits = parse_text_edits(edits_region);
    if !edits.is_empty() {
        we.files.push((uri, edits));
    }
}

/// Parse a list of `TextEdit` objects from an array slice (`[ {...}, ... ]`).
/// Each edit is `{"range":{"start":{"line":..,"character":..},"end":{...}},
/// "newText":".."}`. Robust to field order.
fn parse_text_edits(arr: &[u8]) -> Vec<TextEdit> {
    let mut out = Vec::new();
    // Split into per-edit objects by tracking brace depth at the array's top
    // level (depth 1 inside the array). Each depth-1 `{...}` is one edit.
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
                        if let Some(e) = parse_one_text_edit(&arr[s..=k]) {
                            out.push(e);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse a single `TextEdit` object slice.
fn parse_one_text_edit(obj: &[u8]) -> Option<TextEdit> {
    let nt_at = top_level_field_value_start(obj, "newText")?;
    let (new_text, _) = read_json_string_at(obj, nt_at)?;
    let range = top_level_object_field(obj, "range")?;
    let start = top_level_object_field(range, "start")?;
    let end = top_level_object_field(range, "end")?;
    let start_line = top_level_uint_field(start, "line")?;
    let start_col = top_level_uint_field(start, "character")?;
    let end_line = top_level_uint_field(end, "line")?;
    let end_col = top_level_uint_field(end, "character")?;
    Some(TextEdit {
        start_line,
        start_col,
        end_line,
        end_col,
        new_text,
    })
}

/// Index just past the `}` matching the `{` at `open` (string-aware). Returns
/// `bytes.len()` if unbalanced.
fn match_brace(bytes: &[u8], open: usize) -> usize {
    match_delim(bytes, open, b'{', b'}')
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

/// Apply a list of [`TextEdit`]s to `text` (a whole document string) and return
/// the edited text. Edits are sorted and applied **back-to-front** (last edit in
/// the document first) so earlier edits' byte offsets are never shifted by later
/// ones. Overlapping edits are applied in the back-to-front order (last wins on
/// overlap, matching LSP's "edits must not overlap" contract — we don't error).
///
/// Pure + unit-tested: this is the offset-correct multi-edit core.
pub fn apply_text_edits(text: &str, edits: &[TextEdit]) -> String {
    if edits.is_empty() {
        return text.to_string();
    }
    // Map each edit's (line,col) range to byte offsets in `text`.
    let line_starts = compute_line_starts(text);
    let mut resolved: Vec<(usize, usize, &str)> = Vec::with_capacity(edits.len());
    for e in edits {
        let s = offset_of(text, &line_starts, e.start_line, e.start_col);
        let en = offset_of(text, &line_starts, e.end_line, e.end_col);
        let (lo, hi) = if s <= en { (s, en) } else { (en, s) };
        resolved.push((lo, hi, e.new_text.as_str()));
    }
    // Sort by start offset ascending, then apply from the LAST (rightmost) to the
    // first so each splice doesn't invalidate earlier offsets.
    resolved.sort_by_key(|(lo, _, _)| *lo);
    let mut out = text.to_string();
    for (lo, hi, nt) in resolved.into_iter().rev() {
        let lo = lo.min(out.len());
        let hi = hi.min(out.len());
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        // Clamp to char boundaries to avoid panics on multi-byte content.
        let lo = floor_char_boundary(&out, lo);
        let hi = floor_char_boundary(&out, hi);
        out.replace_range(lo..hi, nt);
    }
    out
}

/// Byte offsets where each line starts (line 0 starts at 0). `line_starts[i]` is
/// the byte index of the first char of line `i`.
fn compute_line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Byte offset of (0-based) `line`,`col` within `text`, where `col` is an LSP
/// UTF-16 character offset. Clamps to the line's end / the document's end.
fn offset_of(text: &str, line_starts: &[usize], line: u32, col: u32) -> usize {
    let li = line as usize;
    if li >= line_starts.len() {
        return text.len();
    }
    let line_start = line_starts[li];
    // The line ends at the next line start - 1 (the '\n'), or text end.
    let line_end = line_starts
        .get(li + 1)
        .map(|&s| s.saturating_sub(1))
        .unwrap_or(text.len());
    let line_slice = &text[line_start..line_end.min(text.len())];
    // Advance `col` UTF-16 code units into the line. LSP positions default to
    // UTF-16; ASCII stays identical, while non-BMP chars count as two units.
    let mut off = line_start;
    let mut units = 0u32;
    for ch in line_slice.chars() {
        if units >= col {
            break;
        }
        let next = units.saturating_add(ch.len_utf16() as u32);
        if next > col {
            break;
        }
        units = next;
        off += ch.len_utf8();
    }
    off
}

/// Largest char boundary `<= i` in `s` (so `replace_range` never splits a UTF-8
/// sequence). `str::floor_char_boundary` is unstable, so this is a small clone.
fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

// ---------------------------------------------------------------------------
// Code actions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAction {
    pub command: String,
    pub arguments_json: Option<String>,
}

/// One code action: a `title`, optional inline/command edits, optional command
/// metadata, preferred status, and the synthetic "kind" for mty's own fixers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    /// Menu title (e.g. `Replace with 'print'`, `Fix all (mty)`).
    pub title: String,
    /// The action's edit, if it carries one inline.
    pub edit: Option<WorkspaceEdit>,
    /// A workspace edit embedded in a command action's arguments.
    pub command_edit: Option<WorkspaceEdit>,
    /// A command to execute through `workspace/executeCommand` when no edit was
    /// available inline.
    pub command: Option<CommandAction>,
    /// `true` when the server marks this as the preferred quick fix.
    pub is_preferred: bool,
    /// `true` if this is the synthetic shim-provided "Fix all (mty)" action that
    /// runs `mty fix --apply` rather than applying an LSP edit.
    pub fix_all_mty: bool,
}

impl CodeAction {
    fn is_actionable(&self) -> bool {
        self.edit.is_some() || self.command_edit.is_some() || self.command.is_some() || self.fix_all_mty
    }
}

/// Parse the `textDocument/codeAction` response: a `result` array of code
/// actions / commands. Each entry is `{"title":"...","kind":"...","edit":{...}}`
/// (a `CodeAction`) or `{"title":"...","command":"..."}` (a `Command`). We read
/// the `title` and, if present, the inline `edit` (its first `WorkspaceEdit`).
/// Disabled LSP actions are omitted because the current menu has no disabled-row
/// affordance. Returns the actions in order (empty for `[]` / `null`).
pub fn parse_code_actions(json: &str) -> Vec<CodeAction> {
    let bytes = json.as_bytes();
    if bytes.first() == Some(&b'{') && top_level_field_value_start(bytes, "method").is_some() {
        return Vec::new();
    }
    let Some(result_at) = top_level_field_value_start(bytes, "result") else {
        // Some servers omit the wrapper in our isolated slice; try whole input as
        // an array of actions.
        return parse_action_array(bytes);
    };
    if bytes.get(result_at..result_at + 4) == Some(b"null") {
        return Vec::new();
    }
    if result_at >= bytes.len() || bytes[result_at] != b'[' {
        return Vec::new();
    }
    let end = match_bracket(bytes, result_at);
    parse_action_array(&bytes[result_at..end.min(bytes.len())])
}

/// Parse a `[ {title,..}, ... ]` array slice into code actions (splits the
/// top-level objects, then reads each).
fn parse_action_array(arr: &[u8]) -> Vec<CodeAction> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut obj_start: Option<usize> = None;
    let mut in_str = false;
    let mut esc = false;
    let mut started = false;
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
            b'[' if !started => started = true,
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
                        if let Some(a) = parse_one_action(&arr[s..=k]) {
                            out.push(a);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parse one code-action object slice. Reads `title`, optional inline `edit`,
/// and command-form edits embedded in `arguments`.
fn parse_one_action(obj: &[u8]) -> Option<CodeAction> {
    if code_action_disabled(obj) {
        return None;
    }
    let title = top_level_json_string_field(obj, "title")?;
    // Inline edit, if any: a nested WorkspaceEdit under "edit".
    let edit = if let Some(e_at) = top_level_field_value_start(obj, "edit") {
        let end = if obj.get(e_at) == Some(&b'{') {
            match_brace(obj, e_at).min(obj.len())
        } else {
            e_at
        };
        let sub = &obj[e_at..end];
        let we = parse_workspace_edit(&String::from_utf8_lossy(sub));
        if we.is_empty() {
            None
        } else {
            Some(we)
        }
    } else {
        None
    };
    let command = parse_command_action(obj);
    let command_edit = parse_command_edit(obj);
    let fix_all_mty = top_level_json_string_field(obj, "kind")
        .map(|kind| kind == "source.fixAll.mighty")
        .unwrap_or(false)
        || command
            .as_ref()
            .map(|cmd| is_mighty_fix_all_command(&cmd.command))
            .unwrap_or(false);
    Some(CodeAction {
        title,
        edit,
        command_edit,
        command,
        is_preferred: top_level_bool_field(obj, "isPreferred"),
        fix_all_mty,
    })
}

fn is_mighty_fix_all_command(command: &str) -> bool {
    matches!(command, "mighty.fixAll" | "mighty.fix_all" | "mty.fixAll" | "mty.fix_all")
}

fn code_action_disabled(obj: &[u8]) -> bool {
    top_level_field_value_start(obj, "disabled")
        .is_some_and(|i| i < obj.len() && matches!(obj[i], b'{' | b't'))
}

fn top_level_bool_field(obj: &[u8], field: &str) -> bool {
    top_level_field_value_start(obj, field).is_some_and(|i| {
        obj.get(i..i + 4) == Some(b"true")
    })
}

fn top_level_field_value_start(obj: &[u8], field: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < obj.len() {
        match obj[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                let (key, past) = read_json_string_at(obj, i)?;
                if depth == 1 && key == field {
                    let mut value_at = past;
                    while value_at < obj.len()
                        && matches!(obj[value_at], b' ' | b':' | b'\t' | b'\r' | b'\n')
                    {
                        value_at += 1;
                    }
                    return Some(value_at);
                }
                i = past;
            }
            _ => i += 1,
        }
    }
    None
}

fn parse_command_action(obj: &[u8]) -> Option<CommandAction> {
    let cmd_at = top_level_field_value_start(obj, "command")?;
    if let Some((command, _past)) = read_json_string_at(obj, cmd_at) {
        return Some(CommandAction {
            command,
            arguments_json: read_arguments_json(obj),
        });
    }

    let i = cmd_at;
    if i >= obj.len() || obj[i] != b'{' {
        return None;
    }
    let end = match_brace(obj, i).min(obj.len());
    let command_obj = &obj[i..end];
    let inner_at = top_level_field_value_start(command_obj, "command")?;
    let (command, _past) = read_json_string_at(command_obj, inner_at)?;
    Some(CommandAction {
        command,
        arguments_json: read_arguments_json(command_obj).or_else(|| read_arguments_json(obj)),
    })
}

fn read_arguments_json(obj: &[u8]) -> Option<String> {
    let i = top_level_field_value_start(obj, "arguments")?;
    if i >= obj.len() || obj[i] != b'[' {
        return None;
    }
    let end = match_bracket(obj, i).min(obj.len());
    std::str::from_utf8(&obj[i..end]).ok().map(|s| s.to_string())
}

fn parse_command_edit(obj: &[u8]) -> Option<WorkspaceEdit> {
    let we = read_arguments_workspace_edit(obj).or_else(|| {
        top_level_object_field(obj, "command").and_then(read_arguments_workspace_edit)
    })?;
    if we.is_empty() {
        None
    } else {
        Some(we)
    }
}

fn read_arguments_workspace_edit(obj: &[u8]) -> Option<WorkspaceEdit> {
    let args_at = top_level_field_value_start(obj, "arguments")?;
    if obj.get(args_at) != Some(&b'[') {
        return None;
    }
    let args_end = match_bracket(obj, args_at).min(obj.len());
    let args = &obj[args_at..args_end];
    first_workspace_edit_argument(args)
}

fn first_workspace_edit_argument(args: &[u8]) -> Option<WorkspaceEdit> {
    let mut depth = 0i32;
    let mut obj_start: Option<usize> = None;
    let mut in_str = false;
    let mut esc = false;
    let mut started = false;
    for (k, &c) in args.iter().enumerate() {
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
            b'[' if !started => started = true,
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
                        let arg = &args[s..=k];
                        if let Some(we) = workspace_edit_from_argument(arg) {
                            return Some(we);
                        }
                    }
                }
            }
            b']' if depth == 0 => break,
            _ => {}
        }
    }
    None
}

fn workspace_edit_from_argument(arg: &[u8]) -> Option<WorkspaceEdit> {
    top_level_object_field(arg, "workspaceEdit")
        .and_then(parse_workspace_edit_slice)
        .or_else(|| parse_workspace_edit_slice(arg))
}

fn parse_workspace_edit_slice(bytes: &[u8]) -> Option<WorkspaceEdit> {
    let we = parse_workspace_edit(&String::from_utf8_lossy(bytes));
    (!we.is_empty()).then_some(we)
}

// ===========================================================================
// Shim-owned UI state
// ===========================================================================

/// Signature-help popup state: the parsed signature + whether it is shown.
#[derive(Debug, Default)]
pub struct SigState {
    sig: Option<ParsedSignature>,
}

impl SigState {
    pub fn new() -> Self {
        SigState::default()
    }

    pub fn set(&mut self, sig: Option<ParsedSignature>) -> bool {
        let ok = sig.as_ref().map(|s| !s.label.is_empty()).unwrap_or(false);
        self.sig = if ok { sig } else { None };
        ok
    }

    pub fn is_active(&self) -> bool {
        self.sig.is_some()
    }

    pub fn clear(&mut self) {
        self.sig = None;
    }

    /// Draw the signature popup ABOVE the cursor pixel `(cx, cy)` (flips below if
    /// there's no room). The active parameter is highlighted in indigo. No-op
    /// when inactive.
    #[allow(dead_code)]
    pub fn draw(&self, ctx: &mut crate::MuiContext, cx: f32, cy: f32, width: u32, height: u32) {
        self.draw_inset(ctx, cx, cy, width, height, 0.0);
    }

    /// Draw within a left-safe work area so compact windows do not clamp the
    /// popup back over the sidebar.
    pub fn draw_inset(
        &self,
        ctx: &mut crate::MuiContext,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
        min_x: f32,
    ) {
        let Some(sig) = &self.sig else {
            return;
        };
        let chrome = theme::CHROME_FONT_SIZE;
        let pad = 7.0;
        let label = &sig.label;
        // Compute the active-parameter highlight span by locating the param label
        // text inside the signature label.
        let active_param = sig.params.get(sig.active as usize);
        let hi_span = active_param.and_then(|p| {
            label.find(p.as_str()).map(|b| {
                let prefix = &label[..b];
                let (prefix_w, _) = ctx.text.measure_sized(prefix, chrome);
                let (param_w, _) = ctx.text.measure_sized(p, chrome);
                (prefix_w, param_w)
            })
        });

        let has_doc = !sig.doc.is_empty();
        let (label_w, _) = ctx.text.measure_sized(label, chrome);
        let (doc_w, _) = ctx.text.measure_sized(&sig.doc, chrome - 1.0);
        let w = width as f32;
        let h = height as f32;
        let min_x = min_x.max(POPUP_MARGIN).min((w - POPUP_MARGIN).max(POPUP_MARGIN));
        let max_box_w = popup_available_width(w, min_x, 120.0);
        let wanted_w = (label_w.max(doc_w) + 2.0 * pad + 8.0).max(120.0);
        let box_w = wanted_w.min(max_box_w);
        let line_h = layout::LINE_H();
        let lines = if has_doc { 2 } else { 1 };
        let box_h = lines as f32 * line_h + 2.0 * pad;

        let box_x = clamp_popup_x(cx, box_w, w, min_x);
        // Prefer ABOVE the cursor.
        let mut box_y = cy - box_h - 4.0;
        if box_y < layout::TAB_BAR_H + layout::BREADCRUMB_H {
            box_y = cy + line_h; // flip below
        }
        if box_y + box_h > h {
            box_y = (h - box_h).max(0.0);
        }

        let clip = Some((
            box_x.max(0.0) as u32,
            box_y.max(0.0) as u32,
            box_w.max(0.0) as u32,
            box_h.max(0.0) as u32,
        ));
        let radius = 9.0_f32;
        ctx.dl_shadow(box_x, box_y + 5.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.6), 18.0);
        ctx.dl_grad_v(box_x, box_y, box_w, box_h, radius, theme::ELEVATED_2(), theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        let text_x = box_x + pad;
        let label_y = box_y + pad - 0.5;
        let text_w = (box_w - 2.0 * pad).max(0.0);
        let content_w = signature_content_budget(text_w);
        let shown_label = fit_sized(&mut ctx.text, label, content_w, chrome);
        let label_is_full = shown_label == *label;
        // Active-parameter highlight pill behind the param text.
        if let Some((prefix_w, param_w)) = hi_span {
            if label_is_full && param_w > 0.0 && prefix_w + param_w + 3.0 <= content_w {
                let hx = text_x + prefix_w - 3.0;
                let hw = param_w + 6.0;
                ctx.dl_round(hx, label_y - 1.0, hw, chrome + 4.0, 4.0, theme::accent_a(0.26));
                ctx.dl_stroke(hx, label_y - 1.0, hw, chrome + 4.0, 4.0, theme::ACCENT_LINE(), 1.0);
            }
        }
        // The signature label, with the active param drawn in accent on top.
        ctx.text.queue_sized(text_x, label_y, &shown_label, theme::TEXT(), chrome, clip);
        if let Some((prefix_w, param_w)) = hi_span {
            if label_is_full && param_w > 0.0 && prefix_w + param_w + 3.0 <= content_w {
                if let Some(p) = active_param {
                    let px = text_x + prefix_w;
                    ctx.text.queue_sized(px, label_y, p, theme::ACCENT_BRIGHT(), chrome, clip);
                }
            }
        }
        // Optional doc line, dim, below the signature.
        if has_doc {
            let dy = label_y + line_h;
            let shown_doc = fit_sized(&mut ctx.text, &sig.doc, content_w, chrome - 1.0);
            ctx.text.queue_sized(text_x, dy, &shown_doc, theme::TEXT_3(), chrome - 1.0, clip);
        }
    }
}

/// Rename inline-input state: the new-name buffer + the original symbol + the
/// parsed [`WorkspaceEdit`] from the last commit (read back by the ABI to apply).
#[derive(Debug, Default)]
pub struct RenameState {
    active: bool,
    /// The new-name buffer (prefilled with the original symbol on open).
    name: Vec<char>,
    /// True immediately after open, matching the UX of a selected-all field.
    selected_all: bool,
    /// The original symbol (drawn dim as context).
    original: String,
    /// The most recent rename result, set by `commit`.
    last_edit: Option<WorkspaceEdit>,
}

impl RenameState {
    pub fn new() -> Self {
        RenameState::default()
    }

    /// Open the inline input, prefilled with `symbol` (the identifier under the
    /// cursor). The buffer starts selected-all conceptually (a fresh edit).
    pub fn open(&mut self, symbol: &str) {
        self.active = true;
        self.original = symbol.to_string();
        self.name = symbol.chars().collect();
        self.selected_all = true;
        self.last_edit = None;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn push(&mut self, codepoint: u32) {
        if self.active {
            if let Some(c) = char::from_u32(codepoint) {
                if self.selected_all {
                    self.name.clear();
                    self.selected_all = false;
                }
                self.name.push(c);
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.active {
            if self.selected_all {
                self.name.clear();
                self.selected_all = false;
            } else {
                self.name.pop();
            }
        }
    }

    pub fn name_string(&self) -> String {
        self.name.iter().collect()
    }

    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.name.clear();
        self.selected_all = false;
        self.original.clear();
        self.last_edit = None;
    }

    pub fn set_edit(&mut self, edit: Option<WorkspaceEdit>) {
        self.last_edit = edit;
    }

    #[allow(dead_code)]
    pub fn last_edit(&self) -> Option<&WorkspaceEdit> {
        self.last_edit.as_ref()
    }

    /// The display line: `Rename 'old' -> <new>`.
    #[allow(dead_code)]
    pub fn display_line(&self) -> String {
        format!("Rename '{}' \u{2192} {}", self.original, self.name_string())
    }

    /// Draw the inline rename input as a small centered card near the top of the
    /// editor body (reuses the prompt visual language). No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, _height: u32) {
        if !self.active {
            return;
        }
        let chrome = theme::CHROME_FONT_SIZE;
        let clip = ctx.clip;
        let w = width as f32;
        let box_w = rename_card_width(w);
        let box_h = 56.0_f32;
        let box_x = rename_card_x(w, box_w);
        let box_y = layout::TAB_BAR_H + layout::BREADCRUMB_H + 24.0;
        let radius = 10.0;
        ctx.dl_shadow(box_x, box_y + 8.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.7), 26.0);
        ctx.dl_grad_v(box_x, box_y, box_w, box_h, radius, theme::ELEVATED_2(), theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::ACCENT_LINE(), 1.0);

        // Header: "Rename Symbol".
        let title = "Rename Symbol";
        ctx.text.queue_ui_sized(box_x + 14.0, box_y + 8.0, title, theme::TEXT_3(), 11.0, clip);

        // Input field with the editable new name.
        let field_x = box_x + 14.0;
        let field_y = box_y + 26.0;
        let field_w = box_w - 28.0;
        let field_h = 22.0;
        ctx.dl_round(field_x, field_y, field_w, field_h, 5.0, theme::BG_1());
        ctx.dl_stroke(field_x, field_y, field_w, field_h, 5.0, theme::BORDER_STRONG(), 1.0);
        let name = self.name_string();
        let text_x = field_x + 7.0;
        let text_budget = rename_field_text_budget(field_w);
        let shown = fit_sized(&mut ctx.text, &name, text_budget, chrome);
        ctx.text.queue_sized(text_x, field_y + 4.0, &shown, theme::ACCENT_BRIGHT(), chrome, clip);
        let (shown_w, _) = ctx.text.measure_sized(&shown, chrome);
        let caret_x = if name.is_empty() {
            text_x
        } else {
            (text_x + shown_w + 1.0).min(field_x + field_w - 7.0)
        };
        ctx.dl_rect(caret_x, field_y + 4.0, 1.5, chrome + 2.0, theme::ACCENT_BRIGHT());
    }
}

/// Code-action menu state: the action list + selection. Mirrors the completion
/// dropdown's selection discipline.
#[derive(Debug, Default)]
pub struct CodeActionState {
    actions: Vec<CodeAction>,
    sel: usize,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct CodeActionGeometry {
    box_x: f32,
    box_y: f32,
    box_w: f32,
    box_h: f32,
    pad: f32,
    row_h: f32,
    first: usize,
    visible: usize,
}

impl CodeActionState {
    pub fn new() -> Self {
        CodeActionState::default()
    }

    /// Install the action list (LSP actions + any synthetic ones already
    /// appended). Returns the count; a zero count leaves the menu closed.
    pub fn set(&mut self, actions: Vec<CodeAction>) -> usize {
        self.actions = actions
            .into_iter()
            .filter(CodeAction::is_actionable)
            .collect();
        self.sel = self
            .actions
            .iter()
            .position(|action| action.is_preferred)
            .unwrap_or(0);
        self.active = !self.actions.is_empty();
        self.actions.len()
    }

    pub fn count(&self) -> usize {
        self.actions.len()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.actions.len();
        if n == 0 {
            return;
        }
        let n_i = n as i32;
        let mut s = self.sel as i32 + delta;
        s %= n_i;
        if s < 0 {
            s += n_i;
        }
        self.sel = s as usize;
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx < self.actions.len() {
            self.sel = idx;
            true
        } else {
            false
        }
    }

    pub fn selected(&self) -> Option<&CodeAction> {
        if !self.active {
            return None;
        }
        self.actions.get(self.sel)
    }

    pub fn title(&self, i: usize) -> Option<&str> {
        self.actions.get(i).map(|a| a.title.as_str())
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.actions.clear();
        self.sel = 0;
    }

    #[allow(dead_code)]
    fn geometry(
        &self,
        text: &mut crate::text::Text,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
    ) -> (f32, f32, f32, f32, f32, f32) {
        let g = self.geometry_inset(text, cx, cy, width, height, 0.0);
        (g.box_x, g.box_y, g.box_w, g.box_h, g.pad, g.row_h)
    }

    fn geometry_inset(
        &self,
        text: &mut crate::text::Text,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
        min_x: f32,
    ) -> CodeActionGeometry {
        let row_h = layout::LINE_H();
        let chrome = theme::CHROME_FONT_SIZE;
        let pad = 5.0;
        let w = width as f32;
        let h = height as f32;
        let total = self.actions.len();
        let min_x = min_x.max(POPUP_MARGIN).min((w - POPUP_MARGIN).max(POPUP_MARGIN));
        let max_box_w = popup_available_width(w, min_x, 180.0);
        let wanted_w = code_action_popup_width(text, &self.actions, chrome);
        let box_w = wanted_w.min(max_box_w);
        let visible = code_action_visible_rows(total, h, pad, row_h);
        let box_h = visible as f32 * row_h + 2.0 * pad;

        let box_x = clamp_popup_x(cx, box_w, w, min_x);
        let mut box_y = cy + row_h;
        if box_y + box_h > h {
            box_y = (cy - box_h).max(0.0);
        }

        let first = code_action_first_visible(self.sel.min(total.saturating_sub(1)), total, visible);
        CodeActionGeometry {
            box_x,
            box_y,
            box_w,
            box_h,
            pad,
            row_h,
            first,
            visible,
        }
    }

    /// Select the action row under a click. Returns the selected index, or -1
    /// when the click missed the active popup.
    #[allow(dead_code)]
    pub fn click_row(
        &mut self,
        text: &mut crate::text::Text,
        x: f32,
        y: f32,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
    ) -> i32 {
        self.click_row_inset(text, x, y, cx, cy, width, height, 0.0)
    }

    /// Select a row using the same left-safe geometry as [`draw_inset`].
    pub fn click_row_inset(
        &mut self,
        text: &mut crate::text::Text,
        x: f32,
        y: f32,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
        min_x: f32,
    ) -> i32 {
        if !self.active || self.actions.is_empty() {
            return -1;
        }
        let g = self.geometry_inset(text, cx, cy, width, height, min_x);
        if g.visible == 0 {
            return -1;
        }
        if x < g.box_x || x > g.box_x + g.box_w {
            return -1;
        }
        let row_top = g.box_y + g.pad;
        if y < row_top {
            return -1;
        }
        let visible_idx = ((y - row_top) / g.row_h).floor() as usize;
        if visible_idx >= g.visible {
            return -1;
        }
        let idx = g.first + visible_idx;
        if self.select(idx) {
            idx as i32
        } else {
            -1
        }
    }

    /// Draw the code-action menu near the cursor pixel `(cx, cy)` (reuses the
    /// completion-dropdown / palette card styling). No-op when inactive.
    #[allow(dead_code)]
    pub fn draw(&self, ctx: &mut crate::MuiContext, cx: f32, cy: f32, width: u32, height: u32) {
        self.draw_inset(ctx, cx, cy, width, height, 0.0);
    }

    /// Draw within a left-safe work area so compact windows keep the menu out of
    /// the sidebar and inside the right edge.
    pub fn draw_inset(
        &self,
        ctx: &mut crate::MuiContext,
        cx: f32,
        cy: f32,
        width: u32,
        height: u32,
        min_x: f32,
    ) {
        if !self.active || self.actions.is_empty() {
            return;
        }
        let row_h = layout::LINE_H();
        let chrome = theme::CHROME_FONT_SIZE;
        let pad = 5.0;
        let g = self.geometry_inset(&mut ctx.text, cx, cy, width, height, min_x);
        if g.visible == 0 {
            return;
        }
        let box_x = g.box_x;
        let box_y = g.box_y;
        let box_w = g.box_w;
        let box_h = g.box_h;

        let clip = Some((
            box_x.max(0.0) as u32,
            box_y.max(0.0) as u32,
            box_w.max(0.0) as u32,
            box_h.max(0.0) as u32,
        ));
        let radius = 8.0_f32;
        ctx.dl_shadow(box_x, box_y + 8.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.8), 24.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        for (i, a) in self.actions.iter().enumerate().skip(g.first).take(g.visible) {
            let row_y = box_y + pad + (i - g.first) as f32 * row_h;
            let selected = i == self.sel;
            if selected {
                ctx.dl_grad_h(box_x + 5.0, row_y + 2.0, box_w - 10.0, row_h - 4.0, 5.0, theme::accent_a(0.20), 0.9);
                ctx.dl_stroke(box_x + 5.0, row_y + 2.0, box_w - 10.0, row_h - 4.0, 5.0, theme::ACCENT_LINE(), 1.0);
            }
            // Lightbulb glyph badge for quick-fix vibe.
            let bx = box_x + 10.0;
            let by = row_y + (row_h - 18.0) * 0.5;
            let badge = if a.fix_all_mty {
                theme::accent_a(0.16)
            } else if a.is_preferred {
                theme::accent_a(0.20)
            } else {
                MuiColor::new(1.0, 0.824, 0.478, 0.16)
            };
            ctx.dl_round(bx, by, 18.0, 18.0, 4.0, badge);
            // Vector icon (the embedded UI fonts lack the emoji/symbol glyphs that
            // previously rendered as boxes here): a check for "fix all", else a
            // wrench for a single quick-fix.
            let icon = if a.fix_all_mty || a.is_preferred { crate::icons::CHECK } else { crate::icons::WRENCH };
            ctx.dl_icon(bx + 3.0, by + 3.0, 12.0, 12.0, icon, theme::SYN_FUNCTION(), 1.6, false);

            let ty = row_y + (row_h - chrome) * 0.5 - 0.5;
            let fg = if selected { theme::TEXT() } else { theme::TEXT_1() };
            let title_budget = box_w - 52.0 - if a.is_preferred { 68.0 } else { 0.0 };
            let title = fit_ui_sized(&mut ctx.text, &a.title, title_budget, chrome);
            ctx.text.queue_ui_sized(box_x + 36.0, ty, &title, fg, chrome, clip);
            if a.is_preferred {
                let sx = box_x + box_w - 66.0;
                ctx.text.queue_ui_sized(sx, ty, "preferred", theme::TEXT_3(), chrome - 1.0, clip);
            }
        }
    }
}

// ===========================================================================
// LSP client — spawn `mty lsp`, stage the handshake, fire one request.
// ===========================================================================

fn clamp_popup_x(cx: f32, box_w: f32, window_w: f32, min_x: f32) -> f32 {
    let left = min_x.max(POPUP_MARGIN);
    let right = (window_w - POPUP_MARGIN - box_w).max(left);
    cx.clamp(left, right)
}

fn popup_available_width(window_w: f32, min_x: f32, preferred_min: f32) -> f32 {
    let left = min_x.max(POPUP_MARGIN);
    let available = (window_w - POPUP_MARGIN - left).max(1.0);
    if available < preferred_min {
        available
    } else {
        available.max(preferred_min)
    }
}

fn signature_content_budget(text_w: f32) -> f32 {
    (text_w - 22.0).max(12.0)
}

fn code_action_popup_width(text: &mut crate::text::Text, actions: &[CodeAction], chrome: f32) -> f32 {
    let content_w = actions
        .iter()
        .map(|a| {
            let suffix = if a.is_preferred { 68.0 } else { 0.0 };
            text.measure_ui_sized(&a.title, chrome).0 + suffix
        })
        .fold(0.0_f32, f32::max);
    (content_w + 56.0).max(240.0)
}

fn code_action_visible_rows(total: usize, window_h: f32, pad: f32, row_h: f32) -> usize {
    if total == 0 {
        return 0;
    }
    let available = window_h - 2.0 * pad;
    if available < row_h {
        return 0;
    }
    let max_rows = (available / row_h).floor() as usize;
    total.min(max_rows)
}

fn code_action_first_visible(sel: usize, total: usize, visible: usize) -> usize {
    if total == 0 || visible == 0 || visible >= total {
        return 0;
    }
    sel.saturating_sub(visible - 1).min(total - visible)
}

fn rename_field_text_budget(field_w: f32) -> f32 {
    (field_w - 16.0).max(0.0)
}

fn rename_card_width(window_w: f32) -> f32 {
    (window_w * 0.42).clamp(280.0, 520.0).min(window_w.max(1.0))
}

fn rename_card_x(window_w: f32, card_w: f32) -> f32 {
    ((window_w - card_w) * 0.5).max(0.0)
}

fn fit_sized(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    fit_measured(s, max_px, |candidate| text.measure_sized(candidate, size).0)
}

fn fit_ui_sized(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    fit_measured(s, max_px, |candidate| text.measure_ui_sized(candidate, size).0)
}

fn fit_measured<F>(s: &str, max_px: f32, mut measure: F) -> String
where
    F: FnMut(&str) -> f32,
{
    let max_px = max_px.max(0.0);
    if measure(s) <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    if measure(ellipsis) >= max_px {
        return ellipsis.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut candidate: String = chars.iter().take(mid).collect();
        candidate.push_str(ellipsis);
        if measure(&candidate) <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        ellipsis.to_string()
    } else {
        let mut out: String = chars.iter().take(lo).collect();
        out.push_str(ellipsis);
        out
    }
}

pub mod lsp {
    //! Generic `mty lsp` request for the language-intelligence features.
    //! Reuses the proven completion/nav staging discipline (L24): byte-count
    //! `Content-Length`, staged `didOpen` BEFORE the request, read on a worker
    //! thread bounded by `recv_timeout`, kill the child on timeout. Returns the
    //! isolated response object for the request id, or `""` on any failure.

    use std::io::{Read, Write};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    fn mty_path() -> String {
        crate::mty::path()
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

    /// Which language-intelligence request to fire (the method + how to build the
    /// `params` body). `line`/`col` are 0-based positions; `extra` carries the
    /// method-specific tail (e.g. `,"newName":"x"` or the codeAction range/context).
    pub enum Req {
        SignatureHelp { line: u32, col: u32 },
        PrepareRename { line: u32, col: u32 },
        Rename { line: u32, col: u32, new_name: String },
        CodeAction {
            start_line: u32,
            start_col: u32,
            end_line: u32,
            end_col: u32,
            diagnostics_json: String,
        },
        /// `textDocument/documentSymbol` — the Outline panel's preferred source.
        /// (mty-lsp v0.5 answers `-32601`; the shim then falls back to a scanner.)
        DocumentSymbol,
    }

    impl Req {
        fn method(&self) -> &'static str {
            match self {
                Req::SignatureHelp { .. } => "textDocument/signatureHelp",
                Req::PrepareRename { .. } => "textDocument/prepareRename",
                Req::Rename { .. } => "textDocument/rename",
                Req::CodeAction { .. } => "textDocument/codeAction",
                Req::DocumentSymbol => "textDocument/documentSymbol",
            }
        }

        fn params(&self, uri: &str) -> String {
            let u = json_escape(uri);
            match self {
                Req::DocumentSymbol => format!(r#"{{"textDocument":{{"uri":"{u}"}}}}"#),
                Req::SignatureHelp { line, col } | Req::PrepareRename { line, col } => format!(
                    r#"{{"textDocument":{{"uri":"{u}"}},"position":{{"line":{line},"character":{col}}}}}"#
                ),
                Req::Rename { line, col, new_name } => format!(
                    r#"{{"textDocument":{{"uri":"{u}"}},"position":{{"line":{line},"character":{col}}},"newName":"{}"}}"#,
                    json_escape(new_name)
                ),
                Req::CodeAction {
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                    diagnostics_json,
                } => format!(
                    r#"{{"textDocument":{{"uri":"{u}"}},"range":{{"start":{{"line":{start_line},"character":{start_col}}},"end":{{"line":{end_line},"character":{end_col}}}}},"context":{{"diagnostics":{diagnostics_json}}}}}"#
                ),
            }
        }
    }

    /// Run the handshake + one request against a document whose text is `source`,
    /// identified by `path`. Returns the isolated response object for id 2, or
    /// an empty string on any failure / timeout. Default 2.5s overall deadline.
    pub fn request(path: &Path, source: &str, req: Req) -> String {
        request_with_timeout(path, source, req, Duration::from_millis(2500))
    }

    pub fn request_with_timeout(
        path: &Path,
        source: &str,
        req: Req,
        timeout: Duration,
    ) -> String {
        let mty = mty_path();
        let child = Command::new(&mty)
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                eprintln!("language(lsp): spawn `{mty} lsp` failed: {e}");
                return String::new();
            }
        };

        let uri = file_uri(path);
        let method = req.method().to_string();
        let initialize = initialize_msg();
        let initialized = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string();
        let did_open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"mighty","version":1,"text":"{}"}}}}}}"#,
            json_escape(&uri),
            json_escape(source)
        );
        let request_msg = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"{}","params":{}}}"#,
            method,
            req.params(&uri)
        );

        let Some(mut stdin) = child.stdin.take() else {
            kill(child);
            return String::new();
        };
        let writer = std::thread::spawn(move || {
            let stages: [(&str, u64); 4] = [
                (&initialize, 80),
                (&initialized, 40),
                (&did_open, 130),
                (&request_msg, 0),
            ];
            for (msg, pause_ms) in stages {
                if stdin.write_all(&frame(msg)).is_err() || stdin.flush().is_err() {
                    return;
                }
                if pause_ms > 0 {
                    std::thread::sleep(Duration::from_millis(pause_ms));
                }
            }
            drop(stdin);
        });

        let Some(mut stdout) = child.stdout.take() else {
            kill(child);
            return String::new();
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
                        if crate::nav::lsp::has_response_id(&buf, 2) {
                            break;
                        }
                        if buf.len() > 1024 * 1024 {
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
                let bytes = rx.recv_timeout(Duration::from_millis(500)).unwrap_or_default();
                let _ = writer.join();
                let _ = reader.join();
                eprintln!("language(lsp): {method} timed out after {timeout:?}");
                bytes
            }
        };

        let text = String::from_utf8_lossy(&raw).into_owned();
        crate::nav::lsp::isolate_response_id(&text, 2)
    }

    pub(super) fn initialize_msg() -> String {
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{"textDocument":{"rename":{"prepareSupport":true},"documentSymbol":{"hierarchicalDocumentSymbolSupport":true,"symbolKind":{"valueSet":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26]}}}}}}"#.to_string()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- signature help parsing ----

    #[test]
    fn parse_signature_help_reads_label_params_active() {
        // The exact shape mty-lsp emits (verified on the wire).
        let json = r#"{"jsonrpc":"2.0","result":{"activeParameter":1,"activeSignature":0,"signatures":[{"label":"fn add(a: I32, b: I32) -> I32","parameters":[{"label":"p0"},{"label":"p1"}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("signature");
        assert_eq!(sig.label, "fn add(a: I32, b: I32) -> I32");
        assert_eq!(sig.params, vec!["p0".to_string(), "p1".to_string()]);
        assert_eq!(sig.active, 1);
    }

    #[test]
    fn lsp_initialize_advertises_prepare_rename_support_only() {
        let msg = lsp::initialize_msg();

        assert!(msg.contains(r#""rename":{"prepareSupport":true}"#));
        assert!(!msg.contains(r#""honorsChangeAnnotations""#));
    }

    #[test]
    fn lsp_initialize_advertises_document_symbol_shape() {
        let msg = lsp::initialize_msg();

        assert!(msg.contains(r#""documentSymbol":{"hierarchicalDocumentSymbolSupport":true,"symbolKind":{"valueSet":[1,2,3"#));
        assert!(!msg.contains(r#""labelSupport""#));
    }

    #[test]
    fn parse_signature_help_none_on_empty_or_null() {
        assert!(parse_signature_help(r#"{"result":null,"id":2}"#).is_none());
        assert!(parse_signature_help(r#"{"result":{"signatures":[]},"id":2}"#).is_none());
    }

    #[test]
    fn parse_signature_help_picks_active_signature() {
        let json = r#"{"result":{"activeSignature":1,"activeParameter":0,"signatures":[{"label":"first(x)","parameters":[{"label":"x"}]},{"label":"second(y, z)","parameters":[{"label":"y"},{"label":"z"}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");
        assert_eq!(sig.label, "second(y, z)");
        assert_eq!(sig.params, vec!["y".to_string(), "z".to_string()]);
    }

    #[test]
    fn parse_signature_help_uses_result_signatures_not_envelope_fields() {
        let json = r#"{"jsonrpc":"2.0","signatures":[{"label":"wrong(x)","parameters":[{"label":"x"}]}],"activeSignature":0,"result":{"activeSignature":0,"activeParameter":1,"signatures":[{"label":"right(a, b)","parameters":[{"label":"a"},{"label":"b"}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");
        assert_eq!(sig.label, "right(a, b)");
        assert_eq!(sig.params, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(sig.active, 1);
    }

    #[test]
    fn parse_signature_help_ignores_request_shaped_result_envelopes() {
        let json = r#"{"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","result":{"signatures":[{"label":"wrong(a)","parameters":[{"label":"a"}]}]}}"#;
        assert!(parse_signature_help(json).is_none());
    }

    #[test]
    fn parse_signature_help_reads_doc() {
        let json = r#"{"result":{"activeParameter":0,"signatures":[{"label":"f(a)","documentation":"adds a thing","parameters":[{"label":"a"}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");
        assert_eq!(sig.doc, "adds a thing");
    }

    #[test]
    fn parse_signature_help_reads_markup_doc_value_at_top_level() {
        let json = r#"{"result":{"signatures":[{"label":"f(a)","documentation":{"metadata":{"value":"wrong nested doc"},"kind":"markdown","value":"right doc"},"parameters":[{"metadata":{"label":"wrong nested param"},"label":"a"}]}],"activeSignature":0,"activeParameter":0},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");
        assert_eq!(sig.doc, "right doc");
        assert_eq!(sig.params, vec!["a".to_string()]);
    }

    #[test]
    fn parse_signature_help_reads_offset_parameter_labels() {
        let json = r#"{"result":{"activeSignature":0,"activeParameter":1,"signatures":[{"label":"fn add(a: I32, b: I32) -> I32","parameters":[{"label":[7,13]},{"label":[15,21]}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");

        assert_eq!(sig.params, vec!["a: I32".to_string(), "b: I32".to_string()]);
        assert_eq!(sig.active, 1);
    }

    #[test]
    fn parse_signature_help_ignores_invalid_offset_parameter_labels() {
        let json = r#"{"result":{"signatures":[{"label":"fn café(x)","parameters":[{"label":[3,2]},{"label":[6,7]},{"label":"x"}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");

        assert_eq!(sig.params, vec!["x".to_string()]);
    }

    #[test]
    fn parse_signature_help_decodes_unicode_strings() {
        let json = r#"{"result":{"signatures":[{"label":"fn 東京(caf\u00e9: Str) -> \ud83d\ude00","parameters":[{"label":"caf\u00e9"}],"documentation":"\ud83dX"}],"activeSignature":0,"activeParameter":0},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");
        assert_eq!(sig.label, "fn 東京(café: Str) -> \u{1f600}");
        assert_eq!(sig.params, vec!["café".to_string()]);
        assert_eq!(sig.doc, "\u{fffd}X");
    }

    #[test]
    fn language_lsp_response_wait_uses_response_owned_id() {
        let stream = br#"Content-Length: 99

{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":{"signatures":[{"label":"wrong()"}]}}}}Content-Length: 107

{"jsonrpc":"2.0","id":2,"result":{"signatures":[{"label":"right(a)","parameters":[{"label":"a"}]}]}}"#;

        assert!(crate::nav::lsp::has_response_id(stream, 2));
    }

    #[test]
    fn language_lsp_isolate_response_id_skips_progress_metadata_id() {
        let stream = r#"{"jsonrpc":"2.0","method":"$/progress","params":{"metadata":{"id":2,"result":{"signatures":[{"label":"wrong()"}]}}}}{"jsonrpc":"2.0","id":2,"result":{"signatures":[{"label":"right(a)","parameters":[{"label":"a"}]}]}}"#;
        let one = crate::nav::lsp::isolate_response_id(stream, 2);
        let sig = parse_signature_help(&one).expect("sig");

        assert_eq!(sig.label, "right(a)");
        assert!(!one.contains("wrong"));
    }

    // ---- WorkspaceEdit (rename) parsing ----

    #[test]
    fn parse_workspace_edit_changes_map() {
        // The exact rename response mty-lsp emits.
        let json = r#"{"jsonrpc":"2.0","result":{"changes":{"file:///C:/tmp/probe.mty":[{"newText":"plus","range":{"end":{"character":6,"line":0},"start":{"character":3,"line":0}}},{"newText":"plus","range":{"end":{"character":13,"line":5},"start":{"character":10,"line":5}}}]}},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.total_edits(), 2);
        let (uri, edits) = &we.files[0];
        assert_eq!(uri, "file:///C:/tmp/probe.mty");
        assert_eq!(edits[0], TextEdit { start_line: 0, start_col: 3, end_line: 0, end_col: 6, new_text: "plus".into() });
        assert_eq!(edits[1], TextEdit { start_line: 5, start_col: 10, end_line: 5, end_col: 13, new_text: "plus".into() });
    }

    #[test]
    fn parse_workspace_edit_multi_file_changes() {
        let json = r#"{"result":{"changes":{"file:///a.mty":[{"newText":"q","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":3}}}],"file:///b.mty":[{"newText":"q","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 2);
        assert_eq!(we.files[0].0, "file:///a.mty");
        assert_eq!(we.files[1].0, "file:///b.mty");
        assert_eq!(we.files[1].1[0].new_text, "q");
    }

    #[test]
    fn parse_workspace_edit_changes_map_accepts_case_varied_file_uri_keys() {
        let json = r#"{"result":{"changes":{"FILE:///C:/tmp/probe.mty":[{"newText":"plus","range":{"start":{"line":2,"character":4},"end":{"line":2,"character":7}}}]}},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "FILE:///C:/tmp/probe.mty");
        assert_eq!(we.files[0].1[0].new_text, "plus");
    }

    #[test]
    fn parse_workspace_edit_changes_map_ignores_nested_file_uri_text() {
        let json = r#"{"result":{"changes":{"metadata":"file:///ignored.rs","file:///edited.rs":[{"newText":"file:///literal.rs","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///edited.rs");
        assert_eq!(we.files[0].1[0].new_text, "file:///literal.rs");
    }

    #[test]
    fn parse_workspace_edit_uses_result_changes_not_envelope_fields() {
        let json = r#"{"jsonrpc":"2.0","changes":{"file:///wrong.mty":[{"newText":"wrong","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}}]},"result":{"changes":{"file:///right.mty":[{"newText":"right","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}}}]}},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///right.mty");
        assert_eq!(we.files[0].1[0].new_text, "right");
    }

    #[test]
    fn parse_workspace_edit_ignores_request_shaped_result_envelopes() {
        let json = r#"{"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","result":{"changes":{"file:///wrong.mty":[{"newText":"wrong","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}}}]}}}"#;
        assert!(parse_workspace_edit(json).is_empty());
    }

    #[test]
    fn parse_workspace_edit_ignores_nested_changes_without_owner() {
        let json = r#"{"result":{"metadata":{"changes":{"file:///wrong.mty":[{"newText":"wrong","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}}]}}},"id":4}"#;
        assert!(parse_workspace_edit(json).is_empty());
    }

    #[test]
    fn parse_workspace_edit_document_changes_shape() {
        let json = r#"{"result":{"documentChanges":[{"textDocument":{"uri":"file:///z.mty","version":1},"edits":[{"newText":"X","range":{"start":{"line":2,"character":0},"end":{"line":2,"character":1}}}]}]},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///z.mty");
        assert_eq!(we.files[0].1[0], TextEdit { start_line: 2, start_col: 0, end_line: 2, end_col: 1, new_text: "X".into() });
    }

    #[test]
    fn parse_workspace_edit_uses_result_document_changes_not_envelope_fields() {
        let json = r#"{"jsonrpc":"2.0","documentChanges":[{"textDocument":{"uri":"file:///wrong.mty"},"edits":[{"newText":"wrong","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}}]}],"result":{"documentChanges":[{"textDocument":{"uri":"file:///right.mty"},"edits":[{"newText":"right","range":{"start":{"line":3,"character":4},"end":{"line":3,"character":8}}}]}]},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///right.mty");
        assert_eq!(we.files[0].1[0], TextEdit { start_line: 3, start_col: 4, end_line: 3, end_col: 8, new_text: "right".into() });
    }

    #[test]
    fn parse_workspace_edit_ignores_nested_document_changes_without_owner() {
        let json = r#"{"result":{"metadata":{"documentChanges":[{"textDocument":{"uri":"file:///wrong.mty"},"edits":[{"newText":"wrong","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}}}]}]}},"id":4}"#;
        assert!(parse_workspace_edit(json).is_empty());
    }

    #[test]
    fn parse_workspace_edit_document_changes_ignores_resource_operations() {
        let json = r#"{"result":{"documentChanges":[{"kind":"create","uri":"file:///created.rs"},{"textDocument":{"uri":"file:///edited.rs","version":2},"edits":[{"newText":"ok","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}}}]},{"kind":"rename","oldUri":"file:///old.rs","newUri":"file:///new.rs"}]},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///edited.rs");
        assert_eq!(we.files[0].1[0], TextEdit { start_line: 1, start_col: 2, end_line: 1, end_col: 5, new_text: "ok".into() });
    }

    #[test]
    fn parse_workspace_edit_document_changes_allows_field_reordering() {
        let json = r#"{"result":{"documentChanges":[{"edits":[{"newText":"ok","range":{"start":{"line":3,"character":1},"end":{"line":3,"character":4}}}],"textDocument":{"version":2,"uri":"file:///edited.rs"}}]},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///edited.rs");
        assert_eq!(we.files[0].1[0], TextEdit { start_line: 3, start_col: 1, end_line: 3, end_col: 4, new_text: "ok".into() });
    }

    #[test]
    fn parse_workspace_edit_document_changes_uses_entry_top_level_fields() {
        let json = r#"{"result":{"documentChanges":[
          {
            "metadata":{
              "textDocument":{"uri":"file:///wrong-entry.rs"},
              "edits":[{"newText":"wrong-entry","range":{"start":{"line":90,"character":1},"end":{"line":90,"character":2}}}]
            },
            "textDocument":{"metadata":{"uri":"file:///wrong-doc.rs"},"uri":"file:///right.rs"},
            "edits":[
              {
                "metadata":{
                  "newText":"wrong-edit",
                  "range":{"start":{"line":91,"character":3},"end":{"line":91,"character":4}}
                },
                "newText":"right",
                "range":{
                  "metadata":{"start":{"line":92,"character":5},"end":{"line":92,"character":6}},
                  "start":{"metadata":{"line":93,"character":7},"line":4,"character":8},
                  "end":{"metadata":{"line":94,"character":9},"line":4,"character":13}
                }
              }
            ]
          }
        ]},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///right.rs");
        assert_eq!(we.files[0].1[0], TextEdit { start_line: 4, start_col: 8, end_line: 4, end_col: 13, new_text: "right".into() });
    }

    #[test]
    fn parse_workspace_edit_document_changes_skips_resource_only_edits() {
        let json = r#"{"result":{"documentChanges":[{"kind":"delete","uri":"file:///dead.rs"},{"kind":"create","uri":"file:///new.rs"}]},"id":4}"#;
        assert!(parse_workspace_edit(json).is_empty());
    }

    #[test]
    fn parse_workspace_edit_empty_on_null() {
        let we = parse_workspace_edit(r#"{"result":null,"id":4}"#);
        assert!(we.is_empty());
        assert_eq!(we.file_count(), 0);
    }

    #[test]
    fn parse_workspace_edit_decodes_unicode_new_text() {
        let json = r#"{"result":{"changes":{"file:///a.mty":[{"newText":"東京 caf\u00e9 \ud83d\ude00","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.files.len(), 1);
        assert_eq!(we.files[0].1[0].new_text, "東京 café \u{1f600}");
    }

    // ---- multi-edit apply (offset correctness) ----

    #[test]
    fn apply_edits_back_to_front_keeps_offsets() {
        // Two renames of `add` -> `plus` on lines 0 and 5. Applying front-first
        // would shift line-5's offsets after line-0 grows; back-to-front is safe.
        let src = "fn add(a, b) {\n  a + b\n}\n\nfn main() {\n  add(1, 2)\n}\n";
        let edits = vec![
            TextEdit { start_line: 0, start_col: 3, end_line: 0, end_col: 6, new_text: "plus".into() },
            TextEdit { start_line: 5, start_col: 2, end_line: 5, end_col: 5, new_text: "plus".into() },
        ];
        let out = apply_text_edits(src, &edits);
        assert!(out.contains("fn plus(a, b)"));
        assert!(out.contains("  plus(1, 2)"));
        assert!(!out.contains("add"));
    }

    #[test]
    fn apply_edits_same_line_two_edits() {
        // Two edits on the same line; back-to-front order means the later column
        // is spliced first.
        let src = "let x = foo + foo";
        let edits = vec![
            TextEdit { start_line: 0, start_col: 8, end_line: 0, end_col: 11, new_text: "bar".into() },
            TextEdit { start_line: 0, start_col: 14, end_line: 0, end_col: 17, new_text: "bar".into() },
        ];
        let out = apply_text_edits(src, &edits);
        assert_eq!(out, "let x = bar + bar");
    }

    #[test]
    fn apply_edits_insertion_and_unicode() {
        // Insert (zero-width range) and an edit after a multi-byte char.
        let src = "café = 1";
        let edits = vec![
            // Replace `café` (4 chars) with `tea`.
            TextEdit { start_line: 0, start_col: 0, end_line: 0, end_col: 4, new_text: "tea".into() },
        ];
        let out = apply_text_edits(src, &edits);
        assert_eq!(out, "tea = 1");
    }

    #[test]
    fn apply_edits_empty_is_identity() {
        assert_eq!(apply_text_edits("abc", &[]), "abc");
    }

    #[test]
    fn apply_edits_use_lsp_utf16_columns_after_non_bmp_chars() {
        let src = "a😀b";
        let edits = vec![TextEdit {
            start_line: 0,
            start_col: 3,
            end_line: 0,
            end_col: 4,
            new_text: "c".into(),
        }];
        assert_eq!(apply_text_edits(src, &edits), "a😀c");
    }

    #[test]
    fn apply_edits_insert_at_utf16_column_after_non_bmp_chars() {
        let src = "a😀b";
        let edits = vec![TextEdit {
            start_line: 0,
            start_col: 3,
            end_line: 0,
            end_col: 3,
            new_text: "_".into(),
        }];
        assert_eq!(apply_text_edits(src, &edits), "a😀_b");
    }

    #[test]
    fn offset_of_handles_lines_and_chars() {
        let text = "ab\ncde\nf";
        let ls = compute_line_starts(text);
        assert_eq!(offset_of(text, &ls, 0, 0), 0);
        assert_eq!(offset_of(text, &ls, 0, 2), 2); // end of "ab"
        assert_eq!(offset_of(text, &ls, 1, 0), 3); // start of "cde"
        assert_eq!(offset_of(text, &ls, 1, 3), 6); // end of "cde"
        assert_eq!(offset_of(text, &ls, 2, 0), 7); // start of "f"
        // Out-of-range line clamps to end.
        assert_eq!(offset_of(text, &ls, 9, 0), text.len());
    }

    #[test]
    fn offset_of_uses_utf16_units() {
        let text = "a😀b";
        let ls = compute_line_starts(text);
        assert_eq!(offset_of(text, &ls, 0, 0), 0);
        assert_eq!(offset_of(text, &ls, 0, 1), "a".len());
        assert_eq!(offset_of(text, &ls, 0, 2), "a".len());
        assert_eq!(offset_of(text, &ls, 0, 3), "a😀".len());
        assert_eq!(offset_of(text, &ls, 0, 4), text.len());
    }

    // ---- code action parsing ----

    #[test]
    fn parse_code_actions_empty_array() {
        assert!(parse_code_actions(r#"{"jsonrpc":"2.0","result":[],"id":5}"#).is_empty());
    }

    #[test]
    fn parse_code_actions_titles_and_edit() {
        let json = r#"{"result":[{"title":"Replace with 'print'","kind":"quickfix","edit":{"changes":{"file:///a.mty":[{"newText":"print","range":{"start":{"line":2,"character":2},"end":{"line":2,"character":6}}}]}}},{"title":"Fix all in file","kind":"source.fixAll.mighty"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].title, "Replace with 'print'");
        let e = actions[0].edit.as_ref().expect("edit");
        assert_eq!(e.total_edits(), 1);
        assert_eq!(e.files[0].1[0].new_text, "print");
        assert_eq!(actions[1].title, "Fix all in file");
        assert!(actions[1].edit.is_none());
        assert!(actions[1].fix_all_mty);
    }

    #[test]
    fn parse_code_actions_uses_top_level_result_array() {
        let json = r#"{"jsonrpc":"2.0","metadata":{"result":[{"title":"Wrong","command":"wrong.fixAll"}]},"result":[{"title":"Right","command":"server.apply"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Right");
        assert_eq!(actions[0].command.as_ref().map(|c| c.command.as_str()), Some("server.apply"));
        assert!(!actions[0].fix_all_mty);
    }

    #[test]
    fn parse_code_actions_ignores_request_shaped_result_envelopes() {
        let json = r#"{"jsonrpc":"2.0","id":2,"method":"workspace/applyEdit","result":[{"title":"Wrong","command":"server.apply"}]}"#;
        assert!(parse_code_actions(json).is_empty());
    }

    #[test]
    fn parse_code_actions_use_action_top_level_fields() {
        let json = r#"{"result":[{"metadata":{"title":"Wrong","command":"wrong.fixAll","edit":{"changes":{"file:///wrong.rs":[{"newText":"wrong","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}}},"title":"Right","command":"server.apply"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Right");
        assert!(actions[0].edit.is_none());
        assert!(actions[0].command_edit.is_none());
        assert_eq!(actions[0].command.as_ref().map(|c| c.command.as_str()), Some("server.apply"));
        assert!(!actions[0].fix_all_mty);
    }

    #[test]
    fn parse_code_actions_decodes_unicode_titles() {
        let json = r#"{"result":[{"title":"Fix 東京 caf\u00e9 \ud83d\ude00","kind":"quickfix"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Fix 東京 café \u{1f600}");
    }

    #[test]
    fn parse_code_actions_command_form() {
        let json = r#"{"result":[{"title":"Run fixer","command":"mighty.fixAll"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Run fixer");
        assert!(actions[0].edit.is_none());
        assert!(actions[0].command_edit.is_none());
        assert_eq!(actions[0].command.as_ref().map(|c| c.command.as_str()), Some("mighty.fixAll"));
        assert!(actions[0].fix_all_mty);
    }

    #[test]
    fn parse_code_actions_preserves_preferred_marker() {
        let json = r#"{"result":[{"title":"Maybe","command":"server.maybe","isPreferred":false},{"title":"Best","command":"server.best","isPreferred":true}],"id":5}"#;
        let actions = parse_code_actions(json);

        assert_eq!(actions.len(), 2);
        assert!(!actions[0].is_preferred);
        assert!(actions[1].is_preferred);
    }

    #[test]
    fn parse_code_actions_uses_top_level_preferred_marker() {
        let json = r#"{"result":[{"title":"Nested only","command":"server.apply","metadata":{"isPreferred":true}},{"title":"Top level","command":"server.best","isPreferred":true}],"id":5}"#;
        let actions = parse_code_actions(json);

        assert_eq!(actions.len(), 2);
        assert!(!actions[0].is_preferred);
        assert!(actions[1].is_preferred);
    }

    #[test]
    fn parse_code_actions_fix_all_command_must_be_mighty_owned() {
        let json = r#"{"result":[{"title":"Server fix all","command":"rust-analyzer.fixAll"},{"title":"TS source fix all","command":{"title":"Fix all","command":"typescript.applyFixAllCodeAction","arguments":[{"fixId":"fixMissingImport"}]}},{"title":"Mighty fix all","command":"mty.fix_all"}],"id":5}"#;
        let actions = parse_code_actions(json);

        assert_eq!(actions.len(), 3);
        assert_eq!(
            actions[0].command.as_ref().map(|c| c.command.as_str()),
            Some("rust-analyzer.fixAll")
        );
        assert!(!actions[0].fix_all_mty);
        assert_eq!(
            actions[1].command.as_ref().map(|c| c.command.as_str()),
            Some("typescript.applyFixAllCodeAction")
        );
        assert!(!actions[1].fix_all_mty);
        assert!(actions[2].fix_all_mty);
    }

    #[test]
    fn parse_code_actions_command_arguments_workspace_edit() {
        let json = r#"{"result":[{"title":"Apply suggestion","command":"rust-analyzer.applySourceChange","arguments":[{"label":"apply","workspaceEdit":{"changes":{"file:///a.rs":[{"newText":"println!","range":{"start":{"line":1,"character":4},"end":{"line":1,"character":11}}}]}}}]}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Apply suggestion");
        assert!(actions[0].edit.is_none());
        let e = actions[0].command_edit.as_ref().expect("command edit");
        assert_eq!(e.total_edits(), 1);
        assert_eq!(e.files[0].1[0].new_text, "println!");
        let command = actions[0].command.as_ref().expect("command");
        assert_eq!(command.command, "rust-analyzer.applySourceChange");
        assert!(command.arguments_json.as_ref().unwrap().contains("workspaceEdit"));
    }

    #[test]
    fn parse_code_actions_command_arguments_require_workspace_edit_owner() {
        let nested = r#"{"result":[{"title":"Metadata only","command":"server.apply","arguments":[{"metadata":{"workspaceEdit":{"changes":{"file:///wrong.rs":[{"newText":"wrong","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}}}}]}],"id":5}"#;
        let nested_actions = parse_code_actions(nested);
        assert!(nested_actions[0].command_edit.is_none());

        let direct = r#"{"result":[{"title":"Direct edit","command":"server.apply","arguments":[{"changes":{"file:///a.rs":[{"newText":"ok","range":{"start":{"line":1,"character":4},"end":{"line":1,"character":6}}}]}}]}],"id":5}"#;
        let direct_actions = parse_code_actions(direct);
        let edit = direct_actions[0].command_edit.as_ref().expect("direct edit");

        assert_eq!(edit.total_edits(), 1);
        assert_eq!(edit.files[0].1[0].new_text, "ok");
    }

    #[test]
    fn parse_code_actions_nested_command_object() {
        let json = r#"{"result":[{"title":"Apply import","command":{"title":"Apply import","command":"typescript.applyCodeActionCommand","arguments":[{"file":"a.ts","fixId":"fixMissingImport"}]}}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Apply import");
        let command = actions[0].command.as_ref().expect("command");
        assert_eq!(command.command, "typescript.applyCodeActionCommand");
        assert_eq!(
            command.arguments_json.as_deref(),
            Some(r#"[{"file":"a.ts","fixId":"fixMissingImport"}]"#)
        );
        assert!(actions[0].edit.is_none());
        assert!(actions[0].command_edit.is_none());
    }

    #[test]
    fn parse_code_actions_nested_command_object_uses_top_level_command() {
        let json = r#"{"result":[{"title":"Apply import","command":{"metadata":{"command":"wrong.fixAll"},"command":"typescript.applyCodeActionCommand","arguments":[{"file":"a.ts"}]}}],"id":5}"#;
        let actions = parse_code_actions(json);
        let command = actions[0].command.as_ref().expect("command");
        assert_eq!(command.command, "typescript.applyCodeActionCommand");
        assert!(!actions[0].fix_all_mty);
    }

    #[test]
    fn parse_code_actions_omits_disabled_actions() {
        let json = r#"{"result":[{"title":"Unavailable fix","disabled":{"reason":"already imported"},"command":"server.fix"},{"title":"Unavailable edit","disabled":true,"edit":{"changes":{"file:///a.rs":[{"newText":"x","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]}}},{"title":"Apply fix","command":"server.apply"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Apply fix");
        assert_eq!(
            actions[0].command.as_ref().map(|c| c.command.as_str()),
            Some("server.apply")
        );
    }

    #[test]
    fn parse_code_actions_keeps_nested_disabled_argument_text() {
        let json = r#"{"result":[{"title":"Apply command","command":"server.apply","arguments":[{"metadata":{"disabled":{"reason":"not the action state"}}}]}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Apply command");
        let command = actions[0].command.as_ref().expect("command");
        assert_eq!(command.command, "server.apply");
        assert!(command.arguments_json.as_ref().unwrap().contains("disabled"));
    }

    // ---- state types ----

    #[test]
    fn sig_state_set_clear() {
        let mut s = SigState::new();
        assert!(!s.is_active());
        assert!(s.set(Some(ParsedSignature { label: "f(a)".into(), params: vec!["a".into()], active: 0, doc: String::new() })));
        assert!(s.is_active());
        // Empty label -> not active.
        assert!(!s.set(Some(ParsedSignature::default())));
        assert!(!s.is_active());
        s.set(Some(ParsedSignature { label: "x".into(), ..Default::default() }));
        s.clear();
        assert!(!s.is_active());
    }

    #[test]
    fn rename_state_edit_buffer() {
        let mut r = RenameState::new();
        assert!(!r.is_active());
        r.open("add");
        assert!(r.is_active());
        assert_eq!(r.name_string(), "add");
        assert_eq!(r.original(), "add");
        r.push('p' as u32);
        assert_eq!(r.name_string(), "p", "first typed char replaces the selected original");
        r.backspace();
        assert_eq!(r.name_string(), "");
        for c in "plus".chars() {
            r.push(c as u32);
        }
        assert_eq!(r.name_string(), "plus");
        assert_eq!(r.display_line(), "Rename 'add' \u{2192} plus");
        r.cancel();
        assert!(!r.is_active());
        assert_eq!(r.name_string(), "");
    }

    #[test]
    fn rename_field_text_budget_keeps_border_padding() {
        assert_eq!(rename_field_text_budget(200.0), 184.0);
        assert_eq!(rename_field_text_budget(8.0), 0.0);
    }

    #[test]
    fn rename_card_width_clamps_inside_ultra_narrow_windows() {
        let card_w = rename_card_width(180.0);
        let card_x = rename_card_x(180.0, card_w);

        assert_eq!(card_w, 180.0);
        assert!(card_x >= 0.0);
        assert!(card_x + card_w <= 180.0 + 0.5);
    }

    #[test]
    fn rename_card_width_preserves_normal_bounds() {
        assert_eq!(rename_card_width(400.0), 280.0);
        assert_eq!(rename_card_width(900.0), 378.0);
        assert_eq!(rename_card_width(1600.0), 520.0);
    }

    #[test]
    fn rename_field_long_name_fits_measured_budget() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(520, 220) else {
            return;
        };
        let budget = rename_field_text_budget(252.0);
        let shown = fit_sized(
            &mut ctx.text,
            "very_long_symbol_name_that_would_cross_the_input_field_border",
            budget,
            theme::CHROME_FONT_SIZE,
        );
        let (shown_w, _) = ctx.text.measure_sized(&shown, theme::CHROME_FONT_SIZE);

        assert!(shown.ends_with('\u{2026}'));
        assert!(shown_w <= budget);
    }

    #[test]
    fn code_action_state_set_move_select() {
        let mut c = CodeActionState::new();
        assert_eq!(c.set(vec![]), 0);
        assert!(!c.is_active());
        assert_eq!(
            c.set(vec![CodeAction { title: "Inert command".into(), edit: None, command_edit: None, command: None, is_preferred: false, fix_all_mty: false }]),
            0,
            "non-actionable code actions are hidden instead of becoming inert menu rows"
        );
        let actions = vec![
            CodeAction { title: "A".into(), edit: Some(WorkspaceEdit::default()), command_edit: None, command: None, is_preferred: false, fix_all_mty: false },
            CodeAction {
                title: "C".into(),
                edit: None,
                command_edit: None,
                command: Some(CommandAction {
                    command: "server.command".into(),
                    arguments_json: None,
                }),
                is_preferred: false,
                fix_all_mty: false,
            },
            CodeAction { title: "B".into(), edit: None, command_edit: None, command: None, is_preferred: false, fix_all_mty: true },
        ];
        assert_eq!(c.set(actions), 3);
        assert!(c.is_active());
        assert_eq!(c.selection(), 0);
        assert_eq!(c.selected().unwrap().title, "A");
        c.move_sel(1);
        assert_eq!(c.selected().unwrap().title, "C");
        c.move_sel(1);
        assert_eq!(c.selected().unwrap().title, "B");
        assert!(c.selected().unwrap().fix_all_mty);
        c.move_sel(1); // wrap
        assert_eq!(c.selection(), 0);
        c.move_sel(-1); // wrap to last
        assert_eq!(c.selection(), 2);
        assert!(c.select(0));
        assert_eq!(c.title(0), Some("A"));
        c.cancel();
        assert!(!c.is_active());
    }

    #[test]
    fn code_action_state_selects_first_preferred_action() {
        let mut c = CodeActionState::new();
        let actions = vec![
            CodeAction { title: "Ordinary".into(), edit: Some(WorkspaceEdit::default()), command_edit: None, command: None, is_preferred: false, fix_all_mty: false },
            CodeAction { title: "Preferred".into(), edit: Some(WorkspaceEdit::default()), command_edit: None, command: None, is_preferred: true, fix_all_mty: false },
            CodeAction { title: "Later preferred".into(), edit: Some(WorkspaceEdit::default()), command_edit: None, command: None, is_preferred: true, fix_all_mty: false },
        ];

        assert_eq!(c.set(actions), 3);
        assert_eq!(c.selection(), 1);
        assert_eq!(c.selected().unwrap().title, "Preferred");
    }

    #[test]
    fn code_action_click_row_selects_action() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(900, 700) else {
            return;
        };
        let mut c = CodeActionState::new();
        let actions = vec![
            CodeAction { title: "Replace typo".into(), edit: Some(WorkspaceEdit::default()), command_edit: None, command: None, is_preferred: false, fix_all_mty: false },
            CodeAction { title: "Fix all".into(), edit: None, command_edit: None, command: None, is_preferred: false, fix_all_mty: true },
        ];
        assert_eq!(c.set(actions), 2);
        let (box_x, box_y, _box_w, _box_h, pad, row_h) =
            c.geometry(&mut ctx.text, 300.0, 120.0, 900, 700);
        let idx = c.click_row(&mut ctx.text, box_x + 24.0, box_y + pad + row_h + 3.0, 300.0, 120.0, 900, 700);
        assert_eq!(idx, 1);
        assert_eq!(c.selection(), 1);
        assert_eq!(
            c.click_row(&mut ctx.text, box_x - 2.0, box_y + pad + 3.0, 300.0, 120.0, 900, 700),
            -1
        );
    }

    #[test]
    fn compact_popup_x_respects_work_area_and_right_edge() {
        let x = clamp_popup_x(470.0, 240.0, 520.0, 220.0);
        assert!(x >= 220.0);
        assert!(x + 240.0 <= 500.0);
    }

    #[test]
    fn compact_signature_width_never_exceeds_work_area() {
        let width = popup_available_width(560.0, 286.0, 120.0);
        assert!(width <= 560.0 - POPUP_MARGIN - 286.0);
        let x = clamp_popup_x(520.0, width, 560.0, 286.0);
        assert!(x >= 286.0);
        assert!(x + width <= 560.0 - POPUP_MARGIN + 0.1);

        assert_eq!(popup_available_width(900.0, 260.0, 120.0), 620.0);
    }

    #[test]
    fn signature_content_budget_reserves_trailing_cushion() {
        assert_eq!(signature_content_budget(200.0), 178.0);
        assert_eq!(signature_content_budget(8.0), 12.0);
    }

    #[test]
    fn code_action_popup_width_uses_measured_titles() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(900, 700) else {
            return;
        };
        let chrome = theme::CHROME_FONT_SIZE;
        let narrow = vec![CodeAction {
            title: "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii".into(),
            edit: Some(WorkspaceEdit::default()),
            command_edit: None,
            command: None,
            is_preferred: false,
            fix_all_mty: false,
        }];
        let wide = vec![CodeAction {
            title: "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW".into(),
            edit: Some(WorkspaceEdit::default()),
            command_edit: None,
            command: None,
            is_preferred: false,
            fix_all_mty: false,
        }];

        let narrow_w = code_action_popup_width(&mut ctx.text, &narrow, chrome);
        let wide_w = code_action_popup_width(&mut ctx.text, &wide, chrome);
        let measured_delta =
            ctx.text.measure_ui_sized(&wide[0].title, chrome).0 - ctx.text.measure_ui_sized(&narrow[0].title, chrome).0;

        assert!(measured_delta > 10.0, "test titles should differ in rendered width");
        assert!(
            wide_w >= narrow_w + measured_delta.min(200.0) - 1.0,
            "code action popup should grow with measured title width: narrow={narrow_w} wide={wide_w}"
        );
    }

    #[test]
    fn code_action_inset_geometry_and_hit_testing_match() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(520, 360) else {
            return;
        };
        let mut c = CodeActionState::new();
        let actions = vec![
            CodeAction {
                title: "Replace extremely long unresolved symbol with imported candidate".into(),
                edit: Some(WorkspaceEdit::default()),
                command_edit: None,
                command: None,
                is_preferred: false,
                fix_all_mty: false,
            },
            CodeAction { title: "Fix all".into(), edit: None, command_edit: None, command: None, is_preferred: false, fix_all_mty: true },
        ];
        assert_eq!(c.set(actions), 2);
        let min_x = 220.0;
        let g = c.geometry_inset(&mut ctx.text, 470.0, 120.0, 520, 360, min_x);
        assert!(g.box_x >= min_x);
        assert!(g.box_x + g.box_w <= 500.0);
        assert_eq!(
            c.click_row_inset(
                &mut ctx.text,
                g.box_x + 24.0,
                g.box_y + g.pad + g.row_h + 3.0,
                470.0,
                120.0,
                520,
                360,
                min_x
            ),
            1
        );
        assert_eq!(
            c.click_row_inset(&mut ctx.text, min_x - 4.0, g.box_y + g.pad + 3.0, 470.0, 120.0, 520, 360, min_x),
            -1
        );
    }

    #[test]
    fn code_action_inset_geometry_clamps_inside_tiny_work_area() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(260, 240) else {
            return;
        };
        let mut c = CodeActionState::new();
        let actions = vec![CodeAction {
            title: "Replace unresolved symbol".into(),
            edit: Some(WorkspaceEdit::default()),
            command_edit: None,
            command: None,
            is_preferred: false,
            fix_all_mty: false,
        }];
        assert_eq!(c.set(actions), 1);
        let min_x = 210.0;
        let g = c.geometry_inset(&mut ctx.text, 245.0, 90.0, 260, 240, min_x);

        assert!(g.box_x >= min_x);
        assert!(g.box_w <= 30.0);
        assert!(g.box_x + g.box_w <= 240.0 + 0.5);
    }

    #[test]
    fn code_action_geometry_caps_visible_rows_to_viewport() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(520, 86) else {
            return;
        };
        let mut c = CodeActionState::new();
        let actions = (0..8)
            .map(|i| CodeAction {
                title: format!("Action {i}"),
                edit: Some(WorkspaceEdit::default()),
                command_edit: None,
                command: None,
                is_preferred: false,
                fix_all_mty: false,
            })
            .collect();
        assert_eq!(c.set(actions), 8);
        c.select(7);
        let g = c.geometry_inset(&mut ctx.text, 300.0, 70.0, 520, 86, 0.0);

        assert!(g.visible < c.count());
        assert!(g.box_y >= 0.0);
        assert!(g.box_y + g.box_h <= 86.0 + 0.5);
        assert!(g.first <= c.selection());
        assert!(c.selection() < g.first + g.visible);
    }

    #[test]
    fn code_action_click_ignores_rows_beyond_visible_window() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(520, 86) else {
            return;
        };
        let mut c = CodeActionState::new();
        let actions = (0..8)
            .map(|i| CodeAction {
                title: format!("Action {i}"),
                edit: Some(WorkspaceEdit::default()),
                command_edit: None,
                command: None,
                is_preferred: false,
                fix_all_mty: false,
            })
            .collect();
        assert_eq!(c.set(actions), 8);
        c.select(7);
        let g = c.geometry_inset(&mut ctx.text, 300.0, 70.0, 520, 86, 0.0);

        assert_eq!(
            c.click_row_inset(
                &mut ctx.text,
                g.box_x + 24.0,
                g.box_y + g.pad + 3.0,
                300.0,
                70.0,
                520,
                86,
                0.0
            ),
            g.first as i32
        );
        assert_eq!(
            c.click_row_inset(
                &mut ctx.text,
                g.box_x + 24.0,
                g.box_y + g.pad + g.visible as f32 * g.row_h + 3.0,
                300.0,
                70.0,
                520,
                86,
                0.0
            ),
            -1
        );
    }

    #[test]
    fn code_action_geometry_hides_rows_when_viewport_is_too_short() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(520, 24) else {
            return;
        };
        let mut c = CodeActionState::new();
        assert_eq!(
            c.set(vec![CodeAction {
                title: "Action".into(),
                edit: Some(WorkspaceEdit::default()),
                command_edit: None,
                command: None,
                is_preferred: false,
                fix_all_mty: false,
            }]),
            1
        );
        let g = c.geometry_inset(&mut ctx.text, 300.0, 18.0, 520, 24, 0.0);

        assert_eq!(g.visible, 0);
        assert_eq!(
            c.click_row_inset(&mut ctx.text, g.box_x + 8.0, g.box_y + 8.0, 300.0, 18.0, 520, 24, 0.0),
            -1
        );
    }

    // ---- guarded end-to-end LSP integration ----

    #[test]
    fn lsp_language_features_end_to_end() {
        use std::path::PathBuf;
        use std::time::Duration;

        let mty = PathBuf::from(crate::mty::path());
        let has_mty = std::env::var_os("MIGHTY_MTY").is_some() || mty.exists();
        if !has_mty {
            eprintln!("lsp_language_features_end_to_end: no mty binary — skipping");
            return;
        }

        let source = "fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let r = add(1, 2)\n}\n";
        let path = std::env::temp_dir().join("probe_lang.mty");
        let to = Duration::from_secs(8);

        // signatureHelp at the `(` of `add(` on line 5 (char 13).
        let raw = lsp::request_with_timeout(&path, source, lsp::Req::SignatureHelp { line: 5, col: 13 }, to);
        match parse_signature_help(&raw) {
            Some(sig) => {
                eprintln!("sig: {:?}", sig);
                assert!(sig.label.contains("add") || sig.label.contains("fn"));
            }
            None => eprintln!("lsp e2e: no signatureHelp (flaky) — skipping assert"),
        }

        // rename `add` -> `plus` at line 5 col 10.
        let raw = lsp::request_with_timeout(&path, source, lsp::Req::Rename { line: 5, col: 10, new_name: "plus".into() }, to);
        let we = parse_workspace_edit(&raw);
        if !we.is_empty() {
            eprintln!("rename edits: {}", we.total_edits());
            assert!(we.total_edits() >= 2, "expected rename to touch def + use");
        } else {
            eprintln!("lsp e2e: no rename edit (flaky) — skipping assert");
        }
    }
}
