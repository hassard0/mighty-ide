# Mighty IDE

A native, GPU-rendered IDE **written in [Mighty](https://github.com/hassard0/Mighty)** — dogfooding the language by building its own development environment in it. First-class Mighty support, extensible to other languages.

> **Status:** pre-alpha but functional. The editor builds, launches, and edits real files. It is the forcing function for maturing Mighty — every place the language fights us is logged in [`docs/mighty-language-lessons.md`](docs/mighty-language-lessons.md). Visual/interactive polish is ongoing.

## Features (working today)

- **Editing** — open any file, edit, save (Ctrl+S), line-number gutter, cursor-following scroll, click-to-place-cursor, mouse-wheel scroll
- **Navigation** — go-to-line (Ctrl+G), find with match highlighting (Ctrl+F), go-to-definition (F12, cross-file), jump-back (Ctrl+−), hover info (Ctrl+K)
- **Workspace** — tabs (Ctrl+Tab / Ctrl+Shift+Tab / Ctrl+W, click), file-tree sidebar (Ctrl+B), open-by-path (Ctrl+O)
- **Language intelligence (via Mighty's own `mty-lsp`)** — live `mty check` diagnostics (gutter marks + underlines), autocomplete (Ctrl+Space: semantic LSP completions + buffer words)
- **Integrated terminal** — real ConPTY shell with a VT parser (Ctrl+\`)
- **Status bar** — filename · `Ln/Col` · error count

## Architecture

Two layers, one clean boundary:

- **`crates/mighty-ui-sys`** — a Rust crate (winit + wgpu + glyphon, + `portable-pty` for the terminal) built as a **cdylib**, exposing a flat, **scalar-only** C ABI. It owns the window, GPU surface, text rendering, file I/O, terminal, the `mty-lsp` client, and layout. It is "dumb about editing" but does the heavy lifting that Mighty's young FFI can't yet express.
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
