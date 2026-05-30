//! AI copilot — chat panel + streaming Anthropic Messages API client.
//!
//! Like every other capability in this IDE, ALL the AI logic lives shim-side
//! (Rust) and is exposed to Mighty via the scalar `mui_*` ABI (see
//! [`crate::abi`] for the philosophy). Mighty opens the panel, routes keystrokes
//! into the input buffer, calls [`mui_ai_send`] to fire a request, polls
//! [`mui_ai_pump`] each frame to drain the stream into the transcript, and calls
//! [`mui_ai_draw`] to render it.
//!
//! ## Backend
//!
//! The request runs on a **background thread** so the UI never blocks. The
//! thread POSTs to the Anthropic Messages API with `stream:true`, parses the
//! SSE event stream (`event: content_block_delta` → `data: {...delta.text}`),
//! and pushes text deltas into a [`SharedStream`] the UI polls each frame.
//! Errors (no key, non-2xx, network) land in the same shared buffer so the panel
//! can surface them. The provider call is factored behind [`Provider`] so a
//! second backend could be added; we ship Anthropic.
//!
//! ## Cost discipline
//!
//! `max_tokens` is intentionally modest (1024). The SSE parser + request builder
//! are unit-tested with SAMPLE data (no network) in `tests.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::ffi::MuiColor;
use crate::layout;
use crate::theme;

/// The Anthropic model id. A `const` so it is trivial to change. If this 400s as
/// an unknown model, fall back to `"claude-3-5-sonnet-latest"` (the API error
/// text is surfaced in the panel so it is debuggable).
pub const MODEL: &str = "claude-sonnet-4-6";

/// Conservative output cap to be cost-conscious.
pub const MAX_TOKENS: u32 = 1024;

/// Anthropic Messages endpoint.
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Read the API key from the environment (`ANTHROPIC_API_KEY`, then the legacy
/// `CLAUDE_API_KEY` fallback). `None` when neither is set or both are blank.
pub fn api_key() -> Option<String> {
    for var in ["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Who authored a transcript turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One turn in the chat transcript.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

/// The text-delta stream shared between the background request thread and the
/// UI thread. The thread appends deltas / sets an error / marks done; the UI
/// drains `delta` each frame via [`mui_ai_pump`].
#[derive(Default)]
pub struct StreamInner {
    /// Newly-arrived assistant text not yet folded into the transcript.
    pub delta: String,
    /// `true` once the request finished (message_stop, error, or thread exit).
    pub done: bool,
    /// An error message to surface in the panel (non-2xx body, network, etc.).
    pub error: Option<String>,
}

/// A handle to the shared stream + a "running" flag the UI reads cheaply.
#[derive(Clone, Default)]
pub struct SharedStream {
    pub inner: Arc<Mutex<StreamInner>>,
    pub running: Arc<AtomicBool>,
}

impl SharedStream {
    fn push_delta(&self, s: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.delta.push_str(s);
        }
    }
    fn set_error(&self, e: String) {
        if let Ok(mut g) = self.inner.lock() {
            g.error = Some(e);
        }
    }
    fn finish(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.done = true;
        }
        self.running.store(false, Ordering::SeqCst);
    }
}

// ===========================================================================
// Request building (vendor-agnostic surface + the Anthropic shape)
// ===========================================================================

/// A provider-neutral chat request: the system prompt + alternating turns.
pub struct ChatRequest {
    pub system: String,
    pub turns: Vec<Turn>,
    pub model: String,
    pub max_tokens: u32,
}

/// Build the system prompt, optionally embedding the active file's content (and
/// a selection, for inline edits) so the assistant can answer about the open
/// code. Kept pure + testable.
pub fn build_system_prompt(file_name: &str, file_content: &str, selection: &str) -> String {
    let mut s = String::from(
        "You are an AI coding copilot embedded in the Mighty IDE, helping the user \
         with the Mighty programming language and their open project. Be concise. \
         When you show code, wrap it in fenced ``` code blocks.",
    );
    if !file_name.is_empty() {
        s.push_str(&format!("\n\nThe user's active file is `{file_name}`."));
    }
    if !file_content.trim().is_empty() {
        // Cap the embedded file so we stay cost-conscious on large files.
        let capped = cap_chars(file_content, 8000);
        s.push_str("\n\nActive file content:\n```\n");
        s.push_str(&capped);
        s.push_str("\n```");
    }
    if !selection.trim().is_empty() {
        let capped = cap_chars(selection, 4000);
        s.push_str("\n\nThe user has selected this region (relevant to their request):\n```\n");
        s.push_str(&capped);
        s.push_str("\n```");
    }
    s
}

/// Truncate to at most `max` chars on a char boundary (no panics on UTF-8).
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n… (truncated)");
    out
}

/// Serialize a [`ChatRequest`] into the Anthropic Messages JSON body
/// (`{model, max_tokens, system, stream, messages:[{role, content}]}`). Pure +
/// testable (no network). Only `user`/`assistant` roles are emitted; an empty
/// assistant placeholder turn (the one currently streaming) is skipped.
pub fn anthropic_body(req: &ChatRequest) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .turns
        .iter()
        .filter(|t| !(t.role == Role::Assistant && t.text.is_empty()))
        .map(|t| {
            let role = match t.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            serde_json::json!({ "role": role, "content": t.text })
        })
        .collect();
    serde_json::json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "system": req.system,
        "stream": true,
        "messages": messages,
    })
}

// ===========================================================================
// SSE parsing
// ===========================================================================

/// Incremental Server-Sent-Events parser for the Anthropic stream. The transport
/// hands us arbitrary byte chunks (an event may be split across reads, or two
/// events may arrive in one read); we buffer and emit complete `data:` JSON
/// lines, extracting the `delta.text` from `content_block_delta` events.
#[derive(Default)]
pub struct SseParser {
    buf: String,
}

/// One thing the parser extracted from the stream.
#[derive(Debug, PartialEq, Eq)]
pub enum SseEvent {
    /// A text delta to append to the streaming assistant turn.
    Text(String),
    /// `message_stop` — the response is complete.
    Stop,
    /// An `error` event from the API (its message).
    Error(String),
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes; returns any complete events found. Safe to call
    /// with partial data — the remainder is buffered for the next call.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buf.push_str(chunk);
        let mut events = Vec::new();
        // SSE lines are newline-delimited; process every complete line and keep
        // the trailing partial line in `buf`.
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf[..nl].trim_end_matches('\r').to_string();
            self.buf.drain(..=nl);
            if let Some(ev) = parse_data_line(&line) {
                events.push(ev);
            }
        }
        events
    }
}

/// Parse a single SSE line. We only act on `data:` lines (the `event:` lines are
/// advisory; the `type` field inside the JSON is authoritative). Blank lines and
/// `event:`/`:` lines yield `None`.
fn parse_data_line(line: &str) -> Option<SseEvent> {
    let rest = line.strip_prefix("data:")?.trim();
    if rest.is_empty() || rest == "[DONE]" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(rest).ok()?;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("content_block_delta") => {
            let t = v.get("delta")?.get("text")?.as_str()?;
            Some(SseEvent::Text(t.to_string()))
        }
        Some("message_stop") => Some(SseEvent::Stop),
        Some("error") => {
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown API error");
            Some(SseEvent::Error(msg.to_string()))
        }
        _ => None,
    }
}

// ===========================================================================
// Provider trait + Anthropic implementation (background streaming)
// ===========================================================================

/// A streaming chat provider. Structured so another backend could be slotted in
/// behind the same shared-stream contract; we ship Anthropic.
pub trait Provider: Send {
    /// Run `req` to completion, pushing text deltas / errors into `stream`.
    /// Called on a background thread; must not block the UI.
    fn stream(&self, req: ChatRequest, stream: SharedStream);
}

/// The Anthropic Messages provider (BYO key from the environment).
pub struct AnthropicProvider {
    pub api_key: String,
}

impl Provider for AnthropicProvider {
    fn stream(&self, req: ChatRequest, stream: SharedStream) {
        let body = anthropic_body(&req);
        let resp = ureq::post(ENDPOINT)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", ANTHROPIC_VERSION)
            .set("content-type", "application/json")
            .send_json(body);

        match resp {
            Ok(r) => {
                let mut reader = r.into_reader();
                let mut parser = SseParser::new();
                let mut buf = [0u8; 4096];
                use std::io::Read;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]);
                            for ev in parser.feed(&chunk) {
                                match ev {
                                    SseEvent::Text(t) => stream.push_delta(&t),
                                    SseEvent::Stop => {
                                        stream.finish();
                                        return;
                                    }
                                    SseEvent::Error(e) => {
                                        stream.set_error(e);
                                        stream.finish();
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            stream.set_error(format!("stream read error: {e}"));
                            break;
                        }
                    }
                }
                stream.finish();
            }
            // ureq surfaces non-2xx as Error::Status with the response body.
            Err(ureq::Error::Status(code, r)) => {
                let body = r
                    .into_string()
                    .unwrap_or_else(|_| "(no response body)".to_string());
                stream.set_error(format!("API error {code}: {}", body.trim()));
                stream.finish();
            }
            Err(e) => {
                stream.set_error(format!("request failed: {e}"));
                stream.finish();
            }
        }
    }
}

// ===========================================================================
// Panel state (transcript + input + live stream)
// ===========================================================================

/// The AI copilot panel's full state: whether it is open, the transcript, the
/// multi-line input buffer, the scroll offset, and the active background stream.
pub struct AiPanel {
    pub open: bool,
    pub transcript: Vec<Turn>,
    pub input: String,
    /// Vertical scroll offset in pixels (0 = pinned to the latest content).
    pub scroll: f32,
    /// The active stream, if a request is in flight.
    stream: Option<SharedStream>,
    /// Screenshot/demo hook (`MUI_AI_AUTOOPEN`): render the transcript even when
    /// no API key is set, so a headless capture shows the chat UI. Never set on
    /// a normal launch (the no-key state shows instead).
    pub force_transcript: bool,
}

impl Default for AiPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AiPanel {
    pub fn new() -> Self {
        AiPanel {
            open: false,
            transcript: Vec::new(),
            input: String::new(),
            scroll: 0.0,
            stream: None,
            force_transcript: false,
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.running.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Append a finished user turn + an empty assistant turn to stream into, and
    /// kick off the background request. `system` already embeds file context.
    /// No-op (returns `false`) when the input is blank, a request is in flight,
    /// or no API key is set.
    pub fn send(&mut self, system: String) -> bool {
        if self.is_streaming() {
            return false;
        }
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            return false;
        }
        let Some(key) = api_key() else {
            return false;
        };
        self.input.clear();
        self.transcript.push(Turn {
            role: Role::User,
            text: prompt,
        });
        // The empty assistant turn the stream fills in live.
        self.transcript.push(Turn {
            role: Role::Assistant,
            text: String::new(),
        });
        self.scroll = 0.0;

        let req = ChatRequest {
            system,
            turns: self.transcript.clone(),
            model: MODEL.to_string(),
            max_tokens: MAX_TOKENS,
        };
        let stream = SharedStream::default();
        stream.running.store(true, Ordering::SeqCst);
        self.stream = Some(stream.clone());

        let provider = AnthropicProvider { api_key: key };
        std::thread::spawn(move || {
            provider.stream(req, stream);
        });
        true
    }

    /// Drain any pending stream deltas/errors into the last (assistant) turn.
    /// Returns `true` if the transcript changed (so the UI redraws). Called each
    /// frame by [`mui_ai_pump`].
    pub fn pump(&mut self) -> bool {
        let Some(stream) = self.stream.clone() else {
            return false;
        };
        let (delta, err, done) = {
            let Ok(mut g) = stream.inner.lock() else {
                return false;
            };
            let delta = std::mem::take(&mut g.delta);
            (delta, g.error.take(), g.done)
        };
        let mut changed = false;
        if !delta.is_empty() {
            if let Some(last) = self.transcript.last_mut() {
                if last.role == Role::Assistant {
                    last.text.push_str(&delta);
                    changed = true;
                }
            }
        }
        if let Some(e) = err {
            if let Some(last) = self.transcript.last_mut() {
                if last.role == Role::Assistant {
                    if !last.text.is_empty() {
                        last.text.push('\n');
                    }
                    last.text.push_str(&format!("[error] {e}"));
                    changed = true;
                }
            }
        }
        if done {
            self.stream = None;
            changed = true;
        }
        changed
    }
}

// ===========================================================================
// Rendering (Vivid Modern, right-docked panel)
// ===========================================================================

/// Width of the right-docked AI panel (px).
pub const AI_PANEL_W: f32 = 360.0;

/// A simple line of rendered transcript content (already wrapped/segmented).
enum Seg {
    /// A normal text line for `role`.
    Line { role: Role, text: String },
    /// A code line (monospace, inside a code card). `first`/`last` mark the card
    /// rounding rows.
    Code { text: String, first: bool, last: bool },
    /// Vertical gap before a new turn.
    Gap,
}

/// Word-wrap `text` to `max_chars` per line (greedy). Preserves explicit
/// newlines. Used for both prose and code.
fn wrap(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(8);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.chars().count() <= max {
            out.push(raw.to_string());
            continue;
        }
        // Greedy word wrap; fall back to hard-splitting an over-long token.
        let mut cur = String::new();
        for word in raw.split(' ') {
            let wlen = word.chars().count();
            if wlen > max {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut chunk = String::new();
                for ch in word.chars() {
                    if chunk.chars().count() >= max {
                        out.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(ch);
                }
                if !chunk.is_empty() {
                    cur = chunk;
                }
                continue;
            }
            let sep = if cur.is_empty() { 0 } else { 1 };
            if cur.chars().count() + sep + wlen > max {
                out.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
        }
        out.push(cur);
    }
    out
}

/// Flatten the transcript into wrapped segments, splitting fenced ``` code
/// blocks out so they can render in a monospace card (markdown-ish).
fn segments(transcript: &[Turn], prose_cols: usize, code_cols: usize) -> Vec<Seg> {
    let mut segs = Vec::new();
    for (i, turn) in transcript.iter().enumerate() {
        if i > 0 {
            segs.push(Seg::Gap);
        }
        let mut in_code = false;
        let mut code_run: Vec<String> = Vec::new();
        let flush_code = |segs: &mut Vec<Seg>, run: &mut Vec<String>| {
            let n = run.len();
            for (j, l) in run.drain(..).enumerate() {
                segs.push(Seg::Code {
                    text: l,
                    first: j == 0,
                    last: j + 1 == n,
                });
            }
        };
        for line in turn.text.split('\n') {
            if line.trim_start().starts_with("```") {
                if in_code {
                    flush_code(&mut segs, &mut code_run);
                    in_code = false;
                } else {
                    in_code = true;
                }
                continue;
            }
            if in_code {
                for w in wrap(line, code_cols) {
                    code_run.push(w);
                }
            } else {
                for w in wrap(line, prose_cols) {
                    segs.push(Seg::Line {
                        role: turn.role,
                        text: w,
                    });
                }
            }
        }
        if in_code {
            flush_code(&mut segs, &mut code_run);
        }
    }
    segs
}

impl AiPanel {
    /// Draw the right-docked AI panel: header, scrollable transcript (distinct
    /// user/assistant styling, code cards), a streaming indicator, and the
    /// multi-line input box with a send affordance. No-op when closed.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.open {
            return;
        }
        let w = width as f32;
        let h = height as f32;
        let pw = AI_PANEL_W;
        let px = w - pw;
        let clip = Some((px as u32, 0, pw as u32, height));
        let chrome = theme::CHROME_FONT_SIZE;
        use crate::icons;

        // Panel surface + left divider.
        ctx.dl_rect(px, 0.0, pw, h, theme::BG_2());
        ctx.dl_rect(px, 0.0, 1.0, h, theme::BORDER());

        // ---- header band: sparkles icon + "AI COPILOT" + model pill ----
        let head_h = 40.0;
        ctx.dl_rect(px, 0.0, pw, head_h, theme::BG_2());
        ctx.dl_rect(px, head_h - 1.0, pw, 1.0, theme::BORDER_SOFT());
        ctx.dl_icon(
            px + 14.0,
            (head_h - 16.0) * 0.5,
            16.0,
            16.0,
            icons::AGENTS,
            theme::ACCENT_BRIGHT(),
            1.6,
            false,
        );
        ctx.dl_icon(
            px + 14.0,
            (head_h - 16.0) * 0.5,
            16.0,
            16.0,
            icons::AGENTS_DOT,
            theme::ACCENT_BRIGHT(),
            0.0,
            true,
        );
        let title: String = "AI COPILOT".chars().flat_map(|c| [c, '\u{2009}']).collect();
        ctx.text.queue_ui_sized(
            px + 38.0,
            (head_h - (chrome - 2.0)) * 0.5 - 1.0,
            &title,
            theme::TEXT_1(),
            chrome - 2.0,
            clip,
        );
        // Model pill on the right.
        let model_label = MODEL;
        let pill_w = model_label.chars().count() as f32 * (chrome - 3.0) * 0.52 + 16.0;
        let pill_x = px + pw - pill_w - 12.0;
        let pill_y = (head_h - 18.0) * 0.5;
        ctx.dl_round(pill_x, pill_y, pill_w, 18.0, 9.0, theme::accent_a(0.12));
        ctx.dl_stroke(pill_x, pill_y, pill_w, 18.0, 9.0, theme::ACCENT_LINE(), 1.0);
        ctx.text.queue_ui_sized(
            pill_x + 8.0,
            pill_y + 4.0,
            model_label,
            theme::ACCENT_BRIGHT(),
            chrome - 3.0,
            clip,
        );

        // ---- input box at the bottom ----
        let input_pad = 10.0;
        let input_lines = wrap(&self.input, ((pw - 56.0) / (chrome * 0.55)) as usize);
        let n_in = input_lines.len().max(1) as f32;
        let input_h = (n_in * layout::LINE_H).min(120.0) + 16.0;
        let input_y = h - input_h - input_pad;
        let body_top = head_h + 6.0;
        let body_bottom = input_y - 8.0;

        // No-key state: a clear message instead of a transcript / live call.
        // (The screenshot/demo hook forces the transcript so the chat UI renders
        // without a key — never set on a normal launch.)
        if api_key().is_none() && !self.force_transcript {
            let msg_y = body_top + 30.0;
            ctx.dl_icon(
                px + 18.0,
                msg_y,
                18.0,
                18.0,
                icons::INFO_I,
                theme::WARNING(),
                1.5,
                false,
            );
            for (i, line) in [
                "Set ANTHROPIC_API_KEY",
                "to enable the AI copilot.",
                "",
                "(CLAUDE_API_KEY also works.)",
            ]
            .iter()
            .enumerate()
            {
                ctx.text.queue_ui_sized(
                    px + 18.0,
                    msg_y + 28.0 + i as f32 * 20.0,
                    line,
                    theme::TEXT_1(),
                    chrome,
                    clip,
                );
            }
        } else {
            self.draw_transcript(ctx, px, pw, body_top, body_bottom, clip);
        }

        // Input box surface.
        ctx.dl_round(
            px + 10.0,
            input_y,
            pw - 20.0,
            input_h,
            8.0,
            theme::BG_1(),
        );
        ctx.dl_stroke(
            px + 10.0,
            input_y,
            pw - 20.0,
            input_h,
            8.0,
            theme::ACCENT_LINE(),
            1.0,
        );
        if self.input.is_empty() {
            ctx.text.queue_ui_sized(
                px + 20.0,
                input_y + 9.0,
                "Ask about your code…  (Enter to send)",
                theme::TEXT_3(),
                chrome,
                clip,
            );
        } else {
            for (i, line) in input_lines.iter().enumerate() {
                ctx.text.queue_sized(
                    px + 20.0,
                    input_y + 9.0 + i as f32 * layout::LINE_H,
                    line,
                    theme::TEXT(),
                    chrome,
                    clip,
                );
            }
        }
        // Send affordance (paper-plane-ish arrow) bottom-right of the input.
        let send_col = if self.is_streaming() {
            theme::TEXT_3()
        } else {
            theme::ACCENT_BRIGHT()
        };
        ctx.dl_icon(
            px + pw - 34.0,
            input_y + input_h - 26.0,
            16.0,
            16.0,
            "M4 12l16-8-6 16-3-7-7-1z",
            send_col,
            1.6,
            false,
        );

        // ---- streaming indicator just above the input ----
        if self.is_streaming() {
            let dot_y = input_y - 22.0;
            ctx.dl_icon(
                px + 18.0,
                dot_y,
                14.0,
                14.0,
                icons::AGENTS_DOT,
                theme::ACCENT_BRIGHT(),
                0.0,
                true,
            );
            ctx.text.queue_ui_sized(
                px + 38.0,
                dot_y,
                "thinking…",
                theme::ACCENT_BRIGHT(),
                chrome - 1.0,
                clip,
            );
        }
    }

    /// Draw the wrapped transcript inside the body band, distinct styling per
    /// role + monospace code cards. Pinned to the bottom (latest content) minus
    /// the scroll offset.
    #[allow(clippy::too_many_arguments)]
    fn draw_transcript(
        &self,
        ctx: &mut crate::MuiContext,
        px: f32,
        pw: f32,
        body_top: f32,
        body_bottom: f32,
        clip: Option<(u32, u32, u32, u32)>,
    ) {
        let chrome = theme::CHROME_FONT_SIZE;
        let row_h = 18.0_f32;
        let prose_cols = ((pw - 48.0) / (chrome * 0.55)) as usize;
        let code_cols = ((pw - 56.0) / (theme::CHAR_W)) as usize;

        if self.transcript.is_empty() {
            ctx.text.queue_ui_sized(
                px + 18.0,
                body_top + 16.0,
                "Ask the copilot about the open file.",
                theme::TEXT_3(),
                chrome,
                clip,
            );
            return;
        }

        let segs = segments(&self.transcript, prose_cols, code_cols);
        let total_h = segs
            .iter()
            .map(|s| match s {
                Seg::Gap => 10.0,
                _ => row_h,
            })
            .sum::<f32>();
        let band_h = body_bottom - body_top;

        // Pin to the bottom: start so the last line sits at body_bottom, then
        // apply the user scroll offset (scrolling up reveals earlier content).
        let mut y = if total_h <= band_h {
            body_top
        } else {
            body_bottom - total_h + self.scroll
        };

        for seg in &segs {
            match seg {
                Seg::Gap => {
                    y += 10.0;
                }
                Seg::Line { role, text } => {
                    if y + row_h >= body_top && y <= body_bottom {
                        let color = match role {
                            Role::User => theme::TEXT(),
                            Role::Assistant => theme::TEXT_1(),
                        };
                        // A subtle accent bar on user turns to distinguish them.
                        if *role == Role::User {
                            ctx.dl_rect(px + 12.0, y + 2.0, 2.5, row_h - 4.0, theme::ACCENT());
                        }
                        ctx.text.queue_ui_sized(
                            px + 20.0,
                            y,
                            text,
                            color,
                            chrome,
                            clip,
                        );
                    }
                    y += row_h;
                }
                Seg::Code { text, first, last } => {
                    if y + row_h >= body_top && y <= body_bottom {
                        // Code card background (monospace).
                        let cx = px + 16.0;
                        let cw = pw - 32.0;
                        ctx.dl_rect(cx, y - 1.0, cw, row_h + 2.0, theme::BG_1());
                        if *first {
                            ctx.dl_round(cx, y - 2.0, cw, 6.0, 5.0, theme::BG_1());
                        }
                        if *last {
                            ctx.dl_round(cx, y + row_h - 4.0, cw, 6.0, 5.0, theme::BG_1());
                        }
                        ctx.dl_rect(cx, y - 1.0, 2.5, row_h + 2.0, theme::accent_a(0.5));
                        ctx.text.queue_sized(
                            cx + 10.0,
                            y,
                            text,
                            theme::SYN_DEFAULT(),
                            chrome - 0.5,
                            clip,
                        );
                    }
                    y += row_h;
                }
            }
        }
    }
}

/// Color helper kept here so tests can assert role colors without touching the
/// global theme RwLock indirectly. (Unused by the renderer; documents intent.)
#[allow(dead_code)]
pub fn role_color(role: Role) -> MuiColor {
    match role {
        Role::User => theme::TEXT(),
        Role::Assistant => theme::TEXT_1(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- SSE parser: single complete event ----
    #[test]
    fn sse_single_text_delta() {
        let mut p = SseParser::new();
        let evs = p.feed(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        );
        assert_eq!(evs, vec![SseEvent::Text("Hello".to_string())]);
    }

    // ---- SSE parser: a single JSON event SPLIT across two reads ----
    #[test]
    fn sse_event_split_across_reads() {
        let mut p = SseParser::new();
        // First read ends mid-line (no newline yet) — nothing emitted.
        let e1 = p.feed("data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":");
        assert!(e1.is_empty());
        // Second read completes the line.
        let e2 = p.feed("\"text_delta\",\"text\":\"World\"}}\n");
        assert_eq!(e2, vec![SseEvent::Text("World".to_string())]);
    }

    // ---- SSE parser: MULTIPLE events in one chunk, then stop ----
    #[test]
    fn sse_multi_event_chunk_then_stop() {
        let mut p = SseParser::new();
        let chunk = "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"foo \"}}\n\
                     data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"bar\"}}\n\
                     data: {\"type\":\"message_stop\"}\n";
        let evs = p.feed(chunk);
        assert_eq!(
            evs,
            vec![
                SseEvent::Text("foo ".to_string()),
                SseEvent::Text("bar".to_string()),
                SseEvent::Stop,
            ]
        );
    }

    // ---- SSE parser: ping / non-text events are ignored ----
    #[test]
    fn sse_ignores_ping_and_blank() {
        let mut p = SseParser::new();
        let evs = p.feed("event: ping\ndata: {\"type\":\"ping\"}\n\n: keep-alive comment\n");
        assert!(evs.is_empty());
    }

    // ---- SSE parser: API error event surfaces its message ----
    #[test]
    fn sse_error_event() {
        let mut p = SseParser::new();
        let evs = p.feed(
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n",
        );
        assert_eq!(evs, vec![SseEvent::Error("Overloaded".to_string())]);
    }

    // ---- Request body shape ----
    #[test]
    fn request_body_shape() {
        let req = ChatRequest {
            system: "sys".to_string(),
            turns: vec![
                Turn { role: Role::User, text: "hi".to_string() },
                // The streaming placeholder (empty assistant) must be dropped.
                Turn { role: Role::Assistant, text: String::new() },
            ],
            model: "claude-test".to_string(),
            max_tokens: 1024,
        };
        let body = anthropic_body(&req);
        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["stream"], true);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "empty assistant placeholder is skipped");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hi");
    }

    #[test]
    fn system_prompt_embeds_file_and_selection() {
        let sys = build_system_prompt("main.mty", "fn main() {}", "main");
        assert!(sys.contains("main.mty"));
        assert!(sys.contains("fn main() {}"));
        assert!(sys.contains("selected"));
        // Empty file/selection → no context blocks.
        let bare = build_system_prompt("", "", "");
        assert!(!bare.contains("Active file content"));
        assert!(!bare.contains("selected"));
    }

    // ---- No-key path: send() refuses without a key, regardless of input ----
    #[test]
    fn no_key_send_is_noop() {
        // Ensure no key is visible to this test.
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("CLAUDE_API_KEY");
        assert!(api_key().is_none());
        let mut panel = AiPanel::new();
        panel.input = "anything".to_string();
        assert!(!panel.send("sys".to_string()), "send must no-op without a key");
        // Input is preserved (not cleared) and no turns were added.
        assert_eq!(panel.input, "anything");
        assert_eq!(panel.transcript.len(), 0);
    }

    // ---- Transcript state: pump folds deltas into the streaming turn ----
    #[test]
    fn pump_appends_deltas_to_assistant_turn() {
        let mut panel = AiPanel::new();
        panel.transcript.push(Turn { role: Role::User, text: "q".to_string() });
        panel.transcript.push(Turn { role: Role::Assistant, text: String::new() });
        // Wire a shared stream manually (no thread / network).
        let stream = SharedStream::default();
        stream.running.store(true, std::sync::atomic::Ordering::SeqCst);
        panel.stream = Some(stream.clone());

        stream.push_delta("Hel");
        assert!(panel.pump());
        stream.push_delta("lo");
        assert!(panel.pump());
        assert_eq!(panel.transcript[1].text, "Hello");
        assert!(panel.is_streaming());

        // Marking done clears the stream + flips streaming off.
        stream.finish();
        assert!(panel.pump());
        assert!(!panel.is_streaming());
        // A subsequent pump with no stream is a no-op.
        assert!(!panel.pump());
    }

    #[test]
    fn pump_surfaces_error_into_turn() {
        let mut panel = AiPanel::new();
        panel.transcript.push(Turn { role: Role::User, text: "q".to_string() });
        panel.transcript.push(Turn { role: Role::Assistant, text: String::new() });
        let stream = SharedStream::default();
        stream.running.store(true, std::sync::atomic::Ordering::SeqCst);
        panel.stream = Some(stream.clone());
        stream.set_error("API error 400: unknown model".to_string());
        stream.finish();
        assert!(panel.pump());
        assert!(panel.transcript[1].text.contains("API error 400"));
    }

    // ---- Code-block segmentation produces monospace Code segs ----
    #[test]
    fn segments_split_code_fences() {
        let turns = vec![Turn {
            role: Role::Assistant,
            text: "before\n```\nlet x = 1\n```\nafter".to_string(),
        }];
        let segs = segments(&turns, 80, 80);
        let codes = segs
            .iter()
            .filter(|s| matches!(s, Seg::Code { .. }))
            .count();
        let lines = segs
            .iter()
            .filter(|s| matches!(s, Seg::Line { .. }))
            .count();
        assert_eq!(codes, 1, "the one code line renders as a Code seg");
        assert_eq!(lines, 2, "before + after render as prose Lines");
    }

    // One tiny REAL end-to-end call (max_tokens 32, 1-line prompt). `#[ignore]`d
    // so it never runs in CI / a no-key environment and never loops; run with
    // `cargo test -p mighty-ui-sys ai::tests::live_smoke -- --ignored --nocapture`
    // when ANTHROPIC_API_KEY is set, to verify streaming end-to-end.
    #[test]
    #[ignore]
    fn live_smoke() {
        let Some(key) = api_key() else {
            eprintln!("live_smoke: no key set, skipping");
            return;
        };
        let req = ChatRequest {
            system: "Reply with exactly one short word.".to_string(),
            turns: vec![Turn { role: Role::User, text: "Say hi.".to_string() }],
            model: MODEL.to_string(),
            max_tokens: 32,
        };
        let stream = SharedStream::default();
        stream.running.store(true, std::sync::atomic::Ordering::SeqCst);
        AnthropicProvider { api_key: key }.stream(req, stream.clone());
        let g = stream.inner.lock().unwrap();
        eprintln!("live_smoke: delta={:?} error={:?}", g.delta, g.error);
        assert!(g.done);
        assert!(g.error.is_none() || g.delta.is_empty());
        assert!(!g.delta.is_empty() || g.error.is_some());
    }

    #[test]
    fn wrap_respects_width_and_newlines() {
        let lines = wrap("aaaa bbbb cccc", 9);
        assert!(lines.iter().all(|l| l.chars().count() <= 9));
        assert!(lines.len() >= 2);
        // Explicit newlines are preserved as separate lines.
        assert_eq!(wrap("a\nb", 80), vec!["a".to_string(), "b".to_string()]);
    }
}
