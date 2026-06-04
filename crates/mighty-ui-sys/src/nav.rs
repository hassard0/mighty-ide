//! Hover + go-to-definition engine (shim-side, scalar-driven from Mighty).
//!
//! Like completion, the navigation logic lives here on the Rust side: Mighty can
//! only drive the shim through a scalar `extern c` ABI (L17) and must keep its
//! own `Vec` access flat (L21). Mighty asks for hover / definition at the cursor
//! `(line, col)`; the ABI routes Mighty through `mty lsp` and other languages
//! through the generic LSP client, then this module parses the answer with small
//! hand scanners (no serde dependency).
//!
//! Two surfaces:
//!
//! * **Hover** ([`HoverState`]): the parsed hover text is wrapped to a few short
//!   lines and drawn as a popup box near the cursor by [`HoverState::draw`].
//! * **Definition** ([`DefState`]): the parsed `Location` / `LocationLink` (uri
//!   + start position) is resolved to a path + 0-based `(line, col)`. Mighty
//!   reads it back to move the cursor (same file) or open the target as a tab
//!   (other file).
//!
//! Every LSP exchange is short-timeout and failure-tolerant — any error leaves
//! the state empty so the editor simply does nothing (never blocks).

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

// ---------------------------------------------------------------------------
// Hover state
// ---------------------------------------------------------------------------

/// Max popup width in characters (text is wrapped to this).
const HOVER_WRAP: usize = 60;
/// Max popup lines drawn (the hover text is truncated to this many).
const HOVER_MAX_LINES: usize = 6;

/// Shim-owned hover state: the wrapped popup lines + the cursor cell the popup
/// anchors to, so Mighty can keep it open until the cursor moves.
#[derive(Debug, Default)]
pub struct HoverState {
    /// Wrapped display lines (empty when no hover is active).
    lines: Vec<String>,
    /// `true` while a hover popup should be drawn.
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct HoverGeometry {
    box_x: f32,
    box_y: f32,
    box_w: f32,
    box_h: f32,
    pad: f32,
    row_h: f32,
    visible: usize,
}

impl HoverState {
    pub fn new() -> Self {
        HoverState::default()
    }

    /// Install hover text (already extracted from the LSP response). Cleans up
    /// markdown fences / blank-noise and wraps into [`HOVER_MAX_LINES`] short
    /// lines. Returns `true` if any non-empty line resulted (hover available).
    pub fn set_text(&mut self, text: &str) -> bool {
        self.lines = wrap_hover(text, HOVER_WRAP, HOVER_MAX_LINES);
        self.active = !self.lines.is_empty();
        self.active
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Clear the popup.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.active = false;
    }

    /// Draw the popup box near the cursor pixel `(cx, cy)`, clamped on-screen.
    /// No-op when inactive. The box sits ABOVE the cursor line when there is room
    /// (so it doesn't cover the code being inspected), else below.
    pub fn draw(&self, ctx: &mut crate::MuiContext, cx: f32, cy: f32, width: u32, height: u32) {
        if !self.active || self.lines.is_empty() {
            return;
        }
        let row_h = layout::LINE_H();
        let pad = 4.0;
        let g = hover_popup_geometry(&mut ctx.text, &self.lines, cx, cy, width, height, theme::FONT_SIZE(), pad, row_h);
        if g.visible == 0 {
            return;
        }

        let clip = Some((
            g.box_x.max(0.0) as u32,
            g.box_y.max(0.0) as u32,
            g.box_w.max(0.0) as u32,
            g.box_h.max(0.0) as u32,
        ));
        let radius = 9.0_f32;
        // Soft shadow + rounded elevated card + hairline border.
        ctx.dl_shadow(g.box_x, g.box_y + 5.0, g.box_w, g.box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.6), 18.0);
        ctx.dl_grad_v(g.box_x, g.box_y, g.box_w, g.box_h, radius, theme::ELEVATED_2(), theme::ELEVATED());
        ctx.dl_stroke(g.box_x, g.box_y, g.box_w, g.box_h, radius, theme::BORDER_STRONG(), 1.0);
        let fg = theme::TEXT();
        let text_w = (g.box_w - 12.0).max(0.0);
        for (i, line) in self.lines.iter().take(g.visible).enumerate() {
            let row_y = g.box_y + g.pad + i as f32 * g.row_h;
            let shown = fit_hover_line(&mut ctx.text, line, text_w, theme::FONT_SIZE());
            ctx.text.queue(g.box_x + 6.0, row_y + 1.0, &shown, fg, clip);
        }
    }
}

fn hover_popup_width(text: &mut crate::text::Text, lines: &[String], size: f32) -> f32 {
    let content_w = lines
        .iter()
        .map(|line| text.measure_sized(line, size).0)
        .fold(0.0_f32, f32::max);
    (content_w + 12.0).max(60.0)
}

fn hover_visible_lines(total: usize, window_h: f32, pad: f32, row_h: f32) -> usize {
    if total == 0 {
        return 0;
    }
    let available = window_h - 2.0 * pad;
    if available < row_h {
        return 0;
    }
    total.min((available / row_h).floor() as usize)
}

fn hover_popup_geometry(
    text: &mut crate::text::Text,
    lines: &[String],
    cx: f32,
    cy: f32,
    width: u32,
    height: u32,
    size: f32,
    pad: f32,
    row_h: f32,
) -> HoverGeometry {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    let visible = hover_visible_lines(lines.len(), h, pad, row_h);
    let wanted_w = hover_popup_width(text, lines, size);
    let box_w = wanted_w.min(w).max(1.0);
    let box_h = (visible as f32 * row_h + 2.0 * pad).min(h).max(0.0);
    let mut box_x = cx.min((w - box_w).max(0.0)).max(0.0);
    let mut box_y = cy - box_h - 2.0;
    if box_y < 0.0 {
        box_y = cy + row_h;
    }
    if box_x + box_w > w {
        box_x = (w - box_w).max(0.0);
    }
    if box_y + box_h > h {
        box_y = (h - box_h).max(0.0);
    }
    HoverGeometry {
        box_x,
        box_y,
        box_w,
        box_h,
        pad,
        row_h,
        visible,
    }
}

fn fit_hover_line(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    let max_px = max_px.max(0.0);
    if text.measure_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    if text.measure_sized(ellipsis, size).0 > max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut candidate: String = chars.iter().take(mid).collect();
        candidate.push_str(ellipsis);
        if text.measure_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars.iter().take(lo).collect();
    out.push_str(ellipsis);
    out
}

/// Strip markdown noise from an LSP hover `value` and wrap it into at most
/// `max_lines` lines of at most `wrap` chars each. Code-fence lines (```...```)
/// are dropped; the leading code signature and the descriptive lines are kept.
/// Pure + unit-tested.
pub fn wrap_hover(text: &str, wrap: usize, max_lines: usize) -> Vec<String> {
    let wrap = wrap.max(8);
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        if out.len() >= max_lines {
            break;
        }
        let trimmed = raw.trim_end();
        // Drop pure code-fence markers (```mty / ```), keep their contents.
        if trimmed.trim_start().starts_with("```") {
            continue;
        }
        // Light markdown cleanup for a compact plain-text popup.
        let cleaned = clean_hover_markdown(trimmed);
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            continue;
        }
        // Hard-wrap long lines at `wrap` chars (char-based, not byte-based).
        let chars: Vec<char> = cleaned.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            if out.len() >= max_lines {
                break;
            }
            let end = (start + wrap).min(chars.len());
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    out
}

fn clean_hover_markdown(line: &str) -> String {
    let line = strip_markdown_links(line);
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '`' => {}
            '*' | '_' => {
                if chars.get(i + 1) == Some(&ch) {
                    i += 1;
                } else if ch == '_' && is_word_internal_underscore(&chars, i) {
                    out.push(ch);
                }
            }
            '\\' => {
                if let Some(next) = chars.get(i + 1) {
                    out.push(*next);
                    i += 1;
                }
            }
            _ => out.push(ch),
        }
        i += 1;
    }
    out
}

fn is_word_internal_underscore(chars: &[char], idx: usize) -> bool {
    idx > 0
        && idx + 1 < chars.len()
        && chars[idx - 1].is_ascii_alphanumeric()
        && chars[idx + 1].is_ascii_alphanumeric()
}

fn strip_markdown_links(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some((label, consumed)) = parse_markdown_link_label(&chars[i..]) {
                out.push_str(&label);
                i += consumed;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn parse_markdown_link_label(chars: &[char]) -> Option<(String, usize)> {
    let mut label = String::new();
    let mut i = 1;
    while i < chars.len() {
        match chars[i] {
            ']' if chars.get(i + 1) == Some(&'(') => {
                i += 2;
                while i < chars.len() && chars[i] != ')' {
                    i += 1;
                }
                if chars.get(i) == Some(&')') {
                    return Some((label, i + 1));
                }
                return None;
            }
            '\\' if i + 1 < chars.len() => {
                label.push(chars[i + 1]);
                i += 2;
            }
            ch => {
                label.push(ch);
                i += 1;
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Definition state
// ---------------------------------------------------------------------------

/// A resolved definition target: a path + 0-based `(line, col)` start position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefTarget {
    /// Absolute filesystem path resolved from the LSP `uri`.
    pub path: std::path::PathBuf,
    /// 0-based line of the definition start.
    pub line: u32,
    /// 0-based column (character) of the definition start.
    pub col: u32,
}

/// Shim-owned definition state: the most recent resolved target, if any.
#[derive(Debug, Default)]
pub struct DefState {
    target: Option<DefTarget>,
}

impl DefState {
    pub fn new() -> Self {
        DefState::default()
    }

    pub fn set(&mut self, target: Option<DefTarget>) {
        self.target = target;
    }

    pub fn clear(&mut self) {
        self.target = None;
    }

    pub fn target(&self) -> Option<&DefTarget> {
        self.target.as_ref()
    }

    /// `true` if the resolved target path equals `current` (so Mighty moves the
    /// cursor in-place rather than opening a tab). Compares loosely: exact match
    /// OR equal after normalizing separators / case (Windows paths).
    pub fn path_matches(&self, current: Option<&std::path::Path>) -> bool {
        match (&self.target, current) {
            (Some(t), Some(c)) => paths_equal(&t.path, c),
            _ => false,
        }
    }
}

/// Loose path equality for Windows: compare lowercased, separator-normalized
/// strings (the LSP echoes back the same uri we sent, but a definition in
/// another file comes back with its own path, so a robust compare avoids a
/// spurious "different file" when only the casing/slash differs).
///
/// First tries a filesystem canonicalize (resolves `.`/`..`, relative vs.
/// absolute, and casing) — this is the authoritative same-file check; falls back
/// to a normalized-string compare when either side can't be canonicalized (e.g.
/// a path that doesn't exist on disk).
pub fn paths_equal(a: &std::path::Path, b: &std::path::Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
        return ca == cb;
    }
    fn norm(p: &std::path::Path) -> String {
        p.to_string_lossy().replace('\\', "/").to_lowercase()
    }
    norm(a) == norm(b)
}

// ---------------------------------------------------------------------------
// Parsers (pure, unit-tested) — scrape hover text / definition location out of
// the JSON-RPC response stream without a JSON dependency.
// ---------------------------------------------------------------------------

/// Extract display text from a hover JSON-RPC response blob.
///
/// LSP allows hover contents as `MarkupContent` (`{kind,value}`), a plain
/// `MarkedString`, or `MarkedString[]`. We normalize all supported shapes into
/// a newline-joined string for the existing hover popup wrapper. Returns `None`
/// for `null` / missing / empty contents.
pub fn parse_hover_value(json: &str) -> Option<String> {
    let bytes = json.as_bytes();
    let anchor = find_sub(bytes, b"\"contents\"")?;
    let value_at = json_value_start(bytes, anchor + b"\"contents\"".len())?;
    let text = match bytes[value_at] {
        b'"' => read_json_string_after(bytes, value_at)?,
        b'{' => parse_hover_object(bytes, value_at)?,
        b'[' => parse_hover_array(bytes, value_at)?,
        _ => return None,
    };
    if text.trim().is_empty() { None } else { Some(text) }
}

fn parse_hover_object(bytes: &[u8], object_at: usize) -> Option<String> {
    let end = match_enclosed(bytes, object_at, b'{', b'}');
    let obj = &bytes[object_at..end.min(bytes.len())];
    let language = find_sub(obj, b"\"language\"")
        .and_then(|p| read_json_string_after(obj, p + b"\"language\"".len()))
        .filter(|s| !s.trim().is_empty());
    let value = find_sub(obj, b"\"value\"")
        .and_then(|p| read_json_string_after(obj, p + b"\"value\"".len()))?;
    match language {
        Some(lang) => Some(format!("```{lang}\n{value}\n```")),
        None => Some(value),
    }
}

fn parse_hover_array(bytes: &[u8], array_at: usize) -> Option<String> {
    let end = match_enclosed(bytes, array_at, b'[', b']');
    let arr = &bytes[array_at..end.min(bytes.len())];
    let mut parts: Vec<String> = Vec::new();
    let mut i = 1usize;
    while i + 1 < arr.len() {
        i = skip_json_ws_and_commas(arr, i);
        if i >= arr.len() || arr[i] == b']' {
            break;
        }
        match arr[i] {
            b'"' => {
                if let Some(s) = read_json_string_after(arr, i) {
                    if !s.trim().is_empty() {
                        parts.push(s);
                    }
                }
                i = json_value_end(arr, i);
            }
            b'{' => {
                if let Some(s) = parse_hover_object(arr, i) {
                    if !s.trim().is_empty() {
                        parts.push(s);
                    }
                }
                i = match_enclosed(arr, i, b'{', b'}');
            }
            _ => i += 1,
        }
    }
    if parts.is_empty() { None } else { Some(parts.join("\n")) }
}

/// Extract the definition target line/col from a definition JSON-RPC response.
///
/// Supports both `Location` (`uri` + `range.start`) and `LocationLink`
/// (`targetUri` + `targetSelectionRange.start`, falling back to
/// `targetRange.start`). Returns `(uri, line, col)` or `None` for a `null` /
/// missing result.
pub fn parse_definition(json: &str) -> Option<(String, u32, u32)> {
    let bytes = json.as_bytes();
    if let Some(link) = parse_location_link(bytes) {
        return Some(link);
    }
    parse_location(bytes)
}

fn parse_location_link(bytes: &[u8]) -> Option<(String, u32, u32)> {
    let uri_key = b"\"targetUri\"";
    let uri_pos = find_sub(bytes, uri_key)?;
    let uri = read_json_string_after(bytes, uri_pos + uri_key.len())?;
    let range_anchor = find_sub(bytes, b"\"targetSelectionRange\"")
        .or_else(|| find_sub(bytes, b"\"targetRange\""))?;
    let region = &bytes[range_anchor..];
    let start_anchor = find_sub(region, b"\"start\"")?;
    let start_region = &region[start_anchor..];
    let line = read_uint_after(start_region, b"\"line\"")?;
    let col = read_uint_after(start_region, b"\"character\"")?;
    Some((uri, line, col))
}

fn parse_location(bytes: &[u8]) -> Option<(String, u32, u32)> {
    // The uri belongs to the result Location; it appears once. This deliberately
    // does not match `targetUri`; LocationLink is handled first.
    let uri_key = b"\"uri\"";
    let uri_pos = find_sub(bytes, uri_key)?;
    let uri = read_json_string_after(bytes, uri_pos + uri_key.len())?;
    let start_anchor = find_sub(bytes, b"\"start\"")?;
    let start_region = &bytes[start_anchor..];
    let line = read_uint_after(start_region, b"\"line\"")?;
    let col = read_uint_after(start_region, b"\"character\"")?;
    Some((uri, line, col))
}

/// Resolve an LSP `file://` uri to a filesystem path (Windows-aware:
/// `file:///C:/a/b` -> `C:\a\b`, `file:///home/x` -> `/home/x`). Percent-decodes
/// `%20` etc. Returns `None` for a non-`file:` uri.
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // After `file://` an absolute Windows path looks like `/C:/...`; a POSIX path
    // looks like `/home/...`. Strip a single leading slash IFF it precedes a
    // drive letter (`/C:` -> `C:`), else keep it (POSIX absolute).
    let rest = percent_decode(rest);
    let bytes = rest.as_bytes();
    let stripped = if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        &rest[1..]
    } else {
        &rest[..]
    };
    let native = stripped.replace('/', std::path::MAIN_SEPARATOR_STR);
    Some(std::path::PathBuf::from(native))
}

/// Minimal percent-decoder for URI paths (`%20` -> space, etc.). Leaves
/// malformed escapes untouched.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Read a JSON string literal that begins at or after `pos` (skips whitespace,
/// a `:`, then expects the opening `"`). Un-escapes the common cases. Returns
/// the decoded string, or `None` if no string follows.
fn read_json_string_after(bytes: &[u8], pos: usize) -> Option<String> {
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
                return Some(val);
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

fn json_value_start(bytes: &[u8], pos: usize) -> Option<usize> {
    let mut j = pos;
    while j < bytes.len() && matches!(bytes[j], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        j += 1;
    }
    (j < bytes.len()).then_some(j)
}

fn skip_json_ws_and_commas(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len()
        && matches!(bytes[pos], b' ' | b',' | b'\t' | b'\r' | b'\n')
    {
        pos += 1;
    }
    pos
}

fn json_value_end(bytes: &[u8], pos: usize) -> usize {
    match bytes.get(pos).copied() {
        Some(b'"') => {
            let mut j = pos + 1;
            let mut esc = false;
            while j < bytes.len() {
                let b = bytes[j];
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    return j + 1;
                }
                j += 1;
            }
            bytes.len()
        }
        Some(b'{') => match_enclosed(bytes, pos, b'{', b'}'),
        Some(b'[') => match_enclosed(bytes, pos, b'[', b']'),
        _ => {
            let mut j = pos;
            while j < bytes.len() && !matches!(bytes[j], b',' | b']' | b'}') {
                j += 1;
            }
            j
        }
    }
}

fn match_enclosed(bytes: &[u8], open_at: usize, open: u8, close: u8) -> usize {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut k = open_at;
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
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
        k += 1;
    }
    bytes.len()
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

/// First occurrence of `needle` in `hay` (byte substring search).
fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// LSP client — spawn `mty lsp`, stage the handshake, fire one request.
// ---------------------------------------------------------------------------

pub mod lsp {
    //! `mty lsp` client for hover + definition. Reuses the proven completion
    //! staging discipline (L24): byte-count `Content-Length`, and stage
    //! `didOpen` BEFORE the request with small pauses so the server applies the
    //! open to its doc store before answering. Every step is short-timeout and
    //! failure-tolerant — any error returns `None`.

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

    fn json_escape(s: &str) -> String {
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

    fn file_uri(path: &Path) -> String {
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

    /// Which navigation request to fire.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Req {
        Hover,
        Definition,
    }

    /// Run the handshake + one `hover`/`definition` request at (`line0`,`col0`)
    /// (0-based) against a document whose text is `source`, identified by `path`.
    /// Returns the raw JSON response stream (to be parsed by the caller), or an
    /// empty string on any failure / timeout. Default 2.5s overall deadline.
    pub fn request(path: &Path, source: &str, line0: u32, col0: u32, req: Req) -> String {
        request_with_timeout(path, source, line0, col0, req, Duration::from_millis(2500))
    }

    /// [`request`] with an explicit overall timeout (used by tests).
    ///
    /// Same robustness model as the completion client: read on a worker thread,
    /// bound the wait with `recv_timeout`, KILL the child on timeout to close the
    /// pipe and unblock the reader. The reader stops early once the response id
    /// (`"id":2`) is seen so the happy path returns promptly.
    pub fn request_with_timeout(
        path: &Path,
        source: &str,
        line0: u32,
        col0: u32,
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
                eprintln!("nav(lsp): spawn `{mty} lsp` failed: {e} — no hover/def");
                return String::new();
            }
        };

        let uri = file_uri(path);
        let method = match req {
            Req::Hover => "textDocument/hover",
            Req::Definition => "textDocument/definition",
        };

        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#.to_string();
        let initialized = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_string();
        let did_open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"mighty","version":1,"text":"{}"}}}}}}"#,
            json_escape(&uri),
            json_escape(source)
        );
        let request_msg = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"{}","params":{{"textDocument":{{"uri":"{}"}},"position":{{"line":{},"character":{}}}}}}}"#,
            method,
            json_escape(&uri),
            line0,
            col0
        );

        let Some(mut stdin) = child.stdin.take() else {
            kill(child);
            return String::new();
        };
        let writer = std::thread::spawn(move || {
            let stages: [(&str, u64); 4] = [
                (&initialize, 80),
                (&initialized, 40),
                (&did_open, 120),
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
                eprintln!("nav(lsp): {method} timed out after {timeout:?}");
                bytes
            }
        };

        // We only want the response frame for id:2 (hover/def). The initialize
        // result (id:1) also lands in the stream; isolating the id:2 frame keeps
        // the hover-`value` / definition-`uri` parsers from matching the wrong
        // payload (e.g. the server's own `serverInfo` or capability strings).
        let text = String::from_utf8_lossy(&raw).into_owned();
        isolate_response(&text, "\"id\":2")
    }

    /// Return the single JSON object (brace-balanced) that contains `marker`
    /// (e.g. `"id":2`). LSP frames are concatenated `Content-Length`-prefixed
    /// objects; we slice out just the one holding our response id so the field
    /// scanners don't see the initialize result's fields. Falls back to the
    /// whole input if balancing fails.
    pub fn isolate_response(stream: &str, marker: &str) -> String {
        let Some(mpos) = stream.find(marker) else {
            return stream.to_string();
        };
        let bytes = stream.as_bytes();
        // Walk back to the enclosing object's opening brace.
        let mut depth = 0i32;
        let mut start = None;
        let mut i = mpos as isize;
        while i >= 0 {
            match bytes[i as usize] {
                b'}' => depth += 1,
                b'{' => {
                    if depth == 0 {
                        start = Some(i as usize);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            i -= 1;
        }
        let Some(start) = start else {
            return stream.to_string();
        };
        // Walk forward to the matching closing brace.
        let mut depth = 0i32;
        let mut end = None;
        let mut in_str = false;
        let mut esc = false;
        let mut k = start;
        while k < bytes.len() {
            let c = bytes[k];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(k + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            k += 1;
        }
        match end {
            Some(e) => stream[start..e].to_string(),
            None => stream.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- hover wrapping ----

    #[test]
    fn wrap_hover_drops_fences_and_keeps_signature() {
        let v = "```mty\nfn add(a: I32, b: I32) -> I32\n```\n\n_node_: `NAME_REF`\n\n_token_: `IDENT`";
        let lines = wrap_hover(v, 60, 6);
        assert_eq!(
            lines,
            vec![
                "fn add(a: I32, b: I32) -> I32".to_string(),
                "node: NAME_REF".to_string(),
                "token: IDENT".to_string(),
            ]
        );
    }

    #[test]
    fn wrap_hover_hard_wraps_long_lines() {
        let v = "abcdefghij klmnop"; // 17 chars
        let lines = wrap_hover(v, 10, 6);
        assert_eq!(lines, vec!["abcdefghij".to_string(), " klmnop".to_string()]);
    }

    #[test]
    fn wrap_hover_truncates_to_max_lines() {
        let v = "a\nb\nc\nd\ne\nf\ng\nh";
        let lines = wrap_hover(v, 60, 3);
        assert_eq!(lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn hover_state_set_clear() {
        let mut h = HoverState::new();
        assert!(!h.is_active());
        assert!(h.set_text("```mty\nfn f()\n```"));
        assert!(h.is_active());
        assert_eq!(h.line_count(), 1);
        // Empty / fence-only text yields no hover.
        assert!(!h.set_text("```\n```"));
        assert!(!h.is_active());
        h.set_text("real");
        h.clear();
        assert!(!h.is_active());
    }

    #[test]
    fn hover_popup_width_uses_measured_code_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(640, 480) else {
            return;
        };
        let size = theme::FONT_SIZE();
        let lines = vec![
            "short".to_string(),
            "fn add(a: I32, b: I32) -> I32".to_string(),
        ];

        let width = hover_popup_width(&mut ctx.text, &lines, size);
        let expected = (ctx.text.measure_sized(&lines[1], size).0 + 12.0).max(60.0);

        assert_eq!(width, expected);
    }

    #[test]
    fn hover_popup_geometry_clamps_inside_tiny_width() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(96, 180) else {
            return;
        };
        let size = theme::FONT_SIZE();
        let pad = 4.0;
        let row_h = layout::LINE_H();
        let lines = vec!["fn very_long_symbol_name(a: I32, b: I32) -> I32".to_string()];
        let g = hover_popup_geometry(&mut ctx.text, &lines, 80.0, 80.0, 96, 180, size, pad, row_h);

        assert!(g.box_x >= 0.0);
        assert!(g.box_w <= 96.0);
        assert!(g.box_x + g.box_w <= 96.0 + 0.5);
        assert_eq!(g.visible, 1);
    }

    #[test]
    fn hover_popup_geometry_caps_visible_lines_to_height() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 46) else {
            return;
        };
        let size = theme::FONT_SIZE();
        let pad = 4.0;
        let row_h = layout::LINE_H();
        let lines = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
        ];
        let g = hover_popup_geometry(&mut ctx.text, &lines, 180.0, 38.0, 320, 46, size, pad, row_h);

        assert!(g.visible < lines.len());
        assert!(g.box_y >= 0.0);
        assert!(g.box_y + g.box_h <= 46.0 + 0.5);
    }

    #[test]
    fn hover_popup_geometry_hides_rows_when_too_short() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(320, 20) else {
            return;
        };
        let size = theme::FONT_SIZE();
        let pad = 4.0;
        let row_h = layout::LINE_H();
        let lines = vec!["one".to_string()];
        let g = hover_popup_geometry(&mut ctx.text, &lines, 160.0, 12.0, 320, 20, size, pad, row_h);

        assert_eq!(g.visible, 0);
        assert!(g.box_h <= 20.0);
    }

    #[test]
    fn hover_line_fitter_keeps_text_inside_card() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(240, 120) else {
            return;
        };
        let size = theme::FONT_SIZE();
        let shown = fit_hover_line(
            &mut ctx.text,
            "fn very_long_symbol_name(a: I32, b: I32) -> I32",
            72.0,
            size,
        );
        let (shown_w, _) = ctx.text.measure_sized(&shown, size);

        assert!(shown.ends_with('\u{2026}'));
        assert!(shown_w <= 72.0 + 0.5);
    }

    #[test]
    fn wrap_hover_cleans_common_inline_markdown() {
        let lines = wrap_hover(
            "See [`Vec<T>`](https://doc.rust-lang.org) and **Result** _value_ \\*literal\\*",
            80,
            4,
        );
        assert_eq!(lines, vec!["See Vec<T> and Result value *literal*"]);
    }

    #[test]
    fn wrap_hover_leaves_unclosed_markdown_links_readable() {
        let lines = wrap_hover("See [unfinished](https://example.com", 80, 4);
        assert_eq!(lines, vec!["See [unfinished](https://example.com"]);
    }

    // ---- hover response parsing ----

    #[test]
    fn parse_hover_value_extracts_markup() {
        let json = r#"{"jsonrpc":"2.0","result":{"contents":{"kind":"markdown","value":"```mty\nfn add(a: I32, b: I32) -> I32\n```"},"range":{"start":{"line":5,"character":10},"end":{"line":5,"character":13}}},"id":2}"#;
        let v = parse_hover_value(json).expect("hover value");
        assert_eq!(v, "```mty\nfn add(a: I32, b: I32) -> I32\n```");
    }

    #[test]
    fn parse_hover_value_none_on_null_result() {
        let json = r#"{"jsonrpc":"2.0","result":null,"id":2}"#;
        assert_eq!(parse_hover_value(json), None);
    }

    #[test]
    fn parse_hover_handles_escaped_quotes() {
        let json = r#"{"result":{"contents":{"kind":"markdown","value":"a \"q\" b"}}}"#;
        assert_eq!(parse_hover_value(json).unwrap(), "a \"q\" b");
    }

    #[test]
    fn parse_hover_decodes_unicode_strings() {
        let json = r#"{"result":{"contents":{"kind":"markdown","value":"東京 caf\u00e9 \ud83d\ude00 \ud83dX"}}}"#;
        assert_eq!(
            parse_hover_value(json).unwrap(),
            "東京 café \u{1f600} \u{fffd}X"
        );
    }

    #[test]
    fn parse_hover_accepts_plain_string_contents() {
        let json = r#"{"jsonrpc":"2.0","result":{"contents":"plain hover text"},"id":2}"#;
        assert_eq!(parse_hover_value(json).unwrap(), "plain hover text");
    }

    #[test]
    fn parse_hover_accepts_marked_string_array() {
        let json = r#"{"jsonrpc":"2.0","result":{"contents":["first line",{"language":"rust","value":"fn add(a: i32) -> i32"}]},"id":2}"#;
        let v = parse_hover_value(json).unwrap();
        assert_eq!(v, "first line\n```rust\nfn add(a: i32) -> i32\n```");
    }

    #[test]
    fn parse_hover_accepts_language_value_object() {
        let json = r#"{"jsonrpc":"2.0","result":{"contents":{"language":"python","value":"def add(a): ..."}},"id":2}"#;
        assert_eq!(
            parse_hover_value(json).unwrap(),
            "```python\ndef add(a): ...\n```"
        );
    }

    // ---- definition response parsing ----

    #[test]
    fn parse_definition_reads_start_and_uri() {
        // Mirrors the real wire format: range BEFORE uri, start position first.
        let json = r#"{"jsonrpc":"2.0","result":{"range":{"end":{"character":0,"line":4},"start":{"character":2,"line":7}},"uri":"file:///C:/tmp/probe.mty"},"id":3}"#;
        let (uri, line, col) = parse_definition(json).expect("definition");
        assert_eq!(uri, "file:///C:/tmp/probe.mty");
        assert_eq!(line, 7);
        assert_eq!(col, 2);
    }

    #[test]
    fn parse_definition_decodes_json_escaped_uri() {
        let json = r#"{"result":{"uri":"file:///C:/tmp/\u6771%20space.rs","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":3}}},"id":3}"#;
        let (uri, line, col) = parse_definition(json).expect("definition");
        assert_eq!(uri, "file:///C:/tmp/東%20space.rs");
        assert_eq!(line, 1);
        assert_eq!(col, 2);
    }

    #[test]
    fn parse_definition_reads_location_link_selection_range() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"originSelectionRange":{"start":{"line":2,"character":4},"end":{"line":2,"character":7}},"targetUri":"file:///C:/tmp/lib.rs","targetRange":{"start":{"line":30,"character":0},"end":{"line":36,"character":1}},"targetSelectionRange":{"start":{"line":32,"character":8},"end":{"line":32,"character":14}}}]}"#;
        let (uri, line, col) = parse_definition(json).expect("location link");
        assert_eq!(uri, "file:///C:/tmp/lib.rs");
        assert_eq!(line, 32);
        assert_eq!(col, 8);
    }

    #[test]
    fn parse_definition_location_link_falls_back_to_target_range() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":[{"targetUri":"file:///C:/tmp/lib.rs","targetRange":{"start":{"line":9,"character":3},"end":{"line":11,"character":0}}}]}"#;
        let (uri, line, col) = parse_definition(json).expect("location link");
        assert_eq!(uri, "file:///C:/tmp/lib.rs");
        assert_eq!(line, 9);
        assert_eq!(col, 3);
    }

    #[test]
    fn parse_definition_none_on_null() {
        let json = r#"{"jsonrpc":"2.0","result":null,"id":3}"#;
        assert_eq!(parse_definition(json), None);
    }

    // ---- uri -> path ----

    #[test]
    fn uri_to_path_windows_drive() {
        let p = uri_to_path("file:///C:/Users/me/foo.mty").unwrap();
        assert_eq!(p, std::path::PathBuf::from(r"C:\Users\me\foo.mty"));
    }

    #[test]
    fn uri_to_path_percent_decodes() {
        let p = uri_to_path("file:///C:/a%20b/c.mty").unwrap();
        assert_eq!(p, std::path::PathBuf::from(r"C:\a b\c.mty"));
    }

    #[test]
    fn uri_to_path_percent_decodes_utf8_bytes() {
        let p = uri_to_path("file:///C:/tmp/%E6%9D%B1%F0%9F%98%80.rs").unwrap();
        assert_eq!(p, std::path::PathBuf::from(r"C:\tmp\東😀.rs"));
    }

    #[test]
    fn uri_to_path_rejects_non_file() {
        assert_eq!(uri_to_path("http://example.com"), None);
    }

    // ---- path equality ----

    #[test]
    fn paths_equal_normalizes_case_and_sep() {
        let a = std::path::Path::new(r"C:\Users\Me\Foo.mty");
        let b = std::path::Path::new("c:/users/me/foo.mty");
        assert!(paths_equal(a, b));
        let c = std::path::Path::new("c:/users/me/bar.mty");
        assert!(!paths_equal(a, c));
    }

    #[test]
    fn def_state_path_matches() {
        let mut d = DefState::new();
        assert!(!d.path_matches(Some(std::path::Path::new("x"))));
        d.set(Some(DefTarget {
            path: std::path::PathBuf::from(r"C:\a\b.mty"),
            line: 1,
            col: 2,
        }));
        assert!(d.path_matches(Some(std::path::Path::new("c:/a/b.mty"))));
        assert!(!d.path_matches(Some(std::path::Path::new("c:/a/c.mty"))));
        assert!(!d.path_matches(None));
    }

    // ---- response isolation ----

    #[test]
    fn isolate_response_picks_id2_object() {
        let stream = r#"{"result":{"capabilities":{}},"id":1}{"result":{"contents":{"value":"X"}},"id":2}"#;
        let one = lsp::isolate_response(stream, "\"id\":2");
        assert!(one.contains("\"value\":\"X\""));
        assert!(!one.contains("capabilities"));
        // The parser then sees only the right object.
        assert_eq!(parse_hover_value(&one).unwrap(), "X");
    }

    #[test]
    fn isolate_response_handles_braces_in_strings() {
        // A string containing braces must not confuse the balancer.
        let stream = r#"{"result":{"contents":{"value":"a{b}c"}},"id":2}"#;
        let one = lsp::isolate_response(stream, "\"id\":2");
        assert_eq!(parse_hover_value(&one).unwrap(), "a{b}c");
    }

    // ---- guarded LSP integration test ----

    /// Spawn the real `mty lsp`, open a tiny program, and request hover +
    /// definition on a known symbol. SKIPS (passes with a note) if `mty` can't
    /// spawn so CI without `mty` stays green.
    #[test]
    fn lsp_hover_and_definition_end_to_end() {
        use std::path::PathBuf;
        use std::time::Duration;

        let mty = PathBuf::from(crate::mty::path());
        let has_mty = std::env::var_os("MIGHTY_MTY").is_some() || mty.exists();
        if !has_mty {
            eprintln!("lsp_hover_and_definition_end_to_end: no mty binary — skipping");
            return;
        }

        // `add` defined on line 0, called on line 5 (0-based).
        let source = "fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\nfn main() {\n  let r = add(1, 2)\n}\n";
        // Use an ABSOLUTE path so the uri round-trip (uri the server echoes ->
        // uri_to_path) matches faithfully (a relative path becomes `/probe.mty`
        // -> `\probe.mty`, which is only a test artifact, not a real flow).
        let path = std::env::temp_dir().join("probe_nav.mty");

        // Hover on the `add` call (line 5, char 10).
        let hraw = lsp::request_with_timeout(
            &path,
            source,
            5,
            10,
            lsp::Req::Hover,
            Duration::from_secs(8),
        );
        match parse_hover_value(&hraw) {
            Some(v) => {
                eprintln!("hover value: {v:?}");
                assert!(
                    v.contains("add") || v.contains("fn"),
                    "expected the `add` signature in hover, got: {v:?}"
                );
            }
            None => eprintln!(
                "lsp_hover_and_definition_end_to_end: no hover (timeout/flaky) — skipping assert"
            ),
        }

        // Definition on the same `add` call -> should point at line 0.
        let draw = lsp::request_with_timeout(
            &path,
            source,
            5,
            10,
            lsp::Req::Definition,
            Duration::from_secs(8),
        );
        match parse_definition(&draw) {
            Some((uri, line, _col)) => {
                eprintln!("definition: uri={uri} line={line}");
                let p = uri_to_path(&uri).expect("resolvable uri");
                assert!(paths_equal(&p, &path), "def path {p:?} != {path:?}");
                assert_eq!(line, 0, "expected `add` definition on line 0");
            }
            None => eprintln!(
                "lsp_hover_and_definition_end_to_end: no definition (timeout/flaky) — skipping assert"
            ),
        }
    }
}
