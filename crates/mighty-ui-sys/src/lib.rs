//! Mighty IDE native render shim — flat C ABI (`mui_*`).
//!
//! The shim owns the window, GPU surface, and text rendering; the Mighty IDE
//! owns the main loop and drives the shim each frame via these `extern "C"`
//! entry points. The shim NEVER calls back into Mighty (poll/pump model).
//!
//! Module layout:
//! * [`ffi`]    — `#[repr(C)]` types ([`MuiColor`], [`MuiEvent`], key/mouse codes)
//! * [`gpu`]    — wgpu device/surface + solid-rect pipeline (+ offscreen target)
//! * [`text`]   — glyphon font system, atlas, renderer, measurement
//! * [`window`] — winit window + `pump_events` + event-queue translation

mod abi;
mod agents;
mod agentsabi;
mod ai;
mod blame;
mod colorize;
mod completion;
mod config;
mod crumbmenu;
mod dap;
mod dapabi;
mod diagnostics;
mod diff;
mod editor;
mod featureabi;
mod ffi;
mod format;
mod ghost;
mod ghostabi;
mod gpu;
mod history;
mod icons;
mod langdetect;
mod language;
mod layout;
mod lspclient;
mod lspregistry;
mod nav;
mod navsurfaces;
mod outline;
mod palette;
mod panes;
mod peek;
mod panels;
mod problems;
mod prompt;
mod quickopen;
mod run;
mod scm;
mod screenshot;
mod search;
mod settings;
mod settingspanel;
mod snippets;
mod snippetsabi;
mod sticky;
mod stickyabi;
mod syntax;
mod tabs;
mod terminal;
mod testabi;
mod tests_panel;
mod text;
mod theme;
mod themepicker;
mod toast;
mod tree;
mod web;
mod webabi;
mod welcome;
mod vello_proof;
mod vello_ui;
mod window;

pub use abi::*;
pub use ffi::*;

use std::path::PathBuf;

use std::sync::Arc;

use gpu::{Gpu, RectInstance, RenderTarget};
use text::Text;
use window::{EventQueue, WindowHost};

/// Smoke export retained from the spike: proves the staticlib links.
#[no_mangle]
pub extern "C" fn mui_smoke_add(a: i32, b: i32) -> i32 {
    a + b
}

/// Opaque handle returned to the IDE. Layout is private to Rust.
pub struct MuiContext {
    gpu: Gpu,
    text: Text,
    queue: Box<EventQueue>,
    /// `None` in offscreen/headless mode.
    host: Option<WindowHost>,
    #[allow(dead_code)]
    window: Option<Arc<winit::window::Window>>,

    // ---- per-frame state ----
    rects: Vec<RectInstance>,
    /// Overlay-layer rects (palette/autocomplete scrim + cards), drawn in a
    /// second rect pass on top of base text so cards occlude editor glyphs.
    rects_overlay: Vec<RectInstance>,
    /// When `true`, [`mui_fill_rect`] routes into [`Self::rects_overlay`] and
    /// text into the overlay layer (see `text::Text::set_overlay`).
    overlay: bool,
    clip: Option<(u32, u32, u32, u32)>,
    /// Surface frame held between begin/end in windowed mode.
    frame: Option<wgpu::SurfaceTexture>,
    frame_view: Option<wgpu::TextureView>,
    in_frame: bool,

    // ---- scalar-ABI staging state (see abi.rs) ----
    /// Text accumulated codepoint-by-codepoint before a `mui_text_draw`.
    text_stage: String,
    /// Last event delivered by `mui_poll_event_s`, read by scalar accessors.
    last_event: MuiEvent,
    /// Configured source/target file path (shim owns file I/O).
    file_path: Option<PathBuf>,
    /// The detected language of the active file (drives multi-language syntax
    /// highlighting + the status-bar pill + the LSP bridge). Recomputed whenever
    /// `file_path` changes (`sync_active_path` / `mui_path_commit`).
    language: langdetect::Language,
    /// Path bytes staged before `mui_path_commit`.
    path_stage: Vec<u8>,
    /// Bytes of the most recently loaded file (read by index).
    load_buf: Vec<u8>,
    /// Bytes staged for `mui_save_commit`.
    save_buf: Vec<u8>,
    /// Latest parsed diagnostics from `mty check` (refreshed on demand).
    diags: Vec<diagnostics::Diag>,

    // ---- editor-feature state (status bar, prompt, find) ----
    /// Basename of the edited file, drawn in the status bar.
    file_name: String,
    /// 1-based cursor (line, col) fed each frame for the status bar.
    status_cursor: (i32, i32),
    /// Bottom one-line prompt buffer (goto / find), shim-owned (L17).
    prompt: prompt::PromptState,
    /// Find-search engine over the buffer streamed in from Mighty.
    find: prompt::FindState,
    /// In-buffer find/replace bar (Ctrl+H): find + replace fields + focus.
    replace_bar: prompt::ReplaceBar,

    // ---- multi-file workspace state (tabs + file tree) ----
    /// Open tabs + per-tab cursor/scroll/dirty state (shim-owned, L17).
    tabs: tabs::TabStore,
    /// The editor **pane layout** (side-by-side split). Starts with ONE pane, so
    /// it is inert (the unsplit path is byte-identical to before); a split adds a
    /// second pane and `mui_pane_*` rebinds the active tab + per-pane scroll. See
    /// `crate::panes`.
    panes: panes::PaneLayout,
    /// File-tree sidebar model.
    tree: tree::FileTree,
    /// Whether the sidebar is currently shown (toggled by Ctrl+B).
    sidebar_visible: bool,

    // ---- integrated terminal state ----
    /// The PTY-backed terminal, lazily spawned on first open (`mui_term_open`).
    /// `None` until opened or after the shell exits + the panel is closed.
    terminal: Option<terminal::Terminal>,
    /// Whether the terminal panel is currently shown (toggled by Ctrl+`).
    term_open: bool,

    // ---- autocomplete state ----
    /// The completion dropdown engine (candidate list + selection), shim-owned.
    complete: completion::CompletionEngine,
    /// Editor buffer bytes streamed in from Mighty for a completion request
    /// (mirrors the find streaming path — Mighty can't pass a buffer, L17).
    complete_buf: Vec<u8>,

    // ---- hover + go-to-definition state ----
    /// The hover popup (wrapped text + active flag), shim-owned.
    hover: nav::HoverState,
    /// The most recent resolved definition target (path + 0-based line/col).
    def: nav::DefState,
    /// Editor buffer bytes streamed in from Mighty for a hover/def request
    /// (same shape as `complete_buf`; the live unsaved source is the doc text).
    nav_buf: Vec<u8>,

    // ---- deeper language intelligence (signature help / rename / code actions) ----
    /// Signature-help popup state (parsed `SignatureInformation`), shim-owned.
    sig: language::SigState,
    /// Inline rename input + the parsed `WorkspaceEdit` from the last commit.
    rename: language::RenameState,
    /// Code-action menu state (action list + selection), shim-owned.
    codeaction: language::CodeActionState,

    // ---- undo / redo history (shim-owned, L21) ----
    /// Undo + redo stacks of full buffer snapshots for the active tab. Mighty
    /// streams its post-edit buffer in (reusing the byte-streaming path) and the
    /// shim coalesces typing runs / decides whether to push. Re-seeded on load /
    /// tab switch so history is per-active-buffer.
    history: history::HistoryStore,
    /// Restored cursor from the most recent `mui_undo` / `mui_redo`, read back by
    /// Mighty via `mui_undo_cursor_line` / `_col` after pulling the bytes.
    restored_cursor: (i32, i32),

    // ---- command palette (Ctrl+Shift+P) ----
    /// The command palette overlay (registry + query/filter + selection),
    /// shim-owned. Mighty opens it, feeds chars, moves the selection, and reads
    /// the selected command id back to dispatch.
    palette: palette::PaletteEngine,

    // ---- universal Quick-Open (Ctrl+P) ----
    /// The Quick-Open finder: cached workspace file index + MRU + fuzzy matcher,
    /// with mode prefixes (files / `>` commands / `@` symbols / `:` line). Mighty
    /// opens it, feeds chars/keys, and reads back the chosen file path / symbol /
    /// line to dispatch. Shim-owned (L17).
    quickopen: quickopen::QuickOpen,

    // ---- color-theme picker (Preferences: Color Theme) ----
    /// The theme chooser overlay (3 themes, live preview), shim-owned. Mighty
    /// opens it, moves the highlight (live preview), and commits/cancels.
    theme_picker: themepicker::ThemePicker,
    /// Screenshot-only hook (`MUI_THEMEPICKER_AUTOOPEN`): when `true`, the theme
    /// picker is force-drawn each frame so a headless capture shows it.
    theme_picker_autoopen: bool,

    // ---- offscreen screenshot mode (MUI_SCREENSHOT) ----
    /// When `Some`, the context renders into an offscreen texture (no window)
    /// and writes a PNG of the configured frame, then asks the loop to exit.
    /// `None` for normal windowed runs (behavior unchanged).
    screenshot: Option<screenshot::ScreenshotState>,

    // ---- live editor model undo/redo (shim-side; L28 workaround) ----
    /// Undo/redo of full [`editor::TextModel`] snapshots for the ACTIVE tab.
    /// Since the editable buffer now lives shim-side (L28), undo also lives
    /// here: `mui_ed_undo_record` pushes the current model, `mui_ed_undo`/`_redo`
    /// restore one. Reset on load / tab switch (history is per active buffer).
    ed_undo: Vec<editor::TextModel>,
    ed_redo: Vec<editor::TextModel>,
    /// When set by `MUI_EDIT_PROBE`, [`mui_ed_load`] becomes a no-op so the
    /// scripted-edit model survives the IDE's initial load — letting a headless
    /// screenshot capture the LIVE-edited buffer (screenshots/06-edit.png).
    pub(crate) edit_probe_lock: bool,

    /// The interactive minimap's last-drawn geometry (for the FOCUSED pane), so
    /// the editor click router ([`mui_ed_click`]) can hit-test a click in the
    /// strip and jump the editor to the corresponding source line. Updated each
    /// frame by the minimap draw; `None` when the minimap is hidden / too narrow.
    pub(crate) minimap_geom: Option<colorize::MinimapGeom>,

    // ---- Vello proof (MUI_VELLO_PROOF=1; Phase 1 renderer upgrade) ----
    /// When `MUI_VELLO_PROOF` is set, [`render_and_present`] renders a static
    /// Vello vector scene (gradients/rounded/shadow/AA text) instead of the
    /// rect/glyphon UI, proving CSS-quality output. Built lazily on the first
    /// proof frame; `None` for normal runs (the rect path is fully unaffected).
    vello_proof: Option<vello_proof::VelloProof>,

    // ---- Vello UI backend (Phase 2: the DEFAULT render path) ----
    /// The per-frame display list the chrome/editor draw functions build (rounded
    /// rects, gradients, shadows, squiggles, glyph runs). Replayed into a Vello
    /// scene each frame by [`vello_ui::VelloUi`].
    dl: vello_ui::DisplayList,
    /// The Vello UI renderer, built lazily on the first frame. `None` until then.
    vello_ui: Option<vello_ui::VelloUi>,
    /// Screenshot-only hook (`MUI_COMPLETE_AUTOOPEN`): when `Some`, the
    /// autocomplete dropdown is force-drawn at this `(row, col)` each frame so a
    /// headless capture shows it (it otherwise only draws while `completing` in
    /// the Mighty loop, which a non-interactive run can't enter). `None` normally.
    complete_autoopen: Option<(i32, i32)>,
    /// Screenshot-only hooks for the language-intelligence overlays: when `Some`,
    /// the signature popup / code-action menu is force-drawn at `(row, col)`;
    /// `rename_autoopen` force-draws the centered rename input. `None`/`false`
    /// for normal runs.
    sig_autoopen: Option<(i32, i32)>,
    codeaction_autoopen: Option<(i32, i32)>,
    rename_autoopen: bool,

    // ---- activity-rail panels (Explorer / Search / Source Control) ----
    /// The sidebar's active panel: 0 = Explorer, 1 = Search, 2 = Source Control.
    /// Switched by clicking a rail icon (`mui_panel_set`).
    active_panel: i32,
    /// Source-control (git) panel state: repo root + parsed status + commit msg.
    scm: scm::ScmState,
    /// The branch-switcher overlay (list + filter + create-branch input).
    branch_picker: scm::BranchPicker,
    /// The git blame gutter (per-line author/date/sha, cached per file + toggle).
    blame: crate::blame::BlameState,
    /// Project-wide find/replace panel state: query/replace buffers + results.
    search: search::SearchState,

    // ---- AI copilot (Agents rail icon → right-docked chat panel) ----
    /// The AI chat panel: transcript + input + live Anthropic stream, shim-owned.
    /// Mighty opens it, feeds chars/keys, sends, and pumps the stream each frame.
    ai: ai::AiPanel,

    // ---- inline AI ghost-text completions (Copilot-style) ----
    /// The inline ghost-text engine: debounce timer, generation-id cancel, the
    /// background completion request, and the pending dim suggestion overlay.
    /// Mighty arms it after edits, ticks/polls it each frame, and accepts/dismisses.
    ghost: ghost::GhostState,

    // ---- Run panel (the Run rail icon) ----
    /// The Run panel: a background `mty run <path>` whose stdout/stderr streams
    /// into a scrollable output view with clickable diagnostics. Shim-owned.
    run: run::RunPanel,

    // ---- Web Playground (Run in Browser → wasm32-web + browser) ----
    /// The Web Playground: builds the active file to `wasm32-web` and runs it in
    /// the browser — either via a background `mty serve` (web-game packages) or a
    /// `mty build --target wasm32-web` + static-server fallback. Streams the
    /// build/serve output, scrapes the served URL, and offers a stop affordance.
    /// Shim-owned (see `crate::web` / `crate::webabi`).
    web: web::WebPlayground,

    // ---- Test panel (the beaker rail icon → Testing results view) ----
    /// The Test panel: a background `mty test` over the active file's package,
    /// parsed into a pass/fail results tree with click-to-jump. Shim-owned.
    tests_panel: tests_panel::TestPanel,

    // ---- debugger (Run and Debug rail icon → DAP-driven debug view) ----
    /// The debugger model: per-file breakpoints, the live `mty dap` session,
    /// the current stop position, the call stack + selected frame, the
    /// variables, and the debug console. Shim-owned (see `crate::dap`).
    dbg: dap::DebugModel,

    // ---- inline git diff view (Source Control) ----
    /// The inline diff view: a parsed unified diff for one file, rendered in the
    /// editor area (read-only). `None`/inactive until `mui_diff_open`.
    diff: diff::DiffView,

    // ---- Settings panel (Preferences: Settings) ----
    /// The Settings panel: editable live preferences (font size / tab width /
    /// word wrap / minimap / theme). Shim-owned; changes apply live + persist.
    settings_panel: settingspanel::SettingsPanel,

    // ---- Outline / document-symbols panel (rail slot 5) ----
    /// The Outline panel: the active file's symbols (LSP documentSymbol when the
    /// server implements it, else a shim-side scanner) + the cursor-current sym.
    outline: outline::OutlineState,

    // ---- Mighty Agents panel (rail slot 8 → agent-system topology view) ----
    /// The Mighty Agents panel: a STATIC discovery of the workspace's agent
    /// system (protocols / agents / handlers / tools / supervisors + agent→
    /// protocol edges) rendered as a topology tree, plus a Run action and a
    /// best-effort live-inspect surface. Shim-owned (see `crate::agentsabi`).
    agents: agentsabi::AgentTopology,

    // ---- sticky scroll (pin enclosing-scope headers at the editor top) ----
    /// The sticky-header set: the enclosing scopes of the top visible line,
    /// recomputed each frame from the outline symbols + the scroll offset.
    sticky: sticky::StickyState,

    // ---- peek definition (inline framed definition preview) ----
    /// The inline peek card: a resolved definition target + a previewed window of
    /// its source lines, drawn below the cursor line. Inactive until opened.
    /// (The `MUI_PEEK_AUTOOPEN` screenshot hook just opens it; the unconditional
    /// `mui_peek_draw` call then renders it for the capture.)
    peek: peek::PeekState,

    // ---- Problems panel (bottom dock; status-bar chip opens it) ----
    /// The Problems panel: aggregated `mty check` diagnostics across open tabs /
    /// the workspace, grouped by file, click-to-jump. Shim-owned.
    problems: problems::ProblemSet,

    // ---- interactive breadcrumb dropdown ----
    /// The breadcrumb quick-dropdown (folder files or document symbols), styled
    /// like the command palette. Opened by clicking a breadcrumb segment.
    crumb_menu: crumbmenu::CrumbMenu,
    /// File paths backing the crumb file dropdown (index -> path), set when the
    /// file segment opens so accept-by-index can resolve the chosen file.
    crumb_files: Vec<PathBuf>,
    /// Screenshot-only hook (`MUI_OUTLINE_AUTOOPEN` / `MUI_BREADCRUMB_AUTOOPEN`):
    /// force the crumb menu / outline open for a headless capture. `false`
    /// normally (the menu only opens via a click in the live loop).
    crumb_menu_autoopen: bool,

    // ---- Welcome / first-impression screen ----
    /// The Welcome landing (brand + recents + quick actions + tips). Shown in the
    /// editor body when no real file is open, or forced from the palette. Owns its
    /// hit-test rects for click routing.
    welcome: welcome::WelcomeState,

    // ---- toast notifications (transient bottom-right cards) ----
    /// The toast stack: shim-pushed transient cards (saved / committed / build
    /// result / errors / theme changed …) that auto-dismiss. Drawn over chrome.
    toasts: toast::ToastQueue,

    // ---- snippets (prefix → template expansion with navigable tab-stops) ----
    /// The active tab-stop navigation session over an expanded snippet (inactive
    /// until `mui_snippet_try_expand` succeeds). Drives the cursor/selection to
    /// each `$1 $2 … $0` stop on Tab / Shift+Tab; ends on the final stop or Esc.
    /// The snippet DEFINITIONS themselves are language-keyed + computed on demand
    /// (`snippets::snippets_for`), so only the live session is held here.
    snippet_session: snippets::SnippetSession,
}

impl MuiContext {
    /// Push a toast from inside the shim (the common case — file saved, git
    /// committed, build/run finished, errors, theme changed, …). Used across the
    /// shim's existing code paths.
    pub(crate) fn push_toast(&mut self, kind: toast::Kind, message: impl Into<String>) {
        self.toasts.push(kind, message);
    }
}

/// Panel ids (mirror the Mighty side + rail icon order).
pub const PANEL_EXPLORER: i32 = 0;
pub const PANEL_SEARCH: i32 = 1;
pub const PANEL_SCM: i32 = 2;
/// Outline (document symbols) sidebar panel — rail slot 5 (below Run/Agents).
pub const PANEL_OUTLINE: i32 = 5;
/// Run and Debug sidebar panel — rail slot 6 (the bug icon).
pub const PANEL_DEBUG: i32 = 6;
/// Testing sidebar panel — rail slot 7 (the beaker icon).
pub const PANEL_TEST: i32 = 7;
/// Mighty Agents sidebar panel — rail slot 8 (the network/nodes icon). The
/// agent-system topology view (protocols / agents / handlers / tools /
/// supervisors). Unique to an agent-first language.
pub const PANEL_AGENTS_MTY: i32 = 8;

// ---------------------------------------------------------------------------
// Vello display-list helpers (used by the chrome/editor draw functions to emit
// CSS-quality primitives — rounded rects, gradients, shadows, squiggles).
// ---------------------------------------------------------------------------

impl MuiContext {
    /// A flat filled rect (the `mui_fill_rect` primitive), routed to the active
    /// (base/overlay) layer.
    pub(crate) fn dl_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: MuiColor) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::Rect { x, y, w, h, color });
    }
    /// A filled rounded rect.
    pub(crate) fn dl_round(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: MuiColor) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::RoundRect { x, y, w, h, radius, color });
    }
    /// A left→right fading horizontal gradient (current-line band / row tints).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dl_grad_h(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
        fade: f32,
    ) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::GradH { x, y, w, h, radius, color, fade });
    }
    /// A top→bottom vertical gradient (elevated panels/cards).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dl_grad_v(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        top: MuiColor,
        bottom: MuiColor,
    ) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::GradV { x, y, w, h, radius, top, bottom });
    }
    /// A soft (blurred) drop shadow under a rounded rect.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dl_shadow(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
        blur: f32,
    ) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::Shadow { x, y, w, h, radius, color, blur });
    }
    /// A hairline stroke around a rounded rect (borders).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dl_stroke(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: MuiColor,
        width: f32,
    ) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::StrokeRound { x, y, w, h, radius, color, width });
    }
    /// A radial glow over a clip rect (ember brand tile, soft accent glows).
    #[allow(clippy::too_many_arguments, dead_code)]
    pub(crate) fn dl_glow(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        inner: MuiColor,
        outer: MuiColor,
        clip_x: f32,
        clip_y: f32,
        clip_w: f32,
        clip_h: f32,
    ) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::RadialGlow {
            cx,
            cy,
            radius,
            inner,
            outer,
            clip_x,
            clip_y,
            clip_w,
            clip_h,
        });
    }
    /// A real vector icon (SVG path scaled into the box, stroked at `stroke`px,
    /// optionally filled). The canonical icon primitive that replaces glyphs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dl_icon(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        path: &'static str,
        color: MuiColor,
        stroke: f32,
        fill: bool,
    ) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::Icon {
            x,
            y,
            w,
            h,
            path,
            color,
            stroke,
            fill,
            vb: crate::icons::VB,
        });
    }
    /// A stroked icon at the default 1.5px stroke (the common case).
    #[allow(dead_code)]
    pub(crate) fn dl_icon_stroke(
        &mut self,
        x: f32,
        y: f32,
        sz: f32,
        path: &'static str,
        color: MuiColor,
    ) {
        self.dl_icon(x, y, sz, sz, path, color, 1.5, false);
    }
    /// A wavy red diagnostic underline.
    pub(crate) fn dl_squiggle(&mut self, x: f32, y: f32, w: f32, color: MuiColor) {
        self.dl.on_overlay = self.overlay;
        self.dl.clip = self.clip.map(|(x, y, w, h)| (x as f32, y as f32, w as f32, h as f32));
        self.dl.push(vello_ui::UiCmd::Squiggle { x, y, w, color });
    }
}

// ---------------------------------------------------------------------------
// 2.1 — init / shutdown
// ---------------------------------------------------------------------------

/// Initialize the shim: open a window of `width`x`height` titled by the UTF-8
/// bytes at `title_ptr`/`title_len`, and set up GPU + text. Returns an opaque
/// context pointer, or null on failure.
///
/// # Safety
/// `title_ptr` must point to `title_len` valid bytes (or be null with len 0).
#[no_mangle]
pub unsafe extern "C" fn mui_init(
    width: u32,
    height: u32,
    title_ptr: *const u8,
    title_len: usize,
) -> *mut MuiContext {
    let title = read_utf8(title_ptr, title_len).unwrap_or_else(|| "Mighty IDE".to_string());
    build_context(width, height, title, None)
}

/// Build a windowed context with an explicit window `title` and an optional
/// pre-resolved `file_path`. Shared by [`mui_init`] and [`abi::mui_init_s`].
/// Returns null on window/GPU failure.
pub(crate) fn build_context(
    width: u32,
    height: u32,
    title: String,
    file_path: Option<PathBuf>,
) -> *mut MuiContext {
    // Activate the persisted (or MUI_THEME-overridden) color theme before any
    // draw call so the whole IDE — including the first frame / screenshots —
    // renders in the chosen theme. Default is Vivid Modern.
    theme::set_active(config::resolve_startup_theme());
    // Load the persisted editor preferences (font size / tab width / word wrap /
    // minimap) into the active settings so the first frame already reflects them.
    settings::load_into_active();

    let mut queue = Box::new(EventQueue::default());
    let queue_ptr: *mut EventQueue = queue.as_mut();

    // Screenshot mode: render headless into an offscreen texture (no window /
    // surface) and capture a PNG. The window dimensions can be overridden via
    // MUI_SCREENSHOT_W / MUI_SCREENSHOT_H so a faithful large frame is captured
    // regardless of the dimensions the Mighty side passes to mui_init_s.
    let screenshot = screenshot::ScreenshotState::from_env();

    let (host, window, gpu) = if screenshot.is_some() {
        let sw = std::env::var("MUI_SCREENSHOT_W")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(width);
        let sh = std::env::var("MUI_SCREENSHOT_H")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(height);
        let gpu = match Gpu::new_offscreen(sw, sh) {
            Ok(Some(g)) => g,
            Ok(None) => {
                eprintln!("mui_init: MUI_SCREENSHOT set but no GPU adapter available");
                return std::ptr::null_mut();
            }
            Err(e) => {
                eprintln!("mui_init: offscreen init failed: {e}");
                return std::ptr::null_mut();
            }
        };
        (None, None, gpu)
    } else {
        let (host, window) = match WindowHost::create(width, height, title, queue_ptr) {
            Ok((h, w)) => (h, w),
            Err(e) => {
                eprintln!("mui_init: {e}");
                return std::ptr::null_mut();
            }
        };
        let gpu = match Gpu::new_windowed(window.clone()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("mui_init: {e}");
                return std::ptr::null_mut();
            }
        };
        (Some(host), Some(window), gpu)
    };

    let text = Text::new(&gpu.device, &gpu.queue, gpu.format);

    let file_name = file_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Seed the tab store with the initial file as tab 0 (or a scratch tab), and
    // root the file tree at that file's directory (or the cwd).
    let mut tab_store = tabs::TabStore::new();
    if let Some(p) = file_path.clone() {
        tab_store.open_path(p);
    } else {
        tab_store.ensure_scratch();
    }
    let tree_root = file_path
        .as_ref()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .filter(|d| !d.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let mut file_tree = tree::FileTree::new();
    file_tree.set_root(tree_root);

    let ctx = Box::new(MuiContext {
        gpu,
        text,
        queue,
        host,
        window,
        rects: Vec::new(),
        rects_overlay: Vec::new(),
        overlay: false,
        clip: None,
        frame: None,
        frame_view: None,
        in_frame: false,
        text_stage: String::new(),
        last_event: MuiEvent::none(),
        language: file_path
            .as_ref()
            .map(|p| langdetect::detect_path(p))
            .unwrap_or(langdetect::Language::PlainText),
        file_path,
        path_stage: Vec::new(),
        load_buf: Vec::new(),
        save_buf: Vec::new(),
        diags: Vec::new(),
        file_name,
        status_cursor: (1, 1),
        prompt: prompt::PromptState::new(),
        find: prompt::FindState::new(),
        replace_bar: prompt::ReplaceBar::new(),
        panes: panes::PaneLayout::new(tab_store.active()),
        tabs: tab_store,
        tree: file_tree,
        sidebar_visible: true,
        terminal: None,
        term_open: false,
        complete: completion::CompletionEngine::new(),
        complete_buf: Vec::new(),
        hover: nav::HoverState::new(),
        def: nav::DefState::new(),
        nav_buf: Vec::new(),
        sig: language::SigState::new(),
        rename: language::RenameState::new(),
        codeaction: language::CodeActionState::new(),
        history: history::HistoryStore::new(),
        restored_cursor: (0, 0),
        palette: palette::PaletteEngine::new(),
        quickopen: quickopen::QuickOpen::new(),
        theme_picker: themepicker::ThemePicker::new(),
        theme_picker_autoopen: false,
        screenshot,
        ed_undo: Vec::new(),
        ed_redo: Vec::new(),
        edit_probe_lock: false,
        minimap_geom: None,
        vello_proof: None,
        dl: vello_ui::DisplayList::default(),
        vello_ui: None,
        complete_autoopen: None,
        sig_autoopen: None,
        codeaction_autoopen: None,
        rename_autoopen: false,
        active_panel: PANEL_EXPLORER,
        scm: scm::ScmState::new(),
        branch_picker: scm::BranchPicker::new(),
        blame: crate::blame::BlameState::new(),
        search: search::SearchState::new(),
        ai: ai::AiPanel::new(),
        ghost: ghost::GhostState::new(),
        run: run::RunPanel::new(),
        web: web::WebPlayground::new(),
        tests_panel: tests_panel::TestPanel::new(),
        dbg: dap::DebugModel::new(),
        diff: diff::DiffView::new(),
        settings_panel: settingspanel::SettingsPanel::new(),
        outline: outline::OutlineState::new(),
        agents: agentsabi::AgentTopology::new(),
        sticky: sticky::StickyState::new(),
        peek: peek::PeekState::new(),
        problems: problems::ProblemSet::new(),
        crumb_menu: crumbmenu::CrumbMenu::new(),
        crumb_files: Vec::new(),
        crumb_menu_autoopen: false,
        welcome: welcome::WelcomeState::new(),
        toasts: toast::ToastQueue::new(),
        snippet_session: snippets::SnippetSession::new(),
    });
    Box::into_raw(ctx)
}

/// Tear down the shim and free the context.
///
/// # Safety
/// `ctx` must be a pointer previously returned by `mui_init` and not freed.
#[no_mangle]
pub unsafe extern "C" fn mui_shutdown(ctx: *mut MuiContext) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

// ---------------------------------------------------------------------------
// 2.2 — rects
// ---------------------------------------------------------------------------

/// Queue a solid-color rectangle (pixel space) for the current frame.
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_fill_rect(
    ctx: *mut MuiContext,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: MuiColor,
) {
    let Some(ctx) = ctx.as_mut() else { return };
    // Vello path (default): a flat rect in the display list.
    ctx.dl_rect(x, y, w, h, color);
    // Legacy rect-pipeline path (kept for the GPU unit tests that assert on
    // `ctx.rects`); unused by the default Vello render.
    let inst = RectInstance {
        pos: [x, y],
        size: [w, h],
        color: [color.r, color.g, color.b, color.a],
    };
    if ctx.overlay {
        ctx.rects_overlay.push(inst);
    } else {
        ctx.rects.push(inst);
    }
}

// ---------------------------------------------------------------------------
// 2.3 — text
// ---------------------------------------------------------------------------

/// Queue UTF-8 text at (`x`,`y`) in the given color for the current frame.
///
/// # Safety
/// `ctx` valid; `utf8_ptr` points to `len` valid bytes.
#[no_mangle]
pub unsafe extern "C" fn mui_draw_text(
    ctx: *mut MuiContext,
    x: f32,
    y: f32,
    utf8_ptr: *const u8,
    len: usize,
    color: MuiColor,
) {
    let Some(ctx) = ctx.as_mut() else { return };
    let Some(text) = read_utf8(utf8_ptr, len) else {
        return;
    };
    let clip = ctx.clip;
    ctx.text.queue(x, y, &text, color, clip);
}

/// Measure UTF-8 text, writing pixel `width`/`height` to the out-params.
/// Returns `true` on success.
///
/// # Safety
/// `ctx` valid; `utf8_ptr` points to `len` valid bytes; `out_w`/`out_h` valid.
#[no_mangle]
pub unsafe extern "C" fn mui_text_measure(
    ctx: *mut MuiContext,
    utf8_ptr: *const u8,
    len: usize,
    out_w: *mut f32,
    out_h: *mut f32,
) -> bool {
    let Some(ctx) = ctx.as_mut() else { return false };
    let Some(text) = read_utf8(utf8_ptr, len) else {
        return false;
    };
    let (w, h) = ctx.text.measure(&text);
    if !out_w.is_null() {
        *out_w = w;
    }
    if !out_h.is_null() {
        *out_h = h;
    }
    true
}

// ---------------------------------------------------------------------------
// 2.4 — frame lifecycle + clip
// ---------------------------------------------------------------------------

/// Begin a frame: acquire the surface texture (windowed) and reset draw state.
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_begin_frame(ctx: *mut MuiContext) {
    let Some(ctx) = ctx.as_mut() else { return };
    ctx.rects.clear();
    ctx.rects_overlay.clear();
    ctx.dl.clear();
    ctx.overlay = false;
    ctx.clip = None;
    ctx.text.begin();
    ctx.in_frame = true;

    if let RenderTarget::Surface(surface) = &ctx.gpu.target {
        match surface.get_current_texture() {
            Ok(frame) => {
                let view = frame.texture.create_view(&Default::default());
                ctx.frame = Some(frame);
                ctx.frame_view = Some(view);
            }
            Err(_) => {
                // Surface lost/outdated: reconfigure and try once more.
                let (w, h) = (ctx.gpu.width, ctx.gpu.height);
                ctx.gpu.resize(w, h);
                if let RenderTarget::Surface(surface) = &ctx.gpu.target {
                    if let Ok(frame) = surface.get_current_texture() {
                        let view = frame.texture.create_view(&Default::default());
                        ctx.frame = Some(frame);
                        ctx.frame_view = Some(view);
                    }
                }
            }
        }
    }
}

/// Set a scissor clip rect (pixels) applied to subsequent draws this frame.
/// (Width/height of 0 are allowed and clip everything.)
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_set_clip(ctx: *mut MuiContext, x: u32, y: u32, w: u32, h: u32) {
    let Some(ctx) = ctx.as_mut() else { return };
    ctx.clip = Some((x, y, w, h));
}

/// End the frame: submit rects then text, and present (windowed).
///
/// # Safety
/// `ctx` must be a valid context pointer.
#[no_mangle]
pub unsafe extern "C" fn mui_end_frame(ctx: *mut MuiContext) {
    let Some(ctx) = ctx.as_mut() else { return };
    if !ctx.in_frame {
        return;
    }
    render_and_present(ctx);
    ctx.in_frame = false;
}

/// `true` when the legacy solid-rect + glyphon render path should be used
/// instead of the default Vello UI (env `MUI_LEGACY_RENDER=1`). The Vello UI is
/// the DEFAULT (Phase 2); this gate is retained only as a fallback / for the
/// rect-pipeline GPU tests.
fn legacy_render_enabled() -> bool {
    std::env::var("MUI_LEGACY_RENDER")
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0"
        })
        .unwrap_or(false)
}

/// Shared by `mui_end_frame` and tests: encode rects + text and submit.
fn render_and_present(ctx: &mut MuiContext) {
    let (w, h) = (ctx.gpu.width, ctx.gpu.height);

    // Vello proof path (MUI_VELLO_PROOF=1): render the static vector proof scene
    // through Vello instead of the rect/glyphon UI. Vello manages its own GPU
    // submission (compute + blit), so we bypass the rect encoder entirely.
    if vello_proof::proof_enabled() {
        render_vello_proof(ctx, w, h);
        return;
    }

    // Default Phase-2 path: render the whole IDE as a Vello scene from the
    // per-frame display list (the chrome/editor draw functions built it).
    if !legacy_render_enabled() {
        render_vello_ui(ctx, w, h);
        return;
    }

    // Determine the view to render into. Both the surface frame view and the
    // offscreen view are owned elsewhere (ctx.frame_view / ctx.gpu.target), so
    // a borrow suffices.
    let view: &wgpu::TextureView = match (&ctx.frame_view, &ctx.gpu.target) {
        (Some(v), _) => v,
        (None, RenderTarget::Offscreen { view, .. }) => view,
        (None, RenderTarget::Surface(_)) => return, // no frame acquired
    };

    let mut encoder = ctx
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mui frame encoder"),
        });

    // Background pass: clears to BG, then paints the atmospheric glow quad.
    ctx.gpu.render_background(&mut encoder, view);

    // Base rect pass (loads on top of the glow — does NOT clear).
    ctx.gpu
        .render_rects(&mut encoder, view, &ctx.rects, false, ctx.clip);

    // Base text pass (loads, draws on top). Errors are logged, not fatal.
    if let Err(e) = ctx
        .text
        .render(&ctx.gpu.device, &ctx.gpu.queue, &mut encoder, view, w, h, false)
    {
        eprintln!("mui_end_frame (base text): {e}");
    }

    // Overlay rect pass: opaque scrim + cards drawn over the base text so it is
    // occluded, then overlay text on top. Only runs when overlays were queued.
    if !ctx.rects_overlay.is_empty() {
        ctx.gpu
            .render_rects(&mut encoder, view, &ctx.rects_overlay, false, None);
    }
    if let Err(e) = ctx
        .text
        .render(&ctx.gpu.device, &ctx.gpu.queue, &mut encoder, view, w, h, true)
    {
        eprintln!("mui_end_frame (overlay text): {e}");
    }

    ctx.gpu.queue.submit([encoder.finish()]);

    if let Some(frame) = ctx.frame.take() {
        frame.present();
    }
    ctx.frame_view = None;

    // Screenshot mode: on the configured frame, read the offscreen texture back
    // and write a PNG. Done after every other draw call this frame, so the PNG
    // is a faithful capture of the full UI. The next poll returns Close.
    maybe_capture_screenshot(ctx);
}

/// Render the whole IDE as a Vello scene from the per-frame display list (the
/// DEFAULT Phase-2 path). The chrome/editor draw functions pushed rounded rects,
/// gradients, shadows, squiggles and (via glyphon's queued runs) text into
/// `ctx.dl` + `ctx.text`; here we fold the text runs into the display list and
/// replay it over the atmosphere. Vello owns its own GPU submission.
fn render_vello_ui(ctx: &mut MuiContext, w: u32, h: u32) {
    // Lazily build the Vello renderer (with a blit pipeline for the surface
    // format in windowed mode, or pure offscreen for the screenshot path).
    if ctx.vello_ui.is_none() {
        let surface_format = match &ctx.gpu.target {
            RenderTarget::Surface(_) => Some(ctx.gpu.format),
            RenderTarget::Offscreen { .. } => None,
        };
        match vello_ui::VelloUi::new(&ctx.gpu.device, surface_format) {
            Ok(v) => ctx.vello_ui = Some(v),
            Err(e) => {
                eprintln!("mui vello ui: {e}");
                return;
            }
        }
    }

    // Screenshot hook: force-draw the autocomplete dropdown when armed (so a
    // headless capture shows it). Mirrors `mui_complete_draw_at` exactly.
    if let Some((row, col)) = ctx.complete_autoopen {
        let region = layout::region(ctx.sidebar_visible);
        let total = ctx.tabs.active_model().line_count().max(1) as u64;
        let cx = layout::text_x_in(region, total, col);
        let cy = layout::row_y_in(region, row);
        let engine = std::mem::take(&mut ctx.complete);
        ctx.overlay = true;
        ctx.text.set_overlay(true);
        engine.draw(ctx, cx, cy, w, h);
        ctx.overlay = false;
        ctx.text.set_overlay(false);
        ctx.complete = engine;
    }

    // Screenshot hooks for the language-intelligence overlays (signature popup /
    // code-action menu): force-draw when armed so a headless capture shows them.
    if let Some((row, col)) = ctx.sig_autoopen {
        let region = layout::region(ctx.sidebar_visible);
        let total = ctx.tabs.active_model().line_count().max(1) as u64;
        let cx = layout::text_x_in(region, total, col);
        let cy = layout::row_y_in(region, row);
        let sig = std::mem::take(&mut ctx.sig);
        ctx.overlay = true;
        ctx.text.set_overlay(true);
        sig.draw(ctx, cx, cy, w, h);
        ctx.overlay = false;
        ctx.text.set_overlay(false);
        ctx.sig = sig;
    }
    if let Some((row, col)) = ctx.codeaction_autoopen {
        let region = layout::region(ctx.sidebar_visible);
        let total = ctx.tabs.active_model().line_count().max(1) as u64;
        let cx = layout::text_x_in(region, total, col);
        let cy = layout::row_y_in(region, row);
        let menu = std::mem::take(&mut ctx.codeaction);
        ctx.overlay = true;
        ctx.text.set_overlay(true);
        menu.draw(ctx, cx, cy, w, h);
        ctx.overlay = false;
        ctx.text.set_overlay(false);
        ctx.codeaction = menu;
    }
    // Rename input is centered (no anchor needed). In the LIVE path Mighty calls
    // `mui_rename_draw` itself; here we only force it for the headless capture
    // (the autoopen flag), to avoid double-drawing interactively.
    if ctx.rename_autoopen && ctx.rename.is_active() {
        let rename = std::mem::take(&mut ctx.rename);
        ctx.overlay = true;
        ctx.text.set_overlay(true);
        rename.draw(ctx, w, h);
        ctx.overlay = false;
        ctx.text.set_overlay(false);
        ctx.rename = rename;
    }

    // Screenshot hook for the theme picker: force-draw the chooser when armed so
    // a headless capture shows it (it otherwise only draws while the Mighty loop
    // routes to it, which a non-interactive run can't enter).
    if ctx.theme_picker_autoopen && ctx.theme_picker.is_active() {
        let picker = std::mem::take(&mut ctx.theme_picker);
        ctx.overlay = true;
        ctx.text.set_overlay(true);
        picker.draw(ctx, w, h);
        ctx.overlay = false;
        ctx.text.set_overlay(false);
        ctx.theme_picker = picker;
    }

    // Fold the queued glyphon text runs into the display list (each keeps its
    // layer/font/size/color), so the Vello scene reproduces all chrome + code.
    ctx.text.drain_into_display_list(&mut ctx.dl);

    // Render. Borrow the renderer out so we can also borrow gpu/dl immutably.
    let mut vp = ctx.vello_ui.take().unwrap();
    match &ctx.gpu.target {
        RenderTarget::Offscreen { view, .. } => {
            if let Err(e) =
                vp.render_to_texture(&ctx.gpu.device, &ctx.gpu.queue, view, w, h, &ctx.dl)
            {
                eprintln!("mui vello ui: {e}");
            }
        }
        RenderTarget::Surface(_) => {
            if let Some(frame) = ctx.frame.take() {
                if let Err(e) =
                    vp.render_to_surface(&ctx.gpu.device, &ctx.gpu.queue, &frame, w, h, &ctx.dl)
                {
                    eprintln!("mui vello ui: {e}");
                }
                frame.present();
            }
            ctx.frame_view = None;
        }
    }
    ctx.vello_ui = Some(vp);

    maybe_capture_screenshot(ctx);
}

/// Render the static Vello proof scene to the active target (surface or
/// offscreen texture), lazily constructing the Vello renderer on first use.
/// Vello owns its GPU submission; we just hand it the device/queue + target.
fn render_vello_proof(ctx: &mut MuiContext, w: u32, h: u32) {
    // Lazily build the renderer (knows whether it needs a blit pipeline for the
    // surface format vs. a pure offscreen renderer).
    if ctx.vello_proof.is_none() {
        let surface_format = match &ctx.gpu.target {
            RenderTarget::Surface(_) => Some(ctx.gpu.format),
            RenderTarget::Offscreen { .. } => None,
        };
        match vello_proof::VelloProof::new(&ctx.gpu.device, surface_format) {
            Ok(vp) => ctx.vello_proof = Some(vp),
            Err(e) => {
                eprintln!("mui vello proof: {e}");
                return;
            }
        }
    }
    let vp = ctx.vello_proof.as_mut().unwrap();

    match &ctx.gpu.target {
        RenderTarget::Offscreen { view, .. } => {
            if let Err(e) = vp.render_to_texture(&ctx.gpu.device, &ctx.gpu.queue, view, w, h) {
                eprintln!("mui vello proof: {e}");
            }
        }
        RenderTarget::Surface(_) => {
            if let Some(frame) = ctx.frame.take() {
                if let Err(e) =
                    vp.render_to_surface(&ctx.gpu.device, &ctx.gpu.queue, &frame, w, h)
                {
                    eprintln!("mui vello proof: {e}");
                }
                frame.present();
            }
            ctx.frame_view = None;
        }
    }

    // Still honor the screenshot capture on the offscreen path.
    maybe_capture_screenshot(ctx);
}

/// In screenshot mode, count frames and capture the configured one to a PNG.
/// No-op in windowed mode (`ctx.screenshot` is `None`).
fn maybe_capture_screenshot(ctx: &mut MuiContext) {
    let Some(shot) = ctx.screenshot.as_mut() else {
        return;
    };
    if shot.captured {
        return;
    }
    let this_frame = shot.frame;
    shot.frame += 1;
    if this_frame != shot.target_frame {
        return;
    }

    let (w, h) = (ctx.gpu.width, ctx.gpu.height);
    match ctx.gpu.read_pixels() {
        Some(pixels) => {
            let shot = ctx.screenshot.as_mut().unwrap();
            match screenshot::write_png(&shot.out_path, w, h, &pixels) {
                Ok(bytes) => println!(
                    "mui_screenshot: wrote {} ({w}x{h}, {bytes} bytes, frame {})",
                    shot.out_path.display(),
                    shot.target_frame
                ),
                Err(e) => eprintln!("mui_screenshot: {e}"),
            }
            shot.captured = true;
        }
        None => {
            eprintln!("mui_screenshot: read_pixels returned None (not offscreen?)");
            ctx.screenshot.as_mut().unwrap().captured = true;
        }
    }
}

// ---------------------------------------------------------------------------
// 2.5 — event pump
// ---------------------------------------------------------------------------

/// Pump OS events (windowed) then pop one queued event into `*out_ev`.
/// Returns `true` if an event was written, `false` when the queue is empty.
///
/// # Safety
/// `ctx` valid; `out_ev` points to a writable `MuiEvent`.
#[no_mangle]
pub unsafe extern "C" fn mui_poll_event(ctx: *mut MuiContext, out_ev: *mut MuiEvent) -> bool {
    let Some(ctx) = ctx.as_mut() else { return false };

    // Screenshot mode: once the target frame has been captured, deliver a single
    // Close event so the Mighty frame loop exits promptly. Until then, report
    // "no event" so the loop keeps drawing full frames into the offscreen target.
    if let Some(shot) = ctx.screenshot.as_ref() {
        if shot.captured {
            if !out_ev.is_null() {
                *out_ev = MuiEvent::close();
            }
            return true;
        }
        return false;
    }

    // Pump the OS event loop only when there is nothing buffered, so a backlog
    // is drained FIFO before new OS events are folded in.
    if ctx.queue.is_empty() {
        if let Some(host) = ctx.host.as_mut() {
            host.pump();
        }
    }

    // Apply any pending resize (reconfigure the surface) before delivery.
    if let Some((w, h)) = ctx.queue.pending_resize.take() {
        ctx.gpu.resize(w, h);
    }

    match ctx.queue.pop() {
        Some(ev) => {
            if !out_ev.is_null() {
                *out_ev = ev;
            }
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Read `len` bytes at `ptr` as a UTF-8 `String` (lossy). `None` if invalid.
unsafe fn read_utf8(ptr: *const u8, len: usize) -> Option<String> {
    if len == 0 {
        return Some(String::new());
    }
    if ptr.is_null() {
        return None;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    Some(String::from_utf8_lossy(slice).into_owned())
}

// ---------------------------------------------------------------------------
// test-only offscreen helpers (no winit window/surface)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
impl MuiContext {
    /// Build a headless context backed by an offscreen texture. Returns `None`
    /// when no GPU adapter is available (so tests can skip).
    fn new_offscreen(width: u32, height: u32) -> Option<Self> {
        let gpu = match Gpu::new_offscreen(width, height) {
            Ok(Some(g)) => g,
            Ok(None) => return None,
            Err(e) => {
                eprintln!("offscreen init failed: {e}");
                return None;
            }
        };
        let text = Text::new(&gpu.device, &gpu.queue, gpu.format);
        // Seed a scratch tab so the active editor model is always present (the
        // real `build_context` does this; tests build the context directly).
        let mut tabs = tabs::TabStore::new();
        tabs.ensure_scratch();
        Some(MuiContext {
            gpu,
            text,
            queue: Box::new(EventQueue::default()),
            host: None,
            window: None,
            rects: Vec::new(),
            rects_overlay: Vec::new(),
            overlay: false,
            clip: None,
            frame: None,
            frame_view: None,
            in_frame: false,
            text_stage: String::new(),
            last_event: MuiEvent::none(),
            language: langdetect::Language::PlainText,
            file_path: None,
            path_stage: Vec::new(),
            load_buf: Vec::new(),
            save_buf: Vec::new(),
            diags: Vec::new(),
            file_name: String::new(),
            status_cursor: (1, 1),
            prompt: prompt::PromptState::new(),
            find: prompt::FindState::new(),
            replace_bar: prompt::ReplaceBar::new(),
            panes: panes::PaneLayout::new(tabs.active()),
            tabs,
            tree: tree::FileTree::new(),
            sidebar_visible: true,
            terminal: None,
            term_open: false,
            complete: completion::CompletionEngine::new(),
            complete_buf: Vec::new(),
            hover: nav::HoverState::new(),
            def: nav::DefState::new(),
            nav_buf: Vec::new(),
            sig: language::SigState::new(),
            rename: language::RenameState::new(),
            codeaction: language::CodeActionState::new(),
            history: history::HistoryStore::new(),
            restored_cursor: (0, 0),
            palette: palette::PaletteEngine::new(),
            quickopen: quickopen::QuickOpen::new(),
            theme_picker: themepicker::ThemePicker::new(),
            theme_picker_autoopen: false,
            screenshot: None,
            ed_undo: Vec::new(),
            ed_redo: Vec::new(),
            edit_probe_lock: false,
            minimap_geom: None,
            vello_proof: None,
            dl: vello_ui::DisplayList::default(),
            vello_ui: None,
            complete_autoopen: None,
            sig_autoopen: None,
            codeaction_autoopen: None,
            rename_autoopen: false,
            active_panel: PANEL_EXPLORER,
            scm: scm::ScmState::new(),
            branch_picker: scm::BranchPicker::new(),
            blame: crate::blame::BlameState::new(),
            search: search::SearchState::new(),
            ai: ai::AiPanel::new(),
            ghost: ghost::GhostState::new(),
            run: run::RunPanel::new(),
            web: web::WebPlayground::new(),
            tests_panel: tests_panel::TestPanel::new(),
            dbg: dap::DebugModel::new(),
            diff: diff::DiffView::new(),
            settings_panel: settingspanel::SettingsPanel::new(),
            outline: outline::OutlineState::new(),
            agents: agentsabi::AgentTopology::new(),
            sticky: sticky::StickyState::new(),
            peek: peek::PeekState::new(),
            problems: problems::ProblemSet::new(),
            crumb_menu: crumbmenu::CrumbMenu::new(),
            crumb_files: Vec::new(),
            crumb_menu_autoopen: false,
            welcome: welcome::WelcomeState::new(),
            toasts: toast::ToastQueue::new(),
            snippet_session: snippets::SnippetSession::new(),
        })
    }

    fn read_pixels(&self) -> Vec<u8> {
        self.gpu.read_pixels().expect("offscreen read_pixels")
    }
}
