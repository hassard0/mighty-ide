//! Command palette (shim-side, scalar-driven from Mighty).
//!
//! Mirrors [`crate::completion`]: the command registry + the query/filter/
//! selection state live here on the Rust side because the Mighty IDE drives the
//! shim through a scalar-only `extern c` ABI (L17) and keeps its own `Vec`
//! access flat (L21). Mighty opens the palette (Ctrl+Shift+P), feeds typed
//! chars / backspaces, moves the selection, then on Enter reads the selected
//! command id back and dispatches to the SAME code path the keybinding triggers.
//!
//! Filtering is a case-insensitive prefix-OR-subsequence (fuzzy) match against
//! each command's label and shortcut text, ranked so label matches sort ahead
//! of shortcut matches at the same quality. An empty query lists every command
//! in registry order.

use std::borrow::Cow;

use crate::ffi::MuiColor;
use crate::theme;

/// A single editor command in the palette: a stable numeric `id` (the contract
/// with the Mighty dispatch switch), a human `label`, and the `keybinding`
/// string shown right-aligned. `id`s are stable so reordering the table or
/// filtering never changes what Enter dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub id: u32,
    pub label: &'static str,
    pub keybinding: &'static str,
}

// Command ids — kept in sync with the dispatch switch in `src/main.mty`
// (`fn cmd_*` helpers). Stable numeric contract; do not renumber casually.
pub const CMD_OPEN_FILE: u32 = 1;
pub const CMD_SAVE: u32 = 2;
pub const CMD_FIND: u32 = 3;
pub const CMD_GOTO_LINE: u32 = 4;
pub const CMD_GOTO_DEFINITION: u32 = 5;
pub const CMD_HOVER: u32 = 6;
pub const CMD_TOGGLE_TERMINAL: u32 = 7;
pub const CMD_TOGGLE_SIDEBAR: u32 = 8;
pub const CMD_NEXT_TAB: u32 = 9;
pub const CMD_PREV_TAB: u32 = 10;
pub const CMD_CLOSE_TAB: u32 = 11;
pub const CMD_FORMAT_DOCUMENT: u32 = 12;
pub const CMD_UNDO: u32 = 13;
pub const CMD_REDO: u32 = 14;
pub const CMD_AUTOCOMPLETE: u32 = 15;
pub const CMD_JUMP_BACK: u32 = 16;
pub const CMD_QUIT: u32 = 17;
pub const CMD_COLOR_THEME: u32 = 18;
pub const CMD_RUN_FILE: u32 = 19;
pub const CMD_SETTINGS: u32 = 20;
pub const CMD_RUN_TESTS: u32 = 21;
pub const CMD_PEEK_DEFINITION: u32 = 22;
pub const CMD_WELCOME: u32 = 23;
pub const CMD_ZEN_MODE: u32 = 24;
pub const CMD_AGENTS: u32 = 25;
// Git commands — dispatched shim-side via `mui_git_dispatch` so the Mighty
// dispatch ladders need only ONE new arm each (L37/L38 parse-stack ceiling).
pub const CMD_GIT_SWITCH_BRANCH: u32 = 26;
pub const CMD_GIT_PUSH: u32 = 27;
pub const CMD_GIT_PULL: u32 = 28;
pub const CMD_GIT_FETCH: u32 = 29;
pub const CMD_GIT_TOGGLE_BLAME: u32 = 30;
/// First git command id (ids >= this are routed to `mui_git_dispatch`).
pub const CMD_GIT_FIRST: u32 = CMD_GIT_SWITCH_BRANCH;
/// Run the active file in the browser (wasm32-web Web Playground). Placed after
/// the git block so it stays out of the `>= CMD_GIT_FIRST` routing range.
pub const CMD_RUN_IN_BROWSER: u32 = 31;

/// Editor split-pane commands (routed to the single `mui_pane_dispatch` shim
/// entry so the Mighty palette ladder gains ONE arm range, not three — L37/L38).
pub const CMD_SPLIT_RIGHT: u32 = 32;
pub const CMD_FOCUS_NEXT_PANE: u32 = 33;
pub const CMD_CLOSE_PANE: u32 = 34;
/// Open the live Markdown preview in a split pane (routes via `mui_pane_dispatch`).
pub const CMD_MARKDOWN_PREVIEW: u32 = 35;
/// First/last pane command id (ids in `[FIRST, LAST]` route to `mui_pane_dispatch`).
pub const CMD_PANE_FIRST: u32 = CMD_SPLIT_RIGHT;
pub const CMD_PANE_LAST: u32 = CMD_MARKDOWN_PREVIEW;

/// Workspace / Open-Folder commands, routed via `mui_ws_dispatch` so the Mighty
/// palette ladder gains ONE arm range, not two (L37/L38 parse-stack ceiling).
pub const CMD_OPEN_FOLDER: u32 = 36;
pub const CMD_OPEN_RECENT: u32 = 37;
/// First/last workspace command id (range mirrored by the Mighty `cmd_ws_*`
/// helpers + routed via `mui_ws_dispatch`).
#[allow(dead_code)]
pub const CMD_WS_FIRST: u32 = CMD_OPEN_FOLDER;
#[allow(dead_code)]
pub const CMD_WS_LAST: u32 = CMD_OPEN_RECENT;

/// Open the Keyboard Shortcuts reference overlay (searchable command/keybinding
/// list + remapping). Dispatched directly in `src/main.mty` to `mui_keys_open`.
pub const CMD_KEYBOARD_SHORTCUTS: u32 = 38;

/// Code-folding commands, routed via `mui_fold_dispatch` so the Mighty palette
/// ladder gains ONE arm range, not three (L37/L38 parse-stack ceiling). Toggle
/// folds the region at the cursor; Fold/Unfold All act on the whole buffer.
pub const CMD_FOLD_TOGGLE: u32 = 39;
pub const CMD_FOLD_ALL: u32 = 40;
pub const CMD_UNFOLD_ALL: u32 = 41;
/// First/last fold command id (ids in `[FIRST, LAST]` route to `mui_fold_dispatch`;
/// the range is mirrored by the Mighty `cmd_fold_first/last` helpers).
#[allow(dead_code)]
pub const CMD_FOLD_FIRST: u32 = CMD_FOLD_TOGGLE;
#[allow(dead_code)]
pub const CMD_FOLD_LAST: u32 = CMD_UNFOLD_ALL;

/// "Mighty: New Project" — prompts for a name, runs `mty new <name>` (needs the
/// Mighty compiler on PATH), then opens the new project as the workspace. The
/// Mighty side collects the name through the bottom prompt and dispatches to
/// `mui_newproj_create`.
pub const CMD_NEW_PROJECT: u32 = 42;
/// Save the active document to a chosen path through the native Save As dialog.
pub const CMD_SAVE_AS: u32 = 43;
/// Create a named file through the native file picker.
pub const CMD_NEW_FILE: u32 = 44;
/// Prompt for a folder name and create it under the current workspace root.
pub const CMD_NEW_FOLDER: u32 = 45;
/// Prompt for a new basename and rename the active file.
pub const CMD_RENAME_ACTIVE_FILE: u32 = 46;
/// Expand Explorer folders so the active file is visible.
pub const CMD_REVEAL_ACTIVE_FILE: u32 = 47;
/// Prompt for exact basename confirmation, then delete the active file.
pub const CMD_DELETE_ACTIVE_FILE: u32 = 48;
/// Reveal the active file in the operating system file manager.
pub const CMD_REVEAL_ACTIVE_FILE_IN_OS: u32 = 49;
/// Copy the active file's absolute path to the system clipboard.
pub const CMD_COPY_ACTIVE_FILE_PATH: u32 = 50;
/// Copy the active file's path relative to the workspace/tree root.
pub const CMD_COPY_ACTIVE_FILE_RELATIVE_PATH: u32 = 51;
/// Clear every visible toast notification.
pub const CMD_CLEAR_NOTIFICATIONS: u32 = 52;
/// Save every dirty tab, asking for native picker destinations for untitled tabs.
pub const CMD_SAVE_ALL: u32 = 53;
/// Close every clean/saved tab, preserving dirty tabs.
pub const CMD_CLOSE_SAVED_TABS: u32 = 54;
/// Close every clean/saved tab except the active tab, preserving dirty tabs.
pub const CMD_CLOSE_OTHER_SAVED_TABS: u32 = 55;
/// Close clean/saved tabs to the right of the active tab, preserving dirty tabs.
pub const CMD_CLOSE_SAVED_TABS_TO_RIGHT: u32 = 56;
/// Close clean/saved tabs to the left of the active tab, preserving dirty tabs.
pub const CMD_CLOSE_SAVED_TABS_TO_LEFT: u32 = 57;
/// Reopen the most recently closed tab.
pub const CMD_REOPEN_CLOSED_TAB: u32 = 58;
/// Duplicate the active editor tab next to itself.
pub const CMD_DUPLICATE_ACTIVE_TAB: u32 = 59;
/// Reload the active file-backed tab from disk.
pub const CMD_RELOAD_ACTIVE_FILE: u32 = 60;
/// Discard local edits and reload the active file-backed tab from disk.
pub const CMD_REVERT_ACTIVE_FILE: u32 = 61;
/// Copy only the active file's basename to the system clipboard.
pub const CMD_COPY_ACTIVE_FILE_NAME: u32 = 62;
/// Copy the active file's containing directory to the system clipboard.
pub const CMD_COPY_ACTIVE_FILE_DIRECTORY: u32 = 63;
/// Move the active tab one slot to the left.
pub const CMD_MOVE_ACTIVE_TAB_LEFT: u32 = 64;
/// Move the active tab one slot to the right.
pub const CMD_MOVE_ACTIVE_TAB_RIGHT: u32 = 65;
/// Sort open tabs alphabetically by display name.
pub const CMD_SORT_TABS_BY_NAME: u32 = 66;
/// Close clean duplicate file-backed tabs, preserving dirty duplicates.
pub const CMD_CLOSE_DUPLICATE_TABS: u32 = 67;
/// Stage all changed paths in the current git repository.
pub const CMD_GIT_STAGE_ALL: u32 = 68;
/// Unstage all staged paths in the current git repository.
pub const CMD_GIT_UNSTAGE_ALL: u32 = 69;
/// Commit staged changes with the current Source Control message.
pub const CMD_GIT_COMMIT_STAGED: u32 = 70;
/// Open the Explorer view.
pub const CMD_VIEW_EXPLORER: u32 = 71;
/// Open the project Search view.
pub const CMD_VIEW_SEARCH: u32 = 72;
/// Open the Source Control view.
pub const CMD_VIEW_SOURCE_CONTROL: u32 = 73;
/// Open the Outline view.
pub const CMD_VIEW_OUTLINE: u32 = 74;
/// Open the Run and Debug view.
pub const CMD_VIEW_RUN_DEBUG: u32 = 75;
/// Open the Testing view.
pub const CMD_VIEW_TESTING: u32 = 76;
/// Open the Run output panel.
pub const CMD_VIEW_RUN_OUTPUT: u32 = 77;
/// Open the Problems panel.
pub const CMD_VIEW_PROBLEMS: u32 = 78;
/// Open the AI copilot panel.
pub const CMD_VIEW_AI_COPILOT: u32 = 79;
/// Open the integrated terminal.
pub const CMD_VIEW_TERMINAL: u32 = 80;
/// Open the Web Playground output panel.
pub const CMD_VIEW_WEB_PLAYGROUND: u32 = 81;
/// Start a debug session or continue the paused one.
pub const CMD_DEBUG_START_CONTINUE: u32 = 82;
/// Stop the active debug session.
pub const CMD_DEBUG_STOP: u32 = 83;
/// Step over the current debug line.
pub const CMD_DEBUG_STEP_OVER: u32 = 84;
/// Step into the current debug call.
pub const CMD_DEBUG_STEP_INTO: u32 = 85;
/// Step out of the current debug frame.
pub const CMD_DEBUG_STEP_OUT: u32 = 86;
/// Pause the active running debuggee.
pub const CMD_DEBUG_PAUSE: u32 = 87;
/// Restart the last debug target.
pub const CMD_DEBUG_RESTART: u32 = 88;
/// Prompt for a file name and create it under the current workspace root.
pub const CMD_NEW_WORKSPACE_FILE: u32 = 89;
/// Open a fresh untitled editor tab.
pub const CMD_NEW_UNTITLED_FILE: u32 = 90;
/// Set the shared bottom dock to its compact height.
pub const CMD_DOCK_COMPACT: u32 = 91;
/// Reset the shared bottom dock to its default height.
pub const CMD_DOCK_RESET: u32 = 92;
/// Set the shared bottom dock to its expanded height.
pub const CMD_DOCK_EXPANDED: u32 = 93;
/// Close whichever shared bottom dock is currently open.
pub const CMD_DOCK_CLOSE: u32 = 99;
/// Close the right-docked AI copilot panel.
pub const CMD_AI_CLOSE: u32 = 100;
/// Close the left sidebar drawer without changing the active sidebar panel.
pub const CMD_SIDEBAR_CLOSE: u32 = 101;
/// First/last bottom-dock layout command id.
#[allow(dead_code)]
pub const CMD_DOCK_FIRST: u32 = CMD_DOCK_COMPACT;
#[allow(dead_code)]
pub const CMD_DOCK_LAST: u32 = CMD_DOCK_EXPANDED;
/// Open the sidebar drawer at a compact width.
pub const CMD_SIDEBAR_COMPACT: u32 = 94;
/// Open the sidebar drawer at the responsive default width.
pub const CMD_SIDEBAR_DEFAULT: u32 = 95;
/// Open the sidebar drawer at a wider review/debug width.
pub const CMD_SIDEBAR_WIDE: u32 = 96;
/// Open the sidebar drawer and cycle through compact, default, and wide widths.
pub const CMD_SIDEBAR_CYCLE_WIDTH: u32 = 102;
/// Delete the current editor line without touching the clipboard.
pub const CMD_DELETE_LINE: u32 = 103;
/// Join the current editor line with the next line.
pub const CMD_JOIN_LINE: u32 = 104;
/// Select the word at the current editor cursor.
pub const CMD_SELECT_WORD: u32 = 105;
/// Duplicate the current editor line or selection.
pub const CMD_DUPLICATE_LINE_SELECTION: u32 = 106;
/// Move the current editor line or selection up one row.
pub const CMD_MOVE_LINE_UP: u32 = 107;
/// Move the current editor line or selection down one row.
pub const CMD_MOVE_LINE_DOWN: u32 = 108;
/// Select the entire active editor document.
pub const CMD_SELECT_ALL: u32 = 109;
/// Select the current editor line.
pub const CMD_SELECT_LINE: u32 = 110;
/// Toggle line comments for the current editor line or selection.
pub const CMD_TOGGLE_LINE_COMMENT: u32 = 111;
/// Copy the current editor selection, or the current line when no selection exists.
pub const CMD_COPY_SELECTION_OR_LINE: u32 = 112;
/// Cut the current editor selection, or the current line when no selection exists.
pub const CMD_CUT_SELECTION_OR_LINE: u32 = 113;
/// Paste clipboard text into the active editor.
pub const CMD_PASTE_IN_EDITOR: u32 = 114;
/// Delete the word before each active editor caret.
pub const CMD_DELETE_PREVIOUS_WORD: u32 = 115;
/// Delete the word after each active editor caret.
pub const CMD_DELETE_NEXT_WORD: u32 = 116;
/// Indent the current editor line or selected line range.
pub const CMD_INDENT_LINE_SELECTION: u32 = 117;
/// Outdent the current editor line or selected line range.
pub const CMD_OUTDENT_LINE_SELECTION: u32 = 118;
/// Move each active editor caret to the previous word boundary.
pub const CMD_MOVE_WORD_LEFT: u32 = 119;
/// Move each active editor caret to the next word boundary.
pub const CMD_MOVE_WORD_RIGHT: u32 = 120;
/// Move each active editor caret to the start of the document.
pub const CMD_MOVE_DOCUMENT_START: u32 = 121;
/// Move each active editor caret to the end of the document.
pub const CMD_MOVE_DOCUMENT_END: u32 = 122;
/// Move each active editor caret to the smart start of its line.
pub const CMD_MOVE_LINE_START: u32 = 123;
/// Move each active editor caret to the end of its line.
pub const CMD_MOVE_LINE_END: u32 = 124;
/// Add a caret at the next occurrence of the active selection/word.
pub const CMD_ADD_CARET_NEXT_OCCURRENCE: u32 = 125;
/// Add a caret on the line above the primary caret.
pub const CMD_ADD_CARET_ABOVE: u32 = 126;
/// Add a caret on the line below the primary caret.
pub const CMD_ADD_CARET_BELOW: u32 = 127;
/// Collapse all editor carets back to the primary caret.
pub const CMD_COLLAPSE_CARETS: u32 = 128;
/// Open the in-file find and replace bar.
pub const CMD_FIND_REPLACE: u32 = 129;
/// Show signature help at the editor cursor.
pub const CMD_SIGNATURE_HELP: u32 = 130;
/// Start symbol rename at the editor cursor.
pub const CMD_RENAME_SYMBOL: u32 = 131;
/// Show code actions and quick fixes at the editor cursor.
pub const CMD_CODE_ACTIONS: u32 = 132;
/// Open the inline AI ask prompt for the active selection or file.
pub const CMD_INLINE_AI_ASK: u32 = 133;
/// Force an inline AI ghost-text completion at the cursor.
pub const CMD_FORCE_GHOST_COMPLETION: u32 = 134;
/// Open Universal Quick Open for files, commands, symbols, and line jumps.
pub const CMD_QUICK_OPEN: u32 = 135;
/// Stop the currently running `mty run` process.
pub const CMD_RUN_STOP: u32 = 136;
/// Stop the currently running `mty test` process.
pub const CMD_TEST_STOP: u32 = 137;
/// Run the package tests and focus the nearest `fn test_*` around the cursor.
pub const CMD_RUN_TEST_AT_CURSOR: u32 = 138;
/// Stop the currently running Web Playground server.
pub const CMD_WEB_STOP: u32 = 139;
/// Open the current Web Playground URL in the default browser.
pub const CMD_WEB_OPEN_BROWSER: u32 = 140;
/// Clear the AI copilot transcript and draft composer.
pub const CMD_AI_CLEAR_CHAT: u32 = 141;
/// Increase the UI zoom by one step.
pub const CMD_ZOOM_IN: u32 = 142;
/// Decrease the UI zoom by one step.
pub const CMD_ZOOM_OUT: u32 = 143;
/// Reset the UI zoom to 100%.
pub const CMD_ZOOM_RESET: u32 = 144;
/// Collapse every expanded folder in the Explorer tree.
pub const CMD_EXPLORER_COLLAPSE_ALL: u32 = 145;
/// Run the project-wide Search panel query.
pub const CMD_SEARCH_RUN: u32 = 146;
/// Replace all project-wide Search panel matches.
pub const CMD_SEARCH_REPLACE_ALL: u32 = 147;
/// Open Search and move focus between the query and replace fields.
pub const CMD_SEARCH_TOGGLE_REPLACE: u32 = 148;
/// Refresh the Source Control panel's git status.
pub const CMD_GIT_REFRESH_SOURCE_CONTROL: u32 = 149;
/// Refresh the Explorer tree and file-navigation index.
pub const CMD_EXPLORER_REFRESH: u32 = 150;
/// Refresh diagnostics and show the Problems panel.
pub const CMD_PROBLEMS_REFRESH: u32 = 151;
/// Refresh the active document's Outline symbols.
pub const CMD_OUTLINE_REFRESH: u32 = 152;
/// Refresh the Mighty Agents topology model.
pub const CMD_AGENTS_REFRESH: u32 = 153;
/// Clear the Run panel's rendered output without stopping a process.
pub const CMD_RUN_CLEAR_OUTPUT: u32 = 154;
/// Clear the Testing panel's parsed results without stopping a run.
pub const CMD_TEST_CLEAR_RESULTS: u32 = 155;
/// Clear the Web Playground's rendered output without stopping a server.
pub const CMD_WEB_CLEAR_OUTPUT: u32 = 156;
/// Clear the Mighty Agents run transcript without rebuilding topology.
pub const CMD_AGENTS_CLEAR_RUN_OUTPUT: u32 = 157;
/// Close the inline git diff view and return to editing.
pub const CMD_DIFF_CLOSE_VIEW: u32 = 158;
/// Hide the git blame gutter without toggling it back on.
pub const CMD_GIT_HIDE_BLAME: u32 = 159;
/// Close the inline Peek Definition card and return to editing.
pub const CMD_PEEK_CLOSE: u32 = 160;
/// Close the hover popup without requesting new language-server data.
pub const CMD_HOVER_CLOSE: u32 = 161;
/// Close the signature-help popup without requesting new language-server data.
pub const CMD_SIGNATURE_HELP_CLOSE: u32 = 162;
/// Close the live Markdown preview pane without toggling it open.
pub const CMD_MARKDOWN_CLOSE_PREVIEW: u32 = 163;
/// Close the Settings panel without changing preferences.
pub const CMD_SETTINGS_CLOSE: u32 = 164;
/// Close the color theme picker and revert any uncommitted preview.
pub const CMD_COLOR_THEME_CLOSE: u32 = 165;
/// Close the Keyboard Shortcuts overlay even when shortcut capture is active.
pub const CMD_KEYBOARD_SHORTCUTS_CLOSE: u32 = 166;
/// Close the inline rename input without applying a rename.
pub const CMD_RENAME_CANCEL: u32 = 167;
/// Close the Code Actions menu without applying an action.
pub const CMD_CODE_ACTIONS_CLOSE: u32 = 168;
/// Close the active bottom prompt without applying its typed input.
pub const CMD_PROMPT_CANCEL: u32 = 169;
/// Close the in-file Find & Replace bar without applying replacement text.
pub const CMD_FIND_REPLACE_CLOSE: u32 = 170;
/// Close the autocomplete suggestions dropdown without accepting a candidate.
pub const CMD_AUTOCOMPLETE_CLOSE: u32 = 171;
/// Cancel the unsaved-work confirmation overlay without saving or discarding.
pub const CMD_DIRTY_CONFIRM_CANCEL: u32 = 172;
/// Close the Git branch switcher without checking out or creating a branch.
pub const CMD_GIT_BRANCH_CANCEL: u32 = 173;
/// Close the breadcrumb dropdown without opening a file or jumping to a symbol.
pub const CMD_BREADCRUMB_MENU_CANCEL: u32 = 174;
/// Close the command palette without executing the highlighted command.
pub const CMD_COMMAND_PALETTE_CLOSE: u32 = 175;
/// Close Quick Open without opening a file, command, symbol, or line jump.
pub const CMD_QUICK_OPEN_CLOSE: u32 = 176;
/// Close the forced Welcome or Open Recent surface without opening anything.
pub const CMD_WELCOME_CLOSE: u32 = 177;
/// Dismiss the visible inline AI ghost completion without accepting text.
pub const CMD_GHOST_COMPLETION_DISMISS: u32 = 178;
/// Cancel the active snippet tab-stop session without removing expanded text.
pub const CMD_SNIPPET_CANCEL: u32 = 179;
/// Close the integrated terminal without affecting other bottom-dock panels.
pub const CMD_TERMINAL_CLOSE: u32 = 180;
/// Close the Problems panel without affecting other bottom-dock tools.
pub const CMD_PROBLEMS_CLOSE: u32 = 181;
/// Close the Run panel without stopping the active process or clearing output.
pub const CMD_RUN_CLOSE: u32 = 182;
/// Close the Testing panel without stopping a test run or clearing results.
pub const CMD_TEST_CLOSE: u32 = 183;
/// Close the Web Playground panel without stopping the server or clearing output.
pub const CMD_WEB_CLOSE: u32 = 184;
/// Close the Mighty Agents panel without clearing topology or run output.
pub const CMD_AGENTS_CLOSE: u32 = 185;
/// Close the Search panel without clearing query or results.
pub const CMD_SEARCH_CLOSE: u32 = 186;
/// Close the Outline panel without clearing document symbols.
pub const CMD_OUTLINE_CLOSE: u32 = 187;
/// Close the Source Control panel without clearing git status or message state.
pub const CMD_GIT_CLOSE_SOURCE_CONTROL: u32 = 188;
/// Close the Run and Debug panel without stopping or resetting the debug model.
pub const CMD_DEBUG_CLOSE: u32 = 189;
/// Close the Explorer panel without clearing or collapsing the file tree.
pub const CMD_EXPLORER_CLOSE: u32 = 190;
/// Clear the Problems panel diagnostics without closing the panel.
pub const CMD_PROBLEMS_CLEAR: u32 = 191;
/// Clear the Source Control commit-message draft without changing git status.
pub const CMD_GIT_CLEAR_COMMIT_MESSAGE: u32 = 192;
/// Clear Search results while preserving query and replace text.
pub const CMD_SEARCH_CLEAR_RESULTS: u32 = 193;
/// Clear Outline symbols without closing the panel.
pub const CMD_OUTLINE_CLEAR_SYMBOLS: u32 = 194;
/// Clear the current debug session without clearing breakpoints or target.
pub const CMD_DEBUG_CLEAR_SESSION: u32 = 195;
/// Clear the integrated terminal's visible buffer without closing the shell.
pub const CMD_TERMINAL_CLEAR: u32 = 196;
/// Reset the selected Keyboard Shortcuts override to its default binding.
pub const CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED: u32 = 197;
/// Reset every Keyboard Shortcuts override to default bindings.
pub const CMD_KEYBOARD_SHORTCUTS_RESET_ALL: u32 = 198;
/// Toggle a breakpoint at the active editor cursor.
pub const CMD_DEBUG_TOGGLE_BREAKPOINT: u32 = 199;
/// Clear every stored debug breakpoint.
pub const CMD_DEBUG_CLEAR_BREAKPOINTS: u32 = 200;
/// First/last sidebar layout command id.
#[allow(dead_code)]
pub const CMD_SIDEBAR_FIRST: u32 = CMD_SIDEBAR_COMPACT;
#[allow(dead_code)]
pub const CMD_SIDEBAR_LAST: u32 = CMD_SIDEBAR_WIDE;
/// Toggle the native window between restored and maximized states.
pub const CMD_WINDOW_TOGGLE_MAXIMIZE: u32 = 97;
/// Minimize the native IDE window.
pub const CMD_WINDOW_MINIMIZE: u32 = 98;

/// The static command registry. Every action the editor exposes appears here
/// with its keybinding label. Registry order is the default (empty-query) order.
pub const COMMANDS: &[Command] = &[
    Command { id: CMD_NEW_FILE,         label: "File: New File...", keybinding: "Ctrl+N" },
    Command { id: CMD_NEW_UNTITLED_FILE, label: "File: New Untitled File", keybinding: "" },
    Command { id: CMD_NEW_WORKSPACE_FILE, label: "Explorer: New File in Workspace", keybinding: "" },
    Command { id: CMD_NEW_FOLDER,       label: "Explorer: New Folder...",   keybinding: "Ctrl+Shift+N" },
    Command { id: CMD_OPEN_FILE,        label: "File: Open File...", keybinding: "Ctrl+O" },
    Command { id: CMD_SAVE,             label: "File: Save",         keybinding: "Ctrl+S" },
    Command { id: CMD_SAVE_AS,          label: "File: Save As...",   keybinding: "Ctrl+Shift+S" },
    Command { id: CMD_SAVE_ALL,         label: "File: Save All",     keybinding: "Ctrl+Alt+S" },
    Command { id: CMD_RENAME_ACTIVE_FILE, label: "File: Rename Active File", keybinding: "" },
    Command { id: CMD_REVEAL_ACTIVE_FILE, label: "File: Reveal Active File in File Tree", keybinding: "" },
    Command { id: CMD_EXPLORER_REFRESH, label: "Explorer: Refresh",   keybinding: "" },
    Command { id: CMD_EXPLORER_COLLAPSE_ALL, label: "Explorer: Collapse All Folders", keybinding: "" },
    Command { id: CMD_EXPLORER_CLOSE, label: "Explorer: Close Panel", keybinding: "" },
    Command { id: CMD_REVEAL_ACTIVE_FILE_IN_OS, label: "File: Show Active File in File Manager", keybinding: "" },
    Command { id: CMD_COPY_ACTIVE_FILE_PATH, label: "File: Copy Active File Path", keybinding: "" },
    Command { id: CMD_COPY_ACTIVE_FILE_RELATIVE_PATH, label: "File: Copy Active File Relative Path", keybinding: "" },
    Command { id: CMD_COPY_ACTIVE_FILE_NAME, label: "File: Copy Active File Name", keybinding: "" },
    Command { id: CMD_COPY_ACTIVE_FILE_DIRECTORY, label: "File: Copy Active File Directory", keybinding: "" },
    Command { id: CMD_DELETE_ACTIVE_FILE, label: "File: Delete Active File", keybinding: "" },
    Command { id: CMD_CLEAR_NOTIFICATIONS, label: "Notifications: Clear All Toasts", keybinding: "" },
    Command { id: CMD_QUICK_OPEN,       label: "Quick Open",          keybinding: "Ctrl+P" },
    Command { id: CMD_FIND,             label: "Find",               keybinding: "Ctrl+F" },
    Command { id: CMD_FIND_REPLACE,     label: "Find & Replace",     keybinding: "Ctrl+H" },
    Command { id: CMD_FIND_REPLACE_CLOSE, label: "Find & Replace: Close Bar", keybinding: "" },
    Command { id: CMD_GOTO_LINE,        label: "Go to Line",         keybinding: "Ctrl+G" },
    Command { id: CMD_GOTO_DEFINITION,  label: "Go to Definition",   keybinding: "F12" },
    Command { id: CMD_HOVER,            label: "Show Hover",         keybinding: "Ctrl+K" },
    Command { id: CMD_HOVER_CLOSE,      label: "Hover: Close Popup", keybinding: "" },
    Command { id: CMD_SIGNATURE_HELP,   label: "Show Signature Help", keybinding: "Ctrl+Shift+Space" },
    Command { id: CMD_SIGNATURE_HELP_CLOSE, label: "Signature Help: Close Popup", keybinding: "" },
    Command { id: CMD_RENAME_SYMBOL,    label: "Rename Symbol",      keybinding: "F2" },
    Command { id: CMD_RENAME_CANCEL,    label: "Rename Symbol: Cancel", keybinding: "" },
    Command { id: CMD_CODE_ACTIONS,     label: "Code Actions",       keybinding: "Ctrl+." },
    Command { id: CMD_CODE_ACTIONS_CLOSE, label: "Code Actions: Close Menu", keybinding: "" },
    Command { id: CMD_PROMPT_CANCEL,    label: "Prompt: Cancel Input", keybinding: "" },
    Command { id: CMD_TOGGLE_TERMINAL,  label: "Terminal: Open or Focus", keybinding: "Ctrl+`" },
    Command { id: CMD_TOGGLE_SIDEBAR,   label: "View: Toggle Sidebar", keybinding: "Ctrl+B" },
    Command { id: CMD_NEXT_TAB,         label: "Next Tab",           keybinding: "Ctrl+Tab" },
    Command { id: CMD_PREV_TAB,         label: "Previous Tab",       keybinding: "Ctrl+Shift+Tab" },
    Command { id: CMD_CLOSE_TAB,        label: "Close Tab",          keybinding: "Ctrl+W" },
    Command { id: CMD_CLOSE_SAVED_TABS, label: "File: Close Saved Tabs", keybinding: "" },
    Command { id: CMD_CLOSE_OTHER_SAVED_TABS, label: "File: Close Other Saved Tabs", keybinding: "" },
    Command { id: CMD_CLOSE_SAVED_TABS_TO_RIGHT, label: "File: Close Saved Tabs to the Right", keybinding: "" },
    Command { id: CMD_CLOSE_SAVED_TABS_TO_LEFT, label: "File: Close Saved Tabs to the Left", keybinding: "" },
    Command { id: CMD_REOPEN_CLOSED_TAB, label: "File: Reopen Closed Tab", keybinding: "Ctrl+Alt+T" },
    Command { id: CMD_DUPLICATE_ACTIVE_TAB, label: "File: Duplicate Active Tab", keybinding: "" },
    Command { id: CMD_MOVE_ACTIVE_TAB_LEFT, label: "File: Move Active Tab Left", keybinding: "Ctrl+Shift+PageUp" },
    Command { id: CMD_MOVE_ACTIVE_TAB_RIGHT, label: "File: Move Active Tab Right", keybinding: "Ctrl+Shift+PageDown" },
    Command { id: CMD_SORT_TABS_BY_NAME, label: "File: Sort Open Tabs by Name", keybinding: "" },
    Command { id: CMD_CLOSE_DUPLICATE_TABS, label: "File: Close Duplicate Tabs", keybinding: "" },
    Command { id: CMD_RELOAD_ACTIVE_FILE, label: "File: Reload Active File from Disk", keybinding: "" },
    Command { id: CMD_REVERT_ACTIVE_FILE, label: "File: Revert Active File from Disk", keybinding: "" },
    Command { id: CMD_SELECT_ALL,       label: "Edit: Select All", keybinding: "Ctrl+A" },
    Command { id: CMD_SELECT_LINE,      label: "Edit: Select Line", keybinding: "Ctrl+L" },
    Command { id: CMD_SELECT_WORD,      label: "Edit: Select Word", keybinding: "Ctrl+D (first press)" },
    Command { id: CMD_TOGGLE_LINE_COMMENT, label: "Edit: Toggle Line Comment", keybinding: "Ctrl+/" },
    Command { id: CMD_COPY_SELECTION_OR_LINE, label: "Edit: Copy Selection or Line", keybinding: "Ctrl+C" },
    Command { id: CMD_CUT_SELECTION_OR_LINE, label: "Edit: Cut Selection or Line", keybinding: "Ctrl+X" },
    Command { id: CMD_PASTE_IN_EDITOR,  label: "Edit: Paste", keybinding: "Ctrl+V" },
    Command { id: CMD_DELETE_PREVIOUS_WORD, label: "Edit: Delete Previous Word", keybinding: "Ctrl+Backspace" },
    Command { id: CMD_DELETE_NEXT_WORD, label: "Edit: Delete Next Word", keybinding: "Ctrl+Delete" },
    Command { id: CMD_INDENT_LINE_SELECTION, label: "Edit: Indent Line or Selection", keybinding: "Tab" },
    Command { id: CMD_OUTDENT_LINE_SELECTION, label: "Edit: Outdent Line or Selection", keybinding: "Shift+Tab" },
    Command { id: CMD_MOVE_WORD_LEFT, label: "Edit: Move Cursor Word Left", keybinding: "Ctrl+Left" },
    Command { id: CMD_MOVE_WORD_RIGHT, label: "Edit: Move Cursor Word Right", keybinding: "Ctrl+Right" },
    Command { id: CMD_MOVE_DOCUMENT_START, label: "Edit: Move Cursor to Document Start", keybinding: "Ctrl+Home" },
    Command { id: CMD_MOVE_DOCUMENT_END, label: "Edit: Move Cursor to Document End", keybinding: "Ctrl+End" },
    Command { id: CMD_MOVE_LINE_START, label: "Edit: Move Cursor to Line Start", keybinding: "Home" },
    Command { id: CMD_MOVE_LINE_END, label: "Edit: Move Cursor to Line End", keybinding: "End" },
    Command { id: CMD_ADD_CARET_NEXT_OCCURRENCE, label: "Edit: Add Cursor to Next Occurrence", keybinding: "Ctrl+D" },
    Command { id: CMD_ADD_CARET_ABOVE, label: "Edit: Add Cursor Above", keybinding: "Ctrl+Alt+Up" },
    Command { id: CMD_ADD_CARET_BELOW, label: "Edit: Add Cursor Below", keybinding: "Ctrl+Alt+Down" },
    Command { id: CMD_COLLAPSE_CARETS, label: "Edit: Collapse Multiple Cursors", keybinding: "Esc" },
    Command { id: CMD_DUPLICATE_LINE_SELECTION, label: "Edit: Duplicate Line or Selection", keybinding: "Ctrl+Shift+D" },
    Command { id: CMD_MOVE_LINE_UP,     label: "Edit: Move Line Up", keybinding: "Alt+Up" },
    Command { id: CMD_MOVE_LINE_DOWN,   label: "Edit: Move Line Down", keybinding: "Alt+Down" },
    Command { id: CMD_DELETE_LINE,      label: "Edit: Delete Line", keybinding: "Ctrl+Shift+K" },
    Command { id: CMD_JOIN_LINE,        label: "Edit: Join Line",   keybinding: "Ctrl+J" },
    Command { id: CMD_FORMAT_DOCUMENT,  label: "Format Document",    keybinding: "Ctrl+Shift+I" },
    Command { id: CMD_UNDO,             label: "Undo",               keybinding: "Ctrl+Z" },
    Command { id: CMD_REDO,             label: "Redo",               keybinding: "Ctrl+Y" },
    Command { id: CMD_AUTOCOMPLETE,     label: "Trigger Autocomplete", keybinding: "Ctrl+Space" },
    Command { id: CMD_AUTOCOMPLETE_CLOSE, label: "Autocomplete: Close Suggestions", keybinding: "" },
    Command { id: CMD_DIRTY_CONFIRM_CANCEL, label: "Unsaved Changes: Cancel Confirmation", keybinding: "" },
    Command { id: CMD_GIT_BRANCH_CANCEL, label: "Git: Close Branch Switcher", keybinding: "" },
    Command { id: CMD_BREADCRUMB_MENU_CANCEL, label: "Breadcrumb: Close Menu", keybinding: "" },
    Command { id: CMD_COMMAND_PALETTE_CLOSE, label: "Command Palette: Close", keybinding: "" },
    Command { id: CMD_QUICK_OPEN_CLOSE, label: "Quick Open: Close", keybinding: "" },
    Command { id: CMD_WELCOME_CLOSE, label: "Welcome: Close", keybinding: "" },
    Command { id: CMD_SNIPPET_CANCEL, label: "Snippet: Cancel Tab-Stop Session", keybinding: "" },
    Command { id: CMD_JUMP_BACK,        label: "Jump Back",          keybinding: "" },
    Command { id: CMD_QUIT,             label: "Quit",               keybinding: "Esc / close" },
    Command { id: CMD_COLOR_THEME,      label: "Preferences: Color Theme", keybinding: "" },
    Command { id: CMD_COLOR_THEME_CLOSE, label: "Preferences: Close Color Theme Picker", keybinding: "" },
    Command { id: CMD_RUN_FILE,         label: "Run File",           keybinding: "Ctrl+Shift+R" },
    Command { id: CMD_RUN_STOP,         label: "Run: Stop Process",  keybinding: "" },
    Command { id: CMD_RUN_CLEAR_OUTPUT, label: "Run: Clear Output",  keybinding: "" },
    Command { id: CMD_RUN_CLOSE,        label: "Run: Close Panel",   keybinding: "" },
    Command { id: CMD_SETTINGS,         label: "Preferences: Settings", keybinding: "Ctrl+," },
    Command { id: CMD_SETTINGS_CLOSE,   label: "Preferences: Close Settings", keybinding: "" },
    Command { id: CMD_ZOOM_IN,          label: "View: Zoom In",      keybinding: "Ctrl+=" },
    Command { id: CMD_ZOOM_OUT,         label: "View: Zoom Out",     keybinding: "Ctrl+-" },
    Command { id: CMD_ZOOM_RESET,       label: "View: Reset Zoom",   keybinding: "Ctrl+0" },
    Command { id: CMD_RUN_TESTS,        label: "Run Tests",          keybinding: "Ctrl+Shift+T" },
    Command { id: CMD_RUN_TEST_AT_CURSOR, label: "Run Test at Cursor", keybinding: "" },
    Command { id: CMD_TEST_STOP,        label: "Test: Stop Run",     keybinding: "" },
    Command { id: CMD_TEST_CLEAR_RESULTS, label: "Test: Clear Results", keybinding: "" },
    Command { id: CMD_TEST_CLOSE,       label: "Test: Close Panel",  keybinding: "" },
    Command { id: CMD_PEEK_DEFINITION,  label: "Peek Definition",    keybinding: "Alt+F12" },
    Command { id: CMD_PEEK_CLOSE,       label: "Peek: Close View",   keybinding: "" },
    Command { id: CMD_WELCOME,          label: "Welcome",            keybinding: "" },
    Command { id: CMD_ZEN_MODE,         label: "Toggle Zen Mode",    keybinding: "Alt+Z" },
    Command { id: CMD_AGENTS,           label: "Mighty: Agents",     keybinding: "Alt+G" },
    Command { id: CMD_AGENTS_REFRESH,   label: "Mighty Agents: Refresh Topology", keybinding: "" },
    Command { id: CMD_AGENTS_CLEAR_RUN_OUTPUT, label: "Mighty Agents: Clear Run Output", keybinding: "" },
    Command { id: CMD_AGENTS_CLOSE,     label: "Mighty Agents: Close Panel", keybinding: "" },
    Command { id: CMD_GIT_SWITCH_BRANCH, label: "Git: Switch Branch", keybinding: "" },
    Command { id: CMD_GIT_PUSH,         label: "Git: Push",          keybinding: "" },
    Command { id: CMD_GIT_PULL,         label: "Git: Pull",          keybinding: "" },
    Command { id: CMD_GIT_FETCH,        label: "Git: Fetch",         keybinding: "" },
    Command { id: CMD_GIT_TOGGLE_BLAME, label: "Git: Toggle Blame",  keybinding: "Alt+B" },
    Command { id: CMD_GIT_HIDE_BLAME,   label: "Git: Hide Blame",    keybinding: "" },
    Command { id: CMD_GIT_STAGE_ALL,    label: "Git: Stage All",     keybinding: "" },
    Command { id: CMD_GIT_UNSTAGE_ALL,  label: "Git: Unstage All",   keybinding: "" },
    Command { id: CMD_GIT_COMMIT_STAGED, label: "Git: Commit Staged", keybinding: "" },
    Command { id: CMD_GIT_CLEAR_COMMIT_MESSAGE, label: "Source Control: Clear Commit Message", keybinding: "" },
    Command { id: CMD_GIT_REFRESH_SOURCE_CONTROL, label: "Git: Refresh Source Control", keybinding: "" },
    Command { id: CMD_GIT_CLOSE_SOURCE_CONTROL, label: "Source Control: Close Panel", keybinding: "" },
    Command { id: CMD_VIEW_EXPLORER,    label: "View: Explorer",      keybinding: "" },
    Command { id: CMD_VIEW_SEARCH,      label: "View: Search",        keybinding: "Ctrl+Shift+F" },
    Command { id: CMD_SEARCH_RUN,       label: "Search: Run Search",   keybinding: "" },
    Command { id: CMD_SEARCH_CLEAR_RESULTS, label: "Search: Clear Results", keybinding: "" },
    Command { id: CMD_SEARCH_REPLACE_ALL, label: "Search: Replace All", keybinding: "" },
    Command { id: CMD_SEARCH_TOGGLE_REPLACE, label: "Search: Toggle Replace Field", keybinding: "" },
    Command { id: CMD_SEARCH_CLOSE,     label: "Search: Close Panel", keybinding: "" },
    Command { id: CMD_VIEW_SOURCE_CONTROL, label: "View: Source Control", keybinding: "Ctrl+Shift+G" },
    Command { id: CMD_VIEW_OUTLINE,     label: "View: Outline",       keybinding: "" },
    Command { id: CMD_OUTLINE_REFRESH,  label: "Outline: Refresh Symbols", keybinding: "" },
    Command { id: CMD_OUTLINE_CLEAR_SYMBOLS, label: "Outline: Clear Symbols", keybinding: "" },
    Command { id: CMD_OUTLINE_CLOSE,    label: "Outline: Close Panel", keybinding: "" },
    Command { id: CMD_VIEW_RUN_DEBUG,   label: "View: Run and Debug", keybinding: "" },
    Command { id: CMD_VIEW_TESTING,     label: "View: Testing",       keybinding: "" },
    Command { id: CMD_VIEW_RUN_OUTPUT,  label: "View: Run Output",    keybinding: "" },
    Command { id: CMD_VIEW_PROBLEMS,    label: "View: Problems",      keybinding: "" },
    Command { id: CMD_PROBLEMS_REFRESH, label: "Problems: Refresh Diagnostics", keybinding: "" },
    Command { id: CMD_PROBLEMS_CLEAR,   label: "Problems: Clear Diagnostics", keybinding: "" },
    Command { id: CMD_PROBLEMS_CLOSE,   label: "Problems: Close Panel", keybinding: "" },
    Command { id: CMD_VIEW_AI_COPILOT,  label: "View: AI Copilot",    keybinding: "Ctrl+Shift+A" },
    Command { id: CMD_INLINE_AI_ASK,    label: "AI: Inline Ask",      keybinding: "Ctrl+I" },
    Command { id: CMD_FORCE_GHOST_COMPLETION, label: "AI: Force Ghost Completion", keybinding: "Alt+\\" },
    Command { id: CMD_GHOST_COMPLETION_DISMISS, label: "AI: Dismiss Ghost Completion", keybinding: "" },
    Command { id: CMD_AI_CLEAR_CHAT,    label: "AI: Clear Chat",      keybinding: "" },
    Command { id: CMD_AI_CLOSE,         label: "View: Close AI Copilot", keybinding: "" },
    Command { id: CMD_SIDEBAR_CLOSE,    label: "View: Close Sidebar", keybinding: "" },
    Command { id: CMD_VIEW_TERMINAL,    label: "View: Terminal",      keybinding: "Ctrl+`" },
    Command { id: CMD_TERMINAL_CLEAR,   label: "Terminal: Clear Buffer", keybinding: "" },
    Command { id: CMD_TERMINAL_CLOSE,   label: "Terminal: Close",     keybinding: "" },
    Command { id: CMD_VIEW_WEB_PLAYGROUND, label: "View: Web Playground", keybinding: "" },
    Command { id: CMD_DIFF_CLOSE_VIEW,  label: "Diff: Close View",    keybinding: "" },
    Command { id: CMD_DOCK_COMPACT,     label: "View: Bottom Dock Compact", keybinding: "" },
    Command { id: CMD_DOCK_RESET,       label: "View: Bottom Dock Default Size", keybinding: "" },
    Command { id: CMD_DOCK_EXPANDED,    label: "View: Bottom Dock Expanded", keybinding: "" },
    Command { id: CMD_DOCK_CLOSE,       label: "View: Close Bottom Dock", keybinding: "" },
    Command { id: CMD_SIDEBAR_COMPACT,  label: "View: Sidebar Compact", keybinding: "" },
    Command { id: CMD_SIDEBAR_DEFAULT,  label: "View: Sidebar Default Width", keybinding: "" },
    Command { id: CMD_SIDEBAR_WIDE,     label: "View: Sidebar Wide", keybinding: "" },
    Command { id: CMD_SIDEBAR_CYCLE_WIDTH, label: "View: Cycle Sidebar Width", keybinding: "Ctrl+Alt+B" },
    Command { id: CMD_WINDOW_TOGGLE_MAXIMIZE, label: "Window: Toggle Maximize", keybinding: "" },
    Command { id: CMD_WINDOW_MINIMIZE,  label: "Window: Minimize", keybinding: "" },
    Command { id: CMD_DEBUG_START_CONTINUE, label: "Debug: Start / Continue", keybinding: "F5" },
    Command { id: CMD_DEBUG_STOP,       label: "Debug: Stop",         keybinding: "Shift+F5" },
    Command { id: CMD_DEBUG_STEP_OVER,  label: "Debug: Step Over",    keybinding: "F10" },
    Command { id: CMD_DEBUG_STEP_INTO,  label: "Debug: Step Into",    keybinding: "F11" },
    Command { id: CMD_DEBUG_STEP_OUT,   label: "Debug: Step Out",     keybinding: "Shift+F11" },
    Command { id: CMD_DEBUG_PAUSE,      label: "Debug: Pause",        keybinding: "" },
    Command { id: CMD_DEBUG_RESTART,    label: "Debug: Restart",      keybinding: "" },
    Command { id: CMD_DEBUG_TOGGLE_BREAKPOINT, label: "Debug: Toggle Breakpoint at Cursor", keybinding: "" },
    Command { id: CMD_DEBUG_CLEAR_BREAKPOINTS, label: "Debug: Clear Breakpoints", keybinding: "" },
    Command { id: CMD_DEBUG_CLEAR_SESSION, label: "Run and Debug: Clear Session", keybinding: "" },
    Command { id: CMD_DEBUG_CLOSE,      label: "Run and Debug: Close Panel", keybinding: "" },
    Command { id: CMD_RUN_IN_BROWSER,   label: "Mighty: Run in Browser", keybinding: "Alt+W" },
    Command { id: CMD_WEB_STOP,         label: "Web: Stop Server",    keybinding: "" },
    Command { id: CMD_WEB_OPEN_BROWSER, label: "Web: Open in Browser", keybinding: "" },
    Command { id: CMD_WEB_CLEAR_OUTPUT, label: "Web: Clear Output",   keybinding: "" },
    Command { id: CMD_WEB_CLOSE,        label: "Web: Close Panel",    keybinding: "" },
    Command { id: CMD_SPLIT_RIGHT,      label: "Split Editor Right", keybinding: "Ctrl+\\" },
    Command { id: CMD_FOCUS_NEXT_PANE,  label: "Focus Next Editor Pane", keybinding: "Ctrl+1 / Ctrl+2" },
    Command { id: CMD_CLOSE_PANE,       label: "Close Editor Pane",  keybinding: "" },
    Command { id: CMD_MARKDOWN_PREVIEW, label: "Markdown: Open Preview", keybinding: "Ctrl+Shift+V" },
    Command { id: CMD_MARKDOWN_CLOSE_PREVIEW, label: "Markdown: Close Preview", keybinding: "" },
    Command { id: CMD_OPEN_FOLDER,      label: "File: Open Folder...", keybinding: "Ctrl+Shift+O" },
    Command { id: CMD_OPEN_RECENT,      label: "File: Open Recent",   keybinding: "" },
    Command { id: CMD_KEYBOARD_SHORTCUTS, label: "Help: Keyboard Shortcuts", keybinding: "Ctrl+Shift+/" },
    Command { id: CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED, label: "Keyboard Shortcuts: Reset Selected", keybinding: "" },
    Command { id: CMD_KEYBOARD_SHORTCUTS_RESET_ALL, label: "Keyboard Shortcuts: Reset All", keybinding: "" },
    Command { id: CMD_KEYBOARD_SHORTCUTS_CLOSE, label: "Help: Close Keyboard Shortcuts", keybinding: "" },
    Command { id: CMD_FOLD_TOGGLE,      label: "Fold: Toggle at Cursor",  keybinding: "Ctrl+Shift+[" },
    Command { id: CMD_FOLD_ALL,         label: "Fold: Fold All",          keybinding: "" },
    Command { id: CMD_UNFOLD_ALL,       label: "Fold: Unfold All",        keybinding: "" },
    Command { id: CMD_NEW_PROJECT,      label: "Mighty: New Project...",     keybinding: "" },
];

/// Match quality for ranking. Lower sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// Label starts with the query (case-insensitive).
    Prefix = 0,
    /// Query is a contiguous substring of the label.
    Substring = 1,
    /// Query chars appear in order (subsequence / fuzzy).
    Fuzzy = 2,
}

/// Score `label` against the lowercased `query`. Returns `None` if it doesn't
/// match at all, else the rank (for sorting). An empty query matches everything
/// at [`Rank::Prefix`] (so registry order is preserved).
fn score(label: &str, query_lc: &str) -> Option<Rank> {
    if query_lc.is_empty() {
        return Some(Rank::Prefix);
    }
    let label_lc = label.to_ascii_lowercase();
    score_exact(&label_lc, query_lc).or_else(|| {
        let compact = collapse_repeated_chars(query_lc);
        (compact != query_lc).then(|| score_exact(&label_lc, &compact)).flatten()
    })
}

fn score_exact(label_lc: &str, query_lc: &str) -> Option<Rank> {
    if label_lc.starts_with(query_lc) {
        return Some(Rank::Prefix);
    }
    if label_lc.contains(query_lc) {
        return Some(Rank::Substring);
    }
    // Subsequence test: every query char appears in order in the label.
    let mut q = query_lc.chars().peekable();
    for lc in label_lc.chars() {
        if let Some(&qc) = q.peek() {
            if lc == qc {
                q.next();
            }
        } else {
            break;
        }
    }
    if q.peek().is_none() {
        Some(Rank::Fuzzy)
    } else {
        None
    }
}

fn collapse_repeated_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        if Some(ch) != prev {
            out.push(ch);
        }
        prev = Some(ch);
    }
    out
}

fn normalized_shortcut_query(s: &str) -> String {
    s.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn score_keybinding(keybinding: &str, query_lc: &str) -> Option<Rank> {
    if keybinding.is_empty() || query_lc.is_empty() {
        return None;
    }
    let keybinding_lc = keybinding.to_ascii_lowercase();
    if let Some(rank) = score(&keybinding_lc, query_lc) {
        return Some(rank);
    }
    let query_key = normalized_shortcut_query(query_lc);
    if query_key.is_empty() || query_key == query_lc {
        return None;
    }
    keybinding
        .split('/')
        .filter_map(|part| {
            let part_key = normalized_shortcut_query(part);
            if part_key.is_empty() {
                None
            } else {
                score_exact(&part_key, &query_key)
            }
        })
        .min()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShortcutToken {
    Key(String),
    Separator,
}

fn keybinding_tokens(keybinding: &str) -> Vec<ShortcutToken> {
    let mut tokens = Vec::new();
    for (group_idx, group) in keybinding
        .split(" / ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        if group_idx > 0 {
            tokens.push(ShortcutToken::Separator);
        }
        for part in group.split('+').map(str::trim).filter(|s| !s.is_empty()) {
            tokens.push(ShortcutToken::Key(part.to_string()));
        }
    }
    tokens
}

fn shortcut_key_label_width(text: &mut crate::text::Text, label: &str, size: f32) -> f32 {
    text.measure_ui_sized(label, size).0
}

fn shortcut_token_width(text: &mut crate::text::Text, token: &ShortcutToken, size: f32) -> f32 {
    match token {
        ShortcutToken::Key(part) => (shortcut_key_label_width(text, part, size) + 14.0).max(22.0),
        ShortcutToken::Separator => 8.0,
    }
}

pub(crate) fn fit_palette_text(
    text: &mut crate::text::Text,
    s: &str,
    max_px: f32,
    size: f32,
) -> String {
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
        let candidate: String = chars[..mid]
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
            .collect();
        if text.measure_ui_sized(&candidate, size).0 <= max_px {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        ellipsis.to_string()
    } else {
        chars[..lo]
            .iter()
            .copied()
            .chain(std::iter::once('\u{2026}'))
            .collect()
    }
}

/// Filter + rank `commands` against `query`. Returns the matching commands in
/// rank order (prefix, then substring, then fuzzy), ties broken by original
/// registry index so the order is deterministic. Pure + unit-tested.
pub fn filter_commands(commands: &[Command], query: &str) -> Vec<Command> {
    let query_lc = query.to_ascii_lowercase();
    let mut scored: Vec<(Rank, u8, usize, Command)> = commands
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let label = score(c.label, &query_lc).map(|r| (r, 0_u8));
            let keybinding = score_keybinding(c.keybinding, &query_lc).map(|r| (r, 1_u8));
            label
                .into_iter()
                .chain(keybinding)
                .min()
                .map(|(rank, source)| (rank, source, i, *c))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, _, _, c)| c).collect()
}

/// Max rows drawn in the palette at once (the visible window).
const VISIBLE: usize = 12;

/// Shim-owned palette state: the typed query, the filtered command list, and
/// the selection. Mirrors [`crate::completion::CompletionEngine`].
#[derive(Debug, Default)]
pub struct PaletteEngine {
    /// `true` while the palette overlay is open.
    active: bool,
    /// The typed query (lowercased matching happens in [`score`]).
    query: String,
    /// The filtered command list for the current query (in rank order).
    filtered: Vec<Command>,
    /// Selected index into `filtered` (0-based).
    sel: usize,
}

impl PaletteEngine {
    pub fn new() -> Self {
        PaletteEngine::default()
    }

    /// Open the palette: clear the query, list all commands, select the first.
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.sel = 0;
        self.refilter();
    }

    /// Recompute the filtered list for the current query, clamping the selection.
    fn refilter(&mut self) {
        self.filtered = filter_commands(COMMANDS, &self.query);
        if self.sel >= self.filtered.len() {
            self.sel = self.filtered.len().saturating_sub(1);
        }
    }

    /// Append a typed char to the query and refilter (selection resets to top).
    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.sel = 0;
        self.refilter();
    }

    /// Delete the last query char and refilter (selection resets to top).
    pub fn backspace(&mut self) {
        self.query.pop();
        self.sel = 0;
        self.refilter();
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn count(&self) -> usize {
        self.filtered.len()
    }

    pub fn selection(&self) -> usize {
        self.sel
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx < self.filtered.len() {
            self.sel = idx;
            true
        } else {
            false
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Move the selection by `delta` (positive = down), wrapping around.
    pub fn move_sel(&mut self, delta: i32) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let n_i = n as i32;
        let mut s = self.sel as i32 + delta;
        s %= n_i;
        if s < 0 {
            s += n_i;
        }
        self.sel = s as usize;
    }

    /// The command id of the current selection, or `-1` when there is no match.
    pub fn selected_id(&self) -> i32 {
        self.filtered
            .get(self.sel)
            .map(|c| c.id as i32)
            .unwrap_or(-1)
    }

    /// Close the palette and clear its state.
    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
        self.filtered.clear();
        self.sel = 0;
    }

    /// First visible row index so the selected item stays within the window.
    pub fn scroll_top(&self) -> usize {
        if self.filtered.len() <= VISIBLE {
            return 0;
        }
        if self.sel < VISIBLE {
            0
        } else {
            (self.sel + 1).saturating_sub(VISIBLE)
        }
    }

    fn geometry(&self, width: u32, height: u32) -> (f32, f32, f32, f32, f32, usize) {
        let w = width as f32;
        let h = height as f32;
        let top = self.scroll_top();
        let shown = self.filtered.len().saturating_sub(top).min(VISIBLE).min(6);
        let box_w = command_palette_width(w);
        let search_h = 56.0;
        let cat_h = 25.0;
        let row_h = 50.0;
        let foot_h = 37.0;
        let box_h = search_h + cat_h + shown as f32 * row_h + 10.0 + foot_h;
        let box_x = ((w - box_w) * 0.5).max(0.0);
        let box_y = 96.0_f32.min((h - box_h).max(0.0));
        let list_top = box_y + search_h + cat_h;
        (box_x, box_w, list_top, row_h, box_h, shown)
    }

    /// Select the visible row under a click. Returns the selected filtered index,
    /// or -1 for a click outside the visible result rows.
    pub fn click_row(&mut self, x: f32, y: f32, width: u32, height: u32) -> i32 {
        if !self.active {
            return -1;
        }
        let (box_x, box_w, list_top, row_h, _box_h, shown) = self.geometry(width, height);
        if x < box_x || x > box_x + box_w || y < list_top {
            return -1;
        }
        let vis = ((y - list_top) / row_h).floor() as usize;
        if vis >= shown {
            return -1;
        }
        let idx = self.scroll_top() + vis;
        if self.select(idx) {
            idx as i32
        } else {
            -1
        }
    }

    /// A vector icon + a short description for a command id, matching the
    /// mockup's rich palette rows.
    fn meta(id: u32) -> (&'static str, &'static str, bool) {
        use crate::icons;
        // (icon path, description, fill?)
        match id {
            CMD_NEW_FILE => (icons::NEW_FILE, "Choose a folder and filename before creating the tab", false),
            CMD_NEW_UNTITLED_FILE => (icons::NEW_FILE, "Start a temporary editor tab with no disk path", false),
            CMD_NEW_WORKSPACE_FILE => (icons::NEW_FILE, "Use the native picker, starting near the workspace", false),
            CMD_NEW_FOLDER => (icons::NEW_FOLDER, "Choose or create a folder with the native picker", false),
            CMD_OPEN_FILE => (icons::NEW_FILE, "Choose an existing file with the native picker", false),
            CMD_SAVE => (icons::FILE_MTY, "Write the active file to disk", false),
            CMD_SAVE_AS => (icons::FILE_MTY, "Save the active file with the native Save As dialog", false),
            CMD_SAVE_ALL => (icons::FILE_MTY, "Write dirty tabs and ask where untitled files should live", false),
            CMD_RENAME_ACTIVE_FILE => (icons::FILE_MTY, "Rename the active file on disk", false),
            CMD_REVEAL_ACTIVE_FILE => (icons::SEARCH, "Show the active file in the IDE file tree", false),
            CMD_EXPLORER_REFRESH => (icons::REFRESH, "Refresh the Explorer tree and file index", false),
            CMD_EXPLORER_COLLAPSE_ALL => (icons::COLLAPSE, "Collapse all expanded Explorer folders", false),
            CMD_EXPLORER_CLOSE => (icons::CLOSE, "Close the Explorer panel without clearing or collapsing the file tree", false),
            CMD_REVEAL_ACTIVE_FILE_IN_OS => (icons::EXPLORER, "Show the active file in the OS file manager", false),
            CMD_COPY_ACTIVE_FILE_PATH => (icons::FILE_MTY, "Copy the active file path to the clipboard", false),
            CMD_COPY_ACTIVE_FILE_RELATIVE_PATH => (icons::FILE_MTY, "Copy the workspace-relative path", false),
            CMD_COPY_ACTIVE_FILE_NAME => (icons::FILE_MTY, "Copy just the active file name", false),
            CMD_COPY_ACTIVE_FILE_DIRECTORY => (icons::FOLDER, "Copy the active file's containing folder", false),
            CMD_DELETE_ACTIVE_FILE => (icons::ERROR_CIRCLE, "Delete the active file after confirmation", false),
            CMD_CLEAR_NOTIFICATIONS => (icons::CLOSE, "Dismiss every visible toast notification", false),
            CMD_QUICK_OPEN => (icons::SEARCH, "Open files, commands, symbols, or line jumps", false),
            CMD_FIND => (icons::SEARCH, "Search within the current document", false),
            CMD_FIND_REPLACE => (icons::SEARCH, "Search and replace within the current document", false),
            CMD_FIND_REPLACE_CLOSE => (icons::CLOSE, "Close the in-file Find & Replace bar", false),
            CMD_GOTO_LINE => (icons::CHEVRON, "Jump to a specific line number", false),
            CMD_GOTO_DEFINITION => (icons::FN_SYMBOL, "Navigate to the symbol definition", false),
            CMD_HOVER => (icons::INFO_I, "Show type & docs at the cursor", false),
            CMD_HOVER_CLOSE => (icons::CLOSE, "Close the hover popup without moving the cursor", false),
            CMD_SIGNATURE_HELP => (icons::INFO_I, "Show callable parameters at the cursor", false),
            CMD_SIGNATURE_HELP_CLOSE => (icons::CLOSE, "Close the signature-help popup without moving the cursor", false),
            CMD_RENAME_SYMBOL => (icons::FN_SYMBOL, "Rename the symbol under the cursor", false),
            CMD_RENAME_CANCEL => (icons::CLOSE, "Cancel the active inline rename", false),
            CMD_CODE_ACTIONS => (icons::LIGHTBULB, "Show quick fixes and code actions at the cursor", false),
            CMD_CODE_ACTIONS_CLOSE => (icons::CLOSE, "Close the Code Actions menu without applying an action", false),
            CMD_PROMPT_CANCEL => (icons::CLOSE, "Close the active bottom prompt without applying input", false),
            CMD_TOGGLE_TERMINAL => (icons::TEST_BOX, "Open the integrated terminal or focus it if already open", false),
            CMD_TOGGLE_SIDEBAR => (icons::EXPLORER, "Show or hide the left sidebar", false),
            CMD_NEXT_TAB => (icons::CHEVRON, "Switch to the next open tab", false),
            CMD_PREV_TAB => (icons::CHEVRON, "Switch to the previous open tab", false),
            CMD_CLOSE_TAB => (icons::CLOSE, "Close the active editor tab", false),
            CMD_CLOSE_SAVED_TABS => (icons::CLOSE, "Close clean tabs while preserving unsaved work", false),
            CMD_CLOSE_OTHER_SAVED_TABS => (icons::CLOSE, "Close other clean tabs while preserving unsaved work", false),
            CMD_CLOSE_SAVED_TABS_TO_RIGHT => (icons::CLOSE, "Close clean tabs to the right and keep dirty tabs", false),
            CMD_CLOSE_SAVED_TABS_TO_LEFT => (icons::CLOSE, "Close clean tabs to the left and keep dirty tabs", false),
            CMD_REOPEN_CLOSED_TAB => (icons::PLUS, "Restore the last closed editor tab", false),
            CMD_DUPLICATE_ACTIVE_TAB => (icons::PLUS, "Clone the active editor tab next to itself", false),
            CMD_MOVE_ACTIVE_TAB_LEFT => (icons::CHEVRON, "Move the active tab one slot left", false),
            CMD_MOVE_ACTIVE_TAB_RIGHT => (icons::CHEVRON, "Move the active tab one slot right", false),
            CMD_SORT_TABS_BY_NAME => (icons::CHEVRON, "Sort open tabs alphabetically by name", false),
            CMD_CLOSE_DUPLICATE_TABS => (icons::CLOSE, "Close clean duplicate file tabs", false),
            CMD_RELOAD_ACTIVE_FILE => (icons::REFRESH, "Reload the active file from disk", false),
            CMD_REVERT_ACTIVE_FILE => (icons::REFRESH, "Discard local edits and reload from disk", false),
            CMD_SELECT_ALL => (icons::FN_SYMBOL, "Select the entire active document", false),
            CMD_SELECT_LINE => (icons::FN_SYMBOL, "Select the current editor line", false),
            CMD_SELECT_WORD => (icons::FN_SYMBOL, "Select the word at the cursor", false),
            CMD_TOGGLE_LINE_COMMENT => (icons::FN_SYMBOL, "Comment or uncomment the active line or selection", false),
            CMD_COPY_SELECTION_OR_LINE => (icons::FILE_MTY, "Copy the selection or current line to the clipboard", false),
            CMD_CUT_SELECTION_OR_LINE => (icons::CLOSE, "Cut the selection or current line to the clipboard", false),
            CMD_PASTE_IN_EDITOR => (icons::PLUS, "Paste clipboard text into the editor", false),
            CMD_DELETE_PREVIOUS_WORD => (icons::CLOSE, "Delete text back to the previous word boundary", false),
            CMD_DELETE_NEXT_WORD => (icons::CLOSE, "Delete text forward to the next word boundary", false),
            CMD_INDENT_LINE_SELECTION => (icons::ARROW_RIGHT, "Indent the active line or selected line range", false),
            CMD_OUTDENT_LINE_SELECTION => (icons::ARROW_LEFT, "Outdent the active line or selected line range", false),
            CMD_MOVE_WORD_LEFT => (icons::ARROW_LEFT, "Move the cursor to the previous word boundary", false),
            CMD_MOVE_WORD_RIGHT => (icons::ARROW_RIGHT, "Move the cursor to the next word boundary", false),
            CMD_MOVE_DOCUMENT_START => (icons::ARROW_UP, "Move the cursor to the start of the document", false),
            CMD_MOVE_DOCUMENT_END => (icons::ARROW_DOWN, "Move the cursor to the end of the document", false),
            CMD_MOVE_LINE_START => (icons::ARROW_LEFT, "Move the cursor to the smart start of the line", false),
            CMD_MOVE_LINE_END => (icons::ARROW_RIGHT, "Move the cursor to the end of the line", false),
            CMD_ADD_CARET_NEXT_OCCURRENCE => (icons::PLUS, "Add a cursor at the next matching occurrence", false),
            CMD_ADD_CARET_ABOVE => (icons::ARROW_UP, "Add another cursor on the line above", false),
            CMD_ADD_CARET_BELOW => (icons::ARROW_DOWN, "Add another cursor on the line below", false),
            CMD_COLLAPSE_CARETS => (icons::CLOSE, "Return to a single primary cursor", false),
            CMD_DUPLICATE_LINE_SELECTION => (icons::PLUS, "Duplicate the active line or selection", false),
            CMD_MOVE_LINE_UP => (icons::ARROW_UP, "Move the active line or selection upward", false),
            CMD_MOVE_LINE_DOWN => (icons::ARROW_DOWN, "Move the active line or selection downward", false),
            CMD_DELETE_LINE => (icons::CLOSE, "Remove the current line without changing the clipboard", false),
            CMD_JOIN_LINE => (icons::CHEVRON, "Join the current line with the next line", false),
            CMD_FORMAT_DOCUMENT => (icons::PLUS, "Apply mightyfmt to active file", false),
            CMD_UNDO => (icons::CHEVRON, "Undo the last edit", false),
            CMD_REDO => (icons::CHEVRON, "Redo the last undone edit", false),
            CMD_AUTOCOMPLETE => (icons::AGENTS, "Suggest completions at the cursor", false),
            CMD_AUTOCOMPLETE_CLOSE => (icons::CLOSE, "Close autocomplete suggestions without accepting one", false),
            CMD_DIRTY_CONFIRM_CANCEL => (icons::CLOSE, "Cancel the unsaved-work confirmation without saving or discarding", false),
            CMD_GIT_BRANCH_CANCEL => (icons::CLOSE, "Close the Git branch switcher without checking out or creating a branch", false),
            CMD_BREADCRUMB_MENU_CANCEL => (icons::CLOSE, "Close the breadcrumb dropdown without opening a file or jumping to a symbol", false),
            CMD_COMMAND_PALETTE_CLOSE => (icons::CLOSE, "Close the command palette without executing the highlighted command", false),
            CMD_QUICK_OPEN_CLOSE => (icons::CLOSE, "Close Quick Open without opening a file, command, symbol, or line jump", false),
            CMD_WELCOME_CLOSE => (icons::CLOSE, "Close the forced Welcome or Open Recent surface without opening anything", false),
            CMD_SNIPPET_CANCEL => (icons::CLOSE, "Cancel the active snippet tab-stop session without removing expanded text", false),
            CMD_JUMP_BACK => (icons::CHEVRON, "Return to the previous location", false),
            CMD_QUIT => (icons::CLOSE, "Close the editor", false),
            CMD_COLOR_THEME => (icons::SETTINGS, "Switch the editor color theme", false),
            CMD_COLOR_THEME_CLOSE => (icons::CLOSE, "Close the color theme picker and revert preview", false),
            CMD_RUN_FILE => (icons::RUN, "Run the active Mighty file", true),
            CMD_RUN_STOP => (icons::CLOSE, "Stop the active Run output process", false),
            CMD_RUN_CLEAR_OUTPUT => (icons::CLOSE, "Clear the Run output without stopping the process", false),
            CMD_RUN_CLOSE => (icons::CLOSE, "Close the Run panel without stopping a running process", false),
            CMD_SETTINGS => (icons::SETTINGS, "Edit editor preferences", false),
            CMD_SETTINGS_CLOSE => (icons::CLOSE, "Close the Settings panel", false),
            CMD_ZOOM_IN => (icons::PLUS, "Increase the IDE UI scale", false),
            CMD_ZOOM_OUT => (icons::UNSTAGE_MINUS, "Decrease the IDE UI scale", false),
            CMD_ZOOM_RESET => (icons::WIN_MAX, "Reset the IDE UI scale to 100%", false),
            CMD_RUN_TESTS => (icons::BEAKER, "Run the package's tests (mty test)", false),
            CMD_RUN_TEST_AT_CURSOR => (icons::BEAKER, "Run tests and focus the nearest test at the cursor", false),
            CMD_TEST_STOP => (icons::CLOSE, "Stop the active test run", false),
            CMD_TEST_CLEAR_RESULTS => (icons::CLOSE, "Clear parsed test results without stopping the run", false),
            CMD_TEST_CLOSE => (icons::CLOSE, "Close the Testing panel without stopping a test run", false),
            CMD_PEEK_DEFINITION => (icons::FN_SYMBOL, "Preview the definition inline (Alt+F12)", false),
            CMD_PEEK_CLOSE => (icons::CLOSE, "Close the inline Peek Definition view", false),
            CMD_WELCOME => (icons::LANG_M, "Open the Welcome screen", false),
            CMD_ZEN_MODE => (icons::INFO_I, "Toggle distraction-free focus mode", false),
            CMD_AGENTS => (icons::AGENTS_NET, "Open the Mighty Agents topology panel", false),
            CMD_AGENTS_REFRESH => (icons::REFRESH, "Refresh the Mighty Agents topology model", false),
            CMD_AGENTS_CLEAR_RUN_OUTPUT => (icons::CLOSE, "Clear the Mighty Agents run output without rebuilding topology", false),
            CMD_AGENTS_CLOSE => (icons::CLOSE, "Close the Mighty Agents panel without clearing topology or run output", false),
            CMD_GIT_SWITCH_BRANCH => (icons::BRANCH, "Checkout or create a git branch", false),
            CMD_GIT_PUSH => (icons::GIT, "Push commits to the remote", false),
            CMD_GIT_PULL => (icons::GIT, "Pull (fast-forward only) from the remote", false),
            CMD_GIT_FETCH => (icons::GIT, "Fetch refs from the remote", false),
            CMD_GIT_TOGGLE_BLAME => (icons::GIT, "Show or hide git blame in the gutter", false),
            CMD_GIT_HIDE_BLAME => (icons::CLOSE, "Hide the active git blame gutter", false),
            CMD_GIT_STAGE_ALL => (icons::STAGE_PLUS, "Stage every changed path", false),
            CMD_GIT_UNSTAGE_ALL => (icons::UNSTAGE_MINUS, "Unstage every staged path", false),
            CMD_GIT_COMMIT_STAGED => (icons::GIT, "Commit staged changes with the SCM message", false),
            CMD_GIT_CLEAR_COMMIT_MESSAGE => (icons::CLOSE, "Clear the Source Control commit-message draft", false),
            CMD_GIT_REFRESH_SOURCE_CONTROL => (icons::REFRESH, "Refresh the Source Control git status", false),
            CMD_GIT_CLOSE_SOURCE_CONTROL => (icons::CLOSE, "Close the Source Control panel without clearing git status or message state", false),
            CMD_VIEW_EXPLORER => (icons::EXPLORER, "Open the file explorer view", false),
            CMD_VIEW_SEARCH => (icons::SEARCH, "Open project-wide search", false),
            CMD_SEARCH_RUN => (icons::SEARCH, "Run the current project-wide search query", false),
            CMD_SEARCH_CLEAR_RESULTS => (icons::CLOSE, "Clear Search results without changing query or replace text", false),
            CMD_SEARCH_REPLACE_ALL => (icons::REPLACE, "Replace every current project-wide search match", false),
            CMD_SEARCH_TOGGLE_REPLACE => (icons::REPLACE, "Open Search and move focus between query and replace", false),
            CMD_SEARCH_CLOSE => (icons::CLOSE, "Close the Search panel without clearing query or results", false),
            CMD_VIEW_SOURCE_CONTROL => (icons::GIT, "Open source control", false),
            CMD_VIEW_OUTLINE => (icons::FN_SYMBOL, "Open the symbol outline", false),
            CMD_OUTLINE_REFRESH => (icons::REFRESH, "Refresh the active document's Outline symbols", false),
            CMD_OUTLINE_CLEAR_SYMBOLS => (icons::CLOSE, "Clear Outline symbols without closing the panel", false),
            CMD_OUTLINE_CLOSE => (icons::CLOSE, "Close the Outline panel without clearing document symbols", false),
            CMD_VIEW_RUN_DEBUG => (icons::DEBUG, "Open Run and Debug", false),
            CMD_VIEW_TESTING => (icons::BEAKER, "Open the testing view", false),
            CMD_VIEW_RUN_OUTPUT => (icons::RUN, "Open the Run output panel", false),
            CMD_VIEW_PROBLEMS => (icons::ERROR_CIRCLE, "Open diagnostics and build problems", false),
            CMD_PROBLEMS_REFRESH => (icons::REFRESH, "Refresh diagnostics and show Problems", false),
            CMD_PROBLEMS_CLEAR => (icons::CLOSE, "Clear Problems diagnostics without closing the panel", false),
            CMD_PROBLEMS_CLOSE => (icons::CLOSE, "Close the Problems panel without affecting other bottom-dock tools", false),
            CMD_VIEW_AI_COPILOT => (icons::AGENTS, "Open the AI copilot panel", false),
            CMD_INLINE_AI_ASK => (icons::AGENTS, "Ask AI about the active selection or file", false),
            CMD_FORCE_GHOST_COMPLETION => (icons::AGENTS, "Request an inline AI ghost completion now", false),
            CMD_GHOST_COMPLETION_DISMISS => (icons::CLOSE, "Dismiss the visible inline AI ghost completion without accepting text", false),
            CMD_AI_CLEAR_CHAT => (icons::CLOSE, "Clear the AI transcript and draft composer", false),
            CMD_AI_CLOSE => (icons::CLOSE, "Close the AI copilot panel", false),
            CMD_SIDEBAR_CLOSE => (icons::CLOSE, "Close the left sidebar drawer", false),
            CMD_VIEW_TERMINAL => (icons::TEST_BOX, "Open the integrated terminal", false),
            CMD_TERMINAL_CLEAR => (icons::CLOSE, "Clear the integrated terminal buffer without closing the shell", false),
            CMD_TERMINAL_CLOSE => (icons::CLOSE, "Close the integrated terminal without changing other bottom-dock panels", false),
            CMD_VIEW_WEB_PLAYGROUND => (icons::GLOBE, "Open the Web Playground output panel", false),
            CMD_DIFF_CLOSE_VIEW => (icons::CLOSE, "Close the inline git diff view and return to editing", false),
            CMD_DOCK_COMPACT => (icons::ARROW_DOWN, "Open the shared bottom dock at compact height", false),
            CMD_DOCK_RESET => (icons::WIN_MIN, "Open the shared bottom dock at its default height", false),
            CMD_DOCK_EXPANDED => (icons::ARROW_UP, "Open the shared bottom dock at expanded height", false),
            CMD_DOCK_CLOSE => (icons::CLOSE, "Close the active shared bottom dock", false),
            CMD_SIDEBAR_COMPACT => (icons::ARROW_LEFT, "Open the sidebar at compact width", false),
            CMD_SIDEBAR_DEFAULT => (icons::EXPLORER, "Open the sidebar at its default responsive width", false),
            CMD_SIDEBAR_WIDE => (icons::ARROW_RIGHT, "Open the sidebar at wide width", false),
            CMD_SIDEBAR_CYCLE_WIDTH => (icons::EXPLORER, "Open the sidebar and cycle compact, default, and wide width", false),
            CMD_WINDOW_TOGGLE_MAXIMIZE => (icons::WIN_MAX, "Maximize or restore the IDE window", false),
            CMD_WINDOW_MINIMIZE => (icons::WIN_MIN, "Minimize the IDE window", false),
            CMD_DEBUG_START_CONTINUE => (icons::DBG_CONTINUE, "Start debugging or continue the paused session", true),
            CMD_DEBUG_STOP => (icons::DBG_STOP, "Stop the active debug session", false),
            CMD_DEBUG_STEP_OVER => (icons::DBG_STEP_OVER, "Run the next line without entering calls", false),
            CMD_DEBUG_STEP_INTO => (icons::DBG_STEP_INTO, "Enter the next function call", false),
            CMD_DEBUG_STEP_OUT => (icons::DBG_STEP_OUT, "Run until the current frame returns", false),
            CMD_DEBUG_PAUSE => (icons::DBG_PAUSE, "Pause the running debuggee", true),
            CMD_DEBUG_RESTART => (icons::REFRESH, "Restart the last debug target", false),
            CMD_DEBUG_TOGGLE_BREAKPOINT => (icons::BREAKPOINT, "Set or clear a breakpoint on the cursor line", true),
            CMD_DEBUG_CLEAR_BREAKPOINTS => (icons::CLOSE, "Remove every stored debug breakpoint", false),
            CMD_DEBUG_CLEAR_SESSION => (icons::CLOSE, "Clear debug session state without clearing breakpoints or target", false),
            CMD_DEBUG_CLOSE => (icons::CLOSE, "Close the Run and Debug panel without stopping or resetting the debug model", false),
            CMD_RUN_IN_BROWSER => (icons::GLOBE, "Build and serve the active Mighty file for the browser", false),
            CMD_WEB_STOP => (icons::CLOSE, "Stop the active Web Playground server", false),
            CMD_WEB_OPEN_BROWSER => (icons::GLOBE, "Open the active Web Playground URL in the default browser", false),
            CMD_WEB_CLEAR_OUTPUT => (icons::CLOSE, "Clear Web Playground output without stopping the server", false),
            CMD_WEB_CLOSE => (icons::CLOSE, "Close the Web Playground panel without stopping the server", false),
            CMD_SPLIT_RIGHT => (icons::TEST_BOX, "Split the editor into side-by-side panes", false),
            CMD_FOCUS_NEXT_PANE => (icons::CHEVRON, "Move focus between editor panes", false),
            CMD_CLOSE_PANE => (icons::CLOSE, "Close the focused editor pane", false),
            CMD_MARKDOWN_PREVIEW => (icons::FILE_MD, "Open the live Markdown preview pane", false),
            CMD_MARKDOWN_CLOSE_PREVIEW => (icons::CLOSE, "Close the live Markdown preview pane", false),
            CMD_OPEN_FOLDER => (icons::FOLDER, "Open a workspace folder with the native folder picker", false),
            CMD_OPEN_RECENT => (icons::FOLDER, "Open a recent file or workspace folder", false),
            CMD_KEYBOARD_SHORTCUTS => (icons::INFO_I, "List & remap all keyboard shortcuts", false),
            CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED => (icons::REFRESH, "Reset the selected shortcut override to its default", false),
            CMD_KEYBOARD_SHORTCUTS_RESET_ALL => (icons::REFRESH, "Reset every shortcut override to defaults", false),
            CMD_KEYBOARD_SHORTCUTS_CLOSE => (icons::CLOSE, "Close the Keyboard Shortcuts overlay", false),
            CMD_FOLD_TOGGLE => (icons::CHEVRON, "Fold or unfold the block at the cursor", false),
            CMD_FOLD_ALL => (icons::CHEVRON_DOWN, "Fold every foldable block in the document", false),
            CMD_UNFOLD_ALL => (icons::CHEVRON_DOWN, "Unfold every block in the document", false),
            CMD_NEW_PROJECT => (icons::NEW_FOLDER, "Scaffold a new Mighty project (mty new)", false),
            _ => (icons::CHEVRON, "", false),
        }
    }

    fn contextual_desc<'a>(&self, ctx: &crate::MuiContext, id: u32, base: &'a str) -> Cow<'a, str> {
        let model = ctx.tabs.active_model();
        let active_has_selection = model.has_selection();
        let active_can_copy =
            active_has_selection || !model.current_line_text_for_clipboard().is_empty();
        let active_has_path = ctx.tabs.active_has_path();
        let active_read_only = ctx.tabs.active_read_only();
        let workspace_test_target =
            id == CMD_RUN_TESTS
                && !active_has_path
                && crate::testabi::workspace_test_target_for_root(
                    &crate::wsabi::effective_root(ctx),
                )
                .is_some();
        if id == CMD_FORCE_GHOST_COMPLETION && !active_read_only {
            return force_ghost_contextual_desc(
                base,
                crate::settings::inline_ai(),
                crate::ai::api_key().is_some(),
                ctx.ghost.is_inflight(),
            );
        }
        if id == CMD_GHOST_COMPLETION_DISMISS {
            return dismiss_ghost_contextual_desc(base, ctx.ghost.has_ghost());
        }
        if id == CMD_AUTOCOMPLETE && active_has_path && !active_read_only {
            return autocomplete_contextual_desc(base, ctx.language);
        }
        if id == CMD_GIT_TOGGLE_BLAME {
            if !ctx.blame.is_active() {
                if let Some(path) = ctx.tabs.active_path() {
                    if let Some(desc) = blame_stale_target_desc(&path) {
                        return Cow::Owned(desc);
                    }
                }
            }
            return blame_toggle_contextual_desc(base, ctx.blame.is_active(), active_has_path);
        }
        if matches!(id, CMD_FOLD_TOGGLE | CMD_FOLD_ALL | CMD_UNFOLD_ALL) {
            let fold = ctx.tabs.active_fold();
            let cursor_line = model.cursor_line();
            return fold_contextual_desc(
                id,
                base,
                fold.enclosing_start(cursor_line).is_some(),
                fold.ranges().is_empty(),
                fold.ranges().iter().all(|r| fold.is_folded(r.start)),
                fold.ranges().iter().any(|r| fold.is_folded(r.start)),
            );
        }
        if matches!(
            id,
            CMD_TOGGLE_SIDEBAR
                | CMD_DOCK_COMPACT
                | CMD_DOCK_RESET
                | CMD_DOCK_EXPANDED
                | CMD_SIDEBAR_COMPACT
                | CMD_SIDEBAR_DEFAULT
                | CMD_SIDEBAR_WIDE
                | CMD_SIDEBAR_CYCLE_WIDTH
        ) {
            return layout_command_contextual_desc(
                id,
                base,
                ctx.sidebar_visible,
                crate::layout::sidebar_preset(),
                ctx.bottom_dock_open(),
                crate::layout::dock_preset_index(),
            );
        }
        if id == CMD_MARKDOWN_PREVIEW {
            return markdown_preview_contextual_desc(
                base,
                ctx.language,
                ctx.md_preview.is_open() || ctx.md_pane.is_some(),
            );
        }
        if matches!(
            id,
            CMD_TOGGLE_TERMINAL
                | CMD_COLOR_THEME
                | CMD_SETTINGS
                | CMD_KEYBOARD_SHORTCUTS
                | CMD_VIEW_EXPLORER
                | CMD_VIEW_SEARCH
                | CMD_VIEW_SOURCE_CONTROL
                | CMD_VIEW_OUTLINE
                | CMD_VIEW_RUN_DEBUG
                | CMD_VIEW_TESTING
                | CMD_VIEW_RUN_OUTPUT
                | CMD_VIEW_PROBLEMS
                | CMD_VIEW_AI_COPILOT
                | CMD_VIEW_TERMINAL
                | CMD_VIEW_WEB_PLAYGROUND
        ) {
            return open_surface_contextual_desc(
                id,
                base,
                ctx.sidebar_visible && ctx.active_panel == crate::PANEL_EXPLORER,
                ctx.sidebar_visible && ctx.active_panel == crate::PANEL_SEARCH,
                ctx.sidebar_visible && ctx.active_panel == crate::PANEL_SCM,
                ctx.sidebar_visible && ctx.active_panel == crate::PANEL_OUTLINE,
                ctx.active_panel == crate::PANEL_DEBUG || ctx.dbg.is_open(),
                ctx.tests_panel.is_active(),
                ctx.run.is_active(),
                ctx.problems.is_open(),
                ctx.ai.open,
                ctx.term_open,
                ctx.web.is_active(),
                ctx.settings_panel.is_active(),
                ctx.theme_picker.is_active(),
                ctx.shortcuts.is_active(),
                ctx.md_preview.is_open() || ctx.md_pane.is_some(),
            );
        }
        if matches!(
            id,
            CMD_WEB_STOP | CMD_WEB_OPEN_BROWSER | CMD_WEB_CLEAR_OUTPUT | CMD_WEB_CLOSE
        ) {
            return web_contextual_desc(
                id,
                base,
                ctx.web.is_active(),
                ctx.web.is_running(),
                ctx.web.url().is_empty(),
                ctx.web.line_count(),
            );
        }
        if matches!(id, CMD_GIT_HIDE_BLAME | CMD_WELCOME_CLOSE) {
            let welcome_visible = ctx.welcome.force_open
                || (!active_has_path
                    && model.line_count() <= 1
                    && model.line_len(0) == 0
                    && !ctx.welcome.hides_empty_auto());
            return close_action_contextual_desc(
                id,
                base,
                ctx.blame.is_active(),
                welcome_visible,
            );
        }
        if matches!(
            id,
            CMD_REVEAL_ACTIVE_FILE
                | CMD_REVEAL_ACTIVE_FILE_IN_OS
                | CMD_COPY_ACTIVE_FILE_PATH
                | CMD_COPY_ACTIVE_FILE_RELATIVE_PATH
                | CMD_COPY_ACTIVE_FILE_NAME
                | CMD_COPY_ACTIVE_FILE_DIRECTORY
        ) {
            if let Some(path) = ctx.tabs.active_path() {
                if let Some(desc) = active_file_utility_stale_target_desc(id, &path) {
                    return Cow::Owned(desc);
                }
            }
        }
        if matches!(id, CMD_RENAME_ACTIVE_FILE | CMD_DELETE_ACTIVE_FILE)
            && !ctx.tabs.active_read_only()
        {
            if let Some(path) = ctx.tabs.active_path() {
                if id == CMD_DELETE_ACTIVE_FILE && ctx.tabs.any_dirty_path(&path) {
                    return Cow::Owned(format!(
                        "Save or discard changes in {} before deleting",
                        palette_basename(&path)
                    ));
                }
                if let Some(desc) = active_file_edit_stale_target_desc(id, &path) {
                    return Cow::Owned(desc);
                }
            }
        }
        if matches!(id, CMD_RELOAD_ACTIVE_FILE | CMD_REVERT_ACTIVE_FILE)
            && !(id == CMD_RELOAD_ACTIVE_FILE && ctx.tabs.is_dirty(ctx.tabs.active()))
        {
            if let Some(path) = ctx.tabs.active_path() {
                if let Some(desc) = reload_revert_stale_target_desc(id, &path) {
                    return Cow::Owned(desc);
                }
            }
        }
        if matches!(id, CMD_RUN_STOP | CMD_RUN_CLEAR_OUTPUT | CMD_RUN_CLOSE) {
            return run_contextual_desc(
                id,
                base,
                ctx.run.is_active(),
                ctx.run.is_running(),
                ctx.run.line_count(),
            );
        }
        if matches!(
            id,
            CMD_TEST_STOP | CMD_TEST_CLEAR_RESULTS | CMD_TEST_CLOSE
        ) {
            return test_contextual_desc(
                id,
                base,
                ctx.tests_panel.is_active(),
                ctx.tests_panel.is_running(),
                ctx.tests_panel.row_count(),
            );
        }
        if matches!(
            id,
            CMD_AGENTS | CMD_AGENTS_REFRESH | CMD_AGENTS_CLEAR_RUN_OUTPUT | CMD_AGENTS_CLOSE
        ) {
            return agents_contextual_desc(
                id,
                base,
                ctx.active_panel == crate::PANEL_AGENTS_MTY,
                ctx.agents.run_line_count(),
            );
        }
        if matches!(id, CMD_TERMINAL_CLEAR | CMD_TERMINAL_CLOSE) {
            return terminal_contextual_desc(
                id,
                base,
                ctx.term_open,
                ctx.terminal.is_some(),
                ctx.terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.has_visible_content()),
            );
        }
        if matches!(
            id,
            CMD_PROBLEMS_REFRESH | CMD_PROBLEMS_CLEAR | CMD_PROBLEMS_CLOSE
        ) {
            return problems_contextual_desc(
                id,
                base,
                ctx.problems.is_open(),
                ctx.problems.count(),
                active_has_path,
                ctx.language,
            );
        }
        if matches!(
            id,
            CMD_SEARCH_RUN
                | CMD_SEARCH_REPLACE_ALL
                | CMD_SEARCH_CLEAR_RESULTS
                | CMD_SEARCH_TOGGLE_REPLACE
                | CMD_SEARCH_CLOSE
        ) {
            let search_query = ctx.search.query_string();
            return search_contextual_desc(
                id,
                base,
                ctx.active_panel == crate::PANEL_SEARCH,
                ctx.search.replace_focus,
                search_query.trim().is_empty(),
                ctx.search.last_results_query == search_query,
                ctx.search.match_count(),
            );
        }
        if matches!(
            id,
            CMD_OUTLINE_REFRESH | CMD_OUTLINE_CLEAR_SYMBOLS | CMD_OUTLINE_CLOSE
        ) {
            return outline_contextual_desc(
                id,
                base,
                ctx.active_panel == crate::PANEL_OUTLINE,
                ctx.outline.count(),
            );
        }
        if matches!(
            id,
            CMD_DEBUG_STOP
                | CMD_DEBUG_PAUSE
                | CMD_DEBUG_RESTART
                | CMD_DEBUG_STEP_OVER
                | CMD_DEBUG_STEP_INTO
                | CMD_DEBUG_STEP_OUT
                | CMD_DEBUG_CLEAR_BREAKPOINTS
                | CMD_DEBUG_CLEAR_SESSION
                | CMD_DEBUG_CLOSE
        ) {
            return debug_contextual_desc(
                id,
                base,
                ctx.active_panel == crate::PANEL_DEBUG || ctx.dbg.is_open(),
                ctx.dbg.state(),
                ctx.dbg.has_program(),
                ctx.dbg.total_breakpoint_count(),
                ctx.dbg.session_is_empty(),
            );
        }
        if matches!(
            id,
            CMD_GIT_SWITCH_BRANCH
                | CMD_GIT_PUSH
                | CMD_GIT_PULL
                | CMD_GIT_FETCH
                | CMD_GIT_STAGE_ALL
                | CMD_GIT_UNSTAGE_ALL
                | CMD_GIT_COMMIT_STAGED
                | CMD_GIT_CLEAR_COMMIT_MESSAGE
                | CMD_GIT_REFRESH_SOURCE_CONTROL
                | CMD_GIT_CLOSE_SOURCE_CONTROL
        ) {
            return source_control_contextual_desc(
                id,
                base,
                ctx.active_panel == crate::PANEL_SCM,
                ctx.scm.root.is_some(),
                ctx.scm.status.staged_count(),
                ctx.scm.status.unstaged_count(),
                ctx.scm.message.is_empty(),
            );
        }
        if matches!(id, CMD_EXPLORER_REFRESH | CMD_EXPLORER_CLOSE) {
            return explorer_contextual_desc(
                id,
                base,
                ctx.sidebar_visible && ctx.active_panel == crate::PANEL_EXPLORER,
            );
        }
        if matches!(
            id,
            CMD_KEYBOARD_SHORTCUTS_CLOSE
                | CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED
                | CMD_KEYBOARD_SHORTCUTS_RESET_ALL
        ) {
            return keyboard_shortcuts_contextual_desc(
                id,
                base,
                ctx.shortcuts.is_active(),
                ctx.shortcuts.reset_selected_would_clear_override(),
                ctx.shortcuts.overrides().is_empty(),
            );
        }
        if matches!(
            id,
            CMD_SETTINGS_CLOSE
                | CMD_DIFF_CLOSE_VIEW
                | CMD_PEEK_CLOSE
                | CMD_MARKDOWN_CLOSE_PREVIEW
        ) {
            return close_surface_contextual_desc(
                id,
                base,
                ctx.settings_panel.is_active(),
                ctx.diff.is_active(),
                ctx.peek.is_active(),
                ctx.md_preview.is_open() || ctx.md_pane.is_some(),
            );
        }
        if matches!(
            id,
            CMD_AI_CLOSE
                | CMD_SIDEBAR_CLOSE
                | CMD_COLOR_THEME_CLOSE
                | CMD_HOVER_CLOSE
                | CMD_SIGNATURE_HELP_CLOSE
                | CMD_CODE_ACTIONS_CLOSE
                | CMD_FIND_REPLACE_CLOSE
                | CMD_AUTOCOMPLETE_CLOSE
                | CMD_COMMAND_PALETTE_CLOSE
                | CMD_QUICK_OPEN_CLOSE
        ) {
            return transient_surface_contextual_desc(
                id,
                base,
                ctx.ai.open,
                ctx.sidebar_visible,
                ctx.theme_picker.is_active(),
                ctx.hover.is_active(),
                ctx.sig.is_active(),
                ctx.codeaction.is_active(),
                ctx.replace_bar.is_active(),
                ctx.complete.is_active(),
                ctx.palette.is_active(),
                ctx.quickopen.is_active(),
            );
        }
        if matches!(
            id,
            CMD_CLEAR_NOTIFICATIONS
                | CMD_REOPEN_CLOSED_TAB
                | CMD_DOCK_CLOSE
                | CMD_PROMPT_CANCEL
                | CMD_DIRTY_CONFIRM_CANCEL
                | CMD_GIT_BRANCH_CANCEL
                | CMD_BREADCRUMB_MENU_CANCEL
                | CMD_SNIPPET_CANCEL
        ) {
            return utility_command_contextual_desc(
                id,
                base,
                !ctx.toasts.is_empty(),
                ctx.tabs.has_closed_tabs(),
                ctx.bottom_dock_open(),
                ctx.prompt.is_active(),
                ctx.pending_dirty_close.is_some() || ctx.pending_quit.is_some(),
                ctx.branch_picker.is_active(),
                ctx.crumb_menu.is_active(),
                ctx.snippet_session.is_active(),
            );
        }
        if matches!(
            id,
            CMD_MOVE_ACTIVE_TAB_LEFT
                | CMD_MOVE_ACTIVE_TAB_RIGHT
                | CMD_SORT_TABS_BY_NAME
                | CMD_CLOSE_DUPLICATE_TABS
                | CMD_CLOSE_SAVED_TABS
                | CMD_CLOSE_OTHER_SAVED_TABS
                | CMD_CLOSE_SAVED_TABS_TO_RIGHT
                | CMD_CLOSE_SAVED_TABS_TO_LEFT
        ) {
            return tab_management_contextual_desc(
                id,
                base,
                ctx.tabs.active_tab_is_first(),
                ctx.tabs.active_tab_is_last(),
                ctx.tabs.tabs_already_sorted_by_name(),
                ctx.tabs.has_clean_duplicate_file_tabs(),
                ctx.tabs.has_saved_tabs_to_close(),
                ctx.tabs.has_other_saved_tabs_to_close(),
                ctx.tabs.has_saved_tabs_to_right(),
                ctx.tabs.has_saved_tabs_to_left(),
            );
        }
        if active_has_path {
            let desc = language_server_contextual_desc(id, ctx.language, active_read_only);
            if !matches!(desc, Cow::Borrowed("")) {
                return desc;
            }
        }
        command_contextual_desc_with_workspace(
            id,
            base,
            active_has_path,
            active_read_only,
            ctx.tabs.is_dirty(ctx.tabs.active()),
            ctx.tabs.dirty_count(),
            active_has_selection,
            active_can_copy,
            workspace_test_target,
        )
    }

    /// Draw the rich command palette overlay (mockup `.palette`): a dim scrim, a
    /// rounded indigo-glow card with a search field (magnifier + caret + ⌘K
    /// pill), a "COMMANDS" category, rows with icon + title + dim description +
    /// right-aligned kbd pills (selected row indigo-tinted with a key-glow), and
    /// a footer hint line. No-op when inactive.
    pub fn draw(&self, ctx: &mut crate::MuiContext, width: u32, height: u32) {
        if !self.active {
            return;
        }
        use crate::icons;
        let w = width as f32;
        let h = height as f32;
        let chrome = theme::CHROME_FONT_SIZE;
        let clip = ctx.clip;

        let top = self.scroll_top();
        let shown = self.filtered.len().saturating_sub(top).min(VISIBLE).min(6);

        // Card geometry (mockup: 600px wide, search 56px, cat 25px, rows 50px,
        // footer ~37px).
        let search_h = 56.0;
        let cat_h = 25.0;
        let row_h = 50.0;
        let foot_h = 37.0;
        let (box_x, box_w, _list_top, _row_h, box_h, _shown) = self.geometry(width, height);
        let box_y = 96.0_f32.min((h - box_h).max(0.0));
        let radius = 12.0_f32;

        // Scrim: dim + a faint indigo top wash.
        ctx.dl_rect(0.0, 0.0, w, h, MuiColor::new(0.0, 0.0, 0.0, 0.55));
        ctx.dl_grad_v(0.0, 0.0, w, h * 0.5, 0.0, theme::accent_a(0.05), theme::accent_a(0.0));
        // Drop shadow + indigo glow + card + border.
        ctx.dl_shadow(box_x, box_y + 14.0, box_w, box_h, radius, MuiColor::new(0.0, 0.0, 0.0, 0.85), 40.0);
        ctx.dl_shadow(box_x, box_y, box_w, box_h, radius, theme::ACCENT_GLOW(), 40.0);
        let mut card = theme::ELEVATED();
        card.a = 1.0;
        ctx.dl_round(box_x, box_y, box_w, box_h, radius, card);
        ctx.dl_stroke(box_x, box_y, box_w, box_h, radius, theme::BORDER_STRONG(), 1.0);

        // ---- search field ----
        ctx.dl_rect(box_x + 1.0, box_y + search_h - 1.0, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_icon(box_x + 18.0, box_y + (search_h - 20.0) * 0.5, 20.0, 20.0, icons::SEARCH, theme::DIM(), 1.7, false);
        let q_text_base_x = box_x + 50.0;
        let q_text_x = command_field_text_x(q_text_base_x, self.query.is_empty());
        let qy = box_y + (search_h - 16.0) * 0.5 - 1.0;
        let pill_w = 40.0;
        let pill_x = box_x + box_w - pill_w - 18.0;
        let pill_y = box_y + (search_h - 22.0) * 0.5;
        let (q_str, q_color): (&str, _) = if self.query.is_empty() {
            ("Type a command\u{2026}", theme::OVERLAY_SUBTLE())
        } else {
            (self.query.as_str(), theme::TEXT())
        };
        // Search font is larger (16px) per the mockup.
        let query_max = command_query_text_budget(q_text_x, pill_x, self.query.is_empty());
        let q_shown = fit_palette_text(&mut ctx.text, q_str, query_max, 16.0);
        ctx.text.queue_ui_sized(q_text_x, qy, &q_shown, q_color, 16.0, clip);
        let (q_w, _) = ctx.text.measure_ui_sized(&q_shown, 16.0);
        let caret_x = if self.query.is_empty() {
            q_text_base_x + 1.0
        } else {
            (q_text_x + q_w + 1.0).min(pill_x - 14.0)
        };
        ctx.dl_round(caret_x, box_y + (search_h - 18.0) * 0.5, 2.0, 18.0, 1.0, theme::ACCENT_BRIGHT());
        // Command-mode pill (right). ASCII ">_" prompt motif (the UI font lacks the
        // Mac command glyph, which also rendered as a box on Windows).
        ctx.dl_round(pill_x, pill_y, pill_w, 22.0, 5.0, theme::ACCENT_FAINT());
        ctx.dl_stroke(pill_x, pill_y, pill_w, 22.0, 5.0, theme::ACCENT_LINE(), 1.0);
        ctx.text.queue_ui_sized(pill_x + 11.0, pill_y + 4.5, ">_", theme::ACCENT_BRIGHT(), 10.5, clip);

        // ---- category label ----
        let cat_y = box_y + search_h + 9.0;
        let cat: String = "COMMANDS".chars().flat_map(|c| [c, '\u{2009}']).collect();
        ctx.text.queue_ui_sized(box_x + 18.0, cat_y, &cat, theme::OVERLAY_SUBTLE(), chrome - 2.5, clip);

        // ---- rows ----
        let list_top = box_y + search_h + cat_h;
        for vis in 0..shown {
            let idx = top + vis;
            let cmd = &self.filtered[idx];
            let ry = list_top + vis as f32 * row_h;
            let selected = idx == self.sel;
            let (icon, desc, fill) = Self::meta(cmd.id);
            let desc = self.contextual_desc(ctx, cmd.id, desc);
            if selected {
                ctx.dl_grad_h(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::accent_a(0.22), 0.9);
                ctx.dl_stroke(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_LINE(), 1.0);
                ctx.dl_shadow(box_x + 8.0, ry + 2.0, box_w - 16.0, row_h - 4.0, 8.0, theme::ACCENT_GLOW(), 16.0);
            }
            // Leading icon tile (30px rounded, bordered).
            let tile = 30.0;
            let tile_x = box_x + 18.0;
            let tile_y = ry + (row_h - tile) * 0.5;
            if selected {
                ctx.dl_round(tile_x, tile_y, tile, tile, 7.0, theme::accent_a(0.10));
                ctx.dl_stroke(tile_x, tile_y, tile, tile, 7.0, theme::ACCENT_LINE(), 1.0);
            } else {
                ctx.dl_round(tile_x, tile_y, tile, tile, 7.0, theme::BG_2());
                ctx.dl_stroke(tile_x, tile_y, tile, tile, 7.0, theme::BORDER(), 1.0);
            }
            let icon_col = if selected { theme::ACCENT_BRIGHT() } else { theme::TEXT_1() };
            ctx.dl_icon(tile_x + 6.5, tile_y + 6.5, 17.0, 17.0, icon, icon_col, 1.6, fill);

            // Right-aligned kbd pills (commands with no keybinding draw none).
            let gap = 4.0;
            let parts = keybinding_tokens(cmd.keybinding);
            let widths: Vec<f32> = parts
                .iter()
                .map(|p| shortcut_token_width(&mut ctx.text, p, 11.0))
                .collect();
            let total_w: f32 = widths.iter().sum::<f32>() + gap * (parts.len().saturating_sub(1)) as f32;
            let mut px = box_x + box_w - 20.0 - total_w;

            // Title + dim description (two lines), fitted before right chrome.
            let txt_x = box_x + 60.0;
            let text_right = if parts.is_empty() { box_x + box_w - 24.0 } else { px - 28.0 };
            let text_max = (text_right - txt_x).max(0.0);
            let title = fit_palette_text(&mut ctx.text, cmd.label, text_max, 13.5);
            ctx.text.queue_ui_sized(txt_x, ry + 11.0, &title, theme::TEXT(), 13.5, clip);
            if !desc.is_empty() {
                let desc_col = if selected { theme::TEXT_1() } else { theme::OVERLAY_MUTED() };
                let desc = fit_palette_text(&mut ctx.text, &desc, text_max, 11.5);
                ctx.text.queue_ui_sized(txt_x, ry + 28.0, &desc, desc_col, 11.5, clip);
            }

            let pill_h = 21.0;
            let py = ry + (row_h - pill_h) * 0.5;
            for (k, part) in parts.iter().enumerate() {
                let pw = widths[k];
                if matches!(part, ShortcutToken::Separator) {
                    ctx.text.queue_ui_sized(px + 1.5, py + 4.5, "/", theme::OVERLAY_MUTED(), 11.0, clip);
                    px += pw + gap;
                    continue;
                }
                let (pbg, pborder, pfg) = if selected {
                    (theme::accent_a(0.10), theme::ACCENT_LINE(), theme::ACCENT_BRIGHT())
                } else {
                    (theme::BG_2(), theme::BORDER_STRONG(), theme::TEXT_1())
                };
                ctx.dl_round(px, py, pw, pill_h, 5.0, pbg);
                ctx.dl_stroke(px, py, pw, pill_h, 5.0, pborder, 1.0);
                let ShortcutToken::Key(part) = part else {
                    unreachable!("separator handled before pill draw");
                };
                let lbl_w = shortcut_key_label_width(&mut ctx.text, part, 11.0);
                ctx.text.queue_ui_sized(px + (pw - lbl_w) * 0.5, py + 4.5, part, pfg, 11.0, clip);
                px += pw + gap;
            }
        }

        // ---- footer hint line ----
        let foot_y = box_y + box_h - foot_h;
        ctx.dl_rect(box_x + 1.0, foot_y, box_w - 2.0, 1.0, theme::BORDER());
        ctx.dl_round(box_x + 1.0, foot_y, box_w - 2.0, foot_h - 1.0, 0.0, theme::BG_2());
        let fty = foot_y + (foot_h - chrome + 1.0) * 0.5 - 1.0;
        let mut fx = box_x + 18.0;
        let foot_seg = |ctx: &mut crate::MuiContext, key: &str, label: &str, fx: &mut f32| {
            let kw = command_footer_key_width(&mut ctx.text, key, 10.0);
            ctx.dl_round(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BG_1());
            ctx.dl_stroke(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BORDER_STRONG(), 1.0);
            ctx.text.queue_ui_sized(*fx + 5.0, foot_y + (foot_h - 10.0) * 0.5, key, theme::TEXT_1(), 10.0, clip);
            *fx += kw + 6.0;
            ctx.text.queue_ui_sized(*fx, fty, label, theme::OVERLAY_SUBTLE(), 11.0, clip);
            *fx += command_footer_label_advance(&mut ctx.text, label, 11.0);
        };
        foot_seg(ctx, "Enter", "select", &mut fx);
        foot_seg(ctx, "esc", "dismiss", &mut fx);
        let tag = "Mighty Command Palette";
        let (tag_w, _) = ctx.text.measure_ui_sized(tag, 11.0);
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - tag_w, fty, tag, theme::ACCENT_BRIGHT(), 11.0, clip);
    }
}

fn command_field_text_x(base_x: f32, is_placeholder: bool) -> f32 {
    if is_placeholder {
        base_x + 10.0
    } else {
        base_x
    }
}

fn command_query_text_budget(text_x: f32, pill_x: f32, is_placeholder: bool) -> f32 {
    let trailing_gap = if is_placeholder { 24.0 } else { 14.0 };
    (pill_x - trailing_gap - text_x).max(0.0)
}

fn command_palette_width(window_w: f32) -> f32 {
    (window_w - 80.0).max(0.0).clamp(280.0, 600.0).min(window_w.max(1.0))
}

fn command_footer_key_width(text: &mut crate::text::Text, key: &str, size: f32) -> f32 {
    (text.measure_ui_sized(key, size).0 + 10.0).max(20.0)
}

fn command_footer_label_advance(text: &mut crate::text::Text, label: &str, size: f32) -> f32 {
    text.measure_ui_sized(label, size).0 + 16.0
}

/// Static non-contextual description for command surfaces outside the palette.
pub fn command_static_desc(id: u32) -> &'static str {
    PaletteEngine::meta(id).1
}

fn force_ghost_contextual_desc<'a>(
    base: &'a str,
    inline_ai_enabled: bool,
    has_api_key: bool,
    in_flight: bool,
) -> Cow<'a, str> {
    if !inline_ai_enabled {
        Cow::Borrowed("AI inline completion is disabled in Settings")
    } else if !has_api_key {
        Cow::Borrowed("Set ANTHROPIC_API_KEY to enable Inline AI")
    } else if in_flight {
        Cow::Borrowed("AI completion already running")
    } else {
        Cow::Borrowed(base)
    }
}

fn language_server_contextual_desc(
    id: u32,
    lang: crate::langdetect::Language,
    active_read_only: bool,
) -> Cow<'static, str> {
    let needs_server = matches!(
        id,
        CMD_GOTO_DEFINITION
            | CMD_PEEK_DEFINITION
            | CMD_HOVER
            | CMD_SIGNATURE_HELP
            | CMD_RENAME_SYMBOL
            | CMD_CODE_ACTIONS
    );
    if !needs_server {
        return Cow::Borrowed("");
    }
    if active_read_only && matches!(id, CMD_RENAME_SYMBOL | CMD_CODE_ACTIONS) {
        return Cow::Borrowed("");
    }
    match crate::lspregistry::unavailable_reason(lang) {
        Some(reason) => Cow::Owned(reason),
        None => Cow::Borrowed(""),
    }
}

fn dismiss_ghost_contextual_desc(base: &str, has_ghost: bool) -> Cow<'_, str> {
    if has_ghost {
        Cow::Borrowed(base)
    } else {
        Cow::Borrowed("No AI ghost completion visible")
    }
}

fn autocomplete_contextual_desc(
    base: &str,
    lang: crate::langdetect::Language,
) -> Cow<'_, str> {
    match crate::lspregistry::unavailable_reason(lang) {
        Some(reason) => Cow::Owned(format!("Use buffer-word fallback; {reason}")),
        None => Cow::Borrowed(base),
    }
}

fn web_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    running: bool,
    url_empty: bool,
    line_count: usize,
) -> Cow<'a, str> {
    match id {
        CMD_WEB_STOP if !running => Cow::Borrowed("No web server running"),
        CMD_WEB_OPEN_BROWSER if url_empty => Cow::Borrowed("Web URL not ready"),
        CMD_WEB_CLEAR_OUTPUT if line_count == 0 => Cow::Borrowed("Web output already empty"),
        CMD_WEB_CLOSE if !active => Cow::Borrowed("Web Playground is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn run_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    running: bool,
    line_count: usize,
) -> Cow<'a, str> {
    match id {
        CMD_RUN_STOP if !running => Cow::Borrowed("No run process to stop"),
        CMD_RUN_CLEAR_OUTPUT if line_count == 0 => Cow::Borrowed("Run output already empty"),
        CMD_RUN_CLOSE if !active => Cow::Borrowed("Run panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn test_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    running: bool,
    row_count: usize,
) -> Cow<'a, str> {
    match id {
        CMD_TEST_STOP if !running => Cow::Borrowed("No test run to stop"),
        CMD_TEST_CLEAR_RESULTS if row_count == 0 => Cow::Borrowed("Test results already empty"),
        CMD_TEST_CLOSE if !active => Cow::Borrowed("Testing panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn agents_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    run_line_count: usize,
) -> Cow<'a, str> {
    if matches!(id, CMD_AGENTS | CMD_AGENTS_REFRESH) {
        if let Some(desc) = configured_mty_missing_desc(id) {
            return Cow::Owned(desc);
        }
    }

    match id {
        CMD_AGENTS_REFRESH if !active => {
            Cow::Borrowed("Open Mighty Agents and rescan workspace topology")
        }
        CMD_AGENTS_CLEAR_RUN_OUTPUT if run_line_count == 0 => {
            Cow::Borrowed("Agents run output already empty")
        }
        CMD_AGENTS_CLOSE if !active => Cow::Borrowed("Mighty Agents panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn terminal_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    open: bool,
    present: bool,
    has_visible_content: bool,
) -> Cow<'a, str> {
    match id {
        CMD_TERMINAL_CLEAR if open && present && !has_visible_content => {
            Cow::Borrowed("Terminal is already empty")
        }
        CMD_TERMINAL_CLOSE if !open && !present => Cow::Borrowed("Terminal is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn problems_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    open: bool,
    count: usize,
    active_has_path: bool,
    lang: crate::langdetect::Language,
) -> Cow<'a, str> {
    match id {
        CMD_PROBLEMS_REFRESH if !active_has_path => {
            Cow::Borrowed("No file-backed tab; refresh clears diagnostics and opens Problems")
        }
        CMD_PROBLEMS_REFRESH => {
            if let Some(reason) = crate::lspregistry::unavailable_reason(lang) {
                return Cow::Owned(reason);
            }
            if !open {
                return Cow::Borrowed("Open Problems and refresh diagnostics");
            }
            Cow::Borrowed(base)
        }
        CMD_PROBLEMS_CLEAR if count == 0 => Cow::Borrowed("Problems diagnostics already empty"),
        CMD_PROBLEMS_CLOSE if !open => Cow::Borrowed("Problems panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn search_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    replace_focus: bool,
    query_empty: bool,
    results_query_current: bool,
    match_count: i32,
) -> Cow<'a, str> {
    match id {
        CMD_SEARCH_TOGGLE_REPLACE if replace_focus && active => {
            Cow::Borrowed("Focus Search query field")
        }
        CMD_SEARCH_TOGGLE_REPLACE if replace_focus => {
            Cow::Borrowed("Open Search and focus query field")
        }
        CMD_SEARCH_TOGGLE_REPLACE if active => Cow::Borrowed("Focus Search replace field"),
        CMD_SEARCH_TOGGLE_REPLACE => Cow::Borrowed("Open Search and focus replace field"),
        CMD_SEARCH_RUN if query_empty => Cow::Borrowed("Enter text to search"),
        CMD_SEARCH_RUN if results_query_current && match_count == 0 => {
            Cow::Borrowed("No project search results")
        }
        CMD_SEARCH_REPLACE_ALL if query_empty => Cow::Borrowed("Enter search text to replace"),
        CMD_SEARCH_REPLACE_ALL if !results_query_current || match_count == 0 => {
            Cow::Borrowed("Run Search before replacing")
        }
        CMD_SEARCH_CLEAR_RESULTS if match_count == 0 => {
            Cow::Borrowed("Search results already empty")
        }
        CMD_SEARCH_CLOSE if !active => Cow::Borrowed("Search panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn outline_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    symbol_count: usize,
) -> Cow<'a, str> {
    match id {
        CMD_OUTLINE_REFRESH if !active => Cow::Borrowed("Open Outline and refresh symbols"),
        CMD_OUTLINE_CLEAR_SYMBOLS if symbol_count == 0 => {
            Cow::Borrowed("Outline symbols already empty")
        }
        CMD_OUTLINE_CLOSE if !active => Cow::Borrowed("Outline panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn debug_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    state: crate::dap::DebugState,
    has_program: bool,
    breakpoint_count: usize,
    session_empty: bool,
) -> Cow<'a, str> {
    match id {
        CMD_DEBUG_STOP
            if !matches!(
                state,
                crate::dap::DebugState::Running | crate::dap::DebugState::Stopped
            ) =>
        {
            Cow::Borrowed("No debug session to stop")
        }
        CMD_DEBUG_PAUSE if !matches!(state, crate::dap::DebugState::Running) => {
            Cow::Borrowed("Pause is available while running")
        }
        CMD_DEBUG_STEP_OVER if !matches!(state, crate::dap::DebugState::Stopped) => {
            Cow::Borrowed("Step Over is available when paused")
        }
        CMD_DEBUG_STEP_INTO if !matches!(state, crate::dap::DebugState::Stopped) => {
            Cow::Borrowed("Step Into is available when paused")
        }
        CMD_DEBUG_STEP_OUT if !matches!(state, crate::dap::DebugState::Stopped) => {
            Cow::Borrowed("Step Out is available when paused")
        }
        CMD_DEBUG_RESTART if !has_program => Cow::Borrowed("No debug target to restart"),
        CMD_DEBUG_CLEAR_BREAKPOINTS if breakpoint_count == 0 => {
            Cow::Borrowed("No breakpoints to clear")
        }
        CMD_DEBUG_CLEAR_SESSION if session_empty => Cow::Borrowed("Debug session already empty"),
        CMD_DEBUG_CLOSE if !active => Cow::Borrowed("Run and Debug panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn source_control_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    has_repo: bool,
    staged_count: usize,
    unstaged_count: usize,
    message_empty: bool,
) -> Cow<'a, str> {
    match id {
        CMD_GIT_SWITCH_BRANCH
        | CMD_GIT_PUSH
        | CMD_GIT_PULL
        | CMD_GIT_FETCH
        | CMD_GIT_STAGE_ALL
        | CMD_GIT_UNSTAGE_ALL
        | CMD_GIT_COMMIT_STAGED
        | CMD_GIT_REFRESH_SOURCE_CONTROL
            if !has_repo =>
        {
            Cow::Borrowed("Not a git repository")
        }
        CMD_GIT_REFRESH_SOURCE_CONTROL if !active => {
            Cow::Borrowed("Open Source Control and refresh git status")
        }
        CMD_GIT_STAGE_ALL if unstaged_count == 0 => Cow::Borrowed("Nothing to stage"),
        CMD_GIT_UNSTAGE_ALL if staged_count == 0 => Cow::Borrowed("Nothing to unstage"),
        CMD_GIT_COMMIT_STAGED if staged_count == 0 => {
            Cow::Borrowed("No staged changes to commit")
        }
        CMD_GIT_COMMIT_STAGED if message_empty => Cow::Borrowed("Enter a commit message"),
        CMD_GIT_CLEAR_COMMIT_MESSAGE if message_empty => {
            Cow::Borrowed("Source Control message already empty")
        }
        CMD_GIT_CLOSE_SOURCE_CONTROL if !active => {
            Cow::Borrowed("Source Control panel is already closed")
        }
        _ => Cow::Borrowed(base),
    }
}

fn explorer_contextual_desc(id: u32, base: &str, active: bool) -> Cow<'_, str> {
    match id {
        CMD_EXPLORER_REFRESH if !active => Cow::Borrowed("Open Explorer and refresh file tree"),
        CMD_EXPLORER_CLOSE if !active => Cow::Borrowed("Explorer panel is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn keyboard_shortcuts_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active: bool,
    selected_has_override: bool,
    overrides_empty: bool,
) -> Cow<'a, str> {
    match id {
        CMD_KEYBOARD_SHORTCUTS_CLOSE if !active => {
            Cow::Borrowed("Keyboard Shortcuts is already closed")
        }
        CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED if !selected_has_override => {
            Cow::Borrowed("Keyboard Shortcuts selection already uses default")
        }
        CMD_KEYBOARD_SHORTCUTS_RESET_ALL if overrides_empty => {
            Cow::Borrowed("Keyboard Shortcuts already use defaults")
        }
        _ => Cow::Borrowed(base),
    }
}

fn markdown_preview_contextual_desc<'a>(
    base: &'a str,
    language: crate::langdetect::Language,
    markdown_open: bool,
) -> Cow<'a, str> {
    if language != crate::langdetect::Language::Markdown {
        Cow::Borrowed("Markdown preview is available for Markdown files")
    } else if markdown_open {
        Cow::Borrowed("Markdown preview is already open")
    } else {
        Cow::Borrowed(base)
    }
}

#[allow(clippy::too_many_arguments)]
fn open_surface_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    explorer_open: bool,
    search_open: bool,
    source_control_open: bool,
    outline_open: bool,
    debug_open: bool,
    testing_open: bool,
    run_output_open: bool,
    problems_open: bool,
    ai_open: bool,
    terminal_open: bool,
    web_open: bool,
    settings_open: bool,
    theme_picker_open: bool,
    keyboard_shortcuts_open: bool,
    markdown_open: bool,
) -> Cow<'a, str> {
    match id {
        CMD_VIEW_EXPLORER if explorer_open => Cow::Borrowed("Explorer panel is already open"),
        CMD_VIEW_SEARCH if search_open => Cow::Borrowed("Search panel is already open"),
        CMD_VIEW_SOURCE_CONTROL if source_control_open => {
            Cow::Borrowed("Source Control panel is already open")
        }
        CMD_VIEW_OUTLINE if outline_open => Cow::Borrowed("Outline panel is already open"),
        CMD_VIEW_RUN_DEBUG if debug_open => Cow::Borrowed("Run and Debug panel is already open"),
        CMD_VIEW_TESTING if testing_open => Cow::Borrowed("Testing panel is already open"),
        CMD_VIEW_RUN_OUTPUT if run_output_open => Cow::Borrowed("Run output panel is already open"),
        CMD_VIEW_PROBLEMS if problems_open => Cow::Borrowed("Problems panel is already open"),
        CMD_VIEW_AI_COPILOT if ai_open => Cow::Borrowed("AI Copilot panel is already open"),
        CMD_TOGGLE_TERMINAL | CMD_VIEW_TERMINAL if terminal_open => {
            Cow::Borrowed("Focus integrated terminal")
        }
        CMD_VIEW_WEB_PLAYGROUND if web_open => {
            Cow::Borrowed("Web Playground panel is already open")
        }
        CMD_SETTINGS if settings_open => Cow::Borrowed("Settings panel is already open"),
        CMD_COLOR_THEME if theme_picker_open => {
            Cow::Borrowed("Color theme picker is already open")
        }
        CMD_KEYBOARD_SHORTCUTS if keyboard_shortcuts_open => {
            Cow::Borrowed("Keyboard Shortcuts is already open")
        }
        CMD_MARKDOWN_PREVIEW if markdown_open => Cow::Borrowed("Markdown preview is already open"),
        _ => Cow::Borrowed(base),
    }
}

fn close_action_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    blame_active: bool,
    welcome_visible: bool,
) -> Cow<'a, str> {
    match id {
        CMD_GIT_HIDE_BLAME if !blame_active => Cow::Borrowed("Blame is already hidden"),
        CMD_WELCOME_CLOSE if !welcome_visible => Cow::Borrowed("Welcome is already closed"),
        _ => Cow::Borrowed(base),
    }
}

fn blame_toggle_contextual_desc<'a>(
    base: &'a str,
    blame_active: bool,
    active_has_path: bool,
) -> Cow<'a, str> {
    if blame_active {
        Cow::Borrowed("Hide git blame gutter")
    } else if !active_has_path {
        Cow::Borrowed("No file to blame: (scratch)")
    } else {
        Cow::Borrowed(base)
    }
}

fn fold_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    has_cursor_fold: bool,
    no_foldable_blocks: bool,
    all_blocks_folded: bool,
    has_folded_blocks: bool,
) -> Cow<'a, str> {
    match id {
        CMD_FOLD_TOGGLE if !has_cursor_fold => Cow::Borrowed("No foldable block at cursor"),
        CMD_FOLD_ALL if no_foldable_blocks => Cow::Borrowed("No foldable blocks"),
        CMD_FOLD_ALL if all_blocks_folded => {
            Cow::Borrowed("All foldable blocks already folded")
        }
        CMD_UNFOLD_ALL if no_foldable_blocks => Cow::Borrowed("No foldable blocks"),
        CMD_UNFOLD_ALL if !has_folded_blocks => Cow::Borrowed("No folded blocks to unfold"),
        _ => Cow::Borrowed(base),
    }
}

fn layout_command_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    sidebar_visible: bool,
    sidebar_preset: u8,
    bottom_dock_open: bool,
    dock_preset: usize,
) -> Cow<'a, str> {
    match id {
        CMD_TOGGLE_SIDEBAR if sidebar_visible => Cow::Borrowed("Hide the left sidebar"),
        CMD_TOGGLE_SIDEBAR => Cow::Borrowed("Show the left sidebar"),
        CMD_SIDEBAR_COMPACT if sidebar_visible && sidebar_preset == 1 => {
            Cow::Borrowed("Sidebar is already compact")
        }
        CMD_SIDEBAR_COMPACT if sidebar_visible => Cow::Borrowed("Set sidebar to compact width"),
        CMD_SIDEBAR_COMPACT => Cow::Borrowed("Open sidebar at compact width"),
        CMD_SIDEBAR_DEFAULT if sidebar_visible && sidebar_preset == 0 => {
            Cow::Borrowed("Sidebar is already default width")
        }
        CMD_SIDEBAR_DEFAULT if sidebar_visible => Cow::Borrowed("Set sidebar to default width"),
        CMD_SIDEBAR_DEFAULT => Cow::Borrowed("Open sidebar at default width"),
        CMD_SIDEBAR_WIDE if sidebar_visible && sidebar_preset == 2 => {
            Cow::Borrowed("Sidebar is already wide")
        }
        CMD_SIDEBAR_WIDE if sidebar_visible => Cow::Borrowed("Set sidebar to wide width"),
        CMD_SIDEBAR_WIDE => Cow::Borrowed("Open sidebar at wide width"),
        CMD_SIDEBAR_CYCLE_WIDTH if sidebar_preset == 0 && sidebar_visible => {
            Cow::Borrowed("Cycle sidebar to compact width")
        }
        CMD_SIDEBAR_CYCLE_WIDTH if sidebar_preset == 0 => {
            Cow::Borrowed("Open sidebar at compact width")
        }
        CMD_SIDEBAR_CYCLE_WIDTH if sidebar_preset == 1 => {
            Cow::Borrowed("Cycle sidebar to wide width")
        }
        CMD_SIDEBAR_CYCLE_WIDTH => Cow::Borrowed("Cycle sidebar to default width"),
        CMD_DOCK_COMPACT if bottom_dock_open && dock_preset == 0 => {
            Cow::Borrowed("Bottom dock is already compact")
        }
        CMD_DOCK_COMPACT if bottom_dock_open => Cow::Borrowed("Set bottom dock to compact height"),
        CMD_DOCK_COMPACT => Cow::Borrowed("Open bottom dock at compact height"),
        CMD_DOCK_RESET if bottom_dock_open && dock_preset == 1 => {
            Cow::Borrowed("Bottom dock is already default height")
        }
        CMD_DOCK_RESET if bottom_dock_open => Cow::Borrowed("Set bottom dock to default height"),
        CMD_DOCK_RESET => Cow::Borrowed("Open bottom dock at default height"),
        CMD_DOCK_EXPANDED if bottom_dock_open && dock_preset == 2 => {
            Cow::Borrowed("Bottom dock is already expanded")
        }
        CMD_DOCK_EXPANDED if bottom_dock_open => {
            Cow::Borrowed("Set bottom dock to expanded height")
        }
        CMD_DOCK_EXPANDED => Cow::Borrowed("Open bottom dock at expanded height"),
        _ => Cow::Borrowed(base),
    }
}

fn close_surface_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    settings_open: bool,
    diff_open: bool,
    peek_open: bool,
    markdown_open: bool,
) -> Cow<'a, str> {
    match id {
        CMD_SETTINGS_CLOSE if !settings_open => Cow::Borrowed("Settings panel is already closed"),
        CMD_DIFF_CLOSE_VIEW if !diff_open => Cow::Borrowed("Diff view is already closed"),
        CMD_PEEK_CLOSE if !peek_open => Cow::Borrowed("Peek view is already closed"),
        CMD_MARKDOWN_CLOSE_PREVIEW if !markdown_open => {
            Cow::Borrowed("Markdown preview is already closed")
        }
        _ => Cow::Borrowed(base),
    }
}

#[allow(clippy::too_many_arguments)]
fn transient_surface_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    ai_open: bool,
    sidebar_open: bool,
    theme_picker_open: bool,
    hover_open: bool,
    signature_open: bool,
    code_actions_open: bool,
    find_replace_open: bool,
    autocomplete_open: bool,
    palette_open: bool,
    quickopen_open: bool,
) -> Cow<'a, str> {
    match id {
        CMD_AI_CLOSE if !ai_open => Cow::Borrowed("AI Copilot is already closed"),
        CMD_SIDEBAR_CLOSE if !sidebar_open => Cow::Borrowed("Sidebar is already closed"),
        CMD_COLOR_THEME_CLOSE if !theme_picker_open => {
            Cow::Borrowed("No color theme picker open")
        }
        CMD_HOVER_CLOSE if !hover_open => Cow::Borrowed("No hover popup open"),
        CMD_SIGNATURE_HELP_CLOSE if !signature_open => {
            Cow::Borrowed("No signature help popup open")
        }
        CMD_CODE_ACTIONS_CLOSE if !code_actions_open => {
            Cow::Borrowed("No code action menu open")
        }
        CMD_FIND_REPLACE_CLOSE if !find_replace_open => {
            Cow::Borrowed("No Find & Replace bar open")
        }
        CMD_AUTOCOMPLETE_CLOSE if !autocomplete_open => {
            Cow::Borrowed("No autocomplete suggestions open")
        }
        CMD_COMMAND_PALETTE_CLOSE if !palette_open => Cow::Borrowed("No command palette open"),
        CMD_QUICK_OPEN_CLOSE if !quickopen_open => Cow::Borrowed("No Quick Open panel open"),
        _ => Cow::Borrowed(base),
    }
}

#[allow(clippy::too_many_arguments)]
fn utility_command_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    notifications_present: bool,
    has_closed_tabs: bool,
    bottom_dock_open: bool,
    prompt_open: bool,
    dirty_confirm_open: bool,
    branch_picker_open: bool,
    breadcrumb_menu_open: bool,
    snippet_active: bool,
) -> Cow<'a, str> {
    match id {
        CMD_CLEAR_NOTIFICATIONS if !notifications_present => {
            Cow::Borrowed("No notifications to clear")
        }
        CMD_REOPEN_CLOSED_TAB if !has_closed_tabs => Cow::Borrowed("No closed tab to reopen"),
        CMD_DOCK_CLOSE if !bottom_dock_open => Cow::Borrowed("No bottom dock is open"),
        CMD_PROMPT_CANCEL if !prompt_open => Cow::Borrowed("No prompt input open"),
        CMD_DIRTY_CONFIRM_CANCEL if !dirty_confirm_open => {
            Cow::Borrowed("No unsaved changes confirmation open")
        }
        CMD_GIT_BRANCH_CANCEL if !branch_picker_open => Cow::Borrowed("No branch picker open"),
        CMD_BREADCRUMB_MENU_CANCEL if !breadcrumb_menu_open => {
            Cow::Borrowed("No breadcrumb menu open")
        }
        CMD_SNIPPET_CANCEL if !snippet_active => Cow::Borrowed("No snippet session active"),
        _ => Cow::Borrowed(base),
    }
}

#[allow(clippy::too_many_arguments)]
fn tab_management_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active_first: bool,
    active_last: bool,
    tabs_sorted: bool,
    has_clean_duplicate_tabs: bool,
    has_saved_tabs: bool,
    has_other_saved_tabs: bool,
    has_saved_tabs_right: bool,
    has_saved_tabs_left: bool,
) -> Cow<'a, str> {
    match id {
        CMD_MOVE_ACTIVE_TAB_LEFT if active_first => Cow::Borrowed("Tab is already first"),
        CMD_MOVE_ACTIVE_TAB_RIGHT if active_last => Cow::Borrowed("Tab is already last"),
        CMD_SORT_TABS_BY_NAME if tabs_sorted => Cow::Borrowed("Tabs already sorted"),
        CMD_CLOSE_DUPLICATE_TABS if !has_clean_duplicate_tabs => {
            Cow::Borrowed("No duplicate file tabs")
        }
        CMD_CLOSE_SAVED_TABS if !has_saved_tabs => Cow::Borrowed("No saved tabs to close"),
        CMD_CLOSE_OTHER_SAVED_TABS if !has_other_saved_tabs => {
            Cow::Borrowed("No other saved tabs to close")
        }
        CMD_CLOSE_SAVED_TABS_TO_RIGHT if !has_saved_tabs_right => {
            Cow::Borrowed("No saved tabs to the right")
        }
        CMD_CLOSE_SAVED_TABS_TO_LEFT if !has_saved_tabs_left => {
            Cow::Borrowed("No saved tabs to the left")
        }
        _ => Cow::Borrowed(base),
    }
}

#[cfg(test)]
fn command_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active_has_path: bool,
    active_read_only: bool,
    active_dirty: bool,
    dirty_count: usize,
    active_has_selection: bool,
    active_can_copy: bool,
) -> Cow<'a, str> {
    command_contextual_desc_with_workspace(
        id,
        base,
        active_has_path,
        active_read_only,
        active_dirty,
        dirty_count,
        active_has_selection,
        active_can_copy,
        false,
    )
}

fn command_contextual_desc_with_workspace<'a>(
    id: u32,
    base: &'a str,
    active_has_path: bool,
    active_read_only: bool,
    active_dirty: bool,
    dirty_count: usize,
    active_has_selection: bool,
    active_can_copy: bool,
    workspace_test_target: bool,
) -> Cow<'a, str> {
    if active_read_only {
        return match id {
            CMD_SAVE | CMD_SAVE_AS => Cow::Borrowed("Read-only preview: saving is unavailable"),
            CMD_RELOAD_ACTIVE_FILE | CMD_REVERT_ACTIVE_FILE => Cow::Borrowed("Reload this read-only preview from disk"),
            CMD_RENAME_ACTIVE_FILE | CMD_DELETE_ACTIVE_FILE => Cow::Borrowed("Read-only preview: file edits are unavailable"),
            CMD_COPY_SELECTION_OR_LINE if active_can_copy => Cow::Borrowed("Read-only preview: copy is available"),
            CMD_COPY_SELECTION_OR_LINE => Cow::Borrowed("Read-only preview has no text to copy"),
            CMD_CUT_SELECTION_OR_LINE | CMD_PASTE_IN_EDITOR => Cow::Borrowed("Read-only preview: editing clipboard actions are unavailable"),
            CMD_UNDO => Cow::Borrowed("Read-only preview: undo is unavailable"),
            CMD_REDO => Cow::Borrowed("Read-only preview: redo is unavailable"),
            CMD_DELETE_PREVIOUS_WORD
            | CMD_DELETE_NEXT_WORD
            | CMD_INDENT_LINE_SELECTION
            | CMD_OUTDENT_LINE_SELECTION
            | CMD_TOGGLE_LINE_COMMENT
            | CMD_DUPLICATE_LINE_SELECTION
            | CMD_MOVE_LINE_UP
            | CMD_MOVE_LINE_DOWN
            | CMD_DELETE_LINE
            | CMD_JOIN_LINE => Cow::Borrowed("Read-only preview: editing is unavailable"),
            CMD_FIND_REPLACE => Cow::Borrowed("Read-only preview: find is available, replace is unavailable"),
            CMD_AUTOCOMPLETE => Cow::Borrowed("Read-only preview: accepting completions is unavailable"),
            CMD_RENAME_SYMBOL => Cow::Borrowed("Read-only preview: symbol rename is unavailable"),
            CMD_CODE_ACTIONS => Cow::Borrowed("Read-only preview: code-action edits are unavailable"),
            CMD_INLINE_AI_ASK | CMD_FORCE_GHOST_COMPLETION => Cow::Borrowed("Read-only preview: inline AI edits are unavailable"),
            CMD_FORMAT_DOCUMENT => Cow::Borrowed("Read-only preview: formatting is unavailable"),
            CMD_RUN_FILE => Cow::Borrowed("Read-only preview: Run is unavailable"),
            CMD_RUN_TESTS | CMD_RUN_TEST_AT_CURSOR => Cow::Borrowed("Read-only preview: tests are unavailable"),
            CMD_DEBUG_START_CONTINUE => Cow::Borrowed("Read-only preview: debugging is unavailable"),
            CMD_RUN_IN_BROWSER => Cow::Borrowed("Read-only preview: browser run is unavailable"),
            _ => Cow::Borrowed(base),
        };
    }

    if active_has_path
        || id == CMD_NEW_PROJECT
        || (id == CMD_RUN_TESTS && workspace_test_target)
    {
        if let Some(desc) = configured_mty_missing_desc(id) {
            return Cow::Owned(desc);
        }
    }

    match id {
        CMD_COPY_SELECTION_OR_LINE if active_has_selection => Cow::Borrowed("Copy the active selection to the clipboard"),
        CMD_COPY_SELECTION_OR_LINE if active_can_copy => Cow::Borrowed("Copy the current line to the clipboard"),
        CMD_COPY_SELECTION_OR_LINE => Cow::Borrowed("No selection or line text to copy"),
        CMD_CUT_SELECTION_OR_LINE if active_has_selection => Cow::Borrowed("Cut the active selection to the clipboard"),
        CMD_CUT_SELECTION_OR_LINE if active_can_copy => Cow::Borrowed("Cut the current line to the clipboard"),
        CMD_CUT_SELECTION_OR_LINE => Cow::Borrowed("No selection or line text to cut"),
        CMD_SAVE if active_has_path => Cow::Borrowed("Write the active file to disk"),
        CMD_SAVE => Cow::Borrowed("Choose a path before saving this untitled file"),
        CMD_SAVE_AS if active_has_path => Cow::Borrowed("Choose a new path or filename for this file"),
        CMD_SAVE_AS => Cow::Borrowed("Choose where this untitled file should live"),
        CMD_SAVE_ALL if dirty_count == 0 => Cow::Borrowed("No unsaved tabs need writing"),
        CMD_SAVE_ALL if dirty_count == 1 => Cow::Borrowed("Write the one unsaved tab"),
        CMD_SAVE_ALL => Cow::Owned(format!("Write {dirty_count} unsaved tabs")),
        CMD_RELOAD_ACTIVE_FILE if active_has_path && active_dirty => Cow::Borrowed("Save or discard changes before reloading"),
        CMD_RELOAD_ACTIVE_FILE if active_has_path => Cow::Borrowed("Reload the active file from disk"),
        CMD_RELOAD_ACTIVE_FILE => Cow::Borrowed("No file-backed tab to reload"),
        CMD_REVERT_ACTIVE_FILE if active_has_path => Cow::Borrowed("Discard local edits and reload from disk"),
        CMD_REVERT_ACTIVE_FILE => Cow::Borrowed("No file-backed tab to revert"),
        CMD_HOVER if !active_has_path => Cow::Borrowed("Save this untitled file before requesting hover"),
        CMD_GOTO_DEFINITION if !active_has_path => Cow::Borrowed("Save this untitled file before Go to Definition"),
        CMD_PEEK_DEFINITION if !active_has_path => Cow::Borrowed("Save this untitled file before Peek Definition"),
        CMD_SIGNATURE_HELP if !active_has_path => Cow::Borrowed("Save this untitled file before signature help"),
        CMD_RENAME_SYMBOL if !active_has_path => Cow::Borrowed("Save this untitled file before symbol rename"),
        CMD_CODE_ACTIONS if !active_has_path => Cow::Borrowed("Save this untitled file before code actions"),
        CMD_FORMAT_DOCUMENT if !active_has_path => Cow::Borrowed("Save this untitled file before formatting"),
        CMD_RUN_FILE if !active_has_path => Cow::Borrowed("Save this untitled file before running"),
        CMD_RUN_TESTS if !active_has_path && !workspace_test_target => {
            Cow::Borrowed("Save this untitled file or open a Mighty folder before running tests")
        }
        CMD_RUN_TEST_AT_CURSOR if !active_has_path => Cow::Borrowed("Save this untitled file before running test at cursor"),
        CMD_DEBUG_START_CONTINUE if !active_has_path => Cow::Borrowed("Save this untitled file before starting debug"),
        CMD_RUN_IN_BROWSER if !active_has_path => Cow::Borrowed("Save this untitled file before running in browser"),
        CMD_RENAME_ACTIVE_FILE if active_has_path => Cow::Borrowed("Rename the active file on disk"),
        CMD_RENAME_ACTIVE_FILE => Cow::Borrowed("Save this untitled file before renaming it"),
        CMD_DELETE_ACTIVE_FILE if active_has_path && active_dirty => Cow::Borrowed("Save or discard changes before deleting"),
        CMD_DELETE_ACTIVE_FILE if active_has_path => Cow::Borrowed("Delete the active file after confirmation"),
        CMD_DELETE_ACTIVE_FILE => Cow::Borrowed("Needs a file-backed tab"),
        CMD_COPY_ACTIVE_FILE_PATH | CMD_COPY_ACTIVE_FILE_RELATIVE_PATH | CMD_COPY_ACTIVE_FILE_NAME | CMD_COPY_ACTIVE_FILE_DIRECTORY
            if !active_has_path =>
        {
            Cow::Borrowed("Needs a file-backed tab")
        }
        _ => Cow::Borrowed(base),
    }
}

fn palette_basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn active_file_utility_stale_target_desc(id: u32, path: &std::path::Path) -> Option<String> {
    let prefix = match id {
        CMD_REVEAL_ACTIVE_FILE | CMD_REVEAL_ACTIVE_FILE_IN_OS => "Reveal target",
        CMD_COPY_ACTIVE_FILE_PATH
        | CMD_COPY_ACTIVE_FILE_RELATIVE_PATH
        | CMD_COPY_ACTIVE_FILE_NAME
        | CMD_COPY_ACTIVE_FILE_DIRECTORY => "Copy target",
        _ => return None,
    };
    let name = palette_basename(path);
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => None,
        Ok(_) => Some(format!("{prefix} is not a file: {name}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("{prefix} missing: {name}"))
        }
        Err(e) => Some(format!("{prefix} unavailable: {name}: {e}")),
    }
}

fn active_file_edit_stale_target_desc(id: u32, path: &std::path::Path) -> Option<String> {
    let name = palette_basename(path);
    match id {
        CMD_RENAME_ACTIVE_FILE => match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => None,
            Ok(_) => Some(format!("Rename failed: {name}: not a file")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Some(format!("Rename source missing: {name}"))
            }
            Err(e) => Some(format!("Rename failed: {name}: {e}")),
        },
        CMD_DELETE_ACTIVE_FILE => match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => None,
            Ok(_) => Some(format!("Delete failed: {name}: not a file")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Some(format!("Delete target missing: {name}"))
            }
            Err(e) => Some(format!("Delete failed: {name}: {e}")),
        },
        _ => None,
    }
}

fn blame_stale_target_desc(path: &std::path::Path) -> Option<String> {
    let name = palette_basename(path);
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => None,
        Ok(_) => Some(format!("Blame failed: {name}: not a file")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("Blame target missing: {name}"))
        }
        Err(e) => Some(format!("Blame failed: {name}: {e}")),
    }
}

fn reload_revert_stale_target_desc(id: u32, path: &std::path::Path) -> Option<String> {
    let action = match id {
        CMD_RELOAD_ACTIVE_FILE => "Reload",
        CMD_REVERT_ACTIVE_FILE => "Revert",
        _ => return None,
    };
    let name = palette_basename(path);
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => None,
        Ok(_) => Some(format!("{action} failed: {name}: not a file")),
        Err(e) => Some(format!("{action} failed: {name}: {e}")),
    }
}

fn configured_mty_missing_desc(id: u32) -> Option<String> {
    let needs_mty = matches!(
        id,
        CMD_FORMAT_DOCUMENT
            | CMD_RUN_FILE
            | CMD_RUN_TESTS
            | CMD_RUN_TEST_AT_CURSOR
            | CMD_DEBUG_START_CONTINUE
            | CMD_RUN_IN_BROWSER
            | CMD_AGENTS
            | CMD_AGENTS_REFRESH
            | CMD_NEW_PROJECT
    );
    if !needs_mty {
        return None;
    }

    let raw = std::env::var("MIGHTY_MTY").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = std::path::Path::new(trimmed);
    if path.is_file() {
        return None;
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed);
    let action = match id {
        CMD_FORMAT_DOCUMENT => "Format",
        CMD_RUN_FILE => "Run",
        CMD_RUN_TESTS | CMD_RUN_TEST_AT_CURSOR => "Tests",
        CMD_DEBUG_START_CONTINUE => "Debug",
        CMD_RUN_IN_BROWSER => "Browser run",
        CMD_AGENTS | CMD_AGENTS_REFRESH => "Mighty Agents",
        CMD_NEW_PROJECT => "New Project",
        _ => "Command",
    };
    Some(format!(
        "{action} unavailable: MIGHTY_MTY points to missing {name}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_are_unique() {
        let mut ids: Vec<u32> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "command ids must be unique");
    }

    #[test]
    fn registry_distinguishes_dialog_workspace_and_untitled_file_creation() {
        let file_dialog = COMMANDS
            .iter()
            .find(|c| c.id == CMD_NEW_FILE)
            .expect("dialog new-file command should exist");
        let untitled = COMMANDS
            .iter()
            .find(|c| c.id == CMD_NEW_UNTITLED_FILE)
            .expect("untitled new-file command should exist");
        let workspace = COMMANDS
            .iter()
            .find(|c| c.id == CMD_NEW_WORKSPACE_FILE)
            .expect("workspace new-file command should exist");

        assert_eq!(file_dialog.label, "File: New File...");
        assert_eq!(file_dialog.keybinding, "Ctrl+N");
        assert_eq!(untitled.label, "File: New Untitled File");
        assert_eq!(untitled.keybinding, "");
        assert_eq!(workspace.label, "Explorer: New File in Workspace");
        assert_eq!(workspace.keybinding, "");
    }

    #[test]
    fn terminal_shortcut_label_matches_open_or_focus_behavior() {
        let terminal = COMMANDS
            .iter()
            .find(|c| c.id == CMD_TOGGLE_TERMINAL)
            .expect("legacy terminal shortcut command should exist");

        assert_eq!(terminal.label, "Terminal: Open or Focus");
        assert_eq!(terminal.keybinding, "Ctrl+`");
    }

    #[test]
    fn dialog_commands_use_standard_ellipsis_labels() {
        for (id, expected) in [
            (CMD_NEW_FILE, "File: New File..."),
            (CMD_OPEN_FILE, "File: Open File..."),
            (CMD_SAVE_AS, "File: Save As..."),
            (CMD_OPEN_FOLDER, "File: Open Folder..."),
            (CMD_NEW_PROJECT, "Mighty: New Project..."),
        ] {
            let command = COMMANDS
                .iter()
                .find(|c| c.id == id)
                .expect("dialog command should exist");
            assert_eq!(command.label, expected);
        }
    }

    #[test]
    fn empty_query_lists_all_in_registry_order() {
        let got = filter_commands(COMMANDS, "");
        let ids: Vec<u32> = got.iter().map(|c| c.id).collect();
        let expected: Vec<u32> = COMMANDS.iter().map(|c| c.id).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn prefix_match_ranks_first() {
        // "for" prefixes "Format Document"; should appear, prefix-ranked.
        let got = filter_commands(COMMANDS, "for");
        assert_eq!(got.first().map(|c| c.id), Some(CMD_FORMAT_DOCUMENT));
    }

    #[test]
    fn filter_is_case_insensitive() {
        let lower = filter_commands(COMMANDS, "save");
        let upper = filter_commands(COMMANDS, "SAVE");
        let lo: Vec<u32> = lower.iter().map(|c| c.id).collect();
        let up: Vec<u32> = upper.iter().map(|c| c.id).collect();
        assert_eq!(lo, up);
        assert_eq!(lo.first(), Some(&CMD_SAVE));
        assert!(lo.contains(&CMD_SAVE_AS));
    }

    #[test]
    fn substring_and_fuzzy_match() {
        // "term" is a substring of "Terminal: Open or Focus".
        let got = filter_commands(COMMANDS, "term");
        assert!(got.iter().any(|c| c.id == CMD_TOGGLE_TERMINAL));
        // "gtd" is a subsequence of "Go to Definition" (fuzzy).
        let fuzzy = filter_commands(COMMANDS, "gtd");
        assert!(
            fuzzy.iter().any(|c| c.id == CMD_GOTO_DEFINITION),
            "fuzzy subsequence should match: {fuzzy:?}"
        );
    }

    #[test]
    fn prefix_beats_substring_in_order() {
        // "ta": "Terminal: Open or Focus"/"View: Toggle Sidebar"? No. Use "t": prefixes nothing
        // but matches many. Use a query where a prefix and a substring coexist.
        // "g" prefixes "Go to Line"/"Go to Definition" (Prefix) and is a substring
        // of "Toggle ..." (Substring) — prefixes must come first.
        let got = filter_commands(COMMANDS, "g");
        let first_two: Vec<u32> = got.iter().take(2).map(|c| c.id).collect();
        assert!(
            first_two.contains(&CMD_GOTO_LINE) && first_two.contains(&CMD_GOTO_DEFINITION),
            "prefix matches (Go to ...) should rank ahead of substring matches: {got:?}"
        );
    }

    #[test]
    fn no_match_returns_empty() {
        let got = filter_commands(COMMANDS, "zzqqxx");
        assert!(got.is_empty());
    }

    #[test]
    fn duplicate_keystroke_still_finds_command() {
        let got = filter_commands(COMMANDS, "savee as");
        assert_eq!(got.first().map(|c| c.id), Some(CMD_SAVE_AS));
    }

    #[test]
    fn shortcut_text_is_searchable() {
        let got = filter_commands(COMMANDS, "Ctrl+P");
        assert_eq!(got.first().map(|c| c.id), Some(CMD_QUICK_OPEN));

        let space_separated = filter_commands(COMMANDS, "ctrl p");
        assert_eq!(
            space_separated.first().map(|c| c.id),
            Some(CMD_QUICK_OPEN)
        );
    }

    #[test]
    fn shortcut_alternatives_are_searchable_individually() {
        let got = filter_commands(COMMANDS, "ctrl2");
        assert_eq!(got.first().map(|c| c.id), Some(CMD_FOCUS_NEXT_PANE));
    }

    #[test]
    fn shortcut_tokens_split_alternatives_without_merging_keys() {
        assert_eq!(
            keybinding_tokens("Ctrl+1 / Ctrl+2"),
            vec![
                ShortcutToken::Key("Ctrl".to_string()),
                ShortcutToken::Key("1".to_string()),
                ShortcutToken::Separator,
                ShortcutToken::Key("Ctrl".to_string()),
                ShortcutToken::Key("2".to_string()),
            ]
        );
    }

    #[test]
    fn shortcut_tokens_keep_slash_key_inside_pill() {
        assert_eq!(
            keybinding_tokens("Ctrl+/"),
            vec![
                ShortcutToken::Key("Ctrl".to_string()),
                ShortcutToken::Key("/".to_string()),
            ]
        );
    }

    #[test]
    fn shortcut_token_width_uses_measured_key_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(480, 200) else {
            return;
        };

        let short = shortcut_token_width(&mut ctx.text, &ShortcutToken::Key("/".to_string()), 11.0);
        let long = shortcut_token_width(&mut ctx.text, &ShortcutToken::Key("Shift".to_string()), 11.0);

        assert!(short >= 22.0);
        assert!(long > short);
        assert_eq!(shortcut_token_width(&mut ctx.text, &ShortcutToken::Separator, 11.0), 8.0);
    }

    #[test]
    fn shortcut_token_width_contains_measured_label_with_padding() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(480, 200) else {
            return;
        };

        let label = "Enter";
        let label_w = shortcut_key_label_width(&mut ctx.text, label, 11.0);
        let pill_w = shortcut_token_width(&mut ctx.text, &ShortcutToken::Key(label.to_string()), 11.0);

        assert!(pill_w >= label_w + 14.0);
        assert!(pill_w >= 22.0);
    }

    #[test]
    fn engine_open_lists_all_selects_first() {
        let mut e = PaletteEngine::new();
        assert!(!e.is_active());
        e.open();
        assert!(e.is_active());
        assert_eq!(e.count(), COMMANDS.len());
        assert_eq!(e.selection(), 0);
        assert_eq!(e.selected_id(), COMMANDS[0].id as i32);
    }

    #[test]
    fn engine_typing_filters_and_resets_selection() {
        let mut e = PaletteEngine::new();
        e.open();
        e.move_sel(3);
        assert_eq!(e.selection(), 3);
        // Type "sa" -> matches "Save" / "Save As"; selection resets to 0.
        e.push_char('s');
        e.push_char('a');
        assert_eq!(e.selection(), 0);
        assert_eq!(e.selected_id(), CMD_SAVE as i32);
        assert!(e.count() >= 2);
        // Backspace back to "s".
        e.backspace();
        assert_eq!(e.query(), "s");
        assert!(e.count() > 1);
    }

    #[test]
    fn engine_move_wraps() {
        let mut e = PaletteEngine::new();
        e.open();
        let n = e.count();
        assert!(n >= 2);
        e.move_sel(-1);
        assert_eq!(e.selection(), n - 1); // wrap below 0 -> last
        e.move_sel(1);
        assert_eq!(e.selection(), 0); // wrap above end -> first
    }

    #[test]
    fn engine_selected_id_is_negative_when_no_match() {
        let mut e = PaletteEngine::new();
        e.open();
        for ch in "zzqqxx".chars() {
            e.push_char(ch);
        }
        assert_eq!(e.count(), 0);
        assert_eq!(e.selected_id(), -1);
    }

    #[test]
    fn engine_cancel_clears() {
        let mut e = PaletteEngine::new();
        e.open();
        e.push_char('s');
        e.cancel();
        assert!(!e.is_active());
        assert_eq!(e.count(), 0);
        assert_eq!(e.query(), "");
        assert_eq!(e.selected_id(), -1);
    }

    #[test]
    fn every_command_has_rich_row_metadata() {
        for cmd in COMMANDS {
            let (_icon, desc, _fill) = PaletteEngine::meta(cmd.id);
            assert!(
                !desc.trim().is_empty(),
                "{} should not render as a generic blank palette row",
                cmd.label
            );
        }
    }

    #[test]
    fn static_command_descriptions_are_available_to_other_surfaces() {
        assert_eq!(
            command_static_desc(CMD_OPEN_RECENT),
            "Open a recent file or workspace folder"
        );
        assert_eq!(
            command_static_desc(CMD_TOGGLE_TERMINAL),
            "Open the integrated terminal or focus it if already open"
        );
        assert_eq!(
            command_static_desc(CMD_TOGGLE_SIDEBAR),
            "Show or hide the left sidebar"
        );
        assert_eq!(
            command_static_desc(CMD_GIT_TOGGLE_BLAME),
            "Show or hide git blame in the gutter"
        );
        assert_eq!(
            command_static_desc(CMD_SEARCH_TOGGLE_REPLACE),
            "Open Search and move focus between query and replace"
        );
        assert_eq!(
            command_static_desc(CMD_DOCK_COMPACT),
            "Open the shared bottom dock at compact height"
        );
        assert_eq!(
            command_static_desc(CMD_DOCK_RESET),
            "Open the shared bottom dock at its default height"
        );
        assert_eq!(
            command_static_desc(CMD_DOCK_EXPANDED),
            "Open the shared bottom dock at expanded height"
        );
        assert_eq!(
            command_static_desc(CMD_SIDEBAR_COMPACT),
            "Open the sidebar at compact width"
        );
        assert_eq!(
            command_static_desc(CMD_SIDEBAR_DEFAULT),
            "Open the sidebar at its default responsive width"
        );
        assert_eq!(
            command_static_desc(CMD_SIDEBAR_WIDE),
            "Open the sidebar at wide width"
        );
        assert_eq!(
            command_static_desc(CMD_SIDEBAR_CYCLE_WIDTH),
            "Open the sidebar and cycle compact, default, and wide width"
        );
        assert_eq!(
            command_static_desc(CMD_MARKDOWN_PREVIEW),
            "Open the live Markdown preview pane"
        );
        assert_eq!(
            command_static_desc(CMD_MARKDOWN_CLOSE_PREVIEW),
            "Close the live Markdown preview pane"
        );
    }

    #[test]
    fn scroll_top_keeps_selection_visible() {
        let mut e = PaletteEngine::new();
        e.open(); // all commands, count > VISIBLE only if registry large enough
        if e.count() <= VISIBLE {
            // Registry smaller than the window: top is always 0.
            assert_eq!(e.scroll_top(), 0);
            return;
        }
        for _ in 0..(e.count() - 1) {
            e.move_sel(1);
        }
        let expected = (e.selection() + 1).saturating_sub(VISIBLE);
        assert_eq!(e.scroll_top(), expected);
    }

    #[test]
    fn click_row_selects_visible_result() {
        let mut e = PaletteEngine::new();
        e.open();
        let (box_x, _box_w, list_top, row_h, _box_h, _shown) = e.geometry(900, 700);
        let idx = e.click_row(box_x + 30.0, list_top + row_h + 4.0, 900, 700);
        assert_eq!(idx, 1);
        assert_eq!(e.selection(), 1);
        assert_eq!(e.click_row(box_x - 2.0, list_top + 4.0, 900, 700), -1);
    }

    #[test]
    fn geometry_clamps_card_inside_ultra_narrow_windows() {
        let mut e = PaletteEngine::new();
        e.open();
        let (box_x, box_w, _list_top, _row_h, _box_h, _shown) = e.geometry(180, 560);

        assert!(box_x >= 0.0);
        assert!(box_w <= 180.0);
        assert!(box_x + box_w <= 180.0 + 0.5);
    }

    #[test]
    fn empty_command_placeholder_does_not_overlap_caret() {
        let base = 300.0;
        assert_eq!(command_field_text_x(base, false), base);
        assert!(command_field_text_x(base, true) >= base + 8.0);
    }

    #[test]
    fn command_query_text_budget_stops_before_prompt_pill() {
        let text_x = 150.0;
        let pill_x = 560.0;
        let placeholder_budget = command_query_text_budget(text_x, pill_x, true);
        let query_budget = command_query_text_budget(text_x, pill_x, false);

        assert!(placeholder_budget < query_budget);
        assert!(text_x + placeholder_budget <= pill_x - 24.0);
        assert!(text_x + query_budget <= pill_x - 14.0);
        assert_eq!(command_query_text_budget(pill_x, text_x, false), 0.0);
    }

    #[test]
    fn language_server_palette_rows_explain_unavailable_servers() {
        let _guard = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old_config_dir = std::env::var_os("MUI_CONFIG_DIR");
        let root = std::env::temp_dir().join(format!(
            "mui_palette_lsp_unavailable_{}",
            std::process::id()
        ));
        let config_dir = root.join("config");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("lsp.toml"),
            "python = \"definitely-not-a-real-python-lsp-for-mighty-ide-tests\"\n",
        )
        .unwrap();
        std::env::set_var("MUI_CONFIG_DIR", &config_dir);

        let expected = "Python language server unavailable; configure python in lsp.toml";
        assert_eq!(
            autocomplete_contextual_desc("base", crate::langdetect::Language::Python).as_ref(),
            "Use buffer-word fallback; Python language server unavailable; configure python in lsp.toml"
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_REFRESH,
                "base",
                true,
                0,
                true,
                crate::langdetect::Language::Python
            )
            .as_ref(),
            expected
        );
        for id in [
            CMD_GOTO_DEFINITION,
            CMD_PEEK_DEFINITION,
            CMD_HOVER,
            CMD_SIGNATURE_HELP,
            CMD_RENAME_SYMBOL,
            CMD_CODE_ACTIONS,
        ] {
            assert_eq!(
                language_server_contextual_desc(
                    id,
                    crate::langdetect::Language::Python,
                    false
                )
                .as_ref(),
                expected
            );
        }

        assert_eq!(
            language_server_contextual_desc(
                CMD_RENAME_SYMBOL,
                crate::langdetect::Language::Python,
                true
            ),
            Cow::Borrowed("")
        );
        assert_eq!(
            language_server_contextual_desc(
                CMD_CODE_ACTIONS,
                crate::langdetect::Language::Python,
                true
            ),
            Cow::Borrowed("")
        );
        assert_eq!(
            language_server_contextual_desc(
                CMD_HOVER,
                crate::langdetect::Language::PlainText,
                false
            ),
            Cow::Borrowed("")
        );

        match old_config_dir {
            Some(v) => std::env::set_var("MUI_CONFIG_DIR", v),
            None => std::env::remove_var("MUI_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn autocomplete_description_keeps_plain_text_as_buffer_words() {
        assert_eq!(
            autocomplete_contextual_desc("base", crate::langdetect::Language::PlainText),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn palette_width_preserves_preferred_width_until_viewport_is_tiny() {
        assert_eq!(command_palette_width(900.0), 600.0);
        assert_eq!(command_palette_width(360.0), 280.0);
        assert_eq!(command_palette_width(180.0), 180.0);
    }

    #[test]
    fn command_footer_key_width_uses_measured_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(480, 200) else {
            return;
        };

        let short = command_footer_key_width(&mut ctx.text, "esc", 10.0);
        let long = command_footer_key_width(&mut ctx.text, "Enter", 10.0);

        assert!(short >= 20.0);
        assert!(long > short);
    }

    #[test]
    fn command_footer_label_advance_uses_measured_text() {
        let Some(mut ctx) = crate::MuiContext::new_offscreen(480, 200) else {
            return;
        };

        let open = command_footer_label_advance(&mut ctx.text, "open", 11.0);
        let dismiss = command_footer_label_advance(&mut ctx.text, "dismiss", 11.0);

        assert!(dismiss > open);
        assert!(open > 16.0);
    }

    #[test]
    fn file_command_descriptions_reflect_document_state() {
        assert_eq!(
            command_contextual_desc(CMD_SAVE, "base", false, false, false, 0, false, false),
            Cow::Borrowed("Choose a path before saving this untitled file")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE_AS, "base", true, false, false, 0, false, false),
            Cow::Borrowed("Choose a new path or filename for this file")
        );
        assert_eq!(
            command_contextual_desc(CMD_RENAME_ACTIVE_FILE, "base", false, false, false, 0, false, false),
            Cow::Borrowed("Save this untitled file before renaming it")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE, "base", true, true, true, 1, false, false),
            Cow::Borrowed("Read-only preview: saving is unavailable")
        );
        assert_eq!(
            command_contextual_desc(CMD_REVERT_ACTIVE_FILE, "base", true, true, false, 0, false, false),
            Cow::Borrowed("Reload this read-only preview from disk")
        );
        assert_eq!(
            command_contextual_desc(CMD_RELOAD_ACTIVE_FILE, "base", true, false, true, 1, false, false),
            Cow::Borrowed("Save or discard changes before reloading")
        );
        assert_eq!(
            command_contextual_desc(CMD_DELETE_ACTIVE_FILE, "base", true, false, true, 1, false, false),
            Cow::Borrowed("Save or discard changes before deleting")
        );
    }

    #[test]
    fn copy_command_descriptions_reflect_editor_state() {
        assert_eq!(
            command_contextual_desc(
                CMD_COPY_SELECTION_OR_LINE,
                "base",
                true,
                false,
                false,
                0,
                true,
                true
            ),
            Cow::Borrowed("Copy the active selection to the clipboard")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_COPY_SELECTION_OR_LINE,
                "base",
                true,
                false,
                false,
                0,
                false,
                true
            ),
            Cow::Borrowed("Copy the current line to the clipboard")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_COPY_SELECTION_OR_LINE,
                "base",
                true,
                false,
                false,
                0,
                false,
                false
            ),
            Cow::Borrowed("No selection or line text to copy")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_COPY_SELECTION_OR_LINE,
                "base",
                true,
                true,
                false,
                0,
                false,
                true
            ),
            Cow::Borrowed("Read-only preview: copy is available")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_COPY_SELECTION_OR_LINE,
                "base",
                true,
                true,
                false,
                0,
                false,
                false
            ),
            Cow::Borrowed("Read-only preview has no text to copy")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_CUT_SELECTION_OR_LINE,
                "base",
                true,
                false,
                false,
                0,
                true,
                true
            ),
            Cow::Borrowed("Cut the active selection to the clipboard")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_CUT_SELECTION_OR_LINE,
                "base",
                true,
                false,
                false,
                0,
                false,
                true
            ),
            Cow::Borrowed("Cut the current line to the clipboard")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_CUT_SELECTION_OR_LINE,
                "base",
                true,
                false,
                false,
                0,
                false,
                false
            ),
            Cow::Borrowed("No selection or line text to cut")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_CUT_SELECTION_OR_LINE,
                "base",
                true,
                true,
                false,
                0,
                true,
                true
            ),
            Cow::Borrowed("Read-only preview: editing clipboard actions are unavailable")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_PASTE_IN_EDITOR,
                "base",
                true,
                true,
                false,
                0,
                false,
                true
            ),
            Cow::Borrowed("Read-only preview: editing clipboard actions are unavailable")
        );
    }

    #[test]
    fn read_only_command_descriptions_reflect_blocked_actions() {
        for (id, expected) in [
            (CMD_UNDO, "Read-only preview: undo is unavailable"),
            (CMD_REDO, "Read-only preview: redo is unavailable"),
            (CMD_DELETE_PREVIOUS_WORD, "Read-only preview: editing is unavailable"),
            (CMD_DELETE_NEXT_WORD, "Read-only preview: editing is unavailable"),
            (CMD_INDENT_LINE_SELECTION, "Read-only preview: editing is unavailable"),
            (CMD_OUTDENT_LINE_SELECTION, "Read-only preview: editing is unavailable"),
            (CMD_TOGGLE_LINE_COMMENT, "Read-only preview: editing is unavailable"),
            (CMD_DUPLICATE_LINE_SELECTION, "Read-only preview: editing is unavailable"),
            (CMD_MOVE_LINE_UP, "Read-only preview: editing is unavailable"),
            (CMD_MOVE_LINE_DOWN, "Read-only preview: editing is unavailable"),
            (CMD_DELETE_LINE, "Read-only preview: editing is unavailable"),
            (CMD_JOIN_LINE, "Read-only preview: editing is unavailable"),
            (CMD_FIND_REPLACE, "Read-only preview: find is available, replace is unavailable"),
            (CMD_AUTOCOMPLETE, "Read-only preview: accepting completions is unavailable"),
            (CMD_RENAME_SYMBOL, "Read-only preview: symbol rename is unavailable"),
            (CMD_CODE_ACTIONS, "Read-only preview: code-action edits are unavailable"),
            (CMD_INLINE_AI_ASK, "Read-only preview: inline AI edits are unavailable"),
            (CMD_FORCE_GHOST_COMPLETION, "Read-only preview: inline AI edits are unavailable"),
            (CMD_FORMAT_DOCUMENT, "Read-only preview: formatting is unavailable"),
            (CMD_RUN_FILE, "Read-only preview: Run is unavailable"),
            (CMD_RUN_TESTS, "Read-only preview: tests are unavailable"),
            (CMD_RUN_TEST_AT_CURSOR, "Read-only preview: tests are unavailable"),
            (CMD_DEBUG_START_CONTINUE, "Read-only preview: debugging is unavailable"),
            (CMD_RUN_IN_BROWSER, "Read-only preview: browser run is unavailable"),
        ] {
            assert_eq!(
                command_contextual_desc(id, "base", true, true, false, 0, false, false),
                Cow::Borrowed(expected),
                "command {id} should explain read-only preview state"
            );
        }
    }

    #[test]
    fn file_backed_command_descriptions_reflect_unsaved_scratch_state() {
        for (id, expected) in [
            (CMD_HOVER, "Save this untitled file before requesting hover"),
            (CMD_GOTO_DEFINITION, "Save this untitled file before Go to Definition"),
            (CMD_PEEK_DEFINITION, "Save this untitled file before Peek Definition"),
            (CMD_SIGNATURE_HELP, "Save this untitled file before signature help"),
            (CMD_RENAME_SYMBOL, "Save this untitled file before symbol rename"),
            (CMD_CODE_ACTIONS, "Save this untitled file before code actions"),
            (CMD_FORMAT_DOCUMENT, "Save this untitled file before formatting"),
            (CMD_RUN_FILE, "Save this untitled file before running"),
            (CMD_RUN_TEST_AT_CURSOR, "Save this untitled file before running test at cursor"),
            (CMD_DEBUG_START_CONTINUE, "Save this untitled file before starting debug"),
            (CMD_RUN_IN_BROWSER, "Save this untitled file before running in browser"),
        ] {
            assert_eq!(
                command_contextual_desc(id, "base", false, false, false, 0, false, false),
                Cow::Borrowed(expected),
                "command {id} should explain unsaved scratch state"
            );
        }
    }

    #[test]
    fn run_tests_description_reflects_workspace_target_state() {
        assert_eq!(
            command_contextual_desc_with_workspace(
                CMD_RUN_TESTS,
                "Run the package's tests (mty test)",
                false,
                false,
                false,
                0,
                false,
                false,
                false,
            ),
            Cow::Borrowed("Save this untitled file or open a Mighty folder before running tests")
        );
        assert_eq!(
            command_contextual_desc_with_workspace(
                CMD_RUN_TESTS,
                "Run the package's tests (mty test)",
                false,
                false,
                false,
                0,
                false,
                false,
                true,
            ),
            Cow::Borrowed("Run the package's tests (mty test)")
        );
    }

    #[test]
    fn mty_override_command_descriptions_report_missing_configured_tool() {
        let _guard = crate::settings::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let old_mty = std::env::var_os("MIGHTY_MTY");
        let root = std::env::temp_dir().join(format!(
            "mui_palette_missing_mty_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let missing_mty = root.join("missing-mty.exe");
        std::env::set_var("MIGHTY_MTY", &missing_mty);

        assert_eq!(
            command_contextual_desc(
                CMD_FORMAT_DOCUMENT,
                "base",
                true,
                false,
                false,
                0,
                false,
                false,
            )
            .as_ref(),
            "Format unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            command_contextual_desc(CMD_RUN_FILE, "base", true, false, false, 0, false, false)
                .as_ref(),
            "Run unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            command_contextual_desc(
                CMD_RUN_TEST_AT_CURSOR,
                "base",
                true,
                false,
                false,
                0,
                false,
                false,
            )
            .as_ref(),
            "Tests unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            command_contextual_desc(
                CMD_DEBUG_START_CONTINUE,
                "base",
                true,
                false,
                false,
                0,
                false,
                false,
            )
            .as_ref(),
            "Debug unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            command_contextual_desc(
                CMD_RUN_IN_BROWSER,
                "base",
                true,
                false,
                false,
                0,
                false,
                false,
            )
            .as_ref(),
            "Browser run unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            command_contextual_desc(
                CMD_NEW_PROJECT,
                "base",
                false,
                false,
                false,
                0,
                false,
                false,
            )
            .as_ref(),
            "New Project unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            command_contextual_desc(
                CMD_FORMAT_DOCUMENT,
                "base",
                false,
                false,
                false,
                0,
                false,
                false,
            ),
            Cow::Borrowed("Save this untitled file before formatting")
        );
        assert_eq!(
            command_contextual_desc(CMD_RUN_FILE, "base", false, false, false, 0, false, false),
            Cow::Borrowed("Save this untitled file before running")
        );
        assert_eq!(
            command_contextual_desc(
                CMD_FORMAT_DOCUMENT,
                "base",
                true,
                true,
                false,
                0,
                false,
                false,
            ),
            Cow::Borrowed("Read-only preview: formatting is unavailable")
        );
        assert_eq!(
            command_contextual_desc(CMD_RUN_FILE, "base", true, true, false, 0, false, false),
            Cow::Borrowed("Read-only preview: Run is unavailable")
        );
        assert_eq!(
            command_contextual_desc_with_workspace(
                CMD_RUN_TESTS,
                "base",
                false,
                false,
                false,
                0,
                false,
                false,
                true,
            )
            .as_ref(),
            "Tests unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );

        if let Some(v) = old_mty {
            std::env::set_var("MIGHTY_MTY", v);
        } else {
            std::env::remove_var("MIGHTY_MTY");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn force_ghost_description_reflects_runtime_availability() {
        assert_eq!(
            force_ghost_contextual_desc("base", false, false, false),
            Cow::Borrowed("AI inline completion is disabled in Settings")
        );
        assert_eq!(
            force_ghost_contextual_desc("base", true, false, false),
            Cow::Borrowed("Set ANTHROPIC_API_KEY to enable Inline AI")
        );
        assert_eq!(
            force_ghost_contextual_desc("base", true, true, true),
            Cow::Borrowed("AI completion already running")
        );
        assert_eq!(
            force_ghost_contextual_desc("base", true, true, false),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn dismiss_ghost_description_reflects_visible_state() {
        assert_eq!(
            dismiss_ghost_contextual_desc("base", false),
            Cow::Borrowed("No AI ghost completion visible")
        );
        assert_eq!(
            dismiss_ghost_contextual_desc("base", true),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn web_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            web_contextual_desc(CMD_WEB_STOP, "base", false, false, true, 0),
            Cow::Borrowed("No web server running")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_OPEN_BROWSER, "base", true, true, true, 6),
            Cow::Borrowed("Web URL not ready")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_CLEAR_OUTPUT, "base", true, true, false, 0),
            Cow::Borrowed("Web output already empty")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_CLOSE, "base", false, false, true, 0),
            Cow::Borrowed("Web Playground is already closed")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_STOP, "base", true, true, false, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_OPEN_BROWSER, "base", true, true, false, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_CLEAR_OUTPUT, "base", true, true, false, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            web_contextual_desc(CMD_WEB_CLOSE, "base", true, true, false, 6),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn run_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            run_contextual_desc(CMD_RUN_STOP, "base", false, false, 0),
            Cow::Borrowed("No run process to stop")
        );
        assert_eq!(
            run_contextual_desc(CMD_RUN_CLEAR_OUTPUT, "base", true, true, 0),
            Cow::Borrowed("Run output already empty")
        );
        assert_eq!(
            run_contextual_desc(CMD_RUN_CLOSE, "base", false, false, 0),
            Cow::Borrowed("Run panel is already closed")
        );
        assert_eq!(
            run_contextual_desc(CMD_RUN_STOP, "base", true, true, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            run_contextual_desc(CMD_RUN_CLEAR_OUTPUT, "base", true, true, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            run_contextual_desc(CMD_RUN_CLOSE, "base", true, false, 0),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn test_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            test_contextual_desc(CMD_TEST_STOP, "base", false, false, 0),
            Cow::Borrowed("No test run to stop")
        );
        assert_eq!(
            test_contextual_desc(CMD_TEST_CLEAR_RESULTS, "base", true, true, 0),
            Cow::Borrowed("Test results already empty")
        );
        assert_eq!(
            test_contextual_desc(CMD_TEST_CLOSE, "base", false, false, 0),
            Cow::Borrowed("Testing panel is already closed")
        );
        assert_eq!(
            test_contextual_desc(CMD_TEST_STOP, "base", true, true, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            test_contextual_desc(CMD_TEST_CLEAR_RESULTS, "base", true, true, 6),
            Cow::Borrowed("base")
        );
        assert_eq!(
            test_contextual_desc(CMD_TEST_CLOSE, "base", true, false, 0),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn agents_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_REFRESH, "base", false, 0),
            Cow::Borrowed("Open Mighty Agents and rescan workspace topology")
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_CLEAR_RUN_OUTPUT, "base", true, 0),
            Cow::Borrowed("Agents run output already empty")
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_CLOSE, "base", false, 8),
            Cow::Borrowed("Mighty Agents panel is already closed")
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_CLEAR_RUN_OUTPUT, "base", true, 8),
            Cow::Borrowed("base")
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_CLOSE, "base", true, 0),
            Cow::Borrowed("base")
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_REFRESH, "base", true, 0),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn agents_command_descriptions_report_missing_configured_mty() {
        let _guard = crate::settings::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let old_mty = std::env::var_os("MIGHTY_MTY");
        let root = std::env::temp_dir().join(format!(
            "mui_palette_agents_missing_mty_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let missing_mty = root.join("missing-mty.exe");
        std::env::set_var("MIGHTY_MTY", &missing_mty);

        assert_eq!(
            agents_contextual_desc(CMD_AGENTS, "base", false, 0).as_ref(),
            "Mighty Agents unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_REFRESH, "base", true, 0).as_ref(),
            "Mighty Agents unavailable: MIGHTY_MTY points to missing missing-mty.exe"
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_CLEAR_RUN_OUTPUT, "base", true, 0),
            Cow::Borrowed("Agents run output already empty")
        );
        assert_eq!(
            agents_contextual_desc(CMD_AGENTS_CLOSE, "base", false, 0),
            Cow::Borrowed("Mighty Agents panel is already closed")
        );

        if let Some(v) = old_mty {
            std::env::set_var("MIGHTY_MTY", v);
        } else {
            std::env::remove_var("MIGHTY_MTY");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn terminal_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            terminal_contextual_desc(CMD_TERMINAL_CLEAR, "base", true, true, false),
            Cow::Borrowed("Terminal is already empty")
        );
        assert_eq!(
            terminal_contextual_desc(CMD_TERMINAL_CLOSE, "base", false, false, false),
            Cow::Borrowed("Terminal is already closed")
        );
        assert_eq!(
            terminal_contextual_desc(CMD_TERMINAL_CLEAR, "base", false, false, false),
            Cow::Borrowed("base")
        );
        assert_eq!(
            terminal_contextual_desc(CMD_TERMINAL_CLEAR, "base", true, true, true),
            Cow::Borrowed("base")
        );
        assert_eq!(
            terminal_contextual_desc(CMD_TERMINAL_CLOSE, "base", false, true, false),
            Cow::Borrowed("base")
        );
        assert_eq!(
            terminal_contextual_desc(CMD_TERMINAL_CLOSE, "base", true, true, false),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn problems_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_REFRESH,
                "base",
                true,
                0,
                false,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("No file-backed tab; refresh clears diagnostics and opens Problems")
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_REFRESH,
                "base",
                false,
                0,
                true,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("Open Problems and refresh diagnostics")
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_CLEAR,
                "base",
                true,
                0,
                true,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("Problems diagnostics already empty")
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_CLOSE,
                "base",
                false,
                2,
                true,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("Problems panel is already closed")
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_CLEAR,
                "base",
                true,
                2,
                true,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_CLOSE,
                "base",
                true,
                0,
                true,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            problems_contextual_desc(
                CMD_PROBLEMS_REFRESH,
                "base",
                true,
                0,
                true,
                crate::langdetect::Language::PlainText
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn search_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_RUN, "base", true, false, true, false, 0),
            Cow::Borrowed("Enter text to search")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_RUN, "base", true, false, false, true, 0),
            Cow::Borrowed("No project search results")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_REPLACE_ALL, "base", true, false, true, false, 0),
            Cow::Borrowed("Enter search text to replace")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_REPLACE_ALL, "base", true, false, false, false, 2),
            Cow::Borrowed("Run Search before replacing")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_REPLACE_ALL, "base", true, false, false, true, 0),
            Cow::Borrowed("Run Search before replacing")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_CLEAR_RESULTS, "base", true, false, false, false, 0),
            Cow::Borrowed("Search results already empty")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_CLOSE, "base", false, false, false, false, 2),
            Cow::Borrowed("Search panel is already closed")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_TOGGLE_REPLACE, "base", true, false, false, false, 0),
            Cow::Borrowed("Focus Search replace field")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_TOGGLE_REPLACE, "base", true, true, false, false, 0),
            Cow::Borrowed("Focus Search query field")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_TOGGLE_REPLACE, "base", false, false, false, false, 0),
            Cow::Borrowed("Open Search and focus replace field")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_TOGGLE_REPLACE, "base", false, true, false, false, 0),
            Cow::Borrowed("Open Search and focus query field")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_RUN, "base", true, false, false, false, 0),
            Cow::Borrowed("base")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_REPLACE_ALL, "base", true, false, false, true, 2),
            Cow::Borrowed("base")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_CLEAR_RESULTS, "base", false, false, false, false, 2),
            Cow::Borrowed("base")
        );
        assert_eq!(
            search_contextual_desc(CMD_SEARCH_CLOSE, "base", true, false, true, false, 0),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn outline_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            outline_contextual_desc(CMD_OUTLINE_REFRESH, "base", false, 0),
            Cow::Borrowed("Open Outline and refresh symbols")
        );
        assert_eq!(
            outline_contextual_desc(CMD_OUTLINE_CLEAR_SYMBOLS, "base", true, 0),
            Cow::Borrowed("Outline symbols already empty")
        );
        assert_eq!(
            outline_contextual_desc(CMD_OUTLINE_CLOSE, "base", false, 2),
            Cow::Borrowed("Outline panel is already closed")
        );
        assert_eq!(
            outline_contextual_desc(CMD_OUTLINE_CLEAR_SYMBOLS, "base", false, 2),
            Cow::Borrowed("base")
        );
        assert_eq!(
            outline_contextual_desc(CMD_OUTLINE_CLOSE, "base", true, 0),
            Cow::Borrowed("base")
        );
        assert_eq!(
            outline_contextual_desc(CMD_OUTLINE_REFRESH, "base", true, 0),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn debug_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STOP,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("No debug session to stop")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_PAUSE,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("Pause is available while running")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_RESTART,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("No debug target to restart")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STEP_OVER,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("Step Over is available when paused")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STEP_INTO,
                "base",
                true,
                crate::dap::DebugState::Running,
                false,
                0,
                true
            ),
            Cow::Borrowed("Step Into is available when paused")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STEP_OUT,
                "base",
                true,
                crate::dap::DebugState::Terminated,
                false,
                0,
                true
            ),
            Cow::Borrowed("Step Out is available when paused")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_CLEAR_BREAKPOINTS,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("No breakpoints to clear")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_CLEAR_SESSION,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("Debug session already empty")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_CLOSE,
                "base",
                false,
                crate::dap::DebugState::Stopped,
                true,
                1,
                false
            ),
            Cow::Borrowed("Run and Debug panel is already closed")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STOP,
                "base",
                true,
                crate::dap::DebugState::Stopped,
                true,
                1,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_PAUSE,
                "base",
                true,
                crate::dap::DebugState::Running,
                true,
                1,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STEP_OVER,
                "base",
                true,
                crate::dap::DebugState::Stopped,
                true,
                1,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STEP_INTO,
                "base",
                true,
                crate::dap::DebugState::Stopped,
                true,
                1,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_STEP_OUT,
                "base",
                true,
                crate::dap::DebugState::Stopped,
                true,
                1,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_RESTART,
                "base",
                true,
                crate::dap::DebugState::Idle,
                true,
                0,
                true
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_CLEAR_BREAKPOINTS,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                1,
                true
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_CLEAR_SESSION,
                "base",
                true,
                crate::dap::DebugState::Stopped,
                true,
                1,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            debug_contextual_desc(
                CMD_DEBUG_CLOSE,
                "base",
                true,
                crate::dap::DebugState::Idle,
                false,
                0,
                true
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn source_control_command_descriptions_reflect_runtime_state() {
        for id in [
            CMD_GIT_SWITCH_BRANCH,
            CMD_GIT_PUSH,
            CMD_GIT_PULL,
            CMD_GIT_FETCH,
        ] {
            assert_eq!(
                source_control_contextual_desc(id, "base", true, false, 0, 0, true),
                Cow::Borrowed("Not a git repository")
            );
        }
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_STAGE_ALL,
                "base",
                true,
                false,
                0,
                0,
                true
            ),
            Cow::Borrowed("Not a git repository")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_REFRESH_SOURCE_CONTROL,
                "base",
                true,
                false,
                0,
                0,
                true
            ),
            Cow::Borrowed("Not a git repository")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_REFRESH_SOURCE_CONTROL,
                "base",
                false,
                true,
                0,
                0,
                true
            ),
            Cow::Borrowed("Open Source Control and refresh git status")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_STAGE_ALL,
                "base",
                true,
                true,
                0,
                0,
                true
            ),
            Cow::Borrowed("Nothing to stage")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_UNSTAGE_ALL,
                "base",
                true,
                true,
                0,
                1,
                true
            ),
            Cow::Borrowed("Nothing to unstage")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_COMMIT_STAGED,
                "base",
                true,
                true,
                0,
                1,
                false
            ),
            Cow::Borrowed("No staged changes to commit")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_COMMIT_STAGED,
                "base",
                true,
                true,
                1,
                0,
                true
            ),
            Cow::Borrowed("Enter a commit message")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_CLEAR_COMMIT_MESSAGE,
                "base",
                true,
                true,
                0,
                0,
                true
            ),
            Cow::Borrowed("Source Control message already empty")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_CLOSE_SOURCE_CONTROL,
                "base",
                false,
                true,
                1,
                1,
                false
            ),
            Cow::Borrowed("Source Control panel is already closed")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_STAGE_ALL,
                "base",
                true,
                true,
                0,
                1,
                true
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_UNSTAGE_ALL,
                "base",
                true,
                true,
                1,
                0,
                true
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_COMMIT_STAGED,
                "base",
                true,
                true,
                1,
                0,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_CLEAR_COMMIT_MESSAGE,
                "base",
                false,
                true,
                0,
                0,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_CLOSE_SOURCE_CONTROL,
                "base",
                true,
                true,
                0,
                0,
                true
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            source_control_contextual_desc(
                CMD_GIT_REFRESH_SOURCE_CONTROL,
                "base",
                true,
                true,
                0,
                0,
                true
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn explorer_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            explorer_contextual_desc(CMD_EXPLORER_CLOSE, "base", false),
            Cow::Borrowed("Explorer panel is already closed")
        );
        assert_eq!(
            explorer_contextual_desc(CMD_EXPLORER_REFRESH, "base", false),
            Cow::Borrowed("Open Explorer and refresh file tree")
        );
        assert_eq!(
            explorer_contextual_desc(CMD_EXPLORER_CLOSE, "base", true),
            Cow::Borrowed("base")
        );
        assert_eq!(
            explorer_contextual_desc(CMD_EXPLORER_REFRESH, "base", true),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn keyboard_shortcuts_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            keyboard_shortcuts_contextual_desc(
                CMD_KEYBOARD_SHORTCUTS_CLOSE,
                "base",
                false,
                false,
                true
            ),
            Cow::Borrowed("Keyboard Shortcuts is already closed")
        );
        assert_eq!(
            keyboard_shortcuts_contextual_desc(
                CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED,
                "base",
                true,
                false,
                false
            ),
            Cow::Borrowed("Keyboard Shortcuts selection already uses default")
        );
        assert_eq!(
            keyboard_shortcuts_contextual_desc(
                CMD_KEYBOARD_SHORTCUTS_RESET_ALL,
                "base",
                true,
                true,
                true
            ),
            Cow::Borrowed("Keyboard Shortcuts already use defaults")
        );
        assert_eq!(
            keyboard_shortcuts_contextual_desc(
                CMD_KEYBOARD_SHORTCUTS_CLOSE,
                "base",
                true,
                false,
                true
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            keyboard_shortcuts_contextual_desc(
                CMD_KEYBOARD_SHORTCUTS_RESET_SELECTED,
                "base",
                true,
                true,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            keyboard_shortcuts_contextual_desc(
                CMD_KEYBOARD_SHORTCUTS_RESET_ALL,
                "base",
                true,
                true,
                false
            ),
            Cow::Borrowed("base")
        );
    }

    fn open_surface_desc_for_flags(id: u32, flags: [bool; 15]) -> Cow<'static, str> {
        open_surface_contextual_desc(
            id, "base", flags[0], flags[1], flags[2], flags[3], flags[4], flags[5], flags[6],
            flags[7], flags[8], flags[9], flags[10], flags[11], flags[12], flags[13],
            flags[14],
        )
    }

    #[test]
    fn open_surface_command_descriptions_reflect_runtime_state() {
        let mut flags = [false; 15];
        flags[0] = true;
        assert_eq!(
            open_surface_desc_for_flags(CMD_VIEW_EXPLORER, flags),
            Cow::Borrowed("Explorer panel is already open")
        );

        let mut flags = [false; 15];
        flags[2] = true;
        assert_eq!(
            open_surface_desc_for_flags(CMD_VIEW_SOURCE_CONTROL, flags),
            Cow::Borrowed("Source Control panel is already open")
        );

        let mut flags = [false; 15];
        flags[4] = true;
        assert_eq!(
            open_surface_desc_for_flags(CMD_VIEW_RUN_DEBUG, flags),
            Cow::Borrowed("Run and Debug panel is already open")
        );

        let mut flags = [false; 15];
        flags[9] = true;
        assert_eq!(
            open_surface_desc_for_flags(CMD_TOGGLE_TERMINAL, flags),
            Cow::Borrowed("Focus integrated terminal")
        );

        let mut flags = [false; 15];
        flags[13] = true;
        assert_eq!(
            open_surface_desc_for_flags(CMD_KEYBOARD_SHORTCUTS, flags),
            Cow::Borrowed("Keyboard Shortcuts is already open")
        );

        assert_eq!(
            open_surface_desc_for_flags(CMD_VIEW_WEB_PLAYGROUND, [false; 15]),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn markdown_preview_command_description_matches_runtime_state() {
        assert_eq!(
            markdown_preview_contextual_desc("base", crate::langdetect::Language::Mighty, false),
            Cow::Borrowed("Markdown preview is available for Markdown files")
        );
        assert_eq!(
            markdown_preview_contextual_desc("base", crate::langdetect::Language::Mighty, true),
            Cow::Borrowed("Markdown preview is available for Markdown files")
        );
        assert_eq!(
            markdown_preview_contextual_desc("base", crate::langdetect::Language::Markdown, true),
            Cow::Borrowed("Markdown preview is already open")
        );
        assert_eq!(
            markdown_preview_contextual_desc("base", crate::langdetect::Language::Markdown, false),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn close_action_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            close_action_contextual_desc(CMD_GIT_HIDE_BLAME, "base", false, true),
            Cow::Borrowed("Blame is already hidden")
        );
        assert_eq!(
            close_action_contextual_desc(CMD_WELCOME_CLOSE, "base", true, false),
            Cow::Borrowed("Welcome is already closed")
        );
        assert_eq!(
            close_action_contextual_desc(CMD_GIT_HIDE_BLAME, "base", true, false),
            Cow::Borrowed("base")
        );
        assert_eq!(
            close_action_contextual_desc(CMD_WELCOME_CLOSE, "base", false, true),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn blame_toggle_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            blame_toggle_contextual_desc("base", false, false),
            Cow::Borrowed("No file to blame: (scratch)")
        );
        assert_eq!(
            blame_toggle_contextual_desc("base", true, false),
            Cow::Borrowed("Hide git blame gutter")
        );
        assert_eq!(
            blame_toggle_contextual_desc("base", false, true),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn fold_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            fold_contextual_desc(CMD_FOLD_TOGGLE, "base", false, true, true, false),
            Cow::Borrowed("No foldable block at cursor")
        );
        assert_eq!(
            fold_contextual_desc(CMD_FOLD_ALL, "base", false, true, true, false),
            Cow::Borrowed("No foldable blocks")
        );
        assert_eq!(
            fold_contextual_desc(CMD_UNFOLD_ALL, "base", false, true, true, false),
            Cow::Borrowed("No foldable blocks")
        );
        assert_eq!(
            fold_contextual_desc(CMD_FOLD_ALL, "base", true, false, true, true),
            Cow::Borrowed("All foldable blocks already folded")
        );
        assert_eq!(
            fold_contextual_desc(CMD_UNFOLD_ALL, "base", true, false, false, false),
            Cow::Borrowed("No folded blocks to unfold")
        );
        assert_eq!(
            fold_contextual_desc(CMD_FOLD_TOGGLE, "base", true, false, false, false),
            Cow::Borrowed("base")
        );
        assert_eq!(
            fold_contextual_desc(CMD_FOLD_ALL, "base", true, false, false, false),
            Cow::Borrowed("base")
        );
        assert_eq!(
            fold_contextual_desc(CMD_UNFOLD_ALL, "base", true, false, false, true),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn layout_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            layout_command_contextual_desc(CMD_TOGGLE_SIDEBAR, "base", true, 0, false, 1),
            Cow::Borrowed("Hide the left sidebar")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_TOGGLE_SIDEBAR, "base", false, 0, false, 1),
            Cow::Borrowed("Show the left sidebar")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_COMPACT, "base", true, 1, false, 1),
            Cow::Borrowed("Sidebar is already compact")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_COMPACT, "base", true, 0, false, 1),
            Cow::Borrowed("Set sidebar to compact width")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_DEFAULT, "base", false, 2, false, 1),
            Cow::Borrowed("Open sidebar at default width")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_WIDE, "base", true, 2, false, 1),
            Cow::Borrowed("Sidebar is already wide")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_CYCLE_WIDTH, "base", true, 0, false, 1),
            Cow::Borrowed("Cycle sidebar to compact width")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_CYCLE_WIDTH, "base", false, 0, false, 1),
            Cow::Borrowed("Open sidebar at compact width")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_CYCLE_WIDTH, "base", true, 1, false, 1),
            Cow::Borrowed("Cycle sidebar to wide width")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_SIDEBAR_CYCLE_WIDTH, "base", true, 2, false, 1),
            Cow::Borrowed("Cycle sidebar to default width")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_DOCK_COMPACT, "base", true, 0, true, 0),
            Cow::Borrowed("Bottom dock is already compact")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_DOCK_COMPACT, "base", true, 0, true, 1),
            Cow::Borrowed("Set bottom dock to compact height")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_DOCK_RESET, "base", true, 0, false, 2),
            Cow::Borrowed("Open bottom dock at default height")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_DOCK_EXPANDED, "base", true, 0, true, 2),
            Cow::Borrowed("Bottom dock is already expanded")
        );
        assert_eq!(
            layout_command_contextual_desc(CMD_DOCK_EXPANDED, "base", true, 0, true, 0),
            Cow::Borrowed("Set bottom dock to expanded height")
        );
    }

    #[test]
    fn close_surface_command_descriptions_reflect_runtime_state() {
        assert_eq!(
            close_surface_contextual_desc(
                CMD_SETTINGS_CLOSE,
                "base",
                false,
                true,
                true,
                true
            ),
            Cow::Borrowed("Settings panel is already closed")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_DIFF_CLOSE_VIEW,
                "base",
                true,
                false,
                true,
                true
            ),
            Cow::Borrowed("Diff view is already closed")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_PEEK_CLOSE,
                "base",
                true,
                true,
                false,
                true
            ),
            Cow::Borrowed("Peek view is already closed")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_MARKDOWN_CLOSE_PREVIEW,
                "base",
                true,
                true,
                true,
                false
            ),
            Cow::Borrowed("Markdown preview is already closed")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_SETTINGS_CLOSE,
                "base",
                true,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_DIFF_CLOSE_VIEW,
                "base",
                false,
                true,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_PEEK_CLOSE,
                "base",
                false,
                false,
                true,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            close_surface_contextual_desc(
                CMD_MARKDOWN_CLOSE_PREVIEW,
                "base",
                false,
                false,
                false,
                true
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn transient_surface_command_descriptions_reflect_runtime_state() {
        let cases = [
            (CMD_AI_CLOSE, "AI Copilot is already closed"),
            (CMD_SIDEBAR_CLOSE, "Sidebar is already closed"),
            (CMD_COLOR_THEME_CLOSE, "No color theme picker open"),
            (CMD_HOVER_CLOSE, "No hover popup open"),
            (
                CMD_SIGNATURE_HELP_CLOSE,
                "No signature help popup open",
            ),
            (CMD_CODE_ACTIONS_CLOSE, "No code action menu open"),
            (CMD_FIND_REPLACE_CLOSE, "No Find & Replace bar open"),
            (
                CMD_AUTOCOMPLETE_CLOSE,
                "No autocomplete suggestions open",
            ),
            (CMD_COMMAND_PALETTE_CLOSE, "No command palette open"),
            (CMD_QUICK_OPEN_CLOSE, "No Quick Open panel open"),
        ];
        for (id, expected) in cases {
            assert_eq!(
                transient_surface_contextual_desc(
                    id, "base", false, false, false, false, false, false, false, false, false,
                    false
                ),
                Cow::Borrowed(expected)
            );
        }

        assert_eq!(
            transient_surface_contextual_desc(
                CMD_AI_CLOSE,
                "base",
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_SIDEBAR_CLOSE,
                "base",
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_COLOR_THEME_CLOSE,
                "base",
                false,
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_HOVER_CLOSE,
                "base",
                false,
                false,
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_SIGNATURE_HELP_CLOSE,
                "base",
                false,
                false,
                false,
                false,
                true,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_CODE_ACTIONS_CLOSE,
                "base",
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_FIND_REPLACE_CLOSE,
                "base",
                false,
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_AUTOCOMPLETE_CLOSE,
                "base",
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_COMMAND_PALETTE_CLOSE,
                "base",
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                true,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            transient_surface_contextual_desc(
                CMD_QUICK_OPEN_CLOSE,
                "base",
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                true
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn utility_command_descriptions_reflect_runtime_state() {
        let cases = [
            (CMD_CLEAR_NOTIFICATIONS, "No notifications to clear"),
            (CMD_REOPEN_CLOSED_TAB, "No closed tab to reopen"),
            (CMD_DOCK_CLOSE, "No bottom dock is open"),
            (CMD_PROMPT_CANCEL, "No prompt input open"),
            (
                CMD_DIRTY_CONFIRM_CANCEL,
                "No unsaved changes confirmation open",
            ),
            (CMD_GIT_BRANCH_CANCEL, "No branch picker open"),
            (CMD_BREADCRUMB_MENU_CANCEL, "No breadcrumb menu open"),
            (CMD_SNIPPET_CANCEL, "No snippet session active"),
        ];
        for (id, expected) in cases {
            assert_eq!(
                utility_command_contextual_desc(
                    id, "base", false, false, false, false, false, false, false, false
                ),
                Cow::Borrowed(expected)
            );
        }

        assert_eq!(
            utility_command_contextual_desc(
                CMD_CLEAR_NOTIFICATIONS,
                "base",
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_REOPEN_CLOSED_TAB,
                "base",
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_DOCK_CLOSE,
                "base",
                false,
                false,
                true,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_PROMPT_CANCEL,
                "base",
                false,
                false,
                false,
                true,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_DIRTY_CONFIRM_CANCEL,
                "base",
                false,
                false,
                false,
                false,
                true,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_GIT_BRANCH_CANCEL,
                "base",
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_BREADCRUMB_MENU_CANCEL,
                "base",
                false,
                false,
                false,
                false,
                false,
                false,
                true,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            utility_command_contextual_desc(
                CMD_SNIPPET_CANCEL,
                "base",
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                true
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn tab_management_command_descriptions_reflect_runtime_state() {
        let cases = [
            (CMD_MOVE_ACTIVE_TAB_LEFT, "Tab is already first"),
            (CMD_MOVE_ACTIVE_TAB_RIGHT, "Tab is already last"),
            (CMD_SORT_TABS_BY_NAME, "Tabs already sorted"),
            (CMD_CLOSE_DUPLICATE_TABS, "No duplicate file tabs"),
            (CMD_CLOSE_SAVED_TABS, "No saved tabs to close"),
            (CMD_CLOSE_OTHER_SAVED_TABS, "No other saved tabs to close"),
            (CMD_CLOSE_SAVED_TABS_TO_RIGHT, "No saved tabs to the right"),
            (CMD_CLOSE_SAVED_TABS_TO_LEFT, "No saved tabs to the left"),
        ];
        for (id, expected) in cases {
            assert_eq!(
                tab_management_contextual_desc(
                    id, "base", true, true, true, false, false, false, false, false
                ),
                Cow::Borrowed(expected)
            );
        }

        assert_eq!(
            tab_management_contextual_desc(
                CMD_MOVE_ACTIVE_TAB_LEFT,
                "base",
                false,
                true,
                true,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_MOVE_ACTIVE_TAB_RIGHT,
                "base",
                true,
                false,
                true,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_SORT_TABS_BY_NAME,
                "base",
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_CLOSE_DUPLICATE_TABS,
                "base",
                true,
                true,
                true,
                true,
                false,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_CLOSE_SAVED_TABS,
                "base",
                true,
                true,
                true,
                false,
                true,
                false,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_CLOSE_OTHER_SAVED_TABS,
                "base",
                true,
                true,
                true,
                false,
                false,
                true,
                false,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_CLOSE_SAVED_TABS_TO_RIGHT,
                "base",
                true,
                true,
                true,
                false,
                false,
                false,
                true,
                false
            ),
            Cow::Borrowed("base")
        );
        assert_eq!(
            tab_management_contextual_desc(
                CMD_CLOSE_SAVED_TABS_TO_LEFT,
                "base",
                true,
                true,
                true,
                false,
                false,
                false,
                false,
                true
            ),
            Cow::Borrowed("base")
        );
    }

    #[test]
    fn active_file_edit_descriptions_report_stale_targets() {
        let root = std::env::temp_dir().join(format!(
            "mui_palette_active_edit_stale_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let missing = root.join("old.mty");
        assert_eq!(
            active_file_edit_stale_target_desc(CMD_RENAME_ACTIVE_FILE, &missing),
            Some("Rename source missing: old.mty".to_string())
        );
        assert_eq!(
            active_file_edit_stale_target_desc(CMD_DELETE_ACTIVE_FILE, &missing),
            Some("Delete target missing: old.mty".to_string())
        );

        let blocked = root.join("blocked.mty");
        std::fs::create_dir_all(&blocked).unwrap();
        assert_eq!(
            active_file_edit_stale_target_desc(CMD_RENAME_ACTIVE_FILE, &blocked),
            Some("Rename failed: blocked.mty: not a file".to_string())
        );
        assert_eq!(
            active_file_edit_stale_target_desc(CMD_DELETE_ACTIVE_FILE, &blocked),
            Some("Delete failed: blocked.mty: not a file".to_string())
        );

        let file = root.join("ok.mty");
        std::fs::write(&file, "ok").unwrap();
        assert_eq!(
            active_file_edit_stale_target_desc(CMD_RENAME_ACTIVE_FILE, &file),
            None
        );
        assert_eq!(
            active_file_edit_stale_target_desc(CMD_REVEAL_ACTIVE_FILE, &missing),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blame_descriptions_report_stale_targets() {
        let root = std::env::temp_dir().join(format!(
            "mui_palette_blame_stale_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let missing = root.join("gone.mty");
        assert_eq!(
            blame_stale_target_desc(&missing),
            Some("Blame target missing: gone.mty".to_string())
        );

        let blocked = root.join("blocked.mty");
        std::fs::create_dir_all(&blocked).unwrap();
        assert_eq!(
            blame_stale_target_desc(&blocked),
            Some("Blame failed: blocked.mty: not a file".to_string())
        );

        let file = root.join("ok.mty");
        std::fs::write(&file, "ok").unwrap();
        assert_eq!(blame_stale_target_desc(&file), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reload_revert_descriptions_report_stale_targets() {
        let root = std::env::temp_dir().join(format!(
            "mui_palette_reload_revert_stale_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let missing = root.join("gone.mty");
        let reload_missing = reload_revert_stale_target_desc(CMD_RELOAD_ACTIVE_FILE, &missing)
            .expect("missing reload target should report");
        assert!(
            reload_missing.starts_with("Reload failed: gone.mty: "),
            "{reload_missing}"
        );

        let blocked = root.join("blocked.mty");
        std::fs::create_dir_all(&blocked).unwrap();
        assert_eq!(
            reload_revert_stale_target_desc(CMD_REVERT_ACTIVE_FILE, &blocked),
            Some("Revert failed: blocked.mty: not a file".to_string())
        );

        let file = root.join("ok.mty");
        std::fs::write(&file, "ok").unwrap();
        assert_eq!(
            reload_revert_stale_target_desc(CMD_RELOAD_ACTIVE_FILE, &file),
            None
        );
        assert_eq!(reload_revert_stale_target_desc(CMD_SAVE, &missing), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn active_file_utility_descriptions_report_stale_targets() {
        let root = std::env::temp_dir().join(format!(
            "mui_palette_active_utility_stale_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let missing = root.join("gone.mty");
        assert_eq!(
            active_file_utility_stale_target_desc(CMD_REVEAL_ACTIVE_FILE, &missing),
            Some("Reveal target missing: gone.mty".to_string())
        );
        assert_eq!(
            active_file_utility_stale_target_desc(CMD_COPY_ACTIVE_FILE_RELATIVE_PATH, &missing),
            Some("Copy target missing: gone.mty".to_string())
        );

        let blocked = root.join("blocked.mty");
        std::fs::create_dir_all(&blocked).unwrap();
        assert_eq!(
            active_file_utility_stale_target_desc(CMD_REVEAL_ACTIVE_FILE_IN_OS, &blocked),
            Some("Reveal target is not a file: blocked.mty".to_string())
        );
        assert_eq!(
            active_file_utility_stale_target_desc(CMD_COPY_ACTIVE_FILE_DIRECTORY, &blocked),
            Some("Copy target is not a file: blocked.mty".to_string())
        );

        let file = root.join("ok.mty");
        std::fs::write(&file, "ok").unwrap();
        assert_eq!(
            active_file_utility_stale_target_desc(CMD_COPY_ACTIVE_FILE_NAME, &file),
            None
        );
        assert_eq!(
            active_file_utility_stale_target_desc(CMD_RELOAD_ACTIVE_FILE, &missing),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_all_description_reports_dirty_count() {
        assert_eq!(
            command_contextual_desc(CMD_SAVE_ALL, "base", true, false, false, 0, false, false),
            Cow::Borrowed("No unsaved tabs need writing")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE_ALL, "base", true, false, true, 1, false, false),
            Cow::Borrowed("Write the one unsaved tab")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE_ALL, "base", true, false, true, 3, false, false)
                .as_ref(),
            "Write 3 unsaved tabs"
        );
    }
}
