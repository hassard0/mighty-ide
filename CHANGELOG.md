# Changelog

All notable changes to the Mighty IDE. The IDE is written in
[Mighty](https://github.com/hassard0/Mighty) (`src/main.mty`) and rendered with
[Vello](https://github.com/linebender/vello); every language friction point is
logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md)
(lessons L1–L58).

## v0.3.0

A code-reading, layout, and workspace pass — all shim-side, Vello-rendered,
driven by `src/main.mty`. ~649 shim tests; clean `clippy -D warnings`.

### Workspace and file handling
- **Markdown preview palette reflects idempotent reopen**: Command Palette now
  describes Open Preview on an already-open Markdown preview as refreshing the
  open preview instead of implying the command is unavailable.
- **Terminal Clear palette covers missing shell state**: Command Palette now
  reports `Terminal is already closed` for Clear Buffer when the panel is open
  but no terminal backend exists, matching the runtime clear guard.
- **Final release docs freeze binary evidence ownership**: README and release
  docs now spell out that Windows ZIP hashes and launch results are generated
  after the final source commit, while macOS and Linux stay `unbuilt` without
  same-pass native runners.
- **Terminal Clear palette matches closed-panel state**: Command Palette now
  reports `Terminal is already closed` for Clear Buffer when the terminal panel
  is hidden, even if the shim still retains terminal state.
- **Revert palette distinguishes clean files**: Command Palette now reports
  `No local edits; reload from disk` for clean file-backed tabs instead of
  implying that Revert will discard edits when the runtime will only reload.
- **Rename Cancel palette state matches runtime**: Command Palette now reports
  `No rename input open` before dispatch when Rename Symbol has no active
  inline prompt, matching the runtime cancel no-op toast.
- **AI Clear Chat palette state matches runtime**: Command Palette now reports
  when the AI transcript and draft are already empty instead of advertising a
  mutating clear action that will only produce a no-op toast.
- **Agents Inspect names missing compiler overrides**: the live-inspect header
  now rejects a stale `MIGHTY_MTY` override before spawning and surfaces the
  same actionable reason in both the panel note and a warning toast.
- **Agents run rejects stale active targets**: Mighty Agents now refuses to
  run deleted or directory-backed active files before spawning, refreshes stale
  workspace views, and reports the exact target in the warning toast.
- **Breakpoint toggles reject stale active targets**: gutter and Command
  Palette breakpoint toggles now refuse missing or directory-backed active
  files instead of recording unusable breakpoints, with matching palette
  preflight feedback.
- **Command Palette launch preflight names stale targets**: Run, Run in
  Browser, Run Tests, and Run Test at Cursor now report missing or
  directory-backed active targets before dispatch, matching their runtime
  no-spawn guards.
- **Command Palette Debug preflight names stale targets**: Debug Start and
  Debug Restart now report missing or directory-backed targets before dispatch,
  matching the runtime guard that avoids spawning the adapter for stale files.
- **Release docs now include a top-level documentation map**: README now points
  release operators directly to the build, packaging, verification, evidence,
  binary-status, and final-handoff documents bundled with each package.
- **Tab and sidebar lifecycle toasts are regression-covered**: saved-tab
  close no-op feedback and Sidebar open/close feedback now have explicit
  coalescing coverage, preventing stale navigation/layout toasts from stacking.
- **Web Playground lifecycle toasts are regression-covered**: Web output clear,
  server stop, and panel close feedback now have explicit coalescing coverage
  so stale Web toasts are replaced consistently.
- **Source Control diff feedback drops legacy no-row text**: diff-open toast
  coalescing now tracks the runtime `No source control row selected` wording
  instead of a stale hyphenated variant.
- **Fix All duplicate-skip feedback coalesces**: duplicate-edit skip messages
  from Fix all (mty) now replace stale Code Action toasts instead of stacking
  as unrelated feedback.
- **Symbol Rename scopes workspace skip feedback**: skipped workspace-edit
  targets reported by Rename now use rename-scoped toast text, so stale rename
  feedback is replaced instead of leaving a generic Code Action warning beside
  it.
- **Symbol Rename reports non-file workspace skips**: rename commits now surface
  skipped non-file, missing-file, and write failures from workspace edits, and
  avoid undo checkpoints when the active rename target is already a directory.
- **Fix All preflight skips known no-op targets**: Fix all (mty) no longer
  asks Mighty to record an undo point when a directory target or dirty
  duplicate tab will block the fixer before it can run.
- **Fix All names duplicate-edit blockers**: Fix all (mty) now names the file
  whose duplicate unsaved edits prevent applying the fixer.
- **Save All names single duplicate-edit blockers**: when duplicate dirty tabs
  all point at one file, Save All and the Command Palette now name that file
  instead of only reporting a skipped count.
- **Duplicate Save blockers name the target**: plain Save and the Command
  Palette now report the file whose duplicate unsaved edits blocked the write.
- **Command Palette contextual Save As names typed-path fallback**: Save As rows
  now keep typed-path recovery visible for both file-backed and untitled tabs.
- **Command Palette untitled Save names typed-path fallback**: Save now tells
  untitled tabs that typed-path recovery is available if the save picker cannot
  open.
- **Command Palette Save As names typed-path fallback**: Save As now mirrors the
  native save-picker unavailable toast by naming the typed-path recovery path.
- **Command Palette open rows name typed-path fallback**: Open File and Open
  Folder descriptions now match unavailable-picker behavior by naming the
  typed-path recovery path.
- **Command Palette Save All names untitled Save As recovery**: Save All rows
  now detect dirty untitled tabs and say when Save As may be needed, matching
  the runtime unavailable-picker guidance.
- **Save All names Save As recovery after unavailable pickers**: when dirty
  untitled tabs cannot open the native save picker, Save All now says to use
  Save As while preserving dirty tabs and normal file-backed saves.
- **Command Palette new-item rows name typed-name fallback**: New File, New
  File in Workspace, New Folder, and New Project descriptions now match the
  runtime unavailable-picker behavior by naming the typed-name recovery path.
- **New item picker fallback toasts name typed-name recovery**: New File, New
  File in Workspace, New Folder, and New Project now report that the typed-name
  prompt is available when native pickers cannot run, while cancellations
  remain no-op.
- **Final release docs name unavailable Linux shell checks**: README, build
  notes, and release evidence docs now distinguish a Windows host with no WSL
  distribution from native macOS/Linux binary evidence, keeping those platforms
  `unbuilt` until their own runners build and launch the packages.
- **Open picker fallback toasts name typed-path recovery**: Open File and Open
  Folder now report that the typed-path prompt is available when native pickers
  cannot run, while keeping cancelled pickers as no-op cancellations.
- **Agents Run and Inline AI name read-only preview targets**: forced inline AI
  completions and Agents Run now report the protected binary/read-only preview
  by name, with matching Command Palette hints for inline AI edit commands.
- **Test and Debug commands name read-only preview targets**: Run Tests, Run
  Test at Cursor, and Debug Start now report the protected binary/read-only
  preview by name, with matching Command Palette hints.
- **Run commands name read-only preview targets**: Run File and Run in Browser
  now report the protected binary/read-only preview by name, and Command
  Palette hints match the runtime guard.
- **Undo, Redo, and Replace name read-only preview targets**: history and
  in-file replace guards now report the protected binary/read-only preview by
  name, and Command Palette hints match the runtime guard.
- **Editor edit guards name read-only preview targets**: direct editor edits,
  ghost-text accepts, and snippet expansions now report the protected
  binary/read-only preview by name instead of a generic edit failure.
- **Code Actions and Format name read-only preview targets**: applying a code
  action or formatting a binary/read-only preview now reports the protected
  target by name, matching the palette and other write guards.
- **Symbol Rename names read-only preview targets**: committing a symbol rename
  in a binary/read-only preview now reports the protected target by name,
  matching the command palette and file-operation read-only guards.
- **Save As protects read-only previews early**: typed and native Save As now
  reject binary/read-only previews before resolving typed targets or opening a
  picker, keeping the protected target and staged path untouched on disk.
- **Rename/Delete protect read-only previews**: active-file Rename and Delete
  now reject binary/read-only previews before consuming staged prompt input or
  touching the filesystem, and the command palette names the protected target.
- **Command Palette names scratch active-file targets**: Rename, Delete, and
  Copy Active File path/name/directory rows now report the same `(scratch)`
  no-target wording as the runtime guards before dispatch.
- **Command Palette names read-only save targets**: File: Save and Save As now
  name the active read-only preview before dispatch, matching the runtime
  `{name} is read-only in the text editor` save guard.
- **Command Palette matches Save All empty-state wording**: File: Save All now
  reports `No unsaved files` when there is nothing to write, matching the
  runtime no-op toast.
- **Command Palette previews dirty duplicate Save All skips**: File: Save All
  now reports when dirty duplicate file tabs will be skipped before dispatch,
  including the exact `{n} files skipped` summary when every dirty tab conflicts.
- **Command Palette previews dirty duplicate save blockers**: File: Save now
  reports `Save skipped: duplicate edits` when another tab for the same file has
  unsaved edits, matching the runtime save guard before dispatch.
- **Final package docs name the compiler preflight**: README and final handoff
  docs now show how to pass an explicit `mty.exe` to `package-win.ps1` and
  clarify that v0.47.0 or newer is required before a clean Windows binary pass
  can start.
- **Command Palette names dirty close-tab blockers**: Close Tab now names the
  dirty tab that will open the unsaved-work confirmation before dispatch,
  matching the runtime close guard.
- **Command Palette names dirty reload blockers**: Reload Active File now names
  the dirty active file before dispatch, matching the runtime guard that refuses
  to overwrite local edits.
- **Command Palette previews file-manager reveal blockers**: Reveal Active File
  rows now report scratch buffers before dispatch, and Show Active File in File
  Manager reports unavailable platform reveal support before launch.
- **Command Palette previews Markdown preview blockers**: Markdown: Open Preview
  now reports non-Markdown active buffers before dispatch, matching the runtime
  preview guard even when a preview pane is already open.
- **Direct inline diff rejects stale active targets**: opening a diff for the
  active file now reports missing or directory-backed targets before invoking
  git, matching stale Source Control row handling.
- **Command Palette previews git repository blockers consistently**: Switch
  Branch, Push, Pull, and Fetch now report `Not a git repository` before
  dispatch, matching Source Control stage, commit, and refresh rows.
- **Git Blame rejects stale active targets early**: Toggle Blame now reports
  missing or directory-backed active files before invoking git, and the command
  palette mirrors that feedback before dispatch.
- **Command Palette previews stale rename/delete targets**: Rename Active File
  and Delete Active File now report missing or directory-backed active paths
  before dispatch, and Delete names dirty duplicate blockers before asking for
  confirmation.
- **Command Palette previews stale active-file utility targets**: Reveal Active
  File, Show in File Manager, and Copy Active File path/name/directory rows now
  report missing or directory-backed active paths before dispatch.
- **Command Palette previews stale reload/revert targets**: Reload Active File
  and Revert Active File now report missing or directory-backed active paths
  before dispatch instead of first promising a disk reload.
- **Command Palette previews Agents compiler blockers**: Mighty Agents open and
  refresh rows now report when `MIGHTY_MTY` points to a missing compiler before
  dispatch, while Clear Run Output and Close Panel keep their no-op state
  descriptions.
- **Final stop pass records artifact evidence and stops**: README and release
  docs now state that after the Windows ZIP size, ZIP SHA-256, package checks,
  packaged launch result, and macOS/Linux `unbuilt` decisions are recorded,
  further implementation or docs edits belong to a new source/package pass.
- **Command Palette previews broken Mighty compiler overrides**: Format, Run,
  Test, Debug, Run in Browser, and New Project rows now report when
  `MIGHTY_MTY` points to a missing compiler before dispatch, while preserving
  read-only and unsaved scratch-buffer explanations.
- **Command Palette previews autocomplete fallback**: Trigger Autocomplete now
  reports when the configured/default semantic language server is unavailable
  and the command will rely on buffer-word completions.
- **Command Palette previews Problems LSP blockers**: `Problems: Refresh
  Diagnostics` now shows the configured/default language-server availability
  reason before dispatch, matching the runtime refresh toast.
- **Diagnostics refresh surfaces missing LSP servers**: explicit Problems
  refresh for LSP-backed languages now reports unavailable configured/default
  language servers with the same actionable `lsp.toml` feedback as navigation,
  completion, rename, and code-action commands.
- **Command Palette refresh rows describe runtime impact**: Explorer, Problems,
  Outline, Source Control, and Mighty Agents refresh commands now preview panel
  reveal behavior, scratch-buffer diagnostics clearing, topology rescans, and
  missing git repositories before dispatch.
- **Final package evidence checks the source commit**: release templates now
  require `PACKAGE-MANIFEST.txt` to name the same committed README, changelog,
  build notes, and release docs used for the handoff before a package can be
  treated as publishable.
- **Language actions surface missing LSP servers**: Hover, Go to Definition,
  Signature Help, Rename Symbol, Code Actions, diagnostics refresh, and explicit
  completion requests now share actionable `lsp.toml` feedback when a
  configured/default language server is unavailable, instead of reporting
  generic empty results, silent clean Problems states, or opening optimistic
  fallback edits.
- **Command Palette previews missing LSP servers**: language-server command rows
  now show the same missing `lsp.toml` server reason before dispatch for Hover,
  Go to Definition, Peek Definition, Signature Help, Rename Symbol, and Code
  Actions.
- **Final binary handoff matrix is explicit**: README and release docs now state
  the final Windows-hosted publish decision for each platform: Windows x64 can
  publish only after the PE package rebuild, ZIP scan, manifest, hash, and
  packaged launch pass, while macOS and Linux remain `unbuilt` until matching
  native runners produce and launch their Mach-O or ELF archives.
- **Command Palette layout commands explain the next state**: sidebar toggle,
  sidebar width presets/cycle, and bottom-dock height presets now say whether
  they will open, hide, resize, cycle to the next preset, or are already at the
  requested preset.
- **Command Palette Search focus toggle is explicit**: `Search: Toggle Replace
  Field` now says whether it will focus the query field or replace field,
  including when it first opens the Search panel.
- **Command Palette folding commands explain no-op states**: Fold Toggle, Fold
  All, and Unfold All now mirror runtime feedback for missing foldable blocks,
  already-folded documents, and documents with nothing folded.
- **Command Palette Git Blame toggle explains live state**: `Git: Toggle Blame`
  now says when it will hide an active blame gutter and mirrors the runtime
  scratch-buffer feedback before dispatch.
- **Final handoff response is artifact-scoped**: README and release status docs
  now require the final Windows-hosted handoff to report the committed source
  hash, generated Windows ZIP size/hash, package-script checks, packaged launch,
  and explicit macOS/Linux `unbuilt` decisions without committing generated
  archive values back into reusable docs.
- **Final release docs avoid self-referential hashes**: README and release
  status docs now keep generated ZIP hashes out of source control, requiring
  the post-commit package manifest, final ZIP size/hash, and packaged launch
  result to carry the authoritative Windows artifact evidence.
- **Final stop pass is source-and-artifact strict**: README and release docs now
  spell out the current pass boundary: commit the final source, tests, README,
  changelog, and release docs, rebuild and launch the Windows PE package from
  that commit, record ZIP size/hash evidence, keep macOS and Linux `unbuilt`
  without native runners, and stop.
- **Release handoff documents clean native binaries**: README, build notes, and
  release docs now make the final artifact contract explicit: Windows PE
  binaries are publishable only after the Windows package script and packaged
  launch pass, while macOS Mach-O and Linux ELF archives remain unbuilt until
  matching native runners produce, scan, manifest, and launch them.
- **Binary previews explain read-only mode on open**: file-opening routes now
  toast when a binary file is opened as a read-only preview, so the first edit
  denial is not the user's first signal that the buffer is protected.
- **Read-only previews remain inspectable**: cursor motion, scrolling,
  selection, select-all, and multi-caret motion now work in binary previews
  while mutating edit commands remain blocked with explicit feedback.
- **Copy has a pure preflight**: editor copy can now report whether the active
  selection or current line is copyable without touching the OS clipboard,
  matching cut/paste command-state checks while keeping read-only previews
  copyable.
- **Command Palette clipboard rows describe the live editor state**: Copy and
  Cut now tell users whether they will use the selection, current line, or
  nothing, while read-only previews show Copy as available and Cut/Paste as
  unavailable.
- **Command Palette edit rows explain read-only previews**: Undo, Redo,
  indentation, line movement, duplication, deletion, join-line, and comment
  commands now report read-only previews as unavailable directly in the palette.
- **Command Palette language and AI edits explain read-only previews**: Find &
  Replace, autocomplete, Rename Symbol, Code Actions, Inline Ask, and forced
  ghost completion now surface read-only limitations before a mutation path is
  attempted.
- **Command Palette forced ghost completion explains runtime availability**:
  Force Ghost Completion now mirrors the inline AI setting, missing
  `ANTHROPIC_API_KEY`, and in-flight request state before dispatching the
  command.
- **Command Palette ghost dismiss explains empty state**: Dismiss Ghost
  Completion now reports when no inline AI ghost text is visible before
  dispatching the no-op command.
- **Command Palette file-backed commands explain scratch buffers**: Hover,
  Go to Definition, Peek Definition, Signature Help, Rename Symbol, Code
  Actions, Format, Run, Run Test at Cursor, Debug Start, and Run in Browser now
  tell users to save untitled buffers before those file-backed operations.
- **Command Palette Run Tests mirrors workspace target state**: Run Tests now
  tells untitled buffers to save or open a Mighty folder only when neither an
  active file nor discoverable workspace test target exists, matching the test
  runner's launch rules.
- **Command Palette launch commands explain read-only previews**: Format, Run,
  Run Tests, Run Test at Cursor, Debug Start, and Run in Browser now show
  read-only preview limitations directly in the palette instead of waiting for
  execution-time feedback.
- **Command Palette Run lifecycle rows explain no-op states**: Run Stop, Clear
  Output, and Close Panel now mirror Run panel runtime feedback for idle
  processes, empty output, and closed panels.
- **Command Palette Testing lifecycle rows explain no-op states**: Test Stop,
  Clear Results, and Close Panel now mirror Testing panel runtime feedback for
  idle runs, empty results, and closed panels.
- **Command Palette Web lifecycle rows explain no-op states**: Web Stop, Open in
  Browser, Clear Output, and Close Panel now mirror Web Playground runtime
  feedback for idle servers, missing URLs, empty output, and closed panels.
- **Command Palette Debug controls explain no-op states**: Pause, Step Over,
  Step Into, Step Out, Restart, and Clear Breakpoints now mirror Run and Debug
  runtime feedback for non-running or non-paused debug sessions, missing restart
  targets, and empty breakpoint inventories.
- **Command Palette Source Control commands explain no-op states**: Stage All,
  Unstage All, and Commit Staged now mirror SCM runtime feedback for non-git
  workspaces, clean stage sets, missing staged changes, and empty commit
  messages.
- **Command Palette Search commands explain no-op states**: Run Search and
  Replace All now mirror Search runtime feedback for empty queries, prior
  no-result searches, stale results, and replacement attempts before running
  the current query.
- **Command Palette Agents lifecycle rows explain no-op states**: Clear Run
  Output and Close Panel now mirror Mighty Agents runtime feedback for empty
  run transcripts and closed panels.
- **Command Palette Terminal lifecycle rows explain no-op states**: Clear
  Buffer and Close now mirror integrated terminal runtime feedback for empty
  visible buffers and closed terminals.
- **Command Palette Problems lifecycle rows explain no-op states**: Clear
  Diagnostics and Close Panel now mirror Problems runtime feedback for empty
  diagnostic lists and closed panels.
- **Command Palette Search and Outline lifecycle rows explain no-op states**:
  Search Clear Results, Search Close Panel, Outline Clear Symbols, and Outline
  Close Panel now mirror runtime feedback for empty result/symbol lists and
  closed panels.
- **Command Palette Debug lifecycle rows explain no-op states**: Debug Stop,
  Clear Session, and Close Panel now mirror Run and Debug runtime feedback for
  idle sessions, empty session state, and closed panels.
- **Command Palette Source Control and Explorer lifecycle rows explain no-op
  states**: Source Control Clear Commit Message, Source Control Close Panel, and
  Explorer Close Panel now mirror runtime feedback for empty message drafts and
  closed panels.
- **Command Palette Keyboard Shortcuts lifecycle rows explain no-op states**:
  Close Keyboard Shortcuts, Reset Selected, and Reset All now mirror runtime
  feedback for closed overlays, selected shortcuts already using defaults, and
  empty override sets.
- **Command Palette close-surface rows explain no-op states**: Settings Close,
  Diff Close View, Peek Close View, and Markdown Close Preview now mirror
  already-closed runtime feedback before dispatch.
- **Command Palette transient close rows explain no-op states**: AI Copilot,
  Sidebar, Color Theme, Hover, Signature Help, Code Actions, Find & Replace,
  Autocomplete, Command Palette, and Quick Open close rows now mirror their
  no-open runtime feedback before dispatch.
- **Command Palette utility rows explain no-op states**: Clear Notifications,
  Reopen Closed Tab, Close Bottom Dock, Prompt Cancel, Unsaved Changes Cancel,
  Git Branch Cancel, Breadcrumb Close, and Snippet Cancel now mirror their
  empty/no-open runtime feedback before dispatch.
- **Command Palette open/focus rows explain already-open surfaces**: View panel,
  Terminal, Settings, Color Theme, Keyboard Shortcuts, and Markdown Preview rows
  now say when they will focus or leave an already-open surface instead of
  describing only the cold-open path.
- **Command Palette dedicated close rows explain hidden surfaces**: Welcome Close
  and Git Hide Blame now mirror runtime feedback when Welcome is already closed
  or the blame gutter is already hidden.
- **Command Palette tab-management rows explain no-op states**: Move Active Tab
  Left/Right, Sort Open Tabs, Close Saved Tabs, Close Duplicate Tabs, Reload,
  and Revert now mirror tab runtime no-op feedback before dispatch.
- **Smart edit ABIs report read-only previews directly**: bracket/quote
  smart-insert and pair-backspace entry points now reject binary previews
  themselves instead of depending on a fallback edit route for the warning.
- **Run in Browser respects read-only previews**: direct Web Playground runs now
  reject binary/read-only buffers before opening the panel or spawning a build,
  with a focused toast explaining why the command is unavailable.
- **Run in Browser explains untitled buffers**: Web Playground launch now tells
  scratch tabs to save before running in browser instead of reporting a generic
  missing active file.
- **Inline AI respects read-only previews**: explicit ghost-completion requests
  and passive debounce ticks now clear/reject binary previews before snapshotting
  editor contents for an AI request.
- **Run and Debug respect read-only previews**: starting a Run process or a new
  debug session from a binary/read-only tab now stops before adapter spawn and
  reports why the command is unavailable.
- **Run Tests respects read-only previews**: Run Tests and Run Test at Cursor
  now reject binary/read-only buffers before starting `mty test`, keeping the
  Testing panel visible with direct feedback instead of treating a preview as a
  runnable source file.
- **Mighty Agents respects read-only previews**: Agents Run now rejects
  binary/read-only buffers before starting `mty run`, so preview tabs cannot
  launch agent programs accidentally.
- **Code-intelligence edits respect read-only previews**: Symbol Rename and
  Code Action Apply now reject binary/read-only buffers before applying LSP
  edits or fallback edits, with command-specific feedback instead of a silent
  no-op or preview mutation.
- **Code Actions explain untitled buffers before LSP requests and applies**:
  explicit Code Actions on scratch tabs now ask the user to save before
  requesting or applying code actions instead of reporting generic no-action or
  file-needed results.
- **Format Document respects read-only previews**: direct format requests now
  reject binary/read-only buffers before spawning `mty fmt`, preserving preview
  contents with explicit feedback.

### Release packaging
- **Windows-hosted stop pass is locked down**: README and release docs now make
  the final pass explicit: rebuild and launch only the Windows PE package on
  this host, record macOS and Linux as `unbuilt` when native runners are not
  available, and do not reuse stale or copied archives as clean-binary evidence.
- **Final package docs are source-to-artifact strict**: README and release
  verification docs now state that release documentation must be committed
  before package generation, and any later source or doc change requires
  rebuilding the affected native archive before upload.
- **Stale Mighty compilers fail fast**: build and package scripts now preflight
  `mty --version` and require v0.47.0 or newer, so release packaging reports a
  direct toolchain error instead of cascading parser errors from `src/main.mty`.
- **Package manifests record the source commit**: Windows, macOS, and Linux
  package scripts now include the exact committed source hash in
  `PACKAGE-MANIFEST.txt`, tying each clean-binary archive back to the README and
  release docs it bundles.
- **Final handoff fields are explicit**: README and release docs now specify
  the exact final response fields for a Windows-hosted stop pass: source commit,
  Windows archive size and SHA-256, package checks, packaged launch, and
  explicit macOS/Linux `unbuilt` decisions when native runners are unavailable.
- **Windows Bash package docs match the canonical package**: `package-win.sh`
  now bundles `docs/release-evidence.md` and
  `docs/binary-release-status.md`, matching the PowerShell, macOS, and Linux
  package paths.
- **Binary release status is bundled**: every package now includes
  `docs/binary-release-status.md`, a concise per-platform stop/pass summary
  defining clean binaries, valid `publish`/`hold`/`unbuilt` decisions, and the
  Windows-hosted final-pass rule for macOS and Linux.
- **Final stop-pass wording is explicit**: README and release docs now define
  clean binaries as generated, scanned, and launched per-platform artifacts.
  A Windows-hosted pass may publish only the Windows PE archive; macOS and Linux
  remain `unbuilt` until native runners produce and launch their Mach-O/ELF
  archives.
- **Final package status is explicit**: release verification docs now include a
  final handoff table for Windows, macOS, and Linux. Windows can be published
  only after the local PE package and launch checks pass; macOS and Linux stay
  `unbuilt` until native runners produce and launch Mach-O/ELF packages.
- **Final release handoff is documented**: README and release docs now spell
  out the current Windows-local verification scope, the native-runner
  requirement for macOS and Linux, and the only valid platform decisions:
  `publish`, `hold`, or `unbuilt`.
- **Release verification record is packaged**: release archives now include
  `docs/release-verification.md`, a per-platform evidence template for archive
  size, SHA-256, native payload family, sidecar and foreign-payload scans,
  manifest summary, packaged launch result, and publish decision.
- **Release docs now define the final native-host gate**: README, build notes,
  and platform packaging docs now put the release operator flow up front:
  commit docs first, run each package script on its native OS or matching CI
  runner, smoke-test from the assembled package, and mark unavailable macOS or
  Linux hosts as unbuilt instead of deriving them from Windows artifacts.
- **Archive-level clean-binary checks**: Windows, macOS, and Linux package
  scripts now scan the finished ZIP or tarball for compiler/linker sidecars and
  wrong-platform native payloads after the staged package directory has already
  passed its native-binary checks.
- **Release notes have a clean-binary evidence template**: README and
  `docs/platform-packaging.md` now include the exact fields to record for every
  published Windows, macOS, or Linux archive: archive size, SHA-256, native
  payload family, sidecar/foreign-payload scan result, manifest summary, and
  packaged launch result.

### Tabs and panes
- **Jump Back empty-history feedback is navigation feedback**: `No previous
  location` now replaces stale navigation/search toasts instead of stacking as a
  generic notification after a code-navigation gesture.
- **Stale pane retarget feedback replaces layout toasts**: `No pane at that
  position` now coalesces with split-pane and layout feedback instead of
  stacking beside stale pane-focus or split messages.
- **Unsaved-confirm stale actions are visible**: stale Save and Discard routes
  now report `No unsaved changes confirmation open` after the confirmation has
  already closed, matching stale cancel and click behavior.
- **Bottom dock stale chrome is visible**: stale close, preset, and resize
  affordance routes now report when no bottom dock is open, while ordinary
  editor clicks outside the old dock chrome stay quiet.
- **Sidebar stale resize capture is visible**: stale sidebar resize drag and
  finish routes now report when the sidebar has already closed and release the
  retained resize capture.
- **Direct tab switches preserve split panes**: exported tab switch, next-tab,
  and previous-tab entry points now retarget only the focused split pane, keeping
  the other pane bound to its original document.
- **Direct tab load/store routes reject stale indexes**: exported tab load-into,
  store-begin, store-byte, store-commit, and dirty-state routes now report
  `No tab at that position` instead of silently ignoring a stale tab index or
  clearing the load buffer without feedback.
- **Split-pane retargeting rejects stale indexes**: exported pane-to-tab
  retarget calls now validate pane and tab indexes before changing the layout,
  reporting visible feedback instead of leaving a pane bound to a missing tab.
- **File-opening surfaces preserve split panes**: typed Open File, native Open
  File, New File, Explorer rows, Quick Open, Welcome recents, definition targets,
  and New Untitled now bind the new active tab to the focused pane without
  disturbing the other split pane.
- **Panel navigation preserves split panes**: Search results, Problems, Source
  Control, Run/Test output, Debug breakpoints, Agents nodes, breadcrumbs, and
  Peek Definition targets now share the focused-pane file-open path.

### Language intelligence
- **Symbol rename tolerates unusable prepare ranges**: when a language server
  accepts prepare-rename but returns a range that does not resolve to a local
  identifier, the IDE keeps the cursor-derived symbol instead of erasing it and
  reporting a no-target miss.
- **Symbol rename stale input feedback is code-intelligence feedback**:
  `Rename cancelled` and `No rename input open` now replace stale F2
  rename/code-intelligence toasts instead of grouping with active-file rename
  operations.
- **LSP completion notices replace stale autocomplete feedback**: explicit
  autocomplete notices for unavailable language servers or empty LSP completion
  responses now coalesce with other code-intelligence feedback instead of
  stacking beside stale completion toasts.
- **Explicit autocomplete reports unavailable LSPs**: empty explicit completion
  requests now append language-server availability details when a configured
  server is missing, while passive typing completion still falls back quietly to
  buffer words.
- **Autocomplete draw preserves overlay state**: completion dropdown drawing now
  restores the caller's overlay flag instead of always clearing it, preventing
  later overlay text or chrome from being queued on the base layer.
- **Command overlay draw preserves overlay state**: Command Palette, Keyboard
  Shortcuts, Color Theme, and Settings drawing now restore the caller's overlay
  flag instead of always clearing it, preventing later overlay chrome from being
  queued on the base layer.
- **Editor popup draw preserves overlay state**: Quick Open, Hover, Signature
  Help, Rename, Code Actions, and Find & Replace drawing now restore the
  caller's overlay flag instead of always clearing it, preventing later popup
  chrome from being queued on the base layer.
- **Panel and navigation draw preserves overlay state**: Prompt, Problems,
  Branch Switcher, AI Copilot, Breadcrumb menus, Sticky Scroll, and Peek
  Definition drawing now restore the caller's overlay flag instead of always
  clearing it, leaving frame-start as the only source-level overlay reset.
- **Prompt stale submit routes are visible**: stale Go-to-Line and Find submit
  routes now report `No prompt input open` after the prompt has closed instead
  of treating the route as an empty query.
- **Explorer stale routes are visible**: Explorer header, row-hit, row-open,
  toggle, and collapse routes now report when the panel has already closed and
  cannot mutate retained hidden tree state.
- **Snippet stale tab-stop routes are visible**: direct next-stop,
  previous-stop, and placeholder-replace calls now report
  `No snippet session active` after the snippet session has already ended.
- **AI Copilot stale focus routes are visible**: focused send, scroll,
  Backspace, and newline routes now report `AI Copilot is already closed` when
  stale focus reaches them after the panel has closed.
- **Web stale focus routes are visible**: focused Web Playground click and
  scroll routes now report `Web Playground is already closed` when stale focus
  reaches them after the panel has closed.
- **Terminal stale input routes are visible**: focused terminal key, typed
  codepoint, raw-byte, scroll, focus, and mouse routes now report
  `Terminal is not open` when stale focus tries to send input after the PTY has
  closed.
- **Terminal stale header routes are visible**: Terminal header-clear hit
  routing now reports when the integrated Terminal has already closed instead
  of silently returning no action from stale header clicks.
- **Outline stale routes are visible**: Outline row-hit, row-open, and
  header-action routes now report when the sidebar panel has already closed and
  cannot jump through retained hidden symbols.
- **Problems stale routes are visible**: Problems row-hit, row-open,
  close-hit, header-action, and scroll routes now report when the panel has
  already closed and cannot activate retained hidden diagnostics.
- **Breadcrumb menu stale routes are visible**: breadcrumb dropdown move,
  click-row, and accept routes now report when the dropdown has already closed
  instead of silently ignoring stale keyboard or mouse routing.
- **Run output stale routes are visible**: Run panel header, output-row click,
  and scroll routes now report when the panel has already closed, and retained
  hidden output can no longer be activated by stale row calls.
- **Inline preview stale scroll routes are visible**: Diff, Peek Definition,
  and Markdown preview scroll entry points now report when their surface has
  already closed instead of mutating hidden viewport state. Markdown preview
  stale close-click routing also reports the closed preview instead of returning
  silently.
- **Testing stale routes are visible**: Testing toolbar, result-row click, and
  scroll routes now report when the panel has already closed, and retained
  hidden results can no longer be activated by stale row calls.
- **Debug stale routes are visible**: Run and Debug sidebar click, breakpoint
  open/remove, breakpoint-clear, and toolbar action routes now report when the
  panel has already closed and cannot mutate retained hidden debug state.
- **Mighty Agents stale routes are visible**: Agents header hit, topology-row
  hit, scroll, and node-open routes now report when the panel has already
  closed and cannot activate retained hidden topology.
- **Search stale routes are visible**: Search input, focus toggles, run/replace,
  click hit-testing, row-hit, and row-open routes now report when the panel has
  already closed and cannot mutate retained hidden results.
- **Source Control stale routes are visible**: Source Control header,
  message-clear, stage, and row-open routes now report when the panel has
  already closed and cannot activate retained hidden status or commit text.
- **Keyboard shortcut persistence failures are visible**: remap and reset actions
  still update the live shortcut table, but failed `keybindings.toml` writes now
  warn that the override state may reset after restart.
- **Recent history persistence failures are visible**: Recent file and recent
  folder updates now warn when the Open Recent MRU cannot be written, so the
  session state does not masquerade as restart-safe history.
- **Recent persistence warnings replace Open Recent feedback**: those
  file/folder persistence warnings now coalesce with stale recent-row and other
  Open Recent toasts instead of stacking as unrelated warnings.
- **Typed Open Folder failures replace stale Open feedback**: missing folder
  paths and existing non-folder paths now coalesce with the Open workflow, so
  `No such folder: ...` and `Not a folder: ...` replace older open-file,
  open-folder, or Open Recent toasts instead of lingering beside them.
- **Rename collisions stay in rename feedback**: destination name conflicts now
  report `Rename failed: <name>: already exists`, so active-file rename misses
  replace stale rename results instead of being classified as New File feedback.
- **Theme preference failures are visible**: Color Theme applies still take
  effect immediately, but failed config writes now report a warning instead of
  implying the selected theme will survive restart.
- **Settings stale routes are visible**: Settings panel move, click, adjust,
  and toggle entry points now report when the panel has already closed instead
  of silently ignoring stale keyboard or mouse routes.
- **Generic diagnostics reject directory sources**: non-active generic
  diagnostics now preflight stale source paths before reading them. Directory
  targets report `Diagnostics failed: <name>: not a file` instead of surfacing
  platform read errors or clearing diagnostics as if the file were empty.
- **Project Replace All reports write failures**: project-wide Search Replace
  All now counts files that matched but could not be written, leaves their
  contents unchanged, and reports them as skipped failed files instead of
  silently reducing the replacement count.
- **Load paths reject directory targets explicitly**: staged `mui_load` and
  active editor load now report `Load failed: <name>: not a file` when a
  configured or active path is a directory. Staged loads clear the load buffer;
  active loads preserve the live editor buffer and refresh workspace indexes.
- **Definition targets reject directories**: cross-file definition opens now
  require the resolved target to be a real file. Directory targets from stale or
  malformed resolver output report `Definition target is not a file: <name>`
  and leave the tab list unchanged.
- **Click-to-open panels reject directories**: Run output, Test results, Agents
  topology, Source Control rows, and Debug breakpoints now require jump targets
  to be real files. Directory targets report target-specific `is not a file`
  feedback and leave the tab list unchanged.
- **Directory-backed Agents rows refresh away**: clicking an Agents topology row
  whose source path became a directory now refreshes the topology after the
  warning, so repeated clicks report `Agent node no longer listed`.
- **Failed hunk apply refreshes stale diffs**: when a per-hunk stage or unstage
  patch no longer applies, the inline diff now refreshes or closes before
  reporting the git error, so stale hunk buttons do not remain actionable.
- **Directory-backed SCM rows refresh away**: opening a Source Control row whose
  path became a directory now leaves the refreshed status list empty for that
  stale row, so repeated clicks report `Source control row no longer listed`.
- **Branch switcher stale routes are visible**: branch-picker click and keyboard
  routes now report `No branch picker open` when the overlay has already
  closed, matching the accept and cancel paths.
- **Prompt stale keyboard routes are visible**: bottom-prompt text entry and
  backspace routes now report `No prompt input open` when the prompt has already
  closed, matching prompt cancel. That closed-prompt feedback now coalesces
  with navigation prompt results instead of unrelated name-input validation.
- **Command Palette stale keyboard routes are visible**: palette text entry,
  backspace, and selection-move routes now report `No command palette open`
  when the overlay has already closed, matching stale clicks and cancel.
- **Quick Open stale keyboard routes are visible**: Quick Open text entry,
  backspace, and selection-move routes now report `No Quick Open panel open`
  when the overlay has already closed, matching stale clicks and cancel.
- **Keyboard Shortcuts stale keyboard routes are visible**: shortcuts filter
  text entry, backspace, and selection-move routes now report
  `Keyboard Shortcuts is already closed` when the overlay has already closed,
  matching stale clicks, remap, reset, and close.
- **Color Theme stale apply is non-mutating**: theme-picker move and apply
  routes now report `No color theme picker open` when the picker has already
  closed, and stale apply no longer commits the highlighted theme.
- **Rename stale keyboard routes are visible**: rename input text entry and
  backspace routes now report `No rename input open` when the inline rename
  field has already closed, matching cancel and commit feedback.
- **Code Actions stale move routes are visible**: quick-fix selection movement
  now reports `No code action menu open` when the menu has already closed,
  matching stale clicks, apply, and cancel.
- **Autocomplete stale move routes are visible**: suggestion selection movement
  now reports `No autocomplete suggestions open` when the dropdown has already
  closed, matching stale clicks, accept, and cancel.
- **Find & Replace stale action routes are visible**: text entry, backspace,
  focus toggle, replace-next, and replace-all now report
  `No Find & Replace bar open` when the bar has already closed.
- **Run output directory misses demote precisely**: clicking a Run diagnostic
  row whose target became a directory now demotes that row and preserves
  `Run target is not a file: <name>` for repeated clicks.
- **Directory-backed breakpoints are pruned**: opening a debug breakpoint row
  whose source path became a directory now removes the stale breakpoint after
  reporting `Breakpoint target is not a file: <name>`.
- **Explorer directory rows revalidate before toggling**: cached Explorer rows
  that used to be directories now report missing or replaced directory targets
  before expand/collapse or row-open actions mutate the tree.
- **Explorer stale-directory feedback replaces cleanly**: replaced-directory
  Explorer toasts now coalesce with the rest of the navigation feedback instead
  of stacking stale row warnings.
- **Navigation surfaces reject directory targets**: Search results, Problems
  rows, breadcrumb file-menu jumps, and Peek Definition go-to now validate that
  stale targets are real files before opening tabs.
- **Open Recent rejects directory file rows**: Welcome/Open Recent file entries
  now distinguish stale directories from missing files, prune the bad recent,
  and report `Recent file is not a file: <name>`.
- **Welcome stale recents stay actionable for one click**: Welcome now records
  the rendered recent-file and recent-folder rows before pruning persisted MRUs,
  so an already-visible stale row reports the exact missing or non-folder target
  before its hit snapshot is cleared.
- **Quick Open rejects directory file rows**: accepting a file row whose indexed
  target became a directory now refreshes the file index, keeps Quick Open open,
  and reports `Quick Open target is not a file: <name>`.
- **Save targets reject directories explicitly**: Save, Save As, and Save All
  now preflight existing non-file targets and report `Save failed: <name>: not
  a file` instead of leaking platform-specific write errors.
- **Fix-all code actions reject directory pre-save targets**: the `mty fix
  --apply` path now reports `Save failed before code action: <name>: not a
  file` before running the fixer when the active file path became a directory.
- **Workspace-edit code actions skip directory file targets**: workspace edits
  now report `Skipped non-file during workspace edit: <name>` when an edit
  target is an existing directory, instead of classifying it as missing.
- **Packages include a verification manifest**: Windows, macOS, Linux, and the
  Git-Bash Windows package path now write `PACKAGE-MANIFEST.txt` into the
  package root with platform/version metadata, native payload hashes and sizes,
  and the clean-binary checks completed before archiving.
- **Package sidecar checks are stricter**: Windows, macOS, and Linux package
  assembly now also rejects `.dSYM`, `.debug`, and `.map` artifacts, and the
  README/build/platform docs list the same clean-binary contract operators must
  verify before publishing archives.
- **Package scripts reject static archives**: Windows, macOS, and Linux package
  assembly now treats Unix `.a` static libraries as build byproducts, and the
  Windows script revalidates the PE executable after icon stamping so the final
  staged binary is checked before the archive is written.
- **Packages include offline docs**: Windows, macOS, and Linux package scripts
  now bundle README, keybindings, changelog, build notes, license, platform
  packaging notes, samples, and platform-specific `RUN.txt` instructions while
  preserving native binary format checks and build-sidecar rejection.
- **Release docs define the clean-binary contract**: README, build notes, and
  platform packaging docs now spell out the per-OS verification record for
  Windows PE, macOS Mach-O, and Linux ELF packages, including hash/size capture,
  sidecar scans, foreign-payload rejection, and native-host smoke testing.

### Editing & layout
- **Scalar `Saved` feedback is save feedback**: the predefined Mighty-side
  `Saved` toast now replaces stale save warnings instead of stacking as a
  generic notification.
- **Scalar `Copied` feedback is clipboard feedback**: the predefined Mighty-side
  `Copied` toast now replaces stale copy/cut/paste toasts instead of stacking as
  a generic notification.
- **Screenshot capture reports blocked output parents**: offscreen
  `MUI_SCREENSHOT` PNG writing now reports parent-directory creation failures
  with the blocked path instead of ignoring the error and failing later at the
  output file.
- **Settings persistence failures are visible**: Settings panel changes still
  apply live, but failed config writes now warn that the preference may reset
  after restart instead of looking durably saved.
- **Zoom preference failures are visible**: zoom commands still apply
  immediately, but failed persistence now logs the config I/O error and shows a
  warning that the zoom will reset after restart.
- **Keyboard shortcut overrides honor isolated config dirs**: `keybindings.toml`
  now uses the same Mighty IDE config directory resolver as theme, settings,
  zoom, and recents, including `MUI_CONFIG_DIR` for harnesses and automation.
- **Tab-level saves report non-directory parents**: dirty-confirm Save and
  file-backed Save All now preflight parent paths before writing. If a parent
  path is an existing file, they report `Save failed: <parent>: not a file` or
  `Save All failed: <parent>: not a file` while preserving the dirty buffer.
- **Workspace-edit code actions report parent path blockers**: code actions now
  preflight parent components before applying workspace edits. Non-directory
  parents report `Skipped non-file during workspace edit: <parent>`, leave the
  quick-fix menu open, and keep active buffers unchanged when the write cannot
  be committed.
- **Auto-save reports stale non-file targets**: auto-save now shares the same
  preflight as manual saves for directory targets and non-directory parents.
  Stale bindings report `Auto-save failed: <path>: not a file`, keep the buffer
  dirty, and refresh workspace file views instead of failing silently.
- **Save paths report non-directory parents**: staged save plus typed and
  native Save As now preflight parent directories before writing. If a parent
  path is an existing file, saves report `Save failed: <parent>: not a file`
  before touching the dirty buffer or binding the tab.
- **New File rejects directory targets**: typed and native-dialog New File now
  distinguish existing directories from existing files. Directory targets report
  `File create failed: <name>: not a file`, refresh workspace file views, and
  leave the current tab set unchanged.
- **Rename Active File rejects directory endpoints**: active-file rename now
  preflights both the source path and requested destination name before moving.
  Directory endpoints report `Rename failed: <name>: not a file`, refresh
  workspace file views, and preserve the active tab binding.
- **Rename Active File reports stale missing sources**: if the active file is
  externally deleted before a rename commit, the command now prunes the stale
  recent entry, refreshes Explorer and Quick Open, and warns
  `Rename source missing: <name>` instead of naming the requested destination
  with a raw filesystem error.
- **Stale file-operation toasts replace cleanly**: stale copy, reveal, and
  delete target feedback now belongs to the same toast families as the commands
  that produced it, so repeated command attempts update one notification
  instead of stacking unrelated warnings.
- **Navigation stale-target toasts replace cleanly**: non-file targets from
  Definition, Explorer, Quick Open, breadcrumb, Problems, and Search now
  coalesce with their navigation/code-intelligence feedback families just like
  missing-target warnings.
- **Go to Line validation toasts replace cleanly**: invalid prompt and Quick
  Open line-number submissions now coalesce with navigation feedback instead of
  stacking a fresh `Enter a line number` notice on every retry.
- **Project Search feedback replaces cleanly**: Search panel close/clear,
  empty-query, no-result, and replace-preflight notices now join the relevant
  navigation or replace toast family instead of accumulating repeated status
  cards.
- **Mighty Agents panel feedback replaces cleanly**: Agents run-output and
  panel close/no-op notices now coalesce with the rest of the Agents feedback
  family instead of leaving repeated lifecycle toasts behind.
- **Outline panel feedback replaces cleanly**: Outline close/no-op messages now
  coalesce with navigation feedback alongside Outline symbol updates instead of
  stacking stale navigation-surface lifecycle cards.
- **Explorer and Problems panel feedback replaces cleanly**: Explorer close
  messages plus Problems close and diagnostics-clear/no-op notices now coalesce
  with navigation feedback instead of being treated as unrelated layout changes.
- **Run, Test, and Source Control stale-target toasts replace cleanly**:
  non-file target warnings from runner, test, and source-control surfaces now
  coalesce with the command family that produced them instead of lingering as
  independent warnings.
- **Open Recent stale-entry toasts replace cleanly**: missing recent folders
  and recent file rows that became directories now coalesce with the rest of
  the Open/Open Recent feedback instead of stacking as separate warnings.
- **Delete Active File rejects directory targets**: after exact-name
  confirmation and dirty-buffer checks, Delete Active File now preflights the
  active path before removal. Directory targets report
  `Delete failed: <name>: not a file`, refresh workspace file views, and leave
  the active tab bound to the original path.
- **Delete Active File reports stale missing targets**: if an externally
  deleted clean file is confirmed for deletion, the command now refreshes the
  file views, prunes the stale recent entry, warns
  `Delete target missing: <name>`, and keeps the active tab open instead of
  closing it with a success toast.
- **Reload and Revert reject directory targets**: Reload Active File and Revert
  Active File now preflight the active path before reading from disk. Directory
  targets report `Reload failed: <name>: not a file` or
  `Revert failed: <name>: not a file`, refresh workspace file views, and leave
  clean or dirty buffers unchanged.
- **Format Document rejects directory targets**: formatting now preflights the
  active file path before spawning `mty fmt`. If the `.mty` file was replaced
  by a directory, the command reports `Format failed: <name>: not a file`, keeps
  the live buffer unchanged, and leaves the tab clean.
- **Diagnostics refresh failures are visible**: explicit Mighty diagnostics
  refresh now reports `Diagnostics failed: <checker>: <reason>` and clears stale
  diagnostics when `mty check` cannot be spawned, instead of silently showing an
  empty diagnostic set.
- **New Folder names file conflicts**: typed and dialog-routed folder creation
  now report `Folder path is not a folder: <name>` when the target already
  exists as a file, instead of claiming the folder already exists.
- **Native Open File validates picked paths**: dialog-routed opens now reject
  stale or directory picker results with `Open failed: <file>: <reason>` and
  leave the tab list unchanged instead of opening an empty file-backed tab.
- **Generic diagnostic source failures are visible**: non-Mighty diagnostics now
  report `Diagnostics failed: <file>: <reason>` and clear stale diagnostics when
  a non-active file cannot be read, instead of sending empty source to the LSP.
- **Staged load failures are visible**: scalar `mui_load` read failures for a
  configured path now report `Load failed: <file>: <reason>` and clear the load
  buffer, matching editor-load feedback instead of only logging to stderr.
- **Staged save preflight failures are visible**: staged save commits with no
  file path now report `No file path to save`, and dirty open-tab protection
  reports `Save skipped: <file> has unsaved changes` instead of only logging to
  stderr.
- **Code-action apply names closed menus**: applying with no active quick-fix
  menu now reports `No code action menu open`, while active empty selections
  keep `No code action selected`.
- **Bottom-dock close misses are explicit**: closing the shared bottom dock when
  no lower panel is open now reports `No bottom dock is open`, and the no-op is
  covered alongside the dock preset commands.
- **Signature-help no-op feedback is consistent**: closing signature help when
  no popup is open now reports `No signature help popup open`, matching the
  sentence-style wording used by other overlay no-op messages.
- **Peek go-to misses are visible**: accepting a stale or absent Peek Definition
  target now reports `No Peek target selected` instead of returning silently.
- **Diff row misses match Source Control wording**: opening a diff without a
  selected Source Control row now reports `No source control row selected`,
  matching the SCM open and stage paths.
- **Diff hunk misses are specific**: per-hunk stage/unstage actions now
  distinguish an absent hunk click from a stale hunk index, reporting
  `No diff hunk selected` or `Diff hunk no longer listed` instead of the
  generic no-hunk message.
- **Run output stale targets stay named**: repeated clicks on a Run output
  diagnostic row whose target file disappeared continue to report
  `Run target missing: <file>` after the row is demoted, instead of falling
  back to a generic no-file-target message.
- **Run in Browser reports temp output blockers directly**: the Web Playground
  build fallback now stops before spawning `mty build` when its temporary
  output directory cannot be created, showing the blocked path in the panel and
  error toast context.
- **Run in Browser rejects directory targets up front**: stale active tabs whose
  backing file was replaced by a directory now report `target is not a file` in
  the Web panel and toast before any `mty serve` or `mty build` process is
  spawned.
- **Debug start rejects stale targets up front**: F5 now preflights the active
  path before spawning `mty dap`, so missing or directory-backed active files
  fail in the Debug panel with target-specific console output and toast
  feedback instead of depending on adapter spawn fallout.
- **Debug restart rejects stale previous targets up front**: Debug Restart now
  preflights the remembered program path before respawning `mty dap`, so
  deleted or directory-backed previous targets report restart-specific Debug
  panel output and toast feedback before any adapter process is launched.
- **Testing rejects stale active targets up front**: Run Tests and Run Test at
  Cursor now preflight the active file before spawning `mty test`, so deleted
  or directory-backed tabs produce Testing-panel failure rows and target-specific
  toasts instead of launching against stale filesystem state or falling back to
  another workspace target.
- **Reveal commands reject stale active targets up front**: File Tree reveal and
  Show in File Manager now preflight the active path, refresh stale workspace
  indexes, and report missing or directory-backed targets directly instead of
  mislabeling them as outside the Explorer root or launching the OS file manager
  against stale paths.
- **Active-file copy commands reject stale targets up front**: Copy Active File
  Path, Relative Path, Name, and Directory now preflight the active file before
  writing the clipboard, so deleted or directory-backed tabs report
  target-specific warnings instead of copying stale path text.
- **Run active file rejects stale targets up front**: `mty run` now refuses
  missing or directory-backed active paths before spawning a child process,
  keeping the Run panel and toast pointed at the actual stale file state.
- **Explorer row misses are explicit**: file-tree open requests now report
  `No Explorer row selected` for negative row codes and
  `Explorer row no longer listed` for stale non-negative row indices.
- **Run output stale rows are explicit**: out-of-range Run output row jumps now
  report `Run output row no longer listed`, while negative row codes still
  report `No run output row selected`.
- **Test result stale targets are named**: opening a test result row whose suite
  file disappeared now reports `Test target missing: <suite>` instead of the
  generic no-file-target message.
- **Autocomplete accept misses are visible**: accepting an autocomplete
  suggestion with no active dropdown now reports `No autocomplete suggestions
  open`, while preflight remains quiet and repeated code-intelligence misses
  replace stale toasts instead of stacking.
- **Saves handle duplicate tabs coherently**: manual Save, Save All, and
  autosave now refuse conflicting dirty duplicate views of the same file, and
  refresh clean duplicate views after successful writes. The scalar staged-save
  ABI now follows the same dirty-open-tab guard and clean-tab refresh rule.
- **Reload and revert refresh clean duplicate tabs**: reloading or reverting a
  file-backed tab now updates every clean duplicate view of the same file while
  leaving dirty duplicate buffers untouched.
- **Workspace edits refresh clean duplicate tabs**: code actions and LSP
  workspace/applyEdit requests now refresh every clean duplicate view of a
  changed file while preserving undo history on the active edited tab.
- **Project replace requires current search results**: Search Replace All now
  refuses keyboard, mouse, and palette replaces when the query has changed
  since the visible results were produced, and empty or missed project searches
  report visible feedback.
- **Project replace skips dirty open files**: Search Replace All now refuses to
  rewrite files that have any dirty equivalent open tab, and refreshes every
  clean duplicate view after successful replacements.
- **Project replace skips stale disk files**: Search Replace All now fingerprints
  matched files and skips any result whose on-disk bytes changed after the
  search, avoiding writes based on stale result coordinates.
- **Search result opens validate freshness**: opening a project-search match now
  checks that the target file still matches the searched bytes before jumping to
  cached line/column coordinates.
- **Problems jumps report stale targets**: opening a Problems row now reports
  invalid selections, disappeared rows, and missing target files with visible
  feedback instead of silently failing.
- **Quick Open recovers from deleted indexed files**: accepting a file row whose
  target disappeared now reports the missing target, refreshes the file index,
  and keeps Quick Open open with the stale row removed.
- **Quick Open empty accepts stay routed**: pressing Enter on an empty
  files/symbols/line result now reports the missing selection and keeps the
  picker open, with Mighty mirroring the shim's active state after failed
  accepts.
- **Command Palette empty accepts stay editable**: pressing Enter when the
  current palette filter has no command match now reports `No command selected`
  and leaves the palette open so the query can be corrected.
- **Quick Open command misses stay editable**: pressing Enter in `>` command
  mode with no matching command now reports `No command selected` and leaves
  Quick Open open for correction.
- **Quick Open command stale rows are explicit**: accepting a vanished `>`
  command row now reports `Command row no longer listed`, while empty command
  accepts still report `No command selected`.
- **Quick Open file stale rows are explicit**: accepting a vanished file result
  row now reports `Quick Open row no longer listed`, while empty file accepts
  still report `No Quick Open result selected`.
- **Quick Open stale symbols are explicit**: accepting a vanished `@` symbol row
  now reports `Symbol row no longer listed`, while empty symbol accepts still
  report `No symbol selected`.
- **Go to Line invalid input stays editable**: blank or non-numeric Ctrl+G
  submissions now report `Enter a line number` and leave the prompt open instead
  of dismissing the input with no navigation.
- **Find misses stay editable**: blank or no-match Ctrl+F submissions now report
  visible feedback and keep the Find prompt open, while successful matches still
  jump and close the prompt.
- **Explorer rejects stale file rows**: opening a file row whose target was
  deleted now reports the missing file, refreshes Explorer and Quick Open, and
  avoids creating an empty phantom tab for the missing path.
- **Explorer directory rows toggle on open**: direct open-row activation now
  expands or collapses folders instead of treating directory rows as inert,
  while still returning no tab index for folder activations.
- **Open Recent clears stale hit rows**: missing Welcome/Open Recent file or
  folder rows now prune their cached hit-test snapshot immediately, so repeated
  clicks cannot keep targeting a removed resource before the next draw.
- **Open Recent names stale row state**: Welcome and workspace Open Recent now
  distinguish an invalid selection from a row that disappeared after recents
  were pruned, keeping file and folder misses actionable.
- **Workspace New File names dialog scope**: cancelling or missing the
  workspace-scoped New File picker now reports `New workspace file ...` so it
  is distinguishable from the general File: New File command.
- **Run, Diff, Blame, and Agents name scratch misses**: path-scoped workflow
  commands now identify untitled buffers in refusal feedback, so users can tell
  when they need to save `(scratch)` before running Agents or Run, or when Diff
  and Blame have no file-backed target.
- **Testing preflights name scratch misses**: Run Tests and Run Test at Cursor
  now identify `(scratch)` when they cannot find a saved Mighty file or testable
  workspace target.
- **Agents node misses name the row**: clicking a topology row with no file
  target now reports the node name instead of a generic row failure.
- **Agents stale rows are explicit**: clicking a vanished topology row now
  reports `Agent node no longer listed` instead of the no-selection message.
- **Debug Restart explains missing history**: restarting without a previous
  debug target now tells the user to start debug first and names the active
  context.
- **Testing result target misses name the row**: clicking a result row with no
  resolvable source location now includes the test name in the feedback.
- **Run output target misses name the row**: clicking non-clickable Run output
  now includes a compact copy of the output line in the feedback.
- **Copy file metadata names scratch buffers**: copy-path, copy-relative-path,
  copy-name, and copy-directory now identify `(scratch)` when the active tab has
  no file path.
- **Active file commands name scratch and dirty targets**: rename, reveal,
  show-in-file-manager, and delete now identify scratch buffers when no file is
  active, and delete names the dirty file that must be saved or discarded.
- **Debug preflights name scratch buffers**: starting debug and toggling
  breakpoints on an untitled buffer now report `Save (scratch) before ...`.
- **Breadcrumb file menus prune stale paths**: accepting a breadcrumb file row
  whose source disappeared now removes that dead path from the menu backing
  list while reporting the missing target.
- **Breadcrumb misses avoid navigation side effects**: failed breadcrumb accepts
  still close the dropdown and report feedback, but no longer reset undo or
  refresh diagnostics/outline as if a file or symbol jump succeeded.
- **Breakpoint rows prune missing targets**: opening a debug breakpoint row
  whose source file was deleted now removes the stale breakpoint entry and keeps
  the inventory from repeatedly targeting a missing file.
- **Breakpoint stale rows are explicit**: opening or removing a breakpoint row
  that has fallen out of the visible breakpoint inventory now reports
  `Breakpoint row no longer listed` instead of the generic no-selection message.
- **Source Control stale rows are explicit**: opening or staging a row that has
  disappeared from the SCM status list now reports
  `Source control row no longer listed`, while negative row codes still report
  `No source control row selected`.
- **Diff stale SCM rows are explicit**: opening a diff from a vanished
  Source Control row now reports `Source control row no longer listed` instead
  of the older generic diff-row message.
- **Testing result jumps distinguish stale rows**: out-of-range Testing result
  jumps now report `Test result row no longer listed`, while negative row codes
  still report `No test result row selected`.
- **Fix-all code actions protect dirty duplicate tabs**: Fix all (mty) now
  refuses to save or run the external fixer when another equivalent tab for the
  active file has unsaved edits, matching inline workspace-edit safety, and
  refreshes clean duplicate views after both the pre-fix save and the fixer
  reload.
- **Format reloads refresh clean duplicate tabs**: the undo-preserving load path
  used after Format Document now updates every clean duplicate view from the
  formatted disk bytes while leaving dirty duplicate buffers untouched.
- **Format preflight protects dirty open tabs**: the formatter ABI now refuses
  to run when the target file has unsaved open edits, including dirty duplicate
  views, so direct callers cannot format disk underneath live buffers.
- **Tab switches clear stale active diagnostics**: syncing the active file now
  invalidates cached diagnostics so underlines and code-action context cannot
  bleed from the previous tab before the next diagnostics refresh.
- **Tab switches clear stale Find matches**: active-file sync now invalidates
  cached in-buffer Find match coordinates so highlights and next/previous match
  navigation cannot point into the previous tab.
- **Tab switches re-scope Auto Save debounce**: active-file sync now resets the
  autosave timer and content signature, then starts a fresh debounce for the new
  dirty file-backed tab, so a due timer from one tab cannot immediately save a
  different tab after switching.
- **Tab switches close stale language popups**: active-file sync now also clears
  hover, definition, signature-help, completion, rename, snippet tab-stops,
  inline AI ghost text, code-action, and quick-fix lightbulb state, closes
  inline peek cards, and dismisses breadcrumb menus, so transient language UI
  cannot follow the wrong tab.
- **Tab switches clear stale Outline symbols**: active-file sync now invalidates
  the cached document-symbol list so Outline and Sticky Scroll cannot show
  symbol rows from the previous file before the next refresh.
- **Workspace edits protect dirty duplicate tabs**: code actions and rename
  workspace edits now skip a target path when any equivalent non-active tab is
  dirty, including duplicate views of the active file, avoiding disk writes
  underneath unsaved buffers.
- **Recent files de-duplicate equivalent paths**: Quick Open now removes
  canonical-equivalent prior file entries when recording a recent file, matching
  the stale-row removal path and avoiding duplicate MRU rows for the same file.
- **Workspace recents compare folder identity**: recent workspace de-duplication
  and stale-folder removal now canonicalize folder paths and use Windows
  slash/case fallback, keeping Open Recent from listing equivalent folders twice.
- **Recent files track rename and delete**: file rename now removes the old path
  before recording the new one, and file delete removes the deleted path
  immediately, so Quick Open and Welcome recents stay actionable.
- **Rename rebinds duplicate open tabs**: renaming an active file now updates
  every equivalent open tab for that file, including dirty duplicate views, so
  no tab remains pointed at the old on-disk path after the move.
- **Delete accounts for duplicate open tabs**: deleting an active file now
  refuses the operation if any equivalent open tab for that file is dirty, and
  closes all clean duplicate tabs without adding deleted files to reopen history.
- **Open tabs compare equivalent file paths**: tab lookup now canonicalizes
  file-backed paths and applies Windows slash/case fallback, preventing
  duplicate tabs and Save As target misses when the same file is referenced by
  an alternate spelling such as `.\file`.
- **Save As rejects platform-trap filenames**: typed and native Save As targets
  now reject reserved Windows device names and trailing-dot/space basenames
  before binding the tab or writing to disk.
- **Native creation catches trailing-space basenames**: New File and New Folder
  dialog paths now reject basenames ending in spaces as well as dots before
  Windows can normalize them to a different on-disk name.
- **Native creation dialogs reject platform-trap basenames**: dialog-selected
  New File and New Folder paths now apply the same Windows device-name and
  trailing-dot safety checks as typed creation before touching the filesystem,
  while still allowing broader native-dialog filenames.
- **New project and folder names reject Windows traps**: shared name validation
  now blocks reserved Windows device names such as `CON`, `NUL`, `COM1`, and
  `LPT1`, plus trailing-dot names, before project or folder creation reaches the
  filesystem.
- **Debugger payloads require matching DAP roles**: debugger event and response
  parsers now verify the expected DAP `event` or `command` before reading a
  `body`, so wrong-command responses and wrong-event payloads cannot update the
  stack, threads, variables, console, exit state, or stopped location.
- **Language features require response envelopes**: signature help, rename
  workspace edits, and code actions now ignore request-shaped JSON-RPC objects
  with top-level `method` even when those objects carry incidental `result`
  payloads, keeping server requests out of editor actions.
- **Mighty navigation requires response envelopes**: built-in Mighty hover and
  go-to-definition now ignore request-shaped JSON-RPC objects with top-level
  `method` even when they carry matching `id` and incidental `result` fields,
  so server requests cannot replace the real navigation answer.
- **Diagnostics require publish notifications for URI filters**: URI-specific
  LSP diagnostics parsing now ignores request-shaped objects with matching
  `params.uri` and `params.diagnostics`, so only real
  `textDocument/publishDiagnostics` notifications update Problems.
- **Prepare rename requires response envelopes**: rename preparation now ignores
  request-shaped JSON-RPC objects with top-level `method` while reading
  `prepareRename` rejection or accepted ranges, so incidental request payloads
  cannot block or redirect symbol rename.
- **Generic LSP waits require response envelopes**: the cross-language LSP
  client now ignores objects with a top-level `method` while waiting for request
  id `2`, even if a server request carries incidental `result` data.
- **Completion labels require response-owned results**: LSP completion scraping
  now ignores completion-looking `result` payloads on server requests or
  progress notifications in a stream, so only real response results populate the
  autocomplete dropdown.
- **Signature help supports offset parameter labels**: LSP signature-help
  parameters that use the standard `[start,end]` label form now derive the
  highlighted parameter text from the signature label, so servers that emit
  offsets still get active-argument highlighting.
- **Debugger stopped events resolve real threads**: stopped events without a
  body-owned `threadId` now request the adapter's `threads` response and use a
  real returned thread before fetching the stack, instead of assuming thread 1.
- **Blame dates preserve valid cached metadata**: git blame parsing now stores
  author timestamps and timezones only after successful parsing, so malformed
  repeat headers cannot erase a commit's cached date, and date-less blame rows
  no longer render an empty separator in the gutter.
- **Diagnostics require params-owned arrays**: LSP diagnostic objects now read
  diagnostics only from `params.diagnostics` while preserving direct raw-array
  parsing for isolated payloads, so wrapper-level arrays cannot become Problems.
- **Debugger stack frames require owned lines**: DAP stack-trace rows now need
  both a frame-owned `id` and `line`, so metadata-only line fields cannot jump
  the editor to the top of a source file.
- **Agents live rows require owned identity**: live `mty inspect --json` agent
  rows now require a top-level numeric `agent_id` plus `agent_type`, so nested
  metadata cannot fabricate an agent `0` in the topology panel.
- **Code-action fix-all commands require Mighty ownership**: language-server
  commands such as `rust-analyzer.fixAll` now remain server commands instead of
  being mistaken for the shim's synthetic `mty fix --apply` action, and
  command-argument `workspaceEdit` wrappers are extracted without broad nested
  metadata scans.
- **Outline errors require owned codes**: document-symbol parsing now treats
  method-not-found as a fallback signal only when the top-level `error` object
  owns `code: -32601`, so nested metadata cannot suppress a valid outline
  result.
- **Debugger events require body-owned details**: DAP stopped, output, and
  exited events now read details only from the event `body`, so top-level
  envelope fields cannot masquerade as debugger state.
- **Workspace edits require top-level edit owners**: rename and code-action edit
  parsing now ignores nested metadata-only `changes` or `documentChanges`
  fields when the owning WorkspaceEdit lacks those top-level edit containers.
- **Execute-command streams require real apply-edit requests**: generic LSP
  command execution now isolates its own response by response-owned `id: 2` and
  appends the raw stream only when a top-level `workspace/applyEdit` request was
  actually received, so metadata text cannot trigger edit parsing.
- **Completion waits for response-owned IDs**: semantic completion now waits for
  a complete top-level response object with `id: 2` before scraping labels, so
  progress metadata or server requests cannot end completion collection early.
- **Mighty language requests wait for response-owned IDs**: signature help,
  rename preparation, rename edits, code actions, and document-symbol requests
  now share the envelope-owned response wait used by navigation, so progress
  metadata or server requests cannot truncate those language features.
- **Mighty navigation waits for response-owned IDs**: the built-in Mighty hover
  and go-to-definition client now stops and isolates responses only from
  complete top-level JSON-RPC response objects with `id: 2`, so progress
  metadata or server requests cannot replace the actual hover/definition result.
- **LSP requests wait for response-owned IDs**: generic hover, completion,
  definition, signature, rename, and code-action requests now stop reading only
  after a complete response object with top-level `id: 2` and a `result` or
  `error`, so progress metadata or server requests cannot truncate responses.
- **Diagnostics wait for matching publish notifications**: generic LSP
  diagnostics collection now stops only after a complete top-level
  `textDocument/publishDiagnostics` notification whose `params.uri` matches the
  opened file, so other files' diagnostic text or related locations cannot end
  the read early.
- **Prepare rename reads result-owned ranges**: rename preparation now reads the
  server's accepted `result.range.start` / `result.start` from the owning result
  object, so metadata coordinates cannot choose the wrong symbol.
- **Definitions read range-owned start positions**: go-to-definition parsing now
  reads `Location.range.start` and `LocationLink` target starts from their
  owning range objects, preventing nested metadata from moving jump targets.
- **Diagnostics read params and range owner fields**: generic LSP diagnostics
  now read `params.diagnostics` and each diagnostic range's top-level
  `start` / `end` positions, so nested metadata cannot replace problem rows or
  underline coordinates.
- **Apply-edit responses route by top-level request fields**: generic LSP
  `workspace/applyEdit` acknowledgement now finds complete request objects and
  reads only top-level `method` / `id`, so nested metadata cannot hijack command
  response IDs.
- **Completion labels read result item fields**: semantic completion scraping now
  reads labels from the JSON-RPC `result` array or `result.items`, and only from
  each CompletionItem's top-level `label`, so metadata labels cannot pollute the
  autocomplete dropdown.
- **Workspace document changes read entry-scoped edits**: LSP
  `documentChanges` parsing now reads `textDocument.uri`, `edits`, `newText`,
  and range coordinates from each owning object, so nested metadata cannot
  redirect or reshape rename/code-action edits.
- **Debugger events read envelope and body scopes separately**: DAP envelope
  routing now uses only top-level `type`, `event`, `command`, `request_seq`, and
  `success` fields, while stopped/output/exited event details come from the
  event `body` object.
- **Debugger stack and variable responses read body-scoped fields**: DAP
  `stackTrace` and `variables` parsing now reads arrays from the response
  `body` object and row values from each row's top-level fields, so envelope or
  row metadata cannot replace debugger panel contents.
- **Agents inspect reads top-level snapshot fields**: live agent snapshot parsing
  now requires the root `agents` array, reads `worker_count` from the root
  object, and reads each agent row's surfaced fields from that row object, so
  nested metadata cannot replace the Agents panel contents.
- **Outline reads result-scoped document symbols**: LSP document-symbol parsing
  now anchors on the top-level JSON-RPC `result` array and reads symbol `name`,
  `kind`, ranges, and `children` from each symbol object, so metadata cannot
  hijack the Outline panel.
- **Code actions read top-level action fields**: LSP code-action parsing now
  anchors on the top-level JSON-RPC `result` array and reads each action's
  `title`, `edit`, `command`, `kind`, and `arguments` from that action object,
  so nested metadata cannot hijack menu rows or command execution.
- **Workspace edits read result-scoped edits**: rename and code-action edit
  parsing now prefers the JSON-RPC `result` payload before reading `changes` or
  `documentChanges`, preventing envelope metadata from hiding the real
  WorkspaceEdit.
- **Signature help reads result-scoped signatures**: LSP signature-help parsing
  now isolates the JSON-RPC `result`, reads `activeSignature` /
  `activeParameter` from that payload, and parses signature labels, parameters,
  and docs from top-level signature fields.
- **Hover reads result-scoped contents**: LSP hover parsing now isolates the
  JSON-RPC `result` before reading `contents`, and reads markup `value` fields
  at the hover object's top level so envelope metadata cannot replace hover
  text.
- **Go-to-definition reads result-scoped targets**: LSP definition parsing now
  isolates the JSON-RPC `result` before reading `Location` or `LocationLink`
  fields, preventing envelope or metadata URI/range fields from hijacking the
  navigation target.
- **Diagnostics parse complete LSP objects**: generic LSP diagnostics now scan
  complete JSON messages instead of splitting on the raw `publishDiagnostics`
  text, so diagnostic messages can mention the method name without corrupting
  latest-notification selection.
- **Diagnostics ignore related-information ranges**: generic LSP diagnostics now
  read the diagnostic's own top-level `range`, `severity`, `code`, and
  `message` fields, so related-information locations cannot move the underline
  or replace the primary message.
- **Diagnostics match equivalent file URIs**: generic LSP diagnostics now accept
  case-varied schemes, `localhost` authorities, and percent-hex casing changes
  when matching a publish notification to the active document.
- **LSP requests emit UNC authorities correctly**: network paths such as
  `\\server\share\file` now become `file://server/share/file` instead of a
  four-slash local path URI.
- **LSP requests percent-encode file paths**: completion, hover, definition,
  rename, code-action, diagnostics, and generic LSP requests now share one
  file-URI builder that encodes spaces, `#`, `?`, and non-ASCII path bytes.
- **Workspace edits accept case-varied file URI keys**: rename and code-action
  edits using `FILE:///...` keys in the LSP `changes` map now parse correctly,
  while nested URI-looking text inside edits is ignored as payload.
- **Rename prepare ignores nested error text**: valid `prepareRename` responses
  that include placeholder or metadata strings such as `"error"` no longer get
  mistaken for JSON-RPC failures.
- **Code actions hide unavailable server fixes**: LSP actions marked with
  `disabled` are now omitted from the quick-fix menu instead of appearing as
  selectable rows that the server says cannot run, while nested command
  arguments remain intact.
- **Go-to-definition accepts case-varied file URIs**: LSP definition targets now
  accept `FILE://...` schemes and `file://LOCALHOST/...` authorities, matching
  URI casing rules instead of dropping otherwise valid navigation targets.
- **Go-to-definition handles UNC file URIs**: LSP definition targets using
  `file://localhost/...` or `file://server/share/...` now resolve to local drive
  paths or UNC paths correctly instead of becoming relative paths.
- **Hover popups read cleaner markdown**: inline hover text now removes common
  markdown noise such as link URLs, backticks, emphasis markers, and escaped
  punctuation before wrapping, making LSP docs easier to scan in the compact
  popup.
- **User snippets load from copied VS Code files**: Mighty now reads the legacy
  config `snippets` file, `snippets.json`, `user-snippets.json`, and sorted
  `*.code-snippets` files from the config directory, so existing snippet files
  can be dropped in without renaming.
- **Snippet variable transforms cover common filename casing**: imported
  snippets can now use VS Code-style `${VAR/(.*)/${1:/pascalcase}/}` transforms
  for `upcase`, `downcase`, `capitalize`, `camelcase`, and `pascalcase`, so
  filename-derived class and component snippets expand cleanly.
- **Snippet comment variables resolve by language**: imported snippets can now
  use `$LINE_COMMENT`, `$BLOCK_COMMENT_START`, and `$BLOCK_COMMENT_END`; values
  come from the active language's syntax configuration, with braced defaults
  still honored for languages without block comments.
- **User snippets accept VS Code JSON**: the optional user snippet file can now
  be a VS Code-style JSON object with string or array prefixes and string or
  array bodies, including comments and trailing commas from existing JSONC
  snippet files. Imported VS Code `scope` fields now keep language-specific
  snippets out of unrelated files, while the existing tab-separated format still
  works.
- **Current-date snippet variables resolve**: imported snippets can now use
  `$CURRENT_YEAR`, `$CURRENT_MONTH`, `$CURRENT_DATE`, and related day/time
  variables from the local expansion time.
- **Current-line snippet variables resolve**: imported snippets can now use
  `$TM_CURRENT_LINE`, `$TM_CURRENT_WORD`, `$TM_LINE_INDEX`, and
  `$TM_LINE_NUMBER` from the expansion site.
- **Workspace snippet variables resolve**: imported snippets can now use
  `$WORKSPACE_NAME`, `$WORKSPACE_FOLDER`, and `$RELATIVE_FILEPATH` from the
  active workspace and file path, including default fallbacks.
- **Selected-text snippet variables resolve**: imported snippets can now use
  `$TM_SELECTED_TEXT` and `${TM_SELECTED_TEXT:default}` to wrap or reuse the
  active editor selection during direct Tab or completion-driven expansion.
- **Braced snippet variables honor defaults**: imported snippets can now use
  `${TM_FILENAME}`, `${TM_FILENAME_BASE:default}`, and unknown-variable default
  fallbacks without leaving braced marker text in the editor.
- **Snippet file variables resolve from the active tab**: imported snippets can
  now use `$TM_FILENAME`, `$TM_FILENAME_BASE`, `$TM_DIRECTORY`, and
  `$TM_FILEPATH`, including inside editable placeholder defaults.
- **Nested snippet defaults flatten cleanly**: imported snippets that nest
  defaults such as `${1:${2:name}}` now expand to the default text instead of
  leaving malformed marker fragments in the editor.
- **Snippet choices expand as editable placeholders**: VS Code-style choice
  stops such as `${1|red,green,blue|}` now insert and select the first choice
  instead of leaving the marker literal, with escaped separators handled.
- **Terminal restores saved xterm titles**: window-operation title stack
  controls (`CSI 22 t` / `CSI 23 t`) now save and restore OSC titles, keeping
  full-screen terminal apps from leaving stale panel titles behind.
- **Terminal tracks OSC 7 working directories**: shells that report their current
  directory with OSC 7 now update terminal metadata without leaking URI bytes
  into the grid, and the terminal header can fall back to that path when no
  explicit OSC title is available.
- **Terminal answers pixel window-size probes**: xterm `14t` and `16t`
  window-operation queries now report grid pixel size and character-cell pixel
  size from Mighty's live terminal metrics, alongside the existing character
  dimension replies.
- **Terminal reports application keypad mode**: DEC keypad application/numeric
  mode now tracks `ESC =`, `ESC >`, and `CSI ?66 h/l`, and mode status queries
  report the current state instead of treating it as unknown.
- **Terminal handles horizontal scroll CSI**: space-intermediate scroll-left and
  scroll-right sequences now shift the visible scroll region horizontally
  instead of being mistaken for insert-character or cursor-up commands.
- **Panel commands stop stale typing state**: command-dispatched panel open,
  refresh, clear, close, and action routes now clear transient editor typing
  state while transferring ownership to sidebar, dock, Copilot, or Web surfaces.
- **Rail switches stop stale typing state**: activity-rail and topbar
  navigation now clear transient editor typing state while transferring focus
  to Run, Debug, Testing, Copilot, Agents, or another sidebar panel.
- **Focused side-panel Escape releases stale focus**: Search and Source
  Control Escape-to-Explorer routes now clear stale surface/search focus before
  returning keyboard input to the editor/sidebar.
- **Palette and Quick Open local exits release stale focus**: Escape, Enter,
  and mouse accept/dismiss routes now clear stale surface/search focus before
  returning control to the editor or shared command dispatcher.
- **Dirty-confirm and keyboard-shortcut exits release stale focus**: Unsaved
  Changes save/discard/cancel and Keyboard Shortcuts close/cancel local routes
  now clear stale surface/search focus before returning keyboard input.
- **Branch and breadcrumb local exits release stale focus**: branch picker
  accept/cancel and breadcrumb accept/dismiss local routes now clear stale
  surface/search focus before returning keyboard input to the editor.
- **Peek local keyboard exits release stale focus**: Peek Escape, Enter
  navigation, and other-key dismissal now clear stale surface/search focus
  before returning keyboard input to the editor.
- **Rename and code-action local exits release stale focus**: inline Rename
  Escape/Enter and Code Actions apply/cancel local exits now clear stale
  surface/search focus before returning keyboard input to the editor.
- **Autocomplete local dismissals release stale focus**: suggestion Escape,
  unhandled-key, and mouse-miss closes now clear stale surface/search focus
  before returning keyboard input to the editor.
- **Prompt local cancels release stale focus**: prompt Escape, close-button,
  and outside-click cancels now clear stale surface/search focus before
  returning keyboard input to the editor.
- **Diff and replace local exits release stale focus**: inline Diff Escape plus
  Find & Replace Escape/close-click now clear stale surface/search focus before
  returning input to the editor.
- **Settings and theme exits release stale focus**: local Escape, close, outside
  click, and apply exits in Settings and Color Theme now clear stale
  panel/terminal/AI/Agents/search focus before returning to editor input.
- **Terminal focus routes release stale focus**: Ctrl+` unfocus plus terminal
  scroll, header clear, body click, open, clear, and close routes now clear
  stale panel/AI/Agents/search focus and transient typing state.
- **Focused output clicks release stale focus**: Run/Web/Testing focused header,
  row-jump, and outside-click routes now explicitly clear competing focus before
  keeping panel ownership or returning keyboard input to the editor.
- **Bottom-band Escape exits release stale focus**: Escape from focused Run,
  Web, and Testing output now clears stale panel/AI/Agents/search focus before
  returning keyboard input to the editor.
- **AI focus exits release stale focus**: Ctrl+Shift+A and Escape from the
  focused AI Copilot input now clear stale Run/Web/Testing/Terminal/Agents/search
  focus before returning keyboard input to the editor.
- **Early chrome clicks release stale focus**: first-click titlebar, bottom-dock,
  Web header, and resize/preset routes now clear stale AI/Agents/search focus
  before the normal mouse router runs.
- **Overlay shortcuts release stale focus**: Ctrl+Shift+P and Ctrl+P now clear
  stale Run/Web/Testing/Terminal/AI/Agents/search focus when opening the
  Command Palette or Quick Open directly, matching the command-dispatch path.
- **Direct chrome clicks release stale focus**: branch, breadcrumb, Markdown
  preview, Problems close, Explorer header, Settings utility, topbar Palette/
  Quick Open, terminal scroll, and editor body mouse routes now clear stale
  surface/search focus before handing keyboard input to the new owner.
- **Direct tab no-ops release stale focus**: Ctrl+W, Ctrl+Shift+PageUp/
  PageDown, tab close clicks, and same-tab clicks now clear stale panel/search
  focus even when the close, move, or switch is refused.
- **Tab command no-ops release stale focus**: tab close/reopen/move/sort/
  duplicate-close/reload/revert commands now clear stale Run/Web/Testing/
  Terminal/AI/Agents focus even when the tab operation is refused or has
  nothing to do.
- **Active-file utility commands release stale focus**: reveal and copy-path/name/
  directory commands now clear stale Run/Web/Testing/Terminal/AI/Agents focus
  just like other file commands, so subsequent typing returns to the editor.
- **Terminal paste failures are visible**: terminal paste now reports
  `Terminal is not open` before reading the clipboard when no integrated
  terminal is available, instead of silently returning from the command.
- **Source Control commit checks use the live index**: `Git: Commit Staged` now
  refreshes repository status before deciding whether staged changes or a commit
  message are missing, so files staged outside the UI do not produce stale
  prerequisite feedback.
- **Source Control commit feedback is specific**: `Git: Commit Staged` now tells
  users whether they need staged changes or a commit message instead of collapsing
  both states into `Nothing to commit`.
- **Git range commands claim visible ownership**: Switch Branch now releases stale
  panel/search focus when the branch picker opens, Push/Pull/Fetch reveal Source
  Control after dispatch, and Toggle Blame returns keyboard ownership to the
  editor instead of leaving stale panel focus behind.
- **Autocomplete, Jump Back, and Zoom release stale focus**: editor-returning
  palette commands for suggestions, navigation history, and zoom now clear stale
  Run/Web/Testing/Terminal/AI/Agents/search focus before returning keyboard input
  to the editor.
- **Transient close commands release stale focus**: Find & Replace, Hover,
  Signature Help, Rename, Code Actions, prompt, autocomplete, dirty-confirm,
  Git branch picker, breadcrumb menu, Command Palette, Quick Open, and Peek
  close/cancel commands now clear stale Run/Web/Testing/Terminal/AI/Agents/search
  focus when dismissing their overlay.
- **Editor operation commands release stale focus**: Delete/Join Line,
  selection, multi-caret, comment, copy/cut/paste, word delete,
  indent/outdent, cursor movement, duplicate, and move-line commands now clear
  stale Run/Web/Testing/Terminal/AI/Agents/search focus when returning input to
  the editor.
- **File and edit commands release stale focus**: New file/folder prompts,
  rename/delete prompts, Open File, Save/Save As/Save All, Format Document,
  Undo/Redo, and Explorer close now clear stale
  Run/Web/Testing/Terminal/AI/Agents/search focus when returning to editor or
  prompt ownership.
- **Welcome, Zen, Diff, and Blame commands release stale focus**: opening or
  closing Welcome, toggling Zen mode, closing inline diff, and hiding blame now
  clear stale Run/Web/Testing/Terminal/AI/Agents/search focus.
- **Close and transient commands release stale focus**: Run, Testing, Web,
  Agents, Search, Source Control, Outline, Debug, Problems, AI, Sidebar, and
  Terminal close paths plus inline AI, ghost-completion, and snippet-cancel
  commands now clear stale surface focus before returning input.
- **Layout and window commands clear hidden focus**: Dock presets/close,
  sidebar width presets/cycle, and window minimize/maximize now release stale
  AI/Agents/search focus in addition to bottom-dock focus.
- **Fold commands return to editor focus**: Fold Toggle, Fold All, and Unfold
  All now clear stale Run/Web/Testing/Terminal/AI/Agents/search focus after
  changing editor-visible code structure.
- **Workspace entrypoints release stale focus**: File: Open Folder's prompt
  fallback and File: Open Recent's picker or empty-state feedback now clear
  stale Run/Web/Testing/Terminal/AI/Agents/search focus when workspace UI owns
  the next interaction.
- **Pane and Markdown commands return to editor focus**: Split Editor, Focus
  Next Pane, Close Pane, Markdown Preview, and Markdown Close Preview now clear
  stale Run/Web/Testing/Terminal/AI/Agents/search focus when they move
  interaction back into editor-owned panes.
- **Palette tab commands return focus to the editor**: Next/Previous Tab,
  close saved/duplicate tabs, reopen, duplicate, move, sort, reload, and revert
  tab commands now clear stale Run/Web/Testing/Terminal/AI/Agents/search focus
  after switching editor content.
- **Preference overlays release stale panel focus**: Preferences: Settings,
  Preferences: Color Theme, their close commands, and the Ctrl+, Settings
  shortcut now clear stale Run/Web/Testing/Terminal/AI/Agents/search focus when
  the Preferences overlay owns the next interaction.
- **Modal command focus is consistent**: Keyboard Shortcuts open/reset/close
  commands and the New Project prompt fallback now clear stale
  Run/Web/Testing/Terminal/AI/Agents/search focus when they move interaction
  into modal or prompt-owned UI.
- **Editor shortcuts release stale panel focus**: Ctrl+I, Ctrl+F, Ctrl+G,
  Ctrl+H, Ctrl+K, Ctrl+., Ctrl+Shift+Space, F2, F12, Alt+F12, Ctrl+O,
  Ctrl+Shift+S prompt fallbacks, and direct tab-switch shortcuts now clear
  stale Run/Web/Testing/Terminal/AI/Agents/search focus just like the matching
  command-palette actions.
- **Editor assistance commands release stale panel focus**: Go to Definition,
  Hover, Signature Help, Rename Symbol, Code Actions, and Peek Definition now
  clear stale Run/Web/Testing/Terminal/AI/Agents/search focus when they move
  interaction back to editor-owned UI.
- **Find-style overlays release stale focus**: Find, Go to Line, and Find &
  Replace now clear stale Run/Web/Testing/Terminal/AI/Agents/search focus when
  opening their keyboard-focused prompt bars.
- **Quick Open and terminal commands clear stale focus**: palette Quick Open now
  releases stale surface focus when opening its overlay, and the legacy
  Terminal open/focus command now matches the Ctrl+` shortcut and View:
  Terminal ownership behavior.
- **Sidebar action commands now claim their panels**: Explorer refresh/collapse,
  Search run/clear/replace/toggle, and Outline refresh/clear now release stale
  Run/Web/Testing/Terminal/AI/Agents/search focus after revealing their sidebar
  panel.
- **Sidebar view and Source Control actions release stale focus**: Explorer,
  Search, Source Control, and Outline view commands now drop stale AI focus, and
  Source Control refresh/stage/unstage/commit/clear-message commands release
  stale Run/Web/Testing/Terminal/AI/Agents/search focus after revealing SCM.
- **Problems commands now own their visible panel**: opening, refreshing, or
  clearing Problems from the palette or status-bar/header controls now releases
  stale Run/Web/Testing/Terminal/AI/Agents/search focus before returning input.
- **Debug keyboard shortcuts now own Debug focus**: F5, Shift+F5, F10, F11,
  and Shift+F11 now reveal Run and Debug and release stale
  Run/Web/Testing/Terminal/AI/Agents/search focus before running their debug
  action.
- **Debug action commands now own Debug focus**: start/continue, stop, pause,
  step, restart, toggle breakpoint, and clear-breakpoints now release stale
  Run/Web/Testing/Terminal/AI/Agents/search focus after revealing Run and Debug.
- **Search replace-focus source docs match the palette**: the command constant
  now documents that it opens Search before moving focus, and stale sidebar
  toggle wording in palette-ranking comments was updated.
- **Sidebar toggle is labeled as a view command**: Ctrl+B now appears as
  `View: Toggle Sidebar`, and its description says it shows or hides the left
  sidebar rather than only the file explorer.
- **Search replace-focus command advertises its reveal path**:
  `Search: Toggle Replace Field` now says it opens Search before moving focus
  between query and replace, matching the existing dispatch path.
- **Git blame toggle describes both states**: `Git: Toggle Blame` now advertises
  showing or hiding the blame gutter, matching the actual toggle ABI and keeping
  the dedicated hide command as a one-way close action.
- **Close Sidebar now releases sidebar-local focus**: `View: Close Sidebar`
  clears stale Agents focus and transient search navigation after closing the
  drawer, matching the close behavior of individual sidebar panels.
- **Sidebar toggle now claims focus when it opens Explorer**: Ctrl+B and the
  palette sidebar toggle now release stale bottom-dock, AI, Agents, and search
  navigation focus when the toggle opens the sidebar, while preserving current
  surface focus when it closes the sidebar.
- **Sidebar width commands advertise their reveal behavior**: compact, default,
  wide, and cycle-width commands now describe opening the sidebar before sizing
  it, matching their hidden-sidebar dispatch path that reveals Explorer.
- **Bottom-dock preset commands advertise their reveal behavior**: compact,
  default, and expanded dock commands now describe opening the shared bottom
  dock at the requested size, matching the existing Run-panel reveal path when
  no lower dock is active.
- **Terminal shortcut command no longer promises a close toggle**: the legacy
  Ctrl+` command now appears as `Terminal: Open or Focus`, matching its actual
  behavior of opening the integrated terminal or focusing the existing one while
  leaving `Terminal: Close` as the explicit close path.
- **Markdown Preview commands describe one-way actions**: palette descriptions
  now say `Markdown: Open Preview` opens the live preview pane and
  `Markdown: Close Preview` closes it, matching the dedicated command paths
  instead of implying a toggle.
- **Windows packaging avoids linker PDB failures**: `package-win.ps1` now applies
  release-only linker flags that disable PDB emission for the packaging build,
  avoiding `LNK1318` failures while restoring any caller-provided `RUSTFLAGS`.
- **New File uses the dialog label everywhere**: the command registry and
  command-label tests now show `File: New File...` for the Ctrl+N native picker
  flow, while `File: New Untitled File` remains the instant scratch-tab action.
- **Open Recent empty state is explicit**: when no valid recent files or folders
  remain after pruning, `File: Open Recent` now reports that directly instead of
  opening an unrelated Open Folder prompt.
- **Open Recent now preflights stale entries**: `File: Open Recent` prunes
  missing recent files and folders before deciding whether the focused recent
  picker is available, avoiding empty picker detours.
- **Chrome clicks now follow the same focus contract**: rail, topbar, AI, tab,
  and terminal mouse paths release stale competing surface focus just like
  palette commands and keyboard shortcuts.
- **Lifecycle palette commands now own their surfaces**: Run, Testing, Web, and
  Terminal action commands release stale competing focus after revealing or
  mutating their output surfaces.
- **Keyboard shortcuts now release stale surface focus**: Run, Tests, AI,
  Terminal, Search, and Source Control shortcuts clear competing dock/sidebar
  focus so the next keystroke lands on the surface the shortcut opened.
- **Agents commands now own focus cleanly**: `Mighty Agents`, refresh, and
  clear-run-output palette commands release stale Run/Web/Testing/Terminal/AI
  focus when they show the Agents panel.
- **Run Output and Testing view commands claim focus**: `View: Run Output` now
  clears stale Terminal/AI/navigation focus, and `View: Testing` now explicitly
  claims Testing focus while releasing competing output owners.
- **Run and Debug view now owns focus cleanly**: `View: Run and Debug` clears
  stale Run/Web/Testing/Terminal/AI/Agents focus when it switches to the Debug
  sidebar, preventing hidden surfaces from receiving the next input.
- **Sidebar view commands now release dock focus**: `View: Explorer`,
  `View: Search`, `View: Source Control`, and `View: Outline` clear stale
  Run/Web/Testing/Terminal focus when they switch the sidebar panel.
- **AI Copilot commands now clear stale dock focus**: `View: AI Copilot` and
  `AI: Clear Chat` release stale Run/Web/Testing/Terminal focus when they show
  Copilot, so the next input belongs to the visible right-dock surface.
- **Bottom-dock view commands now claim focus cleanly**: `View: Terminal`,
  `View: Web Playground`, and `View: Problems` clear stale Run/Web/Testing/
  Terminal/Agents navigation focus when they open their surfaces.
- **Run, Testing, and Web lifecycle commands reveal their panels**: palette
  Stop and Clear commands now open the affected Run output, Testing, or Web
  Playground surface before mutating it, making stop/clear feedback visible.
- **Terminal clear reveals the buffer first**: `Terminal: Clear Buffer` now
  opens the integrated Terminal before clearing, so palette cleanup happens on
  the same visible surface as the local header action.
- **AI clear-chat reveals Copilot before clearing**: `AI: Clear Chat` now opens
  the AI Copilot panel before resetting the transcript and composer, so the
  command result is visible immediately.
- **Problems clearing reveals the diagnostic list**: `Problems: Clear
  Diagnostics` now opens the Problems panel before clearing diagnostics, keeping
  the command result, toast feedback, and next focus target visible.
- **Debug breakpoint clearing reveals the inventory first**: `Debug: Clear
  Breakpoints` now switches to Run and Debug before clearing stored
  breakpoints, so the visible breakpoint list, toast feedback, and follow-up
  focus match the command result.
- **Source Control palette commits reveal their state**: `Git: Commit Staged`
  and `Source Control: Clear Commit Message` now switch to the SCM panel before
  acting, so the visible staged set, draft message, refresh result, and follow-up
  focus all line up with the command-palette path.
- **Source Control now follows the folder users actually open**: switching
  workspaces resets git-root discovery before refresh, so the SCM panel no
  longer stages or commits against a stale repository. The Windows harness opens
  a temporary git workspace through the native folder picker, clicks row
  stage/unstage, runs Stage All / Unstage All, commits with the visible header
  button, and asserts both traces and the isolated repo state.
- **Multi-cursor gestures are now real-user verified**: the Windows harness opens
  fresh editors, drives `Ctrl+D`, `Ctrl+Alt+Up`, Alt+Click, and multi-caret
  typing, then asserts shim traces and captures the visible editor state.
- **Full real-mouse UX harness is back to green**: the Windows harness now clicks
  Explorer header buttons, bottom-dock preset controls, Source Control refresh,
  and the rail Settings gear using the same live geometry the app hit-tests.
  It also waits for slower git refresh traces instead of misclassifying a
  successful visible click as a failure.
- **Inline AI Ask now fails visibly when unavailable**: `Ctrl+I` routes through
  the same AI send preflight as the visible Copilot send button, so blank
  prompts, missing API keys, active streams, and startup failures produce the
  same toast and trace outcomes instead of silently returning `0`.
- **Borderless window resize targets are easier to hit**: side and bottom edge
  grab bands are wider, corner zones are larger, and tests cover the expanded
  direction mapping so manual resizing feels less like finding a one-pixel edge.
  The bottom rail account/settings icons now explicitly win over southwest
  resize hit-testing, preventing enlarged corner zones from stealing utility
  clicks.
- **Packaged Windows app now launches as a real GUI app**: `mighty.toml` passes
  the Windows subsystem and `mainCRTStartup` entry flags, so `mighty-ide.exe`
  no longer opens a console window that steals focus from the IDE.
- **Real desktop UX harnesses capture the visible IDE bounds**:
  `drive-input.ps1` and `win-ui-harness.ps1` now bring the app forward and crop
  to DWM extended frame bounds, making mouse-driven screenshots match what
  humans actually see instead of including invisible shadow margins or whatever
  window was behind the IDE.
- **Keyboard Shortcuts overlay no longer double-renders in screenshot/headless
  flows**: the shortcuts auto-open path now uses the normal Mighty draw call
  only, and the draw wrapper honors visible screenshot bounds so compact
  captures keep the footer inside the viewport.
- **Overlay clicks no longer get stolen by resize handles**: bottom-dock,
  Web-panel, and sidebar resize/header click paths now share the same
  modal-overlay guard as top-bar commands, so palette, quick-open, settings,
  theme, and prompt surfaces keep owning their mouse clicks.
- **Compact Welcome no longer clips the Recent Files empty state**: the
  single-column Welcome layout now requires enough room for a section header and
  row before drawing it, preventing bottom-edge text clipping at 860x560.
- **Save All cancellation now reads like a deliberate dialog outcome**:
  cancelling the Save As picker for an untitled tab now reports `Save All
  cancelled; 1 untitled file still unsaved`, while unavailable native dialogs
  report a distinct system limitation.
- **Web Playground idle controls now explain themselves**: pressing Stop when no
  server is running now reports `No web server running`, and opening the browser
  before a served URL exists reports `Web URL not ready` instead of failing
  silently.
- **AI Copilot send now explains unavailable actions**: blank prompts, missing
  API keys, in-flight responses, and startup failures now surface immediate
  toasts instead of making Enter or the visible send button look broken. The
  Windows harness types into the Copilot input and clicks Send with no key set,
  then asserts the missing-key trace and captures the visible toast.
- **Open Folder cancellations now acknowledge the dialog result**: cancelling the
  native folder picker now reports `Open folder cancelled`, and unavailable
  folder pickers report `Open folder dialog unavailable`, matching Open File's
  visible dialog outcomes.
- **Save dialog cancellations now speak consistently**: cancelling Save on an
  untitled tab or cancelling Save As now reports `Save cancelled; tab is still
  open`, instead of leaving the user to infer that the dialog cancellation was
  handled.
- **Problems panel close now confirms the mouse action**: clicking the Problems
  dock header X now reports `Problems panel closed`, so the visible close
  control no longer collapses a dock without feedback.
- **Markdown preview lifecycle is now visible**: opening the preview reports
  `Markdown preview opened`, and closing it from the pane header reports
  `Markdown preview closed` instead of silently collapsing the split.
- **Visible dock close now acknowledges the click**: clicking the bottom-dock X
  now reports `Bottom dock closed`, matching the palette close command instead
  of silently changing layout.
- **Welcome recent rows now show cleaner location context**: recent folders now
  display the folder name with its parent location, instead of repeating the full
  selected folder path in dim text.
- **Explorer header buttons now have clearer hit targets**: the new-file,
  new-folder, and collapse-all buttons are spaced as distinct controls, and the
  click geometry now shares the same centers as the rendered buttons.
- **Manual resize drags now finish with visible feedback**: releasing a sidebar
  or bottom-dock drag now reports the final size, and layout toasts replace
  older layout messages instead of leaving stale resize/preset text on screen.
- **Tab move/sort no-ops now explain themselves**: Move Active Tab Left/Right
  and Sort Open Tabs by Name now report already-first, already-last, and
  already-sorted states instead of looking like dead palette or shortcut
  actions.
- **Invalid tab commands now fail visibly**: tab switch/close ABIs now reject
  out-of-range targets with `No tab at that position` instead of silently
  returning the active tab, making bad mouse hit-tests or command routing errors
  obvious to the user.
- **Dialog cancellations now acknowledge the command**: Open File, New File, New
  Folder, and New Project native picker cancels now show a short info toast, and
  unavailable dialog fallbacks show a warning, so a closed dialog does not read
  as a dead toolbar/menu action.
- **Terminal open failures now surface in the UI**: failed shell/PTY startup now
  pushes `Terminal failed to open` instead of only logging stderr, and first
  successful opens acknowledge the panel with `Terminal opened`.
- **Terminal close now reports its state**: the terminal close ABI now confirms
  `Terminal closed` and distinguishes the no-op `Terminal is already closed`
  path, so direct terminal lifecycle controls do not fail silently.
- **Sidebar toggle now confirms the layout change**: Ctrl+B and palette/sidebar
  toggle routes now toast `Sidebar opened` or `Sidebar closed`, so a major
  drawer visibility change has the same clear feedback as explicit close and
  size commands.
- **Dock preset buttons now acknowledge clicks**: the visible compact/default/
  expanded buttons in the lower dock header now show the same feedback as the
  command-palette dock actions, so a successful resize does not feel like a
  silent or broken click.
- **Toast feedback clears stale dialog outcomes**: save/open/create workflows now
  replace older toasts from the same operation family, so cancelled dialogs,
  failed saves, Save As prompts, and later successful saves do not stack as
  contradictory stale labels.
- **Bottom dock resizing no longer jumps on grab**: the Terminal/Run/Tests/Web
  dock now preserves the pointer's offset inside the forgiving resize band, so
  grabbing the top edge off-center does not immediately change the dock height
  before the user actually drags.
- **Dirty-close dialog buttons fit compact windows**: the unsaved-tab
  confirmation now derives action-button widths from the actual card width and
  centers button labels by measurement, so Cancel/Save/Discard stay inside the
  modal on narrow windows.
- **Dirty-close dialog detail text now fits the card**: long filenames in the
  unsaved-tab confirmation are middle-truncated before drawing, preserving the
  important consequence text without letting the detail line run across the
  modal.
- **Dirty-close Save now explains cancelled dialogs**: when a user tries to
  close an unsaved untitled tab, clicks Save, and cancels the native picker, the
  confirmation stays open and a toast now explains that the tab is still open
  instead of failing silently.
- **Sidebar resizing no longer jumps on grab**: direct sidebar drag now preserves
  the pointer's offset inside the forgiving hit band, so grabbing the divider
  off-center does not immediately nudge the Explorer width before the user
  actually drags.
- **Explorer header actions now show dialog affordances**: New File and New
  Folder keep their compact icon buttons, but now carry a tiny `...` marker so
  they read as picker/prompt actions instead of looking identical to immediate
  commands like Collapse All.
- **Welcome quick actions now advertise dialogs**: first-run actions use the
  same `...` convention as the command palette for New File, New Project, Open
  File, and Open Folder, so launch-screen buttons do not hide the picker flow.
- **Disabled Settings rows no longer act clickable**: unavailable preferences
  such as Inline AI without an API key now select for explanation only; their
  visible switch no longer reports a toggle/cycle click back to Mighty.
- **Command Palette file actions describe current state**: Save, Save As,
  Save All, Reload, Revert, Rename, Delete, and copy-path rows now update their
  helper text for untitled tabs, read-only previews, and the current dirty-tab
  count, so context-limited commands no longer look generically broken.
- **File commands read like standard dialog actions**: command palette labels now
  use `...` for New File, Open File, Save As, Open Folder, and New Project
  because those actions ask the user for a file or folder, while `New Untitled
  File` remains the instant scratch-tab action.
- **Windows icon coverage matches real shell sizes**: the generated ICO now
  includes 16/20/24/32/40/48/64/128/256px DIB entries, with a heavier
  taskbar-scale Mighty mark and a regression test that pins the size ladder.
- **Mighty Agents is covered by real-mouse UX tests**: the Windows harness now
  opens the language-native Agents topology from the rail, captures the panel,
  and clicks the visible Inspect and Run header affordances while tracing their
  dispatch.
- **Web Playground is usable from inside the panel**: the browser runtime panel
  now shows a compact empty state before any session has run, exposes a visible
  Run button in its header, lets that button win over the shared resize band,
  traces palette opens and button clicks, and is covered by the strict Windows
  real-mouse harness with screenshot capture.
- **Integrated Terminal is covered by real command output**: the Windows harness
  now opens the terminal from the command palette, runs `set` in the real PTY,
  captures the dock, and verifies the inherited probe environment value reached
  the visible terminal grid.
- **Project Search replace is safer and tested by mouse**: the Search panel's
  replace-all button now only becomes active when the current search has real
  matches, uses the replace icon instead of a generic checkmark, and the Windows
  harness proves the visible replace field and button rewrite a file on disk.
- **Branch switcher respects compact windows**: the Git branch overlay now uses
  shared, height-aware row budgeting for drawing and mouse hit-testing, and its
  card width clamps inside narrow windows instead of producing invalid geometry.
- **Branch switcher is covered by real-mouse UX tests**: the Windows harness now
  clicks the visible status-bar branch segment, captures the branch picker, and
  closes it through the visible close button while tracing overlay open/close.
- **Delete confirmation names the target up front**: the active-file delete
  prompt now shows the exact file basename in the prompt label before the user
  types, instead of only revealing the required confirmation name after a failed
  attempt.
- **Sidebar resizing looks like a divider**: the file-tree sidebar now renders a
  slim edge divider with a compact centered grip instead of a tall floating
  thumb, while keeping the same generous mouse hit target for dragging.
- **Open Recent paths keep orientation**: recent-file and recent-folder rows now
  shorten long paths from the middle, preserving both the drive/root context and
  the actionable tail instead of showing ambiguous left-truncated fragments.
- **Status problem counters read like labels**: the bottom status bar now renders
  diagnostics as `N err` and `N warn` when space allows, while keeping the
  compact icon-number fallback for narrow windows. This removes the stray-looking
  bare numbers visible beside branch state in full-width screenshots.
- **Markdown Preview only opens on Markdown files**: command-palette and
  keyboard preview requests now warn on non-Markdown buffers instead of opening
  a misleading rendered pane for `.mty` or plain text. The real-mouse harness
  now opens a `.md` fixture through the native file dialog before testing the
  rendered preview and visible close affordance.
- **New Project uses a native folder picker first**: Welcome and command-palette
  New Project now ask for the intended project folder through the native picker,
  reject non-empty folders without overwriting content, and keep the bottom
  prompt only as an unavailable-dialog fallback.
- **Welcome action tests match the shipped compact labels**: stale assertions for
  `New File at Location...` and `New Mighty Project...` now track the visible
  `New File` and `New Project` labels, keeping the Welcome regression suite
  aligned with the compact UX.
- **Rename and ghost-text captures show code context**: screenshot-only hooks for
  rename and inline ghost suggestions now seed Mighty source, dismiss Welcome,
  and lock the probe buffer; the ghost fixture uses an incomplete edit so
  suggested lines render in open space instead of overlapping real code.
- **Minimap gallery captures always show the minimap**: the screenshot-only
  minimap autoopen hook now forces the minimap preference on while seeding its
  tall demo buffer, and compact editor panes use a slimmer strip anchored to the
  visible pane edge so audits cannot silently capture a no-minimap editor.
- **Overlay geometry honors screenshot-visible bounds**: shared visible-surface
  sizing now caps to the screenshot/window override dimensions, so compact
  popups such as Code Actions do not measure against a wider offscreen surface
  and spill past the right edge in audits.
- **Mighty Agents rows preserve compact signatures**: protocol messages and
  `implements` edges now use measured fitting with compact labels like
  `Submit(Str) -> U8` and `impl Summarize` before ellipsizing, making the
  agent topology readable in narrow sidebars.
- **Compact Source Control headers use a complete title**: narrow SCM sidebars
  now show `SCM` instead of truncating `SOURCE CONTROL` into an unfinished
  `SOURCE CO...` label beside the git action icons.
- **Diff gallery captures show the actual diff**: the inline-diff screenshot hook
  now suppresses automatic empty-buffer Welcome before drawing, so visual audits
  no longer pass while hiding the diff body behind the landing screen.
- **Split editor panes get divider breathing room**: right-hand panes now start
  with a small inner inset after the divider, so gutter numbers and source text
  no longer appear glued to the split border in compact windows.
- **Signature Help stays inside compact editor columns**: signature popups now
  cap their width to the actual visible work area before fitting text, avoiding
  right-edge overflow in narrow windows.
- **Peek Definition headers fit compact cards**: inline peek now uses shorter
  compact action hints and measured-ellipsis file labels, so `file:line` and
  `Go/Esc` do not crowd each other in narrow editor columns.
- **Topbar Run works on the first click**: the early titlebar-action guard now
  handles the visible play button instead of swallowing it before the normal
  mouse routing can start the Run panel.
- **Bottom dock resize chrome reads as a dock control**: the shared lower-dock
  grip now uses a subtler full-width header treatment and calmer compact action
  buttons, avoiding the old floating scrollbar-like handle over editor content.
- **Compact Debug headers use a complete title**: narrow Run-and-Debug sidebars
  now show `DEBUG` instead of truncating `RUN AND DEBUG` into `RUN AND DEB...`,
  while preserving the `paused` / `running` state pill.
- **Keyboard Shortcuts no longer mixes action hints with chords**: compact
  selected rows now show `Remap` instead of a standalone `Enter` label beside
  the shortcut pills, so the visible chord reads as `Ctrl` + `N`, not
  `Enter` + `Ctrl` + `N`.
- **Welcome copy fits compact windows**: the first-run surface now says
  `New File`, `New Project`, `Open File`, and `Open Folder` without faux
  truncation marks, and uses a shorter compact subtitle while keeping the same
  native file/project/folder dialog behavior.
- **Focused dock panels no longer steal tab clicks**: when Run, Web Playground,
  or Testing has keyboard focus, tab switch and tab close clicks now route on
  the first click and clear transient panel focus after the tab action succeeds.
- **Compact Problems rows prioritize diagnostic text**: narrow Problems docks now
  shorten location metadata to `line:col` and omit the redundant code column, so
  the actual error/warning message has room before the right edge.
- **Compact toast stacks are less intrusive**: narrow windows and bottom-dock
  layouts now render at most two toast cards at once, while retaining the queue
  and click-dismiss behavior.
- **Run status chip no longer touches the Run label**: compact Run output
  headers now reserve a measured gap between `RUN` and status chips like
  `exit 1`, avoiding the broken-looking `RUNexit` merge.
- **Testing Stop control is no longer blank at compact width**: the compact
  Testing toolbar now keeps the `Stop` action label visible beside the square
  icon, so the disabled/runnable state reads like a control instead of an empty
  button.
- **Quick Open placeholder yields before the mode pill**: compact overlays now
  reserve a larger measured text budget before the `FILES` / `CMDS` adornment,
  and fall back to a clean short placeholder instead of clipping halfway through
  the mode-hint sentence.
- **Run output rows fit compact docks**: Run panel output now ellipsizes by
  measured code-font width instead of a fixed character estimate, keeping long
  diagnostics inside compact bottom-dock bounds.
- **Typed Open File rejects blank paths**: pressing Enter on an empty Open File
  fallback prompt now reports `No file path entered` instead of silently closing
  the prompt as though the active tab had been opened again.
- **Typed Save As rejects blank paths**: pressing Enter on an empty Save As
  fallback prompt now reports `No save path entered` while keeping the dirty
  untitled tab untouched.
- **Typed Open Folder blank paths stay in the Open feedback lane**: empty
  Open Folder fallback submissions still report `Enter a folder path`, and now
  replace stale open-folder toasts instead of stacking separately.
- **Open Recent empty feedback replaces stale Open toasts**: `No recent files
  or folders` is now treated as part of the Open workflow, so it replaces older
  open-file/open-folder outcomes instead of appearing as unrelated feedback.
- **Terminal clipboard-copy feedback stays in the Copy lane**: OSC52 terminal
  copies that report `Copied from terminal` now replace stale copy/paste toasts
  instead of stacking as unrelated feedback.
- **Save All now has native-picker proof for untitled tabs**: deterministic
  SaveFileDialog sequences let the Windows harness verify Save As and Save All
  in one run, including the exact Save All path used for a dirty untitled tab.
- **Borderless resize is now discoverable**: the status bar reserves the
  bottom-right corner for a visible diagonal resize grip, shifts notifications
  away from that target, and the strict Windows harness captures the pre-drag
  frame before proving the real mouse resize path.
- **Inline AI settings state is honest**: when no `ANTHROPIC_API_KEY` or
  `CLAUDE_API_KEY` is configured, the Inline AI row now reads as unavailable
  instead of showing an enabled purple toggle for a feature that cannot run.
- **AI Copilot no-key controls are honest**: without an API key, the chat input
  now shows setup copy, uses a muted border, and disables the send affordance
  instead of inviting Enter-to-send on a no-op path.
- **Explorer New File now proves the native picker path**: the Windows harness
  verifies that the visible Explorer New File button dispatches the native
  workspace file picker for the selected path, not just that a file eventually
  appears on disk.
- **Open Recent has a real close affordance**: the focused Open Recent picker
  now shows the same top-right close button pattern as the other modals, and
  mouse clicks dismiss the forced picker instead of requiring Esc or a row pick.
  The Windows harness now explicitly opens, closes, reopens, and selects from
  the picker with real mouse events.
- **Quick Open placeholder no longer collides with the caret**: empty Quick Open
  now insets placeholder copy and uses overlay-specific muted text, matching the
  command palette and shortcuts overlay spacing.
- **Keyboard shortcut alternatives render honestly**: slash-separated bindings
  like `Alt+Up / Alt+Down` now draw as distinct shortcut groups while slash-key
  chords like `Ctrl+/` remain a real key pill.
- **Color Theme selected state is explicit**: the current theme row now uses a
  checked accent capsule instead of a plus icon, so the picker reads as
  "selected" rather than "add".
- **Project Search replace no longer looks disabled**: the replace field uses
  readable secondary text and the replace-all check button gains an active
  accent state whenever a real query is present.
- **Autocomplete rows read like real IDE metadata**: semantic completions now
  show a clear `function` kind and suppress placeholder inline signatures,
  leaving full parameter detail in the selected-row footer.
- **Autocomplete misses name the query site**: explicit empty completion
  requests now report the active file or scratch buffer plus 1-based cursor
  position instead of a generic no-candidates toast.
- **Code-action misses name the query site**: empty Ctrl+. requests now report
  the active file or scratch buffer plus 1-based line/column, keeping
  no-quick-fix feedback actionable in multi-tab sessions.
- **Hover misses name the query site**: empty Ctrl+K requests now report the
  active file plus 1-based line/column instead of a generic no-hover toast.
- **Signature-help misses name the query site**: empty Ctrl+Shift+Space
  requests now report the active file plus 1-based line/column instead of
  failing silently.
- **Rename misses name the query site**: F2 on non-renamable locations now
  reports the active file or scratch buffer plus 1-based line/column.
- **Formatter preflight names scratch buffers**: formatting an unsaved buffer now
  reports `Save (scratch) before formatting` instead of a generic save-first
  warning.
- **Formatter dirty-tab refusals name the target**: dirty active or duplicate
  tabs now report the blocked file before refusing to run `mty fmt`.
- **Code-action no-file refusals name scratch buffers**: Fix All and server
  command actions on untitled buffers now report `(scratch)` instead of a
  generic file-required warning.
- **Code-intelligence save-first refusals name scratch buffers**: hover,
  definition, peek definition, and signature help now report `(scratch)` when
  an untitled buffer needs saving before LSP lookup.
- **Testing result metadata is readable**: suite names and run duration in the
  Testing drawer now use secondary text instead of near-invisible faint chrome.
- **Overlay helper text is readable**: command palette, Keyboard Shortcuts, and
  Settings now use overlay-specific secondary colors so descriptions,
  placeholders, and footer hints no longer disappear into dark modal surfaces.
- **Split editor** (`Ctrl+\`): side-by-side editor panes; focus a pane with
  `Ctrl+1` / `Ctrl+2`, click a pane to focus it.
- **Save All** (`Ctrl+Alt+S`): writes dirty file-backed tabs in place and asks
  where dirty untitled buffers should be saved through the native picker.
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
- **Open Recent is a focused chooser**: the command now opens a compact recent
  workspace/file picker instead of jumping back to the branded Welcome landing,
  and the strict mouse harness clicks a recent row to prove it dispatches.
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
- **Testing Run works from scratch tabs**: clicking **Run Tests** now falls back
  to the open workspace's `mighty.toml` or first `.mty` file when the active tab
  has no file path, and the strict mouse harness clicks the visible button to
  prove it dispatches.
- **Project Search is mouse-verified end to end**: the Search panel now emits
  trace markers for run, replace-all, and result-open actions; its header
  refresh icon runs the current query; and the strict mouse harness types a real
  query, clicks both visible run affordances, and clicks a result row to prove
  it opens the matching file.
- **Debug toolbar Play is actionable from idle**: the Run and Debug panel's
  visible Play button now shares the F5 start/continue behavior instead of
  silently doing nothing when idle, disabled toolbar actions surface feedback,
  and the strict mouse harness clicks Play and Stop to prove dispatch.
- **Source Control refresh is local and mouse-verified**: the SCM header's
  visible refresh icon now re-scans local Git status instead of invoking Fetch,
  emits a refresh trace, and the strict mouse harness clicks it to prove the
  empty-state "Refresh to scan Git status" path works.
- **Top-right action chrome reads like controls**: the Run and More buttons now
  sit on the same tab-bar surface with compact button affordances instead of a
  wide dark strip that looked like an inactive/dead tab block beside the window
  controls.
- **Taskbar icon small sizes are sharper**: the generated Windows ICO now uses
  simplified 16px/32px variants with a larger Mighty mark and a single crisp
  outline, plus a generated size-strip preview for future visual checks.
- **In-app Mighty marks match the icon system**: the rail and Welcome brand
  tiles now use the same simplified single-outline treatment with a larger mark,
  avoiding the fuzzy nested-frame look in small UI surfaces.
- **Overflow tabs are real-mouse verified**: the Windows harness now creates a
  crowded tab strip and uses an actual mouse-wheel event over the tab row,
  requiring a tab-scroll trace so hidden tabs are proven reachable by mouse.
- **Borderless window resizing is mouse-verified**: the strict Windows harness
  now drags the bottom-right corner, requires the OS window size to change, and
  verifies the resize trace before restoring the test window.
- **Compact sidebar leaves more room for work**: narrow windows now reduce the
  sidebar to 160px, giving terminal, debug, palette, and welcome views more
  usable width instead of crowding dock/header content.
- **Settings footer is cleaner in compact windows**: the modal no longer crams
  shortcut helper text into the footer, avoiding tiny overlapping copy at narrow
  widths.
- **Keyboard Shortcuts footer is responsive**: the modal keeps capture/status
  feedback visible, but hides the static helper legend when the card is too
  narrow to fit it cleanly.
- **Explorer header title no longer overlaps actions**: compact sidebar headers
  now measure and ellipsize the workspace title before the New File/New Folder
  buttons.
- **Markdown Preview is readable in compact windows**: opening the live preview
  now temporarily hides the sidebar when a split preview would leave cramped
  columns, then restores the sidebar when the preview closes.
- **Markdown Preview typography adapts to split panes**: compact preview columns
  now use smaller margins and responsive heading sizes so rendered prose remains
  readable instead of wrapping into awkward one- or two-word lines.
- **Binary files open safely**: image, icon, font, and other non-text files now
  open as read-only previews instead of corrupt editable text, and Save, Save
  As, Save All, and autosave refuse to overwrite them from the text editor.
- **Toast notifications avoid compact sidebars**: toasts now shrink and align
  inside the work area when the activity rail/sidebar are visible, so feedback
  no longer looks like stale text over the Explorer.
- **Toast notifications stay out of modal dialogs**: active Settings, Keyboard
  Shortcuts, Theme Picker, and dirty-work confirmation overlays suppress toast
  drawing and toast hit targets so transient feedback cannot cover modal content.
- **Source Control opens without freezing the UI**: the SCM model now checks for
  a `.git` marker before shelling out to `git rev-parse`, and view-switch
  commands no longer run `git status` directly on the click path. The panel opens
  immediately and the refresh action performs the scan intentionally.
- **Top-bar command access is stable after dialogs and tab growth**: mouse-opened
  Palette and Quick Open surfaces now ignore the opener click that follows the
  More/command-center press, preventing the surface from closing itself and
  sending typed commands into the editor.
- **Outline opens without blocking the rail**: switching to the Outline panel no
  longer refreshes symbols directly on the click path; document-open/save paths
  continue to refresh outline data when content changes.
- **Sidebar resizing is direct**: the left drawer now has a visible draggable
  divider with an east-west resize cursor, custom width persistence, and
  real-mouse harness coverage instead of requiring palette-only width presets.
- **Real-mouse verification is more resilient**: the Windows harness restores
  the target window before strict click/drag steps and traces Problems panel
  opens, preventing a focus/minimize slip from cascading into false failures.
- **UTF-8 BOMs no longer render as stray editor glyphs**: files opened from
  Windows tools strip the leading BOM at the text-model boundary, which keeps the
  editor and live Markdown preview from showing an odd first character.
- **The empty titlebar gap is now a command center**: when tabs leave enough
  room, the top bar shows a Quick Open pill instead of a blank drag-only block,
  and clicking it opens Quick Open without breaking normal window dragging.
- **Toasts stay out of command overlays**: Command Palette, Quick Open,
  breadcrumb dropdowns, and branch picker now suppress toast drawing and toast
  clicks while active, avoiding stale-looking text over overlay footers.
- **Language popups fit compact work areas**: signature help and code-action
  menus now clamp to the editor safe area and ellipsize measured text instead of
  clipping against the right edge.
- **Testing toolbar labels stay readable**: compact Testing panels now show
  **Run** instead of the ambiguous clipped **Re** label after previous runs.
- **Testing summaries keep real words in compact sidebars**: narrow Testing
  panels wrap the pass/fail/total summary instead of falling back to cryptic
  `p/f/t` shorthand.
- **Source Control header no longer overlaps actions**: compact SCM panels now
  measure and ellipsize the title before the commit/pull/push/fetch icons.
- **Source Control section metadata is responsive**: the branch label now yields
  to the `CHANGES` count in cramped sidebars instead of drawing over it.
- **Debug sidebar fits compact widths**: the Run and Debug header title now
  measures against the state pill, and its toolbar buttons shrink inside the
  sidebar instead of spilling into the editor. Call-stack rows also measure the
  source location first so function names no longer draw underneath it.
- **Bottom prompts own their overlay layer**: find/replace and prompt fallback
  bands now draw text on the overlay layer so editor or Welcome text cannot
  bleed over the active input.
- **Quick Open input text respects the mode pill**: compact Quick Open now
  ellipsizes the search placeholder/query before the `FILES`/`CMDS`/`SYMS` pill.
- **Bottom dock resize chrome no longer paints over panel headers**: the shared
  resize strip sits above the dock and the header reserves enough height for
  compact/default/expanded/close actions.
- **Compact Run/Web headers degrade cleanly**: Run status chips use short ASCII
  labels that fit the chip, and Web uses compact header geometry so action controls do
  not collide with the `WEB` title lane.
- **Toast cards stay readable over busy screens**: toast surfaces now remain
  effectively opaque during their slide animation so Welcome/editor text cannot
  bleed through and look like stale toast copy.
- **Toast dismissal is mouse-verified**: visible toast clicks and
  **Notifications: Clear All Toasts** now emit trace evidence, and the strict
  mouse harness verifies both dismissal paths after real file-operation toasts.
- **Overflowing tabs are reachable with the mouse**: the tab strip now keeps a
  first-visible tab offset, scrolls with the mouse wheel over the top tab row,
  keeps the active tab visible after tab commands, and includes a screenshot
  gallery case for many-tab layouts.
- **Welcome starts real projects**: the first-run Welcome surface now exposes
  **New Mighty Project...** beside New File/Open File/Open Folder, routing the
  click into the existing `mty new` project prompt instead of hiding project
  creation behind the command palette.
- **Tab titles preserve the useful filename**: narrow tabs now truncate
  file-backed labels in the middle, keeping the basename start and extension
  visible instead of showing leading ellipses like `...swelcome.mty`.
- **Command surfaces read cleaner**: file-dialog commands no longer bake fake
  `...` truncation into their names, and Command Palette / Keyboard Shortcuts
  rows now measure titles and descriptions against right-side shortcut chrome.
- **Command shortcut gutters have breathing room**: Command Palette rows reserve
  a wider gap before keybinding chips, and the Keyboard Shortcuts modal compacts
  the selected-row remap affordance when the key gutter is tight.
- **Open Folder harness follows the human flow**: strict real-mouse verification
  now waits for the selected workspace trace before checking post-dialog
  responsiveness, avoiding false failures while a native picker handoff is still
  completing.
- **Top-bar command clicks win over editor focus**: More and command-center mouse
  clicks now open Palette/Quick Open before editor completion or panel focus can
  consume the following typed query.
- **Testing empty states stay readable**: the Testing panel now wraps its
  no-results guidance across measured lines in compact sidebars instead of
  truncating a short sentence.
- **Palette verification waits for real readiness**: command-surface traces now
  record palette opens, query updates, selected ids, and cancellation, and the
  strict mouse harness waits for palette-open state before typing commands.
- **Harness recents no longer pollute the user profile**: the config layer now
  honors `MUI_CONFIG_DIR`, and strict mouse runs use an output-local config dir
  so temporary picker workspaces cannot appear in the real Welcome/Open Recent
  history.
- **AI panel mouse verification is DPI-aware**: the strict mouse harness now
  derives the visible AI close-button target from the same titlebar geometry as
  the app instead of relying on a fixed coordinate.
- **Keyboard shortcut search is visually clean**: the empty search-field caret
  no longer draws on top of the placeholder text.
- **Command palette empty search is visually clean**: the focused caret no
  longer collides with the `Type a command...` placeholder.
- **SCM empty-state hints fit cleanly**: narrow sidebars now show concise
  action-oriented copy instead of tail-clipping to an unclear fragment.
- **Toast timing feels less stale**: success/info notifications now clear faster
  while warning/error messages remain visible longer, so completion messages do
  not follow users into unrelated panels.
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
  try to foreground the actual IDE window, move the OS cursor, and emit real
  mouse button events; automated sessions that cannot take foreground ownership fall
  back explicitly, while `-StrictRealMouse` fails instead of hiding that gap.
- **Windows UX harness waits for visible click effects**: strict mouse checks now
  normalize trace paths, account for UI scale, inject button events reliably, and
  wait for the topbar command-palette trace before typing command queries.
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
