# Mighty IDE

**A native, GPU-vector-rendered code editor — written in [Mighty](https://github.com/hassard0/Mighty), rendered with [Vello](https://github.com/linebender/vello), dogfooding the language by building its own development environment in it.**

The entire UI is drawn each frame as a Vello scene — smooth gradients, true rounded corners, soft drop shadows, wavy diagnostic underlines, anti-aliased text — at CSS quality. The editor orchestration is Mighty source (`src/main.mty`) calling a Rust rendering/services shim across a scalar `extern c` ABI. First-class Mighty support, extensible to other languages.

![Mighty IDE](screenshots/01-editor.png)

## Features

### Editing
- Live edit / save (Ctrl+S) with syntax coloring, a current-line band, line-number gutter, click-to-place cursor, mouse-wheel + cursor-following scroll
- Undo / redo (Ctrl+Z / Ctrl+Y), typing-run coalescing
- Toggle line comment (Ctrl+/), auto-indent on Enter (brace-aware), bracket/quote auto-close + skip-over + empty-pair backspace, bracket-match highlight
- Duplicate line/selection (Ctrl+Shift+D), move line up/down (Alt+↑ / Alt+↓), word-wise motion (Ctrl+←/→), smart Home, Shift+motion selection
- In-file find & replace (Ctrl+H), find with match highlighting (Ctrl+F)

### Navigation & Intelligence
- Go-to-line (Ctrl+G), go-to-definition (F12, cross-file), jump-back (Ctrl+−)
- Hover info (Ctrl+K), autocomplete (Ctrl+Space — semantic LSP completions + buffer words)
- Signature help, rename symbol (F2), code actions / quick-fix (Ctrl+.)
- Live `mty check` diagnostics — gutter dots + wavy underlines
- All language intelligence speaks to Mighty's own `mty-lsp` over stdio

### Workspace
- Tabs (Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+W, click), file-tree sidebar (Ctrl+B), open-by-path (Ctrl+O)
- Project-wide Search panel (Ctrl+Shift+F)
- Source Control: git status panel (Ctrl+Shift+G) with an inline diff view
- Command palette (Ctrl+Shift+P), fuzzy-filtered
- Run panel (Ctrl+Shift+R) — background `mty run` with streamed output + clickable diagnostics
- Settings panel (Ctrl+,) — live font size / tab width / word wrap / minimap / theme

### AI
- AI copilot Agents panel (Ctrl+Shift+A) — streaming Anthropic chat
- Inline ask (Ctrl+I)
- Reads `ANTHROPIC_API_KEY` from the environment

### Themes
Three live-switchable design systems, all rendered through Vello:
- **Vivid Modern** (default) — near-black surfaces, indigo accents
- **Aurora Glass** — dark glass over an aurora gradient
- **Warm Studio** — a light, warm-paper theme

### Terminal
- Integrated terminal (Ctrl+`) — a real ConPTY shell with a VT parser

Bundled fonts: **JetBrains Mono** (code) + **Bricolage Grotesque** (UI chrome), both SIL OFL (`fonts/`).

## Gallery

| | |
|---|---|
| ![Editor](screenshots/01-editor.png) | ![Command palette](screenshots/04-palette.png) |
| ![Inline diff](screenshots/19-diff.png) | ![Autocomplete](screenshots/07-autocomplete.png) |
| ![Aurora Glass theme](screenshots/13-theme-aurora.png) | ![Warm Studio theme](screenshots/14-theme-warm.png) |
| ![AI copilot](screenshots/17-ai.png) | ![Settings](screenshots/20-settings.png) |

## Architecture

Two layers, one clean boundary:

- **The IDE itself — `src/main.mty`, written in Mighty.** It owns the main event loop, input routing, command dispatch, and editor orchestration, driving the shim each frame via scalar `extern c` calls.
- **`crates/mighty-ui-sys` — a Rust `cdylib` shim.** It owns the window (winit), GPU surface (wgpu), the **Vello** vector scene (gradients / rounded rects / shadows / glyph runs), text shaping, file I/O, the integrated terminal (`portable-pty`), the `mty-lsp` client, git/diff, the Run process, and the Anthropic AI client. Each frame, Mighty's draw calls build a display list that is replayed into one `vello::Scene` (`src/vello_ui.rs`).

**Why a scalar-only ABI:** Mighty v0.36's `extern c` can pass only scalars — no strings, pointers, or structs across the boundary. So strings, pixels, paths, and buffers live shim-side and are driven by scalar getters/setters. See the lessons doc (L17–L25) for the language constraints that shaped this design.

## Build & Run

Prerequisites:
- The **`mty` compiler** from [hassard0/Mighty](https://github.com/hassard0/Mighty) (build with `cargo build -p mty-cli --bin mty`)
- A **Rust** toolchain
- **clang** (the linker `mty build` drives)

```sh
./build-ide.sh                  # cargo-builds the shim cdylib + arena runtime, then `mty build src/main.mty`
./target/main.exe path/to/file  # open a file (defaults to ./scratch.mty)
```

`build-ide.sh` sets `MTY_LINKER=clang`, builds `mighty-ui-sys` as a DLL, stages the import lib + the bumpalo arena runtime, copies the DLL beside the exe, and runs `mty build`.

- `MTY_LINKER` / `STARDUST_LINKER` — point `mty build` at clang (the build script sets both).
- `ANTHROPIC_API_KEY` — enables the AI copilot panel.
- On a tight disk, set `CARGO_INCREMENTAL=0` and clear `target/debug/incremental` if a link fails on space.

See [BUILDING.md](BUILDING.md) for the exact toolchain paths and commands.

## Dogfooding Mighty

The IDE is the **forcing function** for maturing Mighty: every place the language fights us while building real native software is logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md), so each friction point can be promoted into a `stardust` issue / RFC. That feedback loop has already driven real fixes in the Mighty compiler — for example the native `Vec`-growth codegen bug ([L28](docs/mighty-language-lessons.md)), the `extern c` scalar ABI (L17), and the LSP-client discipline (L24–L25).

## Status & known caveats

Pre-alpha but functional: the editor builds, launches, and edits real files live.

The one architectural caveat is the **authoritative text model**. Under native `mty build`, a Mighty `Vec` grown in a loop came back empty (the confirmed codegen bug [L28](docs/mighty-language-lessons.md)), so the text model (lines + cursor + selection + scroll + dirty, per tab) currently lives shim-side (`crates/mighty-ui-sys/src/editor.rs`) and Mighty drives every edit through scalar `mui_ed_*` ops. This is a workaround, not a design choice — now that the codegen bug is fixed it can move back to Mighty, a localized change since Mighty already owns the event loop, key routing, and command dispatch. Visual and interactive polish is ongoing.

## License

MIT — see [LICENSE](LICENSE).
