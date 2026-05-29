# Mighty IDE

A native, GPU-rendered IDE **written in [Mighty](https://github.com/hassard0/Mighty)** — dogfooding the language by building its own development environment. First-class support for Mighty, extensible to other languages.

> **Status:** pre-alpha. Building sub-project 0 (the render shell + a minimal editor). See [`docs/superpowers/specs`](docs/superpowers/specs) for the design.

## Architecture (in brief)

Two layers with one clean boundary:

- **`mighty-ui-sys`** — a small Rust crate (winit + wgpu + cosmic-text) compiled to a static library and exposed through a flat C ABI. It owns the window, GPU surface, and text rendering. It is deliberately "dumb": it knows pixels, not editors.
- **The IDE itself** — written in Mighty, linked against `mighty-ui-sys` via `extern "C"`. Mighty owns the main loop and all editor logic (buffer, cursor, selection, syntax highlighting, AI).

Mighty owns the event loop and drives the shim each frame with high-level draw calls (`draw_text`, `fill_rect`, `set_clip`); the shim never calls back into Mighty.

## License

MIT — see [LICENSE](LICENSE).
