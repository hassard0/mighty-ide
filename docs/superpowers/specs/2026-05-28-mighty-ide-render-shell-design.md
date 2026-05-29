# Mighty IDE — Sub-project 0: Render Shell + Minimal Editor

**Date:** 2026-05-28
**Status:** Approved (design); pending implementation plan
**Repo:** `hassard0/mighty-ide` (new, public, MIT) at `C:\Users\ihass\mighty-ide`

---

## Context and scope decisions

The originating spec (`deep-research-report (17).md`) describes a full IDE platform at
parity with VS Code + JetBrains + Zed + Codespaces combined: editor core, plugin host +
marketplace, LSP bridge, multi-provider AI manager, CRDT collaboration, integrated
terminal with SSH/containers, cloud workspaces, debugger/profiler, CI/CD, and
desktop+web+cloud deployment. That is a multi-year, multi-engineer platform, **not a
single design**. It must be decomposed.

### Decisions taken during brainstorming

1. **Foundation:** Build the IDE core **from scratch, written in Mighty itself**
   (dogfooding the language at `C:\Users\ihass\stardust` → `hassard0/Mighty`).
2. **UI substrate:** **Native GPU-rendered GUI** reached through Mighty's `extern "C"`
   FFI (the Zed path) — *not* a web/WASM UI and *not* a TUI.
   - **Consequence (accepted):** the Web target is no longer free. It becomes its own
     later sub-project (Mighty→WASM + DOM), and must not distort the native architecture.
3. **Render stack:** A Rust shim binding **winit + wgpu + cosmic-text**, exposing a flat
   C ABI for Mighty to drive.
4. **Repo identity:** New public MIT repo `hassard0/mighty-ide`; the Rust FFI shim crate
   (`mighty-ui-sys`) lives **inside** this repo, keeping the IDE self-contained and
   separate from the language repo.

### Sub-project decomposition (each gets its own design → plan → build cycle)

| # | Sub-project | Delivers |
|---|---|---|
| **0** | **Render shell + minimal editor** (THIS SPEC) | C-ABI render shim + Mighty bindings; open/render/edit/save one `.mty` file with syntax highlighting. |
| 1 | Editor core (full) | Robust rope, selection model, undo/redo, multi-cursor, find/replace. |
| 2 | Mighty language smarts | Wire in `mty-lsp` in-process: completions, diagnostics, go-to-def. |
| 3 | Workspace | File tree, tabs, fuzzy-open, project search. |
| 4 | Integrated terminal | PTY via FFI. |
| 5 | AI copilot | Leans on Mighty's `llm`/`mcp`/`swarm` stdlib modules — the core differentiator. |
| 6 | Generic LSP bridge | Support languages other than Mighty. |
| 7+ | Debugger (DAP), CRDT collab, Web target, Cloud | Deferred. |

This spec covers **sub-project 0 only**.

---

## Goal of sub-project 0 (the tracer bullet)

A native window that **opens a `.mty` file, renders it with Mighty syntax highlighting,
and supports cursor movement, text editing, scrolling, and save.** Nothing more.

Success means the entire architecture is proven end-to-end: Mighty source → C ABI → GPU
pixels, with all editor logic living in Mighty.

---

## Architecture: two layers, one clean boundary

### Layer 1 — `mighty-ui-sys` (Rust crate → static lib → flat C ABI)

Deliberately "dumb": it knows pixels, not editors. Responsibilities:

- **winit** — window creation + input, using the **poll / pump-events model** (the shim
  does *not* own a callback-driven loop and never calls back into Mighty).
- **wgpu** — GPU surface plus two pipelines: a solid-rect pipeline and a glyph pipeline.
- **cosmic-text / glyphon** — font loading, Unicode shaping, and a glyph atlas.

**C ABI surface (initial):**

```
mui_init(width, height, title)            -> *Context        // create window + GPU surface
mui_poll_event(ctx, &out Event)           -> bool            // drain one queued input event
mui_begin_frame(ctx)                       -> void           // acquire surface texture, clear
mui_fill_rect(ctx, x, y, w, h, rgba)       -> void
mui_draw_text(ctx, x, y, utf8_ptr, len, rgba) -> void
mui_text_measure(ctx, utf8_ptr, len, &out w, &out h) -> void
mui_set_clip(ctx, x, y, w, h)              -> void
mui_end_frame(ctx)                         -> void           // submit + present
mui_shutdown(ctx)                          -> void
```

`Event` is a flat C struct (tag + union-ish fields: key, modifiers, mouse x/y/button,
scroll delta, resize w/h, char codepoint). All strings cross the boundary as
`(ptr, len)` UTF-8; no ownership transfer of editor data.

### Layer 2 — the IDE (`.mty`)

All logic lives here:

- `extern "C"` declarations binding the shim functions above.
- **Main loop (Mighty owns it):**
  `loop { while mui_poll_event(...) { dispatch } ; update_model() ; render_frame() }`
- **Editor model:** rope (or gap buffer) text storage, cursor position, selection,
  viewport / scroll offset, undo stack.
- **Syntax highlighting:** for MVP-0, a minimal Mighty-side tokenizer for `.mty`
  (keywords, strings, line comments, numbers, identifiers). Reusing `mty-syntax`
  proper (via FFI or a shared crate) is a later optimization, not in this slice.

---

## Data flow (per frame)

```
file ──load──> rope (Mighty)
                 │
   input event ──┤ editor command ──> mutate rope + cursor/selection
                 │
   render_frame: compute visible line range from viewport
              ─> tokenize visible lines (Mighty tokenizer)
              ─> emit fill_rect (cursor, selection, gutter) + draw_text (glyphs, themed)
              ─> shim rasterizes to GPU surface and presents
```

The model is mutated by input; the next frame re-renders from the model. No retained
scene graph — immediate-mode draw calls each frame, which keeps the C ABI minimal.

---

## Build & link model

- `cargo` builds `mighty-ui-sys` into a **static library** (`staticlib`/`cdylib`).
- The Mighty IDE source AOT-compiles to a native object and **links against** that
  library. A `Makefile` (or build script) orchestrates: build shim → `mty build`
  linking the shim → produce the IDE binary.
- Per-platform linking details (Win/Mac/Linux) are an implementation concern for the
  plan; the same shim source covers all three via winit/wgpu.

---

## #1 feasibility risk and its mitigation

The whole bet rests on Mighty being able to **(a)** declare `extern "C"` functions with
pointer / struct / out-parameter signatures, and **(b)** link against an external native
static library at AOT time. `examples/14_extern_c.mty` indicates FFI exists, but an IDE
exercises it far harder than any example.

**Mitigation — a Day-1 spike, treated as a HARD GATE before any real work:**
get Mighty to call a single trivial Rust C-ABI function that opens a winit window and
clears it to a solid color, driven from a Mighty `poll` / `present` loop.

- **Green spike** → proceed with the full slice.
- **Red spike** → stop and revisit the FFI strategy *before* sinking weeks into it
  (options at that point: extend Mighty's FFI/linking, or fall back to a different
  UI-substrate decision).

---

## Testing strategy

- **Editor model** (rope, cursor, edits, undo, viewport math) is **pure Mighty logic** →
  unit-tested headlessly with Mighty's test framework. Built **test-first (TDD)**.
- **Render shim** → offscreen wgpu render-to-texture with pixel assertions on simple
  cases (a filled rect, a single glyph at a known position), plus manual smoke runs.
- **Integration** → the Day-1 spike, then a manual smoke checklist: open file → see
  highlighted text → arrow keys move cursor → type inserts → scroll works → save writes
  the file back.

---

## Explicitly OUT of scope for sub-project 0

LSP / completions / diagnostics, multiple files / tabs, file tree / explorer, integrated
terminal, AI copilot panel, configurable themes, multi-language support, plugins /
marketplace, collaboration, debugger, and the Web/cloud targets. Each is a later
sub-project with its own spec.

---

## Open implementation questions (for the plan, not blockers)

- Exact Mighty `extern "C"` syntax and supported signature shapes (verified by the spike).
- Rope vs. gap buffer for MVP-0 (gap buffer is simpler; rope scales — decide in plan).
- Font selection / bundling (ship a default monospace font with the repo).
- HiDPI / scale-factor handling (winit reports it; decide minimum viable handling).
