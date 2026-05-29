# Mighty IDE

A native, **GPU vector-rendered** IDE **written in [Mighty](https://github.com/hassard0/Mighty)** — dogfooding the language by building its own development environment in it. The entire UI is drawn each frame as a [Vello](https://github.com/linebender/vello) scene (smooth gradients, true rounded corners, soft drop shadows, wavy diagnostic underlines, anti-aliased text), at CSS quality. First-class Mighty support, extensible to other languages.

> **Status:** pre-alpha but functional. The editor builds, launches, and **edits real files live**. It is the forcing function for maturing Mighty — every place the language fights us is logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md).

![Editor](screenshots/01-editor.png)

## Features (working today)

- **Live editing** — open any file, type/insert/delete/newline, save (Ctrl+S), syntax-colored body, current-line band, line-number gutter, cursor-following scroll, click-to-place-cursor, mouse-wheel scroll, undo/redo (Ctrl+Z / Ctrl+Y)
- **Navigation** — go-to-line (Ctrl+G), find with match highlighting (Ctrl+F), go-to-definition (F12, cross-file), jump-back (Ctrl+−), hover info (Ctrl+K)
- **Workspace** — tabs (Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+W, click), file-tree sidebar (Ctrl+B), open-by-path (Ctrl+O)
- **Language intelligence (via Mighty's own `mty-lsp`)** — live `mty check` diagnostics (gutter dots + squiggle underlines), autocomplete (Ctrl+Space: semantic LSP completions + buffer words)
- **Integrated terminal** — real ConPTY shell with a VT parser (Ctrl+\`)
- **Command palette** — Ctrl+Shift+P, fuzzy-filtered
- **Theme** — the **"Aurora Noir"** dark design system rendered through **Vello** (GPU 2D vector renderer): a layered radial-gradient atmosphere, ember accents, rounded panels/cards with soft shadows, and AA text in the bundled **JetBrains Mono** (code) + **Bricolage Grotesque** (UI chrome) fonts (`fonts/`, SIL OFL)

## Live editing: the buffer lives shim-side (L28 workaround)

Under v0.36 native `mty build`, a Mighty `Vec` grown in a loop comes back empty (a confirmed codegen bug, [L28](docs/mighty-language-lessons.md)). So the **authoritative text model** (lines + cursor + selection + scroll + dirty, per tab) lives in the shim (`crates/mighty-ui-sys/src/editor.rs`), and Mighty drives every edit through scalar `mui_ed_*` ops. The model mutates in place and renders straight from itself each frame, so editing is genuinely live. It moves back to Mighty once the codegen bug is fixed.

## Architecture

Two layers, one clean boundary:

- **`crates/mighty-ui-sys`** — a Rust crate (winit + wgpu + **Vello** for vector rendering, `portable-pty` for the terminal) built as a **cdylib**, exposing a flat, **scalar-only** C ABI. It owns the window, GPU surface, Vello scene-building + text shaping, file I/O, terminal, the `mty-lsp` client, and layout. Each frame the chrome/editor draw entry points build a display list of rounded rects / gradients / shadows / glyph runs that is replayed into one `vello::Scene` (`src/vello_ui.rs`). It is "dumb about editing" but does the heavy lifting that Mighty's young FFI can't yet express. (Setting `MUI_LEGACY_RENDER=1` falls back to the old solid-rect + glyphon path.)
- **The IDE itself** (`src/main.mty`) — written in Mighty, linked against the shim via `extern c`. Mighty owns the main loop, input routing, and editor orchestration, driving the shim each frame via scalar calls.

Why scalar-only: Mighty v0.36's `extern c` can pass only scalars (no strings/pointers/structs across the boundary), so strings, pixels, paths, and buffers live shim-side and are driven by scalar getters/setters. See the lessons doc (L17–L25) for the language constraints that shaped this design.

## Build & run

Requires: the `mty` compiler (from the [Mighty](https://github.com/hassard0/Mighty) repo), a Rust toolchain, and `clang` (the linker `mty build` drives).

```sh
./build-ide.sh                 # builds the shim (cdylib) + runtime stub, then `mty build src/main.mty`
./target/main.exe path/to/file # open a file (defaults to ./scratch.mty)
```

`build-ide.sh` sets `MTY_LINKER=clang`, builds `mighty-ui-sys` as a DLL + a small C runtime-symbol stub, copies the DLL beside the exe, and runs `mty build`.

## License

MIT — see [LICENSE](LICENSE).
