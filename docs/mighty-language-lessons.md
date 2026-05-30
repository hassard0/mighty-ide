# Mighty Language — Lessons from Building the IDE

A living list of concrete ways to improve **Mighty** (the language, `hassard0/Mighty` /
`C:\Users\ihass\stardust`), discovered while building **Mighty IDE** in Mighty itself.
The IDE is the forcing function: every place the language fights us is logged here so it
can be promoted into a `stardust` issue / RFC.

**Legend:** ✅ verified against current source · 🔎 inferred from example comments / docs
(verify before acting) · severity **[P0]** blocks native dogfooding, **[P1]** major
ergonomics, **[P2]** papercut.

_Last updated: 2026-05-29 (developer-workflow features: Run panel + inline git diff + live Settings panel — L33/L34. Prior: LIVE EDITING via a shim-side authoritative text model — the L28 workaround; "Ember Graphite" visual redesign + bundled JetBrains Mono; command palette shim-side registry; L27.)_

> **Terminal note (no NEW limitation):** the integrated terminal (sub-project 5)
> was built without hitting any new language friction — the existing constraints
> already dictated the shape. Per **L21** the rows×cols terminal grid + VT parser
> + PTY live entirely in the shim; Mighty only toggles the panel, forwards a
> codepoint/keycode + mods to `mui_term_*`, and calls `mui_term_pump` +
> `mui_term_draw` each frame. Mighty never holds a grid `Vec`, so L21's
> nested-loop SIGSEGV is sidestepped by construction. Ctrl+` is detected as a
> `Char` event (codepoint 96) with the Ctrl mod set — `Key::Character` emits text
> even when Ctrl is held (a winit/shim behavior the IDE already relied on for
> Ctrl+S, not a Mighty issue).

---

## P0 — Blocks building real native apps in Mighty

### L12. `Vec[T].push(x)` as a statement is a NO-OP — it returns a new value but never mutates the receiver ✅ **[P0]**
Confirmed by reading the interpreter (`crates/mty-ir/src/interp/run.rs:1929` `"push" => ... (Array(xs), Some(v)) => { let mut out = xs.clone(); out.push(v.clone()); Array(out) }`) and by runtime probes under **both** `mty test`, `mty run`, and `mty run --legacy-interp`:
```mty
let mut v: Vec[U8] = Vec.new()
v.push(65_u8)        // statement form — DISCARDED
v.push(66_u8)
v.len()              // == 0  (!!)  push never mutated v
```
The method *returns* the grown `Array` (the in-source comment admits "they can only return the new value — the caller is responsible for storing it back"), but a bare `v.push(x)` statement throws that return away, so the binding never grows. `pop()` and `clear()` have the same return-only behavior. This silently breaks every push-loop in the editor plan (Line/Buffer were specified with statement-form `out.push(...)`).

**Workaround (verified ✅):** capture-and-rebind — `v = v.push(x)`. Despite `push` nominally returning `Unit`, the typechecker accepts `let mut v: Vec[U8] = Vec.new(); v = v.push(65_u8)` and the rebinding grows the vec correctly (`len()`, `v[i]`, `.get(i)` all then work). The whole Phase-1 model is written in this style.

Related gotchas found while probing:
- Empty array literal `[]` does **not** unify with `Vec[U8]` (`MT2001: expected Vec[U8], found [?0; 0]`). Start from `Vec.new()` (a growable Array), not `[]`.
- A non-empty literal `[a, b, c]` is a **fixed-size array** `[T; N]`: `.get(i)` works and reads are fine, but `pop()` and index-assign `v[i] = x` against it do not behave as a growable Vec. Use `Vec.new()` + capture-push to get a real growable buffer.

**Why it matters:** Mutating-method-as-statement is the single most common collection idiom; having it silently no-op (rather than error) is a correctness landmine for any real program, not just the IDE. **Suggested fix:** make `Stmt::Expr(MethodCall{recv, "push"/"pop"/"clear", ..})` write the returned value back to `recv`'s place (the deref-write path the comment mentions, generalized to plain locals), OR give `Vec` true in-place mutation in the value model. Until then, document the `v = v.push(x)` idiom prominently.

### L13. `mty test` / the pipeline has NO package-level module resolution — `use mod.{fn}` of a sibling `src/` module silently resolves to nothing ✅ **[P0]**
The test runner (`crates/mty-stdlib/src/test.rs::run_dir`) walks `tests/`, then for **each file independently** does `parse_source(one_file) → lower → typecheck → run test_* fns`. There is no step that reads `src/`, no manifest-driven module graph, no linking of sibling files. `crates/mty-driver/src/pipeline.rs` operates on a single `ParsedFile`. Probe:
```mty
// tests/x_test.mty
use exp.{add_one}            // exp.mty defines `pub fn add_one(x)->x+1`
fn test() { if add_one(2) != 3 { panic("...") } }   // FAILS: add_one returns a default, not 3
```
The `use` neither errors nor imports — the call resolves to some default and returns the wrong value. Same applies to `mty check` (single PATH) — you can only check one file's closure at a time.

**Workaround:** Phase-1 test files are **self-contained** — each `tests/<mod>_test.mty` inlines the implementation it exercises (mirroring the canonical `src/<mod>.mty`, which is kept separately and validated with `mty check`). This duplicates code between `src/` and `tests/` but is the only way to get green `mty test` runs today.

**Why it matters:** A multi-file Mighty package can't be unit-tested as a package; you cannot test `src/foo.mty` from `tests/foo_test.mty` without copy-pasting. This blocks normal TDD-against-modules and any non-trivial app layout. **Suggested fix:** assemble the package (all `src/**/*.mty` + the test file) into one HIR `Package` before lower/typecheck/run in the test runner, and make `use <localmod>.{...}` resolve against sibling modules (erroring on a genuinely missing symbol instead of returning a silent default).

### L15. Struct field reads ALWAYS return field 0 — `t.b` / `t.col` ignore the field name ✅ **[P0]**
Any read of a non-first named field returns the value of the **first** field instead. Probed under `mty test`:
```mty
struct T3 { a: USize, b: USize, c: USize }
let t = T3 { a: 10, b: 20, c: 30 }
t.a   // == 10  ✅
t.b   // == 10  ❌ (should be 20 — returns field 0)
t.c   // == 10  ❌ (should be 30)
```
Also reproduced with mixed field types (`struct Mixed { name: String, count: USize }; m.count` returns the `String`/first field, not 5). Single-field structs read correctly (`struct One { x }; o.x` is fine), which is why the bug hid until a 2-field type. `read_field(v, i)` in `crates/mty-ir/src/interp/run.rs:1392` indexes correctly, so the defect is upstream: the `field` **index** carried by `Rvalue::FieldRead` (HIR field-name → index resolution, or the projection emitted for `expr.fieldname`) collapses to 0. Tuple positional access (`t.0`/`t.1`) is *also* unavailable — it's a hard parse error (`MT0001: expected L_BRACE, got .`).

**Why it matters:** `struct`s with ≥2 fields are unusable for reads — this guts the most basic aggregate. The plan's `Cursor { line, col }` and `Viewport { first_line, rows }` and `Token { kind, start, end }` all break. **Workaround used:** model small fixed records as a `Vec[USize]`/`Vec[T]` and access positionally by index (`v[0]`, `v[1]`), which the interpreter handles correctly. Cursor = `[line, col]`, Viewport = `[first_line, rows]`, each Token = `[kind, start, end]` flattened into a parallel `Vec`. The public function API (`cur_line`, `cur_col`, ...) is preserved; only the underlying representation changed. **Suggested fix:** fix the field-name→index resolution in HIR lowering (and/or the `expr.field` projection) so `FieldRead.field` is the declared field's ordinal; add the `tuple.N` positional-access grammar. This is the single highest-value correctness fix for writing ordinary Mighty programs.

### L16. Top-level `const` typechecks but evaluates to a default at runtime ✅ **[P1]**
`const KIND_KW: U8 = 1_u8` passes `mty check`, but reading `KIND_KW` in a `test_*` fn yields the wrong value (the `if KIND_KW != 1_u8` guard fired), i.e. the const reference is not resolved to its initializer by the interpreter — it reads a default. **Workaround:** expose each constant as a zero-arg function (`fn kind_keyword() -> U8 { 1_u8 }`) and call it where a value is needed; function calls evaluate correctly. The tokenizer uses `kind_*()` fns instead of `KIND_*` consts. **Suggested fix:** wire top-level `const` items into the interpreter's value environment (resolve `Path`-to-const at eval time), or reject `const` at check time until it's supported so it fails loudly instead of silently returning a default.

### L14. Public functions that allocate must declare `effect alloc` ✅ **[P2-for-us, by-design]**
`pub fn line_insert(...) -> Line { ...Vec.new()/push... }` fails `mty check` with `MT4001: public function 'line_insert' is missing declared effect(s): alloc`. Fix is to annotate: `pub fn line_insert(...) -> Line effect alloc { ... }` (effect clause goes after the return type; `effect a | E` and `!{a}` row forms also exist). This is intended (effects are a public contract per §9), not a bug — logged so the pattern is on record: any `pub` fn in `src/` that constructs a `Vec`/`String` needs `effect alloc`. (Non-`pub` helpers and `test_*` fns in test files don't trip it, which is why the inlined test copies omit it.)



### L1. `mty build` (native Cranelift) lags `mty run` (interpreter); no interpreter fallback in built binaries 🔎 **[P0]**
`mty run` JIT-compiles and "Programs whose MtyIR the native backend can't yet lower fall
back to the tree-walking interpreter transparently." A built binary (`mty build`) has **no
such fallback** — whatever Cranelift can't lower simply won't ship. Documented native gaps
in the examples:
- `examples/05_match_expr.mty`: "Cranelift native codegen only accepts string-literal args
  to `log` today (CODEGEN_V0_2_NOTES 'non-literal string in log/print')."
- `examples/17_unsafe.mty`: "slice-8 wasm codegen mishandles the i32→i64 widening when a U8
  call result is consumed."

**Why it matters:** An IDE (or any real app) calls functions with computed args, prints
computed strings, and runs tight loops. Native parity is the single biggest unlock for
using Mighty to build native software.
**Suggested work:** Treat `mty build` native-backend parity as a release gate — dynamic
args to all calls (incl. FFI), integer widening correctness (U8/U16→I32/I64), and
non-literal `log`/print lowering. Add a conformance suite that runs each example through
**both** `mty run` and `mty build` and diffs behavior.

### L2. `mty build` cannot link ANY user library; link driver is clang-only ✅ **[P0]**
Confirmed by reading `crates/mty-codegen-cranelift/src/object.rs`:
- `link_executable()` invokes the linker as exactly **`<linker> obj.o -o out.exe`** (plus
  `-lc` on unix). It adds **no user libraries and no Mighty runtime archive** — there is no
  flag, manifest key, or env var to inject `mighty_ui_sys.lib`. So `mty build` can *never*
  produce a binary that resolves `extern c` symbols defined in an external lib.
- `find_linker()` order: `STARDUST_LINKER` env → `clang` → `gcc` → `cc` → `lld-link`. It
  uses **GNU/clang `-o` argument syntax**, so MSVC `link.exe` is unusable (wrong syntax),
  and even `lld-link` (in the candidate list) is MSVC-style and would choke on `-o` — a
  latent inconsistency.
- On a clean Windows box with only the MSVC Rust toolchain (no clang), `mty build` prints
  `wrote object target\x.o (no linker found; set $STARDUST_LINKER)` and emits just the COFF
  `.o`. `STARDUST_LINKER` is mentioned only in `MT8008`, not in `mty build --help` or
  getting-started.

**Why it matters:** This is *the* foundation of the IDE (Mighty calling a Rust GPU shim)
and of any native app that binds C/Rust. Today the only path is to manually link mty's
emitted `.o` yourself with clang — undocumented and fiddly.
**Suggested work (high value):**
1. Add a manifest mechanism, e.g. `[build] native-libs = ["mighty_ui_sys"]`,
   `link-search = ["target/debug"]`, that `mty build` appends to the link line.
2. Support MSVC `link.exe`/`lld-link` arg syntax (detect linker flavor; emit `/OUT:` +
   positional libs for MSVC-style, `-o` for GNU-style) so Windows works without clang.
3. Document `STARDUST_LINKER` in `mty build --help` and getting-started.
(Overlaps the v0.36 "static-lib linking + extern c matrix" item — this entry pins the
concrete root cause and the arg-syntax bug.)

### L10. `mty build` never links the Mighty runtime archive → native exes don't build ✅ **[P0]**
A `mty build` object references `mty_runtime_log/_alloc/_panic/_extern_call/_arena_*/...`
(defined `#[no_mangle] extern "C"` in `crates/mty-runtime/src/codegen_abi.rs`), but the link
step links only `obj.o -o out.exe` (+`-lc` on unix) and **does not link any archive
exporting those symbols**. Result: even `fn main(){ log("hi") }` fails to link
(`lld-link: error: undefined symbol: mty_runtime_log`). Only an empty `fn main(){}` links.
Worse, the failure is reported as the misleading `wrote object ... (no linker found; set
$STARDUST_LINKER)` because `build_native` maps a *link error* to `NativeOkNoLinker`
(`mty-driver/src/build.rs:166`).
**Why it matters:** `mty build` → runnable native binary is effectively non-functional for
real programs today; `mty run` (JIT, runtime in-process) is the only working native path.
**Suggested work:** Ship `mty-runtime` as a static archive (or objects) and have
`link_executable` link it; or have codegen emit a self-contained object. Fix the
error-reporting so genuine link failures aren't disguised as "no linker." Document
`STARDUST_LINKER` and make the linker honor MSVC arg syntax when given `link.exe`/`lld-link`.

### L11. `extern c` is not real FFI — a name-only, arg-less, libc-only trampoline ✅ **[P0]**
`extern c fn f(...)` lowers (native codegen) to a local stub that calls
`mty_runtime_extern_call(name_ptr, name_len, args)` (`codegen_abi.rs:120`). That function:
(a) **ignores `args`** (the param is `_args: i64`), (b) dispatches **by name** through a
fixed `ExternRegistry::with_libc()` and returns `i64` via `call_i64(&name)`, and (c) has **no
way to register or `dlopen` arbitrary external symbols**. So a Mighty program *cannot* call
`mui_smoke_add(2, 40)` in our Rust shim — the args are dropped, the symbol isn't in the libc
registry, and it returns 0. (`llvm-nm` confirms `t mui_smoke_add` + `U mty_runtime_extern_call`,
no direct symbol reference.)
**Why it matters:** This is the single blocker for "native app in Mighty that binds a C/Rust
library." It blocked the entire Mighty-IDE native-GUI plan at the spike.
**Suggested work (the big one):** Make `extern c` lower to a **direct call to the named
symbol** (let the linker resolve it), with a real C ABI that passes typed args (i32/i64/f32/
f64/pointers) and returns typed values — i.e. what `extern "C"` means everywhere else. Pair
with L2/L10 so the symbol can actually be linked. (This is the substance behind the v0.36
"extern c matrix" item.) NOTE: the **WASM target** appears to lower `extern`/`extern c` to
real host-import functions (`examples/06` comment: "the slice-8 wasm backend lowers them as
declared host functions"), so the web/WASM path may already support genuine FFI where native
does not — worth confirming, as it changes which IDE substrate is viable today.

---

### L17. `extern c` can pass ONLY scalars from Mighty-owned data — no pointers, structs, or out-params ✅ **[P0]**
v0.36's real `extern c` (the post-L11 direct-call ABI) works, but the *Mighty side* can only originate **scalar** values: `I32`, `I64`, `F32`, `F64`, `U8`/`USize`. Verified end-to-end via `mty build` (a probe linking a C lib + the runtime stub):
- `fn probe_alloc(w: I32, h: I32) -> I64` then `fn probe_sum(handle: I64) -> I32` round-trips a C pointer through Mighty as an `I64` handle and back — **works** (the linchpin for the opaque-handle pattern). `F32` args also pass correctly.
- But every extern-c-matrix row that takes a pointer (`*U8`, row 03/04), a `Str` (row 09), a by-value struct (row 05/07), or an out-pointer (row 04) is marked "works (**wrapper**)": the Mighty source calls a *zero-arg* C entrypoint and **C owns the buffer/struct**. There is no Mighty syntax that yields the address of a Mighty `Vec[U8]`/`String`/local to hand across FFI, and `Str → *U8` coercion is rejected by typeck. `#[repr(C)]` structs can't be constructed-and-passed or returned from Mighty either.

**Consequence for the IDE / any FFI app:** the C ABI must be **scalar-only**. We revised `crates/mighty-ui-sys` to add a parallel `mui_*_s` surface (`abi.rs`): the context is an `i64` handle; colors are four `f32`; **the shim owns all buffers** — text is staged codepoint-by-codepoint (`mui_text_push`/`mui_text_draw`), events are polled to a scalar tag with scalar field accessors (`mui_event_codepoint/_key/_mods`), and file I/O lives entirely in the shim (`mui_load`+`mui_load_byte` for read, `mui_save_push`/`mui_save_commit` for write) because Mighty can pass neither a path string nor a byte buffer. The original struct/pointer ABI in `lib.rs` stays for the Rust GPU tests but is NOT callable from built Mighty.
**Suggested fix:** the v0.37 follow-ups already listed in `extern-c-matrix.md` (Str→*U8 coercion, address-of FFI locals, struct-literal-as-arg) — without at least address-of-local + Str→*U8, FFI apps must push bulk data one scalar at a time.

### L18. `std.fs` is a Rust capability API, not a Mighty-callable surface in built binaries 🔎 **[P1]**
`crates/mty-stdlib/src/fs.rs` exposes `read/read_file/write/write_file/stat/open/...` but they take a `&FsCap` and `&Path`/`&[u8]` — Rust-internal types. There is no Mighty-source path that constructs those, and (per L17) Mighty can't pass a path string across FFI anyway. So **Ctrl+S "save the buffer to disk" cannot be done from Mighty `std.fs` in a `mty build` binary.** The IDE delegates file I/O to the shim instead (the shim's Rust side calls `std::fs`). Needs confirming whether `mty run` (interpreter) exposes a higher-level `fs` to Mighty source.

### L19. `expr as T` numeric casts DON'T convert — the value keeps its original type ✅ **[P0]**
`expr as T` parses as a `HirExpr::Cast` and typeck's Cast arm returns the target type `T` — but the conversion does not actually take effect for numeric types: downstream the expression is still treated as the operand's type. Probed under `mty check`:
```mty
let u: USize = 5
let f: F32 = (u as F32) * 2.0_f32   // MT2017: operator Mul not defined for USize and F32
let b: U8 = (65_i32) as U8          // MT2001: expected U8, found I32
```
i.e. `(u as F32)` is still `USize`, `(i as U8)` is still `I32`. There is also **no implicit numeric promotion** (`I32 + U8` → `MT2017 Add not defined for I32 and U8`) and **no `to_f32`/`to_i32`/… conversion methods** in the stdlib (a `.to_f32()` call type-checks only because method-call typeck is permissive; it has no body). So there is **no working way to convert between integer widths or int↔float** in v0.36.
**Consequence:** keep every value in one type end-to-end. The IDE's edit buffer is `Vec[I32]` (never `U8`) so byte values never need a U8↔I32 cast; and all int→pixel layout is pushed to the shim (`mui_text_draw_line`/`mui_draw_cursor` take integer line/col and compute floats in Rust). A manual `usize_to_i32` that counts up in an `I32` accumulator is the only int-width "conversion" available.
**Suggested fix:** make `HirExpr::Cast` actually emit a numeric conversion in lowering/codegen (sitofp/fptosi/zext/trunc), and/or add `to_f32`/`as_i64`/… stdlib methods. Until then, reject `as` between numeric types at check time so it fails loudly instead of silently keeping the old type.

### L20. Juxtaposed parens `(a)(b)` / `(x - (y))` can mis-parse as a CALL → `MT2008 {integer} is not callable` ✅ **[P1]**
A parenthesised expression immediately followed by another parenthesised group is parsed as a **call** of the first by the second. This bit the bit-test `(half - ((half / 2) * 2)) == 1` — the `(half - (...))` head was treated as a callee applied to the inner parens, yielding `MT2008: value of type {integer} is not callable`. **Workaround:** never juxtapose paren groups; break the expression into intermediate `let`s (`let quarter = half / 2; let even = quarter * 2; let bit = half - even`). **Suggested fix:** only treat `expr(...)` as a call when `expr` is a callable path/closure expression, not for arbitrary parenthesised arithmetic.

### L21. A `Vec` param read deep inside a branchy / nested-loop body is clobbered by native codegen → SIGSEGV ✅ **[P0]**
Discovered building the gutter+scroll render loop. A function `fn draw_buffer(h: I64, buf: Vec[I32], cur: USize, first: USize, rows: USize)` that reads `buf` fine at the **top** (`line_count(buf)`, `line_of(buf, cur)`) then enters `while row < rows { ... if line_idx < total { ... byte_at(buf, line_idx) ... } }` **segfaults at the first `buf` access inside the loop body** — even when that access is `byte_at(buf, cur)` with the very same `cur` that worked at the top, and even before any FFI call in the loop. Bisected with `log` markers under `mty build`: "before draw_buffer" prints, the top-of-fn buf reads succeed, but the first in-loop `byte_at(buf, …)` crashes. The proven-working milestone-3/4 shape — a **single flat `while i < buf.len()` loop** that references `buf` in the *condition* every iteration — never trips it. The trigger appears to be a liveness/register-allocation bug where a `Vec`-typed param that is live across a loop back-edge but only used inside nested branches is dropped/not reloaded.

**Workaround (verified ✅):** structure buffer rendering as ONE flat scan whose loop condition reads the Vec (`while i < buf.len()`), tracking line/col in scalars and emitting draws at line boundaries; do any per-row work that *doesn't* touch the Vec (e.g. gutter line numbers) in a separate flat loop afterward; compute cursor line/col with the flat helper fns after the scan (reuse of `buf` *after* a flat loop is fine). This is how `src/main.mty::draw_buffer` does visible-range rendering for scroll.

**Why it matters:** any non-trivial Mighty program that walks a collection with nested loops + conditionals (i.e. most real code) can hit a silent memory-corruption crash with no diagnostic. **Suggested fix:** audit the Cranelift backend's liveness/spill handling for aggregate (`Vec`/`String`) locals & params that are live across loop back-edges and used only within nested branch arms; add a regression test (flat-top-read then nested-loop-body-read of the same Vec param).

### L28. The `v = v.push(x)` capture-rebind grows NOTHING under native `mty build` — even a single flat loop leaves `v.len()==0` (confirmed codegen bug, NOT the runtime) ✅ **[P0]**
The L12 workaround (`v = v.push(x)` to grow a `Vec`, since bare `v.push(x)` is a no-op) was verified **only under the interpreter** (`mty test` / `mty run`). Under **native `mty build`** it does not work at all: a flat `while` loop that does `v = v.push(byte)` iterates the correct number of times but the `Vec` stays empty (`v.len()==0`). This is exactly the bug that forced the IDE's editor body to render shim-side (`mui_draw_buffer_self` reads the shim's own byte copy) instead of from the live Mighty `buf`.

**Ruled out the runtime first.** The hypothesis was that the IDE's no-op-arena C stub (`vendor/mty_runtime_stub.c`: `arena_push/pop` no-ops, `alloc` a bare `malloc`) broke the arena semantics Mighty's `Vec` grow path expects. So we vendored a **real bumpalo-backed arena runtime** (`crates/mty-rt-abi`, staticlib — thread-local `ArenaStack` of `bumpalo::Bump` frames; `arena_push` pushes/returns depth, `arena_pop` drops the frame, `alloc` allocates on the top frame with a leaked per-thread fallback `Bump` so allocs always succeed) and pointed the IDE at it (`mighty.toml` `[[extern_lib]] mtyrt → vendor/mty_rt_abi.lib`, `build-ide.sh`). **The buffer is STILL empty with the real arena** — so it is NOT a runtime/arena bug. It's in native codegen's `Vec.push` / capture-rebind lowering.

**Minimal standalone repro** (in `repro/`, links the SAME real-arena runtime so the runtime is excluded as a cause):
```mty
// repro/repro.mty (FFI int printer repro_print_i32 supplied by repro/repro_print.c)
fn main() {
  let mut v: Vec[I32] = Vec.new()
  let mut i: USize = 0
  while i < 5 { v = v.push(65); i = i + 1 }
  repro_print_i32(vec_len_i32(v))   // counts v via `while j < v.len()` in an I32 acc (L19)
}
```
Build + run (Windows, clang linker):
```
cd repro
"C:\Program Files\LLVM\bin\clang.exe" -c -O0 repro_print.c -o repro_print.o
"C:\Program Files\LLVM\bin\llvm-ar.exe" rcs repro_print.lib repro_print.o
cp ../target/debug/mty_rt_abi.lib mty_rt_abi.lib
MTY_LINKER="C:\Program Files\LLVM\bin\clang.exe" STARDUST_LINKER="C:\Program Files\LLVM\bin\clang.exe" \
  /c/Users/ihass/stardust/target/debug/mty.exe build repro.mty --out-dir .
./repro.exe
```
**Observed:** `repro: v.len()=0` (expected `5`).

**Confirmed it's the Vec, not the loop or the FFI.** A variant that prints a literal `99` inside the loop body AND counts `v.len()` after prints **five** `99`s then `0` — i.e. the loop runs all 5 iterations and FFI scalar calls inside the loop work fine, but `v` never grew:
```
repro: v.len()=99   (x5, one per iteration)
repro: v.len()=0    (final count)
```
The IDE's own launch probe (`mui_probe_buf_len`, wired after the file-load loop in `src/main.mty::main`) prints the same verdict on a real file: `probe: mty_buf_len=0 shim_load_bytes=37 match=false` for a 37-byte file.

**Consequence / current stance:** the editor body stays rendered shim-side (`mui_draw_buffer_self`) — true live-Mighty-buffer dogfooding is blocked until native codegen grows `Vec` correctly. **Suggested fix (stardust):** native codegen must lower `v.push(x)` (and the `let mut v = ...; v = v.push(x)` rebind) so the returned grown Array is actually written back to the binding's slot and survives the loop back-edge; today the grow is dropped. Add a `mty build` conformance test: build the repro above and assert `v.len()==5` (the interpreter already passes this; the native backend does not). Likely shares a root cause with L21 (aggregate-local liveness across loop back-edges in the Cranelift backend).

**Workaround shipped — the editable buffer + cursor now live SHIM-SIDE (`crates/mighty-ui-sys/src/editor.rs::TextModel`).** Because the Mighty `Vec[I32]` edit buffer can't accumulate under native codegen, live editing was impossible with the buffer in Mighty. So the authoritative text model — a `Vec<String>` of lines plus a cursor / selection / scroll / dirty flag, one per tab — was MOVED into the shim. Mighty drives every edit through scalar `mui_ed_*` ops (`mui_ed_insert_char` / `_backspace` / `_delete` / `_newline` / `_move(dir)` / `_move_to(line,col)`, plus `_cursor_line/_col`, `_line_count`, `_set_scroll`/`_first_visible`, `_dirty`, `_load`/`_save`, `_find_run`, `_complete_request`/`_complete_accept`, `_nav_stream`, `_tab_switch`, `_click`, `_undo_record`/`_undo`/`_redo`, and the body draw `mui_ed_draw(rows)`). The model mutates in place and `mui_ed_draw` renders straight from it each frame, so **editing reflects LIVE on screen** — verified headlessly by `MUI_EDIT_PROBE` (scripts insert/newline/backspace and logs the line count + line lengths growing: `edit-probe: typed="hello" lines 4->5 …`) and exhaustively Rust-unit-tested (`editor.rs` + `tests::editor_abi_drives_live_model_and_undo`). Tab switching is now a plain index change (each tab owns its model) instead of a byte-swap loop; undo/redo are shim-side `TextModel` snapshots. **Move the model back to Mighty once the L28 codegen bug is fixed** — the Mighty side keeps owning the event loop, key routing, command dispatch, and find/diagnostics/tabs/etc., so re-homing the buffer is a localized change.

### L22. `mty check` diagnostics: coarse spans (type errors resolve to the enclosing `fn` start `1:1`), ANSI always on, `check` ≠ full typecheck ✅ **[P2]**
Discovered building the live-diagnostics engine (shim runs `mty check <path>`, parses, exposes scalar getters). Findings for v0.36:
- **Format** (per diagnostic): a header line `[MT<digits>] <Error|Warning>: <message>` followed by an ariadne location line `╭─[<path>:<line>:<col>]` (line/col **1-based**). Diagnostics are separated by blank lines. A clean file prints one line `ok: <path>` and exits 0.
- **Coarse spans:** type-mismatch errors (`MT2001`, `MT2019`, arg-mismatch) report their span as the **enclosing function's start `1:1`**, not the offending token — so an inline underline lands on the `fn` line, not the bad expression. Multiple distinct errors in one fn all report `…:1:1`. The IDE still parses + renders them, but per-error positioning is only as good as the compiler's span.
- **ANSI always emitted:** `NO_COLOR=1` / `TERM=dumb` are **not** honored; output is always SGR-colored, so any parser must strip ANSI (`ESC[ … m`). (`mui-sys/src/diagnostics.rs::strip_ansi`.)
- **`check` is narrower than expected:** an undefined identifier (`log(undefined_thing)`) and a trivial parse glitch (`let =`) both printed `ok:` here — only type-level errors surfaced. So `mty check` is not a full lint pass in v0.36; missing diagnostics aren't a parser bug on our side.
- **No end column:** the report gives only a start col; the engine records `col_end = col_start + 1` so the underline is a visible one-cell marker.
**Suggested fix (stardust):** carry the real expression span into type-error diagnostics (don't collapse to the fn header); honor `NO_COLOR`; widen `check` to report name-resolution/parse errors.

### L23. Native `log(...)` accepts only a string LITERAL → no computed-value tracing from Mighty ✅ **[P1]**
Re-confirmed building the multi-file workspace (tabs + file tree). The IDE wanted to print the live `tab_count` / tree-entry count to stdout as headless launch evidence, but native `mty build` lowers `log` only for string-literal arguments (the CODEGEN_V0_2_NOTES "non-literal string in log/print" gap, first noted in L1). `log(tab_count)` (an `I32`) and any `"prefix" + n` concatenation are both unavailable — Mighty has no string building (L3) and no int→string conversion. **Workaround (verified ✅):** push the print into the shim — a zero-arg-from-Mighty FFI entry (`mui_log_workspace(handle)`) reads the counts shim-side and `println!`s them. Every "show me a computed number" trace in a built Mighty app must round-trip through a Rust FFI printer like this; Mighty `log` is only for fixed string milestones. **Suggested fix:** lower non-literal `log`/`print` args in native codegen (pair with int/float→string formatting in the stdlib), so a built binary can trace computed values without an FFI shim.

### L24. `mty lsp` completion is solid — but a stdio client must (a) byte-count `Content-Length` and (b) stage `didOpen` BEFORE `completion` ✅ **[finding, not a Mighty-source limitation]**
Discovered building the autocomplete dropdown (sub-project 6). `mty.exe lsp` is a full tower-lsp 3.17 server over stdio (`crates/mty-lsp/src/server.rs::run_stdio` → `Server::new(stdin, stdout, socket)`), and its `textDocument/completion` (`completion.rs`) is **good**: it returns the keyword set + every top-level def by name (`DefMap::by_name`) + locals-in-scope + receiver-aware methods after `.`. Live probe at `let|le` returned 171 labels including `let`. So the semantic provider is worth wiring (the IDE merges its labels ahead of buffer words).

Two client-side gotchas that cost real time (both are LSP-client bugs, NOT Mighty issues — logged so the shape is on record for any future Mighty tooling that speaks LSP):
- **`Content-Length` is a BYTE count.** A PowerShell `$json.Length` (UTF-16 code units) gave 75 for a 107-byte body → the server replied `{"error":{"code":-32700,"message":"Parse error"},"id":null}` and answered nothing. The Rust client uses `json.len()` (bytes) and works. (`completion.rs::lsp::frame`.)
- **`didOpen` must land before `completion`.** Firing `initialize`+`initialized`+`didOpen`+`completion` in ONE write burst makes the completion request race ahead of the document open, so the server answers with no result (the doc isn't in its store yet) — observed as 0 labels. **Fix (verified ✅):** stage the writes on a writer thread with brief pauses (`80/40/120 ms`) so `didOpen` settles first; then completion returns 171 labels in ~0.25 s.
- **Blocking-pipe robustness (Windows):** the child's stdout pipe is blocking and the server never closes stdout on its own, so a naive read-with-deadline loop blocks forever (a test hung 492 s until the child was killed). **Fix (verified ✅):** read on a worker thread, stop as soon as the `"id":2` completion response bytes appear, and bound the wait with `recv_timeout`; on timeout KILL the child to force EOF and unblock the reader. The LSP path is best-effort — any spawn/parse/timeout failure silently falls back to the buffer-word provider, so the editor never blocks. (`completion.rs::lsp::semantic_labels_with_timeout`.)
**No Mighty-source limitation** surfaced building sub-project 6: per L21 the candidate list + selection live entirely in the shim (`completion.rs::CompletionEngine`), and Mighty only streams the buffer in (like find), requests at `(line, col)`, moves the selection, and reads the accepted text back to insert via the existing flat `insert_at`/`delete_at` ops — so Mighty never holds the candidate `Vec`.

### L25. `mty lsp` hover + go-to-definition are both solid — same stdio-client discipline as L24 ✅ **[finding, not a Mighty-source limitation]**
Discovered building hover + go-to-definition (sub-project 7). `mty.exe lsp` declares `hoverProvider:true` + `definitionProvider:true` (`crates/mty-lsp/src/server.rs`, dedicated `hover.rs` + `definition.rs`), and both work well over the same staged stdio handshake as completion:
- **Hover** (`textDocument/hover`, `hover.rs`) returns `{"contents":{"kind":"markdown","value":"```mty\n<signature>\n```\n\n_node_: ...\n_token_: ..."},"range":{...}}`. Live probe on an `add(...)` call returned the full ` ```mty\nfn add(a: I32, b: I32) -> I32\n``` ` signature plus node/token info. The shim strips the markdown fences/backticks, wraps to a few short lines, and draws a popup (`nav.rs::wrap_hover` + `HoverState::draw`).
- **Definition** (`textDocument/definition`, `definition.rs`) returns a single `Location` (`GotoDefinitionResponse::Scalar`) — `{"result":{"range":{"end":{...},"start":{"line":N,"character":N}},"uri":"file:///..."}}`. **Wire-order gotcha:** the `range` is serialized BEFORE the `uri`, and `start` comes before `end` inside `range`, so a scanner must anchor the position read at the FIRST `"start"` object and read `uri` separately (`nav.rs::parse_definition`). Live probe on an `add(...)` call resolved to its `fn add` definition on line 0. The shim resolves the `file://` uri to a path (`uri_to_path`, Windows-drive + percent-decode aware) and Mighty either moves the cursor (same file, via canonicalized `paths_equal`) or opens the target as a tab and jumps.
- Same three L24 client gotchas apply verbatim (byte-count `Content-Length`; stage `didOpen` before the request with `80/40/120 ms` pauses; read on a worker thread, stop at the `"id":2` frame, bound with `recv_timeout`, KILL on timeout). Reused wholesale in `nav.rs::lsp::request_with_timeout`.
- **New parsing nuance (logged):** the response stream concatenates the `initialize` result (`id:1`) and the hover/def result (`id:2`); a whole-blob field scrape can match the wrong object (e.g. a capability string's `value`). **Fix (verified ✅):** brace-balance out just the object containing `"id":2` before parsing (`nav.rs::lsp::isolate_response`, string-aware so braces inside JSON strings don't confuse it).

**No Mighty-source limitation** surfaced building sub-project 7: per L21 the hover text + definition target live entirely in the shim (`nav.rs`), and Mighty only streams the buffer in (like completion), requests at `(line, col)`, and reads scalars back (hover availability + a draw call; def path-match + target line/col + an open-target call). F12 is wired through a new `MUI_KEY_F12` named-key code (winit `NamedKey::F12`); Ctrl+K triggers hover; Ctrl+Minus jumps back one stored location.

### L31. mty-lsp implements signatureHelp + rename + codeAction — all three deeper-intelligence features wired with NO new Mighty limitation ✅ **[finding]**
Discovered building deeper language intelligence (signature help / rename / code actions, `language.rs`). `mty.exe lsp` (serverInfo reports `mty-lsp 0.1.0`, log line "mty-lsp initialized (v0.5)") advertises and **fully implements** all three. Verified on the wire:
- **signatureHelp** — `signatureHelpProvider:{triggerCharacters:["(",","],retriggerCharacters:[","]}`. `textDocument/signatureHelp` at a CALL site returns `{"activeParameter":N,"activeSignature":0,"signatures":[{"label":"fn add(a: I32, b: I32) -> I32","parameters":[{"label":"p0"},{"label":"p1"}]}]}`. **Two parsing notes:** (1) the parameter `label`s are placeholder strings (`"p0"`,`"p1"`), NOT the `[start,end]` offset-into-label form, so the active-parameter highlight is computed by locating the actual param substring (`a: I32`) inside the signature label, not by an offset pair; (2) it returns a real signature only at a **call site** — at a function's own definition paren (`fn add(|`) it returns empty. The shim parses `SignatureInformation` (`parse_signature_help`) and draws a popup ABOVE the cursor with the active param highlighted in indigo + the doc line if present (`SigState::draw`). Triggered by typing `(` or Ctrl+Shift+Space; dismissed on `)` / Escape / cursor leaving the line.
- **rename** — `renameProvider:{prepareProvider:true}`. `prepareRename` returns the symbol `Range`; `textDocument/rename` returns a `WorkspaceEdit` in the **`changes` map** shape: `{"changes":{"file:///...":[{"newText":"plus","range":{...}}, ...]}}` (NOT `documentChanges`). A live `add`→`plus` rename returned 2 edits (the `fn add` definition + the one call site). The shim parses both `changes` and `documentChanges` shapes (`parse_workspace_edit`) and applies edits **back-to-front per file** (`apply_text_edits`, sorted by start offset, spliced rightmost-first) so earlier offsets never shift — covered by unit tests for same-line, multi-line, insertion, and Unicode cases. Active file edited in its live model + saved; other files rewritten on disk + their open tab reloaded; focus restored to the original file. F2 opens an inline rename input (centered card, reuses the prompt visual language) prefilled with the identifier under the cursor. A workspace-wide whole-word fallback (`fallback_rename_edits`, active file only, clearly flagged `fallback=true` in the log) is wired but not needed in practice since the LSP rename works.
- **codeAction** — `codeActionProvider:{codeActionKinds:["quickfix","refactor.rewrite","source.fixAll.mighty"],resolveProvider:false}`. `textDocument/codeAction` returns a `result` array of CodeActions/Commands; on a clean line it can be `[]`, and a live probe on a line with a likely-typo (`prnt`) returned **1** real LSP action. The shim parses `title` + optional inline `edit` (`parse_code_actions`) and ALSO appends a synthetic **"Fix all (mty)"** action when `mty fix --help` succeeds — applying it saves the buffer, runs `mty fix --apply <path>` (the real v0.35 bulk-fix-envelope applier; `--help` confirmed), and reloads. Ctrl+. opens the menu (reuses the completion/palette card styling); Enter applies the selected action's `WorkspaceEdit` or runs the mty fixer.

Per L21 ALL state (the parsed signature, the rename buffer + WorkspaceEdit, the code-action list + selection) lives shim-side (`language.rs` + `abi.rs`); Mighty only triggers requests at `(line,col)`, forwards keys to the active overlay, reads scalars back, and calls draw. The LSP client reuses the proven L24 staging discipline verbatim (one generic `language::lsp::request` covering all four methods, `isolate_response` for the `id:2` frame). New input modes follow L29 (flat if/else arms keyed on Mighty-local flags `renaming` / `code_action_open` / `sig_open`); F2 is a new `MUI_KEY_F2` named-key code. No new Mighty-source limitation surfaced.

### L26. `mty fmt` is a no-op stub in v0.36 — exits 0 but never rewrites the file (and is destructive on non-`.mty` input) ✅ **[finding, P2]**
Discovered building the format-document feature (Feature B). `mty fmt --help` advertises "Format .mty files in place (or stdin)" with `--check` / `--stdin` modes, and `mty fmt <path>` exits **0** — but on v0.36 (`stardust` debug build) it does **not** modify the file at all, even for valid, clearly mis-formatted Mighty (collapsed whitespace, `fn  main( )  {` → unchanged; `let x=1` → unchanged). `--check` likewise returns 0 regardless. So the formatter backend appears unimplemented/passthrough in this build.
- **Sharp edge (logged):** `mty fmt` on a file whose contents are NOT valid Mighty (e.g. a `.txt`) is **destructive** — a 6480-byte `examples/long.txt` copy was truncated to **1 byte** by `mty fmt` (still exit 0). So `fmt` should only ever be pointed at parseable `.mty` files.
- **IDE impact:** the wiring is correct and the invocation (`mty fmt <path>`, in place) matches `--help`. The IDE saves the live buffer first, pushes a **pre-format undo snapshot**, runs fmt, reloads via the existing load path, and records the result — so even when fmt is a no-op (or destructive) the user can Ctrl+Z back to the pre-format text. The feature will start producing visible reformatting for free once the compiler's formatter lands; no IDE change needed.
- **Suggested work (Mighty side):** implement the `fmt` backend (or make `fmt`/`--check` report "no formatter" with a non-zero status) and refuse to write when the input fails to parse, so `fmt` can't silently truncate a non-`.mty` file.

**No Mighty-source limitation** surfaced building Feature A (undo/redo): per L21 the entire snapshot history lives shim-side (`history.rs`); Mighty streams its post-edit buffer in (reusing the save/tab-store byte path), and the shim coalesces single-char typing runs / decides whether to push. The one Mighty-grammar reminder that bit again was **L20** — the redo chord condition `((cp==121||cp==89) || ((cp==122||cp==90) && shift))` mis-parsed as a call (`value of type Bool is not callable`); spelling the chords as flat `let`-bound predicate fns (`is_undo_chord` / `is_redo_chord` / `is_format_chord`) fixed it. **Granularity chosen:** one Ctrl+Z undoes a contiguous typing run; the run is broken (→ a fresh undo step) by any non-insert action — newline, Tab char, delete/backspace, cursor move (arrows/click), completion accept, save, format, find-jump, and tab switch (`mui_undo_break`). Buffer-replacement events (tab switch/open/close, go-to-def cross-file) re-seed a fresh per-buffer baseline (`mui_undo_seed_*`).

### L32. Editor power-features (comment/indent/auto-close/bracket-match/duplicate/move-line/word-motion/in-file-replace) fit the shim-side model with NO new Mighty limitation — only the `mods` bit-test patterns (L20) recur ✅ **[finding]**
Discovered adding daily-driver editor features (Ctrl+/, auto-indent on Enter, bracket/quote auto-close + skip-over + pair-backspace, bracket-match highlight, Ctrl+Shift+D duplicate, Alt+Up/Down move-line, Ctrl+Left/Right word motion + Shift-extend, smart Home, Ctrl+H in-file replace). Per L28 the editable text lives in the shim's `TextModel` (`editor.rs`), so every feature is a pure, exhaustively-unit-tested Rust method exposed through a scalar `mui_ed_*` op; Mighty only routes the keybinding and reads scalars back. The bracket-match highlight is drawn **inside `mui_ed_draw`** (the shim already owns the body render and knows the cursor), so no extra Mighty call or per-frame Vec read is needed. The in-file replace bar reuses the prompt/find shim-state pattern (a `ReplaceBar` with two char-vec fields + a focus flag) and one new flat input-mode arm (L29). No new language gap surfaced. Reminders that recur (not new):
- **L20 bit-tests:** the new `Alt` modifier predicate (`alt_held`, bit value 4) and the `Ctrl+Shift+D` / `Ctrl+H` chord predicates must be spelled as flat `let`-bound boolean fns (no juxtaposed paren groups), exactly like the existing `is_undo_chord` family.
- **Smart-insert return contract:** `mui_ed_insert_smart` returns `1` when it auto-closed/skipped (so Mighty must NOT also insert) and `0` to fall back to a plain `mui_ed_insert_char` — a single scalar drives the branch, no struct/tuple needed (sidesteps L15/L27).
- **Auto-close is shim-decided:** because Mighty can't inspect the char to the right of the cursor without a round-trip, the whole "open vs skip-over vs fall-through" decision is made in the shim from the model and returned as the one scalar above; Mighty stays a thin router.

**Multi-cursor (Ctrl+D, the optional feature) was SKIPPED — by design, not a language limit.** The `TextModel` is single-cursor + single-anchor throughout (`insert_char`/`backspace`/`newline`/`move_*`/the `mui_ed_draw` selection + caret path + the undo snapshots all assume one cursor). Adding N cursors would mean a `Vec<(line,col,anchor)>` and re-deriving every edit/draw/undo op to fan out over it — a large, risk-bearing change to the proven single-cursor core for a feature the spec marked "only if it fits cleanly." The foundation it would build on (select-word, select next occurrence) IS shipped as `select_word` / `mui_ed_select_word` (feature 7), so a future multi-cursor pass has its primitives ready. Skipping it keeps the live-edit/undo/render invariants intact.

## P1 — Major ergonomic gaps for real programs

### L3. `String` has no insert / remove / slice / char-indexing ✅ **[P1]**
Confirmed public surface of `std.String` (from `crates/mty-stdlib/src/string.rs`):
`new, with_capacity, from_str, from_utf, push, push_str, len, clear, is_empty, as_bytes,
into_bytes, as_str, to_str, capacity, valid_up_to`.
**Missing:** `insert(idx, ch)`, `remove(idx)`, `split`, substring/slice, `chars()` /
grapheme iteration, `replace`, `find`. `len()` is bytes only; there's no char-index access.

**Why it matters:** Text editing *is* insert/remove-at-a-position. Their absence forced the
IDE to model each line as `Vec[U8]` and rebuild via `push` loops (O(n) per edit). Any text
tooling in Mighty hits this immediately.
**Suggested work:** Add `insert`/`remove`/`split_at`/`slice`/`chars()` to `String`, and a
clear byte-vs-char-vs-grapheme story for indexing.

### L4. `Vec[T]` has no insert / remove at arbitrary index ✅ **[P1]**
Confirmed `Vec` surface (from `examples/26_string_vec.mty` + `vec.rs`): `new, with_capacity,
push, pop, get → Option[T], len, clear`, index read/assign (`v[i]`, `v[i] = x`).
**Missing:** `insert(idx, x)`, `remove(idx)`, `splice`, slicing, iterators.

**Why it matters:** Same as L3 — editing collections mid-sequence is fundamental. Forces
rebuild-by-push patterns everywhere.
**Suggested work:** Add `insert`/`remove`/`swap_remove`/`extend`/`iter` to `Vec`.

### L5. Building a `String` from raw bytes is round-trip-only 🔎 **[P1]**
No `String.push_byte`; appending a known UTF-8 byte means accumulating a `Vec[U8]` then
`from_utf`/`from_utf8`. (Need to confirm exact `from_utf` signature.)
**Suggested work:** `String.push_byte(u8)` (debug-checked UTF-8) and/or a `BytesBuilder`,
so byte-oriented producers (parsers, codecs) don't pay a copy.

### L6. User-defined types use free functions, not methods; mutation needs rebinding 🔎 **[P1]**
Stdlib types have methods (`s.push_str(...)`), but user `struct`s in examples are operated
on by free functions (`area(s: Shape)`), and "Mighty parameters are immutable in name only
— to demonstrate IndexMut we go through a local rebind" (`let mut local = param`). If
user-defined `impl`/methods and `&mut` params aren't available, that's a real ergonomics
gap (it shaped the IDE's `verb_noun(struct, ...)` API style and forced return-the-new-value
everywhere).
**Verify:** Does Mighty support `impl`/methods and `&mut self`/`&mut param` on user types?
If not, that's a high-value addition.

### L7. WASM Component multi-export friction 🔎 **[P1]**
Many examples prefix helpers with `_` specifically to keep them **out** of the WIT export
world, because "the component encoder needs every world export to have a matching core wasm
export, which the slice-8 emit doesn't yet do for non-main fns." Exporting more than `main`
to a component is a sharp edge.
**Why it matters:** The later "Web target" sub-project will export many functions to a
component; this needs to just work.

---

## P2 — Papercuts

### L8. Hex/binary numeric literals lack type suffixes ✅ **[P2]**
`examples/26`: "numeric-literal grammar accepts decimal-with-suffix (`222_u8`) but not
hex-with-suffix yet." So colors/masks must be written in decimal (`222_u8` not `0xDE_u8`).
Painful for graphics/bytecode. (On the v0.36 list.)

### L9. `mty --version` reports `0.1.0`, not the real version ✅ **[P2]**
A debug build from `stardust` (project at v0.30.1) prints `mty 0.1.0`. The CLI version
string isn't wired to the workspace/release version. Trivial fix, but it undermines trust
in `--version` for bug reports.

---

### L27. Stateful editor actions can't be factored into a shared helper that BOTH a key handler and a dispatcher call — no `&mut` params, no multi-return, no struct-field reads (L15) ✅ **[P1]**
Discovered building the command palette (Ctrl+Shift+P). The palette must, on Enter, "dispatch to the SAME code path the keybinding triggers." The clean factoring would be a single `fn do_save(...)`, `fn do_next_tab(...)`, etc., called from both the key handler and the palette dispatch. But most editor actions mutate the main-loop locals `buf: Vec[I32]`, `cur: USize`, `first: USize`, `active: I32` (and flags like `find_nav`, `completing`, `hovering`). In v0.36 a helper cannot:
- take those by `&mut` and write them back (params are immutable in name only — L6), and
- return more than one value (no tuple/struct return that the caller can destructure — `t.b` returns field 0, L15), so a helper can't return the new `(buf, cur, first, active)` set.

So actions whose *whole* work is a single shim call or flag set DID factor cleanly and are shared verbatim (`save_buffer(h, buf)`, `request_completion(...)`, `request_def(...)`, `mui_sidebar_toggle`, `mui_term_open`, the `mui_prompt_open` opener) — both the keybinding and the palette call the identical helper / shim entry. But the buffer-replacing actions (tab next/prev/close, undo, redo, format-reload, go-to-def cross-file) have their flat 5–8-line local-state plumbing **duplicated** between the key handler and the palette dispatch arm, because there's no way to hand the four mutable locals to a helper and get them all back. The shared "code path" is real (the same shim entry + the same edit helpers run), but the local-state shuffle around it is copy-pasted.

**Workaround (used):** keep the single-shim-call/flag actions in shared flat helpers; inline the buffer-replacing arms in the palette dispatch, mirroring the key handler's exact flat sequence (each still bottoms out in a shared helper like `store_tab`/`load_tab_buffer`/`restore_cursor`/`mui_undo`/`mui_format_current`). **Suggested fix:** add `&mut` params for user functions (L6) and/or real multi-value returns + struct-field reads (L15) so a stateful action can live in one helper. (No new shim-side limitation: per L21 the command registry + fuzzy filter + selection live entirely in `palette.rs`; Mighty only opens/types/moves/reads the selected id and draws — it never holds the command Vec. Ctrl+Shift+P is detected as a `Char` event with the Ctrl+Shift mods set, like the existing Ctrl+Shift+I format chord.)

### L29. Mode-routing must be expressed as a flat if/else chain keyed on shim-side state, not a Mighty-side enum/match — confirmed again building the rail panels ✅ **[finding, not a new limitation]**
Building the Source Control + Search activity-rail panels, the main loop now has SIX input modes (palette / prompt / terminal / autocomplete / search-panel / scm-panel) plus the default editor. There is no Mighty-side mode enum + `match`; each mode is one arm of the flat `if ... else if ...` chain in the event loop, and the *authoritative* mode for the two new panels is read back from the shim each iteration (`mui_panel_active(h) == panel_search()`), not stored in a Mighty local. This keeps with L20/L21: all panel state (active panel, search query/replace buffers, commit message, git status list) lives shim-side; Mighty only forwards `Char`/`Key`/`MouseDown` events to `mui_search_*` / `mui_scm_*` and reads scalar getters back. The panels' draw functions are no-ops unless their panel is active, so the per-frame draw is also a flat `if/else if/else` over `mui_panel_active`. **Implication for future panels:** the input-routing chain grows by one arm per mode; there is no cleaner dispatch table available in v0.36, but because each arm bottoms out in shim calls the duplication stays shallow.

### L30. Proportional UI-font match highlighting needs a shim-measured x, not a CHAR_W estimate — minor visual drift in the Search panel ✅ **[finding, P2]**
The Search panel highlights the matched span behind each result preview. The preview is drawn with the proportional UI font (`queue_ui_sized`), but the panel code positions the highlight rect using a fixed per-char advance estimate (`CHROME_FONT_SIZE * 0.55`), the same estimate the breadcrumb uses. For short ASCII previews the indigo highlight lands on the matched word; for long or glyph-varied lines it can drift a few px because proportional glyphs are not a constant width. The editor's own find-highlight (`mui_find_highlight_row`) is pixel-perfect because the editor uses the monospace font with a real `CHAR_W`. **Fix (deferred):** expose a shim text-measure for the preview prefix and position the rect from that. Acceptable as-is for v1 — the highlight reads clearly and is never far off.

### L31. A runtime-switchable theme system fits the shim-side state model cleanly; the only Mighty-side cost is one more flat input-mode arm ✅ **[finding, not a new limitation]**
Adding three live-switchable color themes (Vivid Modern / Aurora Glass / Warm Studio) reinforced L21/L29: ALL theming lives shim-side. The palette + style params (light/dark, glass, shadow color, atmosphere stops) live in a single `theme::Theme` value behind a global `RwLock<Theme>`; the historical `pub const NAME: MuiColor` surface became zero-arg accessor *functions* of the same name (`theme::ACCENT()`), so ~280 draw sites switched with a mechanical `theme::NAME → theme::NAME()` sweep and zero logic changes. Light mode (Warm Studio) is handled by branching the renderer on `theme.is_light` only where the visual logic differs — soft DARK drop-shadows + dark hairlines for elevation on paper (vs the dark themes' white top-highlight), dark ink text, and a paper-tint atmosphere (low-alpha warm washes) instead of additive glow. The Mighty side gained exactly one input-mode arm (`theme_picker_open`) in the flat if/else chain (L29) + a `mui_theme_picker_*` scalar ABI (open/move/apply/cancel/active/draw) and `mui_theme_count/active/set/name_*`; theme names cross the FFI char-by-char (L17). Picker does live preview on Up/Down (re-skins the whole IDE each move) and reverts on Esc — all in `themepicker.rs`, Mighty only forwards keys + reads `mui_theme_picker_active`. Persistence is a 1-line `theme=<slug>` config at `%APPDATA%/mighty-ide/config`, loaded in `build_context` before the first draw (env `MUI_THEME` overrides it for screenshot capture). **No new language limitation:** the global-active-theme + accessor-fn pattern is a clean Rust-side idiom and the scalar ABI mirrors the existing palette/completion engines exactly.

### L32. A streaming LLM client fits the shim-side model via a background thread + a polled shared buffer; Mighty never sees the thread, the socket, or the JSON ✅ **[finding, not a new limitation]**
Adding the AI copilot (Anthropic Messages API, SSE streaming, BYO key) reinforced L21/L29 once more: ALL of it — the `ureq` HTTP+TLS client, the incremental SSE parser, the request-body builder, the transcript + input state, and the chat-panel renderer — lives shim-side in `ai.rs`. The one genuinely new shape is **async I/O without blocking the single-threaded Mighty frame loop**: `mui_ai_send` spawns a `std::thread`, the thread streams deltas into an `Arc<Mutex<StreamInner>>` + an `AtomicBool` "running" flag, and the Mighty loop calls `mui_ai_pump(h)` once per frame (before `begin_frame`) to drain the shared buffer into the transcript and return `1` if it changed. This is exactly the same poll/pump discipline the terminal already uses (`mui_term_pump`) — Mighty never holds a socket, a thread handle, or any JSON; it only forwards `Char`/`Key` events to `mui_ai_input_*`, fires `mui_ai_send`, polls `mui_ai_pump`/`mui_ai_streaming`, and draws. The Mighty side gained one input-mode arm (`ai_focus`) in the flat if/else chain (L29) plus the `mui_ai_*` scalar ABI. The model id is a `const MODEL` in `ai.rs` (default `claude-sonnet-4-6`; fall back to `claude-3-5-sonnet-latest` if it 400s — the API error body is pushed into the transcript so it's debuggable in-panel). Inline-ask (Ctrl+I) reuses the bottom prompt UI (new `PromptKind::Ai`) to collect an instruction, then `mui_ai_send_inline` embeds the active file + selection as context and streams the answer into the panel. The SSE parser is unit-tested against SAMPLE data (multi-chunk + split-across-reads + multi-event + error events); the no-key path, request-body shape, and transcript pump are unit-tested too; a single `#[ignore]`d `live_smoke` test does the one real call (max_tokens 32) when a key is set. **No new language limitation:** the right-docked panel renders on the overlay layer (like the autocomplete/palette cards) and a `MUI_AI_AUTOOPEN` hook seeds a fake transcript + forces it past the no-key gate so a headless screenshot captures the chat UI without a network call.

### L33. LIVE editor-metric preferences (font size / tab width / minimap / wrap) fit the shim-side global-state model; the cost is converting the `const` metrics to accessor fns ✅ **[finding, not a new limitation]**
Adding the Settings panel (font size / tab width / word wrap / minimap / theme) reinforced L31's pattern but for *layout metrics*, not just colors. The editor font size, line height and monospace cell advance were `pub const` in `theme.rs` (`FONT_SIZE`/`LINE_HEIGHT`/`CHAR_W`) and re-exported as `layout::LINE_H`/`layout::CHAR_W` consts. To make font size live-adjustable they became zero-arg accessor **functions** of the same name (`theme::FONT_SIZE()`, `layout::LINE_H()`), each reading a global `RwLock<Settings>` in a new `settings.rs` (mirroring `theme::active()`). A mechanical `NAME → NAME()` sweep across ~9 files updated the call sites; the only structural change was that `const`-context derivations (`TERM_MIN_H = 4.0 * LINE_H`, text.rs's `const FONT_SIZE`) also had to become functions. `CHAR_W`/`LINE_HEIGHT` scale linearly with the font size off a reference ratio, so the gutter/cursor/click math stays aligned at any size automatically (the layout math already routes through these accessors). Tab width feeds the auto-indent unit (`" ".repeat(tab_width)`); minimap toggles the editor's right strip (and frees its reserved width when off); word wrap is a stored pref (true soft-wrap deferred — scoped to the pref + read-back per the brief). The Settings panel + persistence reuse the theme config file via a new `config::save_all()` (theme + `font_size`/`tab_width`/`word_wrap`/`minimap` lines); `save_theme` now delegates to it so the picker no longer clobbers settings. Mighty gained one input-mode arm (`settings_open`, L29) + the `mui_settings_*` scalar ABI (open/active/move/sel/adjust/toggle/draw) + `mui_pref_*` getters. **No new language limitation.** **Test note:** the settings/theme globals are process-wide statics, so unit tests that assert on them must serialize via a shared `settings::TEST_LOCK` (the editor's tab-width-dependent auto-indent tests pin the default under the same lock) — parallel `cargo test` otherwise races the global.

### L34. The Run panel + inline diff reuse the terminal's pump pattern and the diagnostics location parser; both are pure-shim, zero new language friction ✅ **[finding, not a new limitation]**
The Run panel runs `mty run <path>` via `std::process::Command` with piped stdout+stderr, one reader thread per pipe appending into an `Arc<Mutex<Vec<u8>>>`, a joiner thread that signals completion — then `mui_run_pump(h)` drains the buffer into a line list once per frame (exactly the terminal's `mui_term_pump` poll/pump discipline, L32). Each completed output line is scanned for an ariadne `[<path>:<line>:<col>]` location (the same shape `diagnostics::parse_location` recognizes — `strip_ansi` is now `pub` so the Run panel shares it) and, when found, becomes a CLICKABLE entry whose `(file,line,col)` the IDE reads back via `mui_run_click_*` to open the tab + jump. The inline diff view shells `git -C <root> diff [--cached] -- <path>`, parses the unified hunks into a flat `Vec<DiffLine>` (hunk headers, +/-/context with old+new line numbers, `\ No newline` meta) with a pure, unit-tested `parse_unified` (multi-hunk, the single-count `@@ -a +c @@` form, pre-hunk header skipping), and draws read-only in the editor body (green/red row tints, two-column line-number gutter) over `mui_ed_draw`; Escape closes it. Both gained one flat input-mode arm each (`run_focus`, `diff_open`, L29) and a `mui_run_*` / `mui_diff_*` scalar ABI; clicking an SCM row now opens its diff. **No new language limitation** — process spawning, threads, and git all live shim-side; Mighty only toggles, pumps, scrolls, and reads scalars. Screenshot hooks (`MUI_RUN_AUTOOPEN` seeds fake output incl. a clickable diagnostic; `MUI_DIFF_AUTOOPEN` opens a sample diff; `MUI_SETTINGS_AUTOOPEN` opens the panel) render all three headless without external state.

### L35. mty-lsp v0.5 does NOT implement `textDocument/documentSymbol`; the Outline panel uses a shim-side scanner. Three more code-nav surfaces, all pure-shim ✅ **[finding + LSP gap]**
**LSP gap (probed 2026-05-29):** `mty-lsp` v0.5 answers `textDocument/documentSymbol` with JSON-RPC error `-32601 "Method not found"` and omits `documentSymbolProvider` from its `initialize` capabilities. (For reference, the capabilities it DOES advertise: `completionProvider`, `definitionProvider`, `hoverProvider`, `documentFormattingProvider`, `renameProvider{prepareProvider}`, `codeActionProvider{quickfix, refactor.rewrite, source.fixAll.mighty}`, `inlayHintProvider`, `semanticTokensProvider`.) So the **Outline panel uses a shim-side scanner** (`outline.rs::scan_symbols`): a line-oriented, brace-depth, string/comment-aware scan for `fn`/`struct`/`enum`/`agent`/`protocol`/`type`/`impl` (plus top-level `let`/`const`), producing a flat pre-order list with `depth`. The shim keeps a full `parse_document_symbols` (both the hierarchical `DocumentSymbol[]` and flat `SymbolInformation[]` shapes) ready behind `OutlineState::refresh`, which tries the LSP path first and falls back to the scanner, recording which was used (`used_lsp()` — always `false` today). When mty-lsp gains the method, the Outline lights up with server symbols for free.

The Outline panel is a NEW sidebar panel on **rail slot 5** (`PANEL_OUTLINE = 5`, a 6th rail icon — the activity rail grew from 5 to 6 cells; `mui_rail_panel_at_click` and `mui_panel_set` extended to accept slot/panel 5). Two more surfaces shipped alongside it, both reinforcing L21/L29 with zero new language friction:
- **Problems panel** (`problems.rs`): a bottom dock that aggregates `mty check` diagnostics across the active file + every open `.mty` tab (reusing `diagnostics::run_check`/`parse_check_output`), grouped+sorted by `(file, line, col)`, with file-group headers and `severity message code Ln:Col` rows; click-to-jump opens the file + moves the cursor. The status-bar problems chip is now clickable (`mui_status_problems_chip_at_click` -> `mui_problems_open`), and the status bar shows the aggregated error/warning counts once the panel has run. It shares the bottom band with the Run panel (opening one closes the other).
- **Interactive breadcrumb** (`crumbmenu.rs`): the breadcrumb segments are hit-tested by a pure `CrumbLayout::segment_ranges` that reproduces the draw's x-advance math; clicking the file segment opens a folder-files dropdown, clicking the symbol segment opens a document-symbols dropdown (reusing Outline data), both styled like the command palette (rounded elevated card, indigo selection, per-kind icons). The symbol segment of the breadcrumb itself now reflects the symbol under the cursor (driven each frame by `mui_outline_set_cursor`).

All three are pure-shim: Mighty gained the `mui_outline_*` / `mui_problems_*` / `mui_breadcrumb_click[_row]` + `mui_crumb_menu_*` scalar ABIs and a few flat input-mode arms (the crumb dropdown is the highest-priority transient arm, like the palette). Per-kind symbol icons + colors are new vector glyphs (`SymKind::icon()`/`color()`) in the Vivid-Modern palette. Screenshot hooks `MUI_OUTLINE_AUTOOPEN` / `MUI_PROBLEMS_AUTOOPEN` (seeds a representative aggregated set, no subprocess) / `MUI_BREADCRUMB_AUTOOPEN=symbol|file` render all three headless. **No new language limitation.**

## Open questions to resolve as the IDE progresses
- Exact `extern c` signature support: pointers (`*U8`), out-params (`&out T`), passing a
  `Vec`/slice as `(ptr, len)`, returning `#[repr(C)]` structs by value vs. out-param?
  (The Phase-0 spike will answer much of this — record results here.)
- Does native `mty build` handle dynamic FFI calls in a loop? (Phase-0 Gate B.)
- `fs` module API names for read-to-string / write (needed by IDE save/load).
