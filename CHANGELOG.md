# Changelog

All notable changes to the Mighty IDE. The IDE is written in
[Mighty](https://github.com/hassard0/Mighty) (`src/main.mty`) and rendered with
[Vello](https://github.com/linebender/vello); every language friction point is
logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md)
(lessons L1–L46).

## v0.3.0

A code-reading, layout, and workspace pass — all shim-side, Vello-rendered,
driven by `src/main.mty`. ~649 shim tests; clean `clippy -D warnings`.

### Editing & layout
- **Split editor** (`Ctrl+\`): side-by-side editor panes; focus a pane with
  `Ctrl+1` / `Ctrl+2`, click a pane to focus it.
- **Save conveniences** (Settings, opt-in): trim trailing whitespace on save,
  ensure a final newline, and timed auto-save.

### Code-reading visual polish
- **Bracket-pair colorization**: matched `()[]{}` colored by nesting depth with a
  theme-derived rainbow palette; unmatched/extra brackets show an error color.
  Toggle **Bracket Colors** (Settings, default on).
- **Indent guides**: faint vertical guide lines at each indentation level (carried
  across blank lines), with the cursor block's active level brightened. Toggle
  **Indent Guides** (Settings, default on).
- **Interactive minimap**: clicking the minimap jumps the editor to the matching
  source line (tall files compress so the whole file maps across the strip), with
  a clearer viewport rectangle over the visible range.

### Typography
- **Real bold/italic font faces**, used semantically — italic comments, bold
  headings and chrome — rather than synthesized slants.

### Markdown
- **Live Markdown preview** (`Ctrl+Shift+V`): a themed, live-updating split-pane
  render reusing the split-editor machinery.

### Workspace / Open Folder
- The workspace root is now an explicit, settable concept. **File: Open Folder…**
  (`Ctrl+Shift+O`, command palette, or the Welcome screen) opens a native Windows
  folder picker (with a typed-path prompt fallback) and re-roots the file tree,
  Quick-Open index, project Search, git, and Agents discovery to the chosen folder.
- **Recent Folders** (MRU, cap 10) persist across restarts; reopen from the Welcome
  screen's "Recent Folders" column or **File: Open Recent**.
- The explorer header shows the active workspace name.

### Quick-fix lightbulb
- A lightbulb appears in the editor gutter when the cursor's line has available
  code actions; clicking it (or `Ctrl+.`) opens the code-actions menu at that line.
  The "has actions" check is debounced (refreshes on cursor-line-change / idle) so
  the language server isn't spammed.

### Keyboard Shortcuts overlay
- **Keyboard Shortcuts reference + remapping** (`Ctrl+Shift+/`): a searchable list
  of every command with its current binding; router-routed commands are remappable
  to an `Alt`+letter chord, with conflict detection. Overrides persist to
  `%APPDATA%/mighty-ide/keybindings.toml`.

### Notes
- Still all shim-side over the scalar `extern c` ABI; the editor key ladder gained
  no new top-level arms (the `mui_chord` router and arm-folding keep it under the
  mty parse-stack ceiling — L37/L38). Wiring the shortcuts overlay surfaced a new
  mty parse trap: unary `!` binds tighter than a call, so `!fn(args)` mis-parses
  (L46). The authoritative editor **text model remains shim-side**
  (`crates/mighty-ui-sys/src/editor.rs`), the L28 codegen workaround.

## v0.2.0

A large feature pass — all shim-side, Vello-rendered, driven by `src/main.mty`.
~546 shim tests; clean `clippy -D warnings`.

### Editing & Multi-cursor
- Multi-cursor / multiple selections: add caret at next occurrence (`Ctrl+D`),
  add caret above/below (`Ctrl+Alt+Up/Down`), toggle caret on `Alt+Click`.
- Snippets: type a prefix + `Tab` to expand a template with navigable tab-stops.

### Navigation & Code-reading
- Universal Quick-Open (`Ctrl+P`): fuzzy files + MRU, with `>` command, `@`
  symbol, and `:` line modes in one overlay.
- Sticky scroll (pinned enclosing scopes) and peek definition (`Alt+F12`,
  inline framed preview).
- Outline, Problems, and an interactive breadcrumb code-nav bar.

### Language Intelligence
- Multi-language support: config-driven syntax highlighting + a generic LSP
  bridge across 15 languages, in addition to first-class Mighty.

### AI
- Inline AI ghost-text completions (Copilot-style), debounced, with
  generation-id cancel and word-wise partial accept; force with `Alt+\`.
- (Existing) streaming Anthropic copilot Agents panel (`Ctrl+Shift+A`) +
  inline ask (`Ctrl+I`).

### Source Control
- Full git client wired into the IDE: branch switcher, push / pull / fetch,
  per-hunk stage/unstage (reconstructed unified patches), and a blame gutter
  (`Alt+B`), on top of the existing status panel + inline diff (`Ctrl+Shift+G`).

### Run · Test · Debug
- Debugger (DAP): a shim-side DAP client driving `mty dap`, breakpoints, run
  controls, call stack + variables, and the Run-and-Debug view
  (`F5` / `F10` / `F11`).
- Test runner panel: shim-side `mty-test` parser + results model
  (`Ctrl+Shift+T`).

### Web
- Web Playground / "Run in Browser" (`Alt+W`): build the active file to
  `wasm32-web` and serve it (web-game packages via `mty serve`, or a
  `mty build --target wasm32-web` + static-server fallback), scrape the URL,
  open the browser, stop affordance. Sample: `examples/webspin/`.

### Workspace & UX
- Welcome screen, toast notifications, and Zen / focus mode (`Alt+Z`).
- Mighty Agents panel (`Alt+G`): static agent-system topology, run, and
  live `mty inspect`.
- Centralized `mui_chord` router so new chords add no new top-level key-ladder
  arms (works around the mty parse-stack ceiling — see L37/L38).

### Notes
- The authoritative editor **text model is still shim-side**
  (`crates/mighty-ui-sys/src/editor.rs`) as a workaround for the native
  `Vec`-growth codegen bug (L28). That codegen fix is now merged in Mighty, so
  the model can move back into Mighty source — a localized change, since Mighty
  already owns the event loop, key routing, and command dispatch.

## v0.1.0

Initial public release of the Mighty IDE.

- **Editing** — live edit/save, undo/redo, syntax highlighting + current-line
  band + gutter, comment toggle, brace-aware auto-indent, bracket/quote
  auto-close + match, duplicate/move-line, word motion, in-file find & replace.
- **Navigation & intelligence** — go-to-line/def, jump-back, hover,
  autocomplete, signature help, rename (`F2`), code actions, live `mty check`
  diagnostics — all over Mighty's own `mty-lsp`.
- **Workspace** — tabs, file tree, project Search, Source Control (git) with an
  inline diff view, command palette, Run panel (streamed `mty run`), live
  Settings.
- **AI** — streaming Anthropic copilot panel + inline ask (`ANTHROPIC_API_KEY`).
- **Themes** — Vivid Modern / Aurora Glass / Warm Studio, live-switchable.
- **Terminal** — integrated ConPTY shell with a VT parser.
