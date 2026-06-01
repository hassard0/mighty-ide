# Changelog

All notable changes to the Mighty IDE. The IDE is written in
[Mighty](https://github.com/hassard0/Mighty) (`src/main.mty`) and rendered with
[Vello](https://github.com/linebender/vello); every language friction point is
logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md)
(lessons L1–L58).

## v0.3.0

A code-reading, layout, and workspace pass — all shim-side, Vello-rendered,
driven by `src/main.mty`. ~649 shim tests; clean `clippy -D warnings`.

### Editing & layout
- **Split editor** (`Ctrl+\`): side-by-side editor panes; focus a pane with
  `Ctrl+1` / `Ctrl+2`, click a pane to focus it.
- **Save All** (`Ctrl+Alt+S`): writes every dirty file-backed tab at once and
  leaves untitled buffers dirty with a toast that says they still need Save As.
- **Close Saved Tabs**: clears clean tab clutter while preserving every dirty
  buffer.
- **Close Other Saved Tabs**: keeps the active tab plus dirty tabs and removes
  the rest of the clean tab clutter.
- **Close Saved Tabs to the Left/Right**: directional tab cleanup around the
  active file, still preserving dirty buffers.
- **Reopen Closed Tab** (`Ctrl+Alt+T`): restores the most recently closed editor
  tab after an accidental close, including clean tabs removed by cleanup commands.
- **Duplicate Active Tab**: clones the current editor tab next to itself from the
  live buffer, preserving dirty state and cursor context.
- **Move Active Tab Left/Right** (`Ctrl+Shift+PageUp/PageDown`): reorders the
  active tab while preserving split-pane document bindings.
- **Sort Open Tabs by Name**: alphabetizes open tabs while preserving the active
  logical document and split-pane document bindings.
- **Close Duplicate Tabs**: collapses clean duplicate file tabs while preserving
  dirty duplicate buffers and split-pane bindings.
- **Reload Active File from Disk**: refreshes clean file-backed tabs after
  external edits while refusing to overwrite dirty buffers.
- **Revert Active File from Disk**: intentionally discards local edits and
  reloads the file-backed tab from disk.
- **Open Recent** now opens the recent picker when either recent files or recent
  folders exist, instead of falling back to an open-folder prompt for file-only
  history.
- **New File commands are explicit**: `Ctrl+N` is labeled
  **File: New File at Location** and opens the native picker, while the palette
  also exposes **File: New Untitled Tab** for a scratch buffer and
  **Explorer: New File in Workspace** for creating under the current workspace.
- **New File at Location now honors the chosen location**: the native picker can
  create and open a file outside the current workspace, while the Explorer
  workspace command remains scoped to the open folder.
- **Explorer new-file routing is workspace-scoped**: the Explorer header and
  **Explorer: New File in Workspace** command now use a dedicated workspace
  picker, instead of sharing the arbitrary-location File > New flow.
- **Welcome now names the file picker intent**: the first quick action is
  labeled **New File at Location** so it matches the native path picker behavior.
- **Compact sidebar leaves more room for work**: narrow windows now reduce the
  sidebar to 160px, giving terminal, debug, palette, and welcome views more
  usable width instead of crowding dock/header content.
- **Settings footer is cleaner in compact windows**: the modal no longer crams
  shortcut helper text into the footer, avoiding tiny overlapping copy at narrow
  widths.
- **Explorer header title no longer overlaps actions**: compact sidebar headers
  now measure and ellipsize the workspace title before the New File/New Folder
  buttons.
- **Markdown Preview is readable in compact windows**: opening the live preview
  now temporarily hides the sidebar when a split preview would leave cramped
  columns, then restores the sidebar when the preview closes.
- **Save on untitled buffers opens Save As**: direct Save now routes through the
  native Save As picker for pathless tabs instead of failing silently.
- **Welcome respects bottom docks**: opening Terminal/Run-style lower panels now
  gives the Welcome screen a reduced layout budget so quick actions do not draw
  underneath the dock.
- **Bottom docks reserve editor space consistently**: Run, Web, Problems, and
  Terminal now share one lower-dock owner model, so editor/ghost rows no longer
  keep flowing underneath testing or output drawers.
- **Borderless resize is more discoverable**: side/bottom resize hit targets are
  larger, the top tab row keeps a smaller resize band, and bottom corner grips
  are drawn into the custom frame.
- **Run drawer header text is measured**: long active filenames now ellipsize
  before the status pill instead of colliding with it.
- **Problems drawer text is measured**: empty-state text, file group headers,
  and diagnostic messages now ellipsize by shaped UI-font width instead of
  fixed character estimates.
- **Testing sidebar text is measured**: summaries, durations, test names, suite
  badges, and failure details now budget against shaped UI text instead of
  fixed character estimates.
- **Workspace New File uses a native picker**: Explorer's New File button and
  "File: New File in Workspace" now open a SaveFileDialog-style file picker,
  falling back to the typed prompt only when native dialogs are unavailable.
- **Prompt text is measured**: long typed paths, rename targets, and delete
  confirmations now keep their useful tail and fit within the bottom prompt
  band instead of running under neighboring chrome.
- **Status bar text is measured**: branch names, ahead/behind counts, problem
  counters, cursor position, encoding, indentation, and language pill layout now
  use shaped UI-font widths so compact windows do not let left and right status
  clusters collide.
- **Tab labels are measured**: long basenames now fit to the real space before
  the dirty indicator and close icon, keeping tab controls visually clear and
  clickable.
- **Explorer filenames are measured**: long tree row names now fit before git
  status badges instead of relying on fixed character counts.
- **Source Control rows are measured**: commit text, branch labels, changed-file
  names, and directory tails now fit against real stage/action space.
- **File-operation toasts clear stale state**: newer save/open/create/rename/delete
  results now replace older same-operation notifications instead of stacking
  contradictory text.
- **Result toasts clear stale state**: newer test, web-run, format, and
  navigation-result notifications now replace older same-operation messages, so
  a later success does not leave an old failure visible in the toast stack.
- **Prompt fallback clicks are less modal**: clicking outside the visible bottom
  prompt now dismisses it, so typed-path fallbacks do not leave the IDE feeling
  stuck after a native-dialog fallback.
- **Welcome layout respects the Explorer**: the first-run welcome surface now
  switches to a single-column layout when the editor body is narrowed by the
  sidebar, avoiding clipped recent-file columns.
- **Bottom dock resize is visible and draggable**: Terminal, Run, Web, and
  Problems share a top-edge grab handle, and mouse dragging resizes the lower
  dock while the editor row budget updates with it.
- **Sidebar drawers have size commands**: Search, Source Control, Outline,
  Debug, Testing, Agents, and Explorer can now switch between compact, default,
  and wide widths from the command palette without resizing the whole window.
- **Bottom docks have a shared close button**: the lower Terminal/Run/Web/
  Problems drawer now exposes the same visible header close affordance instead
  of relying on rail toggles or Escape.
- **Bottom dock chrome respects zoom/DPI**: shared dock actions and right-aligned
  header content use the physical viewport converted back to logical pixels, so
  drawer buttons and labels do not drift off-screen under UI zoom.
- **Unsaved tab close uses the confirmation modal**: closing a dirty tab or
  quitting with dirty work now requires the explicit Save/Discard/Cancel overlay
  instead of a fragile repeat-close/repeat-quit shortcut.
- **Native file dialogs open in context**: Open File, New File, and Save As now
  start in the active file's folder when available, falling back to the
  workspace root for untitled tabs.
- **New File is no longer ambiguous**: Ctrl+N and the Welcome New File action
  now open the native file picker to create a named file; scratch buffers remain
  available through the explicit New Untitled File command.
- **Welcome compact layout is cleaner**: narrow editor bodies no longer draw
  shortcut hints at the far edge of the start actions, avoiding clipped text
  when the Explorer sidebar is open.
- **Window resizing stops before broken chrome**: the borderless desktop window
  now has a minimum inner size so the custom title bar, rail, tabs, and status
  bands cannot be squeezed into clipped controls.
- **Window maximize is command reachable**: the command palette now exposes
  **Window: Toggle Maximize**, so borderless-window layout control does not
  depend on hitting the custom titlebar button.
- **Window minimize is command reachable**: **Window: Minimize** gives the
  borderless app a command-palette path for minimizing without targeting the
  custom titlebar button.
- **New Folder rejects invisible out-of-workspace picks**: the native New Folder
  flow now warns when the selected folder is outside the current workspace
  instead of reporting success without changing Explorer.
- **New File rejects invisible out-of-workspace picks**: the workspace New File
  picker now refuses paths outside the current workspace, avoiding a tab that
  opens successfully while Explorer and Quick Open cannot show the file.
- **Open Folder proves the workspace changed**: the live Windows harness now
  drives the command-palette Open Folder flow to a separate picked folder and
  fails unless the IDE records that folder as the active workspace.
- **Bottom dock has a command close path**: **View: Close Bottom Dock** now
  closes whichever lower Run/Web/Problems/Terminal dock is active, so layout
  recovery does not depend on hitting the small header close button.
- **AI copilot has a command close path**: **View: Close AI Copilot** closes the
  right-docked chat panel without clearing its transcript or requiring rail
  toggle behavior.
- **Sidebar has a command close path**: **View: Close Sidebar** deterministically
  hides the left drawer without depending on toggle state or changing the active
  Explorer/Search/SCM/Testing panel.
- **Minimap hides in narrow split panes**: compact split-editor and Markdown
  preview layouts now preserve source readability instead of drawing the minimap
  over code text.
- **Peek stays inside compact windows**: the inline Peek Definition card now
  uses visible-surface bounds and clipped header budgets, so minimum-size
  captures do not cut off the card's right edge or command hint.
- **Outline visual QA shows real symbols**: the Outline gallery case now seeds a
  representative Mighty file before refreshing symbols, so alignment reviews see
  nested functions, agents, and structs instead of an empty scratch-state panel.
- **Brand mark is cleaner at taskbar scale**: the app icon and in-app rail /
  Welcome logo now use a simpler teal-rail + violet-baseline monogram instead
  of a corner dot that read like a notification badge at small sizes.
- **Bottom prompts have real chrome**: find/goto/path fallback prompts now show
  a right-side `Enter / Esc` hint and a clickable close button, with text clipped
  before those controls instead of running underneath them.
- **Find/replace bar is mouse-closable**: the dedicated two-row replace surface
  now gets the same right-side affordances, clips long fields before the controls,
  and routes the close icon through Mighty instead of leaving mouse clicks inert.
- **Signature Help highlight is measured**: the active parameter bubble now uses
  shaped code-font extents for the prefix and parameter instead of fixed character
  math, so compact popups do not draw the highlight or accent text off by a few
  pixels.
- **Welcome starts with New File**: the first-run Start list now puts the native
  New File picker first, followed by Open File/Open Folder, so creation is not
  buried behind navigation commands.
- **AI Copilot is visibly closable**: the right-docked AI panel now draws a
  header close button with matching mouse hit-test routing, instead of relying
  on only rail toggles or the command palette close command. The borderless
  titlebar drag strip now passes that button through to Mighty when AI is open.
- **Modal overlays are visibly closable**: Settings and Keyboard Shortcuts now
  draw header close buttons with explicit mouse routing and harness traces, so
  dismissing them does not depend on `Esc` or clicking outside the card.
- **Theme Picker is visibly closable**: the Color Theme modal now has the same
  header close affordance and mouse trace as the other overlays, with close
  cancelling any live preview and restoring the original theme.
- **Branch Switcher is visibly closable**: the Git branch overlay now has a
  header close button that works in both filter and create-branch modes instead
  of relying on outside-click or Escape.
- **Agents panel avoids clipped rows**: the Mighty Agents topology now draws
  only complete visible rows and shows a slim scrollbar thumb when more agents,
  tools, or supervisors continue below the sidebar.
- **AI Copilot code and composer are clearer**: generated code blocks now wrap
  with indented continuation lines, and the bottom composer has stronger chrome
  so the chat panel does not read as a static transcript.
- **Windows UX harness has a strict real-mouse mode**: click and drag checks now
  try to foreground the actual IDE window, move the OS cursor, and use
  `SendInput`; automated sessions that cannot take foreground ownership fall
  back explicitly, while `-StrictRealMouse` fails instead of hiding that gap.
- **Mighty brand mark is cleaner**: the taskbar icon, rail logo, and Welcome
  mark now share a centered accent Mighty glyph without the old side-rail stripe,
  so the first impression reads as an IDE identity instead of a generic tile.
- **New/Open/Save command labels match behavior**: the palette and keybinding
  docs now distinguish picker-backed file creation from untitled scratch tabs,
  removing the contradictory `Ctrl+N` wording.
- **Source Control empty states are actionable**: clean repos now say
  "Working tree clean" with a next-action hint, and non-git folders explain how
  to enable source control instead of showing a dead panel.
- **Startup scratch tabs are virtual**: no-arg launches no longer create
  `scratch.mty` in the working folder, so opening a clean Git workspace does not
  immediately produce an untracked file in Source Control.
- **Markdown Preview is visibly closable**: the rendered preview pane now has a
  header close button that collapses the split preview through the same pane
  machinery as the command route.
- **Breadcrumb visual QA shows the real menu**: the breadcrumb gallery hook now
  seeds a representative Mighty file before file-load startup can overwrite the
  capture state, so screenshot review exercises the actual symbol dropdown
  layout instead of a blank editor.
- **Visual gallery paths are reliable**: the overlay-gallery QA tool now
  normalizes relative executable, workdir, and output paths before launching the
  packaged IDE, avoiding false missing-screenshot failures.
- **Visual gallery defaults are portable**: the overlay-gallery QA tool now
  derives default paths from the repo root instead of hard-coded workstation
  paths.
- **Testing header avoids status-pill collisions**: the Testing sidebar title now
  measures the right-side `idle`/`running`/`failed`/`passed` pill first and fits
  the header label into the remaining space.
- **Windows taskbar identity is explicit**: the desktop app now sets a stable
  Windows AppUserModelID (`Hassard.MightyIDE`) before creating the window, so the
  stamped Mighty icon and taskbar grouping are tied to the IDE instead of a
  transient process identity.
- **Copy Active File Name / Directory** add basename and containing-folder
  clipboard commands alongside absolute and workspace-relative path copies.
- **Save conveniences** (Settings, opt-in): trim trailing whitespace on save,
  ensure a final newline, and timed auto-save.
- **Save All asks for untitled destinations**: dirty untitled buffers now open
  the native Save As picker during Save All instead of being skipped with only a
  follow-up warning.

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
- **Measured breadcrumb text**: folder, file, and symbol segments now advance
  from shaped UI-font widths, keeping separators and dropdown hit-targets aligned
  on long or mixed-width names.
- **Welcome shortcut hints** now use shaped UI-font measurements and hide when
  a compact column cannot fit the label plus shortcut, preventing first-screen
  overlap.
- **Toast messages** now truncate from shaped UI-font measurements instead of a
  fixed character guess, reducing clipped or stale-looking notification text.

### Markdown
- **Live Markdown preview** (`Ctrl+Shift+V`): a themed, live-updating split-pane
  render reusing the split-editor machinery.

### Workspace / Open Folder
- The workspace root is now an explicit, settable concept. **File: Open Folder…**
  (`Ctrl+Shift+O`, command palette, or the Welcome screen) opens a native Windows
  folder picker (with a typed-path prompt fallback only if the picker is
  unavailable) and re-roots the file tree,
  Quick-Open index, project Search, git, and Agents discovery to the chosen folder.
- **Recent Folders** (MRU, cap 10) persist across restarts; reopen from the Welcome
  screen's "Recent Folders" column or **File: Open Recent**.
- The explorer header shows the active workspace name.
- **File reveal commands are explicit**: the command palette now separates
  **Reveal Active File in File Tree** from **Show Active File in File Manager**,
  which launches Explorer/Finder/xdg-open instead of only expanding the IDE tree.
- **Copy Active File Path** copies the active file's full path to the system
  clipboard from the command palette; **Copy Active File Relative Path** copies
  the workspace-rooted slash-normalized path for issues, imports, and docs.
- **Notifications: Clear All Toasts** dismisses the visible toast stack on demand,
  and clicking any toast dismisses that card immediately so stale save/build/error
  messages can be cleared without waiting for expiry.
- **View commands**: Explorer, Search, Source Control, Outline, Run and Debug,
  Testing, Run Output, Problems, AI Copilot, Terminal, and Web Playground are now
  command-palette reachable, not only rail/status-chip/dock-click reachable.
- **Debug commands**: start/continue, pause, restart, stop, step-over, step-into,
  and step-out are now command-palette and Quick-Open command-mode reachable,
  matching the function-key and toolbar controls.
- **Search polish**: project-search preview highlights now use shaped UI-text
  measurements instead of fixed character estimates, so highlighted matches stay
  aligned with proportional glyphs.
- Agents live-inspect notes now reflect the Mighty runtime's Windows named-pipe
  control endpoint work, so the IDE no longer documents Windows as static-only.

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
- **Git: Stage All / Unstage All**: command-palette bulk index actions for
  preparing or clearing a commit without clicking each changed file.
- **Git: Commit Staged**: command-palette commit action that uses the Source
  Control message buffer and refreshes status after a successful commit.

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
  reachable live `mty inspect` when the Mighty runtime control socket is available.
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
