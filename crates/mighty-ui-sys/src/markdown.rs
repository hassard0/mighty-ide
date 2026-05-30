//! A focused, dependency-free Markdown parser (shim-side) → a block render model
//! the Vello preview pane draws.
//!
//! This is deliberately NOT a full CommonMark implementation — it covers the set
//! the IDE's live preview needs, robustly, over messy real-world input:
//!
//! * ATX headings `#`..`######` (with a trailing `#` run stripped).
//! * Paragraphs with inline spans: `**bold**`, `*italic*` / `_italic_`,
//!   `` `code` ``, `[text](url)`, `~~strike~~`, with escaping (`\*`) and nesting.
//! * Unordered lists (`-`/`*`/`+`) and ordered lists (`1.`/`1)`), nested by
//!   indent (every 2 spaces = one level).
//! * Fenced code blocks (```` ``` ```` / `~~~`) with an optional language tag.
//! * Blockquotes (`>`), with the marker stripped (one level).
//! * Horizontal rules (`---` / `***` / `___`).
//! * Simple pipe tables (`| a | b |` + a `---|---` separator row).
//!
//! The parser is pure + GPU-free so it is exhaustively unit-testable; the preview
//! pane ([`crate::mdpreview`]) turns [`Block`]s + [`Span`]s into themed Vello draw
//! calls. Re-parsing the whole source each frame is cheap for IDE-sized files.

#![allow(dead_code)]

/// One inline span within a paragraph / heading / list item / table cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    /// Plain text run.
    Text(String),
    /// **Bold** run (its own nested spans).
    Bold(Vec<Span>),
    /// *Italic* run (its own nested spans).
    Italic(Vec<Span>),
    /// ~~Strikethrough~~ run.
    Strike(Vec<Span>),
    /// `inline code` (verbatim, no nested formatting).
    Code(String),
    /// `[text](url)` link: the displayed spans + the destination.
    Link { text: Vec<Span>, url: String },
}

/// One block-level element of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // `CodeBlock` is the conventional name
pub enum Block {
    /// ATX heading: `level` 1..=6 + the inline spans of its text.
    Heading { level: u8, spans: Vec<Span> },
    /// A paragraph of inline spans.
    Paragraph(Vec<Span>),
    /// A list (ordered or not) of items; each item is its own spans + an indent
    /// depth (0 = top level). Nested items follow their parent in order with a
    /// higher `depth`; the renderer indents by depth.
    List { ordered: bool, items: Vec<ListItem> },
    /// A fenced code block: the optional language tag + the raw lines (verbatim).
    CodeBlock { lang: Option<String>, lines: Vec<String> },
    /// A blockquote: nested blocks (one `>` level stripped).
    Quote(Vec<Block>),
    /// A horizontal rule.
    Rule,
    /// A simple table: a header row + body rows, each a list of cell span-lists.
    Table {
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

/// One list item: its inline spans + its nesting depth (0 = top level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub depth: usize,
    pub spans: Vec<Span>,
    /// For an ordered list, the marker number as typed (so `3.` shows `3`).
    pub number: Option<u64>,
}

/// Parse a whole markdown `source` string into a list of [`Block`]s. Robust to
/// messy input: unterminated fences close at EOF, stray markers fall back to
/// plain text, blank lines separate paragraphs.
pub fn parse(source: &str) -> Vec<Block> {
    let lines: Vec<&str> = source.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    parse_lines(&lines)
}

/// Parse a slice of (already CR-stripped) lines into blocks. Recursed into for
/// blockquote bodies.
fn parse_lines(lines: &[&str]) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // Blank line: paragraph/section break.
        if trimmed.trim_end().is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block (``` or ~~~), possibly with a language tag.
        if let Some(fence) = fence_marker(trimmed) {
            let lang = trimmed[fence.len()..].trim();
            let lang = if lang.is_empty() { None } else { Some(lang.to_string()) };
            let mut code: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if fence_marker(l.trim_start()).map(|f| f.starts_with(&fence[..1])).unwrap_or(false)
                    && l.trim_start().trim_end().chars().all(|c| c == fence.as_bytes()[0] as char)
                {
                    i += 1; // consume the closing fence
                    break;
                }
                code.push(l.to_string());
                i += 1;
            }
            blocks.push(Block::CodeBlock { lang, lines: code });
            continue;
        }

        // Horizontal rule: a line of 3+ of the same -, * or _ (with optional spaces).
        if is_hr(trimmed) {
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }

        // ATX heading.
        if let Some((level, rest)) = atx_heading(trimmed) {
            blocks.push(Block::Heading { level, spans: parse_inline(rest) });
            i += 1;
            continue;
        }

        // Blockquote: collect the contiguous run of `>`-prefixed lines, strip one
        // level, and recurse.
        if trimmed.starts_with('>') {
            let mut inner: Vec<&str> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                if !t.starts_with('>') {
                    // A blank line ends the quote; anything else also ends it.
                    break;
                }
                // Strip one `>` and an optional following space.
                let after = &t[1..];
                inner.push(after.strip_prefix(' ').unwrap_or(after));
                i += 1;
            }
            blocks.push(Block::Quote(parse_lines(&inner)));
            continue;
        }

        // Table: a header row of pipes followed by a separator row (`---|---`).
        if looks_like_table_row(line) && i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
            let header = parse_table_row(line);
            i += 2; // header + separator
            let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
            while i < lines.len() && looks_like_table_row(lines[i]) {
                rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            blocks.push(Block::Table { header, rows });
            continue;
        }

        // List (unordered or ordered): collect contiguous list lines.
        if list_marker(line).is_some() {
            let ordered = matches!(list_marker(line), Some(Marker::Ordered(_)));
            let mut items: Vec<ListItem> = Vec::new();
            while i < lines.len() {
                let Some(marker) = list_marker(lines[i]) else { break };
                // Mixing ordered/unordered ends the current list.
                let this_ordered = matches!(marker, Marker::Ordered(_));
                if this_ordered != ordered {
                    break;
                }
                let depth = indent_depth(lines[i]);
                let (content, number) = match marker {
                    Marker::Ordered(n) => (strip_list_marker(lines[i]), Some(n)),
                    Marker::Unordered => (strip_list_marker(lines[i]), None),
                };
                items.push(ListItem { depth, spans: parse_inline(content), number });
                i += 1;
            }
            blocks.push(Block::List { ordered, items });
            continue;
        }

        // Otherwise: a paragraph — gather consecutive non-blank, non-block lines.
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim_start();
            if t.trim_end().is_empty()
                || atx_heading(t).is_some()
                || is_hr(t)
                || fence_marker(t).is_some()
                || t.starts_with('>')
                || list_marker(l).is_some()
            {
                break;
            }
            para.push(l.trim());
            i += 1;
        }
        if !para.is_empty() {
            blocks.push(Block::Paragraph(parse_inline(&para.join(" "))));
        }
    }
    blocks
}

/// If `trimmed` opens a code fence, return the fence marker (the run of `` ` ``
/// or `~`, length >= 3). The remainder after it is the info/language string.
fn fence_marker(trimmed: &str) -> Option<String> {
    for ch in ['`', '~'] {
        let run: String = trimmed.chars().take_while(|c| *c == ch).collect();
        if run.len() >= 3 {
            return Some(run);
        }
    }
    None
}

/// `true` if the line is a horizontal rule: 3+ of the same `-`, `*`, or `_`,
/// optionally separated by spaces, and nothing else.
fn is_hr(trimmed: &str) -> bool {
    for ch in ['-', '*', '_'] {
        let stripped: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if stripped.len() >= 3 && stripped.chars().all(|c| c == ch) {
            return true;
        }
    }
    false
}

/// Parse an ATX heading: 1..=6 leading `#` then a space (or end). Returns the
/// level and the trimmed text (a trailing `#` run is stripped). `None` if not a
/// heading (e.g. `#nospace` or 7+ hashes).
fn atx_heading(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None; // `#word` is not a heading
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    Some((hashes as u8, text))
}

/// A detected list marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Unordered,
    Ordered(u64),
}

/// Detect a list marker at the start of `line` (after indentation). Unordered:
/// `-`, `*`, `+` followed by a space. Ordered: digits then `.` or `)` then a
/// space.
fn list_marker(line: &str) -> Option<Marker> {
    let t = line.trim_start();
    let mut chars = t.chars();
    match chars.next() {
        Some('-') | Some('*') | Some('+') => {
            if t.chars().nth(1) == Some(' ') {
                return Some(Marker::Unordered);
            }
            None
        }
        Some(c) if c.is_ascii_digit() => {
            let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = &t[digits.len()..];
            if (after.starts_with(". ") || after.starts_with(") ")) && !digits.is_empty() {
                return digits.parse::<u64>().ok().map(Marker::Ordered);
            }
            None
        }
        _ => None,
    }
}

/// Strip the list marker (and one following space) from `line`, returning the
/// item content (trimmed of leading whitespace before the marker too).
fn strip_list_marker(line: &str) -> &str {
    let t = line.trim_start();
    match list_marker(line) {
        Some(Marker::Unordered) => t[1..].strip_prefix(' ').unwrap_or(&t[1..]),
        Some(Marker::Ordered(_)) => {
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            // skip digits + the `.`/`)` + one space
            let after = &t[digits + 1..];
            after.strip_prefix(' ').unwrap_or(after)
        }
        None => t,
    }
}

/// Indentation depth (every 2 leading spaces, or one tab, = one level).
fn indent_depth(line: &str) -> usize {
    let mut spaces = 0usize;
    for c in line.chars() {
        match c {
            ' ' => spaces += 1,
            '\t' => spaces += 2,
            _ => break,
        }
    }
    spaces / 2
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// `true` if `line` looks like a table row (contains a `|` and is non-blank).
fn looks_like_table_row(line: &str) -> bool {
    let t = line.trim();
    t.contains('|') && !t.is_empty()
}

/// `true` if `line` is a table separator row: cells of only `-`, `:`, spaces,
/// separated by pipes (and at least one `-`).
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('|') && !t.contains('-') {
        return false;
    }
    let body = t.trim_matches('|');
    if body.is_empty() {
        return false;
    }
    let mut saw_dash = false;
    for cell in body.split('|') {
        let cell = cell.trim();
        if cell.is_empty() {
            return false;
        }
        for c in cell.chars() {
            match c {
                '-' => saw_dash = true,
                ':' => {}
                _ => return false,
            }
        }
    }
    saw_dash
}

/// Split a table row on unescaped `|`, trimming and parsing each cell's inline
/// spans. Leading/trailing pipes are ignored.
fn parse_table_row(line: &str) -> Vec<Vec<Span>> {
    let t = line.trim().trim_matches('|');
    split_cells(t)
        .into_iter()
        .map(|c| parse_inline(c.trim()))
        .collect()
}

/// Split a table-row body on `|`, honoring `\|` escapes.
fn split_cells(s: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&n) = chars.peek() {
                cur.push(n);
                chars.next();
                continue;
            }
        }
        if c == '|' {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    cells.push(cur);
    cells
}

// ---------------------------------------------------------------------------
// Inline parsing
// ---------------------------------------------------------------------------

/// Parse a run of inline markdown text into [`Span`]s. Handles `**bold**`,
/// `*italic*`/`_italic_`, `` `code` ``, `~~strike~~`, `[text](url)`, and `\`
/// escapes. Unterminated / malformed markers degrade to plain text.
pub fn parse_inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let spans = parse_inline_chars(&chars);
    if spans.is_empty() {
        vec![Span::Text(String::new())]
    } else {
        spans
    }
}

/// Inline parse over a char slice, coalescing adjacent plain text.
fn parse_inline_chars(chars: &[char]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    let flush = |buf: &mut String, out: &mut Vec<Span>| {
        if !buf.is_empty() {
            out.push(Span::Text(std::mem::take(buf)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        // Backslash escape: the next char is literal.
        if c == '\\' && i + 1 < chars.len() {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        // Inline code: `…` (verbatim, highest precedence).
        if c == '`' {
            if let Some((code, next)) = scan_code(chars, i) {
                flush(&mut buf, &mut out);
                out.push(Span::Code(code));
                i = next;
                continue;
            }
        }
        // Link: [text](url)
        if c == '[' {
            if let Some((text_spans, url, next)) = scan_link(chars, i) {
                flush(&mut buf, &mut out);
                out.push(Span::Link { text: text_spans, url });
                i = next;
                continue;
            }
        }
        // Strikethrough: ~~…~~
        if c == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            if let Some((inner, next)) = scan_delim(chars, i, "~~") {
                flush(&mut buf, &mut out);
                out.push(Span::Strike(parse_inline_chars(inner)));
                i = next;
                continue;
            }
        }
        // Bold: **…** or __…__
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            let delim = if c == '*' { "**" } else { "__" };
            if let Some((inner, next)) = scan_delim(chars, i, delim) {
                flush(&mut buf, &mut out);
                out.push(Span::Bold(parse_inline_chars(inner)));
                i = next;
                continue;
            }
        }
        // Italic: *…* or _…_
        if c == '*' || c == '_' {
            let delim = if c == '*' { "*" } else { "_" };
            if let Some((inner, next)) = scan_delim(chars, i, delim) {
                flush(&mut buf, &mut out);
                out.push(Span::Italic(parse_inline_chars(inner)));
                i = next;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

/// Scan a `` `code` `` span starting at the backtick at `start`. Supports a run
/// of N backticks as the delimiter. Returns `(code, index after closing run)`.
fn scan_code(chars: &[char], start: usize) -> Option<(String, usize)> {
    let open = chars[start..].iter().take_while(|c| **c == '`').count();
    let content_start = start + open;
    let mut i = content_start;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = chars[i..].iter().take_while(|c| **c == '`').count();
            if run == open {
                let code: String = chars[content_start..i].iter().collect();
                // Trim one leading/trailing space (CommonMark code-span rule).
                let code = code.strip_prefix(' ').unwrap_or(&code);
                let code = code.strip_suffix(' ').unwrap_or(code);
                return Some((code.to_string(), i + run));
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Scan a `[text](url)` link starting at `[` at `start`. Returns the parsed
/// display spans, the url, and the index after the closing `)`.
fn scan_link(chars: &[char], start: usize) -> Option<(Vec<Span>, String, usize)> {
    // Find the matching `]` (no nested brackets handling beyond escapes).
    let mut i = start + 1;
    let mut depth = 1;
    let text_start = i;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if i >= chars.len() || chars[i] != ']' {
        return None;
    }
    let text_end = i;
    // Must be followed by `(`.
    if i + 1 >= chars.len() || chars[i + 1] != '(' {
        return None;
    }
    let url_start = i + 2;
    let mut j = url_start;
    while j < chars.len() && chars[j] != ')' {
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    let text_spans = parse_inline_chars(&chars[text_start..text_end]);
    let url: String = chars[url_start..j].iter().collect();
    Some((text_spans, url, j + 1))
}

/// Scan a delimited span (`delim` is `*`, `_`, `**`, `__`, or `~~`) starting at
/// `start`. Returns the inner char slice + the index after the closing delim.
/// `None` if there is no non-empty match before EOL.
fn scan_delim<'a>(chars: &'a [char], start: usize, delim: &str) -> Option<(&'a [char], usize)> {
    let d: Vec<char> = delim.chars().collect();
    let n = d.len();
    let content_start = start + n;
    if content_start >= chars.len() {
        return None;
    }
    // Don't open on whitespace right after the delimiter (e.g. `* not italic`).
    if chars[content_start].is_whitespace() {
        return None;
    }
    let single = d[0];
    let mut i = content_start;
    while i + n <= chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        // For a SINGLE-char emphasis delimiter (`*`/`_`), a doubled run (`**`) is a
        // BOLD marker, not a single-emphasis closer — skip the whole run so
        // `*a **b** c*` closes at the final lone `*`, not inside the bold.
        if n == 1 && chars[i] == single {
            let run = chars[i..].iter().take_while(|c| **c == single).count();
            if run >= 2 {
                i += run;
                continue;
            }
        }
        if chars[i..i + n] == d[..]
            // The closing delimiter must not be immediately preceded by space
            // and (for `*`/`_`) the inner content is non-empty.
            && i > content_start
            && !chars[i - 1].is_whitespace()
        {
            return Some((&chars[content_start..i], i + n));
        }
        i += 1;
    }
    None
}

/// Flatten spans to their plain text (used for measuring / tests / link tooltip).
pub fn spans_text(spans: &[Span]) -> String {
    let mut s = String::new();
    for span in spans {
        match span {
            Span::Text(t) => s.push_str(t),
            Span::Code(t) => s.push_str(t),
            Span::Bold(inner) | Span::Italic(inner) | Span::Strike(inner) => {
                s.push_str(&spans_text(inner))
            }
            Span::Link { text, .. } => s.push_str(&spans_text(text)),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_all_levels() {
        for lvl in 1u8..=6 {
            let src = format!("{} Title", "#".repeat(lvl as usize));
            let blocks = parse(&src);
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                Block::Heading { level, spans } => {
                    assert_eq!(*level, lvl);
                    assert_eq!(spans_text(spans), "Title");
                }
                other => panic!("expected heading, got {other:?}"),
            }
        }
        // 7 hashes is NOT a heading.
        assert!(matches!(parse("####### x")[0], Block::Paragraph(_)));
        // No space after the hashes => not a heading.
        assert!(matches!(parse("#notaheading")[0], Block::Paragraph(_)));
        // Trailing hashes are stripped.
        if let Block::Heading { spans, .. } = &parse("## Title ##")[0] {
            assert_eq!(spans_text(spans), "Title");
        } else {
            panic!("expected heading");
        }
    }

    #[test]
    fn inline_bold_italic_code_strike() {
        let spans = parse_inline("a **b** c *d* `e` ~~f~~");
        // Coalesced text + the formatted runs.
        assert!(spans.iter().any(|s| matches!(s, Span::Bold(b) if spans_text(b) == "b")));
        assert!(spans.iter().any(|s| matches!(s, Span::Italic(it) if spans_text(it) == "d")));
        assert!(spans.iter().any(|s| matches!(s, Span::Code(c) if c == "e")));
        assert!(spans.iter().any(|s| matches!(s, Span::Strike(st) if spans_text(st) == "f")));
    }

    #[test]
    fn inline_underscore_emphasis() {
        let spans = parse_inline("__bold__ and _italic_");
        assert!(spans.iter().any(|s| matches!(s, Span::Bold(b) if spans_text(b) == "bold")));
        assert!(spans.iter().any(|s| matches!(s, Span::Italic(it) if spans_text(it) == "italic")));
    }

    #[test]
    fn inline_nesting_bold_within_italic() {
        // *outer **inner** outer*
        let spans = parse_inline("*a **b** c*");
        assert_eq!(spans.len(), 1);
        match &spans[0] {
            Span::Italic(inner) => {
                assert!(inner.iter().any(|s| matches!(s, Span::Bold(b) if spans_text(b) == "b")));
                assert_eq!(spans_text(inner), "a b c");
            }
            other => panic!("expected italic, got {other:?}"),
        }
    }

    #[test]
    fn inline_escaping() {
        let spans = parse_inline(r"a \*not italic\* b");
        assert_eq!(spans_text(&spans), "a *not italic* b");
        assert!(!spans.iter().any(|s| matches!(s, Span::Italic(_))));
    }

    #[test]
    fn inline_link() {
        let spans = parse_inline("see [the docs](https://example.com/x) now");
        let link = spans.iter().find_map(|s| match s {
            Span::Link { text, url } => Some((spans_text(text), url.clone())),
            _ => None,
        });
        assert_eq!(link, Some(("the docs".to_string(), "https://example.com/x".to_string())));
    }

    #[test]
    fn inline_code_is_verbatim() {
        // Markers inside code are NOT parsed.
        let spans = parse_inline("`a *b* c`");
        assert_eq!(spans.len(), 1);
        assert!(matches!(&spans[0], Span::Code(c) if c == "a *b* c"));
    }

    #[test]
    fn unordered_list_with_nesting() {
        let src = "- one\n- two\n  - nested\n- three";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::List { ordered, items } => {
                assert!(!ordered);
                assert_eq!(items.len(), 4);
                assert_eq!(items[0].depth, 0);
                assert_eq!(spans_text(&items[0].spans), "one");
                assert_eq!(items[2].depth, 1); // nested by 2-space indent
                assert_eq!(spans_text(&items[2].spans), "nested");
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn ordered_list_keeps_numbers() {
        let src = "1. first\n2. second\n3) third";
        let blocks = parse(src);
        match &blocks[0] {
            Block::List { ordered, items } => {
                assert!(ordered);
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].number, Some(1));
                assert_eq!(items[2].number, Some(3));
                assert_eq!(spans_text(&items[1].spans), "second");
            }
            other => panic!("expected ordered list, got {other:?}"),
        }
    }

    #[test]
    fn fenced_code_block_with_language() {
        let src = "```rust\nfn main() {}\nlet x = 1;\n```";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::CodeBlock { lang, lines } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(lines, &["fn main() {}".to_string(), "let x = 1;".to_string()]);
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn fenced_code_unterminated_closes_at_eof() {
        let src = "```\nno close here\nstill code";
        let blocks = parse(src);
        match &blocks[0] {
            Block::CodeBlock { lang, lines } => {
                assert!(lang.is_none());
                assert_eq!(lines.len(), 2);
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn blockquote_with_inline() {
        let src = "> quoted **bold** text\n> second line";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Quote(inner) => {
                assert_eq!(inner.len(), 1);
                match &inner[0] {
                    Block::Paragraph(spans) => {
                        assert!(spans.iter().any(|s| matches!(s, Span::Bold(_))));
                        assert_eq!(spans_text(spans), "quoted bold text second line");
                    }
                    other => panic!("expected paragraph in quote, got {other:?}"),
                }
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn horizontal_rules() {
        for src in ["---", "***", "___", "- - -", "*****"] {
            let blocks = parse(src);
            assert_eq!(blocks.len(), 1, "src {src:?}");
            assert!(matches!(blocks[0], Block::Rule), "src {src:?}");
        }
    }

    #[test]
    fn pipe_table() {
        let src = "| Name | Age |\n|------|-----|\n| Ann | 3 |\n| Bo | 40 |";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Table { header, rows } => {
                assert_eq!(header.len(), 2);
                assert_eq!(spans_text(&header[0]), "Name");
                assert_eq!(rows.len(), 2);
                assert_eq!(spans_text(&rows[1][0]), "Bo");
                assert_eq!(spans_text(&rows[0][1]), "3");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn messy_input_is_robust() {
        // Stray markers, an unterminated bold, a lone bracket, blank lines, a
        // heading with no text — must not panic and should still produce blocks.
        let src = "\
#\n\n\
**unterminated bold\n\n\
some [half link without paren\n\n\
*  not italic (space after star)\n\n\
text with a lone ~ tilde and a | pipe\n";
        let blocks = parse(src);
        assert!(!blocks.is_empty());
        // The "**unterminated" line stays as a paragraph with the literal stars.
        assert!(blocks.iter().any(|b| matches!(b, Block::Paragraph(s) if spans_text(s).contains("unterminated bold"))));
    }

    #[test]
    fn small_document_block_model() {
        let src = "\
# Title\n\
\n\
Intro paragraph with **bold** and a [link](http://x).\n\
\n\
## Section\n\
\n\
- a\n\
- b\n\
\n\
```js\n\
const x = 1;\n\
```\n\
\n\
> a quote\n\
\n\
---\n";
        let blocks = parse(src);
        // Heading, paragraph, heading, list, code, quote, rule.
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::Heading { level: 2, .. }));
        assert!(matches!(blocks[3], Block::List { ordered: false, .. }));
        assert!(matches!(blocks[4], Block::CodeBlock { .. }));
        assert!(matches!(blocks[5], Block::Quote(_)));
        assert!(matches!(blocks[6], Block::Rule));
    }
}
