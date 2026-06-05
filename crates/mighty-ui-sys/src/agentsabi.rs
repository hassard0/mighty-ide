//! Mighty **Agents** panel — topology state, the Vello topology draw, and the
//! scalar `mui_agents_*` C ABI.
//!
//! The panel is rail slot 8 (`PANEL_AGENTS_MTY`). It renders the agent system
//! discovered by [`crate::agents`] as a structured topology TREE (Vivid-Modern
//! style): a *Protocols* section (each protocol → its messages), an *Agents*
//! section (each agent → an "implements <Proto>" edge row + its `on` handlers,
//! with an LLM badge for LLM-backed agents), a *Tools* section, and a
//! *Supervisors* section (each supervisor → its children). Clicking any node
//! with a definition jumps the editor there.
//!
//! Shim-owned + scalar ABI throughout (L17/L21): Mighty refreshes/draws, routes
//! rail clicks + row clicks, and runs the active program. The Run action and
//! the (best-effort) live inspector reuse [`crate::run::RunPanel`]'s
//! process-spawn/pump discipline.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agents::{self, AgentModel, RuntimeSnapshot};
use crate::layout;
use crate::theme;
use crate::MuiContext;

// ===========================================================================
// Display-node model (flattened topology rows)
// ===========================================================================

/// The kind of a topology display row. The scalar values are exposed over the
/// C ABI (`mui_agents_node_kind`) so Mighty / tests can branch on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A dim uppercase section header (Protocols / Agents / Tools / Supervisors).
    Section = 0,
    Protocol = 1,
    Message = 2,
    Agent = 3,
    Handler = 4,
    /// An "implements <Protocol>" relationship row under an agent.
    Implements = 5,
    Tool = 6,
    Supervisor = 7,
    /// A `child <name> = spawn <Type>` row under a supervisor.
    Child = 8,
    /// An LLM badge row note (rendered inline; rarely a standalone row).
    Llm = 9,
}

pub(crate) fn compact_agent_row_label(kind: NodeKind, name: &str) -> String {
    match kind {
        NodeKind::Implements => name
            .strip_prefix("implements ")
            .map(|proto| format!("impl {proto}"))
            .unwrap_or_else(|| name.to_string()),
        NodeKind::Message => compact_signature_label(name),
        _ => name.to_string(),
    }
}

fn compact_signature_label(sig: &str) -> String {
    let Some(open) = sig.find('(') else {
        return sig.to_string();
    };
    let Some(close_rel) = sig[open + 1..].find(')') else {
        return sig.to_string();
    };
    let close = open + 1 + close_rel;
    let params = &sig[open + 1..close];
    let compact_params = params
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.rsplit_once(':').map(|(_, ty)| ty.trim()).unwrap_or(p))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}({}){}", &sig[..open], compact_params, &sig[close + 1..])
}

pub(crate) fn fit_agent_row_label(
    text: &mut crate::text::Text,
    kind: NodeKind,
    name: &str,
    max_px: f32,
    size: f32,
) -> String {
    if max_px <= 0.0 {
        return String::new();
    }
    if text.measure_ui_sized(name, size).0 <= max_px {
        return name.to_string();
    }
    let compact = compact_agent_row_label(kind, name);
    if compact != name && text.measure_ui_sized(&compact, size).0 <= max_px {
        return compact;
    }
    fit_head_px(text, if compact.len() < name.len() { &compact } else { name }, max_px, size)
}

fn fit_head_px(text: &mut crate::text::Text, s: &str, max_px: f32, size: f32) -> String {
    if max_px <= 0.0 {
        return String::new();
    }
    if text.measure_ui_sized(s, size).0 <= max_px {
        return s.to_string();
    }
    let ellipsis = "\u{2026}";
    if text.measure_ui_sized(ellipsis, size).0 > max_px {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut candidate: String = chars.iter().take(mid).collect();
        candidate.push_str(ellipsis);
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
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

fn fit_sidebar_line(text: &mut crate::text::Text, s: &str, sidebar_w: f32, size: f32) -> String {
    let max_px = (sidebar_w - 28.0).max(0.0);
    fit_head_px(text, s, max_px, size)
}

fn header_run_rect(sidebar_right: f32) -> (f32, f32) {
    let x0 = (sidebar_right - 34.0).max(layout::RAIL_W);
    (x0, (sidebar_right - x0).max(0.0))
}

fn header_inspect_rect(sidebar_right: f32) -> (f32, f32) {
    let (run_x, _) = header_run_rect(sidebar_right);
    let x0 = (run_x - 24.0).max(layout::RAIL_W);
    (x0, (run_x - x0).max(0.0))
}

fn header_clear_rect(sidebar_right: f32) -> (f32, f32) {
    let (inspect_x, _) = header_inspect_rect(sidebar_right);
    let x0 = (inspect_x - 24.0).max(layout::RAIL_W);
    (x0, (inspect_x - x0).max(0.0))
}

fn header_rect_contains(rect: (f32, f32), x: f32, include_right: bool) -> bool {
    let (x0, w) = rect;
    if include_right {
        w > 0.0 && x >= x0 && x <= x0 + w
    } else {
        w > 0.0 && x >= x0 && x < x0 + w
    }
}

/// One flattened topology row: kind + display name + nesting depth + an optional
/// jump target (`file` + 0-based `line`). Rows with `line < 0` are not clickable
/// (section headers, the synthetic "implements" edge that points at a protocol
/// keeps its target line so it IS clickable).
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    pub name: String,
    pub depth: u32,
    pub file: PathBuf,
    pub line: i32,
    /// `true` for an LLM-backed agent (drives an inline badge on the Agent row).
    pub llm: bool,
}

impl Node {
    fn header(name: &str) -> Node {
        Node {
            kind: NodeKind::Section,
            name: name.to_string(),
            depth: 0,
            file: PathBuf::new(),
            line: -1,
            llm: false,
        }
    }
}

// ===========================================================================
// Topology state
// ===========================================================================

/// The Mighty Agents panel state: the discovered model, the flattened display
/// rows, scroll, the last clicked jump target, and an embedded [`RunPanel`] for
/// the Run action + the best-effort live inspector.
pub struct AgentTopology {
    model: AgentModel,
    nodes: Vec<Node>,
    /// Top visible row (scroll offset).
    first: usize,
    /// The root the last scan walked (for the header subtitle).
    root: Option<PathBuf>,
    /// The last clicked row's jump target (path + 0-based line), read by the IDE.
    click_target: Option<(PathBuf, i32)>,
    /// Embedded Run panel — `mty run <file>` on a background thread (reused so
    /// the Agents panel needs no duplicate spawn/pump code).
    run: crate::run::RunPanel,
    /// The last live snapshot from `mty inspect --json` (Unix / future Windows).
    snapshot: Option<RuntimeSnapshot>,
    /// A human-readable note about why live inspect is / isn't available.
    inspect_note: String,
}

impl Default for AgentTopology {
    fn default() -> Self {
        AgentTopology::new()
    }
}

impl AgentTopology {
    pub fn new() -> Self {
        AgentTopology {
            model: AgentModel::default(),
            nodes: Vec::new(),
            first: 0,
            root: None,
            click_target: None,
            run: crate::run::RunPanel::new(),
            snapshot: None,
            inspect_note: default_inspect_note(),
        }
    }

    /// Re-scan `root` for the agent system and rebuild the display rows. Returns
    /// the node count.
    pub fn refresh(&mut self, root: &Path) -> usize {
        self.model = agents::scan_project(root);
        self.root = Some(root.to_path_buf());
        self.rebuild();
        self.nodes.len()
    }

    /// Build the model from explicit source (single file) — used by tests + the
    /// screenshot seed.
    pub fn set_model(&mut self, model: AgentModel) {
        self.model = model;
        self.rebuild();
    }

    /// Flatten [`self.model`] into the display-row list (Protocols → Agents →
    /// Tools → Supervisors), each section omitted when empty.
    fn rebuild(&mut self) {
        let mut nodes = Vec::new();

        if !self.model.protocols.is_empty() {
            nodes.push(Node::header("PROTOCOLS"));
            for (file, p) in &self.model.protocols {
                nodes.push(Node {
                    kind: NodeKind::Protocol,
                    name: p.name.clone(),
                    depth: 1,
                    file: file.clone(),
                    line: p.line as i32,
                    llm: false,
                });
                for m in &p.messages {
                    nodes.push(Node {
                        kind: NodeKind::Message,
                        name: m.sig.clone(),
                        depth: 2,
                        file: file.clone(),
                        line: m.line as i32,
                        llm: false,
                    });
                }
            }
        }

        if !self.model.agents.is_empty() {
            nodes.push(Node::header("AGENTS"));
            for (file, a) in &self.model.agents {
                nodes.push(Node {
                    kind: NodeKind::Agent,
                    name: a.name.clone(),
                    depth: 1,
                    file: file.clone(),
                    line: a.line as i32,
                    llm: a.llm,
                });
                if let Some(proto) = &a.protocol {
                    // The "implements <Proto>" edge — clickable, jumps to the
                    // protocol's declaration when we can resolve it.
                    let target = self.protocol_target(proto);
                    nodes.push(Node {
                        kind: NodeKind::Implements,
                        name: format!("implements {proto}"),
                        depth: 2,
                        file: target.as_ref().map(|(f, _)| f.clone()).unwrap_or_else(|| file.clone()),
                        line: target.map(|(_, l)| l).unwrap_or(-1),
                        llm: false,
                    });
                }
                for h in &a.handlers {
                    nodes.push(Node {
                        kind: NodeKind::Handler,
                        name: format!("on {}", h.name),
                        depth: 2,
                        file: file.clone(),
                        line: h.line as i32,
                        llm: false,
                    });
                }
            }
        }

        if !self.model.tools.is_empty() {
            nodes.push(Node::header("TOOLS"));
            for (file, t) in &self.model.tools {
                nodes.push(Node {
                    kind: NodeKind::Tool,
                    name: t.name.clone(),
                    depth: 1,
                    file: file.clone(),
                    line: t.line as i32,
                    llm: false,
                });
            }
        }

        if !self.model.supervisors.is_empty() {
            nodes.push(Node::header("SUPERVISORS"));
            for (file, s) in &self.model.supervisors {
                nodes.push(Node {
                    kind: NodeKind::Supervisor,
                    name: s.name.clone(),
                    depth: 1,
                    file: file.clone(),
                    line: s.line as i32,
                    llm: false,
                });
                for c in &s.children {
                    // Link the child to the spawned agent's definition when known.
                    let target = self.agent_target(&c.agent_ty);
                    nodes.push(Node {
                        kind: NodeKind::Child,
                        name: format!("{} : {}", c.local, c.agent_ty),
                        depth: 2,
                        file: target
                            .as_ref()
                            .map(|(f, _)| f.clone())
                            .unwrap_or_else(|| file.clone()),
                        line: target.map(|(_, l)| l).unwrap_or(c.line as i32),
                        llm: false,
                    });
                }
            }
        }

        self.nodes = nodes;
        if self.first >= self.nodes.len() {
            self.first = 0;
        }
    }

    /// Resolve a protocol name to its `(file, 0-based line)` definition.
    fn protocol_target(&self, name: &str) -> Option<(PathBuf, i32)> {
        self.model
            .protocols
            .iter()
            .find(|(_, p)| p.name == name)
            .map(|(f, p)| (f.clone(), p.line as i32))
    }

    /// Resolve an agent type name to its `(file, 0-based line)` definition.
    fn agent_target(&self, name: &str) -> Option<(PathBuf, i32)> {
        self.model
            .agents
            .iter()
            .find(|(_, a)| a.name == name)
            .map(|(f, a)| (f.clone(), a.line as i32))
    }

    // ---- counts / accessors ----

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&self, i: usize) -> Option<&Node> {
        self.nodes.get(i)
    }

    pub fn agent_count(&self) -> usize {
        self.model.agents.len()
    }

    pub fn protocol_count(&self) -> usize {
        self.model.protocols.len()
    }

    pub fn tool_count(&self) -> usize {
        self.model.tools.len()
    }

    pub fn supervisor_count(&self) -> usize {
        self.model.supervisors.len()
    }

    fn summary_label(count: usize, singular: &str, plural: &str) -> String {
        let label = if count == 1 { singular } else { plural };
        format!("{count} {label}")
    }

    fn summary_line(&self) -> String {
        [
            Self::summary_label(self.agent_count(), "agent", "agents"),
            Self::summary_label(self.protocol_count(), "protocol", "protocols"),
            Self::summary_label(self.tool_count(), "tool", "tools"),
            Self::summary_label(self.supervisor_count(), "supervisor", "supervisors"),
        ]
        .join(" \u{00b7} ")
    }

    fn sidebar_summary_line(&self, text: &mut crate::text::Text, max_px: f32, size: f32) -> String {
        let full = self.summary_line();
        if text.measure_ui_sized(&full, size).0 <= max_px {
            return full;
        }
        let compact = [
            Self::summary_label(self.agent_count(), "agent", "agents"),
            Self::summary_label(self.protocol_count(), "protocol", "protocols"),
            Self::summary_label(self.tool_count(), "tool", "tools"),
        ]
        .join(" \u{00b7} ");
        if text.measure_ui_sized(&compact, size).0 <= max_px {
            compact
        } else {
            format!(
                "{}a \u{00b7} {}p \u{00b7} {}t \u{00b7} {}s",
                self.agent_count(),
                self.protocol_count(),
                self.tool_count(),
                self.supervisor_count()
            )
        }
    }

    /// Total agent→protocol edges (one per agent that implements a protocol).
    pub fn edge_count(&self) -> usize {
        self.model
            .agents
            .iter()
            .filter(|(_, a)| a.protocol.is_some())
            .count()
    }

    /// The `(agent_name, protocol_name)` of edge `i`, for tests / a future graph
    /// view.
    pub fn edge(&self, i: usize) -> Option<(String, String)> {
        self.model
            .agents
            .iter()
            .filter_map(|(_, a)| a.protocol.as_ref().map(|p| (a.name.clone(), p.clone())))
            .nth(i)
    }

    pub fn scroll(&mut self, delta: i32) {
        let max = self.nodes.len().saturating_sub(1) as i32;
        let mut f = self.first as i32 + delta;
        if f < 0 {
            f = 0;
        }
        if f > max.max(0) {
            f = max.max(0);
        }
        self.first = f as usize;
    }

    pub fn click_target(&self) -> Option<&(PathBuf, i32)> {
        self.click_target.as_ref()
    }

    pub fn set_click_target(&mut self, t: Option<(PathBuf, i32)>) {
        self.click_target = t;
    }

    pub fn inspect_note(&self) -> &str {
        &self.inspect_note
    }

    // ---- live inspect (best-effort) ----

    /// Try to attach a live inspector: run `mty inspect --json` against
    /// `MTY_RUNTIME_CONTROL_SOCK` (or an explicit `--sock` from the same env),
    /// parse a snapshot, and store it. Returns the agent count, or `-1` if no
    /// socket is configured / the command fails / parsing fails. Unix runtimes
    /// use a Unix-domain socket; current Mighty on Windows maps the same
    /// configured path to a local named pipe.
    pub fn inspect(&mut self) -> i32 {
        let sock = std::env::var("MTY_RUNTIME_CONTROL_SOCK").ok();
        if sock.as_deref().map(str::trim).unwrap_or("").is_empty() {
            self.inspect_note =
                "Live inspect: set MTY_RUNTIME_CONTROL_SOCK before `mty run` to attach.".to_string();
            self.snapshot = None;
            return -1;
        }
        let mty = mty_path();
        let out = inspect_command(&mty, sock.as_deref()).output();
        match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                if let Some(snap) = agents::parse_snapshot(&stdout) {
                    let n = snap.agents.len();
                    self.inspect_note = format!("Live inspect: {n} agent(s) attached.");
                    self.snapshot = Some(snap);
                    n as i32
                } else {
                    // Transport/no-socket/runtime errors land on stderr (or stdout).
                    let msg = if !stderr.trim().is_empty() {
                        stderr.trim().to_string()
                    } else {
                        stdout.trim().to_string()
                    };
                    self.inspect_note = if msg.is_empty() {
                        default_inspect_note()
                    } else {
                        format!("Live inspect unavailable: {msg}")
                    };
                    self.snapshot = None;
                    -1
                }
            }
            Err(e) => {
                self.inspect_note = format!("Live inspect: could not spawn `{mty} inspect`: {e}");
                self.snapshot = None;
                -1
            }
        }
    }

    pub fn snapshot(&self) -> Option<&RuntimeSnapshot> {
        self.snapshot.as_ref()
    }

    // ---- run (delegates to the embedded RunPanel) ----

    pub fn run_start(&mut self, path: &Path) -> bool {
        self.run.start(path)
    }

    pub fn run_running(&self) -> bool {
        self.run.is_running()
    }

    pub fn run_pump(&mut self) -> bool {
        self.run.pump()
    }

    pub fn run_line_count(&self) -> usize {
        self.run.line_count()
    }

    pub fn clear_run_output(&mut self) -> usize {
        self.run.clear_output()
    }

    pub fn run_line_text(&self, i: usize) -> Option<String> {
        self.run.line(i).map(|l| l.text.clone())
    }

    #[cfg(test)]
    pub fn seed_run_demo(&mut self, path: &str) {
        self.run.seed_demo(path);
    }

    /// Seed the topology + run output for the screenshot hook (no scan / no
    /// process). Uses the bundled `examples/agents.mty` shape.
    pub fn seed_demo(&mut self) {
        let f = PathBuf::from("examples/agents.mty");
        let src = include_str!("../../../examples/agents.mty");
        self.model = agents::scan_file(&f, src);
        self.root = Some(PathBuf::from("examples"));
        self.rebuild();
        // A representative live note for captures without a running program.
        self.inspect_note = default_inspect_note();
    }
}

/// The default live-inspect note shown before a runtime is attached.
fn default_inspect_note() -> String {
    "Live inspect: set control socket".to_string()
}

/// Resolve the `mty` compiler path through the shared Mighty compiler resolver.
fn mty_path() -> String {
    crate::mty::path()
}

// ===========================================================================
// Topology draw
// ===========================================================================

impl AgentTopology {
    /// Y pixel (top) of the first topology row (below the header band + a
    /// summary line + the live-inspect note line).
    fn rows_top() -> f32 {
        40.0 + 22.0 + 20.0
    }

    /// Per-kind icon + color for a row.
    fn row_style(kind: NodeKind) -> (&'static str, crate::ffi::MuiColor) {
        use crate::icons;
        match kind {
            NodeKind::Section => ("", theme::DIM()),
            NodeKind::Protocol => (icons::PROTO_DIAMOND, theme::SYN_KEYWORD()),
            NodeKind::Message => (icons::ENVELOPE, theme::INFO()),
            NodeKind::Agent => (icons::AGENTS, theme::ACCENT_BRIGHT()),
            NodeKind::Handler => (icons::FN_SYMBOL, theme::SYN_FUNCTION()),
            NodeKind::Implements => (icons::CHEVRON, theme::TEXT_3()),
            NodeKind::Tool => (icons::WRENCH, theme::GREEN()),
            NodeKind::Supervisor => (icons::SHIELD, theme::WARNING()),
            NodeKind::Child => (icons::AGENTS_NET, theme::TEXT_1()),
            NodeKind::Llm => (icons::INFO_I, theme::ACCENT()),
        }
    }

    /// Map a click y to a row index (mirrors the draw geometry), or `-1`.
    fn row_at(&self, y: f32) -> i32 {
        let top = Self::rows_top();
        if y < top {
            return -1;
        }
        let row = ((y - top) / layout::LINE_H()).floor() as i32;
        let idx = row + self.first as i32;
        if idx >= 0 && (idx as usize) < self.nodes.len() {
            idx
        } else {
            -1
        }
    }

    /// Draw the panel in the sidebar band. Topology tree with per-kind icons,
    /// depth-indent guides, an LLM badge on LLM-backed agents, and a live-inspect
    /// status line. No-op handled by the caller (panel inactive).
    fn draw(&self, ctx: &mut MuiContext) {
        let h = ctx.gpu.height as f32;
        let clip = ctx.clip;
        let chrome = theme::CHROME_FONT_SIZE;
        let sx = layout::RAIL_W;
        let sw = layout::sidebar_w();

        ctx.dl_rect(sx, 0.0, sw, h, theme::BG_2());
        ctx.dl_rect(sx + sw - 1.0, 0.0, 1.0, h, theme::BORDER());

        // Header band.
        let head_h = 40.0;
        ctx.dl_rect(sx, 0.0, sw, head_h, theme::BG_2());
        ctx.dl_rect(sx, head_h - 1.0, sw, 1.0, theme::BORDER_SOFT());
        let title = "MIGHTY AGENTS";
        let tracked: String = title.chars().flat_map(|c| [c, '\u{2009}']).collect();
        ctx.text.queue_ui_sized(
            sx + 14.0,
            (head_h - (chrome - 2.0)) * 0.5 - 1.0,
            &tracked,
            theme::DIM(),
            chrome - 2.0,
            clip,
        );
        // Small header affordances: Clear transcript + Inspect snapshot + Run.
        let sidebar_right = sx + sw;
        let icon_y = (head_h - 15.0) * 0.5;
        let (clear_x, clear_w) = header_clear_rect(sidebar_right);
        if clear_w >= 15.0 {
            ctx.dl_icon(
                clear_x + (clear_w - 15.0) * 0.5,
                icon_y,
                15.0,
                15.0,
                crate::icons::TRASH,
                theme::TEXT_3(),
                1.5,
                false,
            );
        }
        let (inspect_x, inspect_w) = header_inspect_rect(sidebar_right);
        if inspect_w >= 15.0 {
            ctx.dl_icon(
                inspect_x + (inspect_w - 15.0) * 0.5,
                icon_y,
                15.0,
                15.0,
                crate::icons::INFO_I,
                theme::ACCENT_BRIGHT(),
                1.6,
                true,
            );
        }
        let (run_x, run_w) = header_run_rect(sidebar_right);
        if run_w >= 15.0 {
            ctx.dl_icon(
                run_x + (run_w - 15.0) * 0.5,
                icon_y,
                15.0,
                15.0,
                crate::icons::RUN,
                theme::GREEN(),
                1.6,
                true,
            );
        }

        // Summary line: counts.
        let summary_size = chrome - 2.0;
        let summary_budget = (sw - 28.0).max(0.0);
        let summary = self.sidebar_summary_line(&mut ctx.text, summary_budget, summary_size);
        let shown = fit_sidebar_line(
            &mut ctx.text,
            &summary,
            sw,
            summary_size,
        );
        ctx.text.queue_ui_sized(sx + 14.0, head_h + 4.0, &shown, theme::TEXT_3(), summary_size, clip);

        // Live-inspect status note (dim, single line).
        let note_shown = fit_sidebar_line(&mut ctx.text, &self.inspect_note, sw, chrome - 3.0);
        ctx.text.queue_ui_sized(
            sx + 14.0,
            head_h + 4.0 + 18.0,
            &note_shown,
            theme::TEXT_4(),
            chrome - 3.0,
            clip,
        );

        if self.nodes.is_empty() {
            let empty = fit_sidebar_line(
                &mut ctx.text,
                "No agents found in the workspace.",
                sw,
                chrome,
            );
            ctx.text.queue_ui_sized(
                sx + 14.0,
                Self::rows_top() + 4.0,
                &empty,
                theme::TEXT_3(),
                chrome,
                clip,
            );
            return;
        }

        let row_h = layout::LINE_H();
        let top = Self::rows_top();
        let bottom_pad = 56.0_f32;
        let visible_rows = ((h - top - bottom_pad) / row_h).floor().max(0.0) as usize;
        for (row, n) in self.nodes[self.first..].iter().take(visible_rows).enumerate() {
            let y = top + (row as f32) * row_h;

            if n.kind == NodeKind::Section {
                // Dim uppercase section header with a hairline above (except first).
                let tracked: String = n.name.chars().flat_map(|c| [c, '\u{2009}']).collect();
                ctx.text.queue_ui_sized(
                    sx + 14.0,
                    y + (row_h - (chrome - 2.0)) * 0.5,
                    &tracked,
                    theme::DIM(),
                    chrome - 2.0,
                    clip,
                );
                continue;
            }

            let indent = n.depth as f32 * 16.0;
            // Indent guides: a faint vertical hairline per nesting level.
            let mut g = 1u32;
            while g <= n.depth {
                let gx = sx + 18.0 + (g as f32 - 1.0) * 16.0;
                ctx.dl_rect(gx, y, 1.0, row_h, theme::BORDER_SOFT());
                g += 1;
            }

            let (icon, icol) = Self::row_style(n.kind);
            let ix = sx + 14.0 + indent;
            let icon_y = y + (row_h - 14.0) * 0.5;
            let txt_y = y + (row_h - chrome) * 0.5 - 1.0;
            if !icon.is_empty() {
                let fill = matches!(n.kind, NodeKind::Protocol | NodeKind::Message);
                ctx.dl_icon(ix, icon_y, 14.0, 14.0, icon, icol, 1.5, fill);
            }

            let name_x = ix + 20.0;
            // Agent / protocol / supervisor names use the row color; handlers,
            // messages, edges use softer text.
            let fg = match n.kind {
                NodeKind::Agent => theme::TEXT(),
                NodeKind::Protocol | NodeKind::Supervisor | NodeKind::Tool => theme::TEXT_1(),
                NodeKind::Implements => theme::TEXT_3(),
                _ => theme::TEXT_1(),
            };
            // Reserve room for an LLM badge on agent rows.
            let badge_w = if n.kind == NodeKind::Agent && n.llm { 34.0 } else { 0.0 };
            let max_px = ((sx + sw - 12.0 - badge_w) - name_x).max(0.0);
            let name = fit_agent_row_label(&mut ctx.text, n.kind, &n.name, max_px, chrome);
            ctx.text.queue_ui_sized(name_x, txt_y, &name, fg, chrome, clip);

            // LLM badge (small indigo pill) on LLM-backed agents.
            if n.kind == NodeKind::Agent && n.llm {
                let bx = sx + sw - 38.0;
                let by = y + (row_h - 14.0) * 0.5;
                ctx.dl_round(bx, by, 30.0, 14.0, 7.0, theme::accent_a(0.22));
                ctx.dl_stroke(bx, by, 30.0, 14.0, 7.0, theme::ACCENT_LINE(), 1.0);
                ctx.text.queue_ui_sized(bx + 6.0, by + 1.5, "LLM", theme::ACCENT_BRIGHT(), chrome - 4.0, clip);
            }
        }
        if self.nodes.len() > visible_rows && visible_rows > 0 {
            let track_x = sx + sw - 6.0;
            let track_y = top + 2.0;
            let track_h = (h - top - bottom_pad - 4.0).max(row_h);
            let total = self.nodes.len().max(1) as f32;
            let frac = (visible_rows as f32 / total).clamp(0.12, 1.0);
            let thumb_h = (track_h * frac).max(18.0).min(track_h);
            let max_first = self.nodes.len().saturating_sub(visible_rows).max(1) as f32;
            let scroll_t = (self.first as f32 / max_first).clamp(0.0, 1.0);
            let thumb_y = track_y + (track_h - thumb_h) * scroll_t;
            ctx.dl_round(track_x, track_y, 2.0, track_h, 1.0, theme::BORDER_SOFT());
            ctx.dl_round(track_x - 1.0, thumb_y, 4.0, thumb_h, 2.0, theme::ACCENT_LINE());
        }
    }
}

// ===========================================================================
// Scalar C ABI (mui_agents_*)
// ===========================================================================

#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

/// Re-scan the workspace for the agent system + rebuild the topology rows.
/// Returns the node (row) count. The IDE calls this on panel open + after save.
#[no_mangle]
pub extern "C" fn mui_agents_refresh(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let root = crate::wsabi::effective_root(ctx);
    let mut topo = std::mem::take(&mut ctx.agents);
    let n = topo.refresh(&root);
    println!(
        "agents: scanned {} -> {} agents, {} protocols, {} tools, {} supervisors ({n} rows)",
        root.display(),
        topo.agent_count(),
        topo.protocol_count(),
        topo.tool_count(),
        topo.supervisor_count()
    );
    crate::abi::trace(&format!(
        "agents_refresh rows={n} agents={} protocols={} tools={} supervisors={}",
        topo.agent_count(),
        topo.protocol_count(),
        topo.tool_count(),
        topo.supervisor_count()
    ));
    ctx.agents = topo;
    n as i32
}

/// Number of topology rows.
#[no_mangle]
pub extern "C" fn mui_agents_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.agents.node_count() as i32)
}

/// Kind of row `i` (see [`NodeKind`]), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_agents_node_kind(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.agents.node(i as usize).map_or(-1, |n| n.kind as i32))
}

/// Nesting depth of row `i` (0 = section/top), or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_agents_node_depth(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.agents.node(i as usize).map_or(-1, |n| n.depth as i32))
}

/// 0-based jump line of row `i`, or `-1` (not clickable / out of range).
#[no_mangle]
pub extern "C" fn mui_agents_node_line(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| c.agents.node(i as usize).map_or(-1, |n| n.line))
}

/// Number of chars in row `i`'s display name.
#[no_mangle]
pub extern "C" fn mui_agents_node_name_len(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return 0;
    }
    unsafe { ctx(handle) }.map_or(0, |c| {
        c.agents.node(i as usize).map_or(0, |n| n.name.chars().count() as i32)
    })
}

/// Codepoint `j` of row `i`'s display name, or `-1` out of range.
#[no_mangle]
pub extern "C" fn mui_agents_node_name_char(handle: i64, i: i32, j: i32) -> i32 {
    if i < 0 || j < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.agents.node(i as usize).map_or(-1, |n| {
            n.name.chars().nth(j as usize).map(|ch| ch as i32).unwrap_or(-1)
        })
    })
}

/// Number of agent→protocol edges.
#[no_mangle]
pub extern "C" fn mui_agents_edge_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.agents.edge_count() as i32)
}

/// Open the definition of clickable row `i` (jump to its `(file, line)`):
/// opens the file as a tab if needed and moves the cursor. Returns the tab
/// index, or `-1` (not clickable / out of range).
#[no_mangle]
pub extern "C" fn mui_agents_open_node(handle: i64, i: i32) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    ctx.agents.set_click_target(None);
    if i < 0 {
        ctx.push_toast(crate::toast::Kind::Info, "No agent node selected");
        return -1;
    }
    let (file, line, name) = {
        let Some(n) = ctx.agents.node(i as usize) else {
            ctx.push_toast(crate::toast::Kind::Info, "Agent node no longer listed");
            return -1;
        };
        if n.line < 0 || n.file.as_os_str().is_empty() {
            let name = n.name.trim();
            let label = if name.is_empty() { "node" } else { name };
            ctx.push_toast(
                crate::toast::Kind::Info,
                format!("Agents node has no file target: {label}"),
            );
            return -1;
        }
        (n.file.clone(), n.line, n.name.clone())
    };
    let target_name = crate::abi::file_target_name(&file);
    match agents_target_kind(&file) {
        AgentsTargetKind::File => {}
        AgentsTargetKind::Missing => {
            return refresh_missing_agents_target(
                ctx,
                format!("Agents target missing: {target_name}"),
            );
        }
        AgentsTargetKind::NotFile => {
            return reject_non_file_agents_target(
                ctx,
                format!("Agents target is not a file: {target_name}"),
            );
        }
    }
    let idx = crate::abi::open_path_in_focused_pane(ctx, file.clone());
    crate::abi::record_opened_file(ctx, &file);
    let model = ctx.tabs.active_model_mut();
    model.move_to(line, 0);
    let first = (line - 2).max(0);
    model.set_first_visible(first as usize);
    crate::abi::trace(&format!("agents_open_node target={} node={name}", file.display()));
    idx as i32
}

enum AgentsTargetKind {
    File,
    Missing,
    NotFile,
}

fn agents_target_kind(path: &std::path::Path) -> AgentsTargetKind {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => AgentsTargetKind::File,
        Ok(_) => AgentsTargetKind::NotFile,
        Err(_) => AgentsTargetKind::Missing,
    }
}

fn refresh_missing_agents_target(ctx: &mut MuiContext, message: String) -> i32 {
    let root = ctx
        .agents
        .root
        .clone()
        .unwrap_or_else(|| crate::wsabi::effective_root(ctx));
    let _ = ctx.agents.refresh(&root);
    crate::abi::refresh_workspace_file_views(ctx);
    ctx.push_toast(crate::toast::Kind::Warn, message);
    -1
}

fn reject_non_file_agents_target(ctx: &mut MuiContext, message: String) -> i32 {
    crate::abi::refresh_workspace_file_views(ctx);
    ctx.push_toast(crate::toast::Kind::Warn, message);
    -1
}

/// Scroll the topology by `dir` rows (negative = up).
#[no_mangle]
pub extern "C" fn mui_agents_scroll(handle: i64, dir: i32) {
    if let Some(ctx) = unsafe { ctx(handle) } {
        ctx.agents.scroll(dir);
    }
}

/// Map the last click's pixel position to a topology row index, or `-1` (not on
/// a row / sidebar hidden / wrong panel). Header rows return their index but the
/// IDE only jumps when `mui_agents_node_line >= 0`.
#[no_mangle]
pub extern "C" fn mui_agents_row_at_click(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let sx0 = layout::RAIL_W;
    let sx1 = layout::sidebar_right();
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_AGENTS_MTY {
        return -1;
    }
    if ctx.last_event.x < sx0 || ctx.last_event.x > sx1 {
        return -1;
    }
    ctx.agents.row_at(ctx.last_event.y)
}

/// `1` if the last click landed on the header "Run" affordance, else `0`.
#[no_mangle]
pub extern "C" fn mui_agents_click_is_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.last_event.y <= 40.0
        && header_rect_contains(header_run_rect(layout::sidebar_right()), ctx.last_event.x, true)
    {
        crate::abi::trace("agents_click run");
        1
    } else {
        0
    }
}

/// `1` if the last click landed on the header "Inspect" affordance, else `0`.
#[no_mangle]
pub extern "C" fn mui_agents_click_is_inspect(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.last_event.y <= 40.0
        && header_rect_contains(header_inspect_rect(layout::sidebar_right()), ctx.last_event.x, false)
    {
        crate::abi::trace("agents_click inspect");
        1
    } else {
        0
    }
}

/// `1` if the last click landed on the header "Clear run output" affordance.
#[no_mangle]
pub extern "C" fn mui_agents_click_is_clear(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.last_event.y <= 40.0
        && header_rect_contains(header_clear_rect(layout::sidebar_right()), ctx.last_event.x, false)
    {
        crate::abi::trace("agents_click clear");
        1
    } else {
        0
    }
}

fn inspect_command(mty: &str, sock: Option<&str>) -> Command {
    let mut cmd = Command::new(mty);
    cmd.arg("inspect").arg("--json");
    if let Some(sock) = sock.map(str::trim).filter(|s| !s.is_empty()) {
        cmd.arg("--sock").arg(sock);
    }
    cmd
}

fn active_agent_target_label(ctx: &MuiContext) -> String {
    ctx.tabs
        .active_path()
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(scratch)")
        .to_string()
}

/// Run the active program (`mty run <active file>`) on a background thread,
/// streaming output into the embedded run buffer. Returns `1` if a process
/// spawned, `0` otherwise (no file / spawn failure).
#[no_mangle]
pub extern "C" fn mui_agents_run(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let Some(path) = ctx.tabs.active_path() else {
        crate::abi::trace("agents_run no_active_file");
        ctx.push_toast(
            crate::toast::Kind::Warn,
            format!("Save {} before running Agents", active_agent_target_label(ctx)),
        );
        return 0;
    };
    let mut topo = std::mem::take(&mut ctx.agents);
    let ok = topo.run_start(&path);
    ctx.agents = topo;
    crate::abi::trace(&format!("agents_run ok={} path={}", i32::from(ok), path.display()));
    if ok {
        1
    } else {
        0
    }
}

/// `1` while the run subprocess is still running, else `0`.
#[no_mangle]
pub extern "C" fn mui_agents_running(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| if c.agents.run_running() { 1 } else { 0 })
}

/// Drain pending run output; returns `1` if the run buffer changed this frame.
#[no_mangle]
pub extern "C" fn mui_agents_pump(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let mut topo = std::mem::take(&mut ctx.agents);
    let changed = topo.run_pump();
    ctx.agents = topo;
    if changed {
        1
    } else {
        0
    }
}

/// Number of run-output lines (the Agents panel shows them in the shared Run
/// dock; this lets a caller read the count).
#[no_mangle]
pub extern "C" fn mui_agents_run_line_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| c.agents.run_line_count() as i32)
}

/// Clear the embedded Agents run transcript without stopping a running process
/// or rebuilding the topology. Returns how many output lines were removed.
#[no_mangle]
pub extern "C" fn mui_agents_clear_run_output(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    ctx.active_panel = crate::PANEL_AGENTS_MTY;
    ctx.sidebar_visible = true;
    let mut topo = std::mem::take(&mut ctx.agents);
    let cleared = topo.clear_run_output() as i32;
    ctx.agents = topo;
    if cleared > 0 {
        ctx.push_toast(crate::toast::Kind::Info, "Agents run output cleared");
    } else {
        ctx.push_toast(crate::toast::Kind::Info, "Agents run output already empty");
    }
    crate::abi::trace(&format!("agents_clear_run_output lines={cleared}"));
    cleared
}

/// Close the Mighty Agents panel without clearing topology or embedded run
/// output. Returns `1` when it closed Agents, or `0` when already closed.
#[no_mangle]
pub extern "C" fn mui_agents_close(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if ctx.active_panel == crate::PANEL_AGENTS_MTY {
        ctx.active_panel = crate::PANEL_EXPLORER;
        ctx.push_toast(crate::toast::Kind::Info, "Mighty Agents panel closed");
        crate::abi::trace("agents_close");
        return 1;
    }
    ctx.push_toast(crate::toast::Kind::Info, "Mighty Agents panel is already closed");
    crate::abi::trace("agents_close noop");
    0
}

/// Attempt a best-effort live inspect (`mty inspect --json`). Returns the live
/// agent count, or `-1` if unavailable (no socket / command failure / parse fail).
/// The reason is surfaced in the panel's live-inspect note line.
#[no_mangle]
pub extern "C" fn mui_agents_inspect(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let mut topo = std::mem::take(&mut ctx.agents);
    let n = topo.inspect();
    ctx.agents = topo;
    crate::abi::trace(&format!("agents_inspect result={n}"));
    n
}

/// Number of live agent snapshots from the last inspect (0 if none / unattached).
#[no_mangle]
pub extern "C" fn mui_agents_live_count(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| {
        c.agents.snapshot().map_or(0, |s| s.agents.len() as i32)
    })
}

/// Mailbox depth of live agent `i`, or `-1` (no snapshot / out of range).
#[no_mangle]
pub extern "C" fn mui_agents_live_mailbox(handle: i64, i: i32) -> i32 {
    if i < 0 {
        return -1;
    }
    unsafe { ctx(handle) }.map_or(-1, |c| {
        c.agents.snapshot().map_or(-1, |s| {
            s.agents.get(i as usize).map_or(-1, |a| a.mailbox_depth as i32)
        })
    })
}

/// Draw the Mighty Agents panel. No-op unless the sidebar is shown + this panel
/// is active. Mighty calls this each frame.
#[no_mangle]
pub extern "C" fn mui_agents_draw(handle: i64) {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return;
    };
    if !ctx.sidebar_visible || ctx.active_panel != crate::PANEL_AGENTS_MTY {
        return;
    }
    let topo = std::mem::take(&mut ctx.agents);
    topo.draw(ctx);
    ctx.agents = topo;
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::scan_file;

    fn seeded() -> AgentTopology {
        let mut t = AgentTopology::new();
        let f = PathBuf::from("agents.mty");
        let src = include_str!("../../../examples/agents.mty");
        t.set_model(scan_file(&f, src));
        t
    }

    #[test]
    fn flattens_sections_in_order() {
        let t = seeded();
        // The first section is PROTOCOLS, then AGENTS, then TOOLS, then SUPERVISORS.
        let sections: Vec<&str> = t
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Section)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(sections, vec!["PROTOCOLS", "AGENTS", "TOOLS", "SUPERVISORS"]);
    }

    #[test]
    fn counts_match_model() {
        let t = seeded();
        assert_eq!(t.agent_count(), 2);
        assert_eq!(t.protocol_count(), 2);
        assert_eq!(t.tool_count(), 1);
        assert_eq!(t.supervisor_count(), 1);
        // Both agents implement a protocol -> 2 edges.
        assert_eq!(t.edge_count(), 2);
    }

    #[test]
    fn summary_line_pluralizes_counts() {
        let t = seeded();
        assert_eq!(
            t.summary_line(),
            "2 agents \u{00b7} 2 protocols \u{00b7} 1 tool \u{00b7} 1 supervisor"
        );
    }

    #[test]
    fn clear_run_output_preserves_topology() {
        let mut t = seeded();
        t.seed_run_demo("examples/agents.mty");

        let cleared = t.clear_run_output();

        assert_eq!(cleared, 8);
        assert_eq!(t.run_line_count(), 0);
        assert_eq!(t.agent_count(), 2);
        assert_eq!(t.protocol_count(), 2);
        assert_eq!(t.tool_count(), 1);
        assert_eq!(t.supervisor_count(), 1);
    }

    #[test]
    fn sidebar_summary_uses_clean_compact_form_when_narrow() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(560, 520) else {
            return;
        };
        let t = seeded();
        let size = crate::theme::CHROME_FONT_SIZE - 2.0;
        let compact = "2 agents \u{00b7} 2 protocols \u{00b7} 1 tool";
        let full_budget = ctx.text.measure_ui_sized(&t.summary_line(), size).0 + 1.0;
        let compact_budget = ctx.text.measure_ui_sized(compact, size).0 + 1.0;
        let tiny_budget = ctx.text.measure_ui_sized("2a \u{00b7} 2p \u{00b7} 1t \u{00b7} 1s", size).0 - 1.0;

        assert_eq!(t.sidebar_summary_line(&mut ctx.text, full_budget, size), t.summary_line());
        assert_eq!(t.sidebar_summary_line(&mut ctx.text, compact_budget, size), compact);
        assert_eq!(
            t.sidebar_summary_line(&mut ctx.text, tiny_budget, size),
            "2a \u{00b7} 2p \u{00b7} 1t \u{00b7} 1s"
        );
    }

    #[test]
    fn default_inspect_note_fits_compact_sidebar() {
        let note = default_inspect_note();
        assert!(
            note.chars().count() <= 32,
            "default live-inspect copy should not truncate into an unclear fragment: {note}"
        );
    }

    #[test]
    fn compact_agent_row_labels_preserve_signature_meaning() {
        assert_eq!(
            compact_agent_row_label(NodeKind::Message, "Submit(text: Str) -> U8"),
            "Submit(Str) -> U8"
        );
        assert_eq!(
            compact_agent_row_label(NodeKind::Message, "Ask(doc: Str) -> Str"),
            "Ask(Str) -> Str"
        );
        assert_eq!(
            compact_agent_row_label(NodeKind::Implements, "implements Summarize"),
            "impl Summarize"
        );
    }

    #[test]
    fn agent_row_fitter_uses_compact_label_before_ellipsis() {
        let mut ctx = match crate::MuiContext::new_offscreen(560, 520) {
            Some(c) => c,
            None => return,
        };
        let chrome = crate::theme::CHROME_FONT_SIZE;
        let compact = "Submit(Str) -> U8";
        let budget = ctx.text.measure_ui_sized(compact, chrome).0 + 1.0;
        let shown = fit_agent_row_label(
            &mut ctx.text,
            NodeKind::Message,
            "Submit(text: Str) -> U8",
            budget,
            chrome,
        );
        assert_eq!(shown, compact);
        assert!(
            !shown.ends_with('\u{2026}'),
            "message signature should compact before ellipsizing: {shown}"
        );

        let impl_compact = "impl Summarize";
        let impl_budget = ctx.text.measure_ui_sized(impl_compact, chrome).0 + 1.0;
        let shown = fit_agent_row_label(
            &mut ctx.text,
            NodeKind::Implements,
            "implements Summarize",
            impl_budget,
            chrome,
        );
        assert_eq!(shown, impl_compact);
    }

    #[test]
    fn sidebar_line_fitter_keeps_empty_state_inside_compact_panel() {
        let mut ctx = match crate::MuiContext::new_offscreen(560, 520) {
            Some(c) => c,
            None => return,
        };
        let chrome = crate::theme::CHROME_FONT_SIZE;
        let sidebar_w = 214.0;
        let shown = fit_sidebar_line(
            &mut ctx.text,
            "No agents found in the workspace.",
            sidebar_w,
            chrome,
        );
        let (w, _) = ctx.text.measure_ui_sized(&shown, chrome);
        assert!(
            w <= sidebar_w - 28.0 + 0.5,
            "empty-state line should fit compact sidebar: {shown} ({w}px)"
        );
        assert!(shown.ends_with('\u{2026}'), "long empty-state copy should ellipsize");
    }

    #[test]
    fn sidebar_line_fitter_keeps_live_inspect_note_inside_panel() {
        let mut ctx = match crate::MuiContext::new_offscreen(560, 520) {
            Some(c) => c,
            None => return,
        };
        let chrome = crate::theme::CHROME_FONT_SIZE - 3.0;
        let sidebar_w = 184.0;
        let shown = fit_sidebar_line(
            &mut ctx.text,
            "Live inspect: set MTY_RUNTIME_CONTROL_SOCK before `mty run` to attach.",
            sidebar_w,
            chrome,
        );
        let (w, _) = ctx.text.measure_ui_sized(&shown, chrome);
        assert!(
            w <= sidebar_w - 28.0 + 0.5,
            "live-inspect note should fit compact sidebar: {shown} ({w}px)"
        );
    }

    #[test]
    fn agent_row_carries_llm_flag_and_implements_edge() {
        let t = seeded();
        // Find the Summarizer agent row.
        let summarizer = t
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Agent && n.name == "Summarizer")
            .expect("Summarizer row");
        assert!(summarizer.llm, "Summarizer is LLM-backed");
        // The Implements edge points at the Summarize protocol's line (clickable).
        let edge = t
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Implements && n.name.contains("Summarize"))
            .expect("implements edge");
        assert!(edge.line >= 0, "implements edge resolves to a protocol line");
    }

    #[test]
    fn handler_rows_present_under_agent() {
        let t = seeded();
        let handlers: Vec<&str> = t
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Handler)
            .map(|n| n.name.as_str())
            .collect();
        assert!(handlers.contains(&"on Submit"));
        assert!(handlers.contains(&"on Flush"));
        assert!(handlers.contains(&"on Ask"));
    }

    #[test]
    fn supervisor_children_link_to_agents() {
        let t = seeded();
        let child = t
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Child && n.name.contains("Collector"))
            .expect("child row");
        // The child row's jump line equals the Collector agent's decl line.
        let collector = t
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Agent && n.name == "Collector")
            .unwrap();
        assert_eq!(child.line, collector.line);
    }

    #[test]
    fn empty_model_has_no_rows() {
        let mut t = AgentTopology::new();
        t.set_model(AgentModel::default());
        assert_eq!(t.node_count(), 0);
    }

    #[test]
    fn scroll_clamps() {
        let mut t = seeded();
        t.scroll(1000);
        assert!(t.first <= t.node_count().saturating_sub(1));
        t.scroll(-1000);
        assert_eq!(t.first, 0);
    }

    #[test]
    fn inspect_command_passes_configured_socket() {
        let cmd = inspect_command("mty", Some("  /tmp/mty.sock  "));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["inspect", "--json", "--sock", "/tmp/mty.sock"]);
    }

    #[test]
    fn inspect_command_without_socket_uses_env_default() {
        let cmd = inspect_command("mty", None);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["inspect", "--json"]);
    }

    #[test]
    fn header_affordances_hit_inspect_and_run_separately() {
        let mut ctx = match crate::MuiContext::new_offscreen(900, 600) {
            Some(c) => c,
            None => return,
        };
        ctx.sidebar_visible = true;
        ctx.active_panel = crate::PANEL_AGENTS_MTY;
        let h = (&mut ctx as *mut crate::MuiContext) as usize as i64;
        let right = layout::sidebar_right();

        ctx.last_event = crate::ffi::MuiEvent::mouse(
            crate::ffi::MUI_EVENT_MOUSE_DOWN,
            0,
            right - 68.0,
            20.0,
            0,
        );
        assert_eq!(mui_agents_click_is_clear(h), 1);
        assert_eq!(mui_agents_click_is_inspect(h), 0);
        assert_eq!(mui_agents_click_is_run(h), 0);

        ctx.last_event = crate::ffi::MuiEvent::mouse(
            crate::ffi::MUI_EVENT_MOUSE_DOWN,
            0,
            right - 48.0,
            20.0,
            0,
        );
        assert_eq!(mui_agents_click_is_clear(h), 0);
        assert_eq!(mui_agents_click_is_inspect(h), 1);
        assert_eq!(mui_agents_click_is_run(h), 0);

        ctx.last_event = crate::ffi::MuiEvent::mouse(
            crate::ffi::MUI_EVENT_MOUSE_DOWN,
            0,
            right - 20.0,
            20.0,
            0,
        );
        assert_eq!(mui_agents_click_is_clear(h), 0);
        assert_eq!(mui_agents_click_is_inspect(h), 0);
        assert_eq!(mui_agents_click_is_run(h), 1);
    }

    #[test]
    fn header_affordance_rects_clamp_inside_sidebar_band() {
        let right = layout::RAIL_W + 66.0;
        let (run_x, run_w) = header_run_rect(right);
        let (inspect_x, inspect_w) = header_inspect_rect(right);
        let (clear_x, clear_w) = header_clear_rect(right);

        assert!(clear_x >= layout::RAIL_W);
        assert!(inspect_x >= layout::RAIL_W);
        assert!(run_x >= layout::RAIL_W);
        assert!(clear_x + clear_w <= inspect_x + 0.5);
        assert!(inspect_x + inspect_w <= run_x + 0.5);
        assert!(run_x + run_w <= right + 0.5);
        assert!(header_rect_contains((clear_x, clear_w), clear_x + clear_w * 0.5, false));
        assert!(!header_rect_contains(
            (clear_x, clear_w),
            inspect_x + inspect_w * 0.5,
            false
        ));
        assert!(header_rect_contains((run_x, run_w), run_x + run_w * 0.5, true));
        assert!(!header_rect_contains(
            (inspect_x, inspect_w),
            run_x + run_w * 0.5,
            false
        ));
    }
}
