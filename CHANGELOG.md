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
- **Breadcrumb file menus prune stale paths**: accepting a breadcrumb file row
  whose source disappeared now removes that dead path from the menu backing
  list while reporting the missing target.
- **Breadcrumb misses avoid navigation side effects**: failed breadcrumb accepts
  still close the dropdown and report feedback, but no longer reset undo or
  refresh diagnostics/outline as if a file or symbol jump succeeded.
- **Breakpoint rows prune missing targets**: opening a debug breakpoint row
  whose source file was deleted now removes the stale breakpoint entry and keeps
  the inventory from repeatedly targeting a missing file.
- **Testing result jumps distinguish stale rows**: out-of-range Testing result
  jumps now report `No test result row selected` instead of implying a visible
  row lacked a file target.
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
