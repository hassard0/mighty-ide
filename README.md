# Mighty IDE

**A native, GPU-vector-rendered code editor written in [Mighty](https://github.com/hassard0/Mighty), rendered with [Vello](https://github.com/linebender/vello), and dogfooding the language by building its own development environment in it.**

The UI is drawn every frame as a Vello scene: gradients, rounded corners, drop
shadows, diagnostic underlines, anti-aliased text, panels, command surfaces,
and editor chrome are all rendered by the app. The editor orchestration lives
in Mighty source (`src/main.mty`) and calls a Rust rendering/services shim
through a scalar `extern c` ABI. Mighty is the first-class language target, with
the editor architecture kept open to other languages through LSP, snippets,
formatting, run/test/debug adapters, and project tooling.

![Mighty IDE](screenshots/24-debug.png)

## Quick Start

Mighty IDE is a native desktop app. The checked-in source builds the editor
loop from `src/main.mty` and the Rust `mighty-ui-sys` shim, then packages the
runtime files under `dist/`.

For a local Windows build:

```powershell
.\build-ide.ps1 -Mty C:\path\to\mty.exe
.\target\main.exe samples\hello.mty
```

For a clean Windows release package:

```powershell
.\package-win.ps1 -Mty C:\path\to\mty.exe
dist\mighty-ide-win64\mighty-ide.exe samples\hello.mty
```

macOS and Linux packages are built by `./package-macos.sh` and
`./package-linux.sh` on their matching native hosts. They are not derived from
the Windows ZIP because the app ships a native executable and native shim
library for each OS.

## Features

Full keybinding reference: [KEYBINDINGS.md](KEYBINDINGS.md). Release history: [CHANGELOG.md](CHANGELOG.md).

## Documentation

- [BUILDING.md](BUILDING.md): toolchain setup, local builds, native packaging
  commands, and final release order.
- [KEYBINDINGS.md](KEYBINDINGS.md): complete editor shortcut reference.
- [CHANGELOG.md](CHANGELOG.md): implementation history and release notes.
- [docs/platform-packaging.md](docs/platform-packaging.md): clean-binary rules
  for Windows, macOS, and Linux release packages.
- [docs/release-verification.md](docs/release-verification.md): per-archive
  verification checklist.
- [docs/release-evidence.md](docs/release-evidence.md): upload evidence
  template for generated hashes, sizes, manifests, and launch results.
- [docs/binary-release-status.md](docs/binary-release-status.md): concise
  publish/hold/unbuilt decision rules.
- [docs/release-readiness.md](docs/release-readiness.md): final source and
  binary readiness checklist for all platforms.
- [docs/final-release-handoff.md](docs/final-release-handoff.md): final
  stop-pass handoff contract.

## Release Binaries

Release archives are generated under `dist/` and are intentionally not
committed. A clean release package must be built on the same operating system
that will run it because Mighty IDE ships a native executable plus a native
`mighty-ui-sys` dynamic library.

The source-controlled release contract is this README, `BUILDING.md`,
`KEYBINDINGS.md`, `CHANGELOG.md`, the package scripts, and the release
documents under `docs/`. Commit those files before packaging. Generated
evidence belongs to the ignored package directory, `PACKAGE-MANIFEST.txt`, the
archive size, the archive SHA-256, and the final handoff response.

From a Windows-hosted stop pass, only the Windows PE package can be proven clean
locally. macOS and Linux remain `unbuilt` until native macOS and Linux hosts or
matching CI runners run their own package scripts, scan the archives, write
manifests, and launch the packaged apps from the same source commit.

| Platform | Package command | Archive | Clean-binary requirement |
|----------|-----------------|---------|--------------------------|
| Windows x64 | `.\package-win.ps1` | `dist\mighty-ide-v0.3.0-win64.zip` | PE `mighty-ide.exe` and PE `mighty_ui_sys.dll`; no sidecars; no `.dylib` or `.so`; launch from `dist\mighty-ide-win64` |
| macOS | `./package-macos.sh` on macOS | `dist/mighty-ide-v0.3.0-macos.tar.gz` | Mach-O app executable and Mach-O `.dylib`; no sidecars; no `.exe`, `.dll`, or `.so`; launch from the app bundle |
| Linux x64 | `./package-linux.sh` on Linux | `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` | ELF executable and ELF `.so`; no sidecars; no `.exe`, `.dll`, or `.dylib`; launch from the package directory |

Before packaging, make the Mighty compiler explicit. The packagers require
`mty --version` to report v0.47.0 or newer and fail before build work starts if
the selected compiler is missing or stale. On Windows, set `MIGHTY_MTY` or pass
`-Mty` to the PowerShell packager, then build from the clean committed tree:

```powershell
.\package-win.ps1 -Mty C:\path\to\mty.exe
```

The package scripts remove the previous same-version archive before building,
write `PACKAGE-MANIFEST.txt` with source commit and native payload hashes, scan
the finished archive for build byproducts and foreign native files, and bundle
the README, build notes, keybindings, changelog, release verification docs, and
samples. Do not publish placeholder archives or rename a package from another
OS.

Final stop-pass order:

1. Commit README, docs, changelog, package scripts, and source.
2. Rebuild the Windows package from that exact commit.
3. Record the Windows ZIP size and SHA-256.
4. Launch the packaged Windows executable from `dist\mighty-ide-win64`.
5. Record macOS and Linux as `unbuilt - native runner unavailable for this
   pass` unless their native package runs completed during this same pass.
6. Stop. Any source change after packaging makes the archive stale.

Ignored files under `dist/` are evidence only for the source commit named in
their packaged `PACKAGE-MANIFEST.txt`. Existing archives from earlier commits
must not be carried forward; rerun the native package script after the final
source commit and report only the archive produced by that run.

### Editing & Multi-cursor
- Live edit / save (Ctrl+S), Save As (Ctrl+Shift+S), Save All (Ctrl+Alt+S), and New File... (Ctrl+N, native file picker) with syntax coloring, a current-line band, line-number gutter, click-to-place cursor, mouse-wheel + cursor-following scroll
- New File rejects existing directory targets with explicit `not a file`
  feedback instead of reporting them as ordinary existing files
- Opening a file above 16 MiB produces a lightweight read-only large-file
  preview instead of allocating the full payload into the text editor; Reload
  and Revert use the same limit
- Undo / redo (Ctrl+Z / Ctrl+Y), clipboard copy/cut/paste (Ctrl+C/X/V), select all (Ctrl+A), select current line (Ctrl+L), typing-run coalescing
- Copy now has a pure editor preflight that reports whether the active
  selection or current line is copyable without touching the OS clipboard,
  matching cut/paste command-state checks and keeping read-only previews
  copyable
- Command Palette copy/cut/paste rows reflect the active editor state, including
  empty copy targets and read-only previews
- Clipboard reads are capped at 4 MiB before paste/snippet expansion accepts the
  text, so oversized platform clipboard output fails with explicit feedback
  instead of being inserted into editor or terminal surfaces
- Command Palette Save reports dirty duplicate tabs before dispatch, matching
  the runtime `Save skipped: duplicate edits` guard
- Command Palette Save and Save As name read-only preview targets before
  dispatch, matching the runtime save guard
- Command Palette Save All reports dirty duplicate tabs that will be skipped
  before dispatch, including the exact all-skipped runtime summary
- Command Palette Save All uses the runtime `No unsaved files` no-op language
  when there is nothing to write
- Command Palette edit rows, including undo/redo, indentation, line movement,
  duplication, deletion, join-line, and comment toggles, report read-only
  previews as unavailable while navigation and selection remain usable
- Command Palette language and AI edit rows report read-only previews up front:
  Find remains available while Replace, autocomplete acceptance, Rename Symbol,
  code-action edits, and inline AI edits are unavailable
- Hover, Go to Definition, Signature Help, Rename Symbol, Code Actions,
  diagnostics refresh, and explicit completion requests report missing
  configured language servers with actionable `lsp.toml` feedback instead of
  collapsing the state into generic empty-result messages, silent clean Problems
  states, or optimistic fallback edits
- LSP diagnostics treat malformed, missing, or unsupported `severity` payloads
  as warnings, keeping questionable server data visible without overstating it
  as editor errors
- Command Palette language-server rows show missing configured server feedback
  before dispatch for Hover, Go to Definition, Peek Definition, Signature Help,
  Rename Symbol, and Code Actions
- Command Palette Trigger Autocomplete reports when semantic LSP completions are
  unavailable and the command will rely on buffer-word fallback
- Command Palette Force Ghost Completion mirrors Inline AI runtime availability,
  including disabled settings, missing `ANTHROPIC_API_KEY`, and in-flight
  requests before the command is launched
- Command Palette Dismiss Ghost Completion reports when no inline AI ghost text
  is visible, matching the command's runtime no-op feedback
- Command Palette file-backed commands explain unsaved scratch buffers before
  language lookup, code actions, format, run, test-at-cursor, debug, or
  browser-run actions need a saved path
- Command Palette Run Tests mirrors the test runner's target rules, explaining
  when an untitled tab needs either a saved file or a Mighty workspace target
- Command Palette Format, Run, Test, Debug, Run in Browser, Mighty Agents, and
  New Project rows report broken `MIGHTY_MTY` overrides before dispatch instead
  of waiting for formatter, topology, scaffold, or spawn failures
- Command Palette rows for Format, Run, Test, Debug, and Run in Browser report
  read-only previews as unavailable before the command is launched
- Command Palette Run lifecycle rows mirror Run panel no-op states, including
  no process to stop, empty output, and closed panel
- Command Palette Testing lifecycle rows mirror Testing panel no-op states,
  including no test run to stop, empty results, and closed panel
- Command Palette Web lifecycle rows mirror Web Playground no-op states,
  including no running server, missing URL, empty output, and closed panel
- Web Playground fallback build output is capped at 4 MiB per stdout/stderr
  stream, so failing `mty build --target wasm32-web` commands keep useful output
  without allowing unbounded capture
- Native Windows file/folder/save/project dialog helpers cap PowerShell
  stdout/stderr at 64 KiB per stream before accepting a selected path
- Command Palette Agents lifecycle rows mirror Mighty Agents no-op states,
  including empty run output and closed panel
- Command Palette Terminal lifecycle rows mirror integrated terminal no-op
  states, including empty visible buffer and closed terminal
- Command Palette Problems lifecycle rows mirror Problems panel no-op states,
  including empty diagnostics and closed panel
- Command Palette Search and Outline lifecycle rows mirror Search and Outline
  no-op states, including empty search queries, stale or missing replacement
  results, empty result/symbol lists, and closed panels
- Command Palette refresh rows for Explorer, Problems, Outline, Source Control,
  and Mighty Agents describe when they will reveal a panel, rescan live state,
  clear scratch diagnostics, hit a missing configured language server, or hit a
  missing git repository
- Command Palette Search toggle rows say which search field will receive focus
  when switching between query and replace
- Command Palette Debug lifecycle rows mirror Run and Debug no-op states,
  including unavailable pause, step, restart, and clear-breakpoint actions,
  idle stops, empty sessions, and closed panel
- Command Palette Source Control and Explorer lifecycle rows mirror no-op
  states, including clean stage/unstage sets, missing staged changes or commit
  messages, empty commit-message drafts, and closed panels
- Command Palette Git Switch Branch, Push, Pull, and Fetch rows report missing
  git repositories before dispatch, matching Source Control stage, commit, and
  refresh actions
- Source Control repository discovery, status refresh, branch listing, and
  single-file diff reads cap Git stdout/stderr at 2 MiB per stream so unusual
  repository output cannot allocate unbounded panel buffers
- Command Palette Keyboard Shortcuts lifecycle rows mirror no-op states,
  including closed overlays, selected shortcuts already using defaults, and
  empty override sets
- Command Palette close rows for Settings, Diff, Peek, and Markdown Preview
  mirror already-closed runtime feedback before dispatch
- Command Palette transient close rows for AI Copilot, Sidebar, Color Theme,
  Hover, Signature Help, Code Actions, Find & Replace, Autocomplete, Command
  Palette, and Quick Open mirror their no-open runtime feedback before dispatch
- Command Palette utility and cancel rows for notifications, Reopen Closed Tab,
  bottom dock, prompt input, unsaved-change confirmation, branch picker,
  breadcrumb menu, and snippets mirror their empty/no-open runtime feedback
  before dispatch
- Command Palette layout rows for the sidebar and bottom dock describe whether
  they will open, hide, resize, cycle to the next preset, or are already at the
  requested preset
- Command Palette open/focus rows for View panels, Terminal, Settings, Color
  Theme, Keyboard Shortcuts, and Markdown Preview describe already-open
  surfaces before dispatch
- Command Palette Markdown preview rows report non-Markdown active buffers
  before dispatch, matching the runtime preview guard even when a preview pane
  is already open
- Command Palette Git Blame toggle rows mirror active and scratch-buffer
  runtime feedback before dispatch, including stale missing or directory-backed
  active targets
- Command Palette folding rows mirror no-op states for missing foldable blocks,
  already-folded documents, and documents with nothing folded
- Command Palette dedicated close rows for Welcome and Git Blame mirror
  already-closed/hidden runtime feedback before dispatch
- Scalar clipboard feedback from Mighty coalesces with ordinary copy/cut/paste
  toasts instead of stacking generic `Copied` notifications
- Toggle line comment (Ctrl+/), Tab/Shift+Tab indent and outdent, auto-indent on Enter (brace-aware), bracket/quote auto-close + skip-over + empty-pair backspace, bracket-match highlight
- Duplicate line/selection (Ctrl+Shift+D), delete current line (Ctrl+Shift+K), join line (Ctrl+J), move line up/down (Alt+↑ / Alt+↓), word-wise and document-boundary motion (Ctrl+←/→, Ctrl+Home/End), word deletion (Ctrl+Backspace/Delete), smart Home, Shift+motion selection
- In-file find & replace (Ctrl+H), find with match highlighting (Ctrl+F)
- Find & Replace stale keyboard, replace, and close-click routes report when the
  bar has already closed
- Format document guards unsaved scratch buffers and dirty duplicate tabs with
  target-specific feedback, rejects binary/read-only previews, and rejects
  directory targets with explicit `not a file` feedback before spawning the
  formatter
- Binary/read-only previews stay navigable and selectable, while mutating edit
  commands, Save As target selection, Rename, and Delete report why the preview
  cannot be changed
- Save, Save As, Save All, and auto-save reject directory targets with explicit
  `not a file` feedback, and staged save, tab-level saves, Save As, Save All,
  plus auto-save report non-directory parent paths before writing
- Scalar `Saved` feedback from Mighty coalesces with ordinary save feedback
  instead of stacking beside stale save warnings
- **Multi-cursor** — select word / add caret at next occurrence (Ctrl+D), add caret above/below (Ctrl+Alt+↑/↓), toggle caret on Alt+Click
- **Snippets** — type a prefix + Tab to expand a template with navigable tab-stops; stale direct tab-stop routes report when no snippet session is active, and malformed imported tab-stop/capture indexes do not saturate into impossible snippet state
- **Save conveniences** — opt-in trim-trailing-whitespace, ensure-final-newline, and timed auto-save (Settings)

### Navigation & Code-reading
- **Universal Quick-Open (Ctrl+P)** — fuzzy files + MRU, with `>` command, `@` symbol, and `:` line modes in one overlay
- Quick Open file accepts reject directory targets from stale indexes instead
  of opening empty tabs or reporting them as missing files
- Quick Open command-mode accepts report closed-panel, wrong-mode, empty, and
  stale-row misses instead of silently returning no command
- Command Palette and Quick Open stale click and keyboard routes report when
  their overlay has already closed
- Command palette (Ctrl+Shift+P), fuzzy-filtered
- Go-to-line (Ctrl+G), go-to-definition (F12, cross-file), jump-back (Ctrl+−)
- Jump Back empty-history feedback coalesces with other navigation toasts
  instead of stacking stale code-navigation messages
- **Peek definition (Alt+F12)** — inline framed definition preview; cross-file
  previews above 4 MiB are skipped before reading
- **Sticky scroll** — pinned enclosing scopes
- **Outline, Problems, and an interactive breadcrumb** code-nav bar; Problems
  rows, breadcrumb file jumps, and Peek Definition navigation reject directory
  targets from stale indexes instead of opening empty tabs
- Peek Definition stale scroll routes report when the preview has already
  closed instead of mutating hidden preview state
- Problems closed-panel row, close-hit, header-action, scroll, and row-open
  routes report that the panel is already closed instead of activating retained
  hidden diagnostics
- Outline closed-panel row, header-action, and row-open routes report that the
  panel is already closed instead of jumping through retained hidden symbols
- Breadcrumb menu stale keyboard and click routes report when the dropdown has
  already closed
- **Split editor (Ctrl+\)** — side-by-side panes, focus a pane with Ctrl+1 / Ctrl+2
- **Bracket-pair colorization + indent guides** — nesting-depth rainbow brackets, faint per-level guides with an active-block highlight
- **Interactive minimap** — click to jump; tall files compress so the whole file maps across the strip
- Tabs (Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+W, click), file-tree sidebar (Ctrl+B), native Open File (Ctrl+O) with typed-path fallback when the picker is unavailable
- Explorer closed-panel header, row-hit, row-open, toggle, and collapse routes
  report when the panel has already closed, while expand/collapse actions
  report invalid rows, stale directory targets, and replaced directory rows
  instead of silently leaving the tree unchanged
- Native Open File rejects stale or directory picker results with the same
  target-specific feedback as typed Open File
- Rename Active File preserves tab bindings, reports stale missing sources
  without attempting the move, and rejects directory source or destination
  paths with explicit `not a file` feedback; destination name collisions report
  as rename failures instead of generic file-creation conflicts
- Delete Active File requires exact basename confirmation, protects dirty
  buffers, reports stale missing targets without closing the tab, and rejects
  directory targets with explicit `not a file` feedback
- **Close Saved Tabs**, **Close Other Saved Tabs**, and directional close-left /
  close-right cleanup remove tab clutter while preserving dirty buffers
- Command Palette tab-management rows for moving, sorting, closing saved tabs,
  closing duplicate tabs, reload, and revert mirror their no-op runtime feedback
  before dispatch, including stale missing or directory-backed reload/revert
  targets, dirty reload blockers that name the active file, and dirty Close Tab
  blockers that name the tab requiring confirmation
- Command Palette active-file utility rows for Reveal and Copy Path/Name/Folder
  report scratch buffers with target-specific wording, and stale missing or
  directory-backed active targets before dispatch; Show in File Manager also
  reports unavailable platform reveal support before launch
- Command Palette Rename/Delete rows report stale missing or directory-backed
  active targets before dispatch, runtime Rename/Delete refuse read-only
  previews, and Delete names dirty duplicate blockers before asking for
  confirmation
- Unsaved-changes confirmation stale click, cancel, save, and discard routes
  report when the confirmation has already closed
- Bottom dock stale close, preset, and resize affordances report when no dock is
  open, while ordinary editor clicks stay quiet
- Sidebar stale resize drag and finish routes report when the sidebar has
  already closed and release the retained resize capture
- **Reopen Closed Tab** (Ctrl+Alt+T) restores the most recently closed editor tab,
  including tabs removed by cleanup commands, without collapsing split-pane
  layouts
- **Duplicate Active Tab** clones the current editor tab next to itself from the
  live buffer, including dirty state and cursor context, without collapsing
  split-pane layouts
- Direct tab switches, next-tab, and previous-tab entry points retarget the
  focused split pane without collapsing or rebinding the other pane
- Direct tab load, store, and dirty-state entry points reject stale tab indexes with
  visible feedback instead of silently ignoring the routed update
- Split-pane retargeting rejects stale pane or tab indexes with visible
  feedback before changing the layout, so exported pane calls cannot bind a pane
  to a missing tab
- File-opening surfaces, including Open File, New File, Explorer rows, Quick
  Open, Welcome recents, definition targets, and New Untitled, follow the same
  focused-pane binding rule so split layouts stay coherent outside the main UI
  router
- Panel navigation surfaces, including Search results, Problems, Source
  Control, Run/Test output, Debug breakpoints, Agents nodes, breadcrumbs, and
  Peek Definition targets, use that same focused-pane binding rule
- **Move Active Tab Left/Right** (Ctrl+Shift+PageUp/PageDown) reorders tabs from
  the keyboard or command palette while preserving split-pane document bindings
- **Sort Open Tabs by Name** alphabetizes tab clutter without losing the active
  document or split-pane document bindings
- **Close Duplicate Tabs** collapses clean duplicate file tabs while preserving
  dirty duplicate buffers and split-pane bindings
- **Reload Active File from Disk** refreshes clean file-backed tabs after
  external edits while protecting dirty buffers and rejecting directory targets
  with explicit `not a file` feedback
- **Revert Active File from Disk** intentionally discards local edits and reloads
  the file-backed tab from disk, while refusing directory targets before any
  dirty buffer is discarded
- Project-wide Search panel (Ctrl+Shift+F), with stale directory targets
  rejected before opening a tab, files above 4 MiB skipped before scanning, and
  Replace All reporting dirty, changed, missing, or failed-write files instead
  of silently skipping them
- Search closed-panel input, focus, run/replace, click, row-hit, and row-open
  routes report that the panel is already closed instead of mutating retained
  hidden results

### Language Intelligence
- Hover info (Ctrl+K), autocomplete (Ctrl+Space — semantic LSP completions + buffer words)
- Generic LSP initialization advertises supported completion, hover, and
  definition capabilities across the registry-backed multi-language client and
  the Mighty hover/definition client, so servers can return markdown/plaintext
  hover content, definition links, and rich completion items without relying on
  accidental defaults
- Generic LSP signature-help initialization advertises markdown/plaintext
  signature documentation and parameter label offset support without claiming
  trigger-context support
- Generic and Mighty-specific LSP rename initialization advertise
  `prepareRename` support without claiming change-annotation handling
- Generic and Mighty-specific LSP outline initialization advertise hierarchical
  document-symbol support and SymbolKind values without claiming unsupported
  label/tag metadata
- Mighty-specific signature-help and code-action initialization advertises the
  parsed documentation, parameter-offset, preferred-action, disabled-action, and
  literal-kind metadata without claiming unsupported resolve/context flows
- Generic LSP execute-command handshakes only acknowledge
  `workspace/applyEdit` requests that own `params.edit`, so metadata-only
  payloads cannot be reported as applied edits; malformed apply-edit objects
  without usable IDs are skipped so later valid server requests can still run,
  and fractional numeric JSON-RPC IDs are rejected instead of being truncated to
  integer prefixes
- Mighty-specific hover, definition, completion, signature-help, rename,
  code-action, and outline response waiting use the same numeric JSON-RPC ID
  token-boundary checks, so malformed `id:2.5` responses cannot short-circuit
  the real `id:2` result
- Mighty-specific navigation response IDs and definition target coordinates
  reject overflow, so malformed hover/definition payloads cannot saturate into
  bogus response matches or Go to Definition jump targets
- LSP payload numeric fields for signature help, parameter offset labels,
  prepare-rename ranges, workspace edits, and diagnostics require complete
  unsigned integer tokens without overflow, so malformed payloads cannot
  saturate into clamped signature indexes, rename prompts, or workspace-edit
  coordinates
- LSP diagnostics reject coordinate values that overflow the shim parser or do
  not fit the signed editor diagnostic model, so bad server payloads cannot
  wrap into negative Problems locations
- Outline document-symbol error codes, SymbolKind values, and range start lines
  require complete integer tokens without overflow, so malformed LSP outline
  payloads cannot trigger scanner fallback, classify rows, or jump to truncated
  line numbers
- Debugger Adapter Protocol numeric fields for response sequence IDs, stopped
  thread IDs, thread rows, stack frames, and exit codes require complete integer
  tokens without overflow, so malformed debugger payloads cannot redirect stack
  or thread state
- Headless/screenshot automation numeric environment controls require complete
  decimal tokens, so plus-prefixed or partial `MUI_WIDTH`,
  `MUI_SCREENSHOT_W`, `MUI_SCREENSHOT_FRAME`, `MUI_LIGHTBULB_AUTOOPEN`,
  `MUI_SETTINGS_AUTOOPEN`, and `MUI_HEADLESS_FRAMES` values cannot silently
  drive malformed capture geometry, target frames, or seeded rows
- Integrated terminal CSI/OSC numeric parameters require complete decimal
  tokens, so plus-prefixed cursor counts, SGR colors, and palette indices cannot
  silently move the cursor, style text, or answer/update palette slots
- Persisted keyboard shortcut overrides require complete numeric tokens, so
  plus-prefixed `cmd_<id>` keys or `mods:codepoint` values in
  `keybindings.toml` cannot silently remap commands at startup
- Persisted editor settings and zoom values require complete decimal tokens, so
  malformed plus-prefixed `font_size`, `tab_width`, or zoom files cannot
  silently alter startup preferences
- Quick Open go-to-line input and Testing summary counts require complete
  unsigned decimal tokens, so plus-prefixed or overflowing values cannot
  silently drive malformed navigation or test-result metadata
- Semantic autocomplete preserves LSP `label`, `kind`, `filterText`,
  `sortText`, `preselect`, `deprecated`, `commitCharacters`, `insertText`,
  `textEditText`, safe same-line `textEdit.range` /
  `InsertReplaceEdit.insert` spans, `CompletionList.itemDefaults` for commit
  characters and edit ranges, snippet bodies, provider `detail`,
  `labelDetails`, and provider `documentation`; malformed plus-prefixed numeric
  fields cannot claim snippet format, kind labels, or replacement ranges, and
  the client advertises those supported completion-item capabilities during LSP
  initialization, so generic server results can match, classify, rank, choose
  an initial row, display warnings, commit through punctuation, replace
  qualified prefixes, insert, and describe different text without placeholder
  signatures
- Semantic autocomplete treats `CompletionItem.tags` entries as deprecated
  markers only when the array value is a complete integer token, so malformed
  numeric prefixes cannot mark fresh completion rows as deprecated
- Empty explicit autocomplete requests name the active file or scratch buffer
  and cursor position, so no-candidate feedback stays actionable in multi-tab
  sessions
- Empty explicit autocomplete requests also report when a configured language
  server is unavailable, while passive typing completion still falls back
  quietly to buffer words
- Language-server availability and empty-response notices coalesce with
  autocomplete feedback instead of stacking stale completion toasts
- Autocomplete accept misses report visible feedback when no suggestion is open
  instead of silently doing nothing
- Autocomplete stale click, move, and accept routes report visible feedback when
  the dropdown has already closed
- Autocomplete drawing preserves the caller's overlay layer state, so suggestion
  rows do not accidentally demote later overlay text or chrome
- Staged save failures report visible feedback for scratch targets and dirty
  open tabs instead of only logging to stderr
- Staged and active load failures report `Load failed: <file>: <reason>`, with
  directory targets named as `not a file` instead of leaking platform read
  errors
- Diagnostics refresh failures report the missing checker command or configured
  language server instead of looking like a clean file with no diagnostics
- Mighty compiler report locations are treated as 1-based in Problems and Run
  output; malformed `line:0` or `column:0` records are ignored instead of
  becoming clickable jumps to the top-left of the file
- Mighty compiler diagnostic locations require complete unsigned decimal
  line/column tokens, so plus-prefixed or overflowing `mty check` reports
  cannot drive bogus underline or jump positions
- Mighty diagnostics output is capped at 4 MiB per stdout/stderr stream, so
  noisy or broken `mty check` runs cannot allocate unbounded compiler output
  before diagnostics are parsed
- Agents live-inspect output is capped at 4 MiB per stdout/stderr stream, so
  noisy runtime snapshots or transport errors cannot allocate unbounded
  `mty inspect --json` output before snapshot parsing
- Inline Source Control diff output is capped at 8 MiB per stdout/stderr stream,
  so oversized `git diff` output cannot allocate unbounded patch text before
  hunk parsing
- Diff hunk stage/unstage output is capped with the same budget, so failing
  `git apply` commands keep useful stderr without allowing unbounded output
  capture
- Formatter output is capped at 4 MiB per stdout/stderr stream, so failing
  `mty fmt` commands keep useful stderr without allowing unbounded output
  capture
- New Project output is capped at 4 MiB per stdout/stderr stream, so failing
  `mty new` scaffold commands keep useful detail without allowing unbounded
  output capture
- Source Control accepts only unsigned decimal git divergence counts, so
  malformed `ahead` / `behind` porcelain cannot display signed or exponent-style
  branch sync totals
- Source Control git action output is capped at 2 MiB per stdout/stderr stream,
  so push, pull, fetch, and branch commands cannot allocate unbounded process
  output before the toast summary is produced
- Debug adapter framing requires a positive unsigned decimal `Content-Length`
  header, preventing malformed frames from being treated as empty debugger
  messages
- Debug adapter frames are capped at 16 MiB before body allocation, so broken
  adapters cannot reserve enormous buffers with oversized `Content-Length`
  headers
- Generic LSP stdout streams are capped and discarded when oversized, so noisy
  or broken language servers cannot feed partial responses or diagnostics after
  crossing the response/diagnostic byte budgets
- Mighty LSP completion, hover/definition, and language-action response streams
  share the same fail-closed cap behavior, so oversized `mty lsp` output cannot
  feed partial semantic results into editor surfaces
- Inline git diffs validate unified hunk ranges before showing or applying a
  hunk, so malformed negative starts or bad count fields cannot produce bogus
  line numbers or patchable rows
- Web Playground auto-open only latches loopback `http` / `https` URLs with a
  positive decimal port, so external docs links or malformed URL tokens in build
  output are ignored
- Web Playground `MIGHTY_WEB_PORT` overrides require the same positive decimal
  token shape as scraped server URLs, so plus-prefixed, zero, overflowing, or
  partial port values fall back to the default
- Generic diagnostics report stale non-active source files, including directory
  targets as `not a file`, instead of treating failed disk reads as empty clean
  buffers
- Definition jumps reject directory targets from stale or malformed resolver
  results instead of opening an empty tab
- Empty explicit code-action requests name the active file or scratch buffer
  and cursor position, so no-quick-fix feedback points to the queried site
- Code Actions explain untitled scratch buffers before requesting or applying
  actions, matching the Command Palette's saved-file guidance
- Applying or moving code actions with no active quick-fix menu reports
  `No code action menu open`, while active selection misses keep their own
  feedback
- Code-action stale click and move routes report when the quick-fix menu has
  already closed
- Code Actions preserve LSP `isPreferred`, initially select the first preferred
  actionable fix, and mark preferred rows in the quick-fix menu
- Generic LSP code-action initialization advertises supported literal action
  kinds, preferred-action metadata, and disabled-action metadata without
  claiming unsupported resolve/data flows
- Editor popup drawing, including Quick Open, Hover, Signature Help, Rename,
  Code Actions, and Find & Replace, preserves the caller's overlay layer state
  so later overlay text and chrome stay on the overlay layer
- Empty explicit hover requests name the active file and cursor position, so
  no-hover feedback identifies the queried site
- Empty explicit signature-help requests name the active file and cursor
  position, so no-signature feedback is visible and actionable
- Empty explicit rename requests name the active file or scratch buffer and
  cursor position, so non-renamable locations are clear
- Rename input and commit misses report visible feedback when no rename input is
  open, the proposed name is empty or unchanged, the buffer is unsaved, or
  neither LSP nor fallback edits can be produced
- Symbol Rename and Code Action Apply reject binary/read-only previews before
  applying LSP or fallback edits
- Symbol-rename cancel and stale input routes coalesce with other
  code-intelligence feedback instead of file-rename toasts
- Symbol rename keeps the cursor-derived target when a language server returns
  an unusable prepare-rename range
- Code actions that require a saved file name scratch buffers before refusing
  to run
- Fix-all code actions reject directory pre-save targets with explicit `not a
  file` feedback instead of raw platform write errors
- Workspace-edit code actions skip directory file targets and non-directory
  parent paths with explicit non-file feedback, and keep active buffers
  unchanged when a workspace-edit write cannot be committed
- Hover, definition, peek definition, and signature help name scratch buffers
  before refusing unsaved LSP lookups
- Signature help (Ctrl+Shift+Space), rename symbol (F2), code actions / quick-fix (Ctrl+.)
- **Quick-fix lightbulb** — a gutter bulb appears when the cursor's line has code actions; click it (or Ctrl+.) to open them (debounced so the server isn't spammed)
- Live `mty check` diagnostics — gutter dots + wavy underlines
- First-class Mighty intelligence over its own `mty-lsp`, plus **multi-language support**: config-driven highlighting + a generic LSP bridge across 15 languages

### AI
- AI copilot Agents panel (Ctrl+Shift+A) — streaming Anthropic chat
- AI Copilot stale focused send, scroll, Backspace, and newline routes report
  when the panel has already closed instead of mutating hidden chat state
- Inline ask (Ctrl+I)
- **Inline AI ghost-text** (Copilot-style) — debounced suggestions, force with Alt+\, word-wise partial accept (Ctrl+→)
- Inline AI ghost-text accept and dismiss commands report when no suggestion is
  visible instead of silently doing nothing
- Reads `ANTHROPIC_API_KEY` from the environment

### Source Control
- Source Control panel (Ctrl+Shift+G) — git status + inline diff view
- Source Control stale header, message-clear, stage, and row routes report when
  the panel has already closed and cannot activate retained hidden status
- **Stage All / Unstage All / Commit Staged** command-palette actions for
  keyboard-first index and commit flow
- **Branch switcher + push / pull / fetch**
- Branch switcher stale click and keyboard routes report when the picker is
  already closed
- **Per-hunk stage / unstage** (reconstructed unified patches)
- Failed per-hunk stage or unstage attempts refresh the inline diff, closing it
  when the stale hunk no longer exists
- **Blame gutter (Alt+B)** — porcelain-parsed, per-file cached, with strict
  timestamp/timezone token boundaries and 8 MiB per-stream output caps before
  parsing external `git blame` output
- Source Control row opens reject directory targets from stale git status
  entries instead of opening an empty tab, then refresh the status list so
  repeated clicks do not keep targeting a non-file row
- Direct active-file inline diff opens report stale missing or directory-backed
  targets before invoking git, matching Source Control row diff handling

### Run · Test · Debug
- Run panel (Ctrl+Shift+R) — background `mty run` with streamed output + clickable diagnostics
- Run refuses binary/read-only previews before starting `mty run`, matching the
  rest of the file-backed process commands
- Run output stale header, row click, and scroll routes report when the panel
  has already closed and cannot activate retained hidden output
- Stale Run output diagnostic rows keep naming the missing source file on
  repeated clicks after the row has been demoted
- Run output jumps reject directory targets from stale tool output instead of
  opening an empty tab, then demote the row while preserving the precise
  `not a file` feedback on repeated clicks
- Run output labels require positive decimal line/column tokens, so malformed
  streamed diagnostics do not look like clickable source locations
- Run output clickable diagnostic rows require complete unsigned decimal
  line/column tokens, so plus-prefixed or overflowing streamed locations remain
  plain output instead of becoming bogus jump targets
- **Test runner panel (Ctrl+Shift+T)** — shim-side `mty-test` parser + results model
- Run Tests and Run Test at Cursor reject binary/read-only previews before
  starting `mty test`, so preview tabs cannot accidentally drive package tests
- Testing panel stale toolbar, result-row click, and scroll routes report when
  the panel has already closed and cannot activate retained hidden results
- Test result jumps reject directory targets from stale suite output instead of
  opening an empty tab
- **Debugger (DAP)** — a shim-side client driving `mty dap`: breakpoints, run controls, call stack + variables, Run-and-Debug view, plus palette commands for start/continue, pause, restart, stop, and step controls (F5 start-continue / Shift+F5 stop, F10 step-over, F11 / Shift+F11 step-into/out)
- Run and Debug stale toolbar, breakpoint, and sidebar click routes report when
  the panel has already closed and cannot mutate retained hidden debug state
- Breakpoint jumps reject directory targets from stale debug rows instead of
  opening an empty tab, then prune the stale breakpoint so repeated clicks no
  longer target a non-file row
- Panel and navigation overlay drawing, including Prompt, Problems, Branch
  Switcher, AI Copilot, Breadcrumb menus, Sticky Scroll, and Peek Definition,
  preserves the caller's overlay layer state so later overlay text and chrome
  stay on the overlay layer

- Test result jumps name missing stale suite files instead of reporting a
  generic unresolved row

### Web
- **Run in Browser (Alt+W)** — build the active file to `wasm32-web` and run it in the browser via `mty serve` (web-game packages) or a `mty build --target wasm32-web` + static-server fallback; streams build/serve output, scrapes the served URL, opens the default browser, stop affordance, explains untitled scratch buffers before launch, and reports stale Web click/scroll routes after the panel closes. Sample: `examples/webspin/`

- Diff and Markdown preview stale scroll routes report when their surface has
  already closed instead of mutating hidden viewport state, and Markdown preview
  stale close-click routing reports the closed preview instead of returning
  silently

### Workspace & UX
- **Explicit Workspace + Open Folder (Ctrl+Shift+O)** — native folder picker (typed-path fallback only when the picker is unavailable) re-roots the file tree, Quick-Open, Search, git, and Agents; typed and picked folder paths preserve distinct missing-folder versus `not a folder` feedback; **New Folder** (Ctrl+Shift+N) creates workspace directories; active files can be revealed in the IDE file tree, shown in the OS file manager, or copied as absolute, relative, basename, or directory text from the command palette; **Open Recent** shows recent files or folders from the shared recents picker, reports missing or stale rows with target-specific feedback, and warns when recent-history persistence fails; explorer header shows the active workspace
- **View commands** open Explorer, Search, Source Control, Outline, Run and Debug,
  Testing, Run Output, Problems, AI Copilot, Terminal, and Web Playground from
  the command palette, matching the activity rail, status chip, and docked panels.
- New Folder reports when the target path is an existing file instead of saying
  the folder already exists
- Prompt fallback keyboard and submit routes report when the bottom prompt is
  already closed
- **Live Markdown preview (Ctrl+Shift+V)** — themed, live-updating split-pane render with ordered-list overflow preserving structure and one-based fallback numbering
- **Keyboard Shortcuts overlay (Ctrl+Shift+/)** — searchable command/binding reference with router-command remapping (persists to `keybindings.toml` under the shared Mighty IDE config directory and warns if override writes fail)
- Keyboard Shortcuts keyboard, remap, reset, and click actions report
  closed-overlay, fixed-row, and already-default misses instead of silently
  ignoring the command
- Welcome screen with first-run New File, New Project, Open File, Open
  Folder, Quick Open, and Command Palette actions; clickable toast notifications
  with a command-palette clear-all action, **Zen / focus mode (Alt+Z)**
- Open Recent file rows reject directory targets from stale recents instead of
  opening empty tabs or reporting them as missing files, and Welcome keeps
  already-rendered stale file rows target-specific on click
- Open Recent folder rows reject file-backed stale recents with `not a folder`
  feedback instead of reporting them as ordinary missing folders, while
  already-rendered Welcome folder rows still report the precise stale target
  before their hit snapshot is cleared
- Typed Open Folder missing-path and non-folder validation failures coalesce
  with other Open feedback instead of stacking beside stale open-folder toasts
- Recent file and folder persistence warnings coalesce with other Open Recent
  feedback instead of stacking beside stale recent-row messages
- **Mighty Agents panel (Alt+G)** — static agent-system topology, run, and live `mty inspect` when the Mighty runtime control socket is available
- Agents live-inspect worker counts, agent IDs, mailbox depths, and mailbox
  high-water counters require complete integer tokens, so malformed runtime
  snapshots cannot truncate numeric prefixes or overflow into plausible rows
- Agents Run rejects binary/read-only previews before starting `mty run`, so
  preview tabs cannot launch agent programs accidentally
- Mighty Agents stale header, topology-row, scroll, and node-open routes report
  when the panel has already closed and cannot activate retained hidden topology
- Agents topology jumps reject directory targets from stale scan results instead
  of opening an empty tab, then refresh the topology so repeated clicks do not
  keep targeting a non-file row
- Settings panel (Ctrl+,) — live font size / tab width / word wrap / minimap / theme / bracket colors / indent guides / save conveniences
- Settings panel stale move, click, adjust, and toggle routes report when the
  panel has already closed
- Command overlay drawing, including Command Palette, Keyboard Shortcuts, Color
  Theme, and Settings, preserves the caller's overlay layer state so later
  overlay text and chrome stay on the overlay layer
- Integrated terminal (Ctrl+`) — a real ConPTY shell with a VT parser
- Terminal closed-panel header-clear routes report that the Terminal is already
  closed instead of silently ignoring stale header clicks
- Terminal stale key, text, raw-byte, scroll, focus, and mouse routes report
  when the shell is not open instead of dropping focused input without feedback

### Themes
Three live-switchable design systems, all rendered through Vello:
- **Vivid Modern** (default) — near-black surfaces, indigo accents
- **Aurora Glass** — dark glass over an aurora gradient
- **Warm Studio** — a light, warm-paper theme
- Theme changes apply live and report a visible warning if the preference could
  not be persisted, so a failed config write does not look like a durable choice.
- Color theme picker stale click, move, and apply routes report when the picker
  has already closed, without changing the active theme.

Bundled fonts: **JetBrains Mono** (code) + **Bricolage Grotesque** (UI chrome), both SIL OFL (`fonts/`). **Real bold/italic faces** are used semantically — italic comments, bold headings and chrome — not synthesized slants.

## Gallery

| | |
|---|---|
| ![Split editor](screenshots/43-split.png) | ![Brackets & indent guides](screenshots/44-brackets-guides.png) |
| ![Interactive minimap](screenshots/45-minimap.png) | ![Live Markdown preview](screenshots/46-markdown.png) |
| ![Typography](screenshots/47-typography.png) | ![Open Folder](screenshots/48-openfolder.png) |
| ![Quick-fix lightbulb](screenshots/49-lightbulb.png) | ![Keyboard Shortcuts](screenshots/50-shortcuts.png) |
| ![Debugger](screenshots/24-debug.png) | ![Multi-cursor](screenshots/34-multicursor.png) |
| ![Inline AI ghost-text](screenshots/31-ghost.png) | ![Aurora Glass theme](screenshots/13-theme-aurora.png) |

## Architecture

Two layers, one clean boundary:

- **The IDE itself — `src/main.mty`, written in Mighty.** It owns the main event loop, input routing, command dispatch, and editor orchestration, driving the shim each frame via scalar `extern c` calls.
- **`crates/mighty-ui-sys` — a Rust `cdylib` shim.** It owns the window (winit), GPU surface (wgpu), the **Vello** vector scene (gradients / rounded rects / shadows / glyph runs), text shaping, file I/O, the integrated terminal (`portable-pty`), the `mty-lsp` client, git/diff, the Run process, and the Anthropic AI client. Each frame, Mighty's draw calls build a display list that is replayed into one `vello::Scene` (`src/vello_ui.rs`).

**Why a scalar-only ABI:** Mighty v0.36's `extern c` can pass only scalars — no strings, pointers, or structs across the boundary. So strings, pixels, paths, and buffers live shim-side and are driven by scalar getters/setters. See the lessons doc (L17-L25, with ongoing notes through L1202) for the language constraints that shaped this design.

## Build & Run

Prerequisites:
- The **`mty` compiler** from [hassard0/Mighty](https://github.com/hassard0/Mighty) v0.47.0 or newer (build with `cargo build -p mty-cli --bin mty`)
- A **Rust** toolchain
- **clang** (the linker `mty build` drives)

```sh
./build-ide.sh                  # cargo-builds the shim cdylib + arena runtime, then `mty build src/main.mty`
./target/main.exe path/to/file  # open a file (defaults to ./scratch.mty)
```

`build-ide.sh` sets `MTY_LINKER=clang`, builds `mighty-ui-sys` as a DLL, stages the import lib + the bumpalo arena runtime, copies the DLL beside the exe, and runs `mty build`.
Build and package scripts run `mty --version` first and reject stale compilers before they can emit noisy parser errors against `src/main.mty`.

- `MTY_LINKER` — point `mty build` at clang (the build script sets it).
- `ANTHROPIC_API_KEY` — enables the AI copilot panel.
- On a tight disk, set `CARGO_INCREMENTAL=0` and clear `target/debug/incremental` if a link fails on space.

See [BUILDING.md](BUILDING.md) for the exact toolchain paths and commands.

## Release Packages

Generated release binaries stay out of git. The checked-in package scripts build
into `dist/` from a clean committed tree, validate the native payloads for the
host OS, reject compiler/linker sidecars, reject obvious foreign-platform
binaries, bundle the project docs, and then scan the finished archive before
reporting success. The source tree remains binary-clean because platform
archives and native payloads are generated only under ignored build/package
directories.

The release claim is intentionally narrow and evidence-based: a platform binary
is clean only after that platform's own package script has run on the matching
native OS or CI runner, the finished archive scan has passed, the packaged app
has launched from inside the assembled package, and `PACKAGE-MANIFEST.txt`
records the source commit, generated time, native payload hashes and sizes,
archive name, and clean-binary checks.

| Platform | Command | Archive | Native payload checks |
|----------|---------|---------|-----------------------|
| Windows x64 | `.\package-win.ps1` on Windows | `dist\mighty-ide-v0.3.0-win64.zip` | PE `mighty-ide.exe` and PE `mighty_ui_sys.dll`; staged tree and ZIP contain no sidecars (`.pdb`, `.lib`, `.exp`, `.ilk`, `.obj`, `.o`, `.a`, `.rlib`, `.log`, `.debug`, `.map`, `.dSYM`) or `.dylib`/`.so` files |
| macOS | `./package-macos.sh` on macOS | `dist/mighty-ide-v0.3.0-macos.tar.gz` | Mach-O app executable and `.dylib`; staged tree and tarball contain no sidecars (`.pdb`, `.lib`, `.exp`, `.ilk`, `.obj`, `.o`, `.a`, `.rlib`, `.log`, `.debug`, `.map`, `.dSYM`) or `.exe`/`.dll`/`.so` files |
| Linux x64 | `./package-linux.sh` on Linux | `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` | ELF executable and `.so`; staged tree and tarball contain no sidecars (`.pdb`, `.lib`, `.exp`, `.ilk`, `.obj`, `.o`, `.a`, `.rlib`, `.log`, `.debug`, `.map`, `.dSYM`) or `.exe`/`.dll`/`.dylib` files |

Every package includes `RUN.txt`, `README.md`, `KEYBINDINGS.md`, `CHANGELOG.md`,
`BUILDING.md`, `LICENSE`, `docs/platform-packaging.md`,
`docs/release-verification.md`, `docs/release-evidence.md`,
`docs/binary-release-status.md`, and `docs/final-release-handoff.md` alongside
the runtime payload, plus
`PACKAGE-MANIFEST.txt` with the source commit, native payload hashes, sizes,
and clean-binary verification.

Final handoff rule:

- A platform is publishable only when its package script ran on the matching
  native OS or CI runner, the archive-level clean-binary scan passed, and the
  packaged executable launched from inside the assembled package directory.
- "Clean binaries" is a per-platform artifact claim, not a source-tree claim:
  the committed repo contains scripts and docs, while the verified native
  payloads are generated under ignored `dist/` directories during packaging.
- This Windows checkout can produce and verify only the Windows x64 package.
- macOS/Linux script review from Windows covers syntax, host gating, bundled
  docs, and clean-artifact policy only; it does not create clean Mach-O or ELF
  binaries.
- macOS and Linux are release-ready only after `package-macos.sh` and
  `package-linux.sh` run on native macOS/Linux infrastructure and their
  packaged apps launch there.
- If native macOS or Linux infrastructure is unavailable, record that platform
  as `unbuilt`; do not upload placeholder archives or copied native payloads.

For a stop/pass release handoff, use
[`docs/final-release-handoff.md`](docs/final-release-handoff.md) as the source
of truth for platform decisions and
[`docs/binary-release-status.md`](docs/binary-release-status.md) for the
concise clean-binary status. Use
[`docs/release-verification.md`](docs/release-verification.md) for the evidence
rules, then fill [`docs/release-evidence.md`](docs/release-evidence.md) with
the final upload record. Exact archive size and SHA-256 values are generated
during packaging and must match the bundled `PACKAGE-MANIFEST.txt`.

Each platform package must be built and smoke-tested on its native OS or a
matching CI runner. Do not reuse Windows DLLs, macOS dylibs, or Linux shared
objects across platforms. See
[`docs/platform-packaging.md`](docs/platform-packaging.md) for the full package
contract and verification commands, and
[`docs/release-verification.md`](docs/release-verification.md) for the evidence
rules to apply to each published archive. Use
[`docs/release-evidence.md`](docs/release-evidence.md) for the concise upload
record and
[`docs/final-release-handoff.md`](docs/final-release-handoff.md) for the final
stop condition and per-platform publish decision.

Current Windows-hosted finalization state:

| Platform | Decision from this checkout | Required before upload |
|----------|-----------------------------|------------------------|
| Windows x64 | `publish` after `.\package-win.ps1` and packaged launch pass here | ZIP size/hash, PE header checks, staged-tree and ZIP sidecar/foreign-payload scans, `PACKAGE-MANIFEST.txt` with source commit, packaged launch |
| macOS | `unbuilt` unless a macOS runner completed this pass | Native macOS runner must run `./package-macos.sh`, verify Mach-O payloads, scan the tarball, and launch the app bundle |
| Linux x64 | `unbuilt` unless a Linux runner completed this pass | Native Linux runner must run `./package-linux.sh`, verify ELF payloads, scan the tarball, and launch from the package directory |

Final release wording should be precise:

- "Windows binary is clean" means the current committed tree was packaged by
  `.\package-win.ps1`, the staged directory and ZIP scans passed, the native
  payloads are PE files, `PACKAGE-MANIFEST.txt` was generated, and the packaged
  executable launched from `dist\mighty-ide-win64`.
- "macOS binary is clean" means the same source commit was packaged on macOS,
  the payloads are Mach-O files, the tarball scans passed, the manifest was
  generated, and the app bundle launched on macOS.
- "Linux binary is clean" means the same source commit was packaged on Linux,
  the payloads are ELF files, the tarball scans passed, the manifest was
  generated, and the packaged executable launched on Linux.
- If this pass has no macOS or Linux runner, those platform decisions are
  `unbuilt`, not `publish` or `hold`.

Stop-pass checklist:

1. Commit source, tests, README, changelog, and release documentation first.
2. Rebuild the Windows package from that clean commit with `.\package-win.ps1`.
3. Check macOS/Linux package scripts for syntax and wrong-host refusal from this
   checkout if native runners are unavailable, then record both platforms as
   `unbuilt`.
4. Confirm the generated ZIP and staged package contain only Windows PE native
   payloads and no compiler/linker sidecars.
5. Launch `dist\mighty-ide-win64\mighty-ide.exe` with
   `dist\mighty-ide-win64` as the working directory.
6. Confirm `dist\mighty-ide-win64\PACKAGE-MANIFEST.txt` names the same source
   commit as the final source commit being handed off.
7. Record the ZIP size and SHA-256 in the final handoff, then stop. macOS and
   Linux remain `unbuilt` until their own native runners produce and smoke-test
   Mach-O and ELF archives.

Do not commit generated archive hashes, generated timestamps, or package
manifest values back into this README. Those values belong to the ignored
package directory, external release upload note, and final handoff response for
the package run that produced them.

## Dogfooding Mighty

The IDE is the **forcing function** for maturing Mighty: every place the language fights us while building real native software is logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md), so each friction point can be promoted into a Mighty issue / RFC. That feedback loop (lessons L1-L1202) has already driven real fixes in the Mighty compiler — for example the native `Vec`-growth codegen bug ([L28](docs/mighty-language-lessons.md)), the `extern c` scalar ABI (L17), the LSP-client discipline (L24–L25), the parse-stack ceiling worked around by the `mui_chord` router (L37–L38, and the `!fn_call(args)` precedence trap found wiring the shortcuts overlay, L46), the native runtime/linking gaps captured while hardening Windows packaging (L50–L51), and the repeated prompt-string staging pressure from file-operation commands (L52).

## Status & known caveats

Pre-alpha but functional: the editor builds, launches, and edits real files live.

The one architectural caveat is the **authoritative text model**. Under native `mty build`, a Mighty `Vec` grown in a loop came back empty (the confirmed codegen bug [L28](docs/mighty-language-lessons.md)), so the text model (lines + cursor + selection + scroll + dirty, per tab) currently lives shim-side (`crates/mighty-ui-sys/src/editor.rs`) and Mighty drives every edit through scalar `mui_ed_*` ops. This is a workaround, not a design choice — now that the codegen bug is fixed it can move back to Mighty, a localized change since Mighty already owns the event loop, key routing, and command dispatch. Visual and interactive polish is ongoing.

## License

MIT — see [LICENSE](LICENSE).
