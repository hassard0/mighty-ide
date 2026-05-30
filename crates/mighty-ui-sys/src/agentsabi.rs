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
    /// socket is configured / the platform stub fires / parsing fails.
    ///
    /// On **Windows** the runtime binds no named-pipe socket (v0.16 Unix-only),
    /// so this always reports the stub and stores an explanatory note; on Unix
    /// it works when a program was launched with the env var set.
    pub fn inspect(&mut self) -> i32 {
        let sock = std::env::var("MTY_RUNTIME_CONTROL_SOCK").ok();
        if sock.as_deref().map(str::trim).unwrap_or("").is_empty() {
            self.inspect_note =
                "Live inspect: set MTY_RUNTIME_CONTROL_SOCK before `mty run` to attach.".to_string();
            self.snapshot = None;
            return -1;
        }
        let mty = mty_path();
        let out = Command::new(&mty).arg("inspect").arg("--json").output();
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
                    // The stub / no-socket error lands on stderr (or stdout).
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

    pub fn run_line_text(&self, i: usize) -> Option<String> {
        self.run.line(i).map(|l| l.text.clone())
    }

    /// Seed the topology + run output for the screenshot hook (no scan / no
    /// process). Uses the bundled `examples/agents.mty` shape.
    pub fn seed_demo(&mut self) {
        let f = PathBuf::from("examples/agents.mty");
        let src = include_str!("../../../examples/agents.mty");
        self.model = agents::scan_file(&f, src);
        self.root = Some(PathBuf::from("examples"));
        self.rebuild();
        // A representative live note (the Windows reality).
        self.inspect_note = default_inspect_note();
    }
}

/// The default live-inspect note (the honest platform reality on Windows).
fn default_inspect_note() -> String {
    if cfg!(windows) {
        "Live inspect: mty inspect's control socket is Unix-only in v0.36 \
         (Windows named pipe not yet implemented). Static topology + run are live."
            .to_string()
    } else {
        "Live inspect: launch a program with MTY_RUNTIME_CONTROL_SOCK set to attach.".to_string()
    }
}

/// Resolve the `mty` compiler path (honors `MIGHTY_MTY`, else the dev build).
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
        let adv = chrome * 0.55;
        let sx = layout::RAIL_W;
        let sw = layout::SIDEBAR_W;

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
        // A small "Run" affordance icon at the header's right edge.
        ctx.dl_icon(
            sx + sw - 28.0,
            (head_h - 15.0) * 0.5,
            15.0,
            15.0,
            crate::icons::RUN,
            theme::GREEN(),
            1.6,
            true,
        );

        // Summary line: counts.
        let summary = format!(
            "{} agents \u{00b7} {} protocols \u{00b7} {} tools \u{00b7} {} supervisors",
            self.agent_count(),
            self.protocol_count(),
            self.tool_count(),
            self.supervisor_count()
        );
        let mut shown = summary;
        let avail = ((sw - 24.0) / (adv * 0.92)).floor() as usize;
        if shown.chars().count() > avail && avail > 1 {
            shown = shown.chars().take(avail - 1).collect::<String>() + "\u{2026}";
        }
        ctx.text.queue_ui_sized(sx + 14.0, head_h + 4.0, &shown, theme::TEXT_3(), chrome - 2.0, clip);

        // Live-inspect status note (dim, single line).
        let note = self.inspect_note.clone();
        let mut note_shown = note;
        if note_shown.chars().count() > avail && avail > 1 {
            note_shown = note_shown.chars().take(avail - 1).collect::<String>() + "\u{2026}";
        }
        ctx.text.queue_ui_sized(
            sx + 14.0,
            head_h + 4.0 + 18.0,
            &note_shown,
            theme::TEXT_4(),
            chrome - 3.0,
            clip,
        );

        if self.nodes.is_empty() {
            ctx.text.queue_ui_sized(
                sx + 14.0,
                Self::rows_top() + 4.0,
                "No agents found in the workspace.",
                theme::TEXT_3(),
                chrome,
                clip,
            );
            return;
        }

        let row_h = layout::LINE_H();
        let top = Self::rows_top();
        for (row, n) in self.nodes[self.first..].iter().enumerate() {
            let y = top + (row as f32) * row_h;
            if y > h {
                break;
            }

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
            let avail = (((sx + sw - 12.0 - badge_w) - name_x) / adv).floor() as usize;
            let mut name = n.name.clone();
            if name.chars().count() > avail && avail > 1 {
                name = name.chars().take(avail - 1).collect::<String>() + "\u{2026}";
            }
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
    let root = ctx.tree.root().to_path_buf();
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
    if i < 0 {
        return -1;
    }
    let (file, line) = {
        let Some(n) = ctx.agents.node(i as usize) else {
            return -1;
        };
        if n.line < 0 || n.file.as_os_str().is_empty() {
            return -1;
        }
        (n.file.clone(), n.line)
    };
    if !file.exists() {
        return -1;
    }
    let idx = ctx.tabs.open_path(file);
    crate::abi::sync_active_path(ctx);
    let model = ctx.tabs.active_model_mut();
    model.move_to(line, 0);
    let first = (line - 2).max(0);
    model.set_first_visible(first as usize);
    idx as i32
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
    let sx1 = layout::RAIL_W + layout::SIDEBAR_W;
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
    let run_x0 = layout::RAIL_W + layout::SIDEBAR_W - 34.0;
    if ctx.last_event.y <= 40.0 && ctx.last_event.x >= run_x0 {
        1
    } else {
        0
    }
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
        return 0;
    };
    let mut topo = std::mem::take(&mut ctx.agents);
    let ok = topo.run_start(&path);
    ctx.agents = topo;
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

/// Attempt a best-effort live inspect (`mty inspect --json`). Returns the live
/// agent count, or `-1` if unavailable (no socket / Windows stub / parse fail).
/// The reason is surfaced in the panel's live-inspect note line.
#[no_mangle]
pub extern "C" fn mui_agents_inspect(handle: i64) -> i32 {
    let Some(ctx) = (unsafe { ctx(handle) }) else {
        return -1;
    };
    let mut topo = std::mem::take(&mut ctx.agents);
    let n = topo.inspect();
    ctx.agents = topo;
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
}
