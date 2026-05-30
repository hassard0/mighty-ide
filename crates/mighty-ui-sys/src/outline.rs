//! Outline / document-symbols panel (shim-side).
//!
//! ## Why a fallback scanner (not LSP `documentSymbol`)
//!
//! `mty-lsp` (v0.5) does **not** implement `textDocument/documentSymbol`: it
//! returns JSON-RPC error `-32601 "Method not found"` and omits
//! `documentSymbolProvider` from its `initialize` capabilities (probed
//! 2026-05-29; see `docs/mighty-language-lessons.md`). So we keep an LSP
//! parser (for the day it lands — both the hierarchical `DocumentSymbol[]` and
//! the flat `SymbolInformation[]` shapes) AND a robust shim-side **scanner** of
//! the live buffer, and the [`OutlineState`] uses whichever produced symbols
//! (LSP first, scanner fallback). [`refresh`](OutlineState::refresh) reports
//! which path was used.
//!
//! ## What the scanner recognizes
//!
//! Top-level + nested declarations of the Mighty grammar:
//! `fn` `struct` `enum` `agent` `protocol` `type` (+ `impl` blocks group their
//! methods). Nesting depth is tracked by brace balance, so an `fn` inside an
//! `impl`/`agent`/`protocol` body is reported as a child (`depth + 1`). The
//! scanner is line-oriented and tolerant (comments / strings on the keyword
//! line are handled), which is enough for an editor outline.

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

// ===========================================================================
// Symbol model
// ===========================================================================

/// The kind of a document symbol. The scalar values are exposed over the C ABI
/// (`mui_outline_row_kind`) so Mighty / tests can branch on them; they also map
/// to the LSP `SymbolKind` numbers we care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymKind {
    Function = 0,
    Struct = 1,
    Enum = 2,
    Agent = 3,
    Protocol = 4,
    TypeAlias = 5,
    Impl = 6,
    Field = 7,
    Variant = 8,
    Const = 9,
}

impl SymKind {
    /// The keyword that introduces this kind, if it is a leading-keyword decl.
    fn from_keyword(kw: &str) -> Option<SymKind> {
        Some(match kw {
            "fn" => SymKind::Function,
            "struct" => SymKind::Struct,
            "enum" => SymKind::Enum,
            "agent" => SymKind::Agent,
            "protocol" => SymKind::Protocol,
            "type" => SymKind::TypeAlias,
            "impl" => SymKind::Impl,
            "const" => SymKind::Const,
            "let" => SymKind::Const, // top-level `let` reads as a const-ish binding
            _ => return None,
        })
    }

    /// Map an LSP `SymbolKind` integer to our kind (best-effort; unknown -> Const).
    fn from_lsp(n: u32) -> SymKind {
        match n {
            12 => SymKind::Function,  // Function
            6 => SymKind::Function,   // Method
            23 => SymKind::Struct,    // Struct
            5 => SymKind::Struct,     // Class
            10 => SymKind::Enum,      // Enum
            11 => SymKind::Protocol,  // Interface
            26 => SymKind::TypeAlias, // TypeParameter
            8 => SymKind::Field,      // Field
            7 => SymKind::Field,      // Property
            22 => SymKind::Variant,   // EnumMember
            14 => SymKind::Const,     // Constant
            _ => SymKind::Const,
        }
    }

    /// The vector icon path (24x24) drawn for this kind in the panel + dropdowns.
    pub fn icon(self) -> &'static str {
        match self {
            // `fn` symbol marker (reuses the breadcrumb glyph).
            SymKind::Function => "M5 12h6V6m8 6h-6v6",
            // struct: a small braces/box.
            SymKind::Struct => "M8 4H6a2 2 0 0 0-2 2v3l-1 3 1 3v3a2 2 0 0 0 2 2h2 M16 4h2a2 2 0 0 1 2 2v3l1 3-1 3v3a2 2 0 0 1-2 2h-2",
            // enum: stacked options.
            SymKind::Enum => "M5 7h14M5 12h14M5 17h9",
            // agent: robot-ish head.
            SymKind::Agent => "M6 9h12a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1z M12 6v3 M9 14h6",
            // protocol: interface diamond.
            SymKind::Protocol => "M12 3 21 12 12 21 3 12z",
            // type alias: a tag.
            SymKind::TypeAlias => "M4 7a3 3 0 0 1 3-3h6l7 7-9 9-7-7z M8.5 8.5h.01",
            // impl: layered.
            SymKind::Impl => "M12 3 21 8 12 13 3 8z M3 13l9 5 9-5",
            // field: dot + line.
            SymKind::Field => "M7 12h.01 M11 12h6",
            // variant: bullet.
            SymKind::Variant => "M8 12h.01 M12 12h6",
            // const: a small lock-ish.
            SymKind::Const => "M7 11V8a5 5 0 0 1 10 0v3 M5 11h14v8H5z",
        }
    }

    /// Display color for this kind's icon + name (Vivid-Modern semantic palette).
    pub fn color(self) -> MuiColor {
        match self {
            SymKind::Function => theme::SYN_FUNCTION(),
            SymKind::Struct | SymKind::TypeAlias => theme::SYN_TYPE(),
            SymKind::Enum | SymKind::Variant => theme::INFO(),
            SymKind::Agent => theme::ACCENT_BRIGHT(),
            SymKind::Protocol => theme::SYN_KEYWORD(),
            SymKind::Impl => theme::TEXT_3(),
            SymKind::Field => theme::TEXT_1(),
            SymKind::Const => theme::SYN_NUMBER(),
        }
    }

    /// Short label used in tests / the panel's kind glyph fallback.
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            SymKind::Function => "fn",
            SymKind::Struct => "struct",
            SymKind::Enum => "enum",
            SymKind::Agent => "agent",
            SymKind::Protocol => "protocol",
            SymKind::TypeAlias => "type",
            SymKind::Impl => "impl",
            SymKind::Field => "field",
            SymKind::Variant => "variant",
            SymKind::Const => "const",
        }
    }
}

/// One outline symbol: a name + kind + 0-based `line` (the declaration line) +
/// nesting `depth` (0 = top level). The list is kept FLAT (pre-order DFS) so the
/// scalar ABI can stream rows; `depth` carries the hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymKind,
    /// 0-based declaration line.
    pub line: u32,
    /// Nesting depth (0 = top level).
    pub depth: u32,
}

// ===========================================================================
// Shim-side scanner (the fallback that is actually used today)
// ===========================================================================

/// Scan `source` for declarations and return them in document order (pre-order:
/// a container precedes its members). Depth is brace-balance based: a decl found
/// while inside one or more open `{` blocks is nested under them.
///
/// Recognized leading keywords: `fn struct enum agent protocol type impl const`.
/// `enum` variants and `struct` fields are NOT individually scanned here (kept
/// lightweight); the container line is the jump target. The scan is line based
/// and string/line-comment aware so a `{` inside a string or `//` comment does
/// not skew the depth.
pub fn scan_symbols(source: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    let mut depth: i32 = 0;
    for (lineno, raw) in source.lines().enumerate() {
        let code = strip_line_noise(raw);
        let trimmed = code.trim_start();
        // A declaration keyword must be the first token on the (code) line.
        if let Some((kw, rest)) = leading_keyword(trimmed) {
            if let Some(kind) = SymKind::from_keyword(kw) {
                // `let`/`const` are only outline-worthy at top level (module
                // bindings); inside a body they are locals, not symbols.
                let body_local = matches!(kw, "let" | "const") && depth > 0;
                if !body_local {
                    if let Some(name) = decl_name(kind, rest) {
                        out.push(Symbol {
                            name,
                            kind,
                            line: lineno as u32,
                            depth: depth.max(0) as u32,
                        });
                    }
                }
            }
        }
        // Update brace depth using only the code (noise-stripped) portion.
        depth += brace_delta(&code);
        if depth < 0 {
            depth = 0;
        }
    }
    out
}

/// Remove a trailing `//` line comment and blank out string contents (so braces
/// / keywords inside strings don't affect scanning). Block comments are not
/// fully handled (rare on a decl line); a `/*` truncates the rest of the line.
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
                continue;
            }
            if b == str_ch {
                in_str = false;
            }
            // Replace string content with a space placeholder.
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
            b'/' if i + 1 < bytes.len() && (bytes[i + 1] == b'/' ) => break, // line comment
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => break,    // block comment start
            _ => out.push(b as char),
        }
        i += 1;
    }
    out
}

/// If `s` begins with an identifier token, return `(token, rest_after_token)`.
fn leading_keyword(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    // Skip a `pub` visibility modifier.
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

/// Net `{` minus `}` count in a (noise-stripped) line.
fn brace_delta(code: &str) -> i32 {
    let mut d = 0i32;
    for b in code.bytes() {
        match b {
            b'{' => d += 1,
            b'}' => d -= 1,
            _ => {}
        }
    }
    d
}

/// Extract the declared name from the text after the keyword. For `fn add(...)`
/// the name is `add`; for `struct Point {` it's `Point`; for `impl Foo {` it's
/// `Foo`; for `type Id = ...` it's `Id`; for `const MAX: ...` it's `MAX`.
fn decl_name(kind: SymKind, rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    // The name is the first identifier; stop at `(`, `<`, `:`, `=`, `{`, ws.
    let end = rest
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let name = &rest[..end];
    // `impl Trait for Type` -> prefer the `for Type` target as the display name.
    if kind == SymKind::Impl {
        if let Some(pos) = rest.find(" for ") {
            let after = rest[pos + 5..].trim_start();
            let e2 = after
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
                .map(|(i, _)| i)
                .unwrap_or(after.len());
            if e2 > 0 {
                return Some(format!("{} for {}", name, &after[..e2]));
            }
        }
    }
    Some(name.to_string())
}

// ===========================================================================
// LSP documentSymbol parsing (kept ready for when mty-lsp implements it)
// ===========================================================================

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parse a `textDocument/documentSymbol` response. Handles both the hierarchical
/// `DocumentSymbol[]` (with nested `children`) and the flat `SymbolInformation[]`
/// (`{name, kind, location:{range}}`) shapes, flattening to a pre-order list with
/// `depth`. Returns `None` if the result is `null` / empty / an error so the
/// caller can fall back to the scanner.
pub fn parse_document_symbols(json: &str) -> Option<Vec<Symbol>> {
    let bytes = json.as_bytes();
    if find_sub(bytes, b"\"error\"").is_some() && find_sub(bytes, b"-32601").is_some() {
        return None; // method not found
    }
    // Find the result array start.
    let res_at = find_sub(bytes, b"\"result\"")?;
    let mut i = res_at + b"\"result\"".len();
    while i < bytes.len() && matches!(bytes[i], b' ' | b':' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'[' {
        // Could be empty.
        let mut k = i + 1;
        while k < bytes.len() && matches!(bytes[k], b' ' | b'\t' | b'\r' | b'\n') {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == b']' {
            return Some(Vec::new());
        }
    } else {
        return None;
    }
    let mut syms = Vec::new();
    parse_symbol_array(&bytes[i..], 0, &mut syms);
    if syms.is_empty() {
        Some(Vec::new())
    } else {
        Some(syms)
    }
}

/// Parse a `[ {symbol}, ... ]` array slice into `out` at the given `depth`,
/// recursing into any `children`. Works for both DocumentSymbol (uses
/// `selectionRange`/`range`) and SymbolInformation (uses `location.range`).
fn parse_symbol_array(arr: &[u8], depth: u32, out: &mut Vec<Symbol>) {
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
                        parse_one_symbol(&arr[s..=k], depth, out);
                    }
                }
            }
            b']' if brace == 0 => break,
            _ => {}
        }
    }
}

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

/// Parse one symbol object (`{...}`) into `out`, then recurse into `children`.
fn parse_one_symbol(obj: &[u8], depth: u32, out: &mut Vec<Symbol>) {
    let name = match read_str_after(obj, b"\"name\"") {
        Some(n) => n,
        None => return,
    };
    let kind_n = read_uint_after(obj, b"\"kind\"").unwrap_or(14);
    // selectionRange (DocumentSymbol) preferred, else range, else location.range.
    let line = read_uint_after(obj, b"\"selectionRange\"")
        .map(|_| {
            // grab the line inside selectionRange's start.
            let p = find_sub(obj, b"\"selectionRange\"").unwrap();
            read_uint_after(&obj[p..], b"\"line\"").unwrap_or(0)
        })
        .or_else(|| {
            let p = find_sub(obj, b"\"range\"")?;
            read_uint_after(&obj[p..], b"\"line\"")
        })
        .unwrap_or(0);
    out.push(Symbol {
        name,
        kind: SymKind::from_lsp(kind_n),
        line,
        depth,
    });
    // children: a nested array.
    if let Some(ch_at) = find_sub(obj, b"\"children\"") {
        let mut i = ch_at + b"\"children\"".len();
        while i < obj.len() && matches!(obj[i], b' ' | b':' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i < obj.len() && obj[i] == b'[' {
            parse_symbol_array(&obj[i..], depth + 1, out);
        }
    }
}

// ===========================================================================
// Outline panel state
// ===========================================================================

/// The Outline panel's state: the current symbol list + which source produced it.
#[derive(Debug, Default)]
pub struct OutlineState {
    syms: Vec<Symbol>,
    /// `true` if the last refresh used the LSP `documentSymbol` path; `false` for
    /// the shim-side scanner fallback (the path actually used with mty-lsp v0.5).
    used_lsp: bool,
    /// The symbol index that contains the cursor (set by `set_cursor`), or `None`.
    current: Option<usize>,
}

impl OutlineState {
    pub fn new() -> Self {
        OutlineState::default()
    }

    /// Replace the symbol list from a scan of `source`, trying the LSP response
    /// `lsp_json` first (when non-empty) and falling back to the scanner.
    /// Returns the symbol count. `used_lsp` records the path.
    pub fn refresh(&mut self, source: &str, lsp_json: &str) -> usize {
        if !lsp_json.trim().is_empty() {
            if let Some(syms) = parse_document_symbols(lsp_json) {
                if !syms.is_empty() {
                    self.syms = syms;
                    self.used_lsp = true;
                    return self.syms.len();
                }
            }
        }
        self.syms = scan_symbols(source);
        self.used_lsp = false;
        self.syms.len()
    }

    /// Set the symbol list directly (used by tests / the scanner-only path).
    #[allow(dead_code)]
    pub fn set(&mut self, syms: Vec<Symbol>) {
        self.syms = syms;
    }

    pub fn used_lsp(&self) -> bool {
        self.used_lsp
    }

    pub fn count(&self) -> usize {
        self.syms.len()
    }

    pub fn get(&self, i: usize) -> Option<&Symbol> {
        self.syms.get(i)
    }

    /// The symbols, read-only (used by the breadcrumb dropdown).
    pub fn symbols(&self) -> &[Symbol] {
        &self.syms
    }

    /// The index of the symbol that contains the (0-based) cursor `line`: the
    /// LAST symbol whose declaration line is `<= line` (document order means the
    /// most recent enclosing decl). `None` if no symbol precedes the cursor.
    pub fn symbol_at_line(&self, line: u32) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, s) in self.syms.iter().enumerate() {
            if s.line <= line {
                best = Some(i);
            } else {
                break;
            }
        }
        best
    }

    /// Update the cursor-tracked current symbol from a 0-based `line`.
    pub fn set_cursor(&mut self, line: u32) -> i32 {
        self.current = self.symbol_at_line(line);
        self.current.map(|i| i as i32).unwrap_or(-1)
    }

    pub fn current(&self) -> i32 {
        self.current.map(|i| i as i32).unwrap_or(-1)
    }

    /// 0-based jump line of symbol `i`.
    pub fn line_of(&self, i: usize) -> i32 {
        self.syms.get(i).map(|s| s.line as i32).unwrap_or(-1)
    }

    /// Draw the Outline panel in the sidebar band. Mirrors the SCM/Search panel
    /// chrome: header band + rows (per-kind icon + name, indented by depth, the
    /// cursor-current row highlighted in indigo). No-op handled by the caller.
    pub fn draw(&self, ctx: &mut crate::MuiContext) {
        let h = ctx.gpu.height as f32;
        let clip = ctx.clip;
        let chrome = theme::CHROME_FONT_SIZE;
        let adv = chrome * 0.55;
        let sx = layout::RAIL_W;
        let sw = layout::SIDEBAR_W;

        ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
        ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

        // Header band.
        let head_h = 40.0;
        ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
        ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
        let title = "OUTLINE";
        let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
        ctx.text.queue_ui_sized(
            sx + 14.0,
            (head_h - (chrome - 2.0)) * 0.5 - 1.0,
            &tracked,
            theme::DIM(),
            chrome - 2.0,
            clip,
        );
        let cnt = self.syms.len().to_string();
        ctx.text.queue_ui_sized(sx + 78.0, (head_h - (chrome - 2.0)) * 0.5 - 1.0, &cnt, theme::TEXT_3(), chrome - 2.0, clip);

        if self.syms.is_empty() {
            ctx.text.queue_ui_sized(sx + 14.0, head_h + 12.0, "No symbols", theme::TEXT_3(), chrome, clip);
            return;
        }

        let row_h = layout::LINE_H();
        let top = head_h + 6.0;
        for (i, s) in self.syms.iter().enumerate() {
            let y = top + i as f32 * row_h;
            if y > h {
                break;
            }
            let indent = s.depth as f32 * 14.0;
            let icon_y = y + (row_h - 14.0) * 0.5;
            let txt_y = y + (row_h - chrome) * 0.5 - 1.0;

            if Some(i) == self.current {
                ctx.dl_grad_h(sx + 4.0, y + 1.0, sw - 8.0, row_h - 2.0, 5.0, theme::accent_a(0.18), 0.9);
                ctx.dl_round(sx, y + 1.0, 2.5, row_h - 2.0, 1.0, theme::ACCENT());
            }

            let ix = sx + 14.0 + indent;
            ctx.dl_icon(ix, icon_y, 14.0, 14.0, s.kind.icon(), s.kind.color(), 1.5, false);
            let name_x = ix + 20.0;
            let fg = if Some(i) == self.current { theme::TEXT() } else { theme::TEXT_1() };
            let avail = (((sx + sw - 12.0) - name_x) / adv).floor() as usize;
            let mut name = s.name.clone();
            if name.chars().count() > avail && avail > 1 {
                name = name.chars().take(avail - 1).collect::<String>() + "\u{2026}";
            }
            ctx.text.queue_ui_sized(name_x, txt_y, &name, fg, chrome, clip);
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
    fn scan_top_level_decls() {
        let src = "\
fn add(a: I32, b: I32) -> I32 {\n  a + b\n}\n\
struct Point {\n  x: I32,\n}\n\
enum Color { Red, Green }\n\
type Id = I32\n\
agent Worker {\n}\n\
protocol Greet {\n}\n";
        let syms = scan_symbols(src);
        let names: Vec<_> = syms.iter().map(|s| (s.name.as_str(), s.kind, s.depth)).collect();
        assert_eq!(names[0], ("add", SymKind::Function, 0));
        assert_eq!(names[1], ("Point", SymKind::Struct, 0));
        assert_eq!(names[2], ("Color", SymKind::Enum, 0));
        assert_eq!(names[3], ("Id", SymKind::TypeAlias, 0));
        assert_eq!(names[4], ("Worker", SymKind::Agent, 0));
        assert_eq!(names[5], ("Greet", SymKind::Protocol, 0));
    }

    #[test]
    fn scan_nested_methods_get_depth() {
        let src = "\
impl Point {\n  fn x(self) -> I32 { 0 }\n  fn y(self) -> I32 { 0 }\n}\n\
fn free() {}\n";
        let syms = scan_symbols(src);
        assert_eq!(syms[0].name, "Point");
        assert_eq!(syms[0].kind, SymKind::Impl);
        assert_eq!(syms[0].depth, 0);
        assert_eq!(syms[1].name, "x");
        assert_eq!(syms[1].depth, 1);
        assert_eq!(syms[2].name, "y");
        assert_eq!(syms[2].depth, 1);
        // `free` is back at depth 0 after the impl block closes.
        assert_eq!(syms[3].name, "free");
        assert_eq!(syms[3].depth, 0);
    }

    #[test]
    fn scan_ignores_braces_in_strings_and_comments() {
        let src = "\
fn a() {\n  let s = \"a { b } c\"  // trailing } comment {\n}\n\
fn b() {}\n";
        let syms = scan_symbols(src);
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "a");
        assert_eq!(syms[0].depth, 0);
        assert_eq!(syms[1].name, "b");
        // If the string/comment braces leaked, `b` would be at depth > 0.
        assert_eq!(syms[1].depth, 0);
    }

    #[test]
    fn scan_pub_modifier() {
        let src = "pub fn exported() {}\npub struct S {}\n";
        let syms = scan_symbols(src);
        assert_eq!(syms[0].name, "exported");
        assert_eq!(syms[0].kind, SymKind::Function);
        assert_eq!(syms[1].name, "S");
        assert_eq!(syms[1].kind, SymKind::Struct);
    }

    #[test]
    fn scan_impl_for_target() {
        let src = "impl Display for Point {\n  fn fmt(self) {}\n}\n";
        let syms = scan_symbols(src);
        assert_eq!(syms[0].name, "Display for Point");
        assert_eq!(syms[0].kind, SymKind::Impl);
    }

    #[test]
    fn symbol_at_line_finds_enclosing() {
        let src = "fn a() {\n  x\n}\nfn b() {\n  y\n}\n";
        let mut st = OutlineState::new();
        st.refresh(src, "");
        // line 1 (inside a) -> symbol 0
        assert_eq!(st.symbol_at_line(1), Some(0));
        // line 4 (inside b) -> symbol 1
        assert_eq!(st.symbol_at_line(4), Some(1));
        // line 0 (the fn a decl) -> symbol 0
        assert_eq!(st.symbol_at_line(0), Some(0));
    }

    #[test]
    fn refresh_uses_scanner_when_lsp_empty() {
        let mut st = OutlineState::new();
        let n = st.refresh("fn main() {}\n", "");
        assert_eq!(n, 1);
        assert!(!st.used_lsp());
    }

    #[test]
    fn refresh_uses_scanner_when_lsp_method_not_found() {
        // mty-lsp v0.5's actual response.
        let err = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":2}"#;
        let mut st = OutlineState::new();
        let n = st.refresh("fn main() {}\nstruct S {}\n", err);
        assert_eq!(n, 2);
        assert!(!st.used_lsp(), "should fall back to scanner on -32601");
    }

    #[test]
    fn parse_document_symbols_hierarchical() {
        // DocumentSymbol[] with children.
        let json = r#"{"jsonrpc":"2.0","result":[{"name":"Point","kind":23,"range":{"start":{"line":0,"character":0},"end":{"line":4,"character":1}},"selectionRange":{"start":{"line":0,"character":7},"end":{"line":0,"character":12}},"children":[{"name":"x","kind":8,"range":{"start":{"line":1,"character":2},"end":{"line":1,"character":8}},"selectionRange":{"start":{"line":1,"character":2},"end":{"line":1,"character":3}}}]}],"id":2}"#;
        let syms = parse_document_symbols(json).expect("symbols");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "Point");
        assert_eq!(syms[0].kind, SymKind::Struct);
        assert_eq!(syms[0].depth, 0);
        assert_eq!(syms[0].line, 0);
        assert_eq!(syms[1].name, "x");
        assert_eq!(syms[1].depth, 1);
        assert_eq!(syms[1].line, 1);
    }

    #[test]
    fn parse_document_symbols_flat() {
        // SymbolInformation[] (flat, location.range).
        let json = r#"{"result":[{"name":"add","kind":12,"location":{"uri":"file:///a.mty","range":{"start":{"line":2,"character":0},"end":{"line":4,"character":1}}}},{"name":"main","kind":12,"location":{"uri":"file:///a.mty","range":{"start":{"line":6,"character":0},"end":{"line":8,"character":1}}}}],"id":2}"#;
        let syms = parse_document_symbols(json).expect("symbols");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "add");
        assert_eq!(syms[0].kind, SymKind::Function);
        assert_eq!(syms[0].line, 2);
        assert_eq!(syms[1].name, "main");
        assert_eq!(syms[1].line, 6);
    }

    #[test]
    fn parse_document_symbols_none_on_error_or_null() {
        let err = r#"{"error":{"code":-32601,"message":"Method not found"},"id":2}"#;
        assert!(parse_document_symbols(err).is_none());
        assert!(parse_document_symbols(r#"{"result":null,"id":2}"#).is_none());
        // empty array -> Some(empty)
        assert_eq!(parse_document_symbols(r#"{"result":[],"id":2}"#), Some(Vec::new()));
    }
}
