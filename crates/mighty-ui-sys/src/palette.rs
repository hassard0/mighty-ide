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
//! each command's label, ranked so prefix matches sort ahead of looser fuzzy
//! matches. An empty query lists every command in registry order.

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
/// Set sidebar drawers to a compact width.
pub const CMD_SIDEBAR_COMPACT: u32 = 94;
/// Restore sidebar drawers to the responsive default width.
pub const CMD_SIDEBAR_DEFAULT: u32 = 95;
/// Set sidebar drawers to a wider review/debug width.
pub const CMD_SIDEBAR_WIDE: u32 = 96;
/// Cycle sidebar drawers through compact, default, and wide widths.
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
    Command { id: CMD_NEW_WORKSPACE_FILE, label: "Explorer: New File in Workspace...", keybinding: "" },
    Command { id: CMD_NEW_FOLDER,       label: "Explorer: New Folder...",   keybinding: "Ctrl+Shift+N" },
    Command { id: CMD_OPEN_FILE,        label: "File: Open File...", keybinding: "Ctrl+O" },
    Command { id: CMD_SAVE,             label: "File: Save",         keybinding: "Ctrl+S" },
    Command { id: CMD_SAVE_AS,          label: "File: Save As...",   keybinding: "Ctrl+Shift+S" },
    Command { id: CMD_SAVE_ALL,         label: "File: Save All",     keybinding: "Ctrl+Alt+S" },
    Command { id: CMD_RENAME_ACTIVE_FILE, label: "File: Rename Active File", keybinding: "" },
    Command { id: CMD_REVEAL_ACTIVE_FILE, label: "File: Reveal Active File in File Tree", keybinding: "" },
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
    Command { id: CMD_GOTO_LINE,        label: "Go to Line",         keybinding: "Ctrl+G" },
    Command { id: CMD_GOTO_DEFINITION,  label: "Go to Definition",   keybinding: "F12" },
    Command { id: CMD_HOVER,            label: "Show Hover",         keybinding: "Ctrl+K" },
    Command { id: CMD_SIGNATURE_HELP,   label: "Show Signature Help", keybinding: "Ctrl+Shift+Space" },
    Command { id: CMD_RENAME_SYMBOL,    label: "Rename Symbol",      keybinding: "F2" },
    Command { id: CMD_CODE_ACTIONS,     label: "Code Actions",       keybinding: "Ctrl+." },
    Command { id: CMD_TOGGLE_TERMINAL,  label: "Toggle Terminal",    keybinding: "Ctrl+`" },
    Command { id: CMD_TOGGLE_SIDEBAR,   label: "Toggle Sidebar",     keybinding: "Ctrl+B" },
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
    Command { id: CMD_JUMP_BACK,        label: "Jump Back",          keybinding: "Ctrl+-" },
    Command { id: CMD_QUIT,             label: "Quit",               keybinding: "Esc / close" },
    Command { id: CMD_COLOR_THEME,      label: "Preferences: Color Theme", keybinding: "" },
    Command { id: CMD_RUN_FILE,         label: "Run File",           keybinding: "Ctrl+Shift+R" },
    Command { id: CMD_SETTINGS,         label: "Preferences: Settings", keybinding: "Ctrl+," },
    Command { id: CMD_RUN_TESTS,        label: "Run Tests",          keybinding: "Ctrl+Shift+T" },
    Command { id: CMD_PEEK_DEFINITION,  label: "Peek Definition",    keybinding: "Alt+F12" },
    Command { id: CMD_WELCOME,          label: "Welcome",            keybinding: "" },
    Command { id: CMD_ZEN_MODE,         label: "Toggle Zen Mode",    keybinding: "Alt+Z" },
    Command { id: CMD_AGENTS,           label: "Mighty: Agents",     keybinding: "Alt+G" },
    Command { id: CMD_GIT_SWITCH_BRANCH, label: "Git: Switch Branch", keybinding: "" },
    Command { id: CMD_GIT_PUSH,         label: "Git: Push",          keybinding: "" },
    Command { id: CMD_GIT_PULL,         label: "Git: Pull",          keybinding: "" },
    Command { id: CMD_GIT_FETCH,        label: "Git: Fetch",         keybinding: "" },
    Command { id: CMD_GIT_TOGGLE_BLAME, label: "Git: Toggle Blame",  keybinding: "Alt+B" },
    Command { id: CMD_GIT_STAGE_ALL,    label: "Git: Stage All",     keybinding: "" },
    Command { id: CMD_GIT_UNSTAGE_ALL,  label: "Git: Unstage All",   keybinding: "" },
    Command { id: CMD_GIT_COMMIT_STAGED, label: "Git: Commit Staged", keybinding: "" },
    Command { id: CMD_VIEW_EXPLORER,    label: "View: Explorer",      keybinding: "" },
    Command { id: CMD_VIEW_SEARCH,      label: "View: Search",        keybinding: "Ctrl+Shift+F" },
    Command { id: CMD_VIEW_SOURCE_CONTROL, label: "View: Source Control", keybinding: "Ctrl+Shift+G" },
    Command { id: CMD_VIEW_OUTLINE,     label: "View: Outline",       keybinding: "" },
    Command { id: CMD_VIEW_RUN_DEBUG,   label: "View: Run and Debug", keybinding: "" },
    Command { id: CMD_VIEW_TESTING,     label: "View: Testing",       keybinding: "" },
    Command { id: CMD_VIEW_RUN_OUTPUT,  label: "View: Run Output",    keybinding: "" },
    Command { id: CMD_VIEW_PROBLEMS,    label: "View: Problems",      keybinding: "" },
    Command { id: CMD_VIEW_AI_COPILOT,  label: "View: AI Copilot",    keybinding: "Ctrl+Shift+A" },
    Command { id: CMD_INLINE_AI_ASK,    label: "AI: Inline Ask",      keybinding: "Ctrl+I" },
    Command { id: CMD_FORCE_GHOST_COMPLETION, label: "AI: Force Ghost Completion", keybinding: "Alt+\\" },
    Command { id: CMD_AI_CLOSE,         label: "View: Close AI Copilot", keybinding: "" },
    Command { id: CMD_SIDEBAR_CLOSE,    label: "View: Close Sidebar", keybinding: "" },
    Command { id: CMD_VIEW_TERMINAL,    label: "View: Terminal",      keybinding: "Ctrl+`" },
    Command { id: CMD_VIEW_WEB_PLAYGROUND, label: "View: Web Playground", keybinding: "" },
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
    Command { id: CMD_RUN_IN_BROWSER,   label: "Mighty: Run in Browser", keybinding: "Alt+W" },
    Command { id: CMD_SPLIT_RIGHT,      label: "Split Editor Right", keybinding: "Ctrl+\\" },
    Command { id: CMD_FOCUS_NEXT_PANE,  label: "Focus Next Editor Pane", keybinding: "Ctrl+1 / Ctrl+2" },
    Command { id: CMD_CLOSE_PANE,       label: "Close Editor Pane",  keybinding: "" },
    Command { id: CMD_MARKDOWN_PREVIEW, label: "Markdown: Open Preview", keybinding: "Ctrl+Shift+V" },
    Command { id: CMD_OPEN_FOLDER,      label: "File: Open Folder...", keybinding: "Ctrl+Shift+O" },
    Command { id: CMD_OPEN_RECENT,      label: "File: Open Recent",   keybinding: "" },
    Command { id: CMD_KEYBOARD_SHORTCUTS, label: "Help: Keyboard Shortcuts", keybinding: "Ctrl+Shift+/" },
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

fn shortcut_token_width(token: &ShortcutToken, kadv: f32, pill_pad: f32) -> f32 {
    match token {
        ShortcutToken::Key(part) => (part.chars().count() as f32 * kadv + 2.0 * pill_pad).max(22.0),
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
        let box_w = 600.0_f32.min(w - 80.0);
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
            CMD_GOTO_LINE => (icons::CHEVRON, "Jump to a specific line number", false),
            CMD_GOTO_DEFINITION => (icons::FN_SYMBOL, "Navigate to the symbol definition", false),
            CMD_HOVER => (icons::INFO_I, "Show type & docs at the cursor", false),
            CMD_SIGNATURE_HELP => (icons::INFO_I, "Show callable parameters at the cursor", false),
            CMD_RENAME_SYMBOL => (icons::FN_SYMBOL, "Rename the symbol under the cursor", false),
            CMD_CODE_ACTIONS => (icons::LIGHTBULB, "Show quick fixes and code actions at the cursor", false),
            CMD_TOGGLE_TERMINAL => (icons::TEST_BOX, "Open the integrated terminal", false),
            CMD_TOGGLE_SIDEBAR => (icons::EXPLORER, "Show or hide the file explorer", false),
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
            CMD_JUMP_BACK => (icons::CHEVRON, "Return to the previous location", false),
            CMD_QUIT => (icons::CLOSE, "Close the editor", false),
            CMD_COLOR_THEME => (icons::SETTINGS, "Switch the editor color theme", false),
            CMD_RUN_FILE => (icons::RUN, "Run the active Mighty file", true),
            CMD_SETTINGS => (icons::SETTINGS, "Edit editor preferences", false),
            CMD_RUN_TESTS => (icons::BEAKER, "Run the package's tests (mty test)", false),
            CMD_PEEK_DEFINITION => (icons::FN_SYMBOL, "Preview the definition inline (Alt+F12)", false),
            CMD_WELCOME => (icons::LANG_M, "Open the Welcome screen", false),
            CMD_ZEN_MODE => (icons::INFO_I, "Toggle distraction-free focus mode", false),
            CMD_AGENTS => (icons::AGENTS_NET, "Open the Mighty Agents topology panel", false),
            CMD_GIT_SWITCH_BRANCH => (icons::BRANCH, "Checkout or create a git branch", false),
            CMD_GIT_PUSH => (icons::GIT, "Push commits to the remote", false),
            CMD_GIT_PULL => (icons::GIT, "Pull (fast-forward only) from the remote", false),
            CMD_GIT_FETCH => (icons::GIT, "Fetch refs from the remote", false),
            CMD_GIT_TOGGLE_BLAME => (icons::GIT, "Show git blame in the gutter", false),
            CMD_GIT_STAGE_ALL => (icons::STAGE_PLUS, "Stage every changed path", false),
            CMD_GIT_UNSTAGE_ALL => (icons::UNSTAGE_MINUS, "Unstage every staged path", false),
            CMD_GIT_COMMIT_STAGED => (icons::GIT, "Commit staged changes with the SCM message", false),
            CMD_VIEW_EXPLORER => (icons::EXPLORER, "Open the file explorer view", false),
            CMD_VIEW_SEARCH => (icons::SEARCH, "Open project-wide search", false),
            CMD_VIEW_SOURCE_CONTROL => (icons::GIT, "Open source control", false),
            CMD_VIEW_OUTLINE => (icons::FN_SYMBOL, "Open the symbol outline", false),
            CMD_VIEW_RUN_DEBUG => (icons::DEBUG, "Open Run and Debug", false),
            CMD_VIEW_TESTING => (icons::BEAKER, "Open the testing view", false),
            CMD_VIEW_RUN_OUTPUT => (icons::RUN, "Open the Run output panel", false),
            CMD_VIEW_PROBLEMS => (icons::ERROR_CIRCLE, "Open diagnostics and build problems", false),
            CMD_VIEW_AI_COPILOT => (icons::AGENTS, "Open the AI copilot panel", false),
            CMD_INLINE_AI_ASK => (icons::AGENTS, "Ask AI about the active selection or file", false),
            CMD_FORCE_GHOST_COMPLETION => (icons::AGENTS, "Request an inline AI ghost completion now", false),
            CMD_AI_CLOSE => (icons::CLOSE, "Close the AI copilot panel", false),
            CMD_SIDEBAR_CLOSE => (icons::CLOSE, "Close the left sidebar drawer", false),
            CMD_VIEW_TERMINAL => (icons::TEST_BOX, "Open the integrated terminal", false),
            CMD_VIEW_WEB_PLAYGROUND => (icons::GLOBE, "Open the Web Playground output panel", false),
            CMD_DOCK_COMPACT => (icons::ARROW_DOWN, "Use a smaller shared bottom dock", false),
            CMD_DOCK_RESET => (icons::WIN_MIN, "Restore the shared bottom dock to its default height", false),
            CMD_DOCK_EXPANDED => (icons::ARROW_UP, "Use a taller shared bottom dock", false),
            CMD_DOCK_CLOSE => (icons::CLOSE, "Close the active shared bottom dock", false),
            CMD_SIDEBAR_COMPACT => (icons::ARROW_LEFT, "Use a smaller sidebar drawer", false),
            CMD_SIDEBAR_DEFAULT => (icons::EXPLORER, "Restore responsive sidebar width", false),
            CMD_SIDEBAR_WIDE => (icons::ARROW_RIGHT, "Use a wider sidebar drawer", false),
            CMD_SIDEBAR_CYCLE_WIDTH => (icons::EXPLORER, "Cycle sidebar width through compact, default, and wide", false),
            CMD_WINDOW_TOGGLE_MAXIMIZE => (icons::WIN_MAX, "Maximize or restore the IDE window", false),
            CMD_WINDOW_MINIMIZE => (icons::WIN_MIN, "Minimize the IDE window", false),
            CMD_DEBUG_START_CONTINUE => (icons::DBG_CONTINUE, "Start debugging or continue the paused session", true),
            CMD_DEBUG_STOP => (icons::DBG_STOP, "Stop the active debug session", false),
            CMD_DEBUG_STEP_OVER => (icons::DBG_STEP_OVER, "Run the next line without entering calls", false),
            CMD_DEBUG_STEP_INTO => (icons::DBG_STEP_INTO, "Enter the next function call", false),
            CMD_DEBUG_STEP_OUT => (icons::DBG_STEP_OUT, "Run until the current frame returns", false),
            CMD_DEBUG_PAUSE => (icons::DBG_PAUSE, "Pause the running debuggee", true),
            CMD_DEBUG_RESTART => (icons::REFRESH, "Restart the last debug target", false),
            CMD_RUN_IN_BROWSER => (icons::GLOBE, "Build and serve the active Mighty file for the browser", false),
            CMD_SPLIT_RIGHT => (icons::TEST_BOX, "Split the editor into side-by-side panes", false),
            CMD_FOCUS_NEXT_PANE => (icons::CHEVRON, "Move focus between editor panes", false),
            CMD_CLOSE_PANE => (icons::CLOSE, "Close the focused editor pane", false),
            CMD_MARKDOWN_PREVIEW => (icons::FILE_MD, "Open or close the live Markdown preview", false),
            CMD_OPEN_FOLDER => (icons::FOLDER, "Open a workspace folder with the native folder picker", false),
            CMD_OPEN_RECENT => (icons::FOLDER, "Open a recent file or workspace folder", false),
            CMD_KEYBOARD_SHORTCUTS => (icons::INFO_I, "List & remap all keyboard shortcuts", false),
            CMD_FOLD_TOGGLE => (icons::CHEVRON, "Fold or unfold the block at the cursor", false),
            CMD_FOLD_ALL => (icons::CHEVRON_DOWN, "Fold every foldable block in the document", false),
            CMD_UNFOLD_ALL => (icons::CHEVRON_DOWN, "Unfold every block in the document", false),
            CMD_NEW_PROJECT => (icons::NEW_FOLDER, "Scaffold a new Mighty project (mty new)", false),
            _ => (icons::CHEVRON, "", false),
        }
    }

    fn contextual_desc<'a>(&self, ctx: &crate::MuiContext, id: u32, base: &'a str) -> Cow<'a, str> {
        command_contextual_desc(
            id,
            base,
            ctx.tabs.active_has_path(),
            ctx.tabs.active_read_only(),
            ctx.tabs.dirty_count(),
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
        let (q_str, q_color): (&str, _) = if self.query.is_empty() {
            ("Type a command\u{2026}", theme::OVERLAY_SUBTLE())
        } else {
            (self.query.as_str(), theme::TEXT())
        };
        // Search font is larger (16px) per the mockup.
        ctx.text.queue_ui_sized(q_text_x, qy, q_str, q_color, 16.0, clip);
        let qadv = 16.0 * 0.52;
        let caret_x = q_text_base_x + self.query.chars().count() as f32 * qadv + 1.0;
        ctx.dl_round(caret_x, box_y + (search_h - 18.0) * 0.5, 2.0, 18.0, 1.0, theme::ACCENT_BRIGHT());
        // Command-mode pill (right). ASCII ">_" prompt motif (the UI font lacks the
        // Mac command glyph, which also rendered as a box on Windows).
        let pill_w = 40.0;
        let pill_x = box_x + box_w - pill_w - 18.0;
        let pill_y = box_y + (search_h - 22.0) * 0.5;
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
            let pill_pad = 7.0;
            let gap = 4.0;
            let kadv = 11.0 * 0.55;
            let parts = keybinding_tokens(cmd.keybinding);
            let widths: Vec<f32> = parts
                .iter()
                .map(|p| shortcut_token_width(p, kadv, pill_pad))
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
                let lbl_w = part.chars().count() as f32 * kadv;
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
            let kw = (key.chars().count() as f32 * 6.0 + 10.0).max(20.0);
            ctx.dl_round(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BG_1());
            ctx.dl_stroke(*fx, foot_y + (foot_h - 18.0) * 0.5, kw, 18.0, 4.0, theme::BORDER_STRONG(), 1.0);
            ctx.text.queue_ui_sized(*fx + 5.0, foot_y + (foot_h - 10.0) * 0.5, key, theme::TEXT_1(), 10.0, clip);
            *fx += kw + 6.0;
            ctx.text.queue_ui_sized(*fx, fty, label, theme::OVERLAY_SUBTLE(), 11.0, clip);
            *fx += label.chars().count() as f32 * 6.0 + 16.0;
        };
        foot_seg(ctx, "Enter", "select", &mut fx);
        foot_seg(ctx, "esc", "dismiss", &mut fx);
        let tag = "Mighty Command Palette";
        ctx.text.queue_ui_sized(box_x + box_w - 18.0 - tag.chars().count() as f32 * 6.3, fty, tag, theme::ACCENT_BRIGHT(), 11.0, clip);
    }
}

fn command_field_text_x(base_x: f32, is_placeholder: bool) -> f32 {
    if is_placeholder {
        base_x + 10.0
    } else {
        base_x
    }
}

fn command_contextual_desc<'a>(
    id: u32,
    base: &'a str,
    active_has_path: bool,
    active_read_only: bool,
    dirty_count: usize,
) -> Cow<'a, str> {
    if active_read_only {
        return match id {
            CMD_SAVE | CMD_SAVE_AS | CMD_REVERT_ACTIVE_FILE => Cow::Borrowed("Read-only preview: saving is unavailable"),
            CMD_RELOAD_ACTIVE_FILE => Cow::Borrowed("Reload this read-only preview from disk"),
            CMD_RENAME_ACTIVE_FILE | CMD_DELETE_ACTIVE_FILE => Cow::Borrowed("Read-only preview: file edits are unavailable"),
            _ => Cow::Borrowed(base),
        };
    }

    match id {
        CMD_SAVE if active_has_path => Cow::Borrowed("Write the active file to disk"),
        CMD_SAVE => Cow::Borrowed("Choose a path before saving this untitled file"),
        CMD_SAVE_AS if active_has_path => Cow::Borrowed("Choose a new path or filename for this file"),
        CMD_SAVE_AS => Cow::Borrowed("Choose where this untitled file should live"),
        CMD_SAVE_ALL if dirty_count == 0 => Cow::Borrowed("No unsaved tabs need writing"),
        CMD_SAVE_ALL if dirty_count == 1 => Cow::Borrowed("Write the one unsaved tab"),
        CMD_SAVE_ALL => Cow::Owned(format!("Write {dirty_count} unsaved tabs")),
        CMD_RELOAD_ACTIVE_FILE if active_has_path => Cow::Borrowed("Reload the active file from disk"),
        CMD_RELOAD_ACTIVE_FILE => Cow::Borrowed("Needs a file-backed tab"),
        CMD_REVERT_ACTIVE_FILE if active_has_path => Cow::Borrowed("Discard local edits and reload from disk"),
        CMD_REVERT_ACTIVE_FILE => Cow::Borrowed("Needs a file-backed tab"),
        CMD_RENAME_ACTIVE_FILE if active_has_path => Cow::Borrowed("Rename the active file on disk"),
        CMD_RENAME_ACTIVE_FILE => Cow::Borrowed("Save this untitled file before renaming it"),
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
        assert_eq!(workspace.label, "Explorer: New File in Workspace...");
        assert_eq!(workspace.keybinding, "");
    }

    #[test]
    fn dialog_commands_use_standard_ellipsis_labels() {
        for (id, expected) in [
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
        // "term" is a substring of "Toggle Terminal".
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
        // "ta": "Toggle Terminal"/"Toggle Sidebar"? No. Use "t": prefixes nothing
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
    fn empty_command_placeholder_does_not_overlap_caret() {
        let base = 300.0;
        assert_eq!(command_field_text_x(base, false), base);
        assert!(command_field_text_x(base, true) >= base + 8.0);
    }

    #[test]
    fn file_command_descriptions_reflect_document_state() {
        assert_eq!(
            command_contextual_desc(CMD_SAVE, "base", false, false, 0),
            Cow::Borrowed("Choose a path before saving this untitled file")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE_AS, "base", true, false, 0),
            Cow::Borrowed("Choose a new path or filename for this file")
        );
        assert_eq!(
            command_contextual_desc(CMD_RENAME_ACTIVE_FILE, "base", false, false, 0),
            Cow::Borrowed("Save this untitled file before renaming it")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE, "base", true, true, 1),
            Cow::Borrowed("Read-only preview: saving is unavailable")
        );
    }

    #[test]
    fn save_all_description_reports_dirty_count() {
        assert_eq!(
            command_contextual_desc(CMD_SAVE_ALL, "base", true, false, 0),
            Cow::Borrowed("No unsaved tabs need writing")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE_ALL, "base", true, false, 1),
            Cow::Borrowed("Write the one unsaved tab")
        );
        assert_eq!(
            command_contextual_desc(CMD_SAVE_ALL, "base", true, false, 3).as_ref(),
            "Write 3 unsaved tabs"
        );
    }
}
