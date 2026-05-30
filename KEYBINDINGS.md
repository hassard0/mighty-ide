# Mighty IDE — Keybindings

All shortcuts are routed Mighty-side in `src/main.mty` (the editor key ladder +
the `mui_chord` router). This table is the authoritative list.

## Editing

| Shortcut | Action |
|---|---|
| `Ctrl+S` | Save file |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |
| `Ctrl+/` | Toggle line comment |
| `Ctrl+Shift+D` | Duplicate line / selection |
| `Alt+Up` / `Alt+Down` | Move line up / down |
| `Ctrl+Left` / `Ctrl+Right` | Word-wise cursor motion |
| `Shift`+motion | Extend selection |
| `Ctrl+Shift+I` | Format document |
| `Ctrl+F` | Find (in file) |
| `Ctrl+H` | Find & replace (in file) |

## Multi-cursor

| Shortcut | Action |
|---|---|
| `Ctrl+D` | Add caret at next occurrence of selection |
| `Ctrl+Alt+Up` | Add caret above |
| `Ctrl+Alt+Down` | Add caret below |
| `Alt+Click` | Toggle a caret at the click point |

## Navigation & code-reading

| Shortcut | Action |
|---|---|
| `Ctrl+P` | Universal Quick-Open — files / `>` commands / `@` symbols / `:` line |
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+G` | Go to line |
| `F12` | Go to definition (cross-file) |
| `Alt+F12` | Peek definition (inline, framed preview) |
| `Ctrl+-` | Jump back to previous location |
| `Ctrl+O` | Open file by path |
| `Ctrl+B` | Toggle file-tree sidebar |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+W` | Close active tab |
| `Ctrl+Shift+F` | Project-wide Search panel |

## Language intelligence (via `mty-lsp` / per-language LSP)

| Shortcut | Action |
|---|---|
| `Ctrl+Space` | Autocomplete (semantic completions + buffer words) |
| `Ctrl+Shift+Space` | Signature help |
| `Ctrl+K` | Hover info |
| `F2` | Rename symbol |
| `Ctrl+.` | Code actions / quick-fix |

## AI

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+A` | AI copilot Agents panel (streaming chat) |
| `Ctrl+I` | Inline ask about selection / file |
| `Alt+\` | Force an inline AI ghost-text completion |
| `Ctrl+Right` (ghost shown) | Accept one word of the ghost suggestion |

## Source control (git)

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+G` | Source Control panel (status, branches, push/pull/fetch, per-hunk stage, inline diff) |
| `Alt+B` | Toggle the git blame gutter |

## Run · Test · Debug

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+R` | Run the active file (`mty run`, streamed output) |
| `Ctrl+Shift+T` | Run the package's tests |
| `F5` / `Shift+F5` | Debugger: start-continue / stop |
| `F10` | Debugger: step over |
| `F11` / `Shift+F11` | Debugger: step into / out |

## Web

| Shortcut | Action |
|---|---|
| `Alt+W` | Run in Browser — build the active file to `wasm32-web` and serve it |

## Workspace & UX

| Shortcut | Action |
|---|---|
| `Ctrl+,` | Settings (font size / tab width / word wrap / minimap / theme) |
| `Ctrl+`` ` `` ` | Toggle integrated terminal (ConPTY) |
| `Alt+G` | Mighty Agents topology panel (rescan workspace) |
| `Alt+Z` | Toggle Zen / focus mode |
| `Esc` | Dismiss the active overlay / panel / menu |

## Snippets

Type a snippet prefix and press `Tab` to expand; `Tab` / `Shift+Tab` navigate
the template's tab-stops.
