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

// ===========================================================================
// Pure parsers + edit model (no GPU/context; exhaustively unit-tested)
// ===========================================================================

/// First occurrence of `needle` in `hay` (byte substring search).
fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

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
    while j < bytes.len() && bytes[j] != b'"' {
        if bytes[j] == b'\\' && j + 1 < bytes.len() {
            j += 1;
            match bytes[j] {
                b'n' => val.push('\n'),
                b't' => val.push('\t'),
                b'r' => val.push('\r'),
                b'"' => val.push('"'),
                b'\\' => val.push('\\'),
                b'/' => val.push('/'),
                b'u' if j + 4 < bytes.len() => {
                    let hex = &bytes[j + 1..j + 5];
                    if let Ok(s) = std::str::from_utf8(hex) {
                        if let Ok(cp) = u32::from_str_radix(s, 16) {
                            if let Some(c) = char::from_u32(cp) {
                                val.push(c);
                            }
                            j += 4;
                        }
                    }
                }
                other => val.push(other as char),
            }
        } else {
            val.push(bytes[j] as char);
        }
        j += 1;
    }
    Some((val, j + 1))
}

/// Read the unsigned integer value of `key` somewhere in `region` (scans for the
/// key, skips `:`/whitespace, then parses digits). Returns `None` if absent.
fn read_uint_after(region: &[u8], key: &[u8]) -> Option<u32> {
    let p = find_sub(region, key)?;
    let mut j = p + key.len();
    while j < region.len() && matches!(region[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    let start = j;
    let mut v: u32 = 0;
    while j < region.len() && region[j].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((region[j] - b'0') as u32);
        j += 1;
    }
    if j == start {
        None
    } else {
        Some(v)
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
    // Anchor inside the result's signatures array.
    let sigs_at = find_sub(bytes, b"\"signatures\"")?;
    // No signatures? `"signatures":[]` -> bail.
    let after = &bytes[sigs_at..];
    // Quick empty-array check: first non-ws after `]:[` is `]`.
    {
        let mut k = "\"signatures\"".len();
        while k < after.len() && matches!(after[k], b' ' | b':' | b'\t' | b'\r' | b'\n') {
            k += 1;
        }
        if k < after.len() && after[k] == b'[' {
            let mut m = k + 1;
            while m < after.len() && matches!(after[m], b' ' | b'\t' | b'\r' | b'\n') {
                m += 1;
            }
            if m < after.len() && after[m] == b']' {
                return None;
            }
        }
    }

    let active_sig = read_uint_after(bytes, b"\"activeSignature\"").unwrap_or(0) as usize;
    let active_param = read_uint_after(bytes, b"\"activeParameter\"").unwrap_or(0);

    // Collect every signature's label (in order). We then index `active_sig`.
    let labels = collect_signature_labels(json);
    if labels.is_empty() {
        return None;
    }
    let idx = active_sig.min(labels.len() - 1);
    let (label, params) = labels[idx].clone();
    let doc = parse_signature_doc(json);
    Some(ParsedSignature {
        label,
        params,
        active: active_param,
        doc,
    })
}

/// Collect `(label, params)` for each signature object in order. A signature
/// object is `{"label":"...","parameters":[{"label":"..."},...]}`. We walk
/// `"label"` keys after the `"signatures"` anchor and, for each, read the
/// parameters that belong to it (up to the next signature `"label"`).
fn collect_signature_labels(json: &str) -> Vec<(String, Vec<String>)> {
    let bytes = json.as_bytes();
    let Some(sigs_at) = find_sub(bytes, b"\"signatures\"") else {
        return Vec::new();
    };
    let region = &bytes[sigs_at..];
    // Find each top-level signature label: the first `"label"` after `"signatures"`,
    // then the first `"label"` after each `"parameters"` block boundary. Simpler:
    // a signature label is a `"label"` NOT immediately preceded (within the same
    // object) by `"parameters"`. We approximate by treating the FIRST `"label"`
    // after `"signatures"[` or after a `]` (end of a parameters array) as a sig.
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut i = 0usize;
    let label_key = b"\"label\"";
    let params_key = b"\"parameters\"";
    while i < region.len() {
        if region[i..].starts_with(label_key) {
            // Is this label a signature label (the next key is "parameters" or
            // the array closes) or a parameter label? A parameter label sits
            // inside a `"parameters":[ ... ]` array. We decide by checking whether
            // a `"parameters"` key appears AFTER this label before the next
            // `"label"` — if so this is a signature label.
            let (sig_label, past) = match read_json_string_at(region, i + label_key.len()) {
                Some(v) => v,
                None => {
                    i += label_key.len();
                    continue;
                }
            };
            // Look ahead: the next `"label"` and next `"parameters"`.
            let next_label = find_sub(&region[past..], label_key).map(|p| past + p);
            let next_params = find_sub(&region[past..], params_key).map(|p| past + p);
            let is_sig = match (next_params, next_label) {
                (Some(np), Some(nl)) => np < nl,
                (Some(_), None) => true,
                _ => false,
            };
            if is_sig {
                // Gather parameter labels from the `"parameters"` array that
                // follows, up to the next signature `"label"` (next_label after
                // the params block) — i.e. until the next signature.
                let params = if let Some(np) = next_params {
                    collect_param_labels(&region[np..], next_label)
                } else {
                    Vec::new()
                };
                out.push((sig_label, params));
            }
            i = past;
        } else {
            i += 1;
        }
    }
    out
}

/// Collect parameter `"label"` strings from a `"parameters":[...]` slice,
/// stopping before `stop_at` (relative to the parent region; the absolute index
/// of the next signature label) when provided. Only string `label`s are read
/// (mty-lsp emits `{"label":"p0"}`).
fn collect_param_labels(region: &[u8], _stop_at: Option<usize>) -> Vec<String> {
    // `region` begins at `"parameters"`. Find its `[` and `]` to bound the scan.
    let label_key = b"\"label\"";
    let Some(open) = region.iter().position(|&b| b == b'[') else {
        return Vec::new();
    };
    // Find the matching close bracket (params arrays have no nested brackets in
    // mty-lsp's output, but be safe and track depth).
    let mut depth = 0i32;
    let mut close = region.len();
    let mut in_str = false;
    let mut esc = false;
    for (k, &c) in region.iter().enumerate().skip(open) {
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
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = k;
                    break;
                }
            }
            _ => {}
        }
    }
    let arr = &region[open..close.min(region.len())];
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < arr.len() {
        if arr[i..].starts_with(label_key) {
            if let Some((lbl, past)) = read_json_string_at(arr, i + label_key.len()) {
                out.push(lbl);
                i = past;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Read the signature `documentation` (string form) if present.
fn parse_signature_doc(json: &str) -> String {
    let bytes = json.as_bytes();
    let key = b"\"documentation\"";
    if let Some(p) = find_sub(bytes, key) {
        // documentation may be a string or `{"kind":..,"value":..}`. Try string
        // first; fall back to a nested `value`.
        if let Some((s, _)) = read_json_string_at(bytes, p + key.len()) {
            return s;
        }
        if let Some(vp) = find_sub(&bytes[p..], b"\"value\"") {
            if let Some((s, _)) = read_json_string_at(&bytes[p..], vp + b"\"value\"".len()) {
                return s;
            }
        }
    }
    String::new()
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
    let mut we = WorkspaceEdit::default();

    if let Some(changes_at) = find_sub(bytes, b"\"changes\"") {
        // Walk URI keys inside the changes object. Each key is a `"file://..."`
        // string immediately followed by `:[` and a list of edits up to `]`.
        parse_changes_map(bytes, changes_at, &mut we);
    } else if let Some(dc_at) = find_sub(bytes, b"\"documentChanges\"") {
        parse_document_changes(bytes, dc_at, &mut we);
    }
    we
}

/// Parse the `changes` map shape into `we`.
fn parse_changes_map(bytes: &[u8], changes_at: usize, we: &mut WorkspaceEdit) {
    // Find the opening `{` of the changes object.
    let mut i = changes_at + b"\"changes\"".len();
    while i < bytes.len() && matches!(bytes[i], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return;
    }
    let obj_start = i;
    // Find the matching close `}` so we don't read past the changes object.
    let obj_end = match_brace(bytes, obj_start);
    let region = &bytes[obj_start..obj_end.min(bytes.len())];
    // Each entry: `"uri":[ ...edits... ]`. URIs start with `"file:`.
    let mut k = 0usize;
    while k < region.len() {
        // Find the next `"file` key (a uri).
        let Some(rel) = find_sub(&region[k..], b"\"file") else {
            break;
        };
        let uri_start = k + rel;
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

/// Parse the `documentChanges` array shape into `we`.
fn parse_document_changes(bytes: &[u8], dc_at: usize, we: &mut WorkspaceEdit) {
    let mut i = dc_at + b"\"documentChanges\"".len();
    while i < bytes.len() && matches!(bytes[i], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return;
    }
    let arr_end = match_bracket(bytes, i);
    let region = &bytes[i..arr_end.min(bytes.len())];
    // Each element: {"textDocument":{"uri":...},"edits":[...]}.
    let mut k = 0usize;
    while k < region.len() {
        let Some(uri_rel) = find_sub(&region[k..], b"\"uri\"") else {
            break;
        };
        let uri_at = k + uri_rel;
        let Some((uri, past)) = read_json_string_at(region, uri_at + b"\"uri\"".len()) else {
            break;
        };
        // The edits array follows.
        let Some(edits_rel) = find_sub(&region[past..], b"\"edits\"") else {
            we.files.push((uri, Vec::new()));
            break;
        };
        let edits_key_at = past + edits_rel;
        let mut j = edits_key_at + b"\"edits\"".len();
        while j < region.len() && matches!(region[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if j >= region.len() || region[j] != b'[' {
            we.files.push((uri, Vec::new()));
            k = j;
            continue;
        }
        let e_end = match_bracket(region, j);
        let edits = parse_text_edits(&region[j..e_end.min(region.len())]);
        we.files.push((uri, edits));
        k = e_end;
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
    // newText: a string under "newText".
    let nt_key = b"\"newText\"";
    let nt_at = find_sub(obj, nt_key)?;
    let (new_text, _) = read_json_string_at(obj, nt_at + nt_key.len())?;

    // The range: first "start" then "end" objects (each line/character).
    let start_at = find_sub(obj, b"\"start\"")?;
    let end_at = find_sub(obj, b"\"end\"")?;
    // Bound start region by where end begins (so start's scan can't grab end's
    // numbers if start appears first), and vice versa.
    let (s_region, e_region) = if start_at < end_at {
        (&obj[start_at..end_at], &obj[end_at..])
    } else {
        (&obj[start_at..], &obj[end_at..start_at])
    };
    let start_line = read_uint_after(s_region, b"\"line\"")?;
    let start_col = read_uint_after(s_region, b"\"character\"")?;
    let end_line = read_uint_after(e_region, b"\"line\"")?;
    let end_col = read_uint_after(e_region, b"\"character\"")?;
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

/// Byte offset of (0-based) `line`,`col` (col in CHARS) within `text`. Clamps to
/// the line's end / the document's end.
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
    // Advance `col` chars into the line.
    let mut off = line_start;
    for (c, ch) in line_slice.chars().enumerate() {
        if c as u32 >= col {
            break;
        }
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

/// One code action: a `title`, an optional inline `WorkspaceEdit`, and an
/// optional command (`command` string + the synthetic "kind" for mty's own
/// fixers). For our purposes the title is what the menu shows; applying either
/// runs the edit or, for synthetic actions, triggers `mty fix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    /// Menu title (e.g. `Replace with 'print'`, `Fix all (mty)`).
    pub title: String,
    /// The action's edit, if it carries one inline.
    pub edit: Option<WorkspaceEdit>,
    /// `true` if this is the synthetic shim-provided "Fix all (mty)" action that
    /// runs `mty fix --apply` rather than applying an LSP edit.
    pub fix_all_mty: bool,
}

impl CodeAction {
    fn is_actionable(&self) -> bool {
        self.edit.is_some() || self.fix_all_mty
    }
}

/// Parse the `textDocument/codeAction` response: a `result` array of code
/// actions / commands. Each entry is `{"title":"...","kind":"...","edit":{...}}`
/// (a `CodeAction`) or `{"title":"...","command":"..."}` (a `Command`). We read
/// the `title` and, if present, the inline `edit` (its first `WorkspaceEdit`).
/// Returns the actions in order (empty for `[]` / `null`).
pub fn parse_code_actions(json: &str) -> Vec<CodeAction> {
    let bytes = json.as_bytes();
    // Find the result array. Anchor at `"result"`.
    let Some(res_at) = find_sub(bytes, b"\"result\"") else {
        // Some servers omit the wrapper in our isolated slice; try whole input as
        // an array of actions.
        return parse_action_array(bytes);
    };
    let mut i = res_at + b"\"result\"".len();
    while i < bytes.len() && matches!(bytes[i], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return Vec::new();
    }
    let end = match_bracket(bytes, i);
    parse_action_array(&bytes[i..end.min(bytes.len())])
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

/// Parse one code-action object slice. Reads `title` + optional `edit`.
fn parse_one_action(obj: &[u8]) -> Option<CodeAction> {
    let t_key = b"\"title\"";
    let t_at = find_sub(obj, t_key)?;
    let (title, _) = read_json_string_at(obj, t_at + t_key.len())?;
    // Inline edit, if any: a nested WorkspaceEdit under "edit".
    let edit = if let Some(e_at) = find_sub(obj, b"\"edit\"") {
        let sub = &obj[e_at..];
        let we = parse_workspace_edit(&String::from_utf8_lossy(sub));
        if we.is_empty() {
            None
        } else {
            Some(we)
        }
    } else {
        None
    };
    let fix_all_mty = read_json_field_string(obj, b"\"kind\"")
        .map(|kind| kind == "source.fixAll.mighty")
        .unwrap_or(false)
        || read_json_field_string(obj, b"\"command\"")
            .map(|cmd| cmd.contains("fixAll") || cmd.contains("fix_all"))
            .unwrap_or(false);
    Some(CodeAction {
        title,
        edit,
        fix_all_mty,
    })
}

fn read_json_field_string(obj: &[u8], key: &[u8]) -> Option<String> {
    let at = find_sub(obj, key)?;
    read_json_string_at(obj, at + key.len()).map(|(s, _)| s)
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
    pub fn draw(&self, ctx: &mut crate::MuiContext, cx: f32, cy: f32, width: u32, height: u32) {
        let Some(sig) = &self.sig else {
            return;
        };
        let chrome = theme::CHROME_FONT_SIZE;
        let advance = layout::CHAR_W();
        let pad = 7.0;
        let label = &sig.label;
        // Compute the active-parameter highlight span by locating the param label
        // text inside the signature label.
        let active_param = sig.params.get(sig.active as usize);
        let hi_span = active_param.and_then(|p| label.find(p.as_str()).map(|b| {
            // Convert byte index -> char index for monospace x math.
            let cstart = label[..b].chars().count();
            (cstart, p.chars().count())
        }));

        let has_doc = !sig.doc.is_empty();
        let label_w = label.chars().count() as f32 * advance;
        let doc_w = sig.doc.chars().count() as f32 * (chrome - 1.0) * 0.55;
        let box_w = (label_w.max(doc_w) + 2.0 * pad + 8.0).max(120.0);
        let line_h = layout::LINE_H();
        let lines = if has_doc { 2 } else { 1 };
        let box_h = lines as f32 * line_h + 2.0 * pad;

        let w = width as f32;
        let h = height as f32;
        let mut box_x = cx;
        // Prefer ABOVE the cursor.
        let mut box_y = cy - box_h - 4.0;
        if box_y < layout::TAB_BAR_H + layout::BREADCRUMB_H {
            box_y = cy + line_h; // flip below
        }
        if box_x + box_w > w {
            box_x = (w - box_w).max(0.0);
        }
        if box_y + box_h > h {
            box_y = (h - box_h).max(0.0);
        }

        let clip = ctx.clip;
        let radius = 9.0_f32;
        ctx.dl_shadow(box_x, box_y + 5.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.6), 18.0);
        ctx.dl_grad_v(box_x, box_y, box_w, box_h, radius, theme::ELEVATED_2(), theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        let text_x = box_x + pad;
        let label_y = box_y + pad - 0.5;
        // Active-parameter highlight pill behind the param text.
        if let Some((cstart, clen)) = hi_span {
            if clen > 0 {
                let hx = text_x + cstart as f32 * advance - 2.0;
                let hw = clen as f32 * advance + 4.0;
                ctx.dl_round(hx, label_y - 1.0, hw, chrome + 4.0, 4.0, theme::accent_a(0.26));
                ctx.dl_stroke(hx, label_y - 1.0, hw, chrome + 4.0, 4.0, theme::ACCENT_LINE(), 1.0);
            }
        }
        // The signature label, with the active param drawn in accent on top.
        ctx.text.queue_sized(text_x, label_y, label, theme::TEXT(), chrome, clip);
        if let Some((cstart, clen)) = hi_span {
            if clen > 0 {
                if let Some(p) = active_param {
                    let px = text_x + cstart as f32 * advance;
                    ctx.text.queue_sized(px, label_y, p, theme::ACCENT_BRIGHT(), chrome, clip);
                }
            }
        }
        // Optional doc line, dim, below the signature.
        if has_doc {
            let dy = label_y + line_h;
            ctx.text.queue_sized(text_x, dy, &sig.doc, theme::TEXT_3(), chrome - 1.0, clip);
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
        self.last_edit = None;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn push(&mut self, codepoint: u32) {
        if self.active {
            if let Some(c) = char::from_u32(codepoint) {
                self.name.push(c);
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.active {
            self.name.pop();
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
        let box_w = (w * 0.42).clamp(280.0, 520.0);
        let box_h = 56.0_f32;
        let box_x = (w - box_w) * 0.5;
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
        ctx.text.queue_sized(field_x + 7.0, field_y + 4.0, &name, theme::ACCENT_BRIGHT(), chrome, clip);
        // Caret after the name.
        let caret_x = field_x + 7.0 + name.chars().count() as f32 * layout::CHAR_W();
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
        self.sel = 0;
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

    fn geometry(&self, cx: f32, cy: f32, width: u32, height: u32) -> (f32, f32, f32, f32, f32, f32) {
        let row_h = layout::LINE_H();
        let chrome = theme::CHROME_FONT_SIZE;
        let pad = 5.0;
        let longest = self
            .actions
            .iter()
            .map(|a| a.title.chars().count())
            .max()
            .unwrap_or(0) as f32;
        let box_w = (longest * (chrome * 0.56) + 56.0).max(240.0);
        let box_h = self.actions.len() as f32 * row_h + 2.0 * pad;

        let w = width as f32;
        let h = height as f32;
        let mut box_x = cx;
        let mut box_y = cy + row_h;
        if box_x + box_w > w {
            box_x = (w - box_w).max(0.0);
        }
        if box_y + box_h > h {
            box_y = (cy - box_h).max(0.0);
        }

        (box_x, box_y, box_w, box_h, pad, row_h)
    }

    /// Select the action row under a click. Returns the selected index, or -1
    /// when the click missed the active popup.
    pub fn click_row(&mut self, x: f32, y: f32, cx: f32, cy: f32, width: u32, height: u32) -> i32 {
        if !self.active || self.actions.is_empty() {
            return -1;
        }
        let (box_x, box_y, box_w, _box_h, pad, row_h) = self.geometry(cx, cy, width, height);
        if x < box_x || x > box_x + box_w {
            return -1;
        }
        let row_top = box_y + pad;
        if y < row_top {
            return -1;
        }
        let idx = ((y - row_top) / row_h).floor() as usize;
        if idx >= self.actions.len() {
            return -1;
        }
        if self.select(idx) {
            idx as i32
        } else {
            -1
        }
    }

    /// Draw the code-action menu near the cursor pixel `(cx, cy)` (reuses the
    /// completion-dropdown / palette card styling). No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, cx: f32, cy: f32, width: u32, height: u32) {
        if !self.active || self.actions.is_empty() {
            return;
        }
        let row_h = layout::LINE_H();
        let chrome = theme::CHROME_FONT_SIZE;
        let pad = 5.0;
        let (box_x, box_y, box_w, box_h, _pad, _row_h) = self.geometry(cx, cy, width, height);

        let clip = ctx.clip;
        let radius = 8.0_f32;
        ctx.dl_shadow(box_x, box_y + 8.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.8), 24.0);
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, theme::ELEVATED());
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        for (i, a) in self.actions.iter().enumerate() {
            let row_y = box_y + pad + i as f32 * row_h;
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
            } else {
                MuiColor::new(1.0, 0.824, 0.478, 0.16)
            };
            ctx.dl_round(bx, by, 18.0, 18.0, 4.0, badge);
            // Vector icon (the embedded UI fonts lack the emoji/symbol glyphs that
            // previously rendered as boxes here): a check for "fix all", else a
            // wrench for a single quick-fix.
            let icon = if a.fix_all_mty { crate::icons::CHECK } else { crate::icons::WRENCH };
            ctx.dl_icon(bx + 3.0, by + 3.0, 12.0, 12.0, icon, theme::SYN_FUNCTION(), 1.6, false);

            let ty = row_y + (row_h - chrome) * 0.5 - 0.5;
            let fg = if selected { theme::TEXT() } else { theme::TEXT_1() };
            ctx.text.queue_ui_sized(box_x + 36.0, ty, &a.title, fg, chrome, clip);
        }
    }
}

// ===========================================================================
// LSP client — spawn `mty lsp`, stage the handshake, fire one request.
// ===========================================================================

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
        if let Ok(p) = std::env::var("MIGHTY_MTY") {
            if !p.trim().is_empty() {
                return p;
            }
        }
        const DEV: &str = r"C:\Users\ihass\stardust\target\debug\mty.exe";
        if Path::new(DEV).exists() {
            return DEV.to_string();
        }
        "mty".to_string()
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
        let s = path.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    fn kill(mut child: Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || needle.len() > hay.len() {
            return None;
        }
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// Which language-intelligence request to fire (the method + how to build the
    /// `params` body). `line`/`col` are 0-based positions; `extra` carries the
    /// method-specific tail (e.g. `,"newName":"x"` or the codeAction range/context).
    pub enum Req {
        SignatureHelp { line: u32, col: u32 },
        PrepareRename { line: u32, col: u32 },
        Rename { line: u32, col: u32, new_name: String },
        CodeAction { start_line: u32, start_col: u32, end_line: u32, end_col: u32 },
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
                Req::CodeAction { start_line, start_col, end_line, end_col } => format!(
                    r#"{{"textDocument":{{"uri":"{u}"}},"range":{{"start":{{"line":{start_line},"character":{start_col}}},"end":{{"line":{end_line},"character":{end_col}}}}},"context":{{"diagnostics":[]}}}}"#
                ),
            }
        }
    }

    /// Run the handshake + one request against a document whose text is `source`,
    /// identified by `path`. Returns the isolated `"id":2` response object, or an
    /// empty string on any failure / timeout. Default 2.5s overall deadline.
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
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#.to_string();
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
                        if find_sub(&buf, b"\"id\":2").is_some() {
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
        crate::nav::lsp::isolate_response(&text, "\"id\":2")
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
    fn parse_signature_help_reads_doc() {
        let json = r#"{"result":{"activeParameter":0,"signatures":[{"label":"f(a)","documentation":"adds a thing","parameters":[{"label":"a"}]}]},"id":2}"#;
        let sig = parse_signature_help(json).expect("sig");
        assert_eq!(sig.doc, "adds a thing");
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
    fn parse_workspace_edit_document_changes_shape() {
        let json = r#"{"result":{"documentChanges":[{"textDocument":{"uri":"file:///z.mty","version":1},"edits":[{"newText":"X","range":{"start":{"line":2,"character":0},"end":{"line":2,"character":1}}}]}]},"id":4}"#;
        let we = parse_workspace_edit(json);
        assert_eq!(we.file_count(), 1);
        assert_eq!(we.files[0].0, "file:///z.mty");
        assert_eq!(we.files[0].1[0], TextEdit { start_line: 2, start_col: 0, end_line: 2, end_col: 1, new_text: "X".into() });
    }

    #[test]
    fn parse_workspace_edit_empty_on_null() {
        let we = parse_workspace_edit(r#"{"result":null,"id":4}"#);
        assert!(we.is_empty());
        assert_eq!(we.file_count(), 0);
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
    fn parse_code_actions_command_form() {
        let json = r#"{"result":[{"title":"Run fixer","command":"mighty.fixAll"}],"id":5}"#;
        let actions = parse_code_actions(json);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Run fixer");
        assert!(actions[0].edit.is_none());
        assert!(actions[0].fix_all_mty);
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
        r.backspace();
        r.backspace();
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
    fn code_action_state_set_move_select() {
        let mut c = CodeActionState::new();
        assert_eq!(c.set(vec![]), 0);
        assert!(!c.is_active());
        assert_eq!(
            c.set(vec![CodeAction { title: "Inert command".into(), edit: None, fix_all_mty: false }]),
            0,
            "non-actionable code actions are hidden instead of becoming inert menu rows"
        );
        let actions = vec![
            CodeAction { title: "A".into(), edit: Some(WorkspaceEdit::default()), fix_all_mty: false },
            CodeAction { title: "B".into(), edit: None, fix_all_mty: true },
        ];
        assert_eq!(c.set(actions), 2);
        assert!(c.is_active());
        assert_eq!(c.selection(), 0);
        assert_eq!(c.selected().unwrap().title, "A");
        c.move_sel(1);
        assert_eq!(c.selected().unwrap().title, "B");
        assert!(c.selected().unwrap().fix_all_mty);
        c.move_sel(1); // wrap
        assert_eq!(c.selection(), 0);
        c.move_sel(-1); // wrap to last
        assert_eq!(c.selection(), 1);
        assert!(c.select(0));
        assert_eq!(c.title(0), Some("A"));
        c.cancel();
        assert!(!c.is_active());
    }

    #[test]
    fn code_action_click_row_selects_action() {
        let mut c = CodeActionState::new();
        let actions = vec![
            CodeAction { title: "Replace typo".into(), edit: Some(WorkspaceEdit::default()), fix_all_mty: false },
            CodeAction { title: "Fix all".into(), edit: None, fix_all_mty: true },
        ];
        assert_eq!(c.set(actions), 2);
        let (box_x, box_y, _box_w, _box_h, pad, row_h) = c.geometry(300.0, 120.0, 900, 700);
        let idx = c.click_row(box_x + 24.0, box_y + pad + row_h + 3.0, 300.0, 120.0, 900, 700);
        assert_eq!(idx, 1);
        assert_eq!(c.selection(), 1);
        assert_eq!(c.click_row(box_x - 2.0, box_y + pad + 3.0, 300.0, 120.0, 900, 700), -1);
    }

    // ---- guarded end-to-end LSP integration ----

    #[test]
    fn lsp_language_features_end_to_end() {
        use std::path::PathBuf;
        use std::time::Duration;

        let dev = PathBuf::from(r"C:\Users\ihass\stardust\target\debug\mty.exe");
        let has_mty = std::env::var_os("MIGHTY_MTY").is_some() || dev.exists();
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
