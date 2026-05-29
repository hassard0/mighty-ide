# Mighty IDE — Render Shell + Minimal Editor: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open a native window that renders a `.mty` file with Mighty syntax highlighting and supports cursor movement, text editing, scrolling, and save — with the editor logic written in Mighty and the GPU rendering done by a Rust `extern "C"` shim.

**Architecture:** Two layers. `crates/mighty-ui-sys` (Rust, `staticlib`, flat C ABI) owns window + GPU surface + text rendering via winit/wgpu/glyphon; it is "dumb" (pixels, not editors). The IDE (`src/*.mty`) owns the main loop and all editor logic, linked against the shim via `extern c`. Mighty owns the loop and drives the shim each frame; the shim never calls back into Mighty (poll/pump-events model).

**Tech Stack:** Rust (winit, wgpu, glyphon, pollster), Mighty v0.30.1 (`mty` CLI), a C linker (clang/MSVC). Tests: `cargo test` (Rust shim), `mty test` (Mighty logic).

---

## Decisions locked by this plan

- **Line buffer model:** a document is `Vec[Line]`; a `Line` wraps `Vec[U8]` (UTF-8 bytes). Insert/delete rebuild the byte vector with `push` loops — uses only confirmed Mighty primitives (`Vec.new/push/pop/get/len/clear`, index assignment, `while`/`for`). No stdlib extension needed for MVP-0.
- **Editing granularity:** byte offsets, ASCII-correct. Multi-byte UTF-8 cursor movement is a documented MVP-0 limitation, deferred to sub-project 1.
- **Execution model:** target `mty build` (native binary). If the Phase 0 gate shows native codegen can't yet compile dynamic FFI loops, fall back to `mty run` (JIT + interpreter) and record it as a Mighty-codegen follow-up.
- **Render boundary:** immediate-mode. Each frame Mighty calls `mui_begin_frame`, a series of `mui_fill_rect`/`mui_draw_text`/`mui_set_clip`, then `mui_end_frame`. No retained scene.

---

## File structure

```
mighty-ide/
├── Cargo.toml                         # workspace: members = ["crates/mighty-ui-sys"]
├── crates/
│   └── mighty-ui-sys/
│       ├── Cargo.toml                 # crate-type = ["staticlib"]; deps winit/wgpu/glyphon/pollster
│       ├── cbindgen.toml              # generates include/mighty_ui.h (optional, for the C harness)
│       ├── include/mighty_ui.h        # C header (hand-written or cbindgen) for the C smoke harness
│       └── src/
│           ├── lib.rs                 # C ABI entry points (mui_*); thin, delegates to modules
│           ├── ffi.rs                 # #[repr(C)] structs (MuiEvent, MuiColor), pointer helpers
│           ├── gpu.rs                 # wgpu device/surface/queue, rect pipeline
│           ├── text.rs                # glyphon font system + atlas + text rendering
│           └── window.rs              # winit window + pump_events loop, event → MuiEvent mapping
├── mighty.toml                        # Mighty package manifest (links the staticlib)
├── src/
│   ├── ffi.mty                        # extern c { ... } bindings + #-repr structs mirror
│   ├── line.mty                       # Line = Vec[U8]; insert/delete/len/byte_at/to_bytes
│   ├── buffer.mty                     # Buffer = Vec[Line]; load/save/line ops/split/join
│   ├── cursor.mty                     # Cursor {line,col}; movement clamped to buffer
│   ├── viewport.mty                   # scroll offset + visible-line range math
│   ├── tokenize.mty                   # minimal .mty tokenizer → (kind, start, end) spans
│   ├── theme.mty                      # token-kind → MuiColor
│   ├── render.mty                     # draw a frame from (buffer, cursor, viewport, theme)
│   ├── input.mty                      # MuiEvent → editor command dispatch
│   └── main.mty                       # init shim, main loop, load file argv, save on Ctrl-S
├── tests/
│   ├── line_test.mty
│   ├── buffer_test.mty
│   ├── cursor_test.mty
│   ├── viewport_test.mty
│   └── tokenize_test.mty
├── scripts/
│   ├── build.sh                       # cargo build shim → mty build/link IDE
│   └── spike.sh                       # Phase 0 spike driver
└── fonts/
    └── (a bundled monospace .ttf, added in Phase 2)
```

---

# Phase 0 — Spike & Feasibility Gate (HARD GATE)

Goal: prove the architecture is buildable on today's Mighty *before* investing in the full slice. Each task ends in a recorded GO / NO-GO. Do not start Phase 2+ until the gate is GO.

### Task 0.1: Cargo workspace + staticlib skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `crates/mighty-ui-sys/Cargo.toml`
- Create: `crates/mighty-ui-sys/src/lib.rs`

- [ ] **Step 1: Write workspace `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/mighty-ui-sys"]
```

- [ ] **Step 2: Write the crate `Cargo.toml`**

```toml
[package]
name = "mighty-ui-sys"
version = "0.0.0"
edition = "2021"
publish = false

[lib]
crate-type = ["staticlib"]

[dependencies]
winit = "0.30"
wgpu = "22"
glyphon = "0.6"
pollster = "0.3"
raw-window-handle = "0.6"
```

- [ ] **Step 3: Write a trivial exported function in `src/lib.rs`**

```rust
/// Smoke export: proves the staticlib builds and exports a C symbol.
#[no_mangle]
pub extern "C" fn mui_smoke_add(a: i32, b: i32) -> i32 {
    a + b
}
```

- [ ] **Step 4: Build the staticlib**

Run: `cargo build -p mighty-ui-sys`
Expected: PASS; produces `target/debug/libmighty_ui_sys.a` (or `mighty_ui_sys.lib` on Windows).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/mighty-ui-sys
git commit -m "spike: cargo workspace + mighty-ui-sys staticlib skeleton"
```

### Task 0.2: Rust-side window-clear, proven via a C harness (no Mighty yet)

Isolate the Rust GPU path from the FFI question by driving it from a 20-line C program first.

**Files:**
- Modify: `crates/mighty-ui-sys/src/lib.rs`
- Create: `crates/mighty-ui-sys/src/window.rs`
- Create: `crates/mighty-ui-sys/src/gpu.rs`
- Create: `crates/mighty-ui-sys/include/mighty_ui.h`
- Create: `scripts/spike.sh`

- [ ] **Step 1: Implement minimal window + clear in `window.rs`/`gpu.rs`**

`gpu.rs` — request a wgpu adapter/device/surface for a raw window handle and clear it:

```rust
use std::sync::Arc;
use winit::window::Window;

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl Gpu {
    pub fn new(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: caps.formats[0],
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        Self { surface, device, queue, config }
    }

    pub fn clear(&self, r: f64, g: f64, b: f64) {
        let frame = self.surface.get_current_texture().unwrap();
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let _rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit([enc.finish()]);
        frame.present();
    }
}
```

`window.rs` — own the winit application + pump loop, exposed as opaque context (full event-pump wiring is finished in Phase 2; for the spike a single `mui_run_clear_demo` that opens a window, clears blue, and pumps until close is sufficient).

```rust
// Spike-only: blocking demo that opens a window and clears to blue until closed.
pub fn run_clear_demo() { /* winit 0.30 ApplicationHandler: create window, build Gpu, on RedrawRequested -> gpu.clear(0.1,0.2,0.5), on CloseRequested -> exit */ }
```

- [ ] **Step 2: Export the demo + write the C header**

`lib.rs`:

```rust
mod gpu;
mod window;

#[no_mangle]
pub extern "C" fn mui_run_clear_demo() {
    window::run_clear_demo();
}
```

`include/mighty_ui.h`:

```c
#ifndef MIGHTY_UI_H
#define MIGHTY_UI_H
int  mui_smoke_add(int a, int b);
void mui_run_clear_demo(void);
#endif
```

- [ ] **Step 3: Write `scripts/spike.sh` to build + link a C harness**

```bash
#!/usr/bin/env bash
set -euo pipefail
cargo build -p mighty-ui-sys
LIB=target/debug
cat > /tmp/harness.c <<'EOF'
#include "mighty_ui.h"
int main(void){ mui_run_clear_demo(); return 0; }
EOF
# macOS/Linux link (Windows: cl /I include harness.c mighty_ui_sys.lib + system libs)
clang -I crates/mighty-ui-sys/include /tmp/harness.c \
  "$LIB/libmighty_ui_sys.a" -o /tmp/harness \
  -framework Metal -framework QuartzCore -framework AppKit  # macOS; on Linux: -lvulkan -lX11 -ldl -lm
/tmp/harness
```

- [ ] **Step 4: Run the harness**

Run: `bash scripts/spike.sh`
Expected: a window opens filled solid blue; closing it exits 0. Record the exact system libraries needed to link (per OS) — they are reused when linking the Mighty binary.

- [ ] **Step 5: Commit**

```bash
git add crates/mighty-ui-sys scripts/spike.sh
git commit -m "spike: Rust window-clear proven via C harness"
```

### Task 0.3: Mighty → shim FFI link (the core gate)

**Files:**
- Create: `mighty.toml`
- Create: `src/main.mty`
- Create: `scripts/build.sh`

- [ ] **Step 1: Scaffold the Mighty package manifest**

```toml
[package]
name = "mighty-ide"
version = "0.0.0"
edition = "2026"
profile = "host"

[deps]
```

- [ ] **Step 2: Minimal Mighty program calling one extern c fn**

`src/main.mty`:

```mty
extern c {
  fn mui_smoke_add(a: I32, b: I32) -> I32
}

fn main() {
  let r = mui_smoke_add(2, 40)
  // native codegen only accepts literal log args today; assert via exit code instead
  if r != 42 { panic("FFI add wrong") }
  log("ffi-link-ok")
}
```

- [ ] **Step 3: Discover the link path in `scripts/build.sh`**

Try, in order, and record which works:
1. A `mighty.toml` native-link directive (check `mty` manifest reference for a `[build] link = [...]` / `native-libs` key).
2. `mty build src/main.mty` then manually link the emitted `.o` (see message `MT8008`) against `libmighty_ui_sys.a` + recorded system libs with clang.

```bash
#!/usr/bin/env bash
set -euo pipefail
cargo build -p mighty-ui-sys
mty build src/main.mty || true     # may emit .o + MT8008 instructions if it can't auto-link
# Fallback manual link (fill object path from MT8008 output):
# clang target/main.o target/debug/libmighty_ui_sys.a <system-libs> -o target/mighty-ide
./target/mighty-ide
```

- [ ] **Step 4: Run and verify**

Run: `bash scripts/build.sh`
Expected: prints `ffi-link-ok`, exits 0.
**GATE A (linking):** GO if the Mighty binary calls the Rust symbol and exits 0. NO-GO → stop; the native-GUI-in-Mighty approach needs an FFI/linking fix in Mighty first. Record findings.

- [ ] **Step 5: Commit**

```bash
git add mighty.toml src/main.mty scripts/build.sh
git commit -m "spike: Mighty links and calls the Rust shim (gate A)"
```

### Task 0.4: Dynamic-FFI-in-a-loop codegen stress (the real gate)

**Files:**
- Modify: `src/main.mty`
- Modify: `crates/mighty-ui-sys/src/lib.rs`

- [ ] **Step 1: Add a stateful counter export to the shim**

`lib.rs`:

```rust
use std::sync::atomic::{AtomicI32, Ordering};
static SUM: AtomicI32 = AtomicI32::new(0);

#[no_mangle]
pub extern "C" fn mui_accumulate(x: i32) -> i32 {
    SUM.fetch_add(x, Ordering::SeqCst) + x
}
```

- [ ] **Step 2: Call it with dynamic (non-literal) args in a loop from Mighty**

`src/main.mty`:

```mty
extern c {
  fn mui_accumulate(x: I32) -> I32
}

fn main() {
  let mut i: I32 = 0
  let mut last: I32 = 0
  while i < 10 {
    last = mui_accumulate(i)   // dynamic arg + dynamic capture across FFI in a loop
    i = i + 1
  }
  if last != 45 { panic("loop FFI sum wrong") }
  log("dynamic-ffi-loop-ok")
}
```

- [ ] **Step 3: Build native and run**

Run: `bash scripts/build.sh`
Expected: prints `dynamic-ffi-loop-ok`, exits 0.
**GATE B (native codegen):** GO if `mty build` (native binary) handles dynamic FFI calls in a loop. If `mty build` fails but `mty run src/main.mty` succeeds, set execution model to **`mty run` (JIT)** for now and file a Mighty native-codegen issue; that is still GO for the IDE (degraded build path). NO-GO only if neither path works.

- [ ] **Step 4: Record the gate decision in the spec**

Append a "Spike results (YYYY-MM-DD)" section to `docs/superpowers/specs/2026-05-28-mighty-ide-render-shell-design.md`: Gate A result, Gate B result, chosen execution model, per-OS link libs.

- [ ] **Step 5: Commit**

```bash
git add src/main.mty crates/mighty-ui-sys/src/lib.rs docs/superpowers/specs
git commit -m "spike: dynamic FFI loop codegen (gate B) + recorded results"
```

**>>> HARD GATE: do not proceed to Phase 1+ unless Gates A and B are GO. <<<**

---

# Phase 1 — Editor model in pure Mighty (FFI-independent, full TDD)

This phase has zero FFI and is safe to build regardless of GPU details. It is the de-risked core. All tasks use `mty test`.

### Task 1.1: `Line` — an editable byte vector

**Files:**
- Create: `src/line.mty`
- Test: `tests/line_test.mty`

- [ ] **Step 1: Write the failing test**

```mty
import line.{line_new, line_insert, line_delete, line_len, line_byte_at}

fn test_insert_into_empty() {
  let l = line_new()
  let l2 = line_insert(l, 0, 65_u8)   // 'A'
  assert_eq(line_len(l2), 1)
  assert_eq(line_byte_at(l2, 0), 65_u8)
}

fn test_insert_middle() {
  let mut l = line_new()
  l = line_insert(l, 0, 65_u8)        // "A"
  l = line_insert(l, 1, 67_u8)        // "AC"
  l = line_insert(l, 1, 66_u8)        // "ABC"
  assert_eq(line_len(l), 3)
  assert_eq(line_byte_at(l, 1), 66_u8)
}

fn test_delete() {
  let mut l = line_new()
  l = line_insert(l, 0, 65_u8)
  l = line_insert(l, 1, 66_u8)
  l = line_delete(l, 0)               // remove 'A' -> "B"
  assert_eq(line_len(l), 1)
  assert_eq(line_byte_at(l, 0), 66_u8)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `mty test --dir tests`
Expected: FAIL — `line` module / functions not found.

- [ ] **Step 3: Write minimal implementation**

`src/line.mty`:

```mty
pub type Line = Vec[U8]

pub fn line_new() -> Line {
  Vec.new()
}

pub fn line_len(l: Line) -> USize {
  l.len()
}

pub fn line_byte_at(l: Line, idx: USize) -> U8 {
  match l.get(idx) {
    Some(b) => b
    None => 0_u8
  }
}

// Rebuild with the new byte spliced at `idx` (uses only push).
pub fn line_insert(l: Line, idx: USize, b: U8) -> Line {
  let mut out = Vec.with_capacity(l.len() + 1)
  let mut i: USize = 0
  while i < l.len() {
    if i == idx { out.push(b) }
    out.push(line_byte_at(l, i))
    i = i + 1
  }
  if idx >= l.len() { out.push(b) }
  out
}

pub fn line_delete(l: Line, idx: USize) -> Line {
  let mut out = Vec.with_capacity(l.len())
  let mut i: USize = 0
  while i < l.len() {
    if i != idx { out.push(line_byte_at(l, i)) }
    i = i + 1
  }
  out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mty test --dir tests`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add src/line.mty tests/line_test.mty
git commit -m "feat(buffer): editable Line (Vec[U8]) with insert/delete (TDD)"
```

### Task 1.2: `Buffer` — lines + load/save + split/join

**Files:**
- Create: `src/buffer.mty`
- Test: `tests/buffer_test.mty`

- [ ] **Step 1: Write the failing test**

```mty
import buffer.{buffer_from_text, buffer_line_count, buffer_line, buffer_to_text,
  buffer_split_line, buffer_join_line}
import line.{line_len}

fn test_from_text_splits_on_newline() {
  let b = buffer_from_text("ab\ncd\n")     // trailing newline => 3 logical lines, last empty
  assert_eq(buffer_line_count(b), 3)
  assert_eq(line_len(buffer_line(b, 0)), 2)
  assert_eq(line_len(buffer_line(b, 2)), 0)
}

fn test_roundtrip() {
  let b = buffer_from_text("hello\nworld")
  assert_eq(buffer_to_text(b), "hello\nworld")
}

fn test_split_line_at_col() {
  let b = buffer_from_text("abcd")
  let b2 = buffer_split_line(b, 0, 2)       // "ab" | "cd"
  assert_eq(buffer_line_count(b2), 2)
  assert_eq(line_len(buffer_line(b2, 0)), 2)
  assert_eq(line_len(buffer_line(b2, 1)), 2)
}

fn test_join_line() {
  let b = buffer_from_text("ab\ncd")
  let b2 = buffer_join_line(b, 0)            // join line 1 onto line 0 => "abcd"
  assert_eq(buffer_line_count(b2), 1)
  assert_eq(line_len(buffer_line(b2, 0)), 4)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `mty test --dir tests`
Expected: FAIL — `buffer` module not found.

- [ ] **Step 3: Write minimal implementation**

`src/buffer.mty`:

```mty
import line.{Line, line_new, line_len, line_byte_at}

pub type Buffer = Vec[Line]

pub fn buffer_line_count(b: Buffer) -> USize { b.len() }

pub fn buffer_line(b: Buffer, idx: USize) -> Line {
  match b.get(idx) {
    Some(l) => l
    None => line_new()
  }
}

pub fn buffer_from_text(text: String) -> Buffer {
  let mut out: Vec[Line] = Vec.new()
  let mut cur: Line = line_new()
  let bytes = text.as_bytes()
  let mut i: USize = 0
  while i < bytes.len() {
    let b = bytes[i]
    if b == 10_u8 {            // '\n'
      out.push(cur)
      cur = line_new()
    } else {
      cur.push(b)
    }
    i = i + 1
  }
  out.push(cur)
  out
}

pub fn buffer_to_text(b: Buffer) -> String {
  let mut s = String.new()
  let mut li: USize = 0
  while li < b.len() {
    if li > 0 { s.push('\n') }
    let line = buffer_line(b, li)
    let mut ci: USize = 0
    while ci < line_len(line) {
      s.push_byte(line_byte_at(line, ci))   // see note below
      ci = ci + 1
    }
    li = li + 1
  }
  s
}

pub fn buffer_split_line(b: Buffer, lineno: USize, col: USize) -> Buffer {
  let mut out: Vec[Line] = Vec.new()
  let mut i: USize = 0
  while i < b.len() {
    if i == lineno {
      let src = buffer_line(b, i)
      let mut left: Line = line_new()
      let mut right: Line = line_new()
      let mut c: USize = 0
      while c < line_len(src) {
        if c < col { left.push(line_byte_at(src, c)) }
        else { right.push(line_byte_at(src, c)) }
        c = c + 1
      }
      out.push(left)
      out.push(right)
    } else {
      out.push(buffer_line(b, i))
    }
    i = i + 1
  }
  out
}

pub fn buffer_join_line(b: Buffer, lineno: USize) -> Buffer {
  let mut out: Vec[Line] = Vec.new()
  let mut i: USize = 0
  while i < b.len() {
    if i == lineno && (i + 1) < b.len() {
      let mut merged = buffer_line(b, i)
      let next = buffer_line(b, i + 1)
      let mut c: USize = 0
      while c < line_len(next) { merged.push(line_byte_at(next, c)); c = c + 1 }
      out.push(merged)
      i = i + 1          // skip the consumed next line
    } else {
      out.push(buffer_line(b, i))
    }
    i = i + 1
  }
  out
}
```

> **Implementation note (verify in Step 4):** `String.push` takes a `char`; `buffer_to_text` needs to append a raw byte. If no `push_byte` exists, accumulate into a `Vec[U8]` and convert once with `String.from_utf8`/`from_utf` (seen in `string.rs` as `from_utf`). Adjust the single line accordingly; the test asserts the text result so it will catch a wrong choice.

- [ ] **Step 4: Run tests to verify they pass**

Run: `mty test --dir tests`
Expected: PASS — 4 tests. If `push_byte` is unresolved, switch to the `Vec[U8]` + `from_utf` form noted above and re-run.

- [ ] **Step 5: Commit**

```bash
git add src/buffer.mty tests/buffer_test.mty
git commit -m "feat(buffer): Buffer load/save/split/join over lines (TDD)"
```

### Task 1.3: `Cursor` — clamped movement and edit integration

**Files:**
- Create: `src/cursor.mty`
- Test: `tests/cursor_test.mty`

- [ ] **Step 1: Write the failing test**

```mty
import cursor.{Cursor, cur_new, cur_left, cur_right, cur_up, cur_down, cur_line, cur_col}
import buffer.{buffer_from_text}

fn test_right_clamps_to_line_end_then_wraps_down() {
  let b = buffer_from_text("ab\ncd")
  let c0 = cur_new()                       // line 0, col 0
  let c1 = cur_right(c0, b)                 // col 1
  let c2 = cur_right(c1, b)                 // col 2 (end of "ab")
  let c3 = cur_right(c2, b)                 // wraps to line 1, col 0
  assert_eq(cur_line(c3), 1)
  assert_eq(cur_col(c3), 0)
}

fn test_up_at_top_is_noop() {
  let b = buffer_from_text("ab\ncd")
  let c = cur_up(cur_new(), b)
  assert_eq(cur_line(c), 0)
  assert_eq(cur_col(c), 0)
}

fn test_down_clamps_col_to_shorter_line() {
  let b = buffer_from_text("abcd\nxy")
  let mut c = cur_new()
  c = cur_right(c, b)
  c = cur_right(c, b)
  c = cur_right(c, b)                       // line 0 col 3
  c = cur_down(c, b)                        // line 1 "xy" len 2 => clamp col to 2
  assert_eq(cur_line(c), 1)
  assert_eq(cur_col(c), 2)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `mty test --dir tests`
Expected: FAIL — `cursor` module not found.

- [ ] **Step 3: Write minimal implementation**

`src/cursor.mty`:

```mty
import buffer.{Buffer, buffer_line_count, buffer_line}
import line.{line_len}

pub struct Cursor {
  line: USize
  col: USize
}

pub fn cur_new() -> Cursor { Cursor { line: 0, col: 0 } }
pub fn cur_line(c: Cursor) -> USize { c.line }
pub fn cur_col(c: Cursor) -> USize { c.col }

fn line_length(b: Buffer, l: USize) -> USize { line_len(buffer_line(b, l)) }

pub fn cur_left(c: Cursor, b: Buffer) -> Cursor {
  if c.col > 0 { Cursor { line: c.line, col: c.col - 1 } }
  else if c.line > 0 {
    let pl = c.line - 1
    Cursor { line: pl, col: line_length(b, pl) }
  } else { c }
}

pub fn cur_right(c: Cursor, b: Buffer) -> Cursor {
  let len = line_length(b, c.line)
  if c.col < len { Cursor { line: c.line, col: c.col + 1 } }
  else if (c.line + 1) < buffer_line_count(b) { Cursor { line: c.line + 1, col: 0 } }
  else { c }
}

pub fn cur_up(c: Cursor, b: Buffer) -> Cursor {
  if c.line == 0 { return c }
  let nl = c.line - 1
  let len = line_length(b, nl)
  let nc = if c.col < len { c.col } else { len }
  Cursor { line: nl, col: nc }
}

pub fn cur_down(c: Cursor, b: Buffer) -> Cursor {
  if (c.line + 1) >= buffer_line_count(b) { return c }
  let nl = c.line + 1
  let len = line_length(b, nl)
  let nc = if c.col < len { c.col } else { len }
  Cursor { line: nl, col: nc }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mty test --dir tests`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add src/cursor.mty tests/cursor_test.mty
git commit -m "feat(editor): clamped cursor movement (TDD)"
```

### Task 1.4: `Viewport` — visible-line range from scroll offset

**Files:**
- Create: `src/viewport.mty`
- Test: `tests/viewport_test.mty`

- [ ] **Step 1: Write the failing test**

```mty
import viewport.{Viewport, vp_new, vp_scroll_to_cursor, vp_first_line, vp_visible_rows}

fn test_scroll_down_to_keep_cursor_visible() {
  let vp = vp_new(10)                       // 10 visible rows
  // cursor on line 25, current top 0 => top must move so 25 is visible (top = 16)
  let vp2 = vp_scroll_to_cursor(vp, 25, 100)
  assert_eq(vp_first_line(vp2), 16)
}

fn test_scroll_up_when_cursor_above_top() {
  let mut vp = vp_new(10)
  vp = vp_scroll_to_cursor(vp, 25, 100)     // top 16
  vp = vp_scroll_to_cursor(vp, 5, 100)      // cursor above top => top = 5
  assert_eq(vp_first_line(vp), 5)
}

fn test_no_scroll_when_visible() {
  let vp = vp_new(10)
  let vp2 = vp_scroll_to_cursor(vp, 3, 100)  // already visible => top stays 0
  assert_eq(vp_first_line(vp2), 0)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `mty test --dir tests`
Expected: FAIL — `viewport` module not found.

- [ ] **Step 3: Write minimal implementation**

`src/viewport.mty`:

```mty
pub struct Viewport {
  first_line: USize
  rows: USize
}

pub fn vp_new(rows: USize) -> Viewport { Viewport { first_line: 0, rows: rows } }
pub fn vp_first_line(vp: Viewport) -> USize { vp.first_line }
pub fn vp_visible_rows(vp: Viewport) -> USize { vp.rows }

pub fn vp_scroll_to_cursor(vp: Viewport, cursor_line: USize, total_lines: USize) -> Viewport {
  let mut top = vp.first_line
  if cursor_line < top {
    top = cursor_line
  } else if cursor_line >= top + vp.rows {
    top = cursor_line - vp.rows + 1
  }
  Viewport { first_line: top, rows: vp.rows }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mty test --dir tests`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add src/viewport.mty tests/viewport_test.mty
git commit -m "feat(editor): viewport scroll-to-cursor math (TDD)"
```

### Task 1.5: `tokenize` — minimal `.mty` token spans

**Files:**
- Create: `src/tokenize.mty`
- Test: `tests/tokenize_test.mty`

- [ ] **Step 1: Write the failing test**

```mty
import tokenize.{tokenize_line, tok_count, tok_kind, tok_start, tok_end,
  KIND_KEYWORD, KIND_STRING, KIND_COMMENT, KIND_NUMBER, KIND_IDENT}
import line.{line_new}

// helper to build a Line from an ascii string literal's bytes
import testutil.{line_of}

fn test_keyword_and_ident() {
  let toks = tokenize_line(line_of("fn main"))   // "fn" keyword, "main" ident
  assert_eq(tok_count(toks), 2)
  assert_eq(tok_kind(toks, 0), KIND_KEYWORD)
  assert_eq(tok_kind(toks, 1), KIND_IDENT)
}

fn test_line_comment() {
  let toks = tokenize_line(line_of("x // hi"))
  assert_eq(tok_kind(toks, tok_count(toks) - 1), KIND_COMMENT)
}

fn test_number_and_string() {
  let toks = tokenize_line(line_of("42 \"s\""))
  assert_eq(tok_kind(toks, 0), KIND_NUMBER)
  assert_eq(tok_kind(toks, 1), KIND_STRING)
}
```

- [ ] **Step 2: Add `src/testutil.mty` and run to verify failure**

`src/testutil.mty`:

```mty
import line.{Line, line_new}

// Build a Line from a String literal's bytes (test-only convenience).
pub fn line_of(s: String) -> Line {
  let mut l = line_new()
  let bytes = s.as_bytes()
  let mut i: USize = 0
  while i < bytes.len() { l.push(bytes[i]); i = i + 1 }
  l
}
```

Run: `mty test --dir tests`
Expected: FAIL — `tokenize` module not found.

- [ ] **Step 3: Write minimal implementation**

`src/tokenize.mty`:

```mty
import line.{Line, line_len, line_byte_at}

pub const KIND_TEXT: U8 = 0_u8
pub const KIND_KEYWORD: U8 = 1_u8
pub const KIND_IDENT: U8 = 2_u8
pub const KIND_STRING: U8 = 3_u8
pub const KIND_COMMENT: U8 = 4_u8
pub const KIND_NUMBER: U8 = 5_u8

pub struct Token { kind: U8, start: USize, end: USize }
pub type Tokens = Vec[Token]

pub fn tok_count(t: Tokens) -> USize { t.len() }
pub fn tok_kind(t: Tokens, i: USize) -> U8 {
  match t.get(i) { Some(tk) => tk.kind  None => KIND_TEXT }
}
pub fn tok_start(t: Tokens, i: USize) -> USize {
  match t.get(i) { Some(tk) => tk.start  None => 0 }
}
pub fn tok_end(t: Tokens, i: USize) -> USize {
  match t.get(i) { Some(tk) => tk.end  None => 0 }
}

fn is_space(b: U8) -> Bool { b == 32_u8 || b == 9_u8 }
fn is_digit(b: U8) -> Bool { b >= 48_u8 && b <= 57_u8 }
fn is_ident(b: U8) -> Bool {
  (b >= 65_u8 && b <= 90_u8) || (b >= 97_u8 && b <= 122_u8) || b == 95_u8 || is_digit(b)
}

// Compare a [start,end) byte span against a keyword spelled as bytes.
fn span_is(l: Line, start: USize, end: USize, kw: Vec[U8]) -> Bool {
  if (end - start) != kw.len() { return false }
  let mut i: USize = 0
  while i < kw.len() {
    if line_byte_at(l, start + i) != kw[i] { return false }
    i = i + 1
  }
  true
}

fn keyword_bytes() -> Vec[Vec[U8]] {
  // fn, let, mut, struct, enum, match, if, else, while, for, return, pub, agent
  let mut kws: Vec[Vec[U8]] = Vec.new()
  kws.push(bytes_of("fn"));    kws.push(bytes_of("let"));   kws.push(bytes_of("mut"))
  kws.push(bytes_of("struct"));kws.push(bytes_of("enum"));  kws.push(bytes_of("match"))
  kws.push(bytes_of("if"));    kws.push(bytes_of("else"));  kws.push(bytes_of("while"))
  kws.push(bytes_of("for"));   kws.push(bytes_of("return"));kws.push(bytes_of("pub"))
  kws.push(bytes_of("agent"))
  kws
}

fn bytes_of(s: String) -> Vec[U8] {
  let mut v: Vec[U8] = Vec.new()
  let bs = s.as_bytes()
  let mut i: USize = 0
  while i < bs.len() { v.push(bs[i]); i = i + 1 }
  v
}

fn is_keyword(l: Line, start: USize, end: USize) -> Bool {
  let kws = keyword_bytes()
  let mut k: USize = 0
  while k < kws.len() {
    if span_is(l, start, end, kws[k]) { return true }
    k = k + 1
  }
  false
}

pub fn tokenize_line(l: Line) -> Tokens {
  let mut toks: Tokens = Vec.new()
  let n = line_len(l)
  let mut i: USize = 0
  while i < n {
    let b = line_byte_at(l, i)
    if is_space(b) {
      i = i + 1
    } else if b == 47_u8 && (i + 1) < n && line_byte_at(l, i + 1) == 47_u8 {
      toks.push(Token { kind: KIND_COMMENT, start: i, end: n })   // // to EOL
      i = n
    } else if b == 34_u8 {                                         // " string
      let s = i
      i = i + 1
      while i < n && line_byte_at(l, i) != 34_u8 { i = i + 1 }
      if i < n { i = i + 1 }
      toks.push(Token { kind: KIND_STRING, start: s, end: i })
    } else if is_digit(b) {
      let s = i
      while i < n && (is_digit(line_byte_at(l, i)) || line_byte_at(l, i) == 95_u8) { i = i + 1 }
      toks.push(Token { kind: KIND_NUMBER, start: s, end: i })
    } else if is_ident(b) {
      let s = i
      while i < n && is_ident(line_byte_at(l, i)) { i = i + 1 }
      let kind = if is_keyword(l, s, i) { KIND_KEYWORD } else { KIND_IDENT }
      toks.push(Token { kind: kind, start: s, end: i })
    } else {
      toks.push(Token { kind: KIND_TEXT, start: i, end: i + 1 })
      i = i + 1
    }
  }
  toks
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `mty test --dir tests`
Expected: PASS — 3 tests. (If `const` or nested `Vec[Vec[U8]]` aren't supported, replace `KIND_*` consts with plain `fn kind_keyword() -> U8 { 1_u8 }` and build keyword comparisons inline; the tests pin behavior, not syntax.)

- [ ] **Step 5: Commit**

```bash
git add src/tokenize.mty src/testutil.mty tests/tokenize_test.mty
git commit -m "feat(editor): minimal .mty tokenizer with token spans (TDD)"
```

---

# Phase 2 — GPU shim full API (Rust, contingent on Phase 0 GO)

Detailed task breakdown; expand each into the same 5-step TDD rhythm during execution. Rust tests use offscreen render-to-texture + pixel assertions (`cargo test -p mighty-ui-sys`).

### Task 2.1: `#[repr(C)]` FFI types + opaque context handle
- Define `MuiColor { r,g,b,a: f32 }`, `MuiEvent { tag: u32, ... }` (tags: None, Char, Key, MouseDown, Scroll, Resize, Close), `MuiKey` enum constants. Add `mui_init(width,height,title_ptr,title_len) -> *mut MuiContext` and `mui_shutdown`.
- Test: a Rust unit test constructs/drops a context headlessly (offscreen `wgpu::Texture` instead of a surface when no window — gate behind an `offscreen` constructor).

### Task 2.2: Rect pipeline + `mui_fill_rect`
- wgpu pipeline drawing solid-color quads in pixel space (ortho projection from window size). Batch rects per frame.
- Test: offscreen-render one red rect at (10,10,5,5); read back the texture; assert those texels are red and a corner texel is the clear color.

### Task 2.3: glyphon text + `mui_draw_text` + `mui_text_measure`
- Load the bundled `fonts/*.ttf`, create a `FontSystem` + `SwashCache` + glyphon `TextAtlas`/`TextRenderer`. `mui_draw_text(ctx,x,y,ptr,len,color)` queues a buffer; `mui_text_measure(ptr,len,&w,&h)` shapes and returns extents.
- Test: measure a known ASCII string returns w>0,h>0; offscreen-render a glyph and assert non-clear texels appear within its box.

### Task 2.4: Frame lifecycle + clip
- `mui_begin_frame` (acquire surface, clear), accumulate draw commands, `mui_end_frame` (submit rects then text, present). `mui_set_clip(x,y,w,h)` sets a scissor rect on subsequent draws.
- Test: with clip set to (0,0,4,4), a rect at (10,10) produces no texels (fully clipped).

### Task 2.5: winit event pump + `mui_poll_event`
- Drive winit 0.30 with `pump_events`; translate `WindowEvent` to a queued `MuiEvent`; `mui_poll_event(ctx,&out ev) -> bool` drains one. Map: keyboard text → Char, named keys (arrows, backspace, enter, ctrl+s) → Key+mods, mouse wheel → Scroll, resize → Resize, close → Close.
- Test: a headless unit test pushes synthetic events into the queue and asserts `mui_poll_event` returns them FIFO then `false`.

Commit after each task: `feat(ui-sys): <task> (TDD)`.

---

# Phase 3 — Integration: the editor loop (contingent on Phase 0 GO)

Wires Phase 1 (Mighty model) to Phase 2 (shim). Manual smoke is the acceptance test; pure-logic helpers remain `mty test`-covered.

### Task 3.1: `src/ffi.mty` — bind the full `mui_*` C ABI
- `extern c { fn mui_init(...) ...; fn mui_poll_event(ev: *MuiEvent) -> Bool; fn mui_begin_frame(...); fn mui_fill_rect(...); fn mui_draw_text(...); fn mui_text_measure(...); fn mui_set_clip(...); fn mui_end_frame(...); fn mui_shutdown(...) }` plus a `#[repr]`-matching Mighty `struct MuiEvent`. Resolve struct/pointer passing per Phase-0 findings (out-param vs return).
- Validate with `mty check src/ffi.mty`.

### Task 3.2: `src/theme.mty` — token kind → color
- `fn color_for(kind: U8) -> MuiColor` mapping keyword/string/comment/number/ident/text to a dark theme. Pure; add a `mty test` asserting distinct colors for keyword vs comment.

### Task 3.3: `src/render.mty` — draw one frame
- `fn render_frame(buf, cursor, vp, theme)`: compute visible range from `vp`; for each visible line call `tokenize_line`, then per token `mui_draw_text` the span bytes at the token color and x-advance via `mui_text_measure`; draw the cursor as a `mui_fill_rect`. Gutter line numbers optional.
- Verified by manual smoke (visual) + a `mty test` on a pure `layout_line` helper that returns per-token x offsets given measured widths (inject a stub measure fn to keep it headless).

### Task 3.4: `src/input.mty` — event → command
- `fn apply_event(ev, buf, cursor, vp) -> (Buffer, Cursor, Viewport, Bool dirty)`: Char → `line_insert` + `cur_right`; Backspace → `line_delete`/`buffer_join_line`; Enter → `buffer_split_line` + cursor to next line; arrows → cursor moves; Ctrl+S → save flag; Scroll → adjust viewport. Pure dispatch over the model.
- `mty test`: feed a synthetic Char event, assert buffer/cursor updated; feed Enter, assert line count grows.

### Task 3.5: `src/main.mty` — wire the loop + file I/O
- Read file path from argv; `fs.read_to_string` → `buffer_from_text`; `mui_init`; loop: drain `mui_poll_event` → `apply_event`; `vp_scroll_to_cursor`; `render_frame`; on Close break; on save flag `fs.write` `buffer_to_text`. (Confirm `fs` API names against `crates/mty-stdlib/src/fs.rs` during the task.)
- Build via `scripts/build.sh` (path chosen by Phase 0). 

### Task 3.6: Manual smoke + acceptance
- Run `scripts/build.sh path/to/some.mty`. Verify the checklist: window opens; file text shows with keyword/string/comment/number colors; arrows move the cursor block; typing inserts; Enter splits; Backspace joins/deletes; wheel scrolls; Ctrl+S writes the file (diff to confirm). Record results.
- Commit: `feat: minimal Mighty editor renders, edits, and saves (sub-project 0 complete)`.

---

## Self-review notes

- **Spec coverage:** render shim (Phase 0 + 2) ✓; editor logic in Mighty (Phase 1) ✓; open/highlight/edit/scroll/save (Phase 3) ✓; Day-1 spike hard gate (Phase 0) ✓; offscreen pixel tests + headless model tests (Phases 1–2) ✓; out-of-scope items (LSP, tabs, terminal, AI, multi-language, web) not present ✓.
- **Contingency:** Phases 2–3 are gated on Phase 0 GO and are intentionally task-level (not 5-step) until the FFI/codegen shape is confirmed; expand them after the gate using the Phase-1 TDD rhythm.
- **Known syntax risks flagged inline** (`push_byte` vs `from_utf`, `const`, `Vec[Vec[U8]]`, struct-return vs out-param FFI) with concrete fallbacks; tests pin behavior so a wrong syntax guess fails loudly rather than silently.
```
