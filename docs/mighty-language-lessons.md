# Mighty Language — Lessons from Building the IDE

A living list of concrete ways to improve **Mighty** (the language, `hassard0/Mighty` /
`C:\Users\ihass\stardust`), discovered while building **Mighty IDE** in Mighty itself.
The IDE is the forcing function: every place the language fights us is logged here so it
can be promoted into a `stardust` issue / RFC.

**Legend:** ✅ verified against current source · 🔎 inferred from example comments / docs
(verify before acting) · severity **[P0]** blocks native dogfooding, **[P1]** major
ergonomics, **[P2]** papercut.

_Last updated: 2026-05-31 (persisted recent files — L54; snippet mirror placeholders — L53; Explorer file operations + prompt-string staging pressure — L52. Prior: Windows packaging/runtime ABI hardening — L50/L51; multi-language support: config-driven highlighting + a generic, registry-configurable LSP bridge for non-Mighty languages — L35; verified live against rust-analyzer 1.95.0. Developer-workflow features — Run panel + inline git diff + live Settings panel — L33/L34; LIVE EDITING via a shim-side authoritative text model — the L28 workaround; command palette shim-side registry; L27.)_

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

> **Real-mouse UX harness note (no NEW limitation):** this pass fixed stale
> Windows harness geometry for Explorer header actions, bottom-dock preset
> buttons, Source Control refresh timing, and the rail Settings click trace.
> No new Mighty language defect was found; the work stayed in Rust/PowerShell
> because OS-level foregrounding, DWM visible-bounds capture, native file/folder
> pickers, and real mouse-event automation remain host/shim responsibilities.

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

### L18. `std.fs` is a Rust capability API, not a Mighty-callable surface in built binaries 🔎 **[P1] — FIXED v0.45 T1**
`crates/mty-stdlib/src/fs.rs` exposes `read/read_file/write/write_file/stat/open/...` but they take a `&FsCap` and `&Path`/`&[u8]` — Rust-internal types. There is no Mighty-source path that constructs those, and (per L17) Mighty can't pass a path string across FFI anyway. So **Ctrl+S "save the buffer to disk" cannot be done from Mighty `std.fs` in a `mty build` binary.** The IDE delegates file I/O to the shim instead (the shim's Rust side calls `std::fs`). Needs confirming whether `mty run` (interpreter) exposes a higher-level `fs` to Mighty source.

**v0.44 update — PARTIAL fix.** Under `mty run` (interpreter) the host dispatcher in `crates/mty-stdlib/src/host.rs` now accepts the agent-friendly aliases (`read_file`, `read_to_string`, `write_file`, `write_string`, `read_dir`), so `std.fs.read_to_string("path")` lands on the real `std::fs::read_to_string` path. The cranelift codegen still threw `Unsupported` for these methods and forced fallback to interp, so `mty build` was still useless for disk-touching programs.

**v0.45 T1 update — FULLY FIXED.** The marquee v0.45 fix wires `std.fs.*` through a dedicated native runtime ABI on both the cranelift JIT/AOT and LLVM backends. New runtime symbols (registered in `mty_runtime::codegen_abi::symbol_table` and declared as backend imports via `crates/mty-codegen-cranelift/src/runtime_imports.rs`): `mty_runtime_fs_{read, read_to_string, read_dir, write, write_string, append, exists, metadata, create_dir_all, remove_file, remove_dir_all}`. The cranelift `Stmt::EffectInvoke` arm now intercepts every `std.fs.*` (or bare `fs.*` after `use std.fs`) call and routes it to its dedicated symbol via `emit_fs_call` (see `crates/mty-codegen-cranelift/src/lower.rs`); the LLVM backend does the same via `emit_fs_call_llvm`. Read/read_dir methods write a 24-byte `(ptr, len, ok)` slot; write/etc. return `i32 (1=ok, -errno on err)`; metadata writes a 24-byte `{size:u64, mtime_ms:i64, is_file:i8, is_dir:i8}` record. Capability check stays compile-time: a `pub fn` missing `effect fs` still trips MT4001 at typeck before codegen runs. The IDE can now drop its `mui-sys/src/fs.rs` shim and let Ctrl+S call `std.fs.write_string` directly. See `crates/mty-codegen-cranelift/tests/fs_native_v045_t1.rs` (13 tests: roundtrip + capability + all 11 methods), `crates/mty-driver/tests/fs_native_v045_t1.rs` (4 AOT roundtrip tests behind `host-toolchain`), and the stdlib host-dispatch parity test in `crates/mty-stdlib/src/host.rs`. read_dir currently returns a newline-joined `Str` of paths; the iterator-handle ABI (open-handle / next-entry / close-handle) is deferred to v0.46 — IDE call sites that consume the listing eagerly aren't blocked.

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

**v0.42 T2 update — FIXED.** Typeck side was already correct by the time of this report (the MT2017 example actually type-checked clean post-v0.40 T3); the real symptom was that the runtime / native back-ends silently dropped the conversion (interp returned the unchanged `Int(300, I32)` for `300_i32 as U8`; cranelift / LLVM / wasm fell through to bit-preserving coerces or bitcasts for int↔float). v0.42 T2 rewrites the Cast arm in all four backends: cranelift uses `sextend` / `uextend` / `ireduce` / `fcvt_from_*` / `fcvt_to_*_sat` / `fpromote` / `fdemote`; LLVM uses `sitofp` / `uitofp` / `llvm.fpto[su]i.sat` / `fpext` / `fptrunc`; wasm uses `i64.extend_*` / `iN.trunc_sat_f*_*` / `f*.convert_iN_*` / `f*.{promote,demote}_f*`; interp does proper Rust-`as` semantics including the saturating Float→Int policy. **Float→Int overflow is now saturating** (NaN→0, ±inf clamp to dst's min/max) — documented in `docs/reference/casts.md §v0.42 T2`. The IDE can drop its Vec[I32]-only edit buffer / shim-side pixel-layout workarounds.

### L20. Juxtaposed parens `(a)(b)` / `(x - (y))` can mis-parse as a CALL → `MT2008 {integer} is not callable` ✅ **[P1] — FIXED v0.42 T3**
A parenthesised expression immediately followed by another parenthesised group is parsed as a **call** of the first by the second. This bit the bit-test `(half - ((half / 2) * 2)) == 1` — the `(half - (...))` head was treated as a callee applied to the inner parens, yielding `MT2008: value of type {integer} is not callable`. **Workaround:** never juxtapose paren groups; break the expression into intermediate `let`s (`let quarter = half / 2; let even = quarter * 2; let bit = half - even`).

**v0.42 T3 update — FIXED.** Parser now threads a `PrimaryShape::{Callable, NonCallable}` tag through the Pratt expression rules. The postfix-`(` rule only consumes a following `(` as `CALL_EXPR` when the preceding primary is callable-shaped (path / call / field / index / method / lambda / parens wrapping any of those, plus `move` and `CAST_EXPR` for FFI fn-pointers). `(a + b)(c)` and other arithmetic / unary / binary / tuple / array / map / struct / block / control-flow / HTML-literal forms surface a clear `MT0001` parse error ("expected operator before `(` — bind to a `let` first") instead of MT2008. The IDE can drop the intermediate-`let` workaround and the `is_undo_chord`-style boolean predicate fns.

### L21. A `Vec` param read deep inside a branchy / nested-loop body is clobbered by native codegen → SIGSEGV ✅ **[P0] — FIXED v0.41 T3 (locked in v0.42 T1)**
Discovered building the gutter+scroll render loop. A function `fn draw_buffer(h: I64, buf: Vec[I32], cur: USize, first: USize, rows: USize)` that reads `buf` fine at the **top** (`line_count(buf)`, `line_of(buf, cur)`) then enters `while row < rows { ... if line_idx < total { ... byte_at(buf, line_idx) ... } }` **segfaults at the first `buf` access inside the loop body** — even when that access is `byte_at(buf, cur)` with the very same `cur` that worked at the top, and even before any FFI call in the loop. Bisected with `log` markers under `mty build`: "before draw_buffer" prints, the top-of-fn buf reads succeed, but the first in-loop `byte_at(buf, …)` crashes. The proven-working milestone-3/4 shape — a **single flat `while i < buf.len()` loop** that references `buf` in the *condition* every iteration — never trips it. The trigger appears to be a liveness/register-allocation bug where a `Vec`-typed param that is live across a loop back-edge but only used inside nested branches is dropped/not reloaded.

**Workaround (verified ✅):** structure buffer rendering as ONE flat scan whose loop condition reads the Vec (`while i < buf.len()`), tracking line/col in scalars and emitting draws at line boundaries; do any per-row work that *doesn't* touch the Vec (e.g. gutter line numbers) in a separate flat loop afterward; compute cursor line/col with the flat helper fns after the scan (reuse of `buf` *after* a flat loop is fine). This is how `src/main.mty::draw_buffer` does visible-range rendering for scroll.

**Why it matters:** any non-trivial Mighty program that walks a collection with nested loops + conditionals (i.e. most real code) can hit a silent memory-corruption crash with no diagnostic.

**v0.42 T1 update — FIXED, root cause was NOT liveness/regalloc.** v0.41 T3's auto-arena-push at `main` entry (`crates/mty-codegen-cranelift/src/lower.rs::lower_blocks` lines 836-863) fixed this side-effect-free. The actual root cause: `mty_runtime_alloc` returns 0 when no arena frame is active; a bare `fn main()` had no surrounding `arena {}`, so `Vec.new()` got NULL and every nested deref SIGSEGV'd. v0.42 T1 verified the fix end-to-end against the IDE's `repro/` reproducer, ported the same fix to the LLVM backend, and added 6 JIT + 4 native-binary regression tests (`crates/mty-codegen-cranelift/tests/vec_liveness_v042.rs` + `crates/mty-driver/tests/vec_liveness_native_v042.rs`) so it can't silently regress.

**v0.45 T4 correction — NOT FULLY FIXED.** Reopened. `l28_helper_param_grow_returns_grown_vec` passes under default cargo profile (`debug = 2`) but SIGSEGVs with `STATUS_ACCESS_VIOLATION` (0xC0000005) when built under `CARGO_PROFILE_DEV_DEBUG=0` + `CARGO_PROFILE_TEST_DEBUG=0`. This is the GHA Ubuntu profile from PR #22 (codex/ci-disk-headroom) — meaning the v0.42 GHA SIGSEGV at release time was a real codegen bug, not disk pressure as v0.44 release notes assumed. The auto-arena-push fix only worked because debug=2 metadata kept some pointer-shaped value alive that debug=0 strips. v0.45 T5 is the actual fix.

### L28. The `v = v.push(x)` capture-rebind grows NOTHING under native `mty build` — even a single flat loop leaves `v.len()==0` (confirmed codegen bug, NOT the runtime) ⚠️ **[P0] — REOPENED v0.45 T4, real fix in v0.45 T5**
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

**Workaround shipped — the editable buffer + cursor now live SHIM-SIDE (`crates/mighty-ui-sys/src/editor.rs::TextModel`).** Because the Mighty `Vec[I32]` edit buffer can't accumulate under native codegen, live editing was impossible with the buffer in Mighty. So the authoritative text model — a `Vec<String>` of lines plus a cursor / selection / scroll / dirty flag, one per tab — was MOVED into the shim. Mighty drives every edit through scalar `mui_ed_*` ops (`mui_ed_insert_char` / `_backspace` / `_delete` / `_newline` / `_move(dir)` / `_move_to(line,col)`, plus `_cursor_line/_col`, `_line_count`, `_set_scroll`/`_first_visible`, `_dirty`, `_load`/`_save`, `_find_run`, `_complete_request`/`_complete_accept`, `_nav_stream`, `_tab_switch`, `_click`, `_undo_record`/`_undo`/`_redo`, and the body draw `mui_ed_draw(rows)`). The model mutates in place and `mui_ed_draw` renders straight from it each frame, so **editing reflects LIVE on screen** — verified headlessly by `MUI_EDIT_PROBE` (scripts insert/newline/backspace and logs the line count + line lengths growing: `edit-probe: typed="hello" lines 4->5 …`) and exhaustively Rust-unit-tested (`editor.rs` + `tests::editor_abi_drives_live_model_and_undo`). Tab switching is now a plain index change (each tab owns its model) instead of a byte-swap loop; undo/redo are shim-side `TextModel` snapshots.

**v0.42 T1 update — RE-HOME THE BUFFER.** L28 + L21 are FIXED. The shim-side `TextModel` workaround can now move back into Mighty — the codegen no longer drops Vec growth or SIGSEGVs on nested-loop Vec param reads. Mighty side keeps owning the event loop, key routing, command dispatch, and find/diagnostics/tabs/etc., so re-homing the buffer is the localized change anticipated. Suggested next IDE track.

### L22. `mty check` diagnostics: coarse spans (type errors resolve to the enclosing `fn` start `1:1`), ANSI always on, `check` ≠ full typecheck ✅ **[P2] — FIXED v0.42 T6**
Discovered building the live-diagnostics engine (shim runs `mty check <path>`, parses, exposes scalar getters). Findings for v0.36:
- **Format** (per diagnostic): a header line `[MT<digits>] <Error|Warning>: <message>` followed by an ariadne location line `╭─[<path>:<line>:<col>]` (line/col **1-based**). Diagnostics are separated by blank lines. A clean file prints one line `ok: <path>` and exits 0.
- **Coarse spans:** type-mismatch errors (`MT2001`, `MT2019`, arg-mismatch) report their span as the **enclosing function's start `1:1`**, not the offending token — so an inline underline lands on the `fn` line, not the bad expression. Multiple distinct errors in one fn all report `…:1:1`. The IDE still parses + renders them, but per-error positioning is only as good as the compiler's span.
- **ANSI always emitted:** `NO_COLOR=1` / `TERM=dumb` are **not** honored; output is always SGR-colored, so any parser must strip ANSI (`ESC[ … m`). (`mui-sys/src/diagnostics.rs::strip_ansi`.)
- **`check` is narrower than expected:** an undefined identifier (`log(undefined_thing)`) and a trivial parse glitch (`let =`) both printed `ok:` here — only type-level errors surfaced. So `mty check` is not a full lint pass in v0.36; missing diagnostics aren't a parser bug on our side.
- **No end column:** the report gives only a start col; the engine records `col_end = col_start + 1` so the underline is a visible one-cell marker.
**Suggested fix (stardust):** carry the real expression span into type-error diagnostics (don't collapse to the fn header); honor `NO_COLOR`; widen `check` to report name-resolution/parse errors.

**v0.42 T6 update — all three fixed.** (1) Type-error spans now carry the offending expression's real span — two errors in the same fn report two distinct `:line:col` positions. (2) `NO_COLOR=<anything>` and `TERM=dumb` suppress ANSI SGR escapes through `ariadne::Config::with_color`. The IDE can drop `strip_ansi` in `mui-sys/src/diagnostics.rs` and set `NO_COLOR=1` (or `TERM=dumb`) before spawning `mty check`. (3) `mty check` now runs parse + name-resolution phases and emits their diagnostics — `let = 42;` and `log(undefined_thing)` both produce MT-coded errors and exit non-zero instead of printing `ok:`. End-to-end tests in `crates/mty-cli/tests/cmd_check.rs`.

### L23. Native `log(...)` accepts only a string LITERAL → no computed-value tracing from Mighty ✅ **[P1] — FIXED in v0.42 T4**
Re-confirmed building the multi-file workspace (tabs + file tree). The IDE wanted to print the live `tab_count` / tree-entry count to stdout as headless launch evidence, but native `mty build` lowers `log` only for string-literal arguments (the CODEGEN_V0_2_NOTES "non-literal string in log/print" gap, first noted in L1). `log(tab_count)` (an `I32`) and any `"prefix" + n` concatenation are both unavailable — Mighty has no string building (L3) and no int→string conversion. **Workaround (verified ✅):** push the print into the shim — a zero-arg-from-Mighty FFI entry (`mui_log_workspace(handle)`) reads the counts shim-side and `println!`s them. Every "show me a computed number" trace in a built Mighty app must round-trip through a Rust FFI printer like this; Mighty `log` is only for fixed string milestones. **Fixed in v0.42 T4 ✅:** native codegen (cranelift + LLVM) now dispatches `log()`/`print()` on each operand's SIR type via a typed runtime surface (`mty_runtime_log_i32/_i64/_u32/_u64/_usize/_f32/_f64/_bool` + matching `_print_*`). Multi-arg `log("count=", n)` lowers to `print_str ; print_sep ; print_i32 ; print_newline`. `n.to_str()` on any scalar (`I8..I64`/`U8..U64`/`USize`/`F32`/`F64`/`Bool`/`Char`) returns a real `Str` aggregate via `mty_runtime_fmt_*`. `Str + Str` concat goes through `mty_runtime_str_concat`. The IDE no longer needs `mui_log_workspace`-style FFI printers for trace output — `log(n)` and `log("count=" + n.to_str())` work directly in built binaries (cross-backend conformance: same output under `mty run` JIT and `mty run --legacy-interp`).

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

### L35. Multi-language support: config-driven highlighting + a generic LSP bridge — entirely shim-side, no new Mighty limitation ✅ **[finding]**
Discovered generalizing the IDE beyond Mighty (multi-language highlighting + a configurable LSP bridge; `langdetect.rs`, `syntax.rs`, `lspregistry.rs`, `lspclient.rs`). Per L17/L21 this is a pure-shim change: Mighty still only routes keys and reads scalars; the active language is detected from the file path **shim-side** (`langdetect::detect`, wired into `sync_active_path` / `mui_path_commit`) and cached on `MuiContext`. The highlighter became a per-language `SyntaxConfig` table (keywords / line+block comments / string rules / number rule / PascalCase-as-type), and the status-bar pill reads `ctx.language.display_name()`. Mighty's own config is unchanged so the `.mty` path doesn't regress. No new Mighty-source friction — the work never crossed the FFI boundary in a new way.

Findings worth recording for any future LSP tooling (not Mighty-source limitations):
- **The Mighty `mty lsp` path stays bespoke; everything else routes through one generic client.** The three existing clients (`completion`/`nav`/`language`) keep spawning `mty lsp` with `languageId:"mighty"`; non-Mighty languages go through a new `lspclient` that takes a registry-resolved `ServerSpec` + the language's `languageId`. The ABI entry points branch on `ctx.language == Mighty` so the proven Mighty handshake is byte-for-byte unchanged.
- **Registry + override file.** `lspregistry::server_for(lang)` maps language → server (rust→`rust-analyzer`, python→`pyright-langserver --stdio`, ts/js→`typescript-language-server --stdio`, go→`gopls`, c/cpp→`clangd`, …), overridable per-language via `%APPDATA%/mighty-ide/lsp.toml` (`rust = "/path/to/ra"`, or `rust = off` to disable). A server is only used when its binary is found on PATH (`which`, PATHEXT-aware on Windows) — **uninstalled → silently no LSP** (highlighting + editing still work; never block/crash).
- **Heavyweight servers index on `initialize` and need a real workspace root.** Verified live against **rust-analyzer 1.95.0**: a bare `.rs` file with no `Cargo.toml` makes rust-analyzer emit `window/showMessage` "Failed to discover workspace" and never answer the request. So the generic client sends `initialize` with a real `rootUri` (the file's parent dir) + our `processId`, and uses **larger settle pauses** than the Mighty client (250/80/350 ms vs 80/40/120 ms) because rust-analyzer/gopls take longer to ingest `didOpen`. Practical consequence: per-file completion/hover for these servers is best at a project root; an opened loose file degrades gracefully to highlighting only.
- **Non-Mighty diagnostics come from `publishDiagnostics`, not a `check` subprocess.** Mighty keeps `mty check` (L22); other languages open the doc on their server and the shim parses the `textDocument/publishDiagnostics` notification (`lspclient::parse_publish_diagnostics`) into the same `Diag` shape the squiggle/Problems UI already consumes (severity 1→error, 2/3/4→warning; `range.start` line/char 0-based).
- **Guarded integration test ran for real.** `lspclient::tests::rust_analyzer_handshake_if_present` spawns the installed `rust-analyzer` against a temp Cargo project and asserts the initialize handshake completes (full server capabilities returned) without blocking; it skips cleanly when rust-analyzer isn't on PATH. **It ran and passed** here (rust-analyzer 1.95.0 from the rustup component).
- **Test hygiene (not Mighty):** the config / theme-picker / settings-panel persistence tests all `std::env::set_var("APPDATA", tmp)` — a *process-global* mutation — so under default parallelism they could read back each other's config dir and flake. Unified them all on the single `settings::TEST_LOCK` mutex, acquired **before** the `set_var`.

### L26. `mty fmt` is a no-op stub in v0.36 — exits 0 but never rewrites the file (and is destructive on non-`.mty` input) ✅ **[finding, P2]**
Discovered building the format-document feature (Feature B). `mty fmt --help` advertises "Format .mty files in place (or stdin)" with `--check` / `--stdin` modes, and `mty fmt <path>` exits **0** — but on v0.36 (`stardust` debug build) it does **not** modify the file at all, even for valid, clearly mis-formatted Mighty (collapsed whitespace, `fn  main( )  {` → unchanged; `let x=1` → unchanged). `--check` likewise returns 0 regardless. So the formatter backend appears unimplemented/passthrough in this build.
- **Sharp edge (logged):** `mty fmt` on a file whose contents are NOT valid Mighty (e.g. a `.txt`) is **destructive** — a 6480-byte `examples/long.txt` copy was truncated to **1 byte** by `mty fmt` (still exit 0). So `fmt` should only ever be pointed at parseable `.mty` files.
- **IDE impact:** the wiring is correct and the invocation (`mty fmt <path>`, in place) matches `--help`. The IDE saves the live buffer first, pushes a **pre-format undo snapshot**, runs fmt, reloads via the existing load path, and records the result — so even when fmt is a no-op (or destructive) the user can Ctrl+Z back to the pre-format text. The feature will start producing visible reformatting for free once the compiler's formatter lands; no IDE change needed.
- **Suggested work (Mighty side):** implement the `fmt` backend (or make `fmt`/`--check` report "no formatter" with a non-zero status) and refuse to write when the input fails to parse, so `fmt` can't silently truncate a non-`.mty` file.

**v0.42 T5 update — destructive truncation FIXED, formatter still stub.** Picked the "safety fix" path: `mty fmt` (and `--check`/`--stdin`) now refuses non-`.mty` extensions, refuses parse failures, refuses files whose tree is empty but contains non-whitespace input. The 6480-byte truncation case is eliminated — the file stays unchanged with a clear `refusing — \`mty fmt\` only formats \`.mty\` files` stderr message and non-zero exit. The actual canonical-formatter work is deferred (would force reformatting 65+ pre-push-gated `.mty` files in the repo). 10 tests pin the new behavior + agent surface mirrors the same guards.

**No Mighty-source limitation** surfaced building Feature A (undo/redo): per L21 the entire snapshot history lives shim-side (`history.rs`); Mighty streams its post-edit buffer in (reusing the save/tab-store byte path), and the shim coalesces single-char typing runs / decides whether to push. The one Mighty-grammar reminder that bit again was **L20** — the redo chord condition `((cp==121||cp==89) || ((cp==122||cp==90) && shift))` mis-parsed as a call (`value of type Bool is not callable`); spelling the chords as flat `let`-bound predicate fns (`is_undo_chord` / `is_redo_chord` / `is_format_chord`) fixed it. **Granularity chosen:** one Ctrl+Z undoes a contiguous typing run; the run is broken (→ a fresh undo step) by any non-insert action — newline, Tab char, delete/backspace, cursor move (arrows/click), completion accept, save, format, find-jump, and tab switch (`mui_undo_break`). Buffer-replacement events (tab switch/open/close, go-to-def cross-file) re-seed a fresh per-buffer baseline (`mui_undo_seed_*`).

### L32. Editor power-features (comment/indent/auto-close/bracket-match/duplicate/move-line/word-motion/in-file-replace) fit the shim-side model with NO new Mighty limitation — only the `mods` bit-test patterns (L20) recur ✅ **[finding]**
Discovered adding daily-driver editor features (Ctrl+/, auto-indent on Enter, bracket/quote auto-close + skip-over + pair-backspace, bracket-match highlight, Ctrl+Shift+D duplicate, Alt+Up/Down move-line, Ctrl+Left/Right word motion + Shift-extend, smart Home, Ctrl+H in-file replace). Per L28 the editable text lives in the shim's `TextModel` (`editor.rs`), so every feature is a pure, exhaustively-unit-tested Rust method exposed through a scalar `mui_ed_*` op; Mighty only routes the keybinding and reads scalars back. The bracket-match highlight is drawn **inside `mui_ed_draw`** (the shim already owns the body render and knows the cursor), so no extra Mighty call or per-frame Vec read is needed. The in-file replace bar reuses the prompt/find shim-state pattern (a `ReplaceBar` with two char-vec fields + a focus flag) and one new flat input-mode arm (L29). No new language gap surfaced. Reminders that recur (not new):
- **L20 bit-tests:** the new `Alt` modifier predicate (`alt_held`, bit value 4) and the `Ctrl+Shift+D` / `Ctrl+H` chord predicates must be spelled as flat `let`-bound boolean fns (no juxtaposed paren groups), exactly like the existing `is_undo_chord` family.
- **Smart-insert return contract:** `mui_ed_insert_smart` returns `1` when it auto-closed/skipped (so Mighty must NOT also insert) and `0` to fall back to a plain `mui_ed_insert_char` — a single scalar drives the branch, no struct/tuple needed (sidesteps L15/L27).
- **Auto-close is shim-decided:** because Mighty can't inspect the char to the right of the cursor without a round-trip, the whole "open vs skip-over vs fall-through" decision is made in the shim from the model and returned as the one scalar above; Mighty stays a thin router.

**Multi-cursor is now shipped; no new Mighty-source limitation surfaced.** The original pass skipped it because the single-cursor `TextModel` made the change too broad for that sprint. The later refactor moved the editor model to a primary caret plus `Vec<Caret>` in the shim, then exposed scalar ABI calls for `Ctrl+D`, `Ctrl+Alt+Up/Down`, Alt+Click, multi-edit insertion, deletion, newline, and motion. Mighty still stays a thin router: it declares the flat externs, routes the chords/click modifier, and lets the shim fan edits out across every caret. The only language reminder is the same L20 bit-test constraint for chord predicates; no new language gap was created.

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

### L30. Proportional UI-font match highlighting needs a shim-measured x, not a CHAR_W estimate ✅ **[fixed, P2]**
The Search panel highlights the matched span behind each result preview. The preview is drawn with the proportional UI font (`queue_ui_sized`), but the old panel code positioned the highlight rect using a fixed per-char advance estimate (`CHROME_FONT_SIZE * 0.55`), the same estimate the breadcrumb uses. For short ASCII previews the indigo highlight usually landed on the matched word; for long or glyph-varied lines it could drift a few px because proportional glyphs are not a constant width. The editor's own find-highlight (`mui_find_highlight_row`) is pixel-perfect because the editor uses the monospace font with a real `CHAR_W`.

**v0.3.0 update:** fixed in `panels.rs`: result previews still truncate by
estimated capacity, but the highlight x/width now comes from
`Text::measure_ui_sized(prefix, chrome)` and `Text::measure_ui_sized(match,
chrome)`, matching the actual shaped UI font instead of a constant advance.

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

### L36. Inline AI ghost-text (Copilot-style) is a non-streaming variant of L32; the only new shape is a debounce timer + a generation-id cancel, all shim-side ✅ **[finding, not a new limitation]**
Inline ghost-text completions (`ghost.rs` + `ghostabi.rs`) reuse the L32 background-thread + polled-slot pattern, but the request is a SINGLE non-streaming Anthropic call (`stream:false`, `max_tokens` 120) and the result is rendered as a DIM overlay rather than streamed into a transcript. The genuinely new pieces are pure shim-side logic: (1) a **debounce timer** — `mui_ghost_arm` sets a `~450ms` deadline (`Instant`), and `mui_ghost_tick` (called each frame) only fires the request once `now >= deadline` and nothing is in flight, so a fast typist never fires; (2) a **generation-id cancel** — every edit/move/dismiss bumps a `u64` generation, the request captures the generation at send time, and `mui_ghost_poll` drops any finished result whose generation no longer matches (so a stale completion never appears); (3) the **FIM prompt builder** — `split_at_cursor` + prefix/suffix line-windowing (last 80 / first 30) with `<CURSOR>` markers, plus a `strip_fences` post-filter for the model's stray ```` ``` ```` wrap. Cost discipline is enforced by all three: debounced, one outstanding request max, capped tokens, cancelled aggressively. The `inline_ai` setting (default ON, but `GhostState::enabled()` ANDs it with `api_key().is_some()` so it's a silent no-op without a key) joins the Settings panel as a 6th row (the row count + Theme index shifted 5→6/4→5, and a peer test had to pin defaults under `TEST_LOCK` AFTER `build_context` since `build_context` calls `settings::load_into_active()` and a parallel settings test can write a persisted `tab_width` that races the global). Mighty gained the `mui_ghost_*` scalar ABI (arm/tick/poll/has/accept/accept_word/dismiss/force/draw/enabled), a `mui_ghost_force` chord (Alt+\), Tab-to-accept (when a ghost is shown, else normal Tab), Ctrl+Right partial-accept-one-word, and dismiss-on-(Esc / any edit / any cursor move / click) calls threaded into the existing flat key chain (L29). The dim multi-line overlay paints in `mui_ghost_draw` over `mui_ed_draw`: the first line continues after the cursor column, following lines render dimmed below at column 0 (non-destructive — the real buffer is never touched until accept, which reuses the editor `insert_char` path). Unit tests cover the FIM windowing, fence stripping, the debounce/generation-id cancel (stale ignored, fresh adopted), full + word accept, and the no-key no-op path; `MUI_GHOST_AUTOOPEN` seeds a fake multi-line ghost for the headless screenshot (`31-ghost.png`). **No new language limitation.**

### L37. Multi-cursor fits the shim-side model with ZERO new language friction — but ONE extra `else if` arm in the editor key ladder overflows the mty compiler's parse stack ⚠️ **[finding + compiler limitation, P2]**
Multi-cursor (multiple simultaneous carets / selections) refactored `editor.rs`'s `TextModel` from a single `(cur_line, cur_col, anchor)` triple to a `Vec<Caret>` with `carets[0]` PRIMARY. The refactor is **byte-identical for one caret**: every legacy single-cursor method redirects its field access to `carets[0]` (a mechanical `self.cur_line → self.carets[0].line` sweep), so all 444 pre-existing tests pass unchanged. Multi-caret edits run an existing single-caret op once per caret, **back-to-front** (highest doc position first, so earlier carets' offsets stay valid), translating the other carets by the active caret's net `(line, col)` displacement + line-count delta; carets that collide after a motion/edit merge (sort + dedup by position). Ctrl+D (`add_caret_next_occurrence`) selects the word on the first press, then adds a caret on each next occurrence (wrap-around) on repeats; Ctrl+Alt+Up/Down add column-block carets; Esc collapses to primary. Multi-caret undo is FREE — the existing `mui_ed_undo_record` snapshots the whole `TextModel` (now incl. the caret set via `#[derive(Clone)]`), so one edit = one checkpoint that restores carets too. All pure-shim; Mighty gained the `mui_ed_caret_*` / `mui_ed_add_caret_*` / `mui_ed_collapse_carets` / `mui_ed_*_multi` scalar ABI and routes edit/motion keys through the `_multi` variants.

**⚠️ Compiler limitation hit:** the editor's giant `Char`-event `if / else if / …` ladder (~219 arms across the file, ~70 in the editor branch) is parsed/typechecked by **recursive descent that recurses once per `else`**. The baseline built with no margin — adding just **one** net-new top-level `else if` arm (a separate Ctrl+D arm next to the Ctrl+Shift+D duplicate arm) made `mty build` die with `thread 'main' has overflowed its stack` (no source span — a generic `src/main.mty:1:1`). **Fix:** don't grow the ladder — FOLD related chords into ONE arm and dispatch inside it. Ctrl+D and Ctrl+Shift+D now share a single `is_d_chord` arm that branches on `shift_held(mods)` internally, keeping the arm count flat. Nested `if/else` *inside* an existing arm (e.g. Ctrl+Alt+Up vs Alt+Up vs plain Up) is fine — it's net-new *top-level* arms that tip the stack. **Implication:** the editor key ladder is at its practical ceiling for v0.36; every future chord must fold into an existing arm, not extend the chain. (Can't raise mty's stack — stardust is read-only.) Screenshot `34-multicursor.png` (`MUI_MULTICURSOR_AUTOOPEN` seeds a Ctrl+D chain of 5 "count" selections).

### L38. Welcome / toasts / Zen confirm the L37 ceiling fix: the `mui_chord` router + folding into existing arms add keybindings with ZERO new top-level ladder arms; toasts + zen are pure-shim ✅ **[finding + L37 confirmation]**
Building the first-impression + feedback UX (Welcome screen, toast notifications, Zen/focus mode) re-validated the L37 discipline end-to-end — `mty build` stayed green with **no new top-level `else if` arms** added to the editor key ladder:
- **The shim chord router shipped (the L37-recommended escape hatch).** A new `mui_chord(handle, cp, mods) -> handled` ABI entry centralizes chords that would otherwise each need a ladder arm. The Mighty side calls it from a **single existing arm**: the old `is_ghost_chord` (Alt+\) arm was widened to `is_alt_chord` (`alt_held && !ctrl_held`, ANY Alt+char) and now just calls `mui_chord`, which dispatches Alt+Z → Zen toggle and Alt+\ → ghost force. New Alt chords are added in Rust (`mui_chord`) with **zero** Mighty-ladder growth. This is the cleanest pattern for future chords — prefer it over folding-and-branching when the chords aren't naturally related.
- **Welcome click routing nested inside the EXISTING mouse-down arm** (a leading `if welcome_act >= 0 { … } else if crumb_on == 1 …` guard prepended to the existing chain) — nesting inside an arm is fine (L37); only top-level siblings overflow. The action dispatch is inlined there (not a helper) because several actions set the caller's `palette_open`/`quickopen_open`/`prompt_kind` locals, which a helper can't write back (L27).
- **Palette/Quick-Open command dispatch extended freely.** Welcome + Toggle-Zen are new palette commands (ids 23/24); their dispatch arms live inside the `palette_open` / `quickopen_open` blocks, NOT the editor ladder, so extending those big if/else chains is unconstrained.
- **Zen layout is a process-global, mirroring `settings`/`theme`.** `layout::region()` is called from ~40 draw/click sites; rather than thread a flag through every one, Zen is an `AtomicBool` in `layout` that `region()` reads (pure `region_chrome(sidebar, zen)` underneath for unit tests). `mui_zen_toggle` flips it; the Mighty draw loop reads `mui_zen_active` once/frame and gates the rail/sidebar/tab-bar/breadcrumb/status-bar draws. The whole editor reflows next frame for free.
- **Toasts use the poll/pump discipline (L32 family) but timer-driven, not thread-driven.** `ToastQueue` holds `Instant`-stamped cards; `mui_toast_tick` (once/frame) drops expired ones; `mui_toast_draw` paints the bottom-right stack on the overlay layer (drawn LAST so it's above every panel/card). Shim code paths push via `MuiContext::push_toast` (Saved/Formatted/Committed/Run-finished/Tests-finished/No-definition/Theme-changed); Mighty-originated toasts go through `mui_toast(kind, msg_id)` with a small predefined-message table (strings can't cross FFI, L17). The run/test "finished" toasts use a one-shot `just_finished` latch read+cleared in the pump ABI (the pump's `bool` "changed" return doesn't distinguish the running→done transition).
- **No new Mighty-source limitation.** All three are pure shim state behind scalar ABIs; the only L37-adjacent cost was the one widened Alt arm + the nested mouse-down guard. New ABI: `mui_welcome_active/_open/_dismiss/_draw/_click/_open_recent`, `mui_toast/_tick/_draw`, `mui_zen_toggle/_active`, `mui_chord`. Screenshots `35-welcome.png` / `36-toasts.png` (`MUI_TOAST_AUTOOPEN` seeds 4 stacked severities) / `37-zen.png` (`MUI_ZEN_AUTOOPEN` seeds a demo buffer + enables zen). Tests: 472 pass (was 458; +14 — toast queue push/expiry/max-visible-drop/severity/dedup/presence, welcome hit-testing + recents + force-open, zen region toggle, welcome-active-when-no-tabs).

## Debugger via `mty dap` (DAP client + UI)

The debugger (`mui_dbg_*` / `mui_bp_*` ABI; modules `crate::dap` + `crate::dapabi`) drives
Mighty's Debug Adapter Protocol server (`mty dap`, v0.32 Track A, source
`stardust/crates/mty-cli/src/cmd/dap.rs`). Same shim-owns-everything, scalar-only shape as
the LSP client: the shim spawns `mty dap` per session, runs the handshake on a worker thread,
and drives a request/response + event loop, posting parsed events back over a channel the
model drains each frame (`mui_dbg_pump`, the Run-panel/terminal poll/pump discipline, L32).

**What `mty dap` actually supports (verified on the wire):**
- `initialize` → capabilities (configurationDone, functionBreakpoints, restart, terminate,
  evaluateForHovers; NOT conditional breakpoints, NOT setVariable, NOT stepBack).
- `launch` → success, then **emits the `initialized` event in response to `launch`, not
  `initialize`** (non-standard ordering — the client sends `launch` right after `initialize`
  and waits for `initialized` before `setBreakpoints` + `configurationDone`).
- `setBreakpoints` / `setFunctionBreakpoints` → verified by source line / `fn:`/`agent:` name.
- `configurationDone` → resumes. `threads` → one thread (id 1, "main").
- `stackTrace` → frames (id/name/line/source.path). `scopes` → a single synthetic "Locals"
  (`variablesReference` 1000). `variables` → flat name/value/type rows (**no structured
  expansion / no child references**). `continue`/`next`/`stepIn`/`stepOut`/`pause`, `evaluate`
  (local-name + simple field access), `restart`, `disconnect`/`terminate`.
- Events: `initialized`, `stopped` (reason entry/breakpoint/step/exception), `output`
  (stdout/stderr), `exited`, `terminated`. **No `continued` event** — the client infers the
  running state from issuing a resume.

**Key gap / workaround (load-bearing):** line breakpoints are *verified* but **do not
reliably FIRE on a plain `continue`** in v0.36 — the program runs to completion and emits
`exited`/`terminated` without a `stopped`. By contrast `launch` with `stopOnEntry:true`
**reliably stops** (reason "entry") with a valid stack, and `next`/`stepIn`/`stepOut` then
work *and* populate locals (e.g. `{"name":"a","type":"int","value":"1"}` after the first
step; locals are empty at the entry stop before any binding runs). So the IDE always launches
with `stopOnEntry:true` (land paused at `main`, then step/continue) and still sends the user's
breakpoints (verified, future-proof). The `stackTrace` `line` tends to report the function's
declaration line rather than the precise current statement, so the current-instruction band
tracks the selected frame's reported line.

The live integration test (`dap::tests::live_dap_session_hits_breakpoint`) spawns `mty dap`
against a tiny program, launches, and asserts a real `stopped` + ≥1 stack frame (it auto-skips
if `mty` can't be spawned). It passes against v0.36: "stopped with 1 frame(s)".

Pure JSON-scan gotcha (same family as the LSP parsers): search for the **key** form
(`"event":`, `"variables":`, `"output":`) not the bare token — `mty dap` emits `"type":"event"`
and `"command":"variables"`, so a search for `"event"`/`"variables"` would match the *value*.

UI: a new "Run and Debug" rail slot (slot 6, bug icon) hosts a sidebar panel — a debug toolbar
(continue/step-over/step-into/step-out/stop), a Call Stack section (click a frame → select +
jump), a Variables section (name = value : type), and a Debug Console (the `output` events).
Gutter clicks toggle breakpoints (red dots); the stopped line gets a distinct amber band + a
gutter arrow. F5 start/continue, Shift+F5 stop, F10 next, F11 stepIn, Shift+F11 stepOut (new
named key codes F5/F10/F11). `MUI_DEBUG_AUTOOPEN` seeds a fake stopped state for a headless
capture (screenshots/24-debug.png). **No new language limitation** — process spawning,
threads, framing all live shim-side; Mighty only routes keys/clicks, pumps, and reads scalars.

## `mty test` runner (v0.36) — Test panel findings

`mty test` (confirmed against `mty.exe test --help` + a live run) discovers
`tests/*.test.mty` (legacy bare `tests/*.mty` still accepted) in the current package and runs
every top-level `fn test_*`. A test PASSES if its body returns normally and FAILS if it traps
(`panic`, OOB, assertion, step-budget). Note: the body must type-check as `Unit` — a bare
`assert x == y` expression fails to compile (`returns Unit, body produces Bool`); use a
non-trapping body or `panic(...)` for the fail case.

**Output format (NOT identical to the task's assumed shape):**
```
test sample.test::test_adds ... ok
test sample.test::test_fails ... FAILED
  reason: trap MT5001: <message>
test sample.test::test_passes_again ... ok

test result: 2 passed; 1 failed; 3 total
```
- There is **no `running N tests` header** and **no duration** in the summary line (we time the
  run shim-side). The per-test name is `<file-stem>::<fn>` (e.g. `sample.test::test_adds`).
- The failure detail is a `  reason: <trap text>` line; it carries **no file:line** location.
- `--format json` emits one object per test + a final summary object (`{"passed":N,"failed":N,"total":N,...}`),
  and the per-test JSON object DOES carry a `"file"` field (absolute path), unlike pretty output.

**Scoping flag:** the discovery-root override is **`--manifest-dir <path>`** (help says
"Defaults to the cwd"), NOT `--dir`. The Test panel runs `mty test --manifest-dir <pkg>` where
`<pkg>` is the nearest ancestor of the active file containing `mighty.toml`.

**No name filtering.** `mty test` rejects any positional arg or `--filter` (`error: unexpected
argument`). So "Run Test at Cursor" re-runs the WHOLE package; the IDE records the cursor's
enclosing `fn test_*` name only to highlight that row. Click-to-jump on a failed row works by
scanning the package's `tests/` dir for the `fn <name>` declaration (the report has no location),
guarding against prefix collisions (`test_adds` vs `test_adds_more`).

UI: a new "Testing" rail slot (slot 7, beaker icon) hosts a sidebar panel — a Run/Re-run + Stop
toolbar, a colored proportional pass/fail summary bar (passed green / failed red) with counts +
duration, and a results tree (green check / red x per row, suite badge, failure message on a
red-railed detail row, failed rows clickable to jump). Ctrl+Shift+T + palette "Run Tests" both
trigger it. `MUI_TEST_AUTOOPEN` seeds a mixed pass/fail result set for a headless capture
(screenshots/25-test.png). **No new language limitation** — same shim-owns-everything shape as
the Run/Debug panels (process spawn + reader threads + incremental parse live in Rust; Mighty
routes keys/clicks, pumps, reads scalars).

## Sticky scroll + Peek definition (2026-05-30)

Two code-reading DX features, both shim-owned + scalar-ABI driven (no new language
limitation — same shape as every other surface).

**Sticky scroll** (`crate::sticky` + `mui_sticky_*`): each frame the shim derives the
enclosing-scope chain of the top visible line from the Outline symbols. The scanner gives
only `(line, depth)`, so a symbol's end is **inferred as the next symbol with `depth <=
self.depth`** (next sibling / dedent), else EOF. A symbol pins when `line < top < end` (its
header has scrolled off AND its body still spans the top). Pinned most-outer-first, capped at
5 (deepest kept). Drawn as an opaque elevated band (BG_4 + ELEVATED gradient + downward drop
shadow + bottom hairline) at the editor-body top, syntax-colored like the source, clickable
to jump. A `sticky_scroll` pref (default ON) gates it, persisted in the shared config.

**Peek definition** (`crate::peek` + `mui_peek_*`, Alt+F12 + palette): reuses the nav
`textDocument/definition` request, then reads a ±window of the target's source (live buffer
when same-file so unsaved edits show, else from disk for cross-file) and draws a rounded,
shadowed inline card below the cursor line with a `file:line` header + highlighted lines.
Esc closes, Enter navigates (same-file cursor move or open-tab). The peek key routing takes
priority while the card is up (captures Esc/Enter/Up/Down).

**Screenshot hooks** `MUI_STICKY_AUTOOPEN` / `MUI_PEEK_AUTOOPEN`: both seed a representative
buffer AND set `edit_probe_lock = true` so the IDE's initial `mui_ed_load` (which reloads
argv[1] from disk) is a no-op and the seeded buffer survives for the capture. The sticky demo
must be **longer than the visible row count** (~34 rows at 860px) or the
`first + rows > total` scroll clamp resets `first` to 0 and nothing pins. Also init the
Mighty loop's `first` from `mui_ed_first_visible(h)` (not hardcoded 0) so a seeded scroll
survives the first frame. Captures: `screenshots/32-sticky.png`, `33-peek.png`.

## Mighty Agents panel (2026-05-30) — `mty inspect` reality + agent-system discovery

The **Mighty Agents** panel (rail slot 8, `PANEL_AGENTS_MTY`; nodes/network icon
distinct from the slot-4 AI-copilot robot head) is a bold agent-first feature no
other IDE has: it statically discovers the workspace's agent system and renders
it as a topology tree, runs an agent program, and (where possible) attaches a
live inspector. Modules: `crate::agents` (pure scanner + snapshot parser) +
`crate::agentsabi` (`AgentTopology` state + Vello draw + `mui_agents_*` ABI).

**What `mty inspect` actually offers (probed live against v0.36):**
- `mty inspect` connects to a runtime **control socket**, opt-in via the
  `MTY_RUNTIME_CONTROL_SOCK` env var set *when the runtime starts*
  (`MTY_RUNTIME_CONTROL_SOCK=<path> mty run prog.mty`, then `mty inspect --sock
  <path>`). Flags: `--sock`, `--agent <id>`, `--json`, `--watch <ms>`, and the
  v0.30 `--cost` mode (reads `~/.mty/observations.sqlite`: total $$, per-
  provider/model/agent breakdown, p50/p95/p99 latency, `--top N`).
- `--json` emits a `RuntimeSnapshot` v1: `{version, worker_count, timestamp_ms,
  agents:[{agent_id, agent_type, supervisor_parent, mailbox_depth,
  mailbox_high_water, in_flight_handler, in_flight_elapsed_ms, budget{...},
  last_messages}]}`. The wire version is locked at 1 (additive only).
- **Windows status update (2026-05-31):** the Mighty language work in
  `hassard0/Mighty#16` adds a Tokio named-pipe backend for
  `MTY_RUNTIME_CONTROL_SOCK`, plus Windows client support in `mty inspect` and
  `mty reload`. The IDE does not need a custom transport: `mui_agents_inspect`
  already shells out to `mty inspect --json`, so the Agents panel can attach on
  Windows once that Mighty build is on PATH and the runtime is started with
  the same socket value.
- **What we shipped:** discovery (static, reliable) + topology + run are fully
  live. Live inspect is wired through `mty inspect --json`, and
  `agents::parse_snapshot` (the `RuntimeSnapshot` v1 parser) is implemented +
  unit-tested, so the panel lights up with live mailbox depths whenever the
  configured Mighty runtime exposes its local control endpoint.

**Discovery findings (real v0.36 agent grammar, vs. the task's assumed shape):**
- `@tool` is **not** a bare attribute — it requires args: `@tool("desc", cap:
  fs.read)` then the `fn`. A bare `@tool` is `MT0001: unexpected token @`. The
  scanner latches a `@tool`-prefixed line then tags the next `fn`.
- There is **no `with llm` keyword** in v0.36; an "LLM-backed agent" is
  conventionally one that `use std.llm` + takes an LLM client and calls it
  (`client.messages(...)`, `AnthropicClient`, `Member.anthropic/openai`). The
  scanner detects LLM-backing heuristically from those signals (and still honors
  a forward-compat `with llm` header marker if it ever lands).
- Real grammar confirmed by stardust examples: `protocol P { Msg(args) -> Ret }`,
  `agent A: P { on Msg(args) -> body }` (also `agent A(ctorargs): P {...}`),
  `supervisor S(strategy: one_for_one) { child x = spawn T(..); on_fail(x){...} }`.
  The sample `examples/agents.mty` (2 protocols, 2 agents incl. one LLM-backed, 1
  supervisor, 1 `@tool`) `mty check`s + `mty run`s clean.

**No new Mighty-source limitation, and the L37/L38 ceiling held:** the panel is a
new rail slot (just a `mui_panel_set` target + a 9th `rail_icons` entry — fine
per L37) + a palette command ("Mighty: Agents", id 25) + an **Alt+G** keybinding
routed through the existing `mui_chord` shim router (zero new top-level `else if`
arms). The body-click + draw arms nest inside the existing mouse-down / draw
chains (nesting is fine; only top-level siblings overflow). `mty build` stayed
green — no parse-stack overflow. The Run action reuses `run::RunPanel`'s
process-spawn/pump discipline verbatim (the embedded panel streams `mty run
<file>` output). All topology state lives shim-side; Mighty refreshes, routes
clicks, runs, pumps, and draws. Tests: 490 pass (was 472; +18 — scanner
agent/protocol/tool/supervisor/edge discovery, LLM detection, brace-in-string
robustness, the `RuntimeSnapshot` JSON parser, and the topology flattening).
Screenshot `screenshots/38-agents.png` (`MUI_AGENTS_AUTOOPEN` seeds the model
from the bundled sample) at 1320x860. ABI added: `mui_agents_refresh/count/
node_kind/node_depth/node_line/node_name_len/node_name_char/edge_count/open_node/
scroll/row_at_click/click_is_run/run/running/pump/run_line_count/inspect/
live_count/live_mailbox/draw`.

**Disk note (Windows, this machine):** C: runs chronically near-full and is shared with
parallel build agents. Build with `CARGO_INCREMENTAL=0`; clear `target/debug/incremental` +
stale `target/debug/deps/mighty_ui_sys-*.{exe,pdb}` test binaries (each ~25-240MB) when a
link fails with os error 112. Do NOT pass a `CARGO_PROFILE_DEV_DEBUG` override mid-session —
it diverges from the cached default profile and forces a full ~6GB dep rebuild.

## Real git client (2026-05-30) — branches / push-pull-fetch / per-hunk stage / blame

Turning Source Control into a real git client (branch switcher, push/pull/fetch,
per-hunk stage/unstage, blame gutter) re-confirmed the L37/L38 ceiling discipline
end-to-end — `mty build` stayed green with **zero new top-level `else if` arms**:

- **One shim-side dispatcher kept BOTH command ladders flat.** The five new Git
  palette commands (ids 26-30: Switch Branch / Push / Pull / Fetch / Toggle Blame)
  do NOT each get a `cmd_*` arm in the palette dispatch AND the quick-open `>`
  dispatch (that would be 10 net-new arms across two near-ceiling ladders).
  Instead each ladder got ONE arm — `else if id >= cmd_git_first() { mui_git_dispatch(h, id) }`
  — and `mui_git_dispatch` fans out shim-side. This is the cleanest pattern when a
  family of commands shares no per-command Mighty state.
- **The branch-switcher overlay shares the breadcrumb-dropdown arm (NESTED, L37).**
  Rather than a new top-level `else if branch_open` arm in the event-routing
  ladder, the existing `else if mui_crumb_menu_active(h) == 1` arm was widened to
  `... == 1 || mui_branch_active(h) == 1` and branches INSIDE on which overlay is
  up. Same trick L38 used; the two overlays have disjoint key handling but live in
  one arm.
- **Status-bar branch click + diff hunk-stage click fold into the mouse-down
  ladder by prepending ONE combined guard** (`if diff_on && diff_hunk>=0 {…} else if branch_seg==1 {…} else if welcome_act>=0 {…`).
  The mouse-down ladder has margin (L38 already prepended the Welcome guard there).
- **Alt+B (toggle blame) routes through the existing `mui_chord` shim router** — no
  Mighty-side change beyond the already-widened Alt arm.
- **Per-hunk staging is pure shim logic + git plumbing.** The diff parser already
  tagged each display line; adding a `hunk: i32` field let `reconstruct_hunk_patch`
  rebuild a byte-exact minimal unified diff (`--- a/<path>` / `+++ b/<path>` /
  `@@…@@` + the hunk body with markers, trailing newline) which `git apply --cached`
  (stage) / `--cached --reverse` (unstage) consumes on stdin. A guarded temp-repo
  integration test (`integration_stage_hunk_into_index`) **ran** (git present): it
  modifies a file, reconstructs hunk 0, stages it, and asserts `git diff --cached`
  shows the change.
- **Blame: inline end-of-line annotation, NOT a left band.** First attempt drew a
  left blame band over the gutter — it overlapped the code (unreadable). Switched
  to a GitLens-style dim `• author · date · sha` annotation right of each line's
  content (using the model's per-line length), which never obscures code. Parser
  handles git's INCREMENTAL porcelain headers (commit metadata emitted once per
  sha; later lines reference the sha only → cache + back-fill) and a self-contained
  civil-date conversion (no chrono dep). Cache per file, invalidated on save.

**No new Mighty-source limitation.** All four features are scalar-ABI veneers over
shim-owned state (L17/L21). Tests: 508 pass (was 490; +18 — branch-list parsing,
git-output summarize, branch-picker filter/create, single-hunk patch reconstruction
[byte-exact add/remove/context, multi-hunk selection, no-newline marker, parse
round-trip], the temp-repo stage-hunk integration test, blame porcelain parsing
[author/date/sha, incremental-header back-fill, uncommitted zero-sha, tz offset]).
ABI added: `mui_git_push/pull/fetch/branches/dispatch`, `mui_branch_active/push_char/
backspace/query_len/count/move/is_creating/accept/cancel/draw`, `mui_status_branch_at_click`,
`mui_scm_header_action_at_click`, `mui_diff_hunk_count/hunk_at_click/stage_hunk/
unstage_hunk/toggle_hunk`, `mui_blame_toggle/active/line_count/refresh/sync/draw`.
Screenshots `screenshots/39-branches.png` (`MUI_BRANCH_AUTOOPEN`) + `40-blame.png`
(`MUI_BLAME_AUTOOPEN`) at 1320x860. The status bar now reflects the LIVE branch +
ahead/behind from `scm::ScmStatus` (was hardcoded `main ↑2 ↓0`).

### L41. The Web Playground ("Run in Browser") proved Mighty's on-by-default WASM Component-Model web tooling end-to-end — and re-confirmed the L37/L38 ceiling discipline ✅ **[finding + tooling notes]**
Building the IDE's **Run in Browser** action (build the active Mighty file to
`wasm32-web` and run it in the browser) exercised Mighty's web toolchain and
held the parse-stack ceiling with **no new top-level editor-ladder arm**.

- **What `mty serve` / `wasm32-web` actually do (v0.36, verified):**
  - `mty build --target wasm32-web <file>` emits a **Component-Model** `.wasm`
    to `<dir>/target/<stem>.wasm` (default; `--no-component` for a bare core
    module). It works both on a file inside a package AND on a bare standalone
    `.mty` file. The pure `examples/webspin` sample built to a 2172-byte
    component.
  - `mty serve` reads `mighty.toml`, builds with `--target wasm32-web`, and
    serves `web/` + the freshly-built **`main.wasm`** on `127.0.0.1:<port>`
    (default **8000**; `--port`, `--manifest-dir`, `--watch`). It is
    **package-scoped, not file-scoped** — it needs a `mighty.toml` + a `web/`
    dir (the `mty new --template web-game` shape: `mighty.toml`, `src/main.mty`,
    `web/index.html`, `web/dom-shim.js`). Its **only** stdout line is the
    scrapeable banner `mty serve: listening on http://127.0.0.1:<port>`.
    `--watch` adds a `/_reload` websocket that pushes `reload` on every
    successful rebuild. Verified: `/` → 200, `/main.wasm` → 200, `/dom-shim.js`
    → 200.
  - **Browsers can't run a component directly** — the `web-game` `dom-shim.js`
    scans the Component-Model envelope for the inner `\0asm\x01\x00\x00\x00`
    core-module preamble and instantiates that, wiring `log(ptr,len)` over
    linear memory as a `(env|mty).log` import. The guest speaks the v0.22
    `log("evt:…")` state-event channel; the `mty:web/canvas@0.1` WIT binding
    (direct canvas draws) is slated for v0.24 and not yet usable.

- **Gaps found:** module-level `let mut` globals are a **parse error**
  (`unexpected token let`); number→string formatting for `log` does not lower on
  `wasm32-web` in v0.36 (so a "spinner counter" guest must keep the arithmetic
  host-side and `log` only literals). Keeping the sample to literal `log`s +
  agent state is what makes it build cleanly. **Web samples must be pure** — an
  `extern "C"` program won't wasm-run.

- **The IDE wiring (mode-switch + L37/L38 held):** the Web Playground picks
  `Mode::Serve` when the file's package has both `mighty.toml` + a `web/` dir,
  else falls back to `Mode::Build` (`mty build --target wasm32-web` → a generated
  HTML harness in a temp dir → `python -m http.server`). It reuses the
  `crate::run` spawn/reader-thread/pump pattern verbatim; the served URL is
  scraped from output and opened via `cmd /C start "" <url>`. **No new top-level
  ladder arm:** the chord lives in the shim `mui_chord` router (**Alt+W**, the
  L38 escape hatch), the palette command (`CMD_RUN_IN_BROWSER` id 31, "Mighty:
  Run in Browser") dispatches through the existing palette/quick-open arms, and
  the focused-panel input was folded into the **existing `run_focus` arm**
  (widened to `run_focus || web_focus` with a nested `if web_focus` — nesting is
  fine, only new top-level siblings overflow). The git dispatch arm was
  range-bounded (`>= cmd_git_first() && <= cmd_git_last()`) so id 31 isn't
  swallowed by the catch-all git route.

**No new Mighty-source limitation.** Tests: 523 pass (was 508; +15 — URL
extract [banner/punctuation/https/absent], `port_of`, pump-scrape+fresh-latch,
feed split/partial, error-line latch, `decide_mode` serve-vs-build, `seed_demo`,
scroll clamp, empty-open no-op, + a **guarded integration test that built the
pure `webspin` sample to a real `wasm32-web` component** and asserted the `.wasm`
— it RAN and PASSED, producing a 2172-byte component). ABI added: `mui_web_run/
stop/toggle/active/running/pump/open_browser/url_len/url_char/line_count/line_len/
line_char/scroll/click/draw`. Screenshot `screenshots/41-web.png`
(`MUI_WEB_AUTOOPEN` seeds a `mty serve` session + scraped URL) at 1320x860. New
icon `icons::GLOBE`. Pure sample: `examples/webspin/` (Spin agent → spinning arc
+ frame counter).

### L42. Code-reading polish (bracket colors, indent guides, interactive minimap) is all pure-shim render/geometry math behind ONE folded click — ZERO new Mighty-source friction, L37/L38 ceiling untouched ✅ **[finding, not a new limitation]**
Adding three editor visual features re-confirmed that draw-only + geometry work
never stresses the Mighty side or the parse-stack ceiling:

- **All three are pure shim code.** A new `crate::colorize` module holds the
  testable math — bracket-depth assignment (continuous stack scan from line 0 so
  pairs keep a stable color across scroll; string/comment chars masked via the
  syntax spans), indent-guide depth-per-line with blank-line carry (`min` of the
  nearest non-blank above/below so a blank inside a block keeps its rail but a
  blank between blocks drops), the depth→palette-index (`depth % n`, palette
  derived from the active theme so it fits Vivid/Aurora/Warm), and `MinimapGeom`
  (line↔pixel mapping + center-scroll). The editor body draw (`abi.rs`
  `draw_editor_pane`) just consumes them: indent guides drawn under the text
  (faint accent, active rail brighter), bracket glyphs **re-drawn** over the
  punctuation in their depth color (rather than fighting the per-token span
  granularity), and the minimap compresses tall files so the whole file maps
  across the strip + the viewport rectangle reads clearly.

- **The interactive minimap added NO new top-level ladder arm.** Click→jump is
  folded into the existing `mui_ed_click` (it hit-tests the focused pane's stashed
  `minimap_geom` first, jumps + centers, else falls through to normal cell
  placement) — so the Mighty mouse-down arm is unchanged. The cursor-move +
  `scroll_to_cursor` that already runs each frame carries the jump into view, so
  even the scroll bookkeeping needed no Mighty change. Drag-to-scroll was **not**
  done (the event model exposes click + a `last_event` position but no
  move/drag stream); click-to-jump + the viewport rectangle is the shipped scope.
  New ABI (probing/tests only): `mui_minimap_click(x,y)->line`,
  `mui_minimap_active/_left/_width`.

- **Settings.** Two new persisted toggles `bracket_colors` / `indent_guides`
  (default ON), round-tripped through the shared `key=value` config and surfaced
  as two new Settings-panel rows (the panel is now 8 rows, still fits 860px).

**No new Mighty-source limitation.** Tests: 584 pass (was 562; +22 — bracket
depth nested/wrap/extra-closer/mismatch/cross-line/masked, depth→index wrap,
palette non-empty per theme, leading-indent spaces+tabs, guide-levels,
indent-depth blank-line carry both directions, active-level-from-cursor across
tab widths, minimap click top/middle/bottom/short-file + center-scroll clamp +
contains-x, plus the two settings round-trips + panel toggle). Screenshots
`screenshots/44-brackets-guides.png` (`MUI_BRACKETS_AUTOOPEN` seeds nested code)
and `45-minimap.png` (`MUI_MINIMAP_AUTOOPEN` seeds a 160-fn tall file scrolled
partway) at 1320x860.

### L43. Live Markdown preview is a pure-shim parser + themed Vello renderer reusing the EXISTING split-pane machinery — no new Mighty draw/ladder arm, L37/L38 ceiling untouched ✅ **[finding, not a new limitation]**
A full live `.md` preview dropped in with zero new Mighty-source friction:

- **A focused, dependency-free parser** (`crate::markdown`) → a `Block`/`Span`
  render model: ATX headings (1–6, trailing-`#` stripped, `#word`/7-hash rejected),
  inline `**bold**`/`*italic*`/`` `code` ``/`[text](url)`/`~~strike~~` with `\`
  escaping and nesting, ordered+unordered lists (2-space indent = one depth level),
  fenced code (``` / `~~~` + language tag, unterminated closes at EOF), blockquotes
  (recurse on one stripped `>`), `---`/`***`/`___` rules, and simple pipe tables.
  The one subtlety worth recording: the single-`*` emphasis scanner must **skip a
  doubled `**` run** so `*a **b** c*` closes on the final lone `*` (else the italic
  closes inside the bold); the inner slice is then re-parsed and finds the bold.

- **The renderer reuses the pane system as-is.** A preview is just the RIGHT split
  pane flipped into a "preview mode" flag (`md_pane: Option<usize>`); `mui_ed_draw`
  already loops the panes, so a one-line branch in that loop calls the markdown
  painter for the preview column instead of the editor body. The painter
  (`crate::mdpreview`) pulls every color from `theme::*` (works in all 3 themes),
  word-wraps proportional UI text by estimated advance, draws scaled headings (h1/h2
  with a bottom hairline), a tinted rounded code card (monospace cell advance), inline
  `code` chips, accent-underlined links, indented list markers, an accent-bar
  blockquote, `---` dividers, and tables. Source = the live buffer of the OTHER pane,
  re-parsed each frame (cheap for IDE files), so it updates as you type.

  **RESOLVED (typography pass):** the text path now carries a `FontStyle`
  (Regular/Bold/Italic/BoldItalic) and the Vello backend selects a REAL bundled
  face per run — no faux weight/slant. The bundled families gained their missing
  faces: JetBrains Mono **Bold/Italic/BoldItalic** + Bricolage Grotesque **Bold**
  (all SIL OFL). So markdown headings and `**bold**` now render in a true bold
  face; `*italic*` renders in a true italic (the UI family has no italic, so
  italic body text shapes in the code family's genuine italic). The same channel
  renders editor **comments in italic** (detected by the comment color) and the
  bold UI chrome (active tab label, EXPLORER header, the welcome wordmark +
  section headers). Verified distinct (not faux) in `screenshots/47-typography.png`.
  New text ABI: `Text::queue_styled` / `queue_ui_styled`; the per-run advance in
  Vello uses each face's own `advance_width`, so bold's wider glyphs don't collide.

- **No new top-level Mighty arm (L37/L38).** Ctrl+Shift+V routes through the existing
  `mui_chord` router (the `is_router_chord` predicate widened); the palette command
  ("Markdown: Open Preview") routes through the existing `mui_pane_dispatch` range; the
  breadcrumb "Preview" pill (shown only for `.md`) hit-tests up-front in the existing
  mouse-down ladder. New ABI: `mui_md_open/_active/_set_source/_scroll/_draw/_close` +
  `mui_md_button_at_click`. Tests: 604 pass (was 584; +20 markdown parser + preview
  layout). Screenshot `screenshots/46-markdown.png` (`MUI_MD_AUTOOPEN` seeds a crafted
  sample, source-left / rendered-right) at 1320x860.

### L44. Real bold/italic faces + on-save conveniences are pure-shim render + I/O — no new Mighty arm, L37/L38 ceiling untouched ✅ **[finding, not a new limitation]**
Two quality wins landed together with zero new Mighty-source friction:

- **True typography.** The display-list `Text` command grew a `FontStyle`; the
  Vello backend holds all four code faces (JetBrains Mono Regular/Bold/Italic/
  BoldItalic) + UI Regular/Bold (Bricolage) and picks the REAL face per run.
  Applied to: markdown headings/`**bold**` (bold), `*italic*` (true italic via
  the code face — the UI family has no italic), editor comments (italic, keyed off
  the comment color), and bold UI chrome (active tab, EXPLORER header, welcome
  wordmark/section headers). Each face's own `advance_width` drives layout so
  bold's wider glyphs don't collide. Verified distinct in `47-typography.png`.

- **Save conveniences.** Pure functions in `crate::savefmt` (trim trailing ws,
  ensure final newline; both CRLF-safe) gate behind Settings toggles (`trim_ws`
  /`final_newline` default ON) and run in `mui_ed_save`, reflected back into the
  live buffer via `TextModel::set_text_preserving_cursor` (cursor kept). **Auto
  save** (default OFF) is a debounced clock (`AutoSave`, ~1.2s idle) ticked each
  frame by `mui_autosave_tick`; it detects edits via a cheap FNV signature of the
  active buffer (no per-op instrumentation) and only ever writes real file-backed,
  dirty tabs (read-only/diff/preview/welcome/scratch have no path → skipped). New
  ABI: `mui_autosave_tick` / `mui_autosave_touch` + settings getters
  `trim_ws`/`final_newline`/`autosave`, all surfaced as Settings rows. Tests: 618
  pass (was 604; +14 savefmt + settings). Screenshot `screenshots/47-typography.png`
  (`MUI_TYPO_AUTOOPEN` seeds a comment-rich buffer) at 1320x860.

### L45. Explicit workspace (Open Folder) + a debounced quick-fix lightbulb are pure-shim — no new Mighty draw/ladder arm, L37/L38 ceiling untouched ✅ **[finding, not a new limitation]**
Two features landed with zero new Mighty-source friction:

- **Explicit workspace.** The "workspace root" became a settable shim concept
  (`crate::workspace::Workspace { root, name }` + `RecentWorkspaces` MRU, cap 10,
  persisted to a `recent-workspaces` file in the config dir). The single source of
  truth is `wsabi::effective_root` (explicit root, else the tree root); the file
  tree, Quick-Open index (`quickopen_root`), Search/git (`panels::workspace_dir`),
  and Agents discovery all read through it, and `mui_ws_open` re-roots the tree +
  rebuilds the index + re-runs git status + re-scans Agents + records the recent in
  one call. The **native folder picker is a PowerShell `FolderBrowserDialog`** shelled
  out of the shim (`-STA -Command`), reading the chosen path off stdout. If the
  picker is unavailable the IDE falls back to a typed-path prompt (new
  `PromptKind::OpenFolder`); explicit Cancel is a no-op, matching native editor
  expectations. Re-rooting reuses the SAME byte-staging buffer
  (`mui_path_push`/`_clear`) the Open-File prompt uses — no new staging ABI. New ABI:
  `mui_ws_root_*`/`_name_*`/`_open`/`_open_dialog`/`_open_recent`/`_recent_*`/`_dispatch`
  + `mui_welcome_open_folder`. Welcome gained "RECENT FOLDERS" + "RECENT FILES"
  columns + an "Open Folder…" quick action.

- **Quick-fix lightbulb.** A debounced gutter bulb: `crate::lightbulb::Lightbulb`
  owns the bookkeeping (which line was probed, idle-frame counter, last-drawn rect)
  so the LSP isn't hit every frame — it only re-probes after the cursor SETTLES on a
  new line for `IDLE_FRAMES`. The "has actions" probe REUSES the code-action request
  path: `mui_codeaction_request` was refactored to a shared `compute_line_actions`
  core that the read-only `mui_lightbulb_tick` calls without opening the menu. One
  behavior tightening fell out: "Fix all (mty)" is now only offered when the LSP
  returned at least one action, so the bulb (and the Ctrl+. menu) light only on
  genuinely-fixable lines rather than every line. Clicking the bulb hit-tests its
  drawn rect up-front in the existing mouse-down ladder, then runs the SAME
  code-action path as Ctrl+.. New ABI: `mui_lightbulb_tick`/`_visible`/`_line`/
  `_reset`/`_draw`/`_click` + a `LIGHTBULB` bulb icon.

- **L37/L38 ceiling discipline held.** Ctrl+Shift+O (Open Folder) routes through the
  existing `mui_chord` router (the `is_router_chord` predicate widened); the two new
  palette commands ("File: Open Folder/Open Recent") route through a single new
  `mui_ws_dispatch` arm-range (mirroring the git/pane dispatchers); the lightbulb
  click + the recent-folder Welcome click fold into the existing mouse-down ladder;
  the per-frame `mui_lightbulb_tick` is one call beside the other frame ticks. The
  editor key ladder gained NO new top-level arm. Tests: 637 pass (was 618; +19
  workspace/lightbulb/welcome). Screenshots `screenshots/48-openfolder.png`
  (`MUI_WELCOME_AUTOOPEN` seeds recent folders + a workspace name in the
  explorer header) and `49-lightbulb.png` (`MUI_LIGHTBULB_AUTOOPEN` pins the bulb on
  the type-error line of `examples/with_error.mty`) at 1320x860.

### L46. The Keyboard Shortcuts overlay + remapping is pure-shim — but `!fn_call(args)` is a NEW mty parse trap: unary `!` binds tighter than the call ⚠️ **[finding + NEW language gotcha, P2]**
The discoverability+customization feature (a searchable list of every command/
binding + remapping the router-routed subset) landed shim-side with the usual
scalar ABI and ZERO new top-level editor-ladder arms — but surfaced one concrete
mty v0.36 parse gotcha worth recording:

- **`!fn_call(args)` mis-parses.** Writing `!ctrl_held(mods)` in a condition
  type-errored as **MT2008 "value of type `Bool` is not callable"** (reported at
  the unhelpful `1:1`). The parser binds the prefix `!` to the *identifier* before
  the call — i.e. it reads `(!ctrl_held)(mods)` and then tries to call the `Bool`.
  Every existing `!`-on-Bool in `main.mty` negates a **local** (`!ctrl`, `!shift`),
  never a call result, which is why this never bit before. **Fix/idiom:** bind the
  call to a `let` first (`let ctrl = ctrl_held(mods)`) then negate the local
  (`!ctrl`). (Parenthesizing — `!(ctrl_held(mods))` — likely also works but the
  let-binding matches the established style.)

- **Feature shape (pure-shim, L37/L38 held).** `crate::shortcuts` owns the row
  assembly (palette `COMMANDS` + a static ladder-fixed table → name/keys/
  remappable), a substring filter (name OR key), an `Overrides` map (command id →
  `Chord{cp,mods}`) with conflict detection, reset-one/reset-all, and a
  `keybindings.toml` save/load round-trip in the config dir. The overlay is a
  Vivid-Modern card with kbd pills mirroring the palette. Remapping covers the
  **router-dispatchable subset** (Zen/Agents/Blame/Run-in-Browser/Split/Markdown-
  preview/Open-Folder) — the commands `mui_chord` both detects AND fully dispatches.
  `mui_chord` now resolves the incoming chord through `Overrides::resolve` FIRST
  (override wins; a remapped command's freed default stops firing) via a new
  `router_dispatch(handle, cmd_id)` helper that both the override path and capture
  share. **Remap targets are constrained to `Alt`+letter** — the one modifier class
  `is_router_chord` forwards wholesale to the router, so a new chord always reaches
  it without growing the ladder (a Mighty-side gate can't be data-driven from the
  shim). The overlay's input mode NESTS into the existing breadcrumb/branch
  combined arm (now three overlays, one top-level arm); the open-chord is
  Ctrl+Shift+/ added to `is_router_chord`; the palette command "Help: Keyboard
  Shortcuts" dispatches to `mui_keys_open`. New ABI: `mui_keys_open/_active/
  _capturing/_push_char/_backspace/_move/_sel/_count/_row_name_*/_row_keys_*/
  _row_remappable/_begin_capture/_capture_chord/_reset/_reset_all/_cancel/_draw`.
  Tests: 649 pass (was 637; +12 — chord normalize/label/token round-trip, row
  assembly, filter, override set/resolve, conflict, reset one+all, render/parse +
  save/load round-trip, capture records override, fixed-row rejected). Screenshot
  `screenshots/50-shortcuts.png` (`MUI_SHORTCUTS_AUTOOPEN="alt"` opens the overlay
  filtered) at 1320x860.

### L47. mty `&&` / `||` do NOT short-circuit — both operands always evaluate ⚠️ **[NEW language gotcha, P1 — correctness risk]**
Discovered while debugging live windowed input with the new Windows UI harness
(`tools/win-ui-harness.ps1` + the `MUI_TRACE` shim event log). A guard written as
`(run_focus || web_focus) && !(tag == ev_mouse_down() && mui_rail_panel_at_click(h) >= 0)`
called `mui_rail_panel_at_click(h)` on **every** event — including `char`/`key`
events where `tag == ev_mouse_down()` is false. In a short-circuiting language the
right operand of the inner `&&` would never run. The trace proved otherwise: a
`rail_panel_at_click x=0.0 y=0.0 -> -1` line appeared for each keystroke.

- **Why it matters:** this is a real correctness footgun, not just perf. Idioms
  like `if p != null && p.field > 0` or `if i < len && arr[i] == x` (guard-then-use)
  will execute the second operand even when the guard is false — a null-deref /
  out-of-bounds in any language that assumes short-circuit. We only got away with it
  here because `mui_rail_panel_at_click` is side-effect-free and tolerates `(0,0)`.
- **Fix/idiom for now:** never rely on the guard half of `&&`/`||` to protect the
  other half. Nest the dependent access (`if guard { if use {...} }`) or bind the
  cheap guard first. Keep both operands independently safe to evaluate.
- **Language ask (P1):** make `&&`/`||` short-circuit (the near-universal contract).
  If lazy evaluation is intentionally out of scope, this MUST be loudly documented —
  it silently breaks guard patterns ported from every other language.

### L48. Offscreen render tests are blind to the live winit event loop — a real Windows input/screenshot harness is mandatory **[process finding]**
The IDE's prior "verification" rendered offscreen and pushed input straight through
the ABI, so it never exercised the live window: real DPI scaling, real click
hit-testing, OS focus, and event routing. A user run surfaced four bugs the
offscreen path could never see (dead rail, click-locks-up, can't-type, OS-frame).
Built `tools/win-ui-harness.ps1`: launches the real `.exe`, resolves the LARGEST
visible top-level window of the process (winit briefly exposes a 14×14 helper
window `MainWindowHandle` can latch onto — picking by area avoids it), injects
input via **`PostMessage` to the HWND** (focus-independent — `SendInput` silently
lost every event to the foreground-locked terminal), screen-captures via
`CopyFromScreen` (works for the GPU/DXGI surface; `PrintWindow` returns black), and
probes `SendMessageTimeout(WM_NULL, ABORTIFHUNG)` for true Win32 hangs. Paired with
the env-gated `MUI_TRACE` shim log (every popped event + its shim classification +
a per-60-frame heartbeat), it makes the live app fully observable: it pinpointed the
focus-trap routing bug (clicks reached `main.mty` but a keyboard-focus arm swallowed
the `mouse_down`) and confirmed each fix. **Takeaway:** GUI changes must be verified
with real simulated input on a live window, not offscreen renders; keep the harness
green.

## Open questions to resolve as the IDE progresses
- Exact `extern c` signature support: pointers (`*U8`), out-params (`&out T`), passing a
  `Vec`/slice as `(ptr, len)`, returning `#[repr(C)]` structs by value vs. out-param?
  (The Phase-0 spike will answer much of this — record results here.)
- Does native `mty build` handle dynamic FFI calls in a loop? (Phase-0 Gate B.)
- `fs` module API names for read-to-string / write (needed by IDE save/load).

### L49. UI event handling still wants compact FFI result shapes **[language gap, P2]**
While making project-search feel complete, the shim had to add
`mui_search_action_at_click(handle) -> I32` with encoded return values (`0` miss,
`1` run search, `2` replace all). The Mighty side then branches on magic integers.
This is workable, but it is the same recurring pattern across panels: Rust owns the
geometry and returns scalar codes because the current Mighty/FFI surface is not yet
comfortable with small tagged structs or enums.

- **Why it matters for the IDE:** every clickable panel action becomes another
  scalar ABI function plus comments documenting return codes. It is easy to drift
  between Rust and Mighty, and the UI router grows repetitive ladders.
- **Language ask:** first-class C-compatible enums or tiny return structs would let
  the shim expose `SearchClick { kind, row }` or similar instead of one function per
  action family. If structs are not ready, consider generated constants on the Mighty
  side so `search_click_replace_all()` replaces raw `2`.

### L50. `mty build` can exit success after object-only output when linker discovery fails **[language/tooling gap, P1, FIXED v0.46 T2]**
While re-verifying the IDE build on May 31, 2026, `mty build src/main.mty --out-dir target`
printed `wrote object target\main.o (no linker found; set $MTY_LINKER)` and returned
success even though `target/main.exe` was not produced. Setting `MTY_LINKER` to
`C:\Program Files\LLVM\bin\clang.exe` and also trying `MTY_LINKER=clang` with LLVM
on PATH still produced object-only output in this environment.

- **Why it matters for the IDE:** `build-ide.sh` used to wrap `ls target/main.exe`
  inside `echo "$( ... )"`, so `set -e` did not catch the missing executable. A
  headless or CI build could look green while shipping no runnable app.
- **IDE-side fix:** `build-ide.sh` now deletes stale `target/main.exe`/`main.o`
  before the build and explicitly fails if `target/main.exe` is missing or empty.
- **Mighty ask:** `mty build` should exit non-zero when the requested native
  executable is not produced. It should also honor `MTY_LINKER` on Windows paths
  with spaces, or emit the exact env/path it tried so linker discovery failures
  are actionable.
- **Mighty fix (v0.46 T2, June 2, 2026):** `mty build` now exits non-zero on
  the native target whenever no linker can be discovered or the link step
  doesn't actually produce a non-empty executable. CI flows that only need the
  `.o` may opt back into the historic "object-only is OK" path via `--emit obj`.
  Discovery emits a structured trace of every env var (`$MTY_LINKER`,
  `$STARDUST_LINKER`) and PATH candidate (`clang`, `gcc`, `cc`, `lld-link`,
  `ld.lld`, `lld`) that was probed plus the outcome (`unset` / `not on PATH` /
  `file does not exist` / `found <path>`). `MTY_LINKER` honours wrapping ASCII
  quotes (`MTY_LINKER='"C:\Program Files\LLVM\bin\clang.exe"'`) and PATH
  lookup for bare program names (`MTY_LINKER=clang`). Linker stderr is folded
  into the diagnostic when the link step itself fails, and a successful exit
  that nevertheless produces no/empty output now reports the corner case
  explicitly. Tracked in `crates/mty-codegen-cranelift/src/object.rs`
  (`discover_linker`, `LinkerDiscovery::summary`),
  `crates/mty-driver/src/build.rs`
  (`BuildOutcome::NativeOkNoLinker { object_path, discovery }`), and
  `crates/mty-cli/src/cmd/build.rs` (exit-code routing + `--emit obj`).

### L51. Runtime ABI archives must track Mighty's imported symbol table **[language/tooling gap, P1]**
After fixing `build-ide.sh` to fail on missing `target/main.exe`, the next native
link failure was not linker discovery: `clang` was found, but `main.o` referenced
new typed runtime symbols such as `mty_runtime_log_i32`, `mty_runtime_print_sep`,
`mty_runtime_fmt_i32`, and `mty_runtime_str_concat`. The IDE's vendored
`mty_rt_abi.lib` only exported the older runtime ABI, so the linker stopped with
undefined symbols.

- **IDE-side fix:** `crates/mty-rt-abi` now mirrors the compiler's typed
  log/print/format/string-concat runtime surface, and the build restages the
  refreshed static library into `vendor/mty_rt_abi.lib`.
- **Mighty ask:** publish the runtime ABI symbol list as a generated header,
  crate, or default static runtime artifact so native projects do not have to
  manually discover new `mty_runtime_*` imports after compiler changes.

### L52. Prompt-driven commands still require duplicated char-by-char string staging **[language/FFI gap, P2]**
Adding Explorer-grade file operations (rename active file, reveal active file, and
delete active file with exact-name confirmation) reused the bottom prompt for user
input. Each prompt Enter handler in `src/main.mty` must still copy the query into
the Rust shim one character at a time:

1. `mui_path_clear(handle)`
2. loop `i < mui_prompt_len(handle)`
3. read `mui_prompt_char(handle, i)`
4. push each scalar with `mui_path_push(handle, cp)`
5. call the real action (`mui_file_rename_active`, `mui_file_delete_active_confirm`, etc.)

That loop now exists for Open, Open Folder, New Project, New Folder, Save As,
Rename File, and Delete File. A helper would be the natural shape, but Mighty-side
helpers still cannot reduce much of the ceremony because the current FFI boundary
does not accept a prompt string/slice directly, and UI command handlers often need
to mutate caller-local state (`prompt_kind`, `find_nav`, focus booleans).

- **Why it matters for the IDE:** every new command that needs a typed path/name
  adds another fragile copy loop in the event ladder. It is easy for one handler to
  forget to clear the staging buffer, cancel the prompt, or refresh diagnostics /
  SCM / outline after a path mutation.
- **IDE-side workaround:** keep filesystem/path semantics in Rust and use one
  shared staging buffer, with tests asserting that failed/successful operations
  consume the staged bytes.
- **Mighty ask:** add a safe way to pass the active prompt/query string to an
  `extern c` function, such as `(ptr,len)` for immutable UTF-8 slices, a
  compiler-owned temporary string view, or first-class bindings for common
  shim-owned string buffers. Longer term, lightweight block helpers that can
  mutate caller locals would also make command dispatch less repetitive.

### L53. Snippet mirrors confirm the shim-owned editor-state pattern **[finding, P3]**
Implementing VS Code-style mirrored snippet placeholders (`${1:i}` repeated in a
template) did not require a Mighty language change. The existing scalar event
route stayed intact: Mighty still calls `mui_snippet_replace_stop` before normal
typed-character insertion and `mui_ed_insert_smart_multi` for the actual edit.
The shim now owns the richer state: primary tab-stop navigation indexes, mirror
placeholder ranges, range replacement, and position shifting after each mirrored
edit.

- **Why it matters for the IDE:** users now get expected snippet behavior such as
  JavaScript `for` expanding with all repeated iterator names kept in sync, while
  Tab navigation skips duplicate mirror stops and lands only on meaningful fields.
- **Language note:** no new gap surfaced. This is the right current architecture
  for high-touch editor mechanics: keep mutable text/range state in Rust, expose
  small scalar hooks to Mighty, and avoid growing the already-deep event ladder.

### L54. Persisted recent files follow the existing shim-owned config pattern **[finding, P3]**
The recent-file MRU now persists across IDE restarts, matching the already
persisted recent-workspace MRU, and stale missing files are pruned instead of
opening as blank tabs. This did not require Mighty language work: Mighty already
calls a scalar hook whenever a real file is opened, and the Rust shim owns the
Quick-Open/Welcome MRU state plus the config-directory I/O.

- **Why it matters for the IDE:** Welcome and Quick-Open no longer forget the
  files a user worked on after relaunch, so Open Recent behaves like a real
  daily editor instead of a per-process demo list.
- **Language note:** no new gap surfaced. The same scalar-command boundary
  remains sufficient for persistence features when the shim owns file paths.

### L55. Modal overlay composition needs shim-owned layer hygiene **[finding, P3]**
Fixing command-palette / branch-switcher text bleed-through exposed a renderer
ordering issue, not a Mighty language issue. Mighty calls each overlay draw hook
in a fixed scalar order, while the Rust shim queues shapes and text into base /
overlay display-list layers. Earlier overlay text from panels such as AI can
outlive its visual surface unless the active modal clears stale overlay text and
temporarily owns the full viewport clip.

- **Why it matters for the IDE:** command palette, Quick-Open, settings, theme
  picker, shortcuts, dirty-confirm, and branch switcher must behave like true
  modal surfaces: readable text, opaque card, no background labels bleeding
  through, and no inherited editor/sidebar clipping.
- **IDE-side fix:** modal draw wrappers now clear earlier overlay text only when
  their own overlay is active, temporarily remove inherited clips while drawing
  centered cards, and restore the previous clip afterward. The Windows harness
  coordinates were also updated to match the current topbar / Explorer header.
- **Language note:** no new gap surfaced. Mighty can keep issuing scalar draw
  calls; the shim should continue owning retained UI layers, clipping, and modal
  z-order invariants.

### L56. Welcome stale-recents recovery is shim-state hygiene **[finding, P3]**
Fixing a stale recent-folder click from the Welcome screen did not surface a new
Mighty limitation. The bug was purely in Rust shim state ordering: the Welcome
screen dismissed itself before the selected recent folder was validated, so a
missing folder could close a forced Welcome screen even though no workspace
opened. The fix keeps the validation/prune/toast path in `wsabi`, then dismisses
Welcome only after a successful re-root.

- **Why it matters for the IDE:** Open Recent should recover gracefully from
  renamed/deleted folders without kicking the user out of the selection surface.
- **Language note:** no new gap surfaced. Recent-file/folder recovery should stay
  with the shim-owned MRU and workspace model; Mighty only needs scalar action ids
  and does not need to own path validation or MRU mutation.

### L57. Agents inspect/run header actions are still scalar routing **[finding, P3]**
Making the Mighty Agents panel's live inspect path reachable from the UI did not
require new Mighty language work. The panel now exposes separate shim-side
header hit-tests for Inspect and Run, and the Mighty event ladder only branches
on scalar booleans before calling `mui_agents_inspect` or `mui_agents_run`.

- **Why it matters for the IDE:** a feature is not complete just because the ABI
  exists; every visible panel affordance needs a real click target and a test.
- **Language note:** no new gap surfaced. This follows the same scalar-command
  pattern as the other panels; richer UI hit geometry remains easier and safer in
  the shim.

### L58. Native dialog cancel needs explicit scalar result states **[finding, P3]**
Open File, Save As, and Open Folder now distinguish three outcomes at the
Rust/Mighty boundary: success, user cancel, and native-dialog unavailable. The
old `-1`/failure-only shape collapsed Cancel together with unavailable, so
Mighty opened a typed-path fallback prompt after a perfectly normal Cancel. The
shim now returns a separate cancel code (`-2` for file/save pickers, `0` for
folder picker cancel) and reserves `-1` for "native picker could not run", which
is the only case that should open the fallback prompt.

- **Why it matters for the IDE:** native dialogs must behave like desktop users
  expect. Cancel should leave editor state untouched, not cascade into another
  prompt that looks like a broken button.
- **Language note:** no new Mighty feature is strictly required, but this is
  another case where richer tagged result values would reduce brittle scalar
  conventions in the event ladder. Until Mighty has ergonomic enum/result values
  across `extern c`, the ABI should keep small result-code contracts documented
  and covered by tests.

### L59. Clickable/clearable toasts stay within scalar overlay routing **[finding, P3]**
Making stale toast notifications dismissible did not require a Mighty language
change. The shim now owns toast hit-testing and mutation (`ToastQueue::dismiss_at`,
`mui_toast_click`, `mui_toast_clear`), while Mighty only gives mouse-down events
first chance to the toast overlay before routing the same click to panels/editor
surfaces underneath.

- **Why it matters for the IDE:** transient notifications must not leave stale
  text covering the workspace. Users can click a single toast to dismiss it or run
  **Notifications: Clear All Toasts** from the command palette to clear the whole
  stack.
- **Language note:** no new gap surfaced. This is another case where shim-owned
  geometry is the right boundary: Mighty routes a scalar click result, while Rust
  computes the overlay card rectangles and removes the clicked item.

### L60. Save All reinforces shim-owned tab state **[finding, P3]**
Adding **Save All** did not require new Mighty language work because the shim
already owns tab paths, dirty flags, and authoritative text models. Mighty adds
one scalar command id and calls `mui_save_all`; Rust iterates dirty file-backed
tabs, applies the same on-save transforms as normal Save, writes each path, and
leaves untitled buffers dirty with an explanatory toast.

- **Why it matters for the IDE:** multi-tab work must have a single reliable
  command for persisting all open edits. Saving only the active tab is a daily
  workflow footgun once the editor supports tabs, split panes, search, tests, and
  background tooling.
- **Language note:** no new gap surfaced. This feature fits the current boundary:
  Mighty should dispatch the command, while Rust owns path I/O, dirty tab
  iteration, save transforms, and user-facing summary text.

### L61. Bulk tab cleanup should protect dirty buffers in the shim **[finding, P3]**
Adding **Close Saved Tabs** did not require new Mighty language work. The Rust
tab store owns the invariant: clean tabs may be removed in bulk, dirty tabs are
kept, and an all-clean workspace collapses to the existing single scratch tab.
Mighty only dispatches the command and refreshes the active editor surfaces from
the returned active-tab index.

- **Why it matters for the IDE:** once users have search, tests, split panes, and
  multi-file editing, tab clutter grows quickly. A safe cleanup command must be
  impossible to use as an accidental dirty-buffer discard path.
- **Language note:** no new gap surfaced. The current scalar command boundary is
  adequate because the safety-critical tab filtering and dirty checks are
  shim-owned and unit-tested.

### L62. Active-context tab cleanup is still shim-owned **[finding, P3]**
Adding **Close Other Saved Tabs** followed the same boundary as L61. The shim
keeps the active tab, preserves every dirty tab, removes only clean inactive
tabs, and returns the new active index for Mighty to reload the editor surfaces.

- **Why it matters for the IDE:** daily work often starts from one file of
  interest with many navigation tabs around it. Users need a fast cleanup command
  that narrows the workspace without risking unsaved edits.
- **Language note:** no new gap surfaced. A future Mighty enum/result ABI would
  make these command outcomes clearer, but the current scalar return plus
  shim-owned tab filtering remains adequate and testable.

### L63. Directional tab cleanup should share the same dirty-safety rule **[finding, P3]**
Adding **Close Saved Tabs to the Left/Right** did not require new Mighty
language work. Directional filtering is another tab-store invariant: tabs on the
selected side close only when clean, dirty tabs survive, and Mighty receives the
active index to refresh the editor.

- **Why it matters for the IDE:** mature tab workflows need broad cleanup,
  active-context cleanup, and directional cleanup. All three should behave
  consistently so cleanup commands never become hidden discard commands.
- **Language note:** no new gap surfaced. The command-id ladder is growing, so a
  future table-driven Mighty dispatch form would reduce boilerplate, but the
  current explicit scalar arms remain reliable.

### L64. Reopen-closed-tab history belongs with the tab store **[finding, P3]**
Adding **Reopen Closed Tab** (`Ctrl+Alt+T`) did not require new Mighty language
work. The Rust tab store keeps a bounded stack of recoverable closed tabs, skips
empty scratch tabs, restores the most recent tab, and returns the active index
for Mighty to reload the editor.

- **Why it matters for the IDE:** once tab-close and cleanup commands are fast,
  accidental closes need an immediate recovery path that does not depend on the
  file tree or recent-file list.
- **Language note:** no new gap surfaced. The feature does reinforce the broader
  L63 note: command dispatch would be cleaner if Mighty had an ergonomic
  table-driven or enum-driven dispatcher instead of one scalar arm per command.

### L65. Bulk cleanup history remains a tab-store invariant **[finding, P3]**
Making bulk cleanup commands reversible did not require new Mighty language
work. The shim already owns the closed-tab stack, so **Close Saved Tabs**,
**Close Other Saved Tabs**, and directional cleanup can feed recoverable clean
tabs into the same history that `Ctrl+Alt+T` consumes.

- **IDE note:** cleanup commands should not be destructive when the removed tab
  has file-backed content. Empty scratch tabs are still intentionally skipped.
- **Language note:** no new gap surfaced. This again points to the same
  dispatcher ergonomics gap from L63/L64, but the runtime boundary is sound.

### L66. Tab duplication needs live shim state, not disk reloads **[finding, P3]**
Adding **Duplicate Active Tab** did not require new Mighty language work. The
correct behavior is a shim-owned clone of the active tab record because the
duplicate must preserve unsaved edits, cursor position, scroll, folds, dirty
state, and the active file path without re-reading stale bytes from disk.

- **IDE note:** commands that clone editor context should operate on the
  authoritative tab model, then let Mighty reload its scalar editor view from the
  selected tab index.
- **Language note:** no new gap surfaced beyond the existing command-dispatch
  ergonomics issue. Mighty can route the scalar command cleanly once the shim
  owns the stateful operation.

### L67. Reload-from-disk must be dirty-aware in the shim **[finding, P3]**
Adding **Reload Active File from Disk** did not require new Mighty language work.
The shim already knows the active path, dirty flag, and authoritative text model,
so it can refresh clean file-backed tabs from disk while refusing to overwrite
unsaved local edits.

- **IDE note:** external tool and git workflows need a direct reload command, but
  it must never silently discard dirty buffers.
- **Language note:** no new gap surfaced. The same scalar command routing remains
  enough; the safety invariant belongs with the tab store and ABI guard.

### L68. Revert-from-disk is the explicit destructive twin of reload **[finding, P3]**
Adding **Revert Active File from Disk** did not require new Mighty language work.
The key distinction is command intent: reload refuses dirty buffers, while revert
is an explicit destructive command that reloads the file-backed tab and clears
the dirty flag.

- **IDE note:** keeping reload and revert separate gives external-tool workflows
  both a safe refresh path and a deliberate discard path.
- **Language note:** no new gap surfaced. The Mighty side only needs one more
  scalar command id; the destructive state transition stays in the shim.

### L69. Recents predicates belong beside the MRU stores **[finding, P3]**
Fixing **File: Open Recent** exposed a small cross-store state smell: recent files
live in Quick Open while recent folders live in the workspace MRU. Mighty should
not infer availability from only one store, so the shim now exports one predicate
covering both.

- **IDE note:** Open Recent should show the chooser whenever either recent files
  or folders exist; otherwise it can fall back to the open-folder prompt.
- **Language note:** no new gap surfaced. A single ABI predicate is enough, but
  this reinforces that Mighty should ask the shim for UI state summaries instead
  of duplicating state rules in control code.

### L70. File-context clipboard variants are scalar commands **[finding, P3]**
Adding **Copy Active File Name** and **Copy Active File Directory** stayed within
the existing scalar command pattern. Mighty only routes the command id; the shim
derives the path text and performs the platform clipboard write.

- **IDE note:** basename and containing-folder copies remove a small but constant
  manual-editing tax from command-palette file workflows.
- **Language note:** no new gap surfaced. This is another case where string work
  and platform integration should remain shim-side until Mighty can pass richer
  values across the ABI.

### L71. Tab reordering must remap split-pane tab indices **[finding, P3]**
Adding **Move Active Tab Left/Right** was mostly a tab-store operation, but split
panes made the invariant explicit: panes store tab indices, so adjacent tab swaps
must remap pane indices to keep each pane on the same logical document.

- **IDE note:** tab reordering is a high-frequency organization command; it must
  work from both shortcuts and the command palette without scrambling split views.
- **Language note:** no new gap surfaced. Mighty routes the scalar command and
  resets transient editor state; the tab-order and pane-index invariants belong
  in the shim.

### L72. Full tab reorders need an old-to-new index map **[finding, P3]**
Adding **Sort Open Tabs by Name** generalized L71: adjacent swaps can remap two
indices, but a full sort needs the tab store to return an old-index to new-index
map so split panes can keep following the same documents.

- **IDE note:** bulk tab organization must preserve the active logical document
  and every split-pane binding, not just the visible order.
- **Language note:** no new gap surfaced. Mighty should continue routing one
  scalar command while the shim owns collection reordering and remap invariants.

### L73. Bulk tab compaction needs an old-to-optional-new index map **[finding, P3]**
Adding **Close Duplicate Tabs** is different from sorting: some old tabs vanish,
so panes need an old-index to optional-new-index map. Kept tabs remap directly;
panes that pointed at removed tabs fall back to a valid neighbor.

- **IDE note:** duplicate cleanup should close only clean file-backed duplicates,
  keeping dirty duplicates and untitled buffers so cleanup never discards work.
- **Language note:** no new Mighty gap surfaced. The optional remap and duplicate
  detection are collection-heavy invariants that still belong shim-side.

### L74. Bulk git index actions are another scalar-command win **[finding, P3]**
Adding **Git: Stage All** and **Git: Unstage All** reused the existing
Source-Control split: Mighty routes command ids and the shim owns git process
execution, status refresh, and toasts.

- **IDE note:** per-file stage/unstage is not enough for daily commits; bulk
  index cleanup belongs in the command palette beside push, pull, and fetch.
- **Language note:** no new gap surfaced. The operation needs platform process
  spawning and status parsing, both already intentionally shim-side.

### L75. Commit commands can reuse shim-owned text buffers **[finding, P3]**
Adding **Git: Commit Staged** did not need a new string-passing mechanism.
Mighty already appends characters into the Source-Control message buffer through
scalar ABI calls, so the command palette can route one id and let the shim commit
with the existing buffer.

- **IDE note:** keyboard-first git flow needs stage, unstage, and commit all
  available from commands, not only clickable Source-Control chrome.
- **Language note:** no new gap surfaced. This confirms the current pattern:
  Mighty routes the command; the shim owns text-buffer, process, and refresh
  details until richer FFI strings are available.

### L76. Primary view switching belongs in the command registry **[finding, P3]**
Adding command-palette entries for Explorer, Search, Source Control, Outline,
Run and Debug, and Testing reused the existing `mui_panel_set` scalar ABI. The
view ids stay mirrored as tiny Mighty helper functions, while panel state and
open-side effects stay shim-side.

- **IDE note:** primary views must be keyboard-first and searchable, not only
  reachable by activity-rail clicks or a few hardcoded chords.
- **Language note:** no new gap surfaced. The command registry remains the right
  place to expose view-switching affordances while Mighty only routes ids.

### L77. Docked/non-sidebar views need idempotent open ABIs **[finding, P3]**
Adding command-palette entries for Run Output, Problems, and AI Copilot exposed a
small UI contract difference: a "View:" command should open the surface, not
toggle it closed. The Run and AI rail actions are intentionally toggles, so the
shim now exposes explicit `mui_run_open` and `mui_ai_show` entry points for
command routing.

- **IDE note:** command-palette view commands should be idempotent so search
  results are safe to execute repeatedly and never hide the surface the user just
  asked for.
- **Language note:** no new language gap surfaced, but the growing command
  ladder again reinforces L63/L64: Mighty wants generated command-id mirrors or
  enum-style dispatch once the registry keeps expanding.

### L78. Terminal needs an explicit open command separate from toggle **[finding, P3]**
The integrated terminal already had `mui_term_open`, but the command registry
only exposed `Toggle Terminal`. Adding `View: Terminal` lets command-palette
execution be idempotent while leaving Ctrl+` / rail-style interactions free to
toggle.

- **IDE note:** command names should match their behavior. "View:" opens the
  target surface; "Toggle" is reserved for controls that may close it.
- **Language note:** no new language gap surfaced. This reuses the existing
  scalar terminal ABI and only adds a Mighty command-id mirror.

### L79. Shared bottom docks must be mutually exclusive **[finding, P2]**
Run Output, Web Playground, and Problems all draw in the same lower band. Adding
`View: Web Playground` exposed that some open paths only closed Run, leaving
room for overlapping bottom docks. The open/toggle ABIs now make the dock
contract explicit: opening Run closes Web and Problems, opening Web closes Run
and Problems, and opening Problems closes Run and Web.

- **IDE note:** every command-palette "View:" command should recover a coherent
  layout, not just flip one flag. Shared regions need one owner at a time.
- **Language note:** no new Mighty gap surfaced. The exclusivity policy belongs
  shim-side because the shim owns the dock states and draw order.

### L80. Debugger controls should be palette-reachable **[finding, P3]**
The DAP toolbar and function-key routes already existed, but hidden controls make
the IDE feel unfinished. Adding `Debug: Start / Continue`, `Debug: Stop`,
`Debug: Step Over`, `Debug: Step Into`, and `Debug: Step Out` reused the same
scalar `mui_dbg_*` ABI and the central Mighty command dispatcher. A follow-up
added DAP pause plus clean restart of the last target (`Debug: Pause`,
`Debug: Restart`) without growing the Mighty key ladder. No new Mighty language
gap surfaced; the important pattern is keeping all command surfaces (toolbar,
keys, palette, Quick-Open command mode, remapping) routed through the same small
set of scalar calls.

### L81. Breadcrumb geometry must use shaped UI widths **[finding, P2]**
The breadcrumb bar draws with the proportional UI font, but it still advanced
folder/file/symbol labels with a fixed `chars * 0.54em` estimate. That could
misplace separators and make the visible segment disagree with the dropdown
hit-target on long names or glyph-varied paths.

- **IDE note:** `mui_breadcrumb_draw` now advances labels with
  `measure_ui_sized`, and the interactive `CrumbLayout` stores the same measured
  pixel widths before hit-testing and anchoring dropdowns.
- **Language note:** no new Mighty gap surfaced. The right split remains:
  Mighty forwards scalar click events, while the shim owns shaped text metrics
  and geometric hit-testing.

### L82. Welcome-screen shortcut hints need a measured fit rule **[finding, P3]**
The Welcome quick actions had the same proportional-text smell in a smaller
place: shortcut hints were positioned with fixed character-width guesses while
the labels render with the UI font. On narrow columns, a long label plus chord
could either collide or push the chord out of its column.

- **IDE note:** both compact and two-column Welcome layouts now measure the
  label and shortcut with `measure_ui_sized`, right-align the shortcut when it
  fits, move it after a long label when there is still room, and hide it when
  overlap would be unavoidable.
- **Language note:** no new Mighty gap surfaced. This remains a shim layout
  concern; Mighty only routes the resulting quick-action click id.

### L83. UX debt: file flows, overlays, and resizing need a hard manual pass **[backlog, P1]**
Manual feedback on May 31, 2026: the UX still feels poor around "New File",
"Open File/Open Folder", drawer/test-panel overlap, and manual window resizing.
Treat this as a named backlog track, not a collection of incidental polish bugs.
Latest feedback also calls out file actions still feeling weird, drawer/testing
text overlap, clunky manual resizing, and the need to keep a visible list of
these gaps until they are actually closed.

- **IDE note:** audit the complete first-run and file lifecycle path: Welcome
  quick actions, native dialog cancel/success states, typed-path fallback,
  New File/New Folder prompts, Save/Save As, and all resulting toasts. The goal
  is obvious state transitions with no surprise fallback prompts and no hidden
  dirty-buffer risk.
- **IDE note:** audit overlay/drawer layering with Testing, Run, Problems,
  Source Control, Search, command palette, Quick Open, branch picker, keyboard
  shortcuts, code actions, and toasts. Shared regions need one owner at a time,
  modal overlays must dim or clip underlying text, and status/drawer text must
  not bleed through.
- **IDE note:** resizing needs an explicit interaction design pass: discoverable
  resize affordances, less fragile hit targets, no text collision while dragging,
  and stable responsive breakpoints for compact sidebars, welcome layout, docks,
  and bottom panels.
- **Language note:** no new Mighty compiler gap is implied yet. This is mostly
  shim-owned geometry/state/layering, but it will continue to stress the current
  scalar event routing and command-dispatch ergonomics.

### L84. Toast truncation should use measured UI text **[finding, P3]**
Toast messages are sanitized to one line, but the final truncation still used a
fixed character-width estimate. That can leave proportional-font text clipped at
the close icon or over-trim short narrow-glyph messages.

- **IDE note:** toast drawing now truncates with `measure_ui_sized` and a binary
  search for the longest prefix plus ellipsis that fits the card's message
  width.
- **Language note:** no new Mighty gap surfaced. Toast contents and layout stay
  shim-owned; Mighty only ticks/draws/click-routes the overlay.

### L85. New File needs separate untitled and workspace flows **[finding, P2]**
The same "New File" wording previously meant two different actions depending on
where the user clicked it: Welcome/palette created an untitled scratch tab, while
Explorer New File prompted for a named workspace file. That made the file
lifecycle feel arbitrary even though the underlying operations were both useful.

- **IDE note:** the palette now labels `Ctrl+N` as **File: New Untitled File**
  and adds **File: New File in Workspace**, which routes to the existing
  filename prompt and workspace creation path.
- **Superseded by L107:** the primary `Ctrl+N` and Welcome New File route now
  opens the native file picker; scratch buffers live behind the explicit
  **File: New Untitled File** command.
- **Language note:** no new Mighty compiler gap surfaced, but every new command
  still requires manually mirrored numeric ids between Rust and `src/main.mty`.
  This remains a product-friction point for command evolution.

### L86. Bottom dock ownership must feed editor row math **[finding, P1]**
Run, Web, Problems, and Terminal all occupy the same lower-dock band, but the
editor's visible-row calculation only reserved that space for Terminal. Opening
Testing/Run/Problems-style surfaces could therefore leave editor and inline
ghost text believing rows behind the dock were still visible.

- **IDE note:** `MuiContext::bottom_dock_open()` now centralizes the lower-dock
  ownership state, `mui_visible_rows` and ghost text use it, and opening one
  lower dock hides the other lower dock owners.
- **Language note:** no new Mighty compiler issue surfaced. The bug came from
  split UI ownership across shim modules and the scalar ABI exposing too little
  shared layout state by default.

### L87. Borderless resize needs visible affordances **[finding, P2]**
The custom borderless window had resize hit regions, but the affordance was
mostly invisible and the top-edge region competed with the tab row. That made
manual resizing feel like guessing at an unmarked target.

- **IDE note:** side and bottom resize hit targets are wider, the top edge stays
  conservative to protect tab clicks, and the frame now draws subtle bottom
  corner grips.
- **Language note:** no Mighty gap surfaced. This is native-window chrome and
  shim hit-testing work.

### L88. Drawer headers need measured collision budgets **[finding, P2]**
The Run drawer header drew the active filename before computing the right status
pill. Long filenames could therefore render under "running..." or exit-status
text on compact windows.

- **IDE note:** Run header status is measured first, then the filename is
  measured and ellipsized into the remaining gap.
- **Language note:** no Mighty gap surfaced. This is another shim-owned text
  measurement/layout case.

### L89. Problems drawer rows need shaped text budgets **[finding, P2]**
The Problems drawer still used fixed character estimates for empty-state text,
file group headers, and diagnostic messages. In compact widths a negative or
tiny message budget could leave the full diagnostic message visible under the
right-side code/line cluster.

- **IDE note:** Problems drawer text now uses UI-font measurement with binary
  search ellipsizing before drawing those fields.
- **Language note:** no Mighty compiler issue surfaced. This remains shim-owned
  layout, but it reinforces that all proportional chrome text needs measured
  budgets rather than scalar character guesses.

### L90. Testing sidebar result text needs measured budgets **[finding, P2]**
The Testing panel used fixed character counts for summary text, duration, test
names, suite badges, and failure details. That made compact-sidebar rows
dependent on rough glyph guesses and could leave labels crowding each other.

- **IDE note:** Testing sidebar text now measures the UI font before drawing and
  ellipsizes summaries, empty-state copy, result names, suite badges, and failure
  details into their real available pixel widths.
- **Language note:** no Mighty compiler issue surfaced. The scalar ABI keeps
  this draw/layout work in the shim.

### L91. Workspace New File should use a native file picker **[finding, P1]**
The backlog file-flow pass found one remaining mismatch after Open File/Open
Folder/Save As were moved to native dialogs: Explorer's New File button and the
"File: New File in Workspace" command still forced an in-app typed basename
prompt. That made creating a file in a nested folder feel unlike a desktop IDE.

- **IDE note:** workspace new-file flows now call a native SaveFileDialog-style
  picker first. A selected path creates an empty file, opens it as the active tab,
  refreshes Explorer and Quick-Open, and records it in recent files. Cancel and
  existing-file selections are no-ops with explicit state/toasts; the old typed
  prompt is only the fallback when the native picker is unavailable.
- **Language note:** no new Mighty compiler issue surfaced. The implementation
  added another scalar dialog ABI (`mui_newfile_dialog`), reinforcing the need
  for future enum/result ABI shapes so cancel, unavailable, and success do not
  require magic integer codes at every call site.

### L92. Bottom prompt text needs measured query budgets **[finding, P2]**
The bottom prompt still queued one full `label + query` string. Long typed paths,
rename targets, or delete-confirmation names could run across the prompt band and
under adjacent chrome, especially in compact windows or with a visible sidebar.

- **IDE note:** prompt rendering now measures the label separately from the query,
  draws the label in muted chrome, and ellipsizes the query by shaped UI width.
  Query truncation preserves the tail so filenames and path endings remain useful.
- **Language note:** no new Mighty gap surfaced. The pattern matches the other
  recent layout fixes: text fitting belongs shim-side until Mighty has richer UI
  text measurement and string/result ABI support.

### L93. Status bar clusters need shaped text budgets **[finding, P2]**
The status bar still used fixed character-width estimates for branch names,
ahead/behind counters, problem counts, cursor position, encoding, indentation,
and the language pill. Long branch names or large cursor/problem counts could
push the left status cluster into the right cluster on compact windows.

- **IDE note:** status rendering now lays out the right cluster first, measures
  all UI-font text, and only draws the branch/ahead/problem cluster into the
  remaining pixel budget. Long branches tail-ellipsize, and the Problems chip hit
  rect is only recorded when the chip actually fits.
- **Language note:** no new Mighty compiler gap surfaced, but this repeats the
  same product pressure as L89-L92: Mighty can route scalar draw calls today, but
  rich UI text measurement and structured layout/state values still have to live
  in the Rust shim.

### L94. Tab labels need measured close-button budgets **[finding, P2]**
The tab bar still shortened basenames by character count before drawing the
label. Long proportional-font filenames, especially dirty tabs, could visually
crowd the dirty dot and close icon even though the click target remained fixed.

- **IDE note:** tab labels now measure the available pixel gap between the file
  icon and the trailing dirty/close affordances, then tail-ellipsize by shaped UI
  width. Long dirty filenames keep the close icon readable and clickable.
- **Language note:** no new Mighty gap surfaced. This is another case where the
  Rust shim owns text measurement because Mighty currently passes only scalar
  draw commands and does not expose measured chrome text layout primitives.

### L95. Save All should not skip untitled buffers **[finding, P1]**
Save All wrote dirty file-backed tabs but left dirty untitled tabs untouched and
only reported that they needed Save As. That made the save flow feel incomplete:
the user asked to save everything, but the IDE did not ask where the unsaved
buffers should go.

- **IDE note:** Save All now opens the native Save As picker for each dirty
  untitled buffer, writes the selected destination, binds the tab to that path,
  and leaves the buffer dirty only when the picker is cancelled or unavailable.
- **Language note:** no new compiler gap surfaced, but the implementation again
  highlights the scalar dialog-result friction: cancellation, picker
  unavailability, IO failure, and success are still encoded as integer paths
  through the Mighty/Rust ABI instead of a structured result.

### L96. Explorer tree filenames need badge-aware measured budgets **[finding, P2]**
Explorer rows still truncated filenames by character count while synthetic git
status badges were drawn at the right edge. Long proportional-font filenames
could crowd the `M`/`A`/`U` badge area in compact sidebars.

- **IDE note:** Explorer row labels now measure the UI font and fit into the
  real pixel gap before the git badge. Long names tail-ellipsize, preserving the
  useful file ending while keeping badges readable.
- **Language note:** no new Mighty gap surfaced. This remains shim-owned layout
  work caused by scalar draw calls and no Mighty-side measured text primitive.

### L97. Source Control rows need action-aware measured budgets **[finding, P2]**
The Source Control panel still truncated commit text, branch labels, changed-file
names, and directory tails using fixed character estimates. Long filenames or
branches could crowd the ahead/behind counters or the stage/unstage action at
the right edge.

- **IDE note:** Source Control now uses measured UI text fitting for the commit
  box, branch label, changed-file name, and directory tail. File rows reserve the
  stage/unstage action zone before drawing text.
- **Language note:** no new Mighty gap surfaced. This reinforces the same
  shim-side layout requirement as L89-L96: proportional chrome text needs
  measured pixel budgets until Mighty exposes richer UI layout primitives.

### L98. File-command UX and chrome polish still need a full audit **[backlog, P1]**
Manual use still exposes rough UX around core IDE chrome: New File/Open/Save
flows can feel inconsistent, drawer and test text can still overlap or leave
stale toast copy behind, borderless resizing is discoverable but still clunky,
and the logo/taskbar icon presentation needs a cleaner branded pass.

- **IDE note:** audit the whole File menu/palette/toolbar command path against
  native dialog expectations: New Untitled File should be instant, New File in
  Workspace should pick a destination, Open File/Open Folder should always use
  native pickers when available, Save/Save As/Save All should share predictable
  dirty-buffer behavior, and cancel states should clear transient prompts/toasts.
  Continue measuring every drawer row/header/toast before paint, and replace any
  drag-only resize affordance that lacks visible handles or cursor feedback.
- **Language note:** no new compiler blocker is proven yet, but this is the next
  product pressure point for Mighty: structured UI command results, measured
  layout primitives, and clearer native-dialog abstractions would reduce the
  amount of fragile state plumbing currently living in the Rust shim.

### L99. File-operation toasts should replace stale same-operation state **[finding, P2]**
The toast stack could show contradictory operation state at the same time, such
as an older save failure sitting above a newer save success. In manual use that
reads like notification text failed to clear, even though each individual toast
would eventually expire.

- **IDE note:** toast pushes now classify common file/workspace operations
  (save, open, create file/folder, rename, delete, reveal, copy) and replace any
  older toast from the same operation family before showing the new result.
  Identical messages still refresh in place.
- **Language note:** no new Mighty gap surfaced. This is shim-side state hygiene,
  but a richer event/result model in Mighty would make these command families
  explicit instead of inferred from user-visible strings.

### L100. UX fixes need human-visible hit-test evidence **[backlog, P1]**
Core IDE interactions must be judged by what a person can see and click, not by
whether a command enum happens to be wired. File/folder open/save flows,
tab-close and tab-switch behavior, drawer resizing, prompt fallbacks, and window
resizing all need tests that emulate mouse clicks against visible geometry.

- **IDE note:** add or expand regression coverage around native dialog results,
  visible hit rectangles, outside-click cancellation, tab close/switch targets,
  drawer resize handles, and compact-window overlap. The next UX slices should
  include manual/emulated click checks before claiming a workflow is fixed.
- **Language note:** Mighty still delegates most geometry and native-dialog state
  to Rust. A future Mighty UI layer needs first-class hit-test/layout results so
  tests can assert user-visible behavior without duplicating shim geometry.

### L101. Bottom prompt fallbacks should dismiss on outside click **[finding, P2]**
Typed-path prompt fallbacks were keyboard-modal: when a prompt was open, mouse
clicks outside it did not dismiss the prompt or clearly route to the intended UI.
That made native dialog fallbacks feel stuck and inconsistent with normal IDE
click behavior.

- **IDE note:** the shim now exposes a prompt-band hit-test tied to the actual
  rendered bottom prompt geometry, and Mighty cancels the prompt when a mouse
  down lands outside that band.
- **Language note:** no compiler blocker surfaced, but this again shows the need
  for a structured Mighty-side event model that can express "overlay consumed" vs
  "dismiss and continue routing" without local scalar flags.

### L102. Welcome must respond to the editor body's real width **[finding, P2]**
Visual capture of the default IDE window with the Explorer open showed the
Welcome screen still using a two-column layout while the editor body was too
narrow, clipping the recent folders/files column at the right edge.

- **IDE note:** Welcome now chooses compact single-column layout from the actual
  editor body width, not just the centered content column width. The breakpoint
  keeps the first-run surface readable when the sidebar is visible.
- **Language note:** no new Mighty compiler gap surfaced. This is another layout
  primitive gap: the shim owns responsive breakpoints because Mighty does not yet
  express body-relative adaptive layout declaratively.

### L103. Drag UX needs mouse-move events, not click-only hit tests **[finding, P1]**
The bottom output drawers had fixed lower-third geometry and no visible resize
handle, so users had no reliable target and no way to resize Terminal/Run/Web/
Problems by dragging the surface they could see.

- **IDE note:** the shim now emits `MOUSE_MOVE`, Mighty tracks an active bottom-
  dock drag from mouse-down through mouse-up, and the shared dock layout follows
  the live pointer y with min/max clamps. The visible handle is drawn once over
  all bottom-dock owners so Terminal, Run, Web, and Problems behave identically.
- **Language note:** Mighty can route the new scalar event, but real pointer UX
  still requires shim-owned geometry and stateful flags in `main.mty`. A stronger
  Mighty UI layer should expose pointer capture / drag gestures and reusable
  layout constraints directly.

### L104. Drawer actions need one shared visible contract **[finding, P2]**
Bottom drawers had inconsistent exit affordances: Problems had a close path, but
Terminal/Run/Web leaned on rail toggles or Escape, which makes the UI feel broken
when users look for a normal header close button.

- **IDE note:** the shared bottom-dock overlay now owns one close hit target and
  draws one close icon after all lower panels, while Run/Web header content
  reserves space so the icon does not collide with status pills or URL actions.
  The shared geometry must use the physical viewport converted into logical
  pixels, otherwise user zoom/DPI can push right-aligned controls off-screen even
  when the normal `gpu.width` value looks correct.
- **Language note:** no compiler issue surfaced, but the same scalar-pattern
  friction remains: Mighty can call one `*_at_click` function, while the shim
  must own the cross-panel geometry and state transition.

### L105. Destructive close paths should not be timed shortcuts **[finding, P2]**
Dirty tab close and dirty app quit already had a real Save/Discard/Cancel overlay,
but the lower-level ABI still allowed a repeated close/quit within a time window
to discard changes. That is surprising UX and too easy to trigger while testing
broken close buttons.

- **IDE note:** `mui_tab_close` and `mui_quit_request` now only arm the
  confirmation overlay for dirty work. The destructive operation happens through
  `mui_dirty_confirm_discard`, or through `mui_dirty_confirm_save` after a
  successful save.
- **Language note:** no compiler gap surfaced. This is another scalar-state
  coordination issue: Mighty routes the modal interaction, while the shim owns
  the dirty-tab/quit pending state and the actual destructive transition.

### L106. Native dialogs need context, not just a workspace root **[finding, P2]**
Opening a file dialog at the workspace root is technically correct but feels
clunky when the user is editing inside a nested folder and wants the next file
beside it.

- **IDE note:** Open File, New File, Save As, and unsaved-tab Save confirmation
  now seed native file dialogs with the active file's parent directory when it
  exists, falling back to the effective workspace root for untitled tabs.
- **Language note:** no compiler gap surfaced. This is shim-side UX policy;
  Mighty still calls the same scalar dialog functions.

### L107. Primary file commands must match visible labels **[finding, P2]**
The Welcome screen and Ctrl+N both read like "create a file", but the old route
created an untitled scratch tab. That is technically useful but surprises users
who expect to choose a path and filename.

- **IDE note:** the primary New File route now opens the native file picker and
  falls back to the typed filename prompt only when native dialogs are
  unavailable. Scratch tabs moved to an explicit New Untitled File command, and
  compact Welcome layouts hide shortcut hints rather than clipping them at the
  viewport edge. The desktop window also has a minimum inner size now, so manual
  resize cannot crush the borderless chrome into unreadable controls.
- **Language note:** no compiler gap surfaced. The change is a command-routing
  policy fix in Mighty plus existing shim ABI calls.

### L108. Testing headers need pill-aware budgets too **[finding, P2]**
The Testing sidebar rows were already measured, but the header title still drew
before budgeting for the right-side state pill. In compact sidebars, `TESTING`
could visually crowd `running...`, `failed`, or `passed`.

- **IDE note:** Testing header rendering now measures the state pill first and
  fits/ellipsizes the tracked title into the remaining gap before painting it.
- **Language note:** no new Mighty gap surfaced. This is another shim-side
  proportional text layout fix; Mighty still only asks the Testing panel to
  draw.

### L109. Windows taskbar identity must be explicit **[finding, P2]**
Stamping `mighty-ide.exe` with an `.ico` is necessary, but not sufficient for a
polished Windows taskbar experience. Without a stable process AppUserModelID,
Windows can group or display the borderless app under a transient identity.

- **IDE note:** the window shim now sets `Hassard.MightyIDE` via
  `SetCurrentProcessExplicitAppUserModelID` before creating the winit window.
  The call is Windows-only and best-effort so startup is not blocked if the API
  fails.
- **Language note:** no Mighty compiler gap surfaced. This belongs in the Rust
  native-window shim, not in Mighty source.

### L110. Result toasts should model current state, not history **[finding, P2]**
Toast stacks are easy to mistake for current IDE state. If a failed test run,
browser build, or format action remains visible after a later successful retry,
the UI reads as contradictory even when the underlying action worked.

- **IDE note:** the toast queue now groups test results, Web Playground run
  results, format results, and repeated navigation misses the same way it already
  grouped file operations. A newer result replaces the older same-operation
  notification instead of stacking stale text.
- **Language note:** no new Mighty compiler gap surfaced. This remains a
  shim-side state model, but a future Mighty UI toolkit should make stateful
  notification channels explicit instead of relying on message-pattern grouping.

### L111. Human-click harnesses need stable in-process dialog scripts **[finding, P2]**
One end-to-end run now verifies Welcome New File, Explorer New File, Open File,
Save As, Open Folder, and bottom-dock resizing through posted mouse/key events.
That exposed a harness realism issue: a launched process cannot observe later
environment-variable changes from the parent shell, so multiple dialog picks in
one session need a deterministic in-process sequence.

- **IDE note:** the new-file dialog test hook can consume a `|`-separated pick
  sequence, and the Windows harness now drives the visible bottom-dock handle
  with mouse down/move/up events instead of relying only on unit geometry.
- **Language note:** no new Mighty compiler gap surfaced. The work reinforces
  the current boundary: Mighty dispatches scalar UI intents, while the Rust shim
  owns native dialogs, physical input geometry, and drag-state details.

### L112. Borderless chrome must leave real tabs to the IDE **[finding, P1]**
The custom titlebar drag strip shared the same top row as tabs. In the live
mouse harness, clicks on visible tabs and their close buttons were consumed by
the Rust window-chrome interceptor before Mighty could route them, making tab
switching/closing feel broken even though the pure hit-test math was correct.

- **IDE note:** the shim now lets mouse presses inside the occupied tab range
  pass through to Mighty; only the empty caption strip after the visible tabs
  starts a window drag. The harness verifies both tab switching and tab close
  via actual posted mouse events.
- **Language note:** no new Mighty compiler gap surfaced. This is a cross-layer
  ownership problem: Mighty owns semantic tab routing, while the Rust shim owns
  native window movement and must avoid preempting Mighty-owned pixels.

### L113. Destructive modal flows need live post-action coverage **[finding, P1]**
Dirty-tab confirmation looked covered by unit tests, but the real UX risk is the
sequence around it: open the modal, cancel, reopen it, discard, then keep using
dialogs and commands afterward. That is where stale focus, leftover overlay
text, or the wrong active tab would show up.

- **IDE note:** the Windows harness now drives the dirty-close modal with real
  mouse clicks, verifies Cancel and Discard traces, then creates a known visible
  untitled buffer before Save As so later dialog tests are anchored to what the
  user sees. Save paths now trace byte counts while `MUI_TRACE` is active.
- **Language note:** no new compiler gap surfaced. The useful gap is tooling:
  Mighty needs this style of cross-layer scenario harness because scalar unit
  tests cannot prove post-modal focus and active-tab intent.

### L114. Toasts must leave visible edge space in compact frames **[finding, P2]**
Offscreen compact screenshots showed the toast stack too close to the right
edge; the card, text, and close affordance read as clipped even when the message
itself fit. Notifications should feel temporary but not accidental.

- **IDE note:** toast layout now uses one shared geometry model for drawing and
  hit-testing, with a right-side safety inset and width clamp so cards remain
  fully inside compact viewports. The dismissal test now derives its click
  target from that geometry instead of assuming flush-right cards.
- **Language note:** no compiler gap surfaced. The lesson is ergonomic: Mighty
  UI helpers should expose reusable geometry primitives so drawn shapes and
  hit-tests cannot drift.

### L115. Resize targets need hover feedback, not just bigger hit zones **[finding, P2]**
Wide resize hit bands help, but users still need cursor feedback before they
press. Without hover feedback, borderless edges and dock dividers feel like
guesswork even when the eventual drag works.

- **IDE note:** the window event path now emits hover mouse moves to the shim.
  Mighty still does not see those ordinary moves; the shim consumes them after
  setting the native cursor for window edges, diagonal corners, dock dividers,
  or default pointer state.
- **Language note:** no compiler gap surfaced. Mighty currently relies on the
  Rust shell for native cursor affordances; a future UI layer should make hover
  cursor intent a first-class control property.

### L116. Mouse QA needs to click visible command rows, not just press Enter **[finding, P2]**
The command palette can pass keyboard tests while still failing the human path:
open the palette, read a filtered row, click it, and expect the command to run.

- **IDE note:** the Windows live harness now mirrors the command-palette result-row
  geometry and clicks the visible `Open File` command row before exercising the
  native file picker and save path. The shim traces both `palette_click` row hits
  and the picked `open_file_dialog` path so a future regression identifies
  whether the miss was geometry, dispatch, or dialog handling.
- **Language note:** no compiler gap surfaced. The lesson is toolchain-level:
  Mighty UI tests need exported or shared layout geometry so black-box click
  tests do not duplicate fragile constants by hand.

### L117. Visual QA must reject untrustworthy screenshots **[finding, P1]**
The live harness can successfully drive the IDE by posting window messages even
when Windows refuses to make the IDE foreground. In that state `CopyFromScreen`
captures whatever is actually on top, which can produce polished but irrelevant
desktop screenshots.

- **IDE note:** capture mode now refuses to save PNGs when the IDE window is not
  foreground or has invalid dimensions, and marks the harness failed instead of
  logging a soft warning. `-CaptureSmokeOnly` verifies screenshot trust quickly;
  `-NoCapture` remains the reliable functional smoke path for noninteractive
  runs.
- **Language note:** no compiler gap surfaced. The testing lesson is that
  Mighty needs first-class visual QA metadata: screenshots should carry a
  provenance check, not just bytes on disk.

### L118. Screenshot galleries should validate artifacts before process liveness **[finding, P2]**
The snippet auto-open path can write a valid offscreen screenshot while the
headless process keeps running long enough for the gallery timeout. Treating
process exit as the only success signal made one good visual artifact fail the
entire UX audit.

- **IDE note:** the overlay gallery now validates the PNG first when a process
  times out; if the artifact is useful and has the IDE chrome signature, the case
  passes and the harness kills the lingering process. Missing or invalid PNGs
  still fail.
- **Language note:** no compiler gap surfaced. This is another testing
  contract: visual evidence should be artifact-first, with process liveness as
  cleanup telemetry rather than the primary success condition.

### L119. Docked drawers must anchor to visible width, not raw logical GPU width **[finding, P1]**
The AI copilot gallery passed while its right edge visibly clipped prose and code.
The drawer used the raw logical GPU width, which can exceed the captured or
physical surface under Windows scaling; other bottom docks already clamp through
`dock_visible_width`.

- **IDE note:** AI copilot draw and click geometry now use the visible dock width,
  so the panel starts far enough left for its wrapped transcript and send button
  to stay inside the pixels a user can actually see.
- **Language note:** no compiler gap surfaced. Mighty UI needs a shared
  `visible_width` primitive at the language-side layout boundary so future
  drawers do not rediscover physical/logical width mismatches one panel at a
  time.

### L120. File commands must read like one command family **[finding, P2]**
The command palette exposed some file operations as `File: ...` and others as
bare verbs like `Open File`, `Save As`, and `Save All`. The behavior was mostly
right, but the human-facing labels made the file flow feel inconsistent.

- **IDE note:** Open, Save, Save As, Save All, and Open Folder now use consistent
  `File:` labels, and dialog-backed operations say so in their palette
  descriptions. Command IDs and shortcuts did not change.
- **Language note:** no compiler gap surfaced. A future Mighty UI command API
  should let command groups own labels and dialog intent centrally instead of
  repeating prose in each registry row.

### L121. Welcome quick actions should signal when a picker or prompt follows **[finding, P2]**
Welcome already used ellipses for Open File/Open Folder, but New File/New Folder
looked like immediate actions even though they require a destination or name.

- **IDE note:** Welcome now labels New File and New Folder with ellipses, matching
  the command palette and native-dialog/prompt behavior.
- **Language note:** no compiler gap surfaced. This is a design-system rule:
  action labels should encode whether the command completes immediately or asks
  for more user input.

### L122. Shortcut remapping clicks should select before capturing **[finding, P2]**
The Keyboard Shortcuts overlay said `Enter to remap`, but a mouse click on any
row immediately entered capture mode. That made exploratory clicking feel like a
broken button because the next typed key could be interpreted as a remap attempt.

- **IDE note:** shortcut rows now use a two-step mouse interaction: first click
  selects and updates the footer, clicking the selected remappable row again
  starts capture. Keyboard users still press Enter to remap.
- **Language note:** no compiler gap surfaced. The issue was interaction-state
  modeling: click handlers often need action codes beyond hit/miss when a row
  can both select and activate.

### L123. Resize affordances need precise presets, not only drag targets **[finding, P2]**
The shared lower dock had a visible drag handle and cursor feedback, but resizing
still depended on a manual drag. That is clunky for users who just want a
predictable compact/default/expanded panel height.

- **IDE note:** Terminal/Run/Web/Problems now share compact, reset, and expanded
  dock buttons beside the close affordance. These presets are handled before the
  drag path, so a click is exact and cannot accidentally start a resize gesture.
- **Language note:** no compiler gap surfaced. The UI glue still shows why Mighty
  needs better first-class action-result modeling: scalar hit-test functions are
  doing the work of a richer command/event object.

### L124. Drawer hit tests must not preempt active overlays **[finding, P1]**
The dirty-tab confirmation could overlap the bottom dock divider. A human click
on the confirmation button was being intercepted as a dock resize start when it
landed on the same y band.

- **IDE note:** bottom-dock close, preset, and resize hits now stay inactive
  while modal/overlay surfaces own input. Dirty-confirm, keyboard shortcuts,
  breadcrumb menus, and branch switching can no longer lose clicks to the drawer
  underneath.
- **Language note:** no compiler gap surfaced. This is another case where Mighty
  would benefit from a central z-ordered event router instead of repeated scalar
  hit tests in source order.

### L125. App identity must survive taskbar scale **[finding, P2]**
The previous rounded gradient taskbar icon was readable but generic. It did not
clearly connect the Windows shell icon, the in-app rail logo, and the Welcome
brand surface.

- **IDE note:** the ICO generator now emits a darker IDE tile with a cyan rail
  accent and compact violet command corner. The rail logo and Welcome tile use
  the same structure so the mark is consistent across taskbar, window chrome, and
  first-run UI.
- **Language note:** no compiler gap surfaced. The gap is asset workflow: native
  apps need reproducible icon generation and visual QA alongside code tests.

### L126. Result drawers need column budgets that can disappear **[finding, P2]**
The Testing drawer measured text, but its right-side suite column still reserved
space in narrow sidebars. That could make long test names and suite labels fight
for the same row instead of prioritizing the actionable test name.

- **IDE note:** Testing now gives the suite column a smaller measured budget and
  hides it entirely when the sidebar is too narrow or the test-name column would
  fall below a readable width.
- **Language note:** no compiler gap surfaced. The broader design need is a
  reusable responsive row-layout primitive so every drawer can express optional
  columns declaratively.

### L127. Modal hit boxes must use logical visible dimensions **[finding, P1]**
The dirty-tab confirmation dialog was drawn and hit-tested from raw GPU
dimensions while mouse events were DPI-scaled logical coordinates. Under scaling,
button clicks could miss or land on the underlying dock.

- **IDE note:** dirty-confirm geometry now uses the visible logical width and
  height derived from physical window size, matching the coordinate space used by
  mouse events.
- **Language note:** no compiler gap surfaced. Mighty-side code needs fewer raw
  geometry scalars and a shared `visible_surface` abstraction from the host.

### L128. Overlay stacks need one visible-surface contract **[finding, P1]**
The toast overlay used raw GPU dimensions while mouse events and scaled UI chrome
used logical visible dimensions. On DPI-scaled windows, the newest toast could be
drawn partly off-screen and its click target could drift from what the user saw.

- **IDE note:** toast draw and dismissal now use the same visible logical width
  and height helpers as modal overlays.
- **Language note:** no compiler gap surfaced. This reinforces the need for a
  host-provided `visible_surface` value instead of repeated width/height scalar
  plumbing in Mighty-facing UI calls.

### L129. Lower docks must share the same scaled geometry as overlays **[finding, P1]**
The lower dock had DPI-aware width but still used raw GPU height for its resize
band, preset/close buttons, visible editor rows, and terminal grid. Under Windows
scaling that made the dock feel clunky: controls could render or hit-test below
the logical mouse space.

- **IDE note:** shared lower-dock and Terminal geometry now use the same visible
  logical surface as modals and toasts.
- **Language note:** no compiler gap surfaced. The host should expose a single
  surface descriptor to Mighty instead of separate raw width/height and physical
  width/height values that every feature must reconcile manually.

### L130. Integrated terminals need basic cursor-addressing CSI support **[finding, P1]**
The VT parser skipped cursor-position CSI sequences. Windows ConPTY uses those
while painting `cmd.exe` startup output, so the prompt could appear appended to a
previous line instead of where the terminal intended it.

- **IDE note:** the Terminal parser now handles `CSI row;col H` and `CSI row;col f`
  with 1-based coordinates and visible-grid clamping.
- **Language note:** no compiler gap surfaced. Terminal behavior belongs in a
  host-side subsystem until Mighty can own long-lived byte streams, parser state,
  and structured grids comfortably across FFI.

### L131. Human-facing command names need one source of truth **[finding, P2]**
File commands were functionally routed to native dialogs, but the command palette
still presented multiple near-identical "New File" variants. That made the UX
feel broken even when the dispatch path worked.

- **IDE note:** the palette now distinguishes file creation with a chosen disk
  location, blank untitled tabs, and Explorer-scoped file/folder actions. Overlay
  footers also use fewer instructional hints so shortcut text has room to breathe.
- **Language note:** no compiler gap surfaced. Longer term, Mighty needs a
  typed command descriptor record shared by menus, palette, shortcuts, welcome,
  telemetry, and tests so labels/descriptions cannot drift between surfaces.

### L132. Sidebar rows need measured wrapping, not fixed one-line slots **[finding, P2]**
The Testing panel treated every failed test detail as exactly one row. Long trap
messages were technically present but visually chopped, making the drawer feel
overlapped and unreliable.

- **IDE note:** failed test details now reserve up to two measured lines, and row
  click hit-testing uses the same measured visual height.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a
  reusable text-layout primitive that returns wrapped line boxes for a string,
  width, font size, and maximum line count so panels do not reimplement wrapping.

### L133. Recents lists must self-heal before users see them **[finding, P2]**
Quick Open could show files that were deleted after a test harness run or a
temporary workflow. The stale row was only removed after the user selected it,
which made the picker look broken.

- **IDE note:** Quick Open and Welcome now prune missing recent files before
  rendering recents, persist the cleaned list, and fall back to workspace files
  when pruning leaves the picker with no usable recents.
- **Language note:** no compiler gap surfaced. A future Mighty standard library
  should make filesystem predicates and MRU persistence ergonomic enough to keep
  this logic in app code instead of host shims.

### L134. Demo data in screenshots must be real actions, not fake paths **[finding, P2]**
The Welcome gallery used representative recent-folder paths under
`C:\Users\you\...`. That made the screen look populated, but it trained the UI
to tolerate rows that could never open.

- **IDE note:** Welcome now prunes missing recent folders before drawing, persists
  the cleaned MRU, and seeds gallery captures with real bundled folders.
- **Language note:** no compiler gap surfaced. Mighty should eventually have a
  typed fixture/story system for UI states so screenshots can use real resources
  without ad hoc host-side environment hooks.

### L135. Dense modals need explicit viewport margins **[finding, P2]**
The Settings overlay fit mathematically, but the rendered footer sat on the
bottom crop line in gallery captures. A best-in-class IDE should treat modal
margins as a first-class constraint, not an accidental leftover after rows are
laid out.

- **IDE note:** Settings now uses a more compact row rhythm, reserves a larger
  viewport margin, and keeps the footer inside the visible card.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from a
  layout primitive that constrains cards by header, body rows, footer, and safe
  viewport margins in one reusable calculation.

### L136. UX probes should be tracked tools, not stray worktree noise **[finding, P3]**
The Source Control panel exposed local capture/input helper scripts as untracked
changes. That made the repo look dirty and hid whether there were real product
changes waiting for review.

- **IDE note:** the Windows capture/input probes are now documented as tracked
  tools alongside the gallery and full UI harness.
- **Language note:** no compiler gap surfaced. A future Mighty package/workspace
  convention should distinguish generated scratch files from first-class
  developer tools so IDE status panels can explain them cleanly.

### L137. Screenshot close events need test-safe semantics **[finding, P2]**
The snippet gallery rendered correctly, but the process could loop until the
gallery harness killed it because the synthetic post-capture Close event reused
the same dirty-file confirmation path as a real user quit.

- **IDE note:** headless/screenshot/probe runs now exit directly on Close, while
  normal interactive closes still route through the unsaved-work confirmation.
- **Language note:** Mighty would benefit from a first-class event/source flag so
  app code can distinguish user window closes from synthetic harness closes
  without threading host-mode checks through the main loop.

### L138. Native dialogs must be owned by the IDE window **[finding, P2]**
Open/New/Save/Open Folder used a tiny topmost helper form as the modal owner.
That kept dialogs visible, but it made them feel detached from the IDE and could
place focus or centering in surprising places.

- **IDE note:** Windows native file/folder dialogs now receive the IDE HWND as
  their owner and fall back to the helper form only when no real window exists.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a small
  platform-dialog ABI wrapper so app code can request modal file/folder pickers
  without embedding PowerShell snippets in host code.

### L139. Alignment QA must include the minimum window size **[finding, P2]**
The default gallery passed at 1280x832, but Settings clipped at the bottom when
captured at the supported 860x560 floor. Desktop-only screenshots were hiding a
real resize/overlap defect.

- **IDE note:** the gallery runner now accepts `-Width` / `-Height`, and Settings
  reserves extra small-window vertical margin so the card fits instead of
  clipping.
- **Language note:** no compiler gap surfaced. Mighty needs a reusable
  constraint/layout primitive for modal sizing across viewport breakpoints.

### L140. Dense overlays need breakpoint-specific visible-row counts **[finding, P2]**
The full 860x560 gallery exposed that Keyboard Shortcuts was technically inside
the viewport but visually jammed at the bottom because it kept the desktop row
count in a short window.

- **IDE note:** Keyboard Shortcuts now reduces visible rows on short windows and
  keeps the footer comfortably inside the card at the minimum supported size.
- **Language note:** no compiler gap surfaced. Mighty needs a shared list-modal
  layout helper that computes visible rows, scroll top, and footer-safe card
  bounds consistently across Palette, Quick Open, Shortcuts, and Settings.

### L141. Rendered preview wrapping must be conservative **[finding, P2]**
The Markdown preview looked fine at desktop size, but at 860x560 the split pane
was narrow enough for a large H1 to clip horizontally. The wrapper estimated text
too optimistically for large shaped UI glyphs.

- **IDE note:** Markdown preview now uses a more conservative proportional-width
  estimate, includes inline-code chip padding in wrap width, and clamps preview
  layout to the visible window width before wrapping. Narrow preview panes also
  use a tighter content column so body text wraps before it reaches the surface
  edge, and italic inline spans now advance with the same font path used to draw
  them so adjacent words do not collide.
- **Language note:** no compiler gap surfaced. Mighty UI needs a measured text
  layout API usable from pure layout code so wrapping can use real glyph metrics
  instead of host-side approximations.

### L142. Notification stacks must collapse by user task **[finding, P2]**
The toast stack can become stale even when each card is individually valid:
theme changes, run/build diagnostics, save feedback, and file operations are all
task-level status, not a permanent log. At minimum size, too many live cards also
covers the editor and reads like overlapping UI.

- **IDE note:** Toasts now show at most three cards and replace stale theme and
  Mighty diagnostic feedback within their own families, the same way save/run
  result toasts already replace earlier results. This keeps transient feedback
  useful without turning the editor into a notification pile.
- **Language note:** no compiler gap surfaced. Mighty needs a first-class
  notification primitive with a stable `group`/`replace_key`, rather than
  relying on Rust-side string prefix classification.

### L143. Taskbar icons need small-size visual QA **[finding, P2]**
The 256px brand mark can look polished while the Windows taskbar version still
reads as lopsided or noisy. The previous large cyan rail and corner wedge carried
the brand at full size, but became heavy once Windows shrank it.

- **IDE note:** The generated Windows icon now uses a centered Mighty monogram,
  a thin accent frame, a bottom cyan rule, and a small violet command dot. The
  icon generator also remains the source of truth for the packaged `.ico`.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a
  repo-native visual fixture runner for generated assets so 16/32/48px previews
  can be reviewed alongside UI screenshots.

### L144. Overlay anchors must respect active work surfaces **[finding, P1]**
Toasts were technically on the overlay layer, but while the Web/Run/Problems
bottom dock was open they anchored to the window bottom and covered console text.
That is visually equivalent to overlap even when z-order is intentional.

- **IDE note:** Toast layout and click hit-testing now reserve the shared bottom
  dock height when any lower work panel is active, so notifications float above
  the dock instead of covering output.
- **Language note:** no compiler gap surfaced. Mighty UI needs an overlay
  placement service where surfaces can publish occupied regions and transient
  layers can anchor against those regions automatically.

### L145. File and folder creation should use one mental model **[finding, P2]**
New File had moved to a native picker, but New Folder still opened the in-app
bottom prompt. That made the Explorer header and Welcome actions feel
inconsistent: users had to remember two different creation flows.

- **IDE note:** New Folder now opens a native folder picker with the system "New
  Folder" affordance, with the typed prompt retained only as an unavailable-dialog
  fallback. The palette metadata now describes the native-picker behavior.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a
  standard command-result convention for cancelled vs unavailable native UI so
  fallback routing stays compact on the Mighty side.

### L146. Disk mutations must reconcile with open buffers **[finding, P1]**
Project Replace wrote matching files on disk, but clean tabs already showing
those files could keep stale text in the editor. That makes a successful replace
look like it failed until the user manually reloads, and dirty tabs need explicit
conflict signaling instead of silent overwrite.

- **IDE note:** Project Replace now returns changed paths, refreshes any clean
  open tab from disk, and leaves dirty open tabs untouched with a warning toast.
- **Language note:** no compiler gap surfaced. Mighty needs a first-class file
  event / buffer invalidation channel so disk-changing commands can refresh or
  conflict-mark open documents without bespoke shim plumbing.

### L147. Precision layout actions should not depend on mouse dragging **[finding, P2]**
The shared bottom dock had visible drag and preset buttons, but users still had
to discover and operate a small mouse target to recover from an awkward dock
height. A best-in-class IDE should expose layout state through commands too.

- **IDE note:** Bottom dock compact/default/expanded are now command-palette
  actions, routed through one shim dispatch range, and the visible preset button
  matching the current dock size is highlighted.
- **Language note:** no compiler gap surfaced. Mighty still benefits from the
  shim-range dispatch pattern for feature clusters because the current parser
  stack makes long command ladders fragile.

### L148. Drawer sizing needs command-level control **[finding, P2]**
The sidebar width was responsive to the window, but not directly controllable.
That forced users to resize the entire app just to give Search, Debug, Testing,
or Explorer more room, which makes drawer-heavy workflows feel clunky.

- **IDE note:** Sidebar compact/default/wide are now command-palette actions.
  The commands open the Explorer when the sidebar is hidden, resize every sidebar
  drawer through one shared preset, and are covered by the real-window harness.
- **Language note:** no compiler gap surfaced. Mighty still needs a cleaner
  command-group declaration/dispatch form so related actions can be registered
  and routed without manually mirroring numeric ranges in Mighty source.

### L149. Window chrome actions need command equivalents **[finding, P2]**
The borderless titlebar had visible maximize/restore chrome, but the same action
was not reachable from the command system. Users who miss the small frame button
or prefer keyboard-driven layout control should not need to aim at custom window
chrome.

- **IDE note:** `Window: Toggle Maximize` is now a palette command routed through
  the shared command dispatcher to the native window maximize/restore hook, with
  trace coverage in the real-window harness.
- **Language note:** no compiler gap surfaced. Mighty still needs command
  metadata generation so Rust command ids, Mighty mirror functions, labels, and
  harness coverage do not drift as the command surface grows.

### L150. Minimize is a workflow command, not only chrome **[finding, P2]**
Minimizing a borderless app through a small custom titlebar button is easy to
miss, especially during keyboard-driven work. The action should be available
through the same command surface as other layout and window operations.

- **IDE note:** `Window: Minimize` is now a command-palette action that routes
  through the shared Mighty dispatcher to the native window minimize hook, with
  real-window harness coverage at the end of the interaction suite.
- **Language note:** no compiler gap surfaced. The repeated command-id mirroring
  reinforces the need for generated command bindings shared by Rust and Mighty.

### L151. Native picker success must map to visible workspace change **[finding, P1]**
The New Folder command used a native folder picker, but accepting an arbitrary
folder outside the current workspace could produce a success toast while the
Explorer tree did not change. To a user, that reads as a broken button even
though the dialog returned a valid path.

- **IDE note:** New Folder now validates that the selected or newly created
  folder is inside the current workspace before accepting it. Outside selections
  produce a clear warning and no fake success state.
- **Language note:** no compiler gap surfaced. Mighty still needs a richer
  command-result type so native picker outcomes can carry success, cancelled,
  unavailable, and invalid-for-current-context without integer sentinels.

### L152. Workspace file creation must not create invisible tabs **[finding, P1]**
New File in Workspace used a native save-file picker, but it accepted any path
the picker returned. Picking outside the workspace created and opened a real
file while Explorer and Quick Open stayed rooted elsewhere, making the action
feel inconsistent with its label.

- **IDE note:** Workspace New File now validates the selected path against the
  current workspace root before creating it. Outside picks are rejected with a
  warning and do not create a file or open a tab.
- **Language note:** no compiler gap surfaced. This is another case where a
  structured picker result would reduce Mighty-side sentinel handling and keep
  command labels, scopes, and outcomes aligned.

### L153. Layout recovery needs command paths, not only small chrome **[finding, P2]**
The shared lower dock had a visible close button, but no command-palette action
for closing it. Users who open Problems, Run, Web, or Terminal should have a
predictable keyboard/searchable way to restore editor space without aiming at a
small header affordance or remembering which panel toggle opened it.

- **IDE note:** `View: Close Bottom Dock` now routes through the shared dock
  dispatcher and closes whichever lower dock owner is active.
- **Language note:** no compiler gap surfaced. The separate close id reinforces
  the need for generated command metadata with semantic groups, because related
  layout actions are no longer always contiguous numeric ranges.

### L154. Right-docked panels need explicit close commands **[finding, P2]**
The AI copilot could be opened from the command palette, but closing it still
depended on the rail toggle behavior. That makes editor-space recovery harder
to discover and less predictable than other docked surfaces.

- **IDE note:** `View: Close AI Copilot` now closes the right-docked AI panel
  without clearing transcript or input state, and no-ops with a clear toast when
  it is already hidden.
- **Language note:** no compiler gap surfaced. The command surface is now large
  enough that Mighty should generate command ids, labels, and dispatcher stubs
  from one source instead of mirroring them manually.

### L155. Left drawers need deterministic close commands too **[finding, P2]**
The sidebar had a toggle and explicit width presets, but no command that meant
"make the drawer closed." Toggle commands are ambiguous in a command palette:
they are fine for shortcuts, but weaker for recovering space after Search,
Testing, Source Control, or Explorer has expanded the layout.

- **IDE note:** `View: Close Sidebar` now hides the left drawer without changing
  the active sidebar panel, no-ops with a clear toast when already closed, and is
  covered by both unit tests and the live Windows input harness.
- **Language note:** no new compiler gap surfaced. This reinforces L154: Mighty
  needs generated command metadata and dispatcher stubs so command families can
  expose open/close/resize semantics without manual Rust/Mighty id mirroring.

### L156. Split panes need feature-aware minimum widths **[finding, P2]**
The minimum-window Markdown preview gallery showed the editor minimap still
rendering inside a narrow left split pane. It was technically clipped to the
pane, but visually it covered source text, which users experience as overlap.

- **IDE note:** the editor minimap now has a stricter width gate in split panes:
  it still appears in the focused pane when the column is wide enough, but hides
  in compact split/preview layouts so code remains readable.
- **Language note:** no compiler gap surfaced. Mighty UI needs responsive
  feature gates attached to layout primitives, so optional affordances can yield
  space to primary content without bespoke host-side width checks.

### L157. Visual QA tools must normalize artifact paths **[finding, P2]**
The overlay gallery accepted a repo-relative `-OutDir`, but launched the IDE
from the packaged app directory. That made `MUI_SCREENSHOT` point at a different
relative path than the report verifier checked, so the tool could falsely report
missing screenshots even when the app exited normally.

- **IDE note:** `tools/overlay-gallery.ps1` now resolves `-Exe`, `-WorkDir`, and
  `-OutDir` to absolute paths before launching any capture case, so normal
  repo-relative usage and CI-style invocations write and verify the same files.
- **Language note:** no compiler gap surfaced. The broader Mighty tooling need
  is a standard artifact-path convention for screenshot/test harnesses so app
  workdirs do not leak into verifier paths.

### L158. Visual QA defaults should follow the repo, not one workstation **[finding, P2]**
The overlay gallery's default executable, workdir, and output directory were
absolute paths under the current developer profile. That worked locally, but it
made the visual audit tool fragile for another checkout, another Windows user,
or future CI.

- **IDE note:** `tools/overlay-gallery.ps1` now derives default paths from the
  script's repo root while preserving explicit `-Exe`, `-WorkDir`, and `-OutDir`
  overrides. A fresh clone can run the same gallery command after packaging.
- **Language note:** no compiler gap surfaced. Mighty tooling should prefer
  repo-relative defaults and only resolve to absolute paths at process-launch
  boundaries.

### L159. Dialog smoke tests must prove state, not responsiveness **[finding, P1]**
The Open Folder live-harness step only checked that the IDE was responsive after
the native picker returned. That would miss the exact failure users experience:
the dialog appears to work, but the Explorer and workspace state do not change.

- **IDE note:** workspace re-rooting now emits a `workspace_open_folder` trace
  when `MUI_TRACE` is active, and the Windows harness picks a separate temporary
  folder for Open Folder. The run now fails unless the selected folder becomes
  the active workspace.
- **Language note:** no compiler gap surfaced. Mighty needs richer black-box UX
  assertions around command effects, because scalar command success and app
  liveness are not enough to prove visible human workflows.

### L160. Inline overlays must use visible-surface bounds too **[finding, P2]**
The minimum-window Peek Definition gallery showed the card extending past the
right edge. The body was mostly readable, but the header hint was clipped
off-screen, which makes the inline navigation surface feel unfinished.

- **IDE note:** Peek now computes its card from `dock_visible_width` and
  `visible_height`, matching the DPI/scaled-window contract used by docks and
  other overlays. The header label and shortcut hint also get separate clipped
  budgets so they cannot overlap in compact frames.
- **Language note:** no compiler gap surfaced. This reinforces the broader
  Mighty UI need for a single host-provided visible-surface/layout primitive
  rather than each feature rediscovering raw GPU vs visible window bounds.

### L161. Visual fixtures must demonstrate the feature, not just open the surface **[finding, P2]**
The compact Outline gallery opened the Outline panel, but it was scanning the
default scratch buffer and therefore showed "No symbols." That verifies panel
chrome, but it does not verify the symbol rows, nesting, current-row highlight,
or text budgets a user actually cares about.

- **IDE note:** the Outline screenshot hook now seeds a representative Mighty
  source file before refreshing the outline, then parks the cursor inside a
  nested method so the captured panel contains real agent, function, and struct
  rows.
- **Language note:** no compiler gap surfaced. Mighty tooling still needs
  first-class UI stories/fixtures so visual QA states are declared as product
  scenarios instead of one-off host-side environment hooks.

### L162. Brand assets need one source of truth across native shell and in-app UI **[finding, P2]**
The rail logo, Welcome logo, generated `.ico`, and Windows executable stamp can
drift because the in-app mark is hand-drawn in Rust while the taskbar icon is
drawn by `tools/make-icon.py`. The latest cleanup simplified both copies to the
same teal-rail / violet-baseline monogram, but they are still manually kept in
sync.

- **IDE note:** the corner accent dot was removed because at rail and taskbar
  sizes it looked like an unread notification or stray badge rather than part of
  the Mighty identity.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a
  declarative/vector asset story that can emit both runtime drawing commands and
  packaged native assets from the same source.

### L163. Visible prompt controls must have matching event routes **[finding, P1]**
The bottom prompt looked like a raw text band at compact size and gave users no
obvious mouse affordance for closing it. Adding a close icon alone would have
been worse unless the Mighty event loop could route clicks to it.

- **IDE note:** bottom prompts now reserve right-side space for `Enter / Esc`
  help plus a real close button. The dedicated find/replace bar now does the
  same for its two-row surface. The shim hit-tests those buttons and Mighty
  cancels the active prompt/bar from the same event arm before generic band
  click handling.
- **Language note:** no new compiler gap surfaced, but this repeats a recurring
  Mighty UI pattern: every visible control needs a scalar hit-test ABI because
  Mighty cannot hold rich widget state directly yet.

### L164. Overlay highlight geometry must use shaped text, not character math **[finding, P2]**
The Signature Help compact gallery showed the active parameter highlight drifting
over `b: I32`. The popup was rendered with JetBrains Mono, but the highlight was
positioned by counting characters and multiplying by a nominal cell width. That
is close enough for raw editor columns, but it is not reliable for chrome-sized
overlay text shaped by glyphon.

- **IDE note:** `Text::measure_sized` now exposes shaped code-font extents for a
  caller-specified size. Signature Help uses it for the signature label, active
  parameter prefix, and active parameter width, so the bubble and accent redraw
  align with what users actually see.
- **Language note:** no compiler gap surfaced. Mighty still needs a first-class
  text measurement/layout primitive so Mighty-authored UI can ask the renderer
  for shaped extents instead of duplicating approximate geometry in scalar code.

### L165. First-run file actions should prioritize creation **[finding, P2]**
The Welcome Start list still placed `New File...` below Open File, Open Folder,
Quick Open, and Command Palette. After the primary New File flow moved to a
native picker, burying it made the first-run path feel inconsistent with the
toolbar and shortcut story.

- **IDE note:** Welcome now puts `New File...` first, then Open File/Open Folder,
  Quick Open, Command Palette, and New Folder. The live Windows harness was
  updated to click the visible first-row New File target so the test follows the
  human path instead of a stale coordinate.
- **Language note:** no compiler gap surfaced. The friction is command metadata:
  Mighty still benefits from a single command/action registry that can drive
  Welcome, palette, shortcuts, toolbar order, and harness targets together.

### L166. Docked panels need visible close affordances, not just command exits **[finding, P2]**
The AI Copilot right dock had a command-palette close path, but the compact
visual pass still showed no visible close button in the panel header. Users do
not discover command exits while scanning a docked surface; they look for a
nearby header affordance.

- **IDE note:** AI Copilot now draws a header close button inside its own visible
  panel band and `mui_ai_click` returns a distinct close code. Mighty routes that
  click to `mui_ai_close`, matching the existing close command without clearing
  transcript state. The live mouse harness caught one extra cross-layer bug: the
  close button sat inside the borderless titlebar's drag strip, so the shim now
  passes that rect through to Mighty before starting a native window drag.
- **Language note:** no compiler gap surfaced. This is the same scalar hit-test
  pattern as bottom docks and prompts; Mighty would benefit from declarative
  widget actions so every visible button automatically owns both paint and event
  routing.

### L167. Modal close affordances need paired paint and hit-test ownership **[finding, P2]**
Settings and Keyboard Shortcuts showed `Esc close` in footer copy, but compact
visual review still left users without a familiar visible close target. Adding a
drawn button alone would have been another dead affordance unless the same
geometry also powered the mouse route and live harness proof.

- **IDE note:** Settings and Keyboard Shortcuts now draw close buttons in their
  modal headers, return distinct click codes from their shim hit tests, and emit
  `settings_close` / `shortcuts_close` traces when dismissed. The Windows harness
  clicks the visible button centers, rather than relying on keyboard Escape.
- **Language note:** no new compiler gap surfaced, but this repeats the UI
  architecture problem from L166. Mighty needs a widget/action primitive that
  binds the visual rect, action id, accessibility label, and traceable event in
  one place so modal controls cannot drift from their hit tests.

### L168. Secondary editor panes need local close affordances **[finding, P2]**
The Markdown Preview pane visually reads as a split editor surface, but before
this pass there was no obvious local way to close it. Users should not have to
infer that pane commands or the palette are the escape path for a rendered view.

- **IDE note:** Markdown Preview now draws a close button in its pane header,
  hit-tests that exact rect through `mui_md_close_at_click`, and collapses the
  preview split through the existing `mui_md_close` pane-close path.
- **Language note:** no compiler gap surfaced. The recurring language-side need
  is still a typed action/widget registry so render, hit-test, command routing,
  and test harness coordinates do not need parallel hand-maintained geometry.

### L169. Visual hooks must survive Mighty startup file loading **[finding, P2]**
The breadcrumb compact-gallery case opened a blank editor because the hook seeded
state too early: the later Mighty-side `mui_ed_load` startup path replaced the
demo model with `scratch.mty`, then the outline refresh found zero symbols. That
made the capture look like a layout problem while the real issue was lifecycle
ordering between test hooks and normal editor initialization.

- **IDE note:** `MUI_BREADCRUMB_AUTOOPEN=symbol` now seeds a representative
  Mighty file, moves the cursor onto a function symbol, and uses the existing
  edit-probe lock so `mui_ed_load` preserves the seeded model. The resulting
  gallery screenshot now verifies the actual breadcrumb symbol dropdown.
- **Language note:** no compiler gap surfaced. Mighty still needs a cleaner
  startup/test-hook lifecycle, ideally a post-load UI initialization phase or
  declarative scenario fixture API, so visual QA state is not hidden behind
  ad hoc ordering locks.

### L170. Live-preview modals still need explicit close actions **[finding, P2]**
The Color Theme picker previewed themes live while the user moved through rows,
but it still relied on Escape or an outside click to cancel. That is especially
awkward for a preview modal because users need a clear way to back out after the
whole IDE changes color.

- **IDE note:** Theme Picker now draws a header close button, hit-tests that
  exact rect with a distinct return code, traces `theme_picker_close`, and routes
  the click through `mui_theme_picker_cancel` so the original theme is restored.
  The Windows harness now opens the picker and clicks the visible close button.
- **Language note:** no compiler gap surfaced. The recurring need is the same
  declarative widget/action model from L166-L168: modal headers, close buttons,
  traces, and cancel/apply semantics should be generated from one action source
  instead of hand-wired in parallel.

### L171. Picker header controls must remain active in submodes **[finding, P2]**
The Git branch switcher had a filter mode and a create-branch submode, but no
visible close button. Worse, the click path returned early while create mode was
active, so adding a header button without reworking that ordering would have
produced another dead control.

- **IDE note:** Branch Switcher now shares one geometry helper for draw and
  hit-test, draws a header close button, and returns a distinct close code before
  row/create-mode handling. Mighty routes that code to `mui_branch_cancel`, so
  filter mode and create mode both close from the same visible target.
- **Language note:** no compiler gap surfaced. The language-side pain is still
  lifecycle/routing duplication: modal submodes should not be able to bypass
  parent header actions because each overlay arm manually orders scalar checks.

### L172. Scrollable topology panels must not draw clipped rows **[finding, P2]**
The compact Agents gallery showed the Supervisors section cut off at the bottom
of the sidebar. The data was scrollable, but the paint loop drew until `y > h`,
which allowed a partial final row and gave no visual hint that more topology
continued below.

- **IDE note:** Agents now computes a complete visible-row budget from the
  sidebar height, draws only full rows, and paints a slim scrollbar thumb when
  additional topology rows are above or below the viewport.
- **Language note:** no compiler gap surfaced. Mighty-authored panels need a
  reusable scroll container primitive so row budgeting, clipping, scroll thumbs,
  and hit-test offsets stay synchronized across drawers.

### L173. Generated code in narrow chat panes needs continuation-aware wrapping **[finding, P2]**
The AI Copilot gallery showed a fenced code block wrapping at a bare `+`, then
continuing with a lone `n` at the left edge. That was technically wrapped, but
it read like broken code and made the right dock feel unfinished. The bottom
composer was also too subtle in the compact capture, so the panel read as a
static answer instead of an interactive chat surface.

- **IDE note:** AI code blocks now use a code-specific wrapper that prefers
  punctuation/operator breakpoints and indents continuation lines. The composer
  gets a stronger background, accent border, and top separator so the input area
  remains visible in compact captures.
- **Language note:** no compiler gap surfaced. Mighty UI needs a reusable rich
  text/code layout primitive for chat, markdown, and diagnostics so line wrapping
  can preserve semantic indentation instead of relying on plain word wrapping.

### L174. Mouse-driven QA must use the same input path as humans **[finding, P1]**
The Windows harness described itself as human-style testing, but its click and
drag helper posted mouse messages directly to the IDE window. That is useful for
isolating app event handlers, but it can bypass foreground focus, real cursor
position, DPI conversion, modal ownership, and OS chrome behaviour. Those are
exactly the classes of bugs that make an IDE feel broken even when unit tests
and offscreen captures pass.

- **IDE note:** the harness click and drag paths now try to foreground the real
  IDE window, move the OS cursor, and use `SendInput` for mouse down/up.
  Automated sessions that cannot take foreground ownership fall back explicitly,
  while `-StrictRealMouse` fails instead of hiding the gap. File buttons,
  command-palette row clicks, tab close targets, visible modal close buttons,
  and bottom-dock resizing can now be run in a true human-input mode from an
  interactive desktop.
- **Language note:** no compiler gap surfaced. Mighty needs a first-class
  interaction test fixture API so scenario tests can ask for semantic targets
  like `new-file-button` or `bottom-dock-resize-handle` instead of duplicating
  pixel geometry in PowerShell.

### L175. Brand marks should come from one reusable primitive **[finding, P2]**
The taskbar icon, rail logo, and Welcome logo had drifted into similar but not
identical treatments. The side-rail accent looked acceptable at large sizes but
became a strange colored stripe in compact rail and taskbar contexts.

- **IDE note:** the icon generator, activity rail, and Welcome screen now use the
  same centered Mighty mark direction: dark editor tile, nested accent frame,
  and a clear accent `M` without the old side stripe. The packaged executable was
  restamped and visually checked through the compact Welcome capture and icon
  extraction.
- **Language note:** no compiler gap surfaced. Mighty UI needs a reusable brand
  mark primitive or asset pipeline so app icon, rail chrome, and Welcome art do
  not have to be hand-kept in sync across Rust draw calls and Python icon code.

### L176. File command labels must match picker-backed behavior **[finding, P1]**
The file workflow had become functionally better than its wording: `Ctrl+N`
opened the native picker for a disk-backed file, but docs and palette labels
still implied an untitled scratch buffer in some places. That mismatch makes
New/Open/Save feel unreliable even when the underlying command dispatch works.

- **IDE note:** command labels now distinguish **File: New File at Location**,
  **File: New Untitled Tab**, and **Explorer: New File in Workspace**. README,
  keybinding docs, changelog, and palette tests were updated to match the actual
  native-dialog workflow.
- **Language note:** no compiler gap surfaced. Mighty needs command metadata
  generated from one source of truth so palette labels, keybinding docs, welcome
  actions, and tests do not drift apart as UX semantics change.

### L177. Empty side panels need actionable copy **[finding, P2]**
The Source Control panel showed a commit box and header icons, then only
`No changes` in the body. That was technically correct for a clean repo, but it
made the panel feel inert and gave no clue whether the user should fetch, pull,
edit a file, or open a different folder.

- **IDE note:** Source Control now distinguishes clean repos from non-git
  folders with short, actionable empty states: clean repos say the working tree
  is clean and suggest pull/fetch/edit, while non-git folders explain that a
  Git-backed folder is needed.
- **Language note:** no compiler gap surfaced. Mighty UI needs a reusable empty
  state pattern for side panels so "nothing here" surfaces can consistently
  provide a state label, short reason, and next action without bespoke copy in
  each drawer.

### L178. Startup must not mutate the workspace **[finding, P1]**
While testing the Source Control empty state against a temporary clean Git repo,
the IDE immediately showed an untracked `scratch.mty`. The no-arg startup path
was creating a path-backed scratch file in the current directory, so merely
opening the IDE dirtied a clean workspace.

- **IDE note:** no-arg startup now passes `None` into the initial context so the
  first tab is a virtual scratch tab. The Welcome screen still opens normally,
  typing still starts editing, and explicit Save/New File flows continue to use
  native pickers, but startup no longer writes `scratch.mty`.
- **Language note:** no compiler gap surfaced. Mighty needs clearer lifecycle
  semantics around virtual buffers versus workspace files, ideally encoded in
  the command/file APIs so "open app" cannot accidentally become "create file."

### L179. Native file commands need command-specific path semantics **[finding, P1]**
The UI had separate labels for "New File at Location" and "Explorer: New File
in Workspace", but both paths still flowed through the same workspace guard.
That made the native Save dialog feel broken when a user deliberately chose a
file outside the current project. Direct Save on an untitled tab also depended
on the Mighty command layer to route to Save As correctly.

- **IDE note:** `File: New File at Location` now creates/opens the exact path
  selected in the native picker, while the Explorer workspace prompt remains
  workspace-scoped. Plain Save on an untitled tab now opens the native Save As
  picker directly.
- **Language note:** no compiler gap surfaced. Mighty needs first-class command
  intent metadata for file workflows: workspace-relative creation, arbitrary
  path creation, and save-as are different contracts and should not be inferred
  from generic string-path plumbing.

### L180. Overlays need a shared visible-area contract **[finding, P1]**
The editor row math already reserved space for bottom docks, but the Welcome
surface was still given the full window height. With Terminal open, Welcome
quick actions rendered underneath the terminal panel, exactly the kind of
overlap a human sees immediately in a small window.

- **IDE note:** Welcome now receives the dock-adjusted height from the ABI and
  switches to a tighter, height-aware compact layout when the remaining editor
  area is short.
- **Language note:** no compiler gap surfaced. Mighty needs a shared layout
  primitive for "visible editor area after chrome and overlays" so every
  surface consumes the same bounds instead of each feature recomputing or
  forgetting bottom-dock reservations.

### L181. Similar commands need separate ABI contracts **[finding, P1]**
After separating "New File at Location" from "Explorer: New File in Workspace",
the Mighty event router still sent both the Explorer header action and the
workspace command through the arbitrary-location native picker. The labels were
right, but the command plumbing could still create a file outside the workspace
from an Explorer action.

- **IDE note:** Explorer/header new-file and the workspace palette command now
  call `mui_newfile_workspace_dialog`, a dedicated native picker path that
  starts at and enforces the workspace root. File > New keeps using the
  arbitrary-location picker.
- **Language note:** no compiler gap surfaced. Mighty needs command contracts
  that can be encoded and checked at dispatch boundaries, not just comments and
  labels, so visually similar commands cannot accidentally share the wrong
  filesystem semantics.

### L182. First-screen labels must preserve command intent **[finding, P2]**
The command palette had explicit file-action names, but Welcome still showed a
generic `New File...` row for the arbitrary-location picker. That made the first
screen less clear than the palette and weakened the distinction from Explorer's
workspace-scoped file creation.

- **IDE note:** Welcome now labels the primary action `New File at Location...`
  to match the native path picker it opens.
- **Language note:** no compiler gap surfaced. Mighty UI needs a shared command
  label source so Welcome, palette, shortcuts, and toolbar surfaces do not drift
  when command semantics are clarified.

### L183. Compact chrome must budget for the working surface **[finding, P2]**
The 520px stress gallery showed sidebar chrome taking too much fixed width from
the editor and lower docks. The UI was technically fitting, but human-visible
workflows like Terminal, Debug, Palette, and Welcome felt cramped because the
responsive sidebar floor was still too wide.

- **IDE note:** the compact sidebar floor is now 160px, with a regression test
  pinning the 520px body-left budget so small windows preserve more working
  surface.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a
  first-class responsive layout primitive so width budgets can be stated near
  component intent rather than mirrored through shim-side constants.

### L184. Modal footers should not carry cramped instructions **[finding, P3]**
The compact Settings screenshot showed the footer shortcut hint turning into
tiny edge copy. The text did not improve the core workflow and made the modal
feel less polished on small windows.

- **IDE note:** Settings now uses a quiet footer with only the panel tag, keeping
  the preference rows and controls as the visual focus.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable
  responsive footer patterns so helper copy can be omitted, moved, or collapsed
  consistently instead of being hard-coded per modal.

### L185. Header labels must reserve action-button space **[finding, P2]**
After compacting the sidebar, the Explorer header title could still run under
the New File/New Folder/Collapse buttons. The header was drawn as letter-spaced
text without measuring the right-side action strip.

- **IDE note:** Explorer headers now use measured head ellipsizing against the
  first action button, with a compact-sidebar regression test.
- **Language note:** no compiler gap surfaced. Mighty should eventually expose a
  reusable text-fit helper for common "label plus trailing actions" headers so
  these budgets are not hand-recreated in the renderer.

### L186. Split views need responsive ownership of chrome **[finding, P2]**
The 520px Markdown Preview gallery showed source and preview panes squeezed into
columns too narrow to read because the sidebar stayed open while the preview
split consumed the remaining body width.

- **IDE note:** Markdown Preview now temporarily hides the sidebar only when a
  compact split would fall below the readable pane width, and restores it when
  the preview closes.
- **Language note:** no compiler gap surfaced. Mighty needs a way to express
  responsive chrome ownership across features, so a view can request temporary
  space from surrounding UI without adding ad hoc flags per component.

### L187. Footer helper text needs an affordance budget **[finding, P3]**
The compact Keyboard Shortcuts modal showed its static footer legend fighting
the modal tag. The text was useful on wide cards, but on narrow cards it created
visual noise without improving the active remap flow.

- **IDE note:** Keyboard Shortcuts now hides the default helper legend below the
  clean-fit width while still showing capture and status feedback.
- **Language note:** no compiler gap surfaced. Mighty should expose a shared
  responsive footer/status primitive so optional helper copy can collapse by
  measured space instead of feature-specific thresholds.

### L188. Overlay notifications need chrome-aware safe areas **[finding, P2]**
The compact toast gallery showed notification cards over the Explorer/sidebar,
making underlying labels look like stale toast text. The toast stack knew about
bottom docks but not the left activity rail/sidebar.

- **IDE note:** Toast drawing and hit testing now accept a left reserve, shrink
  to fit the work area, and keep dismissal clicks aligned with the visible card.
- **Language note:** no compiler gap surfaced. Mighty should provide a common
  safe-area model for overlays so toasts, menus, and transient panels can avoid
  rail/sidebar/dock chrome from the same layout contract.

### L189. Cursor popups must clamp before drawing text **[finding, P2]**
The 520px language overlay captures showed signature help and code actions
anchored correctly but clipped at the right edge. The popup box was clamped only
after content chose its full width, so text still exceeded the visible work area.

- **IDE note:** Signature help and code-action menus now receive the editor
  safe-left inset, cap their box width before drawing, and ellipsize text by
  measured width. Code-action hit testing uses the same inset geometry.
- **Language note:** no compiler gap surfaced. Mighty needs a reusable overlay
  geometry primitive that combines anchor, safe area, width budget, and hit-test
  rect so every cursor popup follows the same contract.

### L190. Compact command labels should preserve intent **[finding, P3]**
The compact Testing toolbar shortened **Re-run** to **Re**, leaving a visible
button with an unclear command. The control still worked, but the human-visible
label looked broken.

- **IDE note:** Testing now maps compact run/re-run states to the action verb
  **Run**, while wide panels keep **Run Tests** and **Re-run**.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from a
  semantic command-label helper that chooses alternate labels per width instead
  of slicing display strings by hand.

### L191. Repeated header/action rows need a shared fit contract **[finding, P2]**
The Source Control compact gallery still showed **SOURCE CONTROL** crowding the
commit/pull/push/fetch icons after the Explorer header had already been fixed.
The underlying issue was the same pattern implemented twice: a tracked uppercase
label and a trailing action strip, but only one renderer measured the title
against the icon reserve.

- **IDE note:** SCM headers now use measured head ellipsizing with the first
  action icon as the right boundary, covered by a compact-sidebar regression.
- **Language note:** no compiler gap surfaced. Mighty UI needs a reusable
  header primitive for `label + trailing actions`, with shared measurement,
  clipping, and hit-test geometry so panels do not drift apart.

### L192. Secondary metadata must yield by priority, not just shrink **[finding, P2]**
After the SCM header fit, the section row still let branch metadata crowd the
`CHANGES` label/count in compact sidebars. Ellipsizing the branch was not enough
when the count itself needed the same horizontal lane.

- **IDE note:** Source Control now measures the `CHANGES` label and count first.
  Branch metadata draws only when the remaining budget can fit a useful branch
  cluster; otherwise it yields because branch state is already visible in the
  status bar.
- **Language note:** no compiler gap surfaced. Mighty UI needs priority-aware
  inline layout, where secondary metadata can collapse or move after primary
  task state claims its measured space.

### L193. Toolbars need responsive geometry shared by paint and hit testing **[finding, P2]**
The compact Debug gallery showed a header/title collision and the five-button
debug toolbar was wider than the compact sidebar. The click path used the same
fixed toolbar geometry, so simply clipping paint would have left invisible or
misaligned hit targets.

- **IDE note:** Run and Debug now measures the state pill before fitting the
  header title, and the toolbar geometry derives button/gap sizes from the
  current sidebar width. Drawing and click routing both read that geometry.
  Call-stack rows now measure the right-side `file:line` location first and fit
  the frame name against it.
- **Language note:** no compiler gap surfaced. Mighty UI needs first-class
  responsive toolbar layout so controls can shrink, wrap, or overflow-menu from
  one geometry contract instead of hard-coded per-panel coordinates.

### L194. Bottom input bands are overlays, not ordinary editor text **[finding, P1]**
The compact Replace capture showed Welcome/start-action text rendering over the
two-row replace bar. The bar filled an opaque band, but its text was queued on
the normal text layer while other body text was still present in the frame.

- **IDE note:** Prompt and Replace bands now switch the draw list and text queue
  into overlay mode while painting the band, close affordance, hints, and input
  text, then restore the previous overlay flag.
- **Language note:** no compiler gap surfaced. Mighty UI needs a declarative
  overlay owner/surface concept so transient input bands automatically paint and
  queue text above normal editor/welcome content.

### L195. Search inputs with trailing mode pills need reserved text budgets **[finding, P2]**
The compact Quick Open capture showed the placeholder helper running underneath
the active `FILES` pill. The input rendered its text before computing the pill,
so the placeholder had no right boundary.

- **IDE note:** Quick Open now computes the mode pill first, reserves a gap, and
  measured-ellipsizes the placeholder/query before drawing the caret.
- **Language note:** no compiler gap surfaced. Mighty UI needs an input primitive
  with prefix icons and trailing adornments so placeholder/query text budgets are
  derived from the same geometry as the visual pill.

### L196. Shared dock chrome needs a reserved header contract **[finding, P2]**
The compact Run and Terminal captures showed the shared bottom-dock resize strip
painting across the panel header, while the preset/close controls leaked into
the first content row because the header was only one text line tall.

- **IDE note:** Bottom-dock headers now reserve enough height for the shared
  24px preset/close controls, and the resize strip paints above the dock top
  instead of straddling the header text.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable dock
  primitives with explicit resize-handle, header-action, and content slots so
  z-order and hit-test contracts are declared once rather than rediscovered by
  every panel.

### L197. Compact status controls should avoid fragile glyphs **[finding, P2]**
The compact Run panel rendered its `exit 1 · 142ms` chip poorly: the decorative
separator glyph crowded the narrow pill and looked corrupted in the packaged
capture. The compact Web header also proved that text action buttons cannot be
forced into every width.

- **IDE note:** Run status chips now use short ASCII labels such as `exit 1`
  and measured fitting inside the chip. Web header geometry can fall back to a
  compact action control instead of allowing action text to collide with the
  `WEB` title lane.
- **Language note:** no compiler gap surfaced. Mighty UI should treat compact
  status/action chips as measured components with glyph-safe fallback labels or
  icon-only states, not arbitrary strings painted into fixed rectangles.

### L198. Toast animation must not trade away readability **[finding, P1]**
The compact Toast capture showed Welcome action text reading through stacked
toast cards, making toast copy look stale or uncleared even though the queue was
deduplicating messages correctly.

- **IDE note:** Toasts keep the slide animation but no longer fade visible cards
  down to translucent alpha; the card fill and text stay effectively opaque until
  expiration so underlying editor/welcome text cannot bleed through.
- **Language note:** no compiler gap surfaced. Mighty UI needs overlay/card
  primitives with readability floors for transient feedback, especially when
  animations run above busy content.

### L199. Overflowing tab strips need shared scroll geometry **[finding, P1]**
The tab bar used fixed-width tabs and mapped clicks with
`(x - body_left) / TAB_W`, which meant tabs beyond the visible top-row width had
no mouse-reachable target once many files were open.

- **IDE note:** The tab strip now owns a first-visible tab offset. Drawing,
  click-to-switch, close hit-testing, and wheel-over-tabs all read the same
  geometry, and tab commands clamp the active tab back into the visible strip.
  The screenshot gallery now includes a many-tab overflow case.
- **Language note:** no compiler gap surfaced, but the event ABI had to carry
  cursor coordinates on wheel events before a scroll could be routed to the tab
  strip instead of the editor. Mighty UI should model scrollable tab/list
  surfaces as first-class widgets with shared paint, hit-test, and wheel-routing
  state.

### L200. Rendered document panes need responsive type and spacing **[finding, P2]**
The compact Markdown preview screenshot showed the rendered heading and body
copy squeezed into a tiny prose column. The preview layout had even increased
side margins at narrow widths, which made split-pane reading worse.

- **IDE note:** Markdown preview now reduces side margins in compact columns and
  scales heading typography down before wrapping, preserving a useful reading
  measure in split panes.
- **Language note:** no compiler gap surfaced. Mighty UI needs document-preview
  primitives with responsive spacing and type tokens, so rendered prose panes do
  not depend on hand-tuned constants copied across tools.

### L201. Real-mouse UX tests must wait for UI evidence **[finding, P1]**
The strict Windows harness initially reported many false feature failures:
relative trace paths hid evidence, high-DPI scaling shifted clicks, synthetic
button events only moved the cursor, and command text could be typed before the
topbar click had opened the palette.

- **IDE note:** The harness now normalizes its artifact paths, reads the app's
  startup scale trace, uses reliable real mouse button events, targets the
  visible Welcome and tab controls, and waits for the topbar palette trace before
  typing queries. The strict mouse run now covers file dialogs, tab switching and
  closing, drawer resizing, modal closes, Markdown preview close, and workspace
  folder selection.
- **Language note:** no compiler gap surfaced. Mighty UI and its harnesses need
  first-class observable interaction checkpoints so black-box tests wait for the
  UI state a human sees, not just a fixed sleep after input.

### L202. Text editors need binary/read-only file contracts **[finding, P1]**
The IDE could open icons, fonts, and other binary assets as corrupt editable
text. That was visually broken and risked saving a text preview over real binary
bytes if a user hit Save after experimenting.

- **IDE note:** Tabs now detect likely binary content, preserve the original raw
  bytes, render a clear read-only preview, suppress dirty state for those tabs,
  and reject Save, Save As, Save All, and autosave from the text editor path.
- **Language note:** no compiler gap surfaced. Mighty UI needs a first-class
  document capability model so file-backed views can advertise editability,
  saveability, and preview-only state to commands and chrome from one source of
  truth.

### L203. Overlay layers need declared exclusivity, not just z-order **[finding, P1]**
The Settings modal could open while a recent operation toast was still visible,
leaving the toast card painted over the modal footer. The command succeeded, but
the user-visible result looked like stale notification text was stuck on top of
the active dialog.

- **IDE note:** Toast drawing and toast click hit-testing now suppress while
  blocking modal overlays are active: Settings, Keyboard Shortcuts, Theme Picker,
  and dirty-work confirmation. The real-mouse harness also waits for modal-open
  traces before taking screenshots, so named modal captures show the actual
  surface rather than the command palette that launched it.
- **Language note:** no compiler gap surfaced. Mighty UI needs a first-class
  overlay manager with priority/exclusivity semantics so transient feedback,
  modals, panels, and popovers do not each hand-roll draw order and hit-test
  suppression.

### L204. Text file decoding must normalize UTF-8 BOMs at load boundaries **[finding, P1]**
The real-mouse Markdown preview capture showed a stray first glyph before the
opened file's text. The repro was a normal Windows-created UTF-8 file with a BOM;
the editor loaded the BOM into the first line, so the source pane and Markdown
preview both rendered it as visible document content.

- **IDE note:** `TextModel::from_bytes` now strips a leading UTF-8 BOM before
  splitting lines. This fixes opened tabs, reloads, code-action reloads, and live
  Markdown preview source because they all flow through the same text-model
  boundary.
- **Language note:** no compiler gap surfaced. Mighty's standard/file APIs should
  eventually make text-vs-byte reads explicit and provide a BOM-normalizing text
  read helper so tools do not re-learn Windows encoding edge cases.

### L205. Empty custom chrome should be purposeful or visibly inert **[finding, P1]**
The top titlebar had a wide blank region after the tabs. It was technically a
drag strip, but visually it looked like an empty broken control beside the Run
and More buttons, which matches the user's complaint that some chrome feels
weird or non-functional.

- **IDE note:** the tab bar now draws a command-center Quick Open pill in that
  empty region when there is enough room. The same geometry controls drawing and
  hit-testing, and the shim lets clicks on the pill pass through before starting
  a window drag.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from
  declarative chrome regions that can specify drag, command, resize, and visual
  affordance roles together instead of splitting those decisions across shim and
  Mighty event ladders.

### L206. Command surfaces are blocking overlays for transient feedback **[finding, P1]**
The command palette could stay open while a recent Save/Open toast painted over
its lower-right footer. Functionally the command surface still worked, but the
visual result looked like stale notification text was stuck inside the overlay.

- **IDE note:** toast drawing and toast click hit-testing now suppress while
  Command Palette, Quick Open, breadcrumb dropdowns, branch picker, settings,
  shortcuts, theme picker, and dirty confirmation are active. The real-mouse
  harness still captures these surfaces, so future overlap shows up in evidence.
- **Language note:** no compiler gap surfaced. Mighty UI needs a single overlay
  coordinator with priority, modality, and transient-feedback policy so command
  surfaces can declare "no toast chrome over me" instead of each surface relying
  on ad hoc draw order.

### L207. Compact chrome needs minimum usable widths, not proportional math only **[finding, P1]**
The 520px compact gallery showed rail drawers that were technically responsive
but not usable: agents/test labels were clipped too aggressively and panel
controls crowded the editor boundary, even though the main editor still had
room to yield a little width.

- **IDE note:** the responsive sidebar now keeps a larger compact floor and uses
  a more generous auto fraction before clamping to the full-size width. This
  keeps drawer controls, status chips, and file/action labels inside the panel
  at compact sizes while preserving a readable editor body.
- **Language note:** no compiler gap surfaced. Mighty UI needs declarative
  layout constraints for minimum usable widths per surface, so drawers can
  express "I need 176px for controls" instead of relying on one global
  proportional sidebar formula.

### L208. Packaged app startup must not inherit installer-directory workspace **[finding, P1]**
A no-argument packaged launch opened the Explorer tree on the distribution
folder, exposing `mighty-ide.exe`, DLLs, shortcut scripts, and package support
files as if they were the user's project. The Welcome screen had useful actions,
but the adjacent Explorer made the IDE look like it had opened its own install
directory.

- **IDE note:** startup root selection is now explicit and tested: an argument
  file still roots Explorer at that file's parent; packaged no-arg launches with
  bundled `samples/` root Explorer at `samples/`; development no-arg launches
  still use the current directory.
- **Language note:** no compiler gap surfaced. Mighty apps need an application
  context API that distinguishes executable directory, current directory,
  bundled sample/resources directory, and user workspace root as separate roles.

### L209. Docked panels must not put controls inside titlebar hit zones **[finding, P1]**
The AI Copilot panel drew its visible close button inside the custom titlebar
band. The titlebar command-center hit-test correctly owned that band, so a human
click on the AI close button opened Quick Open instead of closing the panel.

- **IDE note:** the AI panel now docks below the titlebar, its close geometry
  moved with the visible header, and the strict real-mouse harness waits for
  `ai_open` before clicking the close affordance. The mouse-close and
  command-close paths are both covered.
- **Language note:** no compiler gap surfaced. Mighty UI needs declared hit-test
  ownership for titlebar, docked panels, overlays, and command surfaces so
  visible controls cannot be drawn inside another surface's interaction zone.

### L210. First-run language IDEs need project creation on the primary surface **[finding, P1]**
The IDE already had a `Mighty: New Project` command, but it lived behind the
command palette. On first launch, the Welcome screen exposed file and folder
flows while leaving the language-specific project workflow hidden, which makes
the IDE feel less complete than the command registry actually is.

- **IDE note:** Welcome now includes **New Mighty Project...** as a Start action
  and routes the click through the existing Mighty prompt path that calls
  `mty new`, opens the created project as the workspace, and reports the result
  through toasts.
- **Language note:** no compiler gap surfaced. Mighty needs a higher-level
  command/action declaration model so a command can define palette metadata,
  Welcome placement, click routing, prompt kind, and tests in one place instead
  of duplicating numeric action ids across Rust and Mighty.

### L211. File tabs need filename-aware truncation, not path-style tail fitting **[finding, P2]**
The real-mouse screenshots showed active tab labels like `...swelcome.mty`.
That tail-preserving truncation is useful for long paths, but it makes file tabs
feel broken because the meaningful start of the basename disappears.

- **IDE note:** tab labels now use filename-aware middle truncation: when a
  basename is too wide, the tab keeps the beginning of the name and the file
  extension visible while preserving the close affordance hit target.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable text
  fitting policies such as path-tail, filename-middle, and command-head so each
  surface can declare what part of the string is semantically important instead
  of sharing one generic ellipsis helper.

### L212. Command registries should separate action names from UI fitting **[finding, P2]**
The Command Palette and Keyboard Shortcuts overlays showed file commands with
baked `...` suffixes, which looked like accidental truncation even when the row
had enough space. Long labels also did not reserve space for right-side shortcut
pills before drawing.

- **IDE note:** dialog-style file commands now use clean action names while
  descriptions explain the native picker behavior. Palette and shortcuts rows
  fit command names and descriptions against the actual shortcut/remap chrome so
  text cannot run underneath controls.
- **Language note:** no compiler gap surfaced. Mighty UI needs command metadata
  that distinguishes action name, menu/dialog convention, short label,
  description, and measured row layout policy instead of overloading one string
  for every surface.

### L213. Toast lifetime should match message severity **[finding, P2]**
Real-mouse screenshots still showed a success toast after the user had already
navigated into another panel. The message was technically transient, but it read
as stale feedback because quick completion notices lived as long as more
important warnings.

- **IDE note:** toast lifetime is now severity-aware: success feedback clears
  quickly, info toasts stay modestly longer, and warnings/errors remain visible
  long enough to read. The toast tests cover the faster success expiry and the
  longer error lifetime.
- **Language note:** no compiler gap surfaced. Mighty UI needs transient
  feedback policies that can encode severity, user action, and surface context
  instead of relying on one global toast timeout.

### L214. Sidebar resizing needs direct manipulation, not presets only **[finding, P1]**
Manual resizing still felt clunky because the left drawer had palette presets
but no obvious divider the user could drag. A best-in-class IDE should let a
human put the Explorer/Search/SCM width exactly where their project needs it.

- **IDE note:** the sidebar now has a visible right-edge divider, east-west
  cursor feedback, hand-resized width persistence, clamp rules that preserve a
  usable editor body, and strict real-mouse harness coverage for divider drags.
- **Language note:** no new compiler gap surfaced. This repeats the lower-dock
  pattern: Mighty can own the high-level event loop, but shim-owned layout needs
  explicit drag state so normally-consumed mouse-move events can be delivered to
  direct-manipulation controls.

### L215. Strict mouse harnesses need restore-aware foreground and command traces **[finding, P2]**
The sidebar-resize verification exposed a later cascade where the harness lost
foreground/window state and kept clicking stale coordinates. The app behavior
under test was already complete, but the verifier lacked enough recovery and
surface-open tracing to separate an app failure from test-driver drift.

- **IDE note:** the Windows real-mouse harness now restores the IDE window before
  every strict click/drag attempt, and the Problems panel emits a `problems_open`
  trace with count/severity totals for command-path assertions.
- **Language note:** no compiler gap surfaced. This reinforces that Mighty apps
  need explicit, cheap trace/event markers for state transitions that are visible
  to users but otherwise hard for external drivers to prove.

### L216. Shell-backed panels need native fast-fail guards before process IO **[finding, P1]**
Strict real-mouse testing still showed Source Control briefly unresponsive when
opened on the packaged sample workspace. The visible issue was a rail button
that appeared not to work, but the root cause was `git rev-parse` being launched
from a plain folder where a quick ancestor `.git` check could answer immediately.

- **IDE note:** SCM repo discovery now walks ancestors for a `.git` file or
  directory before spawning Git, and Source Control view-switch commands no
  longer run `git status` directly from the mouse/key command path. The panel
  opens immediately, then explicit refresh/save/git mutation paths can update
  status.
- **Language note:** no compiler gap surfaced. Mighty can orchestrate the panel
  flow, but any shim feature that shells out should expose a cheap synchronous
  preflight or an asynchronous path before it is wired directly to mouse input.

### L217. Mouse-opened overlays must swallow their opener event tail **[finding, P1]**
The real-mouse harness showed More opening the Palette and then immediately
closing it from a follow-up mouse event at the same coordinate. The visible
failure looked like Open/Save/Open Folder commands typing into the editor, even
though the command surface had briefly opened.

- **IDE note:** Palette and Quick Open now keep a one-click ignore latch when
  opened from mouse UI, so the opener click cannot be interpreted as an outside
  overlay click. Outline view switches also avoid synchronous refresh work on the
  rail click path.
- **Language note:** no compiler gap surfaced, but Mighty’s scalar event loop
  makes this kind of modal-open handoff easy to miss. Mouse-opened overlays need
  an explicit "opened by this click" latch when the runtime may deliver move/down
  events around the same coordinates.

### L218. Secondary affordances need measured gutters, not leftover pixels **[finding, P2]**
Strict screenshot review showed Command Palette and Keyboard Shortcuts rows
still feeling crowded even after title truncation was added. The failure mode was
not raw overflow: keybinding chips and selected-row action text technically fit,
but the row read as overlapping because the text budget ended too close to the
right-side chrome.

- **IDE note:** Command Palette rows now reserve a wider gutter before shortcut
  chips, and the Keyboard Shortcuts modal compacts "Enter to remap" to "Enter"
  when the selected row has a tight keybinding/action gutter.
- **Language note:** no compiler gap surfaced. Mighty can keep driving these
  overlays, but shim-rendered command surfaces need explicit measured budgets for
  secondary affordances instead of assuming the remaining pixels will look clean.

### L219. Global chrome clicks need an early non-modal routing lane **[finding, P1]**
Strict real-mouse testing caught an Open File -> edit -> Save flow where the
More button logged a top-bar hit, but the typed command query landed in the
editor. The visible behavior was bad: a user clicked command chrome, then saw
`save` inserted into the document.

- **IDE note:** top-bar More and command-center clicks now have an early
  mouse-down lane whenever no modal overlay owns the screen. That lane opens
  Palette or Quick Open before editor completion, bottom-dock focus, or other
  non-modal states can consume subsequent text input.
- **Language note:** no new compiler bug surfaced, but the flat event ladder is
  fragile when global chrome, modal overlays, and editor focus all share scalar
  state. Mighty would benefit from a small first-class event-priority pattern or
  reusable helper that can own these "global chrome before local focus" checks
  without adding broad parse-stack pressure.

### L220. Compact sidebar help text should wrap before it truncates **[finding, P2]**
Strict screenshot review showed the Testing panel empty state cutting off
"Run the package's tests to see results." even though the panel had enough
vertical space for two readable lines. This is a small bug, but it makes the UI
feel less deliberate.

- **IDE note:** the Testing panel now uses measured two-line wrapping for its
  no-results guidance, matching the existing failed-test detail wrapping instead
  of forcing every help string through a single-line ellipsis path.
- **Language note:** no compiler gap surfaced. Mighty can keep delegating panel
  rendering to the shim, but compact sidebar copy needs a reusable measured-wrap
  primitive so helper text does not become fake-fitted one-liners.

### L221. UX verifiers need state traces for command surfaces **[finding, P1]**
The strict mouse harness intermittently failed the Open File -> edit -> Save
workflow because the driver could prove a top-bar More click happened, but could
not prove the command palette was ready before it started typing. When the app
was slow for a frame, the trace only showed later command text reaching a stale
surface, which made the failure hard to diagnose.

- **IDE note:** Palette open, query update, selected-id, and cancel operations
  now emit trace markers, and the Windows strict-mouse harness waits for a fresh
  `palette_open` marker before typing command queries.
- **Language note:** no compiler gap surfaced. Mighty-driven apps need cheap
  state-transition traces for modal/command surfaces so real-input harnesses can
  synchronize on product state instead of sleep timing.

### L222. Automation hooks need isolated persistence, not only deterministic input **[finding, P1]**
Strict mouse runs used deterministic picker env vars, but still wrote Open
Folder results into the user's real recent-workspaces file. If a run timed out
before cleanup, temporary harness workspaces could show up on the real Welcome
screen.

- **IDE note:** the config layer now honors `MUI_CONFIG_DIR`, and the Windows
  strict-mouse harness points it at the run output directory. Harness recents,
  keybindings, and related config artifacts stay with the test output instead of
  leaking into `%APPDATA%/mighty-ide`.
- **Language note:** no compiler gap surfaced. Mighty apps that expose env-based
  automation should also expose persistence scoping, because deterministic input
  without deterministic storage still changes the human user's state.

### L223. Strict automation should consume the app's geometry contract, not folklore coordinates **[finding, P2]**
The AI panel close button avoids the topbar action strip, so its visual target
depends on the DPI-scaled logical window width. A fixed harness click worked at
one scale and missed at another.

- **IDE note:** the Windows strict-mouse harness now derives the AI close target
  from the same logical titlebar constants used by the shim.
- **Language note:** no compiler gap surfaced. A future Mighty UI layer should
  make testable layout rects first-class so user-facing widgets, hit tests, and
  harness clicks can share one source of truth.

### L224. Placeholder and caret geometry must be explicit in focused inputs **[finding, P2]**
The Keyboard Shortcuts overlay drew the empty search-field caret at the same X
as the placeholder text. The field functioned, but the first visual state looked
sloppy and undermined confidence in the command surface.

- **IDE note:** the shortcuts renderer now separates placeholder text from the
  insertion caret while preserving typed-query alignment.
- **Language note:** no compiler gap surfaced. Mighty UI primitives should make
  focused-empty input geometry reusable so every search box and picker field gets
  consistent placeholder/caret spacing.

### L225. Command surfaces need shared input-field primitives **[finding, P2]**
After fixing the Keyboard Shortcuts overlay, the main command palette still had
the same empty placeholder/caret collision. Similar hand-rolled field geometry
across command surfaces makes the UI drift one surface at a time.

- **IDE note:** the command palette now uses the same empty placeholder inset as
  the shortcuts overlay while preserving typed-query caret alignment.
- **Language note:** no compiler gap surfaced. The repeated fix points toward a
  Mighty UI primitive for focused search fields rather than per-surface pixel
  math.

### L226. Text fitting needs intent: paths keep tails, instructions keep heads **[finding, P2]**
The Source Control empty-state hint used path-style tail fitting, so the narrow
sidebar showed an ellipsis plus the end of the sentence and hid the action verb.
That made the panel look broken even though the command existed.

- **IDE note:** SCM instructional hints now use shorter action copy with
  head-fitting fallback, preserving the useful instruction at narrow widths.
- **Language note:** no compiler gap surfaced. Mighty UI helpers should separate
  path/file fitting from instructional-copy fitting so intent is encoded at the
  call site.

### L227. Visible primary actions need a workspace fallback before warning **[finding, P1]**
The Testing panel showed a prominent **Run Tests** button, but if the active tab
was an untitled scratch buffer the action had no file path and returned without
starting anything. To a user this reads as a broken button, especially when a
workspace is visibly open and contains Mighty files.

- **IDE note:** `mui_test_run` now falls back to the open workspace's
  `mighty.toml` or first `.mty` file before warning that no test target exists.
  The Windows strict-mouse harness opens the Testing rail and clicks the visible
  Run button, requiring a `test_run start` trace so this failure mode cannot hide
  behind a screenshot-only pass.
- **Language note:** no compiler gap surfaced. Mighty apps need reusable product
  rules for target resolution: active document first, workspace context second,
  then a clear warning. Scalar ABI remains sufficient for the fix.

### L228. In-workflow pickers should not masquerade as landing pages **[finding, P1]**
`File: Open Recent` reused the Welcome landing to show recent files and folders.
The command worked, but visually it looked like the IDE had navigated back to a
first-run screen instead of opening a chooser for the current workflow.

- **IDE note:** Open Recent now forces a focused recent picker surface that uses
  the existing recent file/folder hit targets but draws a compact chooser over
  the editor body. The strict mouse harness opens the command from the palette
  and clicks a recent workspace row, requiring both picker-open and row-dispatch
  traces.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from a
  reusable modal/picker surface abstraction so commands can share hit-tested row
  behavior without sharing the wrong visual frame.

### L229. Compact summaries should wrap words before abbreviating meaning **[finding, P2]**
The Testing sidebar summary used `16p • 0f • 16t` once the drawer became narrow.
That saved pixels, but it read like developer shorthand and made an otherwise
functional panel feel unfinished.

- **IDE note:** compact Testing summaries now keep semantic labels, wrapping to
  two lines (`passed/failed`, then `total`) and reserving the matching vertical
  space before the Results tree begins.
- **Language note:** no compiler gap surfaced. Mighty UI needs summary/chip
  primitives that can choose between single-line, wrapped, and elided layouts
  without losing the user-facing meaning of the text.

### L230. Dismissible feedback needs traceable hit tests **[finding, P1]**
Toast stale-text complaints are hard to close with screenshots alone: a toast
can look visible in one capture, expire in the next frame, or be hidden under an
overlay. The product already supports click-dismiss and clear-all, but the
strict mouse harness could not prove those paths worked.

- **IDE note:** toast click-dismiss and clear-all now emit trace markers, and
  the Windows strict-mouse harness dismisses a visible Save As toast by mouse
  before invoking **Notifications: Clear All Toasts** against a later save toast.
- **Language note:** no compiler gap surfaced. Mighty UI should treat overlay
  hit tests as observable state transitions so automation can verify human
  interactions instead of inferring them from transient pixels.

### L231. Tool panels need human-click workflow proofs, not just screenshots **[finding, P1]**
The Search rail looked like a real tool, but the strict harness only opened the
panel and captured pixels. That misses the failure users actually feel: typing a
query, clicking the visible action button, and clicking a result row.

- **IDE note:** project Search now traces run, replace-all, and result-open
  actions. The header refresh icon also runs the active query. The Windows
  strict-mouse harness types a deterministic query, clicks both visible run
  affordances, requires one result, then clicks that result and requires the
  matching file-open trace.
- **Language note:** no compiler gap surfaced. Mighty needs a small, reusable
  panel-workflow test vocabulary so scalar UI surfaces can describe "field",
  "primary action", and "result row" interactions without every feature
  re-creating the same harness math.

### L232. Toolbar icons must share command semantics with shortcuts **[finding, P1]**
The Debug panel's Play icon looked like the F5 command, but in idle state it only
called "continue", so a user click could appear broken while the keyboard path
worked. Icon buttons need the same command semantics as the shortcut and palette
entry they visually represent.

- **IDE note:** Debug Play now uses the same start/continue path as F5, disabled
  step/stop actions provide feedback instead of silent no-ops, and the strict
  mouse harness clicks the visible Play and Stop toolbar buttons while requiring
  dispatch traces.
- **Language note:** no compiler gap surfaced. Mighty needs reusable command
  routing helpers so toolbar, keyboard, palette, and rail interactions cannot
  drift into similar-looking but different behavior.

### L233. Refresh affordances must not hide network actions **[finding, P1]**
The Source Control empty state told users to refresh local Git status, and the
header icon was a refresh glyph, but the click path invoked Git Fetch. That makes
the most obvious local scan control do a surprising network action.

- **IDE note:** the SCM header refresh icon now calls local status refresh,
  emits an `scm_refresh` trace, and the strict mouse harness clicks the visible
  icon while requiring a rescan trace. Fetch remains available through palette
  Git commands.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from a
  declarative action model where icon, label, tooltip, keyboard command, and
  dispatcher target are defined once instead of inferred across separate paths.

### L234. Window chrome needs visual affordance primitives **[finding, P2]**
The top-right Run/More strip remained functionally clickable, but its solid rail
backing made it read like a dead tab-shaped block next to the native window
controls. That is a UX problem even when hit tests pass: users scan the title bar
before deciding what is interactive.

- **IDE note:** the late window-control pass now paints the action strip on the
  tab-bar surface and gives Run/More compact button affordances while preserving
  the existing click geometry and overlay-bleed protection.
- **Language note:** no compiler gap surfaced. Mighty needs reusable visual
  affordance primitives for icon buttons in chrome/toolbars so every surface does
  not hand-roll fill/stroke/icon spacing in Rust-side draw code.

### L235. Brand assets need size-aware generation **[finding, P2]**
The same richly framed icon art was being rendered at 16px, 32px, 48px, and
256px. That is convenient, but the smallest Windows shell sizes need a bolder
silhouette and fewer nested strokes than the large preview.

- **IDE note:** `tools/make-icon.py` now renders compact 16px/32px variants with
  one crisp outline and a larger Mighty mark, while preserving the richer 48px
  and 256px tiles. The generator also writes `dist/icon-sizes-preview.png` so
  small-size icon regressions can be checked visually.
- **Language note:** no compiler gap surfaced. Mighty does not currently own
  raster asset generation; a future build pipeline could expose asset-generation
  tasks declaratively instead of relying on ad hoc Python tooling.

### L236. Brand marks should use one visual system across chrome **[finding, P2]**
The taskbar icon, rail logo, and Welcome logo were all variations of the same
Mighty mark, but the in-app versions still used nested strokes that became fuzzy
in compact UI. A product mark should be boringly consistent across shell chrome,
navigation chrome, and first-run surfaces.

- **IDE note:** the activity rail and Welcome brand tiles now use the same
  single-outline treatment and larger filled M as the sharpened Windows icon,
  while preserving their existing layout and hit targets.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from a
  shared brand/icon component primitive so the same mark geometry and spacing
  can be reused without copying draw code between rail, welcome, status, and
  packaging assets.

### L237. Crowded tab strips need live pointer proof **[finding, P1]**
Tab overflow had unit coverage, but the strict Windows harness only clicked
ordinary visible tabs. That leaves a real UX gap: users need hidden tabs to be
reachable with the same mouse wheel gesture they use in other IDEs, and a
borderless title bar must not steal that input.

- **IDE note:** tab-strip scrolling now emits `tab_scroll` traces, and the
  strict Windows harness creates a crowded tab strip, sends a real OS mouse-wheel
  event over the tab row, and requires the visible tab window to move.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable
  black-box interaction fixtures for pointer workflows so overflow, drag, and
  wheel behavior are verified against live windows instead of inferred from
  scalar hit-test units.

### L238. Borderless resize must be proven at the OS level **[finding, P1]**
Hit-test units can prove the bottom-right corner maps to a resize direction, but
they cannot prove the live winit host starts an OS resize loop or that a human
drag actually changes the window. That gap matters because manual resizing was
called out as clunky.

- **IDE note:** window resize now emits a `window_resize` trace, and the strict
  Windows harness drags the bottom-right corner with real mouse input, verifies
  the window rectangle changed, then restores the original test size.
- **Language note:** no compiler gap surfaced. Mighty UI still needs a stronger
  end-to-end test vocabulary for host-window interactions such as move, resize,
  minimize, maximize, and focus restoration.

### L239. Native overlays need contrast tokens and screenshot-grade assertions **[finding, P1]**
The command palette, Settings, and Keyboard Shortcuts were functionally correct
but still read as sloppy because helper text used passive chrome colors that
were too faint on dark elevated cards. Unit tests could pass while a human saw
low-contrast descriptions and footer hints.

- **IDE note:** overlay-specific muted/subtle colors now separate workflow copy
  from passive editor chrome, and theme tests assert that dark-theme overlay
  helper text is brighter than passive tertiary chrome.
- **Language note:** no compiler gap surfaced. Mighty UI would benefit from
  first-class design-token primitives and screenshot assertions for contrast,
  text bounds, and overlay hierarchy so visual quality can be tested in the
  language-facing app layer, not only in the Rust shim.

### L240. Secondary workflow metadata cannot reuse "faint" chrome colors **[finding, P2]**
Testing result suite names and run duration were not overlapping, but they were
so dim that the drawer looked unfinished. The distinction matters: passive
decorative chrome can be faint; workflow metadata still has to be readable.

- **IDE note:** Testing drawer suite labels and duration now use secondary text
  rather than the faint `TEXT_4` color.
- **Language note:** no compiler gap surfaced. A Mighty-native UI toolkit should
  separate semantic roles such as `metadata`, `disabled`, `placeholder`, and
  `decorative` instead of forcing app code to infer meaning from generic color
  constants.

### L241. Completion metadata must not render fake signature fragments **[finding, P2]**
The autocomplete popup looked broken because semantic candidates drew a tiny
inline placeholder signature while the footer already contained the selected
candidate's full signature. A human sees that as sloppy even if the completion
accept path works.

- **IDE note:** semantic completion rows now use a readable `function` kind
  label, and row-level signature fragments render only when they are real
  ASCII-safe detail. The selected-row footer remains the place for full
  parameter context.
- **Language note:** no compiler gap surfaced. Mighty UI needs richer
  completion item metadata from `mty-lsp` so label, kind, detail, and
  documentation are separate fields instead of heuristic strings.

### L242. Destructive workflow controls need honest visual state **[finding, P1]**
Project Search replace was functional, but the replace input and check button
looked disabled because they reused faint tertiary styling. That is especially
bad for destructive or project-wide operations: users need to know whether the
button is available before they trust the workflow.

- **IDE note:** the replace placeholder now uses readable secondary text, and
  the replace-all check button switches to an accented active state when the
  search query is non-empty.
- **Language note:** no compiler gap surfaced. Mighty UI should expose semantic
  button states such as enabled, disabled, pending, and destructive/confirming
  as first-class styling primitives instead of encoding state through ad hoc
  color choices.

### L243. Selected-state affordances must use semantic icons **[finding, P2]**
The Color Theme picker highlighted the active row but used a plus icon at the
right edge. That reads as "add this theme" rather than "this theme is selected",
which makes an otherwise functional picker feel unfinished.

- **IDE note:** selected theme rows now draw a checked accent capsule, and the
  regression test asserts the selected icon cannot drift back to the plus icon.
- **Language note:** no compiler gap surfaced. Mighty UI needs semantic icon
  roles for selected/current, add/create, close, destructive, and disabled
  states so app code does not hand-pick ambiguous symbols at every call site.

### L244. Shortcut labels need structured chord tokens **[finding, P2]**
The Keyboard Shortcuts overlay rendered keybinding pills from casual string
splitting. That makes slash-separated alternatives such as `Alt+Up / Alt+Down`
look like one impossible chord, while slash-key chords such as `Ctrl+/` need to
stay intact.

- **IDE note:** shortcut display now tokenizes alternatives separately from key
  separators, drawing slash-separated bindings as distinct groups without
  breaking real slash-key chords.
- **Language note:** no compiler gap surfaced. Mighty UI should expose a
  structured shortcut/chord model for views and commands so renderers receive
  tokens like `modifier`, `key`, and `alternative` instead of reparsing display
  strings at draw time.

### L245. Empty input placeholders need reserved caret space **[finding, P2]**
Quick Open fitted its long placeholder before the mode pill, but the empty-query
caret still started at the same x-position as the placeholder. Humans read that
as overlapping text even though typed queries work.

- **IDE note:** Quick Open now insets empty placeholder copy away from the caret
  and uses overlay-specific muted text, matching the command palette and
  shortcuts overlay spacing contract.
- **Language note:** no compiler gap surfaced. Mighty UI should expose a
  reusable text-field primitive that owns placeholder, caret, and measured-fit
  geometry together instead of requiring every overlay to duplicate those rules.

### L246. Modal surfaces need consistent close affordances **[finding, P2]**
The focused Open Recent picker could be dismissed indirectly, but it lacked the
visible top-right close button used by Settings, Keyboard Shortcuts, Theme
Picker, and Markdown Preview. That inconsistency makes the dialog feel unfinished
even when row selection works.

- **IDE note:** Open Recent now draws a standard close button and routes its
  click through the Welcome action id path to dismiss the forced picker.
- **Language note:** no compiler gap surfaced. Mighty UI needs modal primitives
  with standard title, close, escape, focus, and hit-test behavior so every
  overlay does not recreate dismissal chrome by hand.

### L247. Settings must show effective availability, not only preferences **[finding, P2]**
Inline AI persisted as enabled by default, but without an API key the ghost
engine is effectively disabled. Showing a purple enabled toggle in Settings made
the app look broken when the feature correctly refused to run.

- **IDE note:** the Inline AI row now displays as unavailable/off when neither
  `ANTHROPIC_API_KEY` nor `CLAUDE_API_KEY` is configured, while preserving the
  stored preference so it can activate once a key exists.
- **Language note:** no compiler gap surfaced. Mighty UI needs settings controls
  that separate stored preference, runtime availability, effective state, and
  explanatory copy so feature gates are honest in the UI.

### L248. Visible modal affordances need mouse-trace coverage **[finding, P2]**
Adding a close button is not enough if the harness only verifies the happy-path
row selection. A human judges whether the dialog can be opened, dismissed, and
reopened without visual leftovers or broken focus.

- **IDE note:** Open Recent now has a strict real-mouse harness leg that opens
  the picker, clicks the visible close button, verifies `welcome_dismiss`,
  reopens it, and then clicks a recent workspace row.
- **Language note:** no compiler gap surfaced. Mighty needs reusable modal test
  primitives so close, escape, focus restore, and row activation can be verified
  consistently without hard-coded harness coordinates for each surface.

### L249. No-op feature gates must not render enabled controls **[finding, P2]**
The AI Copilot body correctly said an API key was required, but the input still
said `Enter to send` and the send glyph stayed accent-colored. That made the
no-key `send()` no-op look like a broken button rather than an unavailable
feature.

- **IDE note:** AI Copilot now mutes the no-key input border, replaces the
  empty-input placeholder with setup copy, disables the send affordance until a
  key and non-empty prompt are present, and captures the panel in the strict
  Windows harness.
- **Language note:** no compiler gap surfaced. Mighty UI needs a first-class
  enabled/available control state so labels, borders, icons, keyboard hints, and
  click routing are derived from one semantic gate.

### L250. File-flow tests must prove the dialog path, not just the result **[finding, P1]**
Explorer New File was already creating the selected file, but the harness only
checked filesystem state. That was too weak for the user's complaint: a broken
in-app prompt, default filename, or stale command path could still create a file
and pass.

- **IDE note:** new-file dialog ABIs now emit cancel, unavailable, and picked-path
  traces. The strict Windows harness requires the visible Explorer New File click
  to produce `new_workspace_file_dialog path=...` for the exact created file.
- **Language note:** no compiler gap surfaced. Mighty still needs structured
  command-result values so dialog success/cancel/unavailable evidence is typed
  instead of emitted as magic return codes plus trace strings.

### L251. Window resize needs a visible affordance, not only hit testing **[finding, P2]**
The borderless window could resize from the bottom-right corner, but screenshots
left the actual target nearly invisible inside the status-bar chrome. Humans
should not have to guess where the OS resize band starts.

- **IDE note:** the status bar now reserves the bottom-right corner for a visible
  diagonal resize grip, moves the notification bell left of that target, pins
  the grip geometry in a unit test, and captures the pre-drag frame in the
  strict Windows harness before performing the real mouse resize.
- **Language note:** no compiler gap surfaced. Mighty UI needs host-window chrome
  primitives for resize, move, minimize, maximize, restored/maximized state, and
  visible affordance drawing so every app does not hand-roll borderless window
  behavior.

### L252. Save workflows need multi-dialog automation evidence **[finding, P1]**
Save As and Save All can both need native SaveFileDialog paths during one human
session. A single deterministic picker path let the harness prove one command,
but not that Save All consumed its own dialog path for a dirty untitled tab.

- **IDE note:** the save dialog shim now supports `MUI_SAVE_FILE_PICK_SEQUENCE`,
  Save All traces `save_all_dialog path=...`, the command palette copy says
  untitled tabs are included, and the strict Windows harness writes a dirty
  untitled tab through Save All after first exercising Save As.
- **Language note:** no compiler gap surfaced, but this reinforces the existing
  dialog-result ABI problem. Mighty still needs structured command results for
  repeated native dialogs so success, cancellation, unavailable state, and picked
  paths are not coordinated through environment strings and trace text.

### L253. Command wording must stay synchronized with behavior **[finding, P2]**
The command registry still called the workspace file action
`Explorer: New Workspace File` while the release notes and backlog used
`Explorer: New File in Workspace`. The behavior was correct, but inconsistent
labels make similar file actions feel arbitrary.

- **IDE note:** the live command registry, palette tests, and changelog now use
  `Explorer: New File in Workspace`, and Save All copy no longer describes only
  file-backed tabs after the native untitled Save All flow shipped.
- **Language note:** no compiler gap surfaced. Mighty needs command metadata as a
  generated source of truth with action name, short label, long description,
  keybinding, dialog semantics, and test fixture expectations derived together.

### L254. Console-like panel text still needs measured fitting **[finding, P2]**
The compact Run output capture showed long diagnostic lines clipped at the right
edge. The row tried to shorten text using a fixed character-width estimate, but
the actual shaped code font did not match that estimate closely enough.

- **IDE note:** Run output rows now use measured code-font fitting and draw the
  fitted text at the same size, preserving diagnostic prefixes such as
  `[MT2001]` while ending long compact rows with a visible ellipsis inside the
  dock bounds.
- **Language note:** no compiler gap surfaced. Mighty UI still needs reusable
  text-fit primitives for code-font and UI-font labels so each panel does not
  duplicate binary-search ellipsizing in Rust.

### L255. Inputs with trailing adornments need shared text budgets **[finding, P2]**
The compact Quick Open capture showed the empty-search placeholder ending too
close to the `FILES` mode pill. The measured text fit was technically inside its
old bounds, but the visual result still read as overlapping because low-priority
hint copy did not reserve enough space before the high-contrast adornment.

- **IDE note:** Quick Open now derives the query text budget from the same
  rendered text origin used by the draw call and reserves extra trailing space
  for placeholder copy before the mode pill. The regression test models the
  compact `560x520` overlay geometry.
- **Language note:** no compiler gap surfaced. Mighty UI needs a shared input
  field primitive that owns icon, caret, placeholder, typed text, and trailing
  adornment budgets together instead of requiring each overlay to re-create that
  layout math in Rust.

### L256. Compact toolbars still need visible action verbs **[finding, P2]**
The compact Testing drawer hid the `Stop` label and left only a faint square icon
inside the secondary toolbar button. The hit-test worked, but the control read as
blank or broken to a human scanning the drawer.

- **IDE note:** the Testing toolbar now keeps `Stop` visible at compact width
  with a measured label-size regression for the `560x520` gallery sidebar.
- **Language note:** no compiler gap surfaced. Mighty UI needs button primitives
  that can degrade from icon+label to icon-only only when an accessible action
  name, tooltip, or disabled-state treatment remains explicit.

### L257. Header status chips need measured left and right constraints **[finding, P2]**
The compact Run output capture showed `RUN` and the `exit 1` status chip merged
visually into `RUNexit 1`. The status pill respected the shared dock action
controls on the right, but did not reserve a minimum measured gap from the fixed
left header label.

- **IDE note:** the Run header now measures the `RUN` label, enforces a minimum
  status-pill x position after it, and tests the compact `560x520` dock case.
- **Language note:** no compiler gap surfaced. Mighty UI needs shared header-row
  layout primitives for left labels, optional middle text, right status chips,
  and trailing action groups so each panel does not hand-roll competing bounds.

### L258. Toast stacks need viewport-aware visible counts **[finding, P2]**
The compact toast gallery still showed three cards covering the Welcome actions,
making transient feedback feel stale and dominant even when each individual card
was readable.

- **IDE note:** compact windows and bottom-dock-heavy layouts now draw and
  hit-test at most two toast cards, while retaining the existing queue and
  newest-toast dismissal behavior.
- **Language note:** no compiler gap surfaced. Mighty UI needs notification
  stack primitives with viewport-aware caps, protected work regions, and
  consistent draw/hit-test contracts.

### L259. Diagnostic rows need compact metadata priority rules **[finding, P2]**
The compact Problems dock squeezed the diagnostic message, compiler code, and
`Ln/Col` metadata into one narrow row. The result was technically clipped, but
visually read like the message and metadata were colliding.

- **IDE note:** compact Problems rows now use short `line:col` metadata and omit
  the redundant compiler code column, giving the diagnostic message more room
  without changing row height or click mapping.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable
  severity-row primitives that can define priority order for message, code,
  file, and location metadata across compact and full-width surfaces.

### L260. Focused panels must not require a cleanup click before global chrome **[finding, P1]**
When Run, Web Playground, or Testing had keyboard focus, clicking an editor tab
could be consumed as "leave the focused panel" instead of switching or closing
the tab. That made a normal tab click feel broken because the first click only
changed hidden focus state.

- **IDE note:** tab switch and tab close hit-tests now bypass focused dock-panel
  input handling, and successful tab actions clear transient Run/Web/Test/
  Terminal/AI focus so typing returns to the editor immediately.
- **Language note:** no compiler gap surfaced. Mighty UI needs a central chrome
  priority router so global window/tab/sidebar controls are always handled
  before focused panel-local input.

### L261. Primary action labels should not explain implementation details **[finding, P2]**
Welcome showed `New File at Location...` and `New Mighty Project...` in compact
layouts. Both actions were functional, but the wording was longer than the row
budget and the literal ellipsis made even short labels look visually clipped.

- **IDE note:** Welcome now uses concise labels without faux truncation marks,
  and the command palette uses shorter `File: New File` wording while
  preserving the native picker-backed behavior.
- **Language note:** no compiler gap surfaced. Mighty UI needs command metadata
  that can separate concise surface labels from longer tooltip/description text.

### L262. Shortcut rows must visually separate action affordances from key chords **[finding, P2]**
The compact Keyboard Shortcuts overlay drew the selected-row remap hint as
`Enter` immediately beside the actual `Ctrl` + `N` shortcut pills. Visually, it
read like `Enter` was part of the shortcut rather than a row action.

- **IDE note:** compact selected shortcut rows now use the action label `Remap`;
  wider rows still have room for `Enter to remap`.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable
  shortcut-row primitives with separate regions for command name, action
  affordance, and keybinding pills.

### L263. Compact headers should switch labels before truncating core nouns **[finding, P2]**
The compact Run-and-Debug sidebar title rendered as `RUN AND DEB...` beside the
state pill. It technically fit, but it looked unfinished and obscured the panel's
actual identity.

- **IDE note:** compact debug sidebars now use the complete `DEBUG` title while
  wider sidebars keep `RUN AND DEBUG`.
- **Language note:** no compiler gap surfaced. Mighty UI needs header-title
  variants, not just ellipsis fitting, for panel names with long formal labels.

### L264. Resize affordances should not look like stray scrollbars **[finding, P2]**
The shared bottom-dock resize handle was functional, but the narrow centered
purple pill looked like a horizontal scrollbar floating over the editor when the
Problems drawer opened.

- **IDE note:** the lower dock now renders a quieter full-width header band with
  a compact drag grip and calmer preset/close buttons.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable dock
  chrome primitives so resize grips, panel headers, and action clusters are
  visually tied to the panel they control.

### L265. Priority click guards need per-action assertions **[finding, P1]**
The titlebar click guard intentionally handled topbar actions before editor and
panel focus, but it only dispatched More and Quick Open. A click on the visible
Run button matched the guard and was consumed before the normal Run route.

- **IDE note:** the early topbar path now toggles/starts Run for the play button
  and clears competing transient focus, matching the later normal route.
- **Language note:** no compiler gap surfaced. Mighty UI needs event-routing
  tests that assert every intercepted action id is either dispatched or allowed
  to fall through deliberately.

### L266. Clipping is not fitting for mixed header/action rows **[finding, P2]**
Peek Definition clipped its `file:line` label to make room for `Enter / Esc`, but
the compact card still looked crowded because neither side had a deliberate
compact variant or measured ellipsis budget.

- **IDE note:** peek headers now choose `Go / Esc`, `Go/Esc`, or `Esc` at compact widths and
  measured-ellipsis the file label before drawing.
- **Language note:** no compiler gap surfaced. Mighty UI needs a reusable header
  row primitive that budgets title text, status/action hints, and icon slots
  together instead of relying on clip rectangles.

### L267. Popup minimum widths must yield to the visible work area **[finding, P2]**
Signature Help used a sensible minimum popup width, but in a compact editor
column that minimum was wider than the remaining work area. The card drew past
the right edge, so both the signature and documentation looked cut off.

- **IDE note:** signature popup width now caps to the actual work-area width
  before label/doc fitting runs.
- **Language note:** no compiler gap surfaced. Mighty UI needs popup geometry
  helpers that take work-area width as a hard constraint, not just a preferred
  minimum.

### L268. Split dividers need an inner content inset **[finding, P2]**
The right editor pane in a compact split began immediately after the one-pixel
divider, making line numbers and code feel glued to the border.

- **IDE note:** non-left split panes now receive a small inner inset while the
  unsplit editor and left pane keep their historical region.
- **Language note:** no compiler gap surfaced. Mighty UI needs pane-layout
  primitives that distinguish divider geometry from the content-start region.

### L269. Screenshot hooks must clear higher-priority overlays **[finding, P1]**
The inline-diff autoopen hook successfully opened a sample diff, but the
automatic empty-buffer Welcome state was still active and the draw loop rendered
Welcome instead of the diff. The gallery case passed because the frame was
nonblank, even though it did not show the surface being audited.

- **IDE note:** the diff autoopen hook now suppresses automatic empty-buffer
  Welcome before the capture frame renders.
- **Language note:** no compiler gap surfaced. Mighty UI needs screenshot hooks
  to assert the target surface is actually visible, not merely active in state.

### L270. Compact panel headers need complete title variants **[finding, P2]**
The compact Source Control sidebar rendered `SOURCE CO...` beside the git action
icons. It fit inside the measured rectangle, but read like an unfinished label
instead of a deliberate compact panel title.

- **IDE note:** compact SCM sidebars now choose `SCM` before ellipsis fitting;
  wider sidebars still show `SOURCE CONTROL`.
- **Language note:** no compiler gap surfaced. Mighty UI needs a reusable panel
  header primitive that budgets title variants and action clusters together.

### L271. Domain rows need semantic compact forms before ellipsis **[finding, P2]**
The Mighty Agents topology clipped protocol messages and relationship rows into
fragments like `Submit(text: Str...` and `implements Summa...`. The rows fit
geometrically, but users lost the return type or target protocol that made the
row meaningful.

- **IDE note:** Agents rows now use measured fitting and semantic compact forms
  such as `Submit(Str) -> U8` and `impl Summarize` before falling back to
  ellipsis.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable
  domain-label formatters for signatures, relationships, and typed graph rows,
  not only generic text clipping helpers.

### L272. Overlay fit must use the actual visible surface **[finding, P1]**
The Code Actions popup used the reported render width while screenshot captures
were clipped to the explicit gallery size. Its geometry test passed for the
helper inputs, but the real capture still showed the card running past the right
edge.

- **IDE note:** shared visible-surface sizing now caps to
  `MUI_SCREENSHOT_W/H` and `MUI_WIDTH/HEIGHT`, so overlay draw and hit-test
  geometry use the same visible bounds as the captured frame.
- **Language note:** no compiler gap surfaced. Mighty UI needs layout primitives
  that carry authoritative visible bounds through draw, hit-test, and screenshot
  harnesses instead of recomputing them from different size sources.

### L273. Feature-specific captures must control feature preferences **[finding, P2]**
The dedicated minimap gallery case seeded a tall file and scroll position, but
could still render no minimap when the persisted user preference had minimap
disabled. The capture looked like a valid editor screenshot while failing to
exercise the feature under audit.

- **IDE note:** the minimap autoopen hook now forces minimap on for its
  screenshot-only demo path, and the strip anchors to the focused pane's right
  edge with a slimmer compact width instead of a broader render width.
- **Language note:** no compiler gap surfaced. Mighty UI needs capture fixtures
  that pin relevant preferences and assert the target affordance is actually
  visible, not just that a frame was produced.

### L274. Feature captures need seeded editor context **[finding, P1]**
Rename and inline ghost-text captures opened their target state, but left the
automatic Welcome surface visible. The screenshot was nonblank and the internal
feature state was active, yet a human saw the landing page instead of rename or
ghost text anchored in code.

- **IDE note:** rename and ghost-text autoopen hooks now seed Mighty source,
  dismiss Welcome, and lock the probe buffer before opening their feature state.
  The ghost fixture now anchors to an incomplete expression so suggested lines
  render into open editor space instead of crossing existing source text.
- **Language note:** no compiler gap surfaced. Mighty UI needs screenshot
  fixtures that can assert both state activation and the visual layer that wins
  composition for that frame.

### L275. Visual tests must track intentional compact copy **[finding, P2]**
The Welcome screen had already been shortened to `New File` and `New Project`
to avoid compact-window truncation, but two tests still expected the older
longer labels. That made the suite stale and reduced trust in Welcome coverage.

- **IDE note:** Welcome action tests now assert the shipped compact labels.
- **Language note:** no compiler gap surfaced. Mighty UI needs a stronger habit
  of coupling visual-copy changes with tests that describe the current product
  decision, not only the historical label.

### L276. Creation flows should prefer native pickers over bottom prompts **[finding, P1]**
New Project still opened the bottom prompt for a name while New File, Open File,
Open Folder, Save As, and Save All had moved to native picker-backed flows. It
worked, but felt unlike the rest of the IDE and made the Welcome quick action
look like an internal command line.

- **IDE note:** New Project now tries a native folder picker first, treats the
  selected path as the intended project folder, rejects non-empty folders, and
  keeps the typed prompt only as a fallback when native dialogs are unavailable.
- **Language note:** Mighty still cannot pass owned strings or paths through the
  scalar ABI, so dialog selection, path validation, and project creation remain
  shim-owned. A richer string/path FFI would let Mighty own more of this flow.

### L277. Feature commands must validate their document context **[finding, P2]**
Markdown Preview looked available from the command palette on any active file,
so the harness opened a rendered pane over a `.mty` buffer and produced a plain
`opened zz` preview. The UI technically responded, but it taught users the wrong
mental model for a document-specific feature.

- **IDE note:** Markdown Preview now rejects non-Markdown buffers with a warning
  toast and no split pane. The real-mouse harness opens a real `.md` fixture
  through the native file picker before asserting preview rendering and close
  hit-testing.
- **Language note:** no compiler gap surfaced. Mighty UI still needs command
  metadata for document/context eligibility so disabled states, palette hints,
  shortcut routing, and tests can share one rule instead of relying on shim-side
  guards.

### L278. Status counters need semantic labels, not bare numerals **[finding, P2]**
The status bar rendered branch state followed by bare diagnostic numbers. In
screenshots this read like stray text (`main ↑0 ↓0 4 0`) instead of a clear
problems summary, especially because Testing and diagnostics counts can differ.

- **IDE note:** wide status bars now render diagnostics as `N err` and `N warn`,
  with the original compact icon-number fallback preserved for narrow windows.
- **Language note:** no compiler defect surfaced. Mighty UI still needs a
  semantic status-chip primitive so labeled counters, tooltips, hit targets, and
  compact fallbacks can be declared from Mighty instead of hand-built in the shim.

### L279. Recent path truncation must preserve orientation **[finding, P2]**
Open Recent shortened long paths from the left, producing fragments like
`...al\Temp\...` that hid the drive/root context and looked like broken text.
Users need to know both where a recent item starts and which file/folder it ends
at before clicking.

- **IDE note:** recent-file and recent-folder rows now use middle path shortening,
  preserving the root/start and actionable tail within the same fixed row.
- **Language note:** no compiler gap surfaced. Mighty UI still needs measured
  path-display helpers so path formatting can be specified semantically from
  Mighty instead of approximated in Rust with fixed character widths.

### L280. Resize handles must read as layout affordances **[finding, P2]**
The sidebar resize control rendered as a tall floating purple thumb, which looked
more like a scrollbar than a draggable divider. That made manual resizing feel
less obvious even though the hit target worked.

- **IDE note:** the sidebar now draws a persistent edge divider with a compact
  centered grip and an accent edge while resizing, keeping the same broad hit
  target for real mouse drags.
- **Language note:** no compiler gap surfaced. Mighty UI needs declarative
  resize-divider primitives that separate visible affordance geometry from
  generous hit-testing geometry.

### L281. Destructive prompts must name the target before input **[finding, P2]**
Delete Active File required typing the active basename, but the bottom prompt
only said `Delete active file, type name:`. The exact required name appeared only
after a failed attempt, so a protected command still felt vague and clunky.

- **IDE note:** the delete prompt now renders `Delete <basename>, type name:`
  before the user types while preserving the exact-basename confirmation check.
- **Language note:** no compiler gap surfaced. Mighty UI needs context-aware
  prompt label composition so destructive confirmations can name their target
  without shim-only string formatting.

### L282. Overlay geometry must share row budgets with hit-testing **[finding, P2]**
The branch switcher still assumed a fixed 10-row card in parts of the draw and
click path, and its card width math could go invalid in very narrow windows.
That made the overlay vulnerable to cramped resize states where what users saw
and what the mouse could hit drifted apart.

- **IDE note:** branch switcher geometry now clamps the card inside compact
  windows and uses one height-aware visible-row budget for both rendering and
  mouse selection.
- **Language note:** no compiler gap surfaced. Mighty needs measured overlay
  layout primitives so cards, row budgets, and hit regions can be declared once
  instead of manually synchronized through shim helpers.

### L283. Overlay lifecycle needs real-mouse evidence **[finding, P2]**
The branch picker had geometry unit tests, but the Windows harness did not open
it through the visible status-bar branch segment. That left a gap between the
implementation proof and the human workflow: click the branch chip, see the
picker, then dismiss it with the visible close affordance.

- **IDE note:** the branch picker now traces `branch_open count=...`, and the
  strict Windows harness clicks the status-bar branch segment, captures the
  picker, and closes it with the real mouse.
- **Language note:** no compiler gap surfaced. Mighty needs a first-class
  overlay lifecycle/event model so open, close, hit-test, and visual capture
  evidence can be declared alongside the UI instead of stitched together with
  shim traces.

### L284. Destructive project actions need stateful enablement and disk proof **[finding, P1]**
Project Search exposed a replace-all button, but the real-mouse harness only
proved search and result opening. The visible button also used a generic
checkmark and could look ready before the current query had any matched files,
which is a weak affordance for a project-wide write.

- **IDE note:** the replace-all button now only appears active after the current
  search has matches, uses the replace glyph for the action, and the Windows
  harness types replacement text, clicks the visible button, and verifies the
  fixture changed on disk.
- **Language note:** no compiler gap surfaced. Mighty needs declarative command
  state for destructive actions so enabled/disabled visuals, click routing,
  confirmation policy, and harness evidence share one source of truth.

### L285. Runtime surfaces need visible-output probes **[finding, P1]**
The integrated terminal had a PTY-backed implementation and parser tests, but
the Windows real-mouse harness did not prove that a command typed through the UI
actually reached the shell and rendered back into the visible grid. Opening a
terminal is weaker evidence than seeing command output in the shipped surface.

- **IDE note:** the terminal pump can now trace an environment-selected probe
  string when it appears in the visible grid, and the strict Windows harness
  opens Terminal, runs `set`, captures the dock, and waits for the inherited
  `MUI_TERM_PROBE_TEXT` value to appear in the visible-grid trace.
- **Language note:** no compiler gap surfaced. Mighty needs a testable runtime
  surface contract for embedded tools so terminal/web/run panels can expose
  concise state and visible-output evidence without bespoke shim hooks.

### L286. Runtime panels need first-open empty states **[finding, P2]**
The Web Playground could be opened from the command palette without starting a
browser session, but the panel body was blank. That makes a functional feature
look broken during the exact first-run path a human is likely to try while
exploring the IDE.

- **IDE note:** the Web panel now renders a compact `No web session yet` state
  before output exists, includes a visible header Run button, traces `web_open`
  and `web_click run`, and the Windows real-mouse harness opens
  `View: Web Playground`, captures the visible panel, clicks the Run affordance,
  and closes the bottom dock afterward.
- **Language note:** no compiler gap surfaced. Mighty needs declarative runtime
  panel states so `idle`, `starting`, `running`, `failed`, and `finished` can
  drive rendering, command enablement, traces, and tests from one typed model.

### L287. Overlapping hit zones need an event-priority model **[finding, P1]**
The Web panel's visible Run button sat inside the bottom-dock header. The
global dock-resize hit test also covered the top part of that same header, so a
human click on the button could be consumed as a resize start before Web-panel
routing ever saw it.

- **IDE note:** mouse-down processing now samples the Web header action once per
  event and lets Stop/Open/Run win before the shared resize band. The strict
  Windows harness clicks the visible Web Run button and waits for
  `web_click run`, proving the real button dispatches.
- **Language note:** no compiler gap surfaced. Mighty needs a declarative
  event-priority or hit-layer table so overlay, dock, panel-header, resize, and
  editor-body regions are ordered by data instead of repeated long `if/else`
  guards.

### L288. Language-native panels need real-mouse evidence **[finding, P1]**
The Mighty Agents topology was implemented and screenshot-audited, but the
strict Windows harness did not open it from the rail or click its header
affordances. That left one of the IDE's most Mighty-specific surfaces weaker
than generic file, terminal, search, and dialog workflows.

- **IDE note:** the Agents panel now traces topology refresh, Inspect clicks,
  Run clicks, and Run attempts. The Windows real-mouse harness opens Mighty
  Agents from the rail, captures the topology, and clicks the visible Inspect
  and Run buttons in the header.
- **Language note:** no compiler gap surfaced. Mighty needs a first-class
  UI-test vocabulary for language-native features so panels like Agents can
  expose typed events and expected state without adding bespoke scalar trace
  hooks for each visible affordance.

### L289. Focused panel routers must transfer companion state **[finding, P1]**
The Testing-focused mouse router could switch the sidebar to Mighty Agents but
did not carry the companion Agents focus state. The topology drew correctly, yet
the first visible Agents header click was routed as if no Agents header was
active.

- **IDE note:** switching from Testing to Mighty Agents now refreshes the
  topology and marks Agents focused. The real-mouse harness covers this by
  opening Agents from Testing focus and dispatching Inspect and Run from the
  visible header.
- **Language note:** Mighty needs a structured panel-transition primitive that
  updates the active panel, focus ownership, and refresh hooks together.

### L290. Brand assets need shell-size regression tests **[finding, P2]**
The Windows taskbar can choose 20px, 24px, 40px, 64px, or 128px icon entries
depending on DPI and shell context. Shipping only 16/32/48/256px forces Windows
to resample near-miss entries, which makes a carefully drawn mark look fuzzier
than the source preview.

- **IDE note:** `tools/make-icon.py` now emits 16/20/24/32/40/48/64/128/256px
  DIB entries, gives the smallest Mighty monogram more optical weight, and the
  window tests assert the bundled ICO size ladder.
- **Language note:** no compiler gap surfaced. Mighty needs an asset pipeline
  primitive for multi-resolution shell resources and visual-regression metadata
  so app identity does not depend on ad hoc Python tooling.

### L291. Dialog commands need visible semantic conventions **[finding, P2]**
File commands were functionally dialog-backed, but palette labels like
`File: New File` and `File: Open File` did not tell users that a picker would
open. That makes common commands feel weird even when the route is correct.

- **IDE note:** picker-backed commands now use standard dialog ellipsis labels
  (`New File...`, `Open File...`, `Save As...`, `Open Folder...`, `New
  Project...`) while the instant scratch-tab action is explicitly `New Untitled
  File`. Palette tests pin those distinctions.
- **Language note:** no compiler gap surfaced. Mighty UI needs command metadata
  that separates action name, dialog behavior, shortcut, description, and
  menu/palette rendering conventions so labels do not drift from behavior.

### L292. Command help text must reflect current document state **[finding, P1]**
The palette could list file commands whose generic descriptions were technically
true but misleading in context. `Save` on an untitled tab opens a path picker,
`Save All` with no dirty tabs does nothing, and read-only previews cannot be
written. Generic helper text makes those outcomes feel like broken buttons.

- **IDE note:** file-related command rows now describe the active state:
  untitled save paths, read-only previews, file-backed requirements, and dirty
  tab counts. The helper is tested independently so command copy stays aligned
  with the active tab model.
- **Language note:** no compiler gap surfaced. Mighty UI needs first-class
  command eligibility/state metadata so labels, helper text, enabled visuals,
  dispatch, and test expectations come from one command model.

### L293. Disabled controls must share visual and dispatch state **[finding, P1]**
The Settings panel correctly dimmed Inline AI when no Anthropic/Claude API key
was available, but its row click still returned the same toggle/cycle action
code as an enabled setting. Mighty then routed the click as an activation even
though the Rust-side toggle was a no-op. To a user, that reads as a broken
button: the control looks disabled, but still behaves like something happened.

- **IDE note:** disabled Settings rows now return a select-only hit result.
  Inline AI without `ANTHROPIC_API_KEY`/`CLAUDE_API_KEY` can still be selected so
  its explanatory copy is visible, but its disabled switch no longer dispatches
  an action-looking toggle. A focused settings-panel test pins the behavior.
- **Language note:** no new compiler gap surfaced. This reinforces L292:
  Mighty UI needs shared command/preference state metadata so disabled visuals,
  explanatory text, hit-testing, and dispatch all read from the same source.

### L294. First-run action labels must match dialog behavior **[finding, P2]**
The command palette had been updated to use standard ellipsis labels for native
picker actions, but the Welcome quick actions still said `New File`,
`New Project`, `Open File`, and `Open Folder`. That makes the first screen feel
inconsistent: the user clicks what looks like an instant action and gets a
picker instead.

- **IDE note:** Welcome quick actions now use `New File...`, `New Project...`,
  `Open File...`, and `Open Folder...`, matching the palette and the native
  dialog flow. Tests pin the Welcome labels and the palette mirror.
- **Language note:** no new compiler gap surfaced. This again points to a
  shared command metadata model so Welcome, menus, palette, shortcuts, and
  tests all render the same behavior-aware labels.

### L295. Icon-only commands need behavior affordances **[finding, P2]**
The Explorer header correctly hit-tested New File, New Folder, and Collapse All,
but all three controls looked like the same kind of compact icon button. New
File and New Folder open prompts/dialog flow; Collapse All acts immediately.
That difference matters when the user is scanning the toolbar before clicking.

- **IDE note:** the Explorer header now marks the dialog-backed New File and
  New Folder buttons with a compact `...` affordance while leaving Collapse All
  unmarked as an immediate tree action. A focused test pins both the visible
  button hit zones and the dialog/immediate action classification.
- **Language note:** no compiler gap surfaced. Mighty still needs a first-class
  command metadata model that can expose labels, icons, dialog/immediate
  behavior, disabled state, help text, and tests from the same source of truth.

### L296. Pointer capture needs grab-offset and gesture metadata **[finding, P2]**
The sidebar divider had a forgiving hit band, but the resize math used the raw
mouse x immediately. If the user grabbed the band a few pixels off the visible
edge, the first resize call nudged the Explorer width before any real drag.
That made manual resizing feel clunky even though the hit target was technically
easy to find.

- **IDE note:** sidebar resizing now stores the grab offset between the visible
  divider and the mouse-down position, then applies that offset throughout the
  drag. The first resize call preserves the current width; only actual pointer
  movement changes the sidebar. A focused test covers the off-center grab case.
- **Language note:** no compiler gap surfaced. Mighty would benefit from richer
  pointer-event metadata, especially click count/double-click and pointer
  capture lifecycle callbacks, so IDE gestures like divider double-click reset
  can be implemented directly in language code instead of inferred through shim
  state.

### L297. Dialog outcomes need explicit user feedback **[finding, P1]**
Dirty-close Save on an untitled tab correctly opened a native Save dialog, but a
cancelled or unavailable picker returned the same failed-save code to Mighty.
The modal stayed active, which was safe, but the user got no explanation for
why Save appeared to do nothing.

- **IDE note:** dirty-confirm Save now distinguishes cancelled and unavailable
  Save dialogs for untitled tabs. Cancel keeps the confirmation open and toasts
  `Save cancelled; tab is still open`; unavailable picker flow toasts
  `Save dialog unavailable; use Save As`. A regression test covers the cancelled
  dirty-close path.
- **Language note:** no compiler gap surfaced, but the Mighty side still wants
  richer dialog result metadata than scalar success/fail codes. Native dialog
  calls should eventually expose picked/cancelled/unavailable/error states as a
  typed result so command handlers can route fallback prompts, toasts, and focus
  restoration from one explicit outcome.

### L298. Modal copy must be measured before drawing **[finding, P1]**
The dirty-close confirmation used the raw tab basename in its detail line. A
very long filename could run across the modal card, which made the dialog look
broken even though the buttons still hit-tested correctly.

- **IDE note:** dirty-confirm detail copy now uses the same measured tail-fit
  strategy as status/file labels. Long filenames are shortened before drawing,
  preserving the consequence text while keeping the line inside the modal text
  budget. A focused regression checks the measured width.
- **Language note:** no compiler gap surfaced. Mighty UI still needs reusable
  text-measurement and fit primitives on the language side so modal/dialog
  labels, helper text, and dynamic filenames can be composed without bespoke
  shim helpers for every surface.

### L299. Modal action rows must derive from available width **[finding, P1]**
The dirty-close modal's card width responds to narrow windows, but its
Cancel/Save/Discard buttons were fixed at desktop widths. On compact surfaces
the row could extend outside the card even though the dialog itself was
centered.

- **IDE note:** dirty-confirm action buttons now compute their width from the
  modal card width and center labels by measured text width. A compact-card
  regression verifies the full action row and all labels fit.
- **Language note:** no compiler gap surfaced. Mighty UI needs reusable layout
  primitives for action rows, including measured labels, min/max button widths,
  gaps, and compact fallback behavior, so every modal/prompt uses the same
  responsive button math.

### L300. Resize gestures need edge-relative pointer capture **[finding, P2]**
The bottom dock resize band is intentionally forgiving, but its drag math used
the raw mouse y position after capture. When a user grabbed a few pixels below
or above the visible dock edge, the first resize calculation could change the
Terminal/Run/Tests/Web dock height before the pointer actually moved. That made
the lower pane feel unstable during manual layout.

- **IDE note:** bottom dock resizing now stores the grab offset between the
  visible dock edge and the mouse-down position, then applies it throughout the
  drag. The first resize query preserves the current dock height; only actual
  pointer movement changes the pane. A focused regression covers the off-center
  grab.
- **Language note:** no compiler gap surfaced. Mighty still needs first-class
  gesture metadata for pointer capture, including captured-edge coordinates and
  pointer-to-edge offsets, so language-side layout code can implement forgiving
  resize targets without bespoke shim state.

### L301. Feedback surfaces need workflow-scoped replacement keys **[finding, P2]**
Several toast messages came from the same user workflow but did not share a
replacement key. For example, a failed save, a cancelled Save dialog, a Save As
fallback prompt, and a later successful save could all remain visible together.
To a human user that reads as stale text that did not clear, even if each toast
is individually valid.

- **IDE note:** toast operation keys now group the dialog-heavy save/open/create
  outcomes more aggressively. New save dialog results replace stale save errors
  and later successful saves replace the fallback/cancel messages. Focused tests
  cover the exact dirty-close/dialog strings.
- **Language note:** no compiler gap surfaced. Mighty should eventually expose
  command/workflow IDs with feedback events so the UI can replace, update, or
  clear messages by operation identity instead of inferring families from human
  strings.

### L302. Mouse controls need the same feedback as command dispatch **[finding, P2]**
The bottom dock's compact/default/expanded buttons changed the panel size, but
they did not show feedback. The command-palette versions of the same actions
already pushed toasts. A human clicking the visible header controls could see
the panel move but still read the button as unreliable because there was no
state acknowledgement.

- **IDE note:** visible bottom-dock preset clicks now push the same `Dock
  compact`, `Dock reset`, and `Dock expanded` feedback used by palette dispatch.
  The dock resize regression now pins that the mouse path acknowledges the
  preset.
- **Language note:** no compiler gap surfaced. Mighty needs a command metadata
  layer where visible controls and palette commands share one action definition,
  including state mutation, toast/feedback policy, and tests.

### L303. Layout visibility commands need explicit state feedback **[finding, P2]**
Sidebar toggle changed a large part of the interface but did not acknowledge the
new state. Explicit close and width-preset commands already pushed feedback, so
the generic toggle felt less reliable even though it worked.

- **IDE note:** sidebar toggle now pushes `Sidebar opened` or `Sidebar closed`
  and traces the direction. A focused test pins both directions.
- **Language note:** no compiler gap surfaced. This is another case for shared
  command metadata: toggles should expose their resulting state and feedback
  text from one definition used by shortcuts, palette rows, and visible chrome.

### L304. Lifecycle close commands need no-op feedback **[finding, P2]**
The integrated terminal close ABI changed state silently and also treated an
already-closed terminal as a quiet no-op. That is safe internally, but direct
lifecycle controls read as unreliable when users receive no confirmation.

- **IDE note:** terminal close now pushes `Terminal closed` when it actually
  closes the panel/shell state and `Terminal is already closed` for the no-op
  path. The regression covers both cases without requiring a PTY spawn.
- **Language note:** no compiler gap surfaced. Mighty needs command result
  metadata that can distinguish changed-state, already-in-state, failed, and
  unavailable lifecycle outcomes without encoding them as ad hoc toast strings.

### L305. Runtime panel startup failures need visible feedback **[finding, P1]**
The terminal open command could fail during shell/PTY startup and only write to
stderr. In a GUI that reads like a broken toolbar button because the user never
sees stderr and the panel does not open.

- **IDE note:** terminal open now pushes `Terminal failed to open` on startup
  failure and `Terminal opened` when the panel first opens or respawns a shell.
  A forced-failure regression covers the visible error path without depending
  on a real PTY.
- **Language note:** no compiler gap surfaced. Mighty needs structured command
  result states for runtime panels, especially unavailable/failed startup
  outcomes, so commands can drive consistent UI feedback and retry behavior.

### L306. Native dialog cancellation is still a command result **[finding, P2]**
Several native dialog commands treated user cancellation as a quiet no-op. That
kept state correct, but a human pressing a toolbar or command-palette action
could see a dialog close and then receive no acknowledgement from the IDE.

- **IDE note:** Open File, New File, New Folder, and New Project cancellations
  now push short info toasts, and unavailable native-dialog fallbacks push warn
  toasts. Toast operation keys group these outcomes with their matching
  open/create workflow so stale dialog messages are replaced instead of stacked.
- **Language note:** no compiler gap surfaced. Mighty needs a first-class
  command-result enum across the FFI boundary: success, cancelled, unavailable,
  failed, already-in-state, and unchanged. Encoding that as scalar integers plus
  hand-written toast strings is workable but too easy to make inconsistent.

### L307. User-facing tab commands should not clamp invalid targets **[finding, P2]**
The tab store safely ignores out-of-range indices, but the public tab switch and
close ABIs used that behavior directly. A bad mouse hit-test or stale command
index could therefore leave the UI unchanged while returning the current active
tab, which reads like a flaky tab bar.

- **IDE note:** tab switch/close now reject invalid indices with `-1`, keep the
  active tab and tab count unchanged, and push `No tab at that position`. The
  regression covers invalid switch and close requests through the public ABI.
- **Language note:** no compiler gap surfaced. This reinforces the need for
  typed command-result states at the Mighty boundary: internal storage no-ops
  and user-visible command failures are different outcomes and should not share
  the same scalar success-looking return.

### L308. Tab reordering commands need visible unchanged-state feedback **[finding, P2]**
Move-active-tab and sort-tabs commands returned `-1` silently when the active
tab was already at the edge or the tab list was already sorted. Keyboard and
palette users saw no visible state change and no explanation.

- **IDE note:** Move Active Tab Left/Right now toast `Tab is already first` /
  `Tab is already last`; Sort Open Tabs by Name toasts `Tabs already sorted`.
  Tests cover unchanged-state paths and tab toast replacement.
- **Language note:** no compiler gap surfaced. This is another command-result
  enum case: changed, unchanged-because-boundary, and unavailable should be
  distinct typed outcomes instead of magic `-1` plus optional toasts.

### L309. Drag completion should be a visible command outcome **[finding, P2]**
Manual sidebar and bottom-dock resizing changed layout while dragging, but mouse
release had no explicit finish state. Preset buttons already acknowledged size
changes, so hand-resizing felt less certain and could leave stale layout toast
text around after repeated adjustments.

- **IDE note:** releasing a sidebar or bottom-dock resize now toasts the final
  width/height, and layout toasts share one replacement family so repeated
  drags and preset changes clear stale layout text.
- **Language note:** no compiler gap surfaced. Mighty still needs typed command
  outcomes for pointer gestures: started, updated, finished, cancelled, and
  unchanged should be representable without ad hoc scalar returns plus string
  matching.

### L310. Header toolbar hit boxes must share rendered geometry **[finding, P2]**
The Explorer header drew three adjacent action buttons with no visual gap, and
the click hit-test used separate hard-coded offsets. The buttons technically
worked, but a human saw a cramped cluster and could not confidently predict the
target boundaries.

- **IDE note:** Explorer new-file, new-folder, and collapse-all buttons now use
  one shared geometry helper for drawing, hit testing, and header text fitting.
  The rendered buttons have visible spacing, and tests click the same centers
  used by the draw path.
- **Language note:** no compiler gap surfaced. Mighty-side UI code would benefit
  from first-class reusable layout structs/tuples across draw and hit-test paths;
  today the safest option is to keep shared geometry in Rust where richer return
  types and arrays are less fragile.

### L311. Recent location rows should separate object name from parent context **[finding, P3]**
The Welcome screen recent-folder row displayed `samples` as the primary label
and then repeated the full `...\samples` path as the secondary line. That made
the first-run surface look noisy and made the dim line less useful.

- **IDE note:** Welcome and Open Recent folder rows now use the folder name as
  the primary label and the parent location as secondary text. File rows keep
  their existing filename/parent split. Tests cover both row types.
- **Language note:** no compiler gap surfaced. Mighty would benefit from a
  path-display helper available to UI code: basename, parent, root-aware
  truncation, and tooltip/full-path values should be reusable instead of
  hand-authored at each surface.

### L312. Visible chrome buttons should share command feedback **[finding, P2]**
The bottom-dock close command from the palette pushed `Bottom dock closed`, but
clicking the visible X in the dock header closed the panel silently. The state
changed correctly, but the visible affordance felt less reliable than the
palette command.

- **IDE note:** the bottom-dock close hit-test now pushes the same
  `Bottom dock closed` toast as the palette command. The dock geometry regression
  covers the visible close button path.
- **Language note:** no compiler gap surfaced. Mighty needs command/action
  descriptors that can bind multiple triggers to one outcome policy so mouse,
  keyboard, and palette paths do not drift on feedback.

### L313. Pane-local lifecycle controls need command-quality feedback **[finding, P2]**
The Markdown preview split opened and closed correctly, but the visible preview
header close button collapsed the pane without any user-facing outcome. That
made a successful mouse action look indistinguishable from a missed or broken
click.

- **IDE note:** Markdown preview open and close now push lifecycle toasts, and
  Markdown preview toasts replace each other as one operation family. Tests cover
  the visible header close button path.
- **Language note:** no compiler gap surfaced. Mighty needs typed lifecycle
  outcomes shared by palette, keyboard, and mouse actions so pane-local controls
  cannot silently diverge from command behavior.

### L314. Bottom-dock panels need shared close semantics **[finding, P2]**
The shared dock close button acknowledged `Bottom dock closed`, but the Problems
panel reused a dock-shaped header X through a separate toggle path and closed
without feedback. A user clicking two visually similar X controls should not get
two different outcome policies.

- **IDE note:** closing the Problems panel through its visible header X now
  routes through the toggle path with a `Problems panel closed` toast, and layout
  toasts replace older dock/sidebar/problem-panel feedback. Tests cover the
  header close hit plus the resulting feedback.
- **Language note:** no compiler gap surfaced. Mighty needs a reusable
  dock-panel action contract so each panel cannot independently decide whether
  close/open operations report outcomes.

### L315. Native dialog cancellations need one outcome vocabulary **[finding, P2]**
The dirty-close Save path already reported `Save cancelled; tab is still open`,
but cancelling plain Save on an untitled tab or explicit Save As returned a
distinct code with no visible message. The tab stayed open correctly, but the UI
looked like the save command had done nothing.

- **IDE note:** untitled Save and Save As cancellations now push the same
  `Save cancelled; tab is still open` toast as dirty-close saves. Tests cover
  both direct dialog paths and verify the tab remains dirty and unbound.
- **Language note:** no compiler gap surfaced. Mighty still needs a typed result
  enum for dialog outcomes so `Picked`, `Cancelled`, and `Unavailable` cannot
  drift across command surfaces.

### L316. Folder picker outcomes must match file picker feedback **[finding, P2]**
Open File cancellation already surfaced `Open file cancelled`, but Open Folder
cancellation returned a no-op with no visible result. The workspace stayed put,
which was correct, but the command looked inert from the user's point of view.

- **IDE note:** Open Folder cancellation now pushes `Open folder cancelled`, and
  unavailable folder pickers push `Open folder dialog unavailable`. Open-folder
  dialog toasts now share the same replacement family as Open File, and tests
  cover the cancelled folder picker state.
- **Language note:** no compiler gap surfaced. Mighty needs a common dialog
  result vocabulary for file and folder pickers so equivalent UX surfaces cannot
  drift by command type.

### L317. Idle panel toolbar buttons should report state, not disappear **[finding, P2]**
The Web Playground Stop and Open-in-Browser controls could be invoked when no
server or URL existed. Both paths returned without user-facing feedback, which
made the panel controls feel broken even though the state check was correct.

- **IDE note:** Web Stop now reports `No web server running` when idle and
  `Web server stopped` when it actually stops a process. Open-in-Browser now
  reports `Web URL not ready` before a URL is available and `Web browser open
  failed` if launching a non-empty URL fails. Tests cover the idle button paths
  and toast replacement.
- **Language note:** no compiler gap surfaced. Mighty needs panel-toolbar action
  descriptors with enabled/disabled state and canonical no-op outcomes so
  inactive controls can be rendered or messaged consistently.

### L318. Multi-file commands need per-dialog outcome accounting **[finding, P2]**
Save All saved file-backed tabs correctly, but when an untitled tab needed a
Save As path, dialog cancellation and dialog unavailability were folded into the
same generic "needs Save As" count. The result was technically correct and
visually ambiguous: a user-initiated cancel looked like another failed command.

- **IDE note:** Save All now tracks cancelled and unavailable Save As pickers
  separately for untitled tabs. Cancelling reports `Save All cancelled; 1
  untitled file still unsaved`, while an unavailable picker reports `Save dialog
  unavailable; 1 untitled file still unsaved`. Toast replacement treats both as
  save-family outcomes so stale save messages clear.
- **Language note:** no compiler gap surfaced. Mighty needs a typed aggregate
  result for batch commands, with per-item outcomes like `Saved`, `Cancelled`,
  `Unavailable`, `ReadOnly`, and `Failed`, so UI text can be derived without
  ad hoc counter plumbing.

### L319. Compact landing sections must reserve full row height **[finding, P2]**
The 860x560 Welcome screenshot showed the "RECENT FILES" empty-state text clipped
against the bottom edge. The compact layout checked for enough room to start a
section, but not enough room to draw the section header plus a readable row.

- **IDE note:** compact Welcome now gates recent sections through a shared helper
  that reserves header height plus at least one row. At 860x560 it cleanly stops
  after Recent Folders instead of painting a clipped Recent Files empty state.
  The fix was verified with `target/ux-welcome-compact-fixed.png` and a unit
  test for the section-fit helper.
- **Language note:** no compiler gap surfaced. Mighty UI needs first-class layout
  fit predicates for repeated sections so draw and hit-test code can reject
  partial sections consistently before any text is queued.

### L320. Chrome hit-tests need one modal-overlay guard **[finding, P1]**
Manual resize/header click paths in the top-level Mighty event loop excluded
some overlays, but not every prompt or centered modal. That meant a human click
near a sidebar or bottom-dock divider could be stolen by resize/header logic
while palette, quick-open, settings, theme, or prompt UI was visible.

- **IDE note:** the event loop now computes one `chrome_click_allowed` guard and
  uses it for bottom-dock close/presets, Web-panel header controls, bottom-dock
  resize, sidebar resize, and top-bar actions. The guard blocks chrome hit-tests
  while prompts or modal overlays are active, and a source-level regression test
  pins the routing shape.
- **Language note:** no compiler gap surfaced. Mighty UI needs a declarative
  event-layer model so modal surfaces, chrome, panel headers, and editor bodies
  can register priority instead of manually repeating boolean guards in a long
  event ladder.

### L321. Screenshot auto-open overlays must not bypass normal draw geometry **[finding, P1]**
The compact Keyboard Shortcuts screenshot showed a second offset card and close
button behind the visible overlay, then a footer clipped at the screenshot edge.
The hook opened the shortcuts engine correctly, but `end_frame` force-drew it a
second time while the Mighty frame also called `mui_keys_draw`. The draw wrapper
also used raw GPU dimensions instead of the screenshot-visible override.

- **IDE note:** the redundant `shortcuts_autoopen` force-draw path is gone, so
  Keyboard Shortcuts renders once through the normal Mighty draw call. The
  `mui_keys_draw` wrapper now uses `visible_surface_size(ctx)` for geometry, so
  compact screenshots and hit targets share the same bounds. Verified with
  `target/ux-compact-shortcuts-bounds-fixed.png`.
- **Language note:** no compiler gap surfaced. Mighty UI needs a screenshot
  overlay contract that separates "activate state for draw" from "force draw in
  renderer tail" so auto-open hooks cannot accidentally render the same modal
  twice or with mismatched bounds.

### L322. GUI apps need first-class native subsystem metadata **[language/tooling gap, P1]**
The real mouse harness launched the packaged Windows EXE and captured a console
window instead of the IDE. The app itself is a GUI surface, but `mty build`
produced a console-subsystem executable unless the app manifest passed raw
Windows linker flags. Adding only `/subsystem:windows` made the CRT search for
`WinMain`; Mighty emits a normal `main`, so the app also needed
`/entry:mainCRTStartup`.

- **IDE note:** `mighty.toml` now sets
  `link-args = ["-Wl,/subsystem:windows,/entry:mainCRTStartup"]`, and the
  packaged app was verified with `tools/drive-input.ps1` against
  `target/ux-drive-rail-dwm-fixed.png`. Both `tools/drive-input.ps1` and
  `tools/win-ui-harness.ps1` crop to DWM extended frame bounds so screenshots
  match the visible IDE window.
- **Language note:** Mighty should expose a first-class native app metadata knob
  such as `[build] subsystem = "windows-gui"` or `mty build --subsystem gui`.
  The compiler/link driver can then choose the correct subsystem and CRT entry
  per host without each GUI app encoding platform-specific linker incantations.

### L323. Unavailable command actions still need visible outcomes **[finding, P2]**
The AI Copilot panel already explained that an API key was required, but the
input action itself could still feel broken: pressing Enter or clicking the
visible send button with a blank prompt, missing key, or active response just
returned `0` without user-facing feedback.

- **IDE note:** `mui_ai_send` now reports blank prompts, missing keys, active
  streams, and startup failures through toasts and `ai_send blocked=...` traces.
  AI feedback replaces stale AI-family toasts, and the Windows harness clears
  API keys, types into the Copilot input, clicks the visible send button, and
  asserts the missing-key outcome.
- **Language note:** no compiler gap surfaced. Mighty UI needs command/action
  descriptors that can declare enabled state, unavailable reasons, and typed
  command results, so controls and keyboard shortcuts share one visible outcome
  model instead of each ABI hand-rolling guard text.

### L324. Prompt-routed commands need the same outcomes as button-routed commands **[finding, P2]**
The `Ctrl+I` inline ask prompt opened the AI panel and copied the instruction,
but then sent through a lower-level path that skipped the visible AI send
guards. With no API key, the panel opened and the prompt vanished while the
action returned `0`, which made the shortcut feel broken even though the toolbar
send button explained the same unavailable state.

- **IDE note:** `mui_ai_send_inline` now shares the same send helper as
  `mui_ai_send`, preserving the staged instruction while reporting blank input,
  missing keys, active streams, startup failures, and successful starts through
  the same toasts and traces. The new regression test covers the no-key inline
  path and asserts that the AI panel opens with a visible missing-key outcome.
- **Language note:** this reinforces L323. Mighty apps need a declarative
  command/action result contract that can be invoked from prompts, buttons,
  palette entries, and shortcuts without each route deciding independently
  whether an unavailable action should clear UI, toast, trace, or stay staged.

### L325. Borderless windows need forgiving native resize hit zones **[finding, P2]**
The Windows harness proved bottom-right resize worked, but a borderless GUI still
felt clunky because users expect the whole perimeter and corners to behave like
native chrome. A narrow invisible target turns resizing into guesswork,
especially at high DPI.

- **IDE note:** side and bottom resize bands are wider, the top edge is slightly
  easier to hit without stealing normal tab clicks, and corner hit squares are
  larger. The title-bar controls and bottom rail utility icons still win before
  resize hit-testing, so minimize, maximize, close, account, and settings remain
  reliable even when they sit near an edge or corner.
- **Language note:** no compiler bug surfaced. Mighty's window/runtime layer
  should offer first-class borderless window affordance metadata or helpers for
  resize margins, cursor feedback, and edge/corner routing so apps do not each
  hand-tune native-feeling resize behavior through scalar ABI glue.

L326. Human-visible chrome polish needs reusable layout primitives, not only raw
coordinates. The rail logo, active rail indicator, utility hit cells, and resize
grip all needed small visual offsets that also had to preserve mouse hit tests.

- **IDE note:** tightened the rail logo tile, pushed the active rail stack away
  from the mark, gave account/settings visible hit cells, and framed the
  bottom-right resize grip so users can see where to grab the borderless window.
- **Language note:** no compiler bug surfaced. Mighty still needs ergonomic UI
  primitives for paired visual/hit-test rectangles, optical padding, and
  stateful chrome affordances so visual polish does not require duplicating
  constants across rendering and routing paths.

L327. Native dialog ownership is not enough; the app also needs predictable focus
restoration after modal child processes return. The Windows file/folder pickers
run through PowerShell STA helpers, and the first click after a picker could
sometimes reactivate the IDE instead of dispatching the intended topbar command.

- **IDE note:** restore the IDE window foreground after file, save, new-file, and
  workspace folder dialogs return, reducing the chance that a human's next click
  is swallowed as a focus-only activation.
- **Language note:** no compiler bug surfaced. Mighty should expose first-class
  native-dialog lifecycle helpers for parent ownership, foreground restoration,
  and post-modal event draining, so apps do not hand-roll Win32 focus repair.

L328. Workspace-level commands need deterministic project discovery, not a
single top-level file fallback. The Test panel could run from a scratch tab by
falling back to the workspace, but it only found a root `mighty.toml` or the
first top-level `.mty` file.

- **IDE note:** workspace test fallback now prefers the root manifest, then
  nested package manifests, then `tests/*.test.mty`, then ordinary `.mty` files,
  while skipping build/cache folders. This makes Run Tests behave better from
  scratch tabs and normal package layouts.
- **Language note:** no compiler bug surfaced. Mighty would benefit from
  first-class package/workspace discovery APIs so IDE commands can ask for a
  testable package target without each tool re-implementing directory traversal.

L329. Sidebar text must be measured in pixels, not guessed from character counts.
The Agents panel empty state and live-inspect note could run into the divider
after a human resize because they used rough width estimates and one raw string.

- **IDE note:** Agents sidebar summary, inspect note, and empty-state copy now
  use the shared measured text fitter before drawing, so compact sidebars
  ellipsize instead of overlapping the editor boundary.
- **Language note:** no compiler bug surfaced. Mighty needs ergonomic measured
  text/ellipsis helpers in the language UI layer so every panel can request
  "fit this line in this box" without hand-rolled Rust-side width code.

L330. Disabled input surfaces need first-class state, not just a dimmed icon.
The AI Copilot no-key state blocked sends correctly, but the composer still
rendered typed draft text, making the unavailable state feel like a broken chat.

- **IDE note:** when no AI key is configured, the composer now renders setup
  copy instead of the typed draft, and its affordance reads as informational
  while still giving a clear unavailable-send response when clicked. The
  disabled composer's height and send hit target are derived from the displayed
  setup copy, not hidden input state.
- **Language note:** no compiler bug surfaced. Mighty UI would benefit from a
  built-in disabled-input/composer primitive that separates stored state from
  displayed setup copy and from click feedback.

L331. Async tool panels need lifecycle states richer than a single running
boolean. The Testing panel can know the final test summary before the child
process is fully reaped, which made complete results appear under a "running"
header during fast UI captures.

- **IDE note:** the Test panel now treats an authoritative final summary as a
  distinct visual state, showing `finalizing...` instead of `running...` while
  the process is still being reaped. Summary copy now uses unresolved-results
  state rather than raw process-running state.
- **Language note:** no compiler bug surfaced. Mighty UI/runtime APIs should
  expose async command lifecycle phases such as starting, streaming output,
  final output received, reaping, complete, and cancelled.

L332. Overlay affordances need shared exclusion geometry. Toasts already
reserved the bottom dock and left sidebar, but the right AI drawer was a
separate overlay path, so warning toasts could cover the AI composer.

- **IDE note:** toast draw and click geometry now accept a right reserve and use
  the AI drawer width while Copilot is open. A toast can still be dismissed, but
  it no longer blocks the drawer's input or close affordances.
- **Language note:** no compiler bug surfaced. Mighty UI would benefit from a
  first-class overlay-safe-area primitive so panels, drawers, modals, toasts,
  and hit-testing consume one shared exclusion model instead of duplicated
  scalar geometry.

L333. Resize handles need explicit affordance states. The sidebar and lower dock
could be resized, but their visible handles were so subtle that manual resizing
felt like guessing at invisible edges.

- **IDE note:** sidebar and bottom-dock resize handles now draw larger pill
  grips with grab dots, stronger active color, and the same existing hit zones.
  This keeps the interaction precise while making the target obvious.
- **Language note:** no compiler bug surfaced. Mighty UI should expose reusable
  resize-grip primitives with normal, hover, dragging, disabled, and orientation
  states so custom panel chrome does not reimplement affordance drawing.

L334. Modal rows need measured text budgets. Settings rows looked fine at the
default desktop size, but their left labels/descriptions were drawn without
reserving the right control column, so narrow windows could let text run under
toggles, steppers, or chips.

- **IDE note:** Settings now computes the right-side control geometry before
  drawing row text, ellipsizes labels/descriptions against the remaining budget,
  and measures the footer label instead of estimating by character count.
- **Language note:** no compiler bug surfaced. Mighty UI needs reusable
  constraint helpers for row layouts: measured leading text, reserved trailing
  controls, ellipsis policy, and shared hit geometry from one declarative row
  spec.

L335. Dense reference overlays should size rows from content, not habit. The
Keyboard Shortcuts overlay used tall single-line rows, so the list looked sparse
and forced unnecessary scanning even though each row only needed a command name
and key pills.

- **IDE note:** the shortcuts overlay now shows more rows with tighter row
  height, keeps key pills vertically centered, and measures/clips the footer
  hint before the branded footer tag.
- **Language note:** no compiler bug surfaced. Mighty UI should expose list/table
  density presets and measured footer slots so reference overlays can choose a
  compact, comfortable, or spacious density without manual geometry tuning.

L336. Native dialog return can swallow the next mouse transition. Immediately
after a native folder picker returned, the next rail mouse-down could be
consumed before the rail hit-test trace appeared, which made downstream
mouse-only SCM checks operate on the previous Search panel.

- **IDE note:** the Windows UI harness now treats rail navigation as a verified
  mouse action, retrying bounded clicks until the intended rail panel trace is
  observed before exercising that panel's visible controls.
- **Language note:** no compiler bug surfaced. Mighty should eventually expose
  an input-settled/native-dialog-complete event or post-dialog focus barrier so
  app code and harnesses can wait for a reliable interactive frame instead of
  timing around OS modal transitions.

L337. Feedback toasts need navigation-aware priority. Low-severity info/success
toasts can become misleading when they linger after the user switches to a
different rail panel; the next panel appears to have caused an old message.

- **IDE note:** panel switches now clear only low-priority toasts while keeping
  warnings and errors visible. This prevents stale completion/no-op feedback
  from following the user into another workflow.
- **Language note:** no compiler bug surfaced. Mighty UI should expose toast
  scopes and priorities so navigation can clear scoped informational feedback
  without dropping action-required warnings or errors.

L338. Focused pickers need their own visual-test entry points. The Open Recent
flow is not the same surface as the branded Welcome landing, so testing only
Welcome let picker footer actions, overflow rows, and empty-section states drift.

- **IDE note:** the Open Recent picker now reserves footer space, always exposes
  Open File/Open Folder actions, shows empty sections as framed rows, reports
  hidden overflow rows, and has a dedicated `MUI_RECENT_AUTOOPEN` gallery case.
- **Language note:** Mighty UI would benefit from declarative visual-test seeds
  for overlays and pickers so test fixtures can populate realistic UI state
  without adding one-off environment hooks in the ABI layer.

L339. Overflow needs visible affordance even when input already works. The tab
strip could scroll and the active tab was kept visible, but the only clue that
earlier/later tabs existed was a tiny edge sliver.

- **IDE note:** tab overflow now draws stronger edge fades and accent grips, with
  a left-edge arrow where it cannot steal the close target. Mouse hit-testing and
  close/switch mapping stay unchanged.
- **Language note:** Mighty UI needs reusable overflow affordance primitives for
  scrollable strips/lists so "more content offscreen" is not reimplemented as
  bespoke rectangles in every component.

L340. Headless visual hooks must not add transient success chrome. The Web
Playground screenshot seed opened a ready URL, then the headless browser-open
path pushed the same success toast a human click would get. That made the
capture technically successful but visually masked the panel being inspected.

- **IDE note:** headless Web browser-open now returns success without pushing an
  `Opened ...` toast. Interactive browser opens still toast, and empty URL still
  warns because it is actionable.
- **Language note:** no compiler bug surfaced. Mighty UI needs a visual-test
  lifecycle primitive that can activate state and acknowledge side effects
  without injecting transient feedback into the frame under audit.

L341. Real mouse UX harnesses need first-class desktop injection evidence. A
focused strict-mouse run could aim at the right logical targets yet still miss
the IDE when the controller terminal stayed above the app, and SendInput
left-button events did not reach winit in this desktop session even after cursor
movement did.

- **IDE note:** the Windows harness now keeps the target IDE window topmost
  during the run, logs client/screen cursor mapping, uses `SetCursorPos` for
  real cursor movement, and uses Win32 mouse button events for clicks/wheels.
  The focused `-EarlyMouseOnly` gate proves command center, Run, branch picker
  open/close, Welcome New Project, and Welcome New File with real mouse traces.
- **Language note:** Mighty still lacks a typed, built-in UI automation harness
  primitive that can assert z-order, physical-to-logical coordinates, and input
  delivery as separate facts instead of inferring them from final UI state.

L342. `mty build` can predeclare filesystem runtime imports even when the app
does not call Mighty-native filesystem APIs. The Windows package gate failed at
link time on `mty_runtime_fs_*` symbols after the release build emitted a new
object against a wider runtime ABI than Mighty IDE's staged archive exported.

- **IDE note:** `mty-rt-abi` now exports minimal filesystem ABI stubs so the
  packaged native binary links again. They return empty/error values because the
  IDE routes filesystem behavior through `mighty-ui-sys` FFI today, not through
  Mighty-native fs calls.
- **Language note:** Mighty needs a versioned runtime ABI manifest or linker
  helper so applications can ship the exact symbol surface required by the
  compiler instead of discovering missing imports during packaging.

L343. Disabled composer placeholders need their own compact copy, not a repeated
setup sentence. The AI panel body already explains `ANTHROPIC_API_KEY` and
restart requirements; repeating that full sentence in the narrow bottom composer
wrapped into a stray final line and made the disabled input look broken.

- **IDE note:** the no-key AI composer now uses the shorter one-line
  `Set API key to enable AI` placeholder while keeping the detailed offline
  instructions in the panel body and warning toast.
- **Language note:** no compiler bug surfaced. The UI layer needs layout tests
  that assert compact empty/disabled placeholders fit within their real control
  budget instead of relying only on screenshot review.

L344. Generic LSP parity must include quick fixes, not only read-only
intelligence. Completion, hover, definition, and diagnostics already routed
non-Mighty files through configured language servers, but Ctrl+. still used the
Mighty-only `mty lsp` code-action request path.

- **IDE note:** code-action requests now route non-Mighty files through the
  registry-backed generic LSP client using `textDocument/codeAction` range
  params, so inline-edit quick fixes from servers such as rust-analyzer can
  appear in the existing quick-fix menu. The Mighty-only `Fix all (mty)` action
  remains limited to Mighty files.
- **Language note:** no compiler bug surfaced. Mighty can now apply command-form
  actions that carry a `WorkspaceEdit` in their arguments, but should eventually
  support the full LSP code-action lifecycle, especially server-initiated
  `workspace/applyEdit` on a long-lived connection.

L345. Generic LSP parity also needs mutating navigation features, not only
read-only requests. Signature help and rename were still routed through the
Mighty-only `mty lsp` helper, so Rust/Python/Go/etc. files could get generic
completion, hover, definition, diagnostics, and quick fixes, but F2 rename and
Ctrl+Shift+Space signature help quietly skipped their configured server.

- **IDE note:** the registry-backed generic LSP client now supports
  `textDocument/signatureHelp`, `textDocument/prepareRename`, and
  `textDocument/rename` request shapes, including escaped `newName` payloads.
  The ABI keeps the Mighty path on the existing bespoke client and routes every
  non-Mighty language through the configured server, then reuses the existing
  `parse_signature_help` and `parse_workspace_edit` application path. The local
  identifier scan/fallback remains as the last resort when a server is missing
  or returns no `WorkspaceEdit`.

L346. Outline parity must route `documentSymbol` through the active language
server. The Outline panel already had a parser for both `DocumentSymbol[]` and
`SymbolInformation[]`, but the refresh path always called the Mighty-only
`mty lsp` helper for any file-backed tab. That meant non-Mighty outlines never
asked rust-analyzer/pyright/gopls/etc. for their real symbol tree.

- **IDE note:** `mui_outline_refresh` now keeps Mighty on the existing
  `mty lsp` request, but routes every other language through the registry-backed
  generic LSP client using `textDocument/documentSymbol`. Missing servers, empty
  results, or mty-lsp's `-32601` still fall back to the shim scanner, so the
  panel remains nonblocking and useful even without an installed server.

L347. Command-only code actions should not become inert menu rows. Some servers
return quick fixes as plain `Command` objects, or as CodeActions whose `command`
field must be executed before the edit is known. The menu previously filtered
those out unless the command arguments happened to embed a `WorkspaceEdit`.

- **IDE note:** parsed code actions now preserve command name + raw
  `arguments`, including nested CodeAction `command:{...}` objects. Command-only
  actions stay visible, and applying one on a non-Mighty file runs a scoped
  `workspace/executeCommand` through the generic LSP client, then applies any
  returned `WorkspaceEdit` through the same existing edit pipeline. This closes
  the common returned-edit command path while keeping failures nonblocking.
- **Remaining LSP gap:** servers that require a long-lived connection and send
  `workspace/applyEdit` as a server-initiated request during command execution
  still need a persistent LSP session with request/response handling.

L348. The Problems panel has to follow the same language routing as editor
squiggles. It previously aggregated only `.mty` tabs via `mty check`, so Rust,
Python, TypeScript, and other LSP-backed files could show active editor
diagnostics while the dock stayed empty.

- **IDE note:** Problems refresh now gathers every open file-backed tab, active
  tab first, using each tab's live `TextModel` as source. Mighty files still use
  `mty check`; other languages use the configured generic LSP server's
  `publishDiagnostics` and feed the resulting diagnostics into the same grouped,
  click-to-jump dock model.

L349. Problems grouping must key on identity, not presentation. Grouping by
basename is visually compact, but two open files named `main.rs` or `index.ts`
are distinct diagnostic targets and should not collapse under one header.

- **IDE note:** the Problems model now groups and counts by full path while
  keeping the basename for icons and normal compact headers. When duplicate
  basenames are present, the header label expands to the path and remains
  ellipsized by the existing row-fitting code.

L350. Active diagnostics should use the same live source as the editor when the
language server supports it. Reading disk for non-Mighty diagnostics makes
unsaved Rust/Python/TypeScript edits look clean until save, even though the IDE
already owns the edited text in the active tab model.

- **IDE note:** `mui_diag_refresh` now keeps Mighty on saved-file `mty check`,
  but sends the active tab's live `TextModel` contents to generic LSP
  `publishDiagnostics` when the configured path is the active tab. Disk fallback
  remains for non-active paths and missing tab state.

L351. Go-to-definition has to parse every shape allowed by LSP, not just the
server shape seen first. Some servers return `LocationLink[]` from
`textDocument/definition`; parsing only `Location` makes definition navigation
look broken even when the server answered correctly.

- **IDE note:** definition parsing now handles `LocationLink` results by reading
  `targetUri` and preferring `targetSelectionRange.start`, with a fallback to
  `targetRange.start`. The existing `Location` parser remains the fallback, so
  Mighty and servers returning plain `Location` keep the same behavior.

L352. Generic diagnostics must be keyed to the requested document URI. Language
servers can publish diagnostics for multiple workspace files, and some send
several notifications for the same file as analysis settles. Reading the first
`diagnostics` array in the stream can show another file's errors or preserve an
empty early result.

- **IDE note:** the generic diagnostics reader now waits for a
  `publishDiagnostics` notification containing the requested `file://` URI, and
  parsing selects the latest matching notification for that URI. The legacy
  parser entry point remains for tests and single-notification callers.

L353. Hover parsing needs the full LSP `Hover.contents` union, not only
`MarkupContent`. Some servers return a plain marked string or an array of marked
strings, so looking only for a nested `value` field can make hover appear empty.

- **IDE note:** hover parsing now accepts `contents: "..."`, `{kind,value}`,
  `{language,value}`, and arrays mixing strings and marked-string objects. The
  parsed text still flows through the existing markdown cleanup, wrapping, and
  popup rendering path.

L354. WorkspaceEdit positions need LSP offset semantics, not Rust `char` counts.
LSP defaults `character` offsets to UTF-16 code units; treating them as Unicode
scalar indexes applies rename/code-action edits at the wrong byte when a line
contains non-BMP characters before the edit range.

- **IDE note:** `apply_text_edits` now maps edit ranges with UTF-16 column
  accounting. ASCII and BMP-only files keep the same offsets, while emoji and
  other surrogate-pair characters no longer shift later LSP edits.

L355. `WorkspaceEdit.documentChanges` is a mixed array, not just text edits. LSP
servers can interleave `TextDocumentEdit` objects with resource operations like
`create`, `rename`, and `delete`; those resource operations also carry URIs, so
a parser that scans for the next URI and next edits array can attribute edits
to the wrong file.

- **IDE note:** `documentChanges` parsing is now object-scoped. Only objects
  containing both `textDocument` and `edits` produce text edits, resource-only
  operations are skipped, and field order inside the `TextDocumentEdit` object
  no longer matters.

L356. Some LSP commands apply edits as a server request, not as the command
response. A command such as `workspace/executeCommand` can trigger
`workspace/applyEdit` and wait for the client to answer before returning its
own result; a one-shot client that only reads the original request id can hang
or report "no edit" even though the server sent a valid `WorkspaceEdit`.

- **IDE note:** generic non-Mighty command execution now keeps stdin available
  while reading, acknowledges `workspace/applyEdit` with `applied:true`, and
  preserves the request body so the existing workspace-edit application path can
  apply those edits. Inline edits and command responses continue through the
  same parser as before.

L357. Layout controls need a fast path as well as palette discoverability.
Sidebar width presets were available from the Command Palette and by mouse drag,
but repeated resize adjustments still required searching commands or aiming at
the divider, which makes compact-window work feel slower than a best-in-class
IDE should.

- **IDE note:** `Ctrl+Alt+B` now cycles the sidebar through compact, wide, and
  responsive default widths using the same `mui_sidebar_layout_dispatch` path as
  the palette commands. The command is palette-visible, appears in Keyboard
  Shortcuts, opens the sidebar when needed, and has tests for chord resolution
  and preset rotation.

L358. Split-editor lifecycle commands should acknowledge both changes and
one-pane no-ops. Split, focus-next, and close-pane were functionally correct,
but palette or shortcut users received no visible confirmation, so a successful
split/focus/close looked too similar to a missed command.

- **IDE note:** generic pane operations now toast `Split editor right`,
  `Focused editor pane N`, `Closed editor pane`, or `Only one editor pane` for
  one-pane no-ops. The existing pane ABI regression now checks that direct and
  palette-dispatched pane operations share those visible outcomes.

L359. LSP code actions need diagnostic context, not just a line range. The IDE
was asking `textDocument/codeAction` with `context.diagnostics: []` even when the
current line already had parsed diagnostics. Some servers use those diagnostics
to decide which quick fixes are applicable, so the empty context could hide real
fixes while completion/hover/rename still worked.

- **IDE note:** code-action requests now serialize the active line's stored
  diagnostics into the LSP `context.diagnostics` array for both the Mighty and
  generic LSP clients. Empty/no-diagnostic lines preserve the previous empty
  array behavior; request-builder tests cover both paths.

L360. Code-action no-ops need visible feedback. Ctrl+. could return no actions,
or an LSP command could execute without yielding a workspace edit, and the UI
looked the same as a missed keystroke.

- **IDE note:** code-action request/apply paths now toast `No code actions
  available`, `Code action needs a file`, `Code action produced no edit`, or
  success/failure outcomes for applied actions. The code-action ABI tests cover
  the no-actions and no-file command paths, and code-action toasts replace stale
  code-action feedback instead of stacking.

L361. Terminals need row-local erase semantics, not only screen clears. After
cursor addressing and display erase were working, the VT parser still skipped
`CSI K` erase-in-line sequences. Shell prompts and progress renderers use line
erases to repaint the current row without disturbing output above or below;
skipping them leaves stale prompt/status text on screen.

- **IDE note:** the integrated terminal now handles `ESC[K`, `ESC[1K`, and
  `ESC[2K`, clearing cursor-to-end, start-to-cursor, or the whole current row
  respectively while preserving adjacent rows. Terminal parser tests cover all
  three modes plus row-locality.

L362. Terminal relative cursor movement is core shell behavior, not a full-TUI
edge case. Prompts, redraw loops, and status renderers use `CSI A/B/C/D` to
move around the current grid after printable writes have advanced the cursor;
skipping those sequences leaves visible escape tails or updates in the wrong
cells.

- **IDE note:** the integrated terminal now handles `ESC[nA`, `ESC[nB`,
  `ESC[nC`, and `ESC[nD`, defaulting missing counts to one and clamping motion
  to the terminal grid. Parser tests cover movement after writes, large-count
  clamping, and consuming the control bytes without printing garbage.

L363. Cursor save/restore needs parser-owned state. Shells often bracket prompt
or status redraws with DEC `ESC 7`/`ESC 8` or CSI `s`/`u`; treating those as
unknown escapes means later text lands wherever the redraw happened to leave the
cursor.

- **IDE note:** the integrated terminal now stores and restores the active grid
  cursor for both DEC and CSI save/restore sequences. Parser tests cover
  overwrite behavior after restore and verify the control bytes are consumed
  instead of leaking into terminal output.

L364. Cursor addressing has more common forms than row/column and arrows. Prompt
renderers also use `CSI G` for absolute columns and `CSI E`/`CSI F` for
next-line/previous-line movement with an implicit carriage return; skipping
those forms makes redraw code leave escape text behind or paint at stale
columns.

- **IDE note:** the integrated terminal now handles `ESC[nG`, `ESC[nE`, and
  `ESC[nF`, including default count one and grid clamping. Parser tests cover
  absolute column moves, next/previous line resets to column zero, large-count
  clamps, and consuming the control bytes.

L365. Horizontal and vertical absolute cursor forms need to preserve the other
axis. `CSI \`` and `CSI d` move only the column or row, while `CSI e` moves
down without the carriage-return behavior of `CSI E`; conflating these with
line movement makes prompt redraws drift horizontally.

- **IDE note:** the integrated terminal now handles `ESC[n\``, `ESC[nd`, and
  `ESC[ne`, including default count one, grid clamping, and column preservation
  for vertical-only movement. Parser tests distinguish these forms from
  next-line/previous-line movement and verify control bytes do not leak.

L366. Command shortcuts need the same unavailable-state feedback as visible
buttons. Running without a file opened the Run dock but returned `0` silently,
so users saw a panel switch without knowing why no process started.

- **IDE note:** `mui_run_start` now reports `No file to run` when invoked from a
  scratch/untitled context and still opens the Run dock while closing competing
  lower panels. Spawn failures now also surface an error toast with the file
  name. Tests cover the no-file command outcome and dock-owner transition.

L367. Focused commands need their own unavailable-state path. `Run Test at
Cursor` is stricter than `Run Tests`: it needs an active file, not just a
workspace fallback. Returning `0` silently from a scratch tab made the shortcut
feel broken even though the Testing panel could be shown.

- **IDE note:** `mui_test_run_at_cursor` now opens the Testing panel and reports
  `Open a Mighty file before running test at cursor` when no active file exists.
  Spawn failures now also surface a file-specific error toast. Tests cover the
  scratch/no-target path and visible panel transition.

L368. Command registry audits must cover metadata and dispatch together. `View:
Cycle Sidebar Width` was callable from `main.mty`, but it was outside the
sidebar-layout range used by the dispatcher audit and lacked rich palette row
metadata, so the command could render as a generic row and fail coverage even
though the behavior existed.

- **IDE note:** the cycle-sidebar-width command now has explicit rich palette
  metadata and is listed in the direct dispatcher audit. Focused tests cover
  rich row metadata, central Mighty dispatcher routing, and the new
  test-at-cursor unavailable-state feedback.

L369. Restart-style commands need visible no-target feedback too. Debug Restart
could fail before any target had ever been launched and only log inside the
debug console, so palette or shortcut users saw no immediate explanation.

- **IDE note:** `mui_dbg_restart` now opens the Run and Debug view and reports
  `No debug target to restart` when no previous program exists, while failed
  restarts of an existing target surface `Debug restart failed`. Debug start
  failures also now toast the active file name. Tests cover the no-target
  restart panel transition and toast.

L370. Toolbar-only feedback is not enough when palette commands call different
ABI entry points. Debug Step/Stop toolbar buttons explained idle-state no-ops,
but palette and shortcut dispatch called `mui_dbg_step_*`, `mui_dbg_pause`, and
`mui_dbg_stop` directly, where the same idle commands returned silently.

- **IDE note:** direct debug Stop, Pause, Step Over, Step Into, and Step Out now
  open the Run and Debug view and show the same unavailable-state messages as
  the toolbar route. Tests cover the idle direct-command path and the broader
  debug command feedback filter.

L371. Stop commands need idle feedback even when the visual button looks
disabled. Run/Test Stop can be invoked through toolbar hit-tests, ABI calls, and
future palette bindings; silently doing nothing leaves users unsure whether the
click missed, the process already exited, or the command failed.

- **IDE note:** idle `mui_run_stop` now opens the Run dock, closes competing
  lower panels, and reports `No run process to stop`. Idle `mui_test_stop` now
  opens Testing and reports `No test run to stop`. Tests cover both visible
  no-op outcomes.

L372. Gutter actions still need command-grade unavailable feedback. Breakpoint
toggling is usually reached by clicking a visual gutter target, but scratch and
untitled buffers have no file path to send to the debugger. Returning `0`
silently made the click look missed instead of explaining the saved-file
precondition.

- **IDE note:** `mui_bp_toggle` now opens the Run and Debug view and reports
  `Save the file before setting breakpoints` when the active tab has no file
  path. The regression test asserts the panel transition, empty breakpoint set,
  and toast message.

L373. Split Start/Continue APIs need the same unavailable-state contract. The
user-facing command routes through `mui_dbg_start`, which can start from idle or
continue from paused, but the lower-level `mui_dbg_continue` ABI only makes
sense when execution is paused. Direct callers previously got a silent no-op
from idle or terminated states.

- **IDE note:** `mui_dbg_continue` now preserves the real continue path when
  paused, reports `Debug session already running` while running, and reports
  `Continue is available when paused` from idle/terminated states while opening
  Run and Debug. The direct-debug regression covers the idle no-op.

L374. Right-aligned header actions need reserved text budgets. The Markdown
breadcrumb rendered `workspace > file > symbol` left-to-right while the Preview
pill was right-aligned independently, so compact windows or long filenames could
draw breadcrumb text underneath the button.

- **IDE note:** `mui_breadcrumb_draw` now reserves the Markdown Preview pill's
  left edge, fits breadcrumb segments to that boundary, and skips separators or
  icons that no longer fit. The regression measures a compact Markdown filename
  budget and ensures the fitted text stays before the Preview pill while keeping
  the `.md` suffix.

L375. Rename fallback must not turn syntax into symbols. A local identifier scan
is useful when a language server is unavailable, but if the server explicitly
rejects `prepareRename` or the token is a language keyword, opening the rename
input invites a wrong edit that can rewrite syntax words across the buffer.

- **IDE note:** symbol rename now honors explicit `prepareRename` failures,
  filters keyword-like tokens such as `fn`, `let`, `agent`, and `protocol`, and
  reports `No rename target` instead of opening the inline rename editor. Unit
  tests cover server rejection parsing, range parsing, keyword filtering, and
  local identifier extraction.

L376. Autocomplete rows need a real right column budget. Completion labels,
signature snippets, and kind metadata are independent text runs; without fitting
the label and footer to the right-aligned metadata boundary, long candidates can
draw under the `function`/`snippet` label or overflow the hint strip.

- **IDE note:** completion drawing now measures the right-aligned kind column,
  fits row labels/signatures before it, and fits the selected footer name/tail
  within the panel edge. Regression tests measure compact completion budgets so
  long labels and footer text ellipsize before overlapping metadata.

L377. Replace commands need visible outcome feedback. `Replace Next` and
`Replace All` both return `0` for empty queries, read-only previews, and no
matches, so leaving those paths silent makes Enter feel broken and gives no clue
about whether the command was refused or simply found nothing.

- **IDE note:** in-file replace now toasts `Enter text to replace`, read-only
  preview refusal, no-match outcomes, and successful replacement counts while
  preserving the existing numeric ABI returns. Regression tests cover empty
  queries, no matches, successful next/all replacement, and read-only binary
  previews.

L378. Dirty-close guards should announce the guard. Opening the unsaved-work
confirmation modal is the correct safety behavior, but the initiating close or
quit command still returns a refusal-like value. Without a toast, a repeated tab
close or quit can feel like a missed shortcut instead of a protected operation.

- **IDE note:** dirty tab close now reports the specific tab that needs review,
  and quit with unsaved work reports the number of unsaved tabs before showing
  the confirmation overlay. The tab ABI regression covers file-backed, scratch,
  repeat-close, and quit confirmation feedback.

L379. Format guards need user-facing explanations. `mty fmt` is intentionally
blocked for non-`.mty` files because it can corrupt unsupported inputs, but a
silent `0` return makes Format Document look inert. Untitled buffers have the
same issue when no file path exists yet.

- **IDE note:** `mui_format_current` now reports `Save the file before
  formatting` for untitled buffers and `Format is available for Mighty files`
  for unsupported extensions, while preserving the existing return codes and
  non-`.mty` data-loss guard. The regression verifies both toasts and proves the
  unsupported file remains byte-for-byte unchanged.

L380. Explicit navigation commands should explain empty results. Hover,
Go to Definition, and Peek Definition are user-invoked actions, so a `0` return
without visible feedback makes a missing saved path, absent server, or genuine
no-result response look like a broken shortcut.

- **IDE note:** hover, definition, and peek now toast save-first guidance for
  untitled buffers; hover reports `No hover information` when no data is
  available; definition and peek report `No definition found` for empty
  targets. The regression uses a saved plain-text file to cover deterministic
  no-result paths without requiring an external language server.

L381. Explicit autocomplete needs a post-merge empty-state hook. The core
completion request runs during ordinary typing, so it cannot toast on empty
results without becoming noisy. Snippet candidates are also merged after the
engine request, so an explicit command must report emptiness only after both
semantic/buffer and snippet sources have contributed.

- **IDE note:** Mighty now calls `mui_complete_report_empty` only from the
  Ctrl+Space and palette autocomplete paths after snippet injection still leaves
  zero candidates. Passive typing remains quiet, while explicit autocomplete
  reports `No completions available`. The regression covers both an empty
  engine and a real buffer-word candidate to prevent false empty toasts.

L382. Modal button labels need the same measured fit as body copy. Compact
confirmation dialogs can reserve enough geometry for today's short English
actions while still being brittle to future copy or localization, so action
labels should be measured and shortened before drawing.

- **IDE note:** the unsaved-changes confirmation now fits button labels through
  the same measured ellipsis path used by other chrome text before centering
  them. The regression keeps current labels unchanged and proves a longer
  destructive-action label cannot spill outside compact modal buttons.

L383. Edge-only editing commands still need visible outcomes. Multi-cursor
commands like Ctrl+D and Ctrl+Alt+Up/Down are explicit requests, so hitting a
document edge or an absent next occurrence should not look like a dropped
shortcut.

- **IDE note:** failed multi-cursor expansion now reports `No word or next
  occurrence for multi-cursor`, `No line above for another caret`, or `No line
  below for another caret` while preserving existing return codes. The ABI
  regression proves successful caret additions stay quiet and only edge failures
  add feedback.

L384. Navigation history should be armed by successful navigation, not attempts.
If Go to Definition fails, Jump Back should not be primed with the current
cursor position, and invoking Jump Back with no target should explain that the
history slot is empty.

- **IDE note:** `go_to_definition` now returns the definition request result to
  Mighty, and both F12 and palette Go to Definition only write the one-slot
  jump-back target after a real hit, preserving any older target across failed
  attempts. Ctrl+Minus and the Jump Back command report `No previous location`
  when no target is available. The regression verifies the predefined toast used
  by Mighty's scalar dispatch path.

L385. Empty undo/redo stacks are command outcomes, not no-ops. Undo and redo are
explicit keyboard and palette actions, so an empty history stack or read-only
preview should explain why the buffer did not change.

- **IDE note:** live-model `mui_ed_undo` and `mui_ed_redo` now toast `Nothing to
  undo`, `Nothing to redo`, or read-only preview warnings on misses while keeping
  successful history moves quiet. The regressions cover empty stacks, read-only
  binary previews, and the existing successful undo/redo round-trip.

L386. Fold commands need semantic no-op feedback. Fold toggles and Fold/Unfold
All are explicit commands, so a buffer with no foldable ranges, a cursor outside
any block, an already-folded document, or an already-unfolded document should not
look like a swallowed shortcut.

- **IDE note:** `mui_fold_dispatch` now returns `0` and reports `No foldable
  block at cursor`, `No foldable blocks`, `All foldable blocks already folded`,
  or `No folded blocks to unfold` for semantic misses while preserving quiet
  successful fold changes. The regression covers empty documents, success paths,
  and repeated all-document fold commands.

L387. Overlay feedback must yield to reserved chrome. Toast cards are useful
only when they explain an outcome without covering the sidebar, bottom dock, or
right-side drawers that provide the next action.

- **IDE note:** toast card width now honors the actual safe lane after left and
  right reserves instead of forcing a 180px minimum into cramped windows. Cards
  shrink when a valid lane remains and are skipped from draw/hit-testing when no
  safe lane exists, while staying queued for expiry or a later wider layout. The
  regressions cover both compact shrink and over-reserved hide behavior.

L388. Source-control inspection commands should explain empty results. Opening a
diff is a deliberate action; when there is no file, no selected row, no git
repository, or no parsed diff, the IDE should report the reason instead of
leaving the user to infer whether the click registered.

- **IDE note:** `mui_diff_open` and `mui_diff_open_row` now preserve their
  existing `0` return codes for misses but add targeted toasts for missing active
  files, invalid SCM rows, missing repository roots, and clean/no-diff files.
  Empty diff results close any stale diff view so the editor does not retain an
  old inspection surface after a no-op request.

L389. Signature-help requests need the same missing-target feedback as other
language navigation commands. A shortcut can legitimately produce no signature,
but an untitled buffer cannot be sent to the language server as a stable file.

- **IDE note:** `mui_sig_request` now preserves its `0` return code for unsaved
  buffers while showing `Save the file before signature help`, matching hover,
  definition, peek, and formatting feedback for commands that require a saved
  file path.

L390. Recent-workspace activation should distinguish a missing row from a stale
folder. Both are legitimate no-op outcomes, but the user needs to know whether
the selection disappeared or the saved folder no longer exists.

- **IDE note:** `mui_ws_open_recent` now reports `No recent folder selected` for
  negative or out-of-range rows while keeping stale-folder pruning on
  `Recent folder missing: ...`. The open-operation toast grouping treats both as
  workspace-open feedback, so repeated open attempts collapse predictably.

L391. Clickable output panels should clear stale targets and explain failed
jumps. A run-output click can miss because there is no row, the row has no
location, or the diagnostic points at a file that no longer exists.

- **IDE note:** `mui_run_click_row` now resets the pending click target before
  resolving a row and reports `No run output row selected`, `Run output row has
  no file target`, or `Run target missing: ...` for misses. Successful diagnostic
  clicks keep the existing open-tab and jump-target behavior.

L392. Duplicate entry points for the same action should share no-op language.
Welcome recent folders and workspace recent folders both express "open this
saved workspace"; missing selections should not feel like different failures.

- **IDE note:** `mui_welcome_open_folder` now reports `No recent folder selected`
  for negative or out-of-range Welcome rows, matching `mui_ws_open_recent`.
  Stale Welcome folders still flow through the shared recent-folder opener so
  pruning and `Recent folder missing: ...` behavior stay centralized.

L393. Test-result jump commands should clear stale targets before resolving the
next row. A miss should not leave the prior successful jump readable through the
ABI, and the panel should explain whether no row or no file target was available.

- **IDE note:** `mui_test_open_row` now resets its pending click target up front
  and reports `No test result row selected`, `Test result row has no file target`,
  or `Test target missing: ...` on failed jumps. Successful result rows keep the
  existing open-tab and jump behavior.

L394. Agent run commands should explain missing program context. Running Agents
is an intentional command; if no file-backed program is active, returning `0`
without feedback makes the header action feel inert.

- **IDE note:** `mui_agents_run` now reports `Open a file before running Agents`
  when there is no active file path. The toast is grouped as Agents feedback so
  repeated attempts refresh the same operation instead of stacking unrelated
  notifications.

L395. Agent topology jumps need the same miss contract as other navigators.
Clicking an empty Agents row, a section header, or a stale source-backed node
should not preserve an old jump target or fail without visible feedback.

- **IDE note:** `mui_agents_open_node` now clears stale click targets before
  resolving a row and reports `No agent node selected`,
  `Agents node has no file target`, or `Agents target missing: ...` for failed
  topology jumps. Successful source nodes keep the existing open-tab and
  cursor-jump behavior.

L396. Definition target openers should validate their cached navigation state.
Even when definition requests report misses, a follow-up open command can still
be invoked against empty or stale state and should explain the miss itself.

- **IDE note:** `mui_def_open_target` now reports
  `No definition target selected` when no target is cached, and clears stale
  cached targets with `Definition target missing: ...` instead of opening a
  non-existent source path as a tab.

L397. Breadcrumb dropdown acceptance should not fail silently after closing the
menu. A stale row or disappeared sibling file is a user-visible navigation miss,
not just an internal `-1`.

- **IDE note:** `mui_crumb_menu_accept` now reports
  `No breadcrumb menu open`, `No breadcrumb row selected`,
  `Breadcrumb file no longer listed`, `Breadcrumb symbol unavailable`, or
  `Breadcrumb target missing: ...` for failed breadcrumb jumps while keeping
  successful file and symbol jumps unchanged.

L398. Source-control row navigation should explain stale status state. SCM rows
are cached snapshots of Git status, so invalid rows, missing repo roots, and
deleted files must not fail as silent `-1` returns.

- **IDE note:** `mui_scm_open_row` now reports
  `No source control row selected`, `Source control root missing`, or
  `Source control target missing: ...` for failed Source Control file jumps.
  Successful changed-file rows still open the file as before.

L399. Search-result jumps should treat stale results as navigation misses.
Project-wide search results are a cached snapshot; files can be deleted or
results can go stale before the user clicks a match.

- **IDE note:** `mui_search_open` now reports `No search result selected`,
  `Search result file no longer listed`, or `Search target missing: ...` for
  failed result jumps. Successful result rows still open the file, move the
  cursor to the match, and scroll it into view.

L400. Welcome recent-file picks should explain empty selections. The Welcome
recent list can be opened by menu actions and keyboard paths, so negative or
out-of-range indices are user-visible misses rather than internal no-ops.

- **IDE note:** `mui_welcome_open_recent` now reports `No recent file selected`
  when the Welcome recent-file picker has no valid row to open. Stale files
  still use `Recent file missing: ...`, prune the missing recent, and keep the
  Welcome surface open.

L401. Branch-switch accepts need visible feedback when no picker is open. Branch
switching is driven by keyboard and mouse overlay routes, so accepting after the
overlay has already closed should not disappear as a bare `0` return.

- **IDE note:** `mui_branch_accept` now reports `No branch picker open` when the
  accept command is routed without an active picker. Existing checkout/create
  failures still surface git's own error text, and active empty pickers continue
  to route Enter into the Create Branch flow.

L402. Source-control stage toggles should explain stale row targets. SCM rows are
cached snapshots, and the stage button can be invoked after the row disappeared
or before a repository root is available.

- **IDE note:** `mui_scm_toggle_stage` now reports `No source control row
  selected`, `Source control root missing`, or `Source control stage/unstage
  failed` instead of silently returning `0` for failed stage-button actions.

L403. Bulk SCM actions should not confuse missing repositories with clean state.
Palette and header commands can be invoked from any workspace, so `Stage All`,
`Unstage All`, and `Commit` need to explain when git is unavailable for the
workspace rather than implying there was merely nothing to do.

- **IDE note:** `mui_scm_stage_all`, `mui_scm_unstage_all`, and
  `mui_scm_commit` now report `Not a git repository` when no repository root can
  be discovered. Existing `Nothing to stage`, `Nothing to unstage`, and
  `Nothing to commit` messages are reserved for real repositories.

L404. Inline hunk feedback belongs to the git operation family. Hunk staging is
part of the same source-control workflow as branch, stage-all, unstage-all, and
commit commands, so old hunk toasts should be replaced by newer git outcomes.

- **IDE note:** `No hunk selected`, `Staged hunk`, `Unstaged hunk`, and
  `Hunk apply failed: ...` now share the Git toast replacement key with the rest
  of the SCM command feedback. Repeated hunk actions no longer leave stale git
  status cards stacked beside the latest result.

L405. Diff-open feedback belongs to the same source-control toast lane. Opening
a diff from the editor or SCM panel is still a git workflow, even when the
result is an empty diff or a missing target.

- **IDE note:** `No file to diff`, `No source-control row`,
  `No git repository for diff`, and `No diff for ...` now share the Git toast
  replacement key. Repeated diff-open misses replace older source-control cards
  instead of stacking stale explanations beside the latest outcome.

L406. SCM row-target feedback should not be treated like file-open feedback.
Stage buttons and stale SCM rows are part of source control, so their missing
row, missing root, and missing target explanations need to replace git status
cards rather than unrelated open-dialog cards.

- **IDE note:** `No source control row selected`, `Source control root missing`,
  and `Source control target missing: ...` now share the Git toast replacement
  key. Source-control row misses now collapse with stage, commit, hunk, branch,
  and diff feedback instead of occupying the Open toast lane.

L407. Terminal lifecycle feedback needs its own replacement lane. Opening,
closing, retrying, and failing to spawn the integrated terminal are one workflow,
and stale terminal state cards should not stack as the user toggles the panel.

- **IDE note:** `Terminal opened`, `Terminal closed`,
  `Terminal is already closed`, and `Terminal failed to open` now share a
  terminal toast replacement key. Repeated terminal actions keep the latest
  lifecycle state visible without leaving old terminal cards behind.

L408. Blame feedback belongs with source-control status, not generic alerts.
Blame is a git view layered into the editor gutter, so unavailable-file,
untracked-file, and blame-enabled outcomes should collapse with other SCM
feedback.

- **IDE note:** `No file to blame`, `No blame (file not tracked?)`, and
  `Blame on ... toggle to hide` now share the Git toast replacement key.
  Repeated blame toggles and follow-up git commands replace stale blame cards
  with the latest source-control outcome.

L409. Code-folding no-ops should collapse as one editor workflow. Palette and
keyboard fold commands can be repeated quickly while the cursor or document
cannot produce a fold, so each new fold explanation should replace the previous
one.

- **IDE note:** `No foldable block at cursor`, `No foldable blocks`,
  `All foldable blocks already folded`, and `No folded blocks to unfold` now
  share a Fold toast replacement key. Repeated fold/unfold commands keep only
  the latest folding outcome visible.

L410. Replace outcomes should share one replacement lane. In-file replace and
project replace-all are the same user intent at different scopes, and repeated
attempts should replace stale replacement cards with the latest result.

- **IDE note:** `Enter text to replace`, read-only replace warnings,
  `No matches to replace`, `No project replacements`, and `Replaced ...
  occurrence...` messages now share a Replace toast replacement key. Replace
  retries keep the freshest result visible across file and project scopes.

L411. Undo/redo feedback is one history workflow. Users often press undo and
redo repeatedly at stack boundaries, or while a read-only preview is focused, so
history explanations should replace each other instead of stacking.

- **IDE note:** `Nothing to undo`, `Nothing to redo`, `Undo is unavailable in
  read-only previews`, and `Redo is unavailable in read-only previews` now share
  a History toast replacement key. Repeated undo/redo commands keep only the
  latest history state visible.

L412. Multi-cursor edge feedback should collapse as one caret workflow. Adding
the next occurrence, adding a caret above, and adding a caret below are related
multi-cursor commands, and their boundary explanations should replace each
other.

- **IDE note:** `No word or next occurrence for multi-cursor`,
  `No line above for another caret`, and `No line below for another caret` now
  share a MultiCursor toast replacement key. Repeated multi-cursor edge commands
  keep only the latest caret-placement explanation visible.

L413. Language-service misses should share one code-intelligence lane. Completion,
hover, and Go to Definition all describe the same editor intelligence surface,
so their save-first, no-result, and missing-target feedback should not stack as
separate stale cards.

- **IDE note:** `No completions available`, hover save/no-info messages,
  Go to Definition, Peek Definition, and signature-help save-first warnings,
  plus `No definition found`, `No definition target selected`, and
  `Definition target missing: ...` now share a CodeIntel toast replacement key.
  Repeated language-service commands keep the latest intelligence outcome
  visible.

L414. Format command feedback should use one formatting lane. Missing file paths,
unsupported file types, formatter failures, and successful formatting are all
outcomes of the same command and should replace one another as users retry.

- **IDE note:** `Save the file before formatting`,
  `Format is available for Mighty files`, `Format failed`, and
  `Formatted document` now share the Format toast replacement key. Repeated
  format attempts keep only the latest formatting outcome visible.

L415. Symbol-rename misses are code-intelligence feedback. F2 rename uses the
same language-service surface as completion, hover, signature help, and
definition lookup, so its no-target state should collapse with those outcomes.

- **IDE note:** `No rename target` now shares the CodeIntel toast replacement
  key. Repeated language-intelligence commands, including symbol rename misses,
  keep the latest editor intelligence outcome visible.

L416. Name validation feedback is one input workflow. When a user retries an
invalid project, file, folder, or rename value, each validation failure is a
new state of the same text-entry task rather than a separate notification.

- **IDE note:** Project-name validator failures, including empty names,
  path-separator errors, invalid traversal names, bad first characters, and
  unsupported characters, now share a NameInput toast replacement key. Repeated
  invalid-name submissions keep only the latest validation reason visible.

L417. Reload and dirty-close feedback belongs to tab lifecycle. Reloading,
reverting, refusing to reload a dirty tab, and asking the user to review
unsaved changes are all states of tab management, so they should replace stale
tab-operation toasts instead of stacking around the editor.

- **IDE note:** `Review ... unsaved ...`, `Save or discard changes before
  reloading`, `Reloaded ...`, `Reverted ...`, reload/revert failures,
  `No file-backed tab to ...`, and no-saved-tabs close messages now share the
  Tab toast replacement key. Repeated tab lifecycle commands keep the latest
  tab state visible.

L418. Debug command feedback should collapse by session workflow. Starting,
continuing, stepping, pausing, stopping, restarting, and setting breakpoints are
one debugger surface, so unavailable-state explanations should replace each
other as the user probes controls.

- **IDE note:** Debug start failures, already-running notices, unavailable
  continue/pause/step/stop/restart messages, and breakpoint save-first warnings
  now share a Debug toast replacement key. Repeated debugger commands keep the
  latest session or breakpoint state visible.

L419. Test runner lifecycle feedback is part of the testing lane. Missing test
targets, failed starts, idle stops, and final pass/fail summaries are all states
of one run workflow, so they should not stack as separate notifications.

- **IDE note:** Test start prompts, `Test run failed to start: ...`,
  `No test run to stop`, result-row navigation feedback, target-missing
  warnings, and numeric pass/fail summaries now share the Test toast
  replacement key. Repeated testing commands keep only the latest run state
  visible.

L420. Agents topology navigation feedback should share the Agents lane. Opening
rows, header rows, missing source targets, and run-without-file prompts all
belong to the same agent-system panel workflow.

- **IDE note:** `No agent node selected`, `Agents node has no file target`,
  `Agents target missing: ...`, and `Open a file before running Agents` now
  share the Agents toast replacement key. Repeated Agents panel actions keep
  the latest topology/run state visible.

L421. Native create-pickers need the same creation feedback lanes as typed
prompts. Workspace-bound file/folder picker rejects and project-folder prepare
failures are still outcomes of create commands, not standalone warnings.

- **IDE note:** `Choose a file inside the workspace` now shares the CreateFile
  toast replacement key, `Choose a folder inside the workspace` shares
  CreateFolder, and `Could not prepare/inspect folder: ...` shares
  CreateProject. Repeated creation attempts keep the latest create outcome
  visible across native and typed flows.

L422. Window and focus-mode feedback is layout state. Minimize, maximize,
restore, and Zen toggle messages describe the active chrome shape, so they
should replace stale layout cards instead of stacking beside pane feedback.

- **IDE note:** `Window minimized`, `Window maximized`, `Window restored`,
  `Zen mode on ...`, and `Zen mode off` now share the Layout toast replacement
  key. Repeated chrome changes keep the latest window/focus state visible.

L423. Core editor conventions still matter after advanced features land.
Multi-cursor, snippets, and command-palette flows do not replace baseline muscle
memory; `Ctrl+A` should select the whole active document without dirtying it.

- **IDE note:** The shim text model now exposes `select_all`, Mighty routes
  `Ctrl+A` to `mui_ed_select_all`, and the shortcut docs list Select All as a
  first-class editing command. Empty documents remain clean no-op selections.

L424. Line selection is another baseline editor convention, not a hidden model
helper. If the text model can select a line, the IDE should expose it through a
normal editing chord and document it beside the other motion commands.

- **IDE note:** `mui_ed_select_line` now exposes the shim model's current-line
  selection, Mighty routes `Ctrl+L` to it, and the shortcut docs list Select
  Current Line. The operation is pure selection motion and does not dirty tabs.

L425. Clipboard editing has to be first-class, not only file-path copying.
Selection-aware copy/cut/paste is table-stakes editor muscle memory; no-selection
copy/cut should operate on the current line so keyboard editing stays fast.

- **IDE note:** `Ctrl+C`, `Ctrl+X`, and `Ctrl+V` now route to editor clipboard
  ABIs. Copy/cut use the active selection or current line, paste replaces the
  selection, and clipboard feedback shares the Copy toast lane.

L426. Selection replacement is part of the edit contract, not a shortcut bonus.
Once Select All, Select Line, and clipboard editing exist, typed text, Enter,
Backspace, and Delete should treat the active selection as the edit target.

- **IDE note:** `TextModel::begin_edit` now deletes active selections before
  text mutations, Backspace/Delete remove selections, smart insert falls back to
  plain replacement when a selection is active, and multi-caret replacement
  treats the selection start as the edit origin.

L427. Tab should be an editor indentation command when no higher-priority Tab
workflow is active. Snippet navigation, snippet expansion, and ghost acceptance
can own Tab first, but the fallback must indent or outdent code instead of
inserting a raw tab byte.

- **IDE note:** `TextModel` now exposes configured-space indent and outdent for
  the current line or selected line range, the ABI exports `mui_ed_indent` and
  `mui_ed_outdent`, and the Mighty key ladder routes plain Tab/Shift+Tab there
  after snippet and ghost handlers decline the key.

L428. Borderless-window resize zones must not steal visible tab targets. A
forgiving top resize band is useful on empty chrome, but tab clicks at the top
edge still need to reach the IDE so switching tabs never feels intermittent.

- **IDE note:** The shim mouse prefilter now lets real tab-slot hits pass
  through before applying resize-edge interception, while empty caption chrome
  continues to support OS drag and resize behavior.

L429. Word motion should come with word deletion. Once Ctrl+Left and Ctrl+Right
exist, Ctrl+Backspace and Ctrl+Delete are the matching edit commands users
expect for fast keyboard cleanup.

- **IDE note:** `TextModel` now deletes to the previous or next word boundary,
  including selection replacement and multi-caret edits. The ABI exports single
  and multi-caret word-delete routes, and Mighty maps Ctrl+Backspace/Ctrl+Delete
  through them in the editor key path.

L430. Delete Line should be a first-class edit command, not a hidden Cut mode.
The model already removed the current line when Cut ran without a selection, but
that overloaded clipboard semantics and left command-palette users without the
expected fast cleanup action.

- **IDE note:** `mui_ed_delete_current_line` now exposes the model operation
  directly, Mighty routes Ctrl+Shift+K before the Ctrl+K hover branch, and the
  command palette lists `Edit: Delete Line` with the same undo/dirty handling as
  other destructive text edits.

L431. Join Line is a distinct editing primitive, not just End plus Delete.
Keyboard-heavy editing needs a command that removes the next line break while
cleaning up indentation and spacing at the join boundary.

- **IDE note:** `TextModel::join_line` now joins the current line with the next
  line, trims leading indentation from the next line, inserts one separator
  space when two text runs would otherwise touch, and returns a changed flag for
  no-op-safe dirty handling. Mighty routes Ctrl+J and the command palette lists
  `Edit: Join Line`.

L432. Document-boundary motion belongs with word and line motion. Home/End alone
only handles one line; real editing muscle memory also expects Ctrl+Home and
Ctrl+End to jump to file start/end, with Shift variants extending selection.

- **IDE note:** `TextModel` now exposes document-start/end motion for primary
  and multi-caret state. The existing Mighty Home/End key arms branch internally
  on Ctrl, so Ctrl+Home/Ctrl+End and Ctrl+Shift+Home/Ctrl+Shift+End work without
  adding more top-level key-ladder arms.

L433. Word selection should be visible outside multi-cursor setup. Ctrl+D
already selected the word on its first press, but hiding Select Word behind a
multi-cursor description made a useful selection command hard to discover.

- **IDE note:** the command palette now lists `Edit: Select Word` and dispatches
  through the existing `mui_ed_select_word` ABI. The shortcut reference now
  documents Ctrl+D as `Select word / add caret at next occurrence`.

L434. Daily edit commands should be palette-reachable, not shortcut-only.
Duplicate line/selection and line movement were fully implemented, tested, and
documented as keyboard shortcuts, but command-palette users could not discover
or invoke them from the same workflow as delete line, join line, and select word.

- **IDE note:** the command palette now lists `Edit: Duplicate Line or Selection`,
  `Edit: Move Line Up`, and `Edit: Move Line Down`. Mighty dispatches those
  commands through the existing `mui_ed_duplicate`, `mui_ed_move_lines_up`, and
  `mui_ed_move_lines_down` ABI calls with the same undo, dirty-tab, and ghost-text
  handling used by their keyboard shortcuts.

L435. Baseline selection and comment commands should not be shortcut-only.
Select All, Select Line, and Toggle Line Comment are core editing actions users
expect to find by name, especially when learning shortcuts through the palette.

- **IDE note:** the command palette now lists `Edit: Select All`,
  `Edit: Select Line`, and `Edit: Toggle Line Comment`. The selection commands
  dispatch through the clean `mui_ed_select_all` / `mui_ed_select_line` ABI calls,
  while comment toggle keeps the existing undo-record and dirty-tab behavior from
  the Ctrl+/ key path.

L436. Clipboard editing should be command-palette visible. Copy, Cut, and Paste
were real editor operations with selection-or-line semantics, but palette users
could only discover file-path copy commands, not text clipboard commands.

- **IDE note:** the command palette now lists `Edit: Copy Selection or Line`,
  `Edit: Cut Selection or Line`, and `Edit: Paste`. Mighty dispatches through
  the existing `mui_ed_copy`, `mui_ed_cut`, and `mui_ed_paste` ABI calls; Cut and
  Paste keep the shortcut path's undo record and dirty-tab update only when the
  ABI reports a real edit.

L437. Word deletion should be discoverable as an edit command. Ctrl+Backspace
and Ctrl+Delete are fast keyboard paths, but palette users need named commands
for the same previous-word and next-word editing operations.

- **IDE note:** the command palette now lists `Edit: Delete Previous Word` and
  `Edit: Delete Next Word`. Mighty dispatches through the existing multi-caret
  `mui_ed_delete_word_left_multi` and `mui_ed_delete_word_right_multi` ABI calls,
  preserving the shortcut path's undo, dirty-tab, and ghost-text handling.

L438. Indent and outdent should be named edit commands, not only Tab fallback
behavior. Snippets, completions, and ghost text can own Tab first, so palette
commands give users a direct route to the underlying line indentation operation.

- **IDE note:** the command palette now lists `Edit: Indent Line or Selection`
  and `Edit: Outdent Line or Selection`. Mighty dispatches through `mui_ed_indent`
  and `mui_ed_outdent`, preserving the Tab fallback's undo record, no-op-safe
  dirty-tab update, and ghost-text refresh behavior.

L439. Cursor navigation should be palette-reachable too. Word-wise movement and
document-boundary jumps are not edits, but they are still named editor actions
that keyboard-first users expect to discover and invoke without memorizing every
chord.

- **IDE note:** the command palette now lists cursor movement commands for
  previous/next word and document start/end. Mighty dispatches through the
  existing multi-caret movement ABI with selection extension disabled, matching
  Ctrl+Left, Ctrl+Right, Ctrl+Home, and Ctrl+End while dismissing stale ghost text.

L440. Line start/end navigation should be visible beside document navigation.
Smart Home and End are baseline cursor commands, and hiding them while exposing
document-boundary commands leaves the palette navigation set oddly incomplete.

- **IDE note:** the command palette now lists `Edit: Move Cursor to Line Start`
  and `Edit: Move Cursor to Line End`. Mighty dispatches through
  `mui_ed_home_smart_multi` and `mui_ed_move_ext_multi(...dir_end...)`, and the
  shortcut table now documents Home/End next to Ctrl+Home/Ctrl+End.

L441. Multi-cursor commands should be palette-reachable. The command palette
exposed Select Word, but not the actions that build and clear multiple cursors.

- **IDE note:** the command palette now lists `Edit: Add Cursor to Next
  Occurrence`, `Edit: Add Cursor Above`, `Edit: Add Cursor Below`, and
  `Edit: Collapse Multiple Cursors`. Mighty dispatches through the existing
  multi-caret ABI, preserving Ctrl+D, Ctrl+Alt+Up/Down, and Esc behavior without
  adding new editor key-ladder arms.

L442. Find-and-replace belongs beside find in the command palette. Ctrl+H was
documented and implemented, but keyboard-first users had no discoverable palette
entry for opening the in-file replace bar.

- **IDE note:** the command palette now lists `Find & Replace` with `Ctrl+H`.
  Mighty dispatches it through `mui_replace_open`, matching the existing chord
  path by opening the replace bar, entering replace mode, and clearing stale
  find navigation state.

L443. Language-service actions need palette entries, not just chords.
Signature Help, Rename Symbol, and Code Actions were implemented and documented
as shortcuts, but hidden from command search.

- **IDE note:** the command palette now lists `Show Signature Help`, `Rename
  Symbol`, and `Code Actions`. Mighty routes them through the same cursor-local
  ABI paths as Ctrl+Shift+Space, F2, and Ctrl+., preserving existing LSP/no-target
  feedback and overlay state.

L444. AI editor actions should be discoverable from the palette. The AI panel
was visible as a view command, but inline ask and forced ghost completion were
shortcut-only actions.

- **IDE note:** the command palette now lists `AI: Inline Ask` and `AI: Force
  Ghost Completion`. Mighty opens the same Ask AI prompt as Ctrl+I and calls
  `mui_ghost_force` for the same explicit inline-completion path as Alt+\.

L445. Navigation surfaces should be reachable from each other. Universal
Quick-Open is a core files/commands/symbols/line-jump surface, but it was only
available by Ctrl+P or the top-bar command center.

- **IDE note:** the command palette now lists `Quick Open` with `Ctrl+P`. The
  dispatcher opens the same `mui_quickopen_open` overlay and resets transient
  panel focus so selecting the command hands off cleanly to the quick-open UI.

L446. Visible shortcut chips should be searchable, not just decorative.
Command palette rows showed keybindings, but filtering only considered command
labels. That made `Ctrl+P`, `F5`, or remembered shortcut fragments useless when
the user forgot the command name.

- **IDE note:** palette filtering now scores command labels first, then
  keybinding text. Shortcut matching accepts both literal forms like `Ctrl+P`
  and normalized forms like `ctrl p` / `ctrlp`, including slash-separated
  alternatives such as `Ctrl+1 / Ctrl+2`.

L447. Shortcut alternatives should render as alternatives, not accidental key
names. The command palette drew shortcut chips by splitting only on `+`, so a
binding like `Ctrl+1 / Ctrl+2` could become a malformed `1 / Ctrl` key pill.

- **IDE note:** palette rows now tokenize shortcut text into key pills plus a
  lightweight `/` separator. `Ctrl+/` stays a real slash key, while
  `Ctrl+1 / Ctrl+2` renders as two clear shortcut alternatives.

L448. Cross-surface command search should not drop the hint that made the match.
Quick Open command mode reused palette filtering, so shortcut searches such as
`>ctrl p` could find `Quick Open`, but the row only showed the command name.

- **IDE note:** Quick Open command rows now carry the palette keybinding into
  the secondary row text. Command-mode results stay dispatch-compatible while
  showing the shortcut that matched, e.g. `Quick Open` with `Ctrl+P`.

L449. Cross-surface command rows need fallback context when no shortcut exists.
After Quick Open command mode started showing shortcuts, commands without
keybindings still looked bare even though the command palette had useful
descriptions for them.

- **IDE note:** Quick Open command mode now uses the palette shortcut as the
  secondary row text when present, otherwise it falls back to the palette's
  static command description. This keeps no-shortcut commands like
  `File: Open Recent` informative without duplicating description copy.

L450. Search-result rows need measured text budgets before adding richer
secondary copy. Quick Open command rows gained fallback descriptions, but the
renderer still drew row names and secondary text without fitting them to the
card width.

- **IDE note:** Quick Open rows now fit both the primary name and secondary
  path/description text against the actual row text budget before drawing. Long
  command descriptions and deep paths ellipsize instead of spilling past the
  overlay edge.

L451. Command palette search fields need the same measured budget as result
rows. The palette command input drew full pasted queries and positioned the
caret from an approximate character advance, so long text could run underneath
the prompt pill.

- **IDE note:** The command palette now measures and fits the search field text
  against the available space before the `>_` pill, then places the caret from
  the measured rendered width. Long queries ellipsize before colliding with the
  right-side command prompt.

L452. Every overlay search field needs a real right-side boundary. The keyboard
shortcuts overlay already fitted row titles and footer text, but its filter
field still drew long queries until they reached the close button.

- **IDE note:** The shortcuts overlay now fits the filter text against the close
  button boundary and derives the caret position from the measured fitted text.
  Long shortcut searches ellipsize before crossing into the overlay controls.

L453. Branch picker text needs pixel budgets, not character-count approximations.
The branch switcher fitted neither its filter/create input nor branch names
against the close button and current/remote badges.

- **IDE note:** Branch picker header input now fits measured text before the
  close button and places the caret from rendered width. Branch row names fit
  against the badge gutter, so long local or remote branch names ellipsize
  before touching `current` or `remote` labels.

L454. Inline rename inputs need measured code-font fitting too. Rename Symbol
drew the full proposed identifier and placed the caret from a fixed character
advance, so a long pasted name could cross the input border.

- **IDE note:** Rename Symbol now fits the editable name with the same measured
  code-font path used elsewhere and clamps the caret inside the input padding.
  Long rename targets ellipsize instead of bleeding through the inline card.

L455. Settings controls should reserve measured value widths. The Settings panel
used character-count estimates for numeric values and theme chips, which made
left-side label budgets depend on rough guesses instead of rendered text.

- **IDE note:** Settings rows now measure value text and theme-chip width before
  computing the control gutter. Labels and descriptions fit against the real
  right-side controls, keeping the preferences card stable on narrow windows.

L456. Theme picker rows and footers need the same measured fitting as command
surfaces. The picker drew theme names/descriptions raw and right-aligned its
footer tag with a character estimate.

- **IDE note:** Theme picker rows now fit names and descriptions before the
  selected-row check control, and the footer hint fits against a measured
  `Mighty Themes` tag. Narrow picker cards keep row text and footer chrome from
  colliding.

L457. Breadcrumb dropdowns need measured popup and row widths. The breadcrumb
menu sized itself from character counts and truncated labels by estimated
advance, which made long file or symbol names unreliable in a proportional UI
font.

- **IDE note:** Breadcrumb menus now measure item labels plus symbol depth to
  choose popup width, then fit each row label against the actual remaining row
  budget. Deep symbol names and long sibling filenames ellipsize before the card
  edge.

L458. Measured rendering changes must keep hit-testing geometry in sync.
Settings values moved to measured drawing, but numeric row click handling still
computed the minus button from a character-count value width.

- **IDE note:** Numeric Settings rows now share a fixed value slot for drawing,
  label budgeting, and mouse hit-testing. The rendered value is centered by
  measured width inside that bounded slot, so `-` and `+` click targets match
  what users see.

L459. Footer shortcut hints should use measured text, not fixed advances.
Quick Open had measured rows and search input, but its footer still advanced
shortcut labels and right-aligned the surface tag from character counts.

- **IDE note:** Quick Open now measures footer key pills, labels, and the
  `Quick Open` tag before drawing. The footer stays aligned with proportional UI
  text just like the newer picker footers.

L460. Command palette footer hints need measured text like Quick Open. Command
Palette still advanced footer key pills, labels, and the right-side tag from
character counts after Quick Open moved to measured footer layout.

- **IDE note:** The command palette now measures footer key pills, labels, and
  the `Mighty Command Palette` tag before drawing, keeping command footer chrome
  aligned with proportional UI text.

L461. Overlay header chips need measured text too. Quick Open had measured rows,
search input, and footer hints, but its mode chip and result count in the header
still used fixed character advances.

- **IDE note:** Quick Open now measures the mode label before sizing and
  centering the header chip, and right-aligns the result count from measured UI
  text. The top chrome follows the same proportional-font contract as the row
  and footer surfaces.

L462. Shortcut-editor key pills need measured text, not estimated advances. The
Shortcuts overlay measured its search and title text, but key pills and the
selected-row remap/fixed affordance still used fixed per-character widths.

- **IDE note:** Shortcut rows now measure each key token before sizing and
  centering keyboard pills, and reserve selected-row affordance space from the
  measured remap/fixed label. Long modifier names and proportional glyphs keep
  the row title gutter accurate.

L463. Command palette keybinding chips need measured labels too. The command
palette had measured search text, rows, and footer hints, but row shortcut pills
still sized and centered key labels from fixed per-character advances.

- **IDE note:** Command palette shortcut chips now measure each key token before
  sizing pills and centering labels. Alternative shortcuts, long modifier names,
  and narrow glyphs reserve the actual rendered space before row titles are fit.

L464. Header status pills need measured text even when labels are compact. The
AI Copilot header compacted the model id to a short badge, but still sized the
badge pill from a fixed per-character estimate.

- **IDE note:** The AI model badge now measures its rendered UI label before
  sizing the header pill. Future model aliases can vary in glyph width without
  crowding the titlebar close affordance or leaving uneven padding.

L465. Completion popup chrome must share measured row budgets. Completion row
names were fitted against the kind label, but the badge letter and kind label
positions still came from fixed character advances.

- **IDE note:** Completion rows now measure the badge glyph and right-side kind
  label before centering or reserving space. Candidate names and signatures fit
  against the actual rendered kind metadata in the autocomplete popup.

L466. Problems panel counters and right clusters need measured widths. The
Problems panel fit messages, but still advanced the header counters and row
location/code cluster from fixed per-character estimates.

- **IDE note:** Problems now measures the header label/counts plus row
  location/code labels before laying out the message budget. Diagnostic messages
  fit against the actual rendered right-side metadata in both compact and wide
  panel modes.

L467. Inline diff action chrome should measure labels before reserving space.
The diff view fit code lines, but its header summary and hunk stage/unstage
button still used fixed character estimates for right alignment.

- **IDE note:** Inline diff now measures the header summary and hunk action
  label before right-aligning those surfaces. Hunk headers reserve the rendered
  button width, keeping section text out from under stage/unstage actions.

L468. Debug variable rows need measured name and type widths. The Debug panel
measured header and stack-frame text, but variable rows still placed `=` and the
right-side type label from fixed character estimates.

- **IDE note:** Debug variable rows now measure the displayed variable name
  before placing `=`, and measure type labels before right-aligning them. Narrow
  and wide identifiers keep value text and type metadata aligned with rendered
  UI text.

L469. Markdown preview code chrome needs measured widths too. Markdown preview
wrapped prose conservatively, but code-block language tags and inline-code chip
backgrounds still used fixed character estimates.

- **IDE note:** Markdown preview now measures code-block language tags and
  inline-code chip text before positioning or sizing their chrome. Code badges
  and inline code backgrounds align with the actual rendered glyphs.

L470. Web Playground header chrome should measure UI text before fitting. The
Web panel drew UI-family package names, mode labels, URL pills, and Stop labels
from fixed character estimates, even though those glyphs are proportional.

- **IDE note:** Web Playground now measures Stop labels, URL pill text, package
  names, and mode labels before sizing or ellipsizing header chrome. The
  clickable URL target and drawn text now share measured layout budgets.

L471. Bottom-panel output clipping should share the measured code-text fitter.
Web Playground output rows still clipped from a fixed monospace estimate even
after Run output moved to measured code-font fitting.

- **IDE note:** Web Playground output rows now use the same measured
  `fit_code_text` path as Run output before drawing command echoes, errors, and
  normal output. Long server/build lines fit the visible dock width by rendered
  glyph width instead of character count.

L472. Compact summary choices need measured budgets before fallback. The Agents
sidebar selected full, compact, or count-only summary text from character
counts, then applied measured fitting afterward.

- **IDE note:** Agents sidebar summaries now choose the most informative form
  that fits the actual rendered UI-font width before final ellipsizing. Compact
  drawers keep readable counts without relying on proportional text estimates.

L473. Debug variable values must reserve measured type metadata. Debug variable
rows measured names before placing `=`, but long values still clipped by a
character estimate and could run into right-aligned type labels.

- **IDE note:** Debug variable rows now fit names and values with measured
  UI-font budgets, and values reserve the rendered type-label width before
  drawing. Long runtime values stop before type metadata instead of crowding it.

L474. Debug Console rows should use the same measured fitting as Debug variables.
Debug Console output lines still clipped from a fixed character estimate even
after variable rows moved to rendered-width budgets.

- **IDE note:** Debug Console rows now fit output and error lines with measured
  UI-font width before drawing. Long debugger messages stay inside the sidebar
  panel without relying on per-character estimates.

L475. Inline diff text should use measured code-font budgets like Run/Web
output. Inline diff measured hunk action buttons, but hunk headers and body rows
still clipped from fixed character estimates.

- **IDE note:** Inline diff now fits hunk headers and body text through the
  measured code-font fitter before drawing. Long diff context and changed lines
  reserve actual rendered space before action buttons and the editor edge.

L476. Inline blame annotations need measured UI-font clipping too. Blame labels
sit at the end of editor lines and still used proportional-width character
estimates, so long author/date labels could run past the window edge.

- **IDE note:** Inline blame now fits annotation labels with the measured
  UI-font fitter before drawing. Long author names and commit metadata stay
  inside the editor width without relying on half-font character estimates.

L477. Markdown fenced code should clip by rendered code width. Code blocks in
preview cards still used fixed character counts, even after inline code chips
and language tags moved to measured text widths.

- **IDE note:** Markdown preview code blocks now fit each rendered monospace
  row with measured code-font budgets before drawing. Long fenced-code lines
  stay inside the card padding without relying on global character estimates.

L478. Outline symbol names need measured sidebar budgets. The Outline panel
still truncated rows from a half-font character estimate, so long symbols could
clip or leave inconsistent space in the sidebar.

- **IDE note:** Outline rows now fit symbol names with measured UI-font widths
  before drawing. Long functions, types, and nested symbols stay inside the
  sidebar row budget without relying on approximate character counts.

L479. Search panel text needs measured row budgets end to end. Search fields,
file rows, and match previews still used half-font estimates even though they
draw with proportional UI text and measured highlight rectangles.

- **IDE note:** Search panel inputs, result file paths, preview rows, and
  preview line-number offsets now use measured UI-font widths before drawing.
  Long queries and match previews stay inside the sidebar while highlights stay
  aligned to the rendered text.

L480. Editor chrome should measure rendered labels even when source columns stay
grid-based. Gutter numbers and folded-region pills still used character-count
widths, while the actual labels were drawn through text shaping.

- **IDE note:** Editor gutter numbers and folded-code indicator pills now use
  measured font widths before placement and sizing. Source text remains on the
  monospace grid, but editor chrome no longer relies on approximate character
  multipliers.

L481. Quick Open fuzzy matches should be visible without fixed glyph advances.
Rows ranked by fuzzy indices, but the renderer ignored those indices after
switching to shaped proportional text, leaving matches visually unmarked.

- **IDE note:** Quick Open now overlays matched characters at measured
  proportional prefix positions. Fuzzy matches remain visually highlighted
  without returning to fixed-advance row rendering, and clipped ellipsis/tail
  characters are skipped safely.

L482. Debug variable separators should use measured spacing. Variable rows fit
names and values with measured budgets, but the name budget and `=` offsets
still used a half-font multiplier.

- **IDE note:** Debug variable rows now derive the compact name budget and
  separator/value offsets from measured UI-font text. Values still reserve room
  for type metadata, and the `name = value` spacing follows the rendered font.

L483. Peek preview gutters should measure rendered line numbers. The editor
gutter moved to measured number widths, but Peek Definition's inline preview
still sized its gutter from digit counts and global character cells.

- **IDE note:** Peek Definition now sizes its preview gutter and right-aligns
  line numbers with measured code-font widths. Preview source remains on the
  monospace grid, while the card gutter follows the rendered digits.

L484. Sticky Scroll gutter labels should match editor gutter measurement. Sticky
headers render as code rows, but their line-number labels still used global
character-cell widths after the editor and Peek gutters moved to measured text.

- **IDE note:** Sticky Scroll now right-aligns pinned header line numbers with
  measured code-font widths. The sticky source text remains grid-aligned, while
  the gutter label follows the rendered digits.

L485. Welcome recents should shorten paths with measured text. Recent rows
preserved path roots and tails, but converted pixel budgets to fixed character
counts before drawing proportional UI text.

- **IDE note:** Welcome recents and the Open Recent picker now shorten names and
  paths with measured UI-font widths. Long workspace paths keep useful root and
  tail context without overflowing their row budgets.

L486. AI chat prose wrapping should use measured UI text. The copilot transcript
and composer converted panel width into character counts before rendering
proportional UI text, so wide glyphs could exceed their row budgets.

- **IDE note:** AI transcript prose and the composer now wrap against measured
  UI-font widths, and the send hit-test shares the measured composer geometry.
  Code blocks still wrap on the monospace grid where columns are intentional.

L487. Markdown preview inline wrapping should match rendered pieces. Preview
paragraphs and headings wrapped flattened spans with per-character estimates,
even though plain text, links, italic text, and inline code chips are drawn with
measured shaped text.

- **IDE note:** Markdown preview now wraps inline pieces using measured UI/code
  text widths, including inline-code chip padding. Strike-through and italic
  advances also follow the rendered glyph width instead of a fixed cell guess.

L488. Completion popup geometry should start from measured row content. Rows fit
labels, signatures, and kind metadata with measured text, but the popup itself
still sized from the longest candidate's character count.

- **IDE note:** Completion popups now compute their natural width from measured
  visible row content and footer text, clamp to the viewport, and use the same
  measured geometry for drawing and click hit-tests.

L489. Code-action popup geometry should measure action titles. Code-action rows
already fit their labels with measured text, but the menu width still used the
longest title's character count times a proportional-font guess.

- **IDE note:** Code-action menus now size from measured action-title widths,
  stay clamped to the visible work area, and use the same measured geometry for
  drawing and click hit-tests.

L490. Hover popups should size from measured code text. Hover cards render their
wrapped lines in the code font, but their card width still came from character
count times the global cell width.

- **IDE note:** Hover popup cards now derive width from measured code-font line
  extents plus padding, so the card bounds follow the same glyph shaping used
  for the rendered hover text.

L491. Terminal line insertion/deletion should shift rows, not disappear.
Prompt redraws and lightweight TUIs use `CSI L` and `CSI M` to insert or delete
lines below the cursor without clearing the whole terminal grid. Skipping those
sequences consumes no visible garbage, but leaves later output aligned against
stale rows.

- **IDE note:** The integrated terminal now handles `ESC[nL` and `ESC[nM`,
  defaulting missing counts to one, clamping at the bottom of the grid, and
  blanking the rows vacated by the shift. Parser tests cover row preservation,
  large-count clamping, default counts, and escape consumption.

L492. Terminal viewport scroll commands should move the visible grid. Some
shell redraws and lightweight terminal UIs use `CSI S` and `CSI T` to scroll the
viewport up or down without emitting newlines. Consuming those escapes without
scrolling leaves old rows in place and makes later output appear detached from
the intended terminal state.

- **IDE note:** The integrated terminal now handles `ESC[nS` and `ESC[nT`,
  defaulting missing counts to one, clamping large counts to the visible grid,
  preserving the cursor, and blanking rows introduced by the scroll. Parser
  tests cover both directions, default counts, large-count clamping, and escape
  consumption.

L493. Terminal erase-character must not behave like delete-character. Redraw
code can use `CSI X` to blank a run of cells on the current row while preserving
the text to the right. Skipping it leaves stale characters, while implementing
it as delete-character would shift the row and corrupt aligned prompts.

- **IDE note:** The integrated terminal now handles `ESC[nX`, defaulting missing
  counts to one, clamping at the row edge, preserving adjacent rows, and leaving
  the row tail in place. Parser tests distinguish `CSI X` from `CSI P` and cover
  large-count clamping plus escape consumption.

L494. Terminal alternate-screen mode must preserve the shell grid. Full-screen
terminal tools switch into a scratch screen with private modes like
`CSI ?1049h` and return with `CSI ?1049l`. Consuming those escapes without
switching screens mixes the tool's frame with the shell prompt and loses the
pre-tool terminal state users expect to return to.

- **IDE note:** The integrated terminal now handles `ESC[?47h/l`,
  `ESC[?1047h/l`, and `ESC[?1049h/l` by snapshotting the primary grid, drawing
  the alternate screen on a cleared grid, and restoring the primary grid and
  cursor on exit. `ESC[?1048h/l` is handled as cursor-only save/restore. Parser
  tests cover alternate-screen restoration, resize-safe snapshot restoration,
  cursor restoration, and escape consumption.

L495. Terminal paste should use the shell paste path, not typed Ctrl+V. When the
terminal has focus, Ctrl+V should paste clipboard text into the PTY. Shells and
TUIs that enable bracketed paste with `CSI ?2004h` expect pasted text to be
framed with `ESC[200~` and `ESC[201~`; sending raw `^V` or unframed multiline
text makes paste behavior fragile and can execute pasted newlines as commands.

- **IDE note:** Terminal focus now routes Ctrl+V through a terminal paste ABI
  that reads the OS clipboard and writes to the PTY. The VT parser tracks
  `ESC[?2004h/l`, and terminal paste wraps clipboard bytes only while bracketed
  paste is enabled. Parser/helper tests cover mode toggling, escape
  consumption, plain paste bytes, and bracketed paste framing.

L496. Terminal focus should forward every named key the window layer exposes.
The window shim already turns PageUp/PageDown and several function keys into
`MUI_KEY_*` events, but the terminal mapper dropped them. That made terminal
applications lose common navigation/help/debug keys even though the IDE had
already captured the correct physical key.

- **IDE note:** The terminal key mapper now emits standard VT sequences for
  PageUp, PageDown, F2, F5, F10, F11, and F12 in addition to arrows, Home/End,
  Delete, Enter, Backspace, Tab, and Escape. Unit coverage pins the bytes for
  every named key currently forwarded to terminal focus.

L497. Terminal focus should not swallow scroll-wheel input. The event loop
already routed keyboard events to the PTY while terminal focus was active, but
wheel events entered the same focus arm and then did nothing. That made pagers
and terminal UIs feel inert even though the IDE had captured the gesture.

- **IDE note:** Terminal focus now forwards wheel direction through a
  `mui_term_scroll` ABI. The shim converts scroll-up/down into three standard
  cursor-up/down VT sequences so shells, pagers, and TUIs receive a predictable
  navigation gesture. Unit coverage pins the emitted bytes and the zero-delta
  no-op case.

L498. Terminal SGR backgrounds are part of text rendering, not decoration.
Many CLI tools use `40..47`, `100..107`, and `49` background SGR params for
selection, status bars, warnings, and table emphasis. Treating those params as
unknown leaves full-screen tools and colored output visibly flatter than the
source terminal stream intended.

- **IDE note:** Terminal cells now carry foreground and background palette
  indices. The parser handles basic and bright background SGR colors plus
  default-background reset, and the terminal draw path paints compact
  contiguous background runs before glyph runs. Unit coverage pins basic,
  bright, compound, `49`, and full-reset behavior.

L499. Terminal 256-color SGR needs sentinel-safe color storage.
The xterm `38;5;n` and `48;5;n` forms use palette entries all the way through
255, which collides with byte-sized "default" sentinels if the grid stores color
as `u8`. Supporting modern CLI color output therefore starts with making the
cell color index type large enough to keep defaults outside the real palette.

- **IDE note:** Terminal cell foreground/background indices now use sentinel-safe
  `u16` values, `38;5;n` and `48;5;n` update the active SGR colors, and the draw
  palette resolves standard xterm cube and grayscale entries. Unsupported
  truecolor SGR forms are consumed as a unit so their RGB components cannot
  accidentally change subsequent terminal styling.

L500. Terminal truecolor should be represented, not just consumed.
Many modern prompts, diff tools, test runners, and TUIs emit `38;2;r;g;b` and
`48;2;r;g;b` because their themes are not limited to the 256-color palette.
Consuming those sequences avoids escape garbage, but it still drops visible
semantic color information from the terminal output.

- **IDE note:** Terminal foreground/background colors now use a compact color
  code that can represent palette indices and exact RGB truecolor values. The
  SGR parser applies valid truecolor foreground/background sequences, rejects
  out-of-range RGB components without style side effects, and the draw resolver
  maps encoded RGB values directly to RGBA.

L501. Terminal scroll regions are structural state, not just an escape to skip.
Full-screen terminal apps use DECSTBM (`CSI top;bottom r`) to reserve headers,
status bars, prompts, and split panes while only the working area scrolls. If
the terminal consumes the escape but keeps scrolling the full grid, those fixed
rows are overwritten and TUIs visibly tear apart during ordinary output.

- **IDE note:** The terminal grid now tracks inclusive scroll margins, snapshots
  them with screen state, resets them on full resets/alternate-screen entry, and
  applies them to linefeed, `CSI S/T`, and `CSI L/M`. Parser tests cover
  margin-preserving linefeed, explicit scroll commands, insert/delete lines
  inside and outside the region, and bare `CSI r` reset to full-grid scrolling.

L502. Terminal wheel input should honor mouse-reporting mode.
Once a TUI enables mouse tracking, scroll-wheel gestures are not just navigation
keys; they are mouse events the app asked to receive. Continuing to translate
wheel input into repeated cursor-up/down bytes makes mouse-aware panes, lists,
and editors behave unlike a real terminal.

- **IDE note:** The VT parser now tracks private mouse modes `1000`, `1002`,
  `1003`, and SGR mouse mode `1006`. Terminal scroll dispatch emits SGR wheel
  reports while mouse reporting is active and keeps the previous cursor-key
  fallback for ordinary shells. Unit coverage pins mode toggling and both
  scroll encodings.

L503. Terminal cursor visibility is app-controlled rendering state.
TUIs commonly hide the text cursor with `CSI ?25l` while rendering their own
selection, focus, or status surfaces, then restore it with `CSI ?25h`. If the
IDE keeps drawing its block cursor anyway, it creates a phantom caret inside
apps that explicitly asked for a clean canvas.

- **IDE note:** The VT parser now tracks cursor visibility mode, resets it on
  full terminal reset, and exposes it through the terminal snapshot used by the
  draw path. The renderer skips the terminal block cursor while `?25l` is active.
  Unit coverage pins hide/show mode toggling and `ESC c` mode restoration.

L504. Terminal cursor shape is part of the app contract.
Shells and editors use DECSCUSR (`CSI Ps SP q`) to request block, underline, or
bar cursors that communicate insert/overwrite/focus state. Ignoring the shape
while always drawing a block cursor makes modal and text-editing TUIs feel less
precise than a real terminal.

- **IDE note:** The VT parser now tracks DECSCUSR cursor shape, maps blinking
  and steady variants onto block/underline/bar geometry, resets shape on full
  terminal reset, and exposes it through the terminal draw snapshot. The renderer
  draws the requested block, underline, or bar cursor, and unit tests pin valid
  shape changes plus ignored non-DECSCUSR `q` sequences.

L505. Terminal key encoding is mode state, not a static table.
Full-screen terminal apps can enable DECCKM (`CSI ?1h`) so arrow keys arrive as
application cursor-key sequences (`ESC O A/B/C/D`) instead of normal cursor
sequences (`ESC [ A/B/C/D`). If the IDE always sends the normal form, TUIs that
switch modes can miss navigation even though the physical key was captured.

- **IDE note:** The VT parser now tracks application cursor-key mode, resets it
  on full terminal reset, and terminal key dispatch routes through parser state
  before writing to the PTY. Unit coverage pins `?1h/l` tracking, reset behavior,
  normal arrow-key encoding, and application-mode arrow-key encoding.

L506. Terminal function keys must be forwarded end-to-end.
Capturing a physical key in the window layer is not enough if the flat ABI never
names it or the terminal mapper has no bytes for it. Shell tools, debuggers,
pagers, and TUIs use F1 through F12 for help, search, pane movement, and command
shortcuts; dropping the less-common function keys makes terminal focus feel
arbitrarily incomplete.

- **IDE note:** The flat key ABI and window named-key mapper now include F1,
  F3, F4, F6, F7, F8, and F9 in addition to the previously forwarded function
  keys. Terminal key encoding emits standard sequences for the full forwarded
  F-key set, with unit coverage at both the window mapping and terminal byte
  layers.

L507. Public flat ABI constants need header parity tests.
The Rust shim constants and the exported `mighty_ui.h` header are one ABI, not
two separate documents. Extending the Rust key set without extending the header
leaves external/generated bindings unable to name the events the window layer
now emits.

- **IDE note:** `mighty_ui.h` now mirrors the complete `MUI_KEY_*` function-key
  range, and `ffi.rs` has a unit test that parses the header's unsigned
  `#define`s and compares them against the Rust constants. Future ABI additions
  now fail fast when the public header drifts.

L508. Visual hit-test tests should use rendered geometry.
When a component has moved from estimated text layout to measured text layout,
test helpers that keep the old character-count math can still pass while the
real renderer and click targets diverge on wide glyphs, DPI-scaled surfaces, or
wrapped composer text.

- **IDE note:** The AI composer hit-test and no-key/active geometry tests now
  call the same measured input-geometry helper as the draw path. The old
  character-estimated composer geometry helper was removed from the test surface,
  so future AI panel regressions are checked against rendered text metrics.

L509. Explicit AI commands should explain unavailable states.
Debounced background completion is allowed to stay quiet so typing never becomes
noisy, but a direct user command like Force Inline AI Completion is an explicit
request. Returning `0` without feedback makes the command palette and shortcut
feel broken when the setting is off, the API key is missing, or a request is
already in flight.

- **IDE note:** `mui_ghost_force` now reports disabled Inline AI, missing
  `ANTHROPIC_API_KEY`, already-running requests, and unexpected start failures
  through the toast lane while preserving the silent automatic debounce path.
  The new messages share the existing AI toast replacement key, so repeated AI
  availability feedback updates in place instead of stacking.

L510. Toolbar actions should share command dispatch with shortcuts.
When a toolbar button reimplements the same state checks as a keyboard shortcut
or palette command, feedback behavior can drift even if the visible operation is
nominally the same. Debug controls are especially sensitive because unavailable
step/stop/continue buttons must explain whether a session is idle, running, or
paused.

- **IDE note:** Debug toolbar action dispatch now delegates to the same
  `mui_dbg_*` entry points used by function keys and palette commands. Toolbar
  regression coverage pins the unavailable-state toasts for Step Over, Step
  Into, Step Out, and Stop, keeping all debug command surfaces aligned.

L511. User-invoked debug commands should reveal the debug surface first.
Shortcut-triggered debug actions can otherwise produce the right state change or
toast while leaving the user on another sidebar panel. Debugging is a stateful
workflow, so F5/F10/F11/Shift+F5 need to show the Run and Debug view just like
palette and toolbar commands.

- **IDE note:** The shared `mui_dbg_*` commands now open the Run and Debug panel
  before applying start, continue, stop, pause, restart, or step behavior.
  Regression coverage pins F5-without-file feedback and paused-step behavior
  from a closed sidebar.

L512. Long-running workflows need palette-visible stop commands.
Starting a run from the command palette without an adjacent stop command leaves
keyboard-first users dependent on locating the visual toolbar. Stop controls are
part of the same workflow as start controls, so both should be discoverable from
the command surface.

- **IDE note:** The command palette now lists `Run: Stop Process` and
  `Test: Stop Run`, dispatching through the existing `mui_run_stop` and
  `mui_test_stop` ABIs. Registry/mirror coverage pins the stable command ids and
  Mighty helpers so the commands remain reachable.

L513. Focused test workflows should be command-surface visible.
`Run Tests` is useful, but it does not advertise the stricter current-cursor
workflow that records the nearest `fn test_*` and highlights that row after the
package run. If the focused path only exists as an ABI or toolbar shortcut, users
cannot discover it from search.

- **IDE note:** The command palette now lists `Run Test at Cursor` and routes it
  through the existing `mui_test_run_at_cursor` ABI. Registry/mirror coverage
  pins the stable command id and Mighty helper beside the full-run and stop
  testing commands.

L514. Web lifecycle controls should be searchable beside Run in Browser.
`Mighty: Run in Browser` starts a long-running Web Playground workflow, but the
matching Stop and Open-in-Browser actions were only reachable from the panel
chrome. Palette users need the same lifecycle controls, especially when the URL
exists but the lower dock is not focused.

- **IDE note:** The command palette now lists `Web: Stop Server` and
  `Web: Open in Browser`, dispatching through the existing `mui_web_stop` and
  `mui_web_open_browser` ABIs. Registry/mirror coverage pins the stable command
  ids and Mighty helpers beside `Mighty: Run in Browser`.

L515. Chat surfaces need explicit reset commands.
AI panels accumulate transcript, draft, scroll, and sometimes in-flight stream
state. If clearing that state is only possible by restarting the app or manually
deleting draft text, the copilot feels less like an editor surface and more like
a one-off modal.

- **IDE note:** The command palette now lists `AI: Clear Chat`, dispatching
  through a new `mui_ai_clear` ABI that opens the AI panel, clears draft,
  transcript, scroll, and active stream state, and reports both changed and
  already-empty outcomes. Registry/mirror and ABI tests pin the behavior.

L516. Global viewport controls should be searchable.
If a feature is implemented only as an intercepted chord or mouse gesture, users
who search the command palette cannot discover it and stale shortcut labels can
drift after input routing changes. UI zoom is a global editor control, so it
belongs beside other View commands.

- **IDE note:** The command palette now lists `View: Zoom In`, `View: Zoom Out`,
  and `View: Reset Zoom`, dispatching through the existing `mui_zoom_in`,
  `mui_zoom_out`, and `mui_zoom_reset` ABIs. `Jump Back` no longer advertises
  the now-reserved `Ctrl+-` chord, and registry/mirror tests pin the command
  labels, keybindings, ids, and Mighty helpers.

L517. Header-only Explorer actions should have command equivalents.
Explorer toolbar buttons are convenient with a mouse, but the same project-tree
maintenance actions should be available to keyboard-first users through command
search. Immediate tree actions are especially easy to miss when they only exist
as compact header icons.

- **IDE note:** The command palette now lists `Explorer: Collapse All Folders`,
  dispatching through the existing `mui_tree_collapse_all` ABI after focusing
  the Explorer panel. Registry, metadata, and dispatcher tests pin the command
  id, label, helper, and Explorer-focus behavior.

L518. Panel primary actions should be command-palette reachable.
Opening a panel is not the same as exposing its core workflow. If Search can only
run or replace from panel-local keys and compact buttons, command-first users
cannot chain those actions from Quick Open or the palette after entering a query.

- **IDE note:** The command palette now lists `Search: Run Search`,
  `Search: Replace All`, and `Search: Toggle Replace Field`, each focusing the
  Search panel before calling the existing Search ABI. Registry, metadata, and
  dispatcher tests pin the ids, labels, helpers, and Search-focus behavior.

L519. Source-control refresh should not require precise mouse targeting.
SCM status can stale after external git commands or filesystem changes. A
compact header refresh button is useful, but command-first users need the same
refresh path available without aiming at a small icon.

- **IDE note:** The command palette now lists `Git: Refresh Source Control`,
  focusing the Source Control panel before calling the existing `mui_scm_refresh`
  ABI. Registry, metadata, and dispatcher tests pin the id, label, helper, and
  SCM-focus behavior.

L520. Workspace refresh should update every file-navigation surface.
Refreshing Explorer after external filesystem changes is only half the workflow
if Quick Open keeps an old file index. A single explicit workspace-tree refresh
command should keep both navigation surfaces in sync.

- **IDE note:** The command palette now lists `Explorer: Refresh`, focusing the
  Explorer panel, calling `mui_tree_refresh`, and reindexing Quick Open through
  `mui_quickopen_reindex`. Registry, metadata, and dispatcher tests pin the id,
  label, helper, Explorer focus, tree refresh, and file-index refresh.

L521. Diagnostics refresh should be an explicit command.
Opening Problems refreshes the list, but once the dock is already visible there
should be a named command that reruns diagnostics and rebuilds the Problems
aggregation without depending on save/open side effects.

- **IDE note:** The command palette now lists `Problems: Refresh Diagnostics`,
  calling `mui_diag_refresh`, `mui_problems_refresh`, and `mui_problems_open` so
  the gutter diagnostics and Problems dock update together. Registry, metadata,
  and dispatcher tests pin the id, label, helper, refresh calls, and panel open.

L522. Symbol navigation needs an explicit refresh command.
Outline refreshes during common file lifecycle events, but command-first users
need a direct way to rescan document symbols after generated edits, language
server recovery, or other state changes that do not pass through save/open.

- **IDE note:** The command palette now lists `Outline: Refresh Symbols`,
  focusing the Outline panel before calling `mui_outline_refresh`. Registry,
  metadata, and dispatcher tests pin the id, label, helper, panel focus, and
  symbol refresh call.

L523. Topology views should expose refresh as a named action.
Panels that summarize project structure can become stale after generated edits
or external file changes. Opening the panel may refresh it, but once visible the
same rescan should be command-palette reachable without leaving the current
keyboard flow.

- **IDE note:** The command palette now lists `Mighty Agents: Refresh Topology`,
  focusing the Mighty Agents topology panel before calling `mui_agents_refresh`.
  Registry, metadata, and dispatcher tests pin the id, label, helper, panel
  focus, and topology refresh call.

L524. Output panels need clear actions that do not imply process control.
Stopping a task and clearing its transcript are different user intents. A Run
panel can be useful while a process keeps running, but the user may still need a
fresh viewport before repeating an interaction or capturing only new output.

- **IDE note:** The command palette now lists `Run: Clear Output`, opening the
  Run dock and calling `mui_run_clear` without stopping the active process or
  resetting the last status. State, ABI, registry, metadata, dispatcher, and
  feedback tests pin the behavior.

L525. Parsed result panels should clear their model, not their process.
Test results are structured data derived from process output. Clearing them
should remove rows, counts, summary state, scroll, and stale click targets while
leaving the selected package and any running `mty test` process alone.

- **IDE note:** The command palette now lists `Test: Clear Results`, focusing
  Testing and calling `mui_test_clear`. State, ABI, registry, metadata,
  dispatcher, and feedback tests pin that results are cleared separately from
  process control.

L526. Browser-run output should clear without losing the served URL.
The Web Playground's transcript, served URL, and server lifecycle are separate
state. Clearing noisy build/serve lines should leave the URL pill usable and
avoid stopping the active server.

- **IDE note:** The command palette now lists `Web: Clear Output`, opening the
  Web Playground and calling `mui_web_clear`. State, ABI, registry, metadata,
  dispatcher, and feedback tests pin that output lines clear while the URL and
  running session remain intact.

L527. Embedded run transcripts need their own clear command.
Panels that reuse a shared run model still own user-facing context. Clearing an
embedded transcript should keep the parent panel focused and preserve its domain
model instead of forcing users into the generic Run dock.

- **IDE note:** The command palette now lists `Mighty Agents: Clear Run Output`,
  focusing the Mighty Agents panel and calling `mui_agents_clear_run_output`.
  State, ABI, registry, metadata, dispatcher, and feedback tests pin that the
  embedded run transcript clears without rebuilding topology.

L528. Modal editor surfaces need named exit commands.
Inline diff is not just a transient key mode; it is a read-only editor surface
that can be opened from Source Control. Command-palette users need an explicit
way back to editing that updates both shim state and Mighty-side mode flags.

- **IDE note:** The command palette now lists `Diff: Close View`, calling
  `mui_diff_close` and clearing Mighty's `diff_open` flag. ABI, registry,
  metadata, dispatcher, and label tests pin the close path.

L529. Toggles still need one-way hide commands for persistent overlays.
Git blame is a persistent editor annotation. A toggle is useful for shortcuts,
but command-palette users need an unambiguous hide action that never turns blame
back on by mistake.

- **IDE note:** The command palette now lists `Git: Hide Blame`, calling
  `mui_blame_close`. ABI, registry, metadata, dispatcher, feedback, and
  idempotency tests pin that hiding blame is separate from toggling it.

L530. Inline preview surfaces need palette-close parity.
Keyboard dismissals are not enough for command-palette users. A peek preview is
an editor surface, so it needs a named close action that reaches the same state
transition as Escape without reopening or navigating anywhere.

- **IDE note:** The command palette now lists `Peek: Close View`, calling
  `mui_peek_close`. ABI, registry, metadata, dispatcher, and label tests pin
  that Peek Definition has an explicit close path.

L531. Language popups need explicit close commands too.
Hover and signature help are transient, but they still occupy editor attention.
Users who drive the IDE through the command palette need one-way dismiss actions
that clear popup state without making a fresh language-server request.

- **IDE note:** The command palette now lists `Hover: Close Popup` and
  `Signature Help: Close Popup`, calling `mui_hover_clear` and `mui_sig_clear`
  while resetting Mighty's local popup flags. ABI, registry, metadata,
  dispatcher, and label tests pin both close paths.

L532. Toggleable previews still need a one-way close.
A preview command that toggles or opens is not the same as an explicit dismiss
command. Command-palette users need a safe way to close Markdown preview without
accidentally opening it when it is already closed.

- **IDE note:** The command palette now lists `Markdown: Close Preview`, calling
  `mui_md_close`. ABI, registry, metadata, dispatcher, and label tests pin that
  Markdown preview has a dedicated one-way close path.

L533. Preference overlays need a command-palette dismiss path.
Settings is a modal preference surface, not just a rail utility. Opening it from
the palette should be matched by a named close command so keyboard-centric users
can leave the surface without relying on the mouse or Escape.

- **IDE note:** The command palette now lists `Preferences: Close Settings`,
  calling `mui_settings_close` and clearing Mighty's `settings_open` flag. ABI,
  registry, metadata, dispatcher, and label tests pin the close path.

L534. Previewing modal pickers need explicit cancel commands.
Color theme selection previews choices live. A command-palette close action must
cancel that picker, not commit the previewed theme or leave Mighty-side modal
flags stale.

- **IDE note:** The command palette now lists
  `Preferences: Close Color Theme Picker`, calling `mui_theme_picker_cancel` and
  clearing Mighty's `theme_picker_open` flag. ABI, registry, metadata,
  dispatcher, and label tests pin that closing the picker reverts previewed
  themes.

L535. Close commands should not inherit context-sensitive cancel semantics.
Keyboard Shortcuts uses Escape to leave remap capture before closing the overlay.
A command-palette close action should be stronger and close the overlay even
when capture is active.

- **IDE note:** The command palette now lists `Help: Close Keyboard Shortcuts`,
  calling a dedicated `mui_keys_close` ABI. ABI, registry, metadata,
  dispatcher, and label tests pin that close exits both capture and overlay
  state, while the existing cancel path still only exits capture first.

L536. Editor-mode popovers need explicit cancel commands.
Rename and Code Actions are transient editor modes opened from the palette.
Command-palette users need one-way cancel actions that close the active mode
without applying a rename or quick fix.

- **IDE note:** The command palette now lists `Rename Symbol: Cancel` and
  `Code Actions: Close Menu`, calling `mui_rename_cancel` and
  `mui_codeaction_cancel` while clearing Mighty's local mode flags. ABI,
  registry, metadata, dispatcher, and label tests pin both close paths.

L537. Bottom prompts need command-palette cancellation too.
Typed fallback prompts collect destructive and file-system inputs, so their
cancel path should not be reachable only through Escape, mouse hit-testing, or
outside clicks.

- **IDE note:** The command palette now lists `Prompt: Cancel Input`, calling
  `mui_prompt_cancel` while clearing Mighty's local `prompt_kind`. ABI,
  registry, metadata, dispatcher, and label tests pin that prompt cancellation
  is available as a first-class command.

L538. Editor find surfaces deserve one-way close commands.
Find & Replace edits the active document when accepted, so dismissing the bar
should be a named palette action that cannot accidentally replace text or reopen
the surface.

- **IDE note:** The command palette now lists `Find & Replace: Close Bar`,
  calling `mui_replace_cancel` while clearing Mighty's local `replacing` flag.
  ABI, registry, metadata, dispatcher, and label tests pin the close path.

L539. Suggestion popups should be dismissible as commands.
Autocomplete suggestions are accepted by Enter or mouse, so the command palette
needs a distinct close action that only dismisses the dropdown and never inserts
the selected candidate.

- **IDE note:** The command palette now lists
  `Autocomplete: Close Suggestions`, calling `mui_complete_cancel` while
  clearing Mighty's local `completing` flag. ABI, registry, metadata,
  dispatcher, and label tests pin the close path.

L540. Destructive confirmations need named non-destructive exits.
Unsaved-work confirmation offers Save and Discard, so cancel must stay explicit
and easy to audit: it should dismiss the modal while preserving the dirty tab and
never implying a save or discard choice.

- **IDE note:** The command palette now lists
  `Unsaved Changes: Cancel Confirmation`, calling `mui_dirty_confirm_cancel`.
  ABI, registry, metadata, dispatcher, and label tests pin that cancellation
  clears the pending confirmation while keeping the dirty tab open.

L541. Git branch pickers need one-way close commands.
Branch switching can check out existing refs or create new branches, so the
dismiss action should stay explicit and never route through the switch/create
path.

- **IDE note:** The command palette now lists `Git: Close Branch Switcher`,
  calling `mui_branch_cancel` while clearing Mighty's local `branch_open` flag.
  ABI, registry, metadata, dispatcher, and label tests pin the close path.

L542. Breadcrumb dropdowns need command-palette dismissal.
Breadcrumb menus can open files or jump to symbols, so their dismiss action
should be a named command that cannot accidentally accept the highlighted row.

- **IDE note:** The command palette now lists `Breadcrumb: Close Menu`, calling
  `mui_crumb_menu_cancel`. ABI, registry, metadata, dispatcher, and label tests
  pin the close path.

L543. Launcher overlays need explicit close commands too.
Command Palette and Quick Open are launchers, not just keyboard shortcuts. Their
close actions should be callable by name, especially from Quick Open command
mode, without executing the selected command or opening a file.

- **IDE note:** The command palette now lists `Command Palette: Close` and
  `Quick Open: Close`, calling `mui_palette_cancel` and `mui_qo_cancel` while
  clearing Mighty's local overlay flags. ABI, registry, metadata, dispatcher,
  and label tests pin both close paths.

L544. Palette-opened landing surfaces need matching close commands.
Welcome and the focused Open Recent picker are reachable from commands and quick
actions, so their dismiss path should also be command-addressable rather than
only mouse-driven.

- **IDE note:** The command palette now lists `Welcome: Close`, calling
  `mui_welcome_dismiss` for both the forced Welcome landing and Open Recent
  picker. ABI, registry, metadata, dispatcher, and label tests pin the close
  path.

L545. Inline suggestions need non-accepting command exits.
Ghost completions are accepted by Tab or Ctrl+Right, so their dismiss path should
be a named command that clears the suggestion without inserting any generated
text.

- **IDE note:** The command palette now lists
  `AI: Dismiss Ghost Completion`, calling `mui_ghost_dismiss`. Seeded ghost,
  registry, metadata, dispatcher, and label tests pin that the command clears the
  visible inline suggestion without accepting it.

L546. Snippet tab-stop sessions need command cancellation.
Expanded snippets leave real text in the editor while Tab and Shift+Tab navigate
placeholders, so cancellation should end only the navigation session and never
delete the expansion.

- **IDE note:** The command palette now lists
  `Snippet: Cancel Tab-Stop Session`, calling `mui_snippet_cancel`. Snippet
  expansion, registry, metadata, dispatcher, and label tests pin that the command
  ends tab-stop mode while preserving the expanded text.

L547. Shared drawers still need surface-specific close commands.
Generic bottom-dock close is useful for layout recovery, but users also expect to
close a named tool without depending on which drawer is active.

- **IDE note:** The command palette now lists `Terminal: Close`, calling
  `mui_term_close` and clearing terminal focus. Terminal state, registry,
  metadata, dispatcher, and label tests pin that the command uses the
  terminal-specific close path rather than the shared dock fallback.

L548. Diagnostic panels deserve direct close routes.
Problems is a named tool with its own open state, refresh behavior, and visible
close button, so closing it from commands should not depend on a generic dock
close or toggle semantics.

- **IDE note:** The command palette now lists `Problems: Close Panel`, calling
  `mui_problems_close`. Problems state, registry, metadata, dispatcher, and label
  tests pin state-aware close feedback for both open and already-closed cases.

L549. Output panels need direct close routes separate from lifecycle controls.
Run output can be hidden without stopping a process or clearing its transcript,
so a command named Stop or Clear cannot safely stand in for Close.

- **IDE note:** The command palette now lists `Run: Close Panel`, calling
  `mui_run_close`. The Run panel reports closed/already-closed feedback while
  preserving output rows and process state, and routing tests pin that it clears
  Run focus through the dedicated ABI.

L550. Testing close is different from stopping or clearing. Test results are
navigation state as much as process output: hiding the Testing panel should not
kill a running test process or discard parsed pass/fail rows.

- **IDE note:** The command palette now lists `Test: Close Panel`, calling
  `mui_test_close`. The close path switches back to Explorer, preserves parsed
  result rows, reports closed/already-closed feedback, and releases Testing
  focus through the Mighty dispatcher.

L551. Web preview close should not stop the served app. The Web Playground's
panel, server lifecycle, scraped URL, and transcript are distinct pieces of
state; hiding the panel must leave the browser session recoverable.

- **IDE note:** The command palette now lists `Web: Close Panel`, calling
  `mui_web_close`. The close path preserves output, URL, and running state,
  reports closed/already-closed feedback, and releases Web focus through the
  Mighty dispatcher.

L552. Agent topology close should preserve inspection context. The Mighty
Agents panel owns a discovered topology, live-inspect notes, and embedded run
output, so closing it from commands should not imply refresh or clearing state.

- **IDE note:** The command palette now lists `Mighty Agents: Close Panel`,
  calling `mui_agents_close`. The close path switches back to Explorer,
  preserves topology and run rows, reports closed/already-closed feedback, and
  releases Agents focus through the Mighty dispatcher.

L553. Search close should preserve the active query. Search panels often hold a
partially composed query, replacement text, and result set. Closing the panel
should hide it, not discard that work.

- **IDE note:** The command palette now lists `Search: Close Panel`, calling
  `mui_search_close`. The close path switches back to Explorer, preserves the
  query/results state, reports closed/already-closed feedback, and routes
  through the Mighty dispatcher.

L554. Outline close should not throw away navigation context. The document
symbol tree and current-symbol highlight are expensive enough to preserve, and
users often hide Outline briefly before returning to the same navigation state.

- **IDE note:** The command palette now lists `Outline: Close Panel`, calling
  `mui_outline_close`. The close path switches back to Explorer, preserves the
  symbol cache and current row, reports closed/already-closed feedback, and
  routes through the Mighty dispatcher.

L555. Source Control close should preserve git context. Branch/ahead-behind
state, the parsed change list, and a partially written commit message are active
workflow state; hiding Source Control must not imply refresh, reset, or discard.

- **IDE note:** The command palette now lists
  `Source Control: Close Panel`, calling `mui_scm_close`. The close path
  switches back to Explorer, preserves status rows and commit-message text,
  reports closed/already-closed feedback, and routes through the Mighty
  dispatcher.

L556. Debug panel close must not stop debugging. Run and Debug owns visual
navigation state around breakpoints, stack frames, variables, and console output,
while Stop is a process lifecycle action. Hiding the panel should leave the
debug model intact.

- **IDE note:** The command palette now lists
  `Run and Debug: Close Panel`, calling `mui_dbg_close`. The close path switches
  back to Explorer, preserves stopped/session state, stack/variable rows, and
  breakpoints, reports closed/already-closed feedback, and routes through the
  Mighty dispatcher.

L557. Explorer close should hide chrome, not reset the tree. Expanded folders
and the active Explorer panel are navigation context; a close command should
make room for editing without collapsing or refreshing the file tree.

- **IDE note:** The command palette now lists `Explorer: Close Panel`, calling
  `mui_explorer_close`. The close path hides the sidebar, keeps Explorer as the
  active panel, preserves expanded rows, reports closed/already-closed feedback,
  and routes through the Mighty dispatcher.

L558. Empty notification clears should still acknowledge the command. Clearing
visible toasts must leave the notification stack empty, but invoking the command
when there is nothing to clear should not feel inert.

- **IDE note:** `mui_toast_clear` now reports `No notifications to clear` only
  for the empty-stack case. Successful clears stay silent so they do not create
  a replacement toast immediately after removing notifications, and repeated
  empty clears share one notification feedback lane.

L559. Problems diagnostics need a clear action separate from close and refresh.
Refresh recomputes diagnostics, and Close hides the dock; neither means "discard
the current aggregated rows but keep the Problems surface available."

- **IDE note:** The command palette now lists
  `Problems: Clear Diagnostics`, calling `mui_problems_clear`. The clear path
  drops rows, counts, and scroll, keeps the Problems dock open, reports
  cleared/already-empty feedback, and routes through the Mighty dispatcher.

L560. Source Control draft text needs its own clear command. Refreshing git
status should not discard a commit message, and closing Source Control should
preserve it; users still need an explicit way to throw away the draft.

- **IDE note:** The command palette now lists
  `Source Control: Clear Commit Message`, calling `mui_scm_clear_message`. The
  command clears only the shim-owned commit-message buffer, preserves git status
  rows and the active Source Control panel, reports cleared/already-empty
  feedback, and routes through the Mighty dispatcher.

L561. Search results need clearing separate from query text. Search query and
replacement drafts are user input, while result rows are derived navigation
state; users should be able to discard stale matches without rebuilding the
query.

- **IDE note:** The command palette now lists `Search: Clear Results`, calling
  `mui_search_clear_results`. The clear path drops only the current result files
  and matches, keeps the Search panel active, preserves query/replace/focus
  state, reports cleared/already-empty feedback, and routes through the Mighty
  dispatcher.

L562. Outline symbols are derived navigation state and need an explicit clear.
Refresh recomputes symbols from the active document, and Close hides the panel;
neither gives users a way to discard stale symbol rows while keeping Outline
visible.

- **IDE note:** The command palette now lists `Outline: Clear Symbols`, calling
  `mui_outline_clear_symbols`. The command clears symbol rows and the
  cursor-current symbol, keeps the Outline panel active, reports
  cleared/already-empty feedback, and routes through the Mighty dispatcher.

L563. Debug sessions need a reset that is not close and not restart. Close hides
the Run and Debug panel, Restart launches the last target again, and Stop leaves
console/session residue; users need an explicit way to discard the current
session model while keeping breakpoints and target setup.

- **IDE note:** The command palette now lists `Run and Debug: Clear Session`,
  calling `mui_dbg_clear_session`. The command disconnects any live adapter,
  clears state/stack/variables/current stop/console, preserves breakpoints and
  the last target, keeps Run and Debug open, reports cleared/already-empty
  feedback, and routes through the Mighty dispatcher.

L564. Terminal buffers need clearing separate from close. Close tears down the
PTY and shell, while users often only want to discard visible scrollback/prompt
noise and keep the current terminal session alive.

- **IDE note:** The command palette now lists `Terminal: Clear Buffer`, calling
  `mui_term_clear`. The command clears the visible terminal grid without closing
  the shell, preserves terminal focus when the panel remains open, reports
  cleared/already-empty/closed feedback, and routes through the Mighty
  dispatcher.

L565. Output-workflow feedback should replace stale clear/close cards. Run and
Testing commands now expose Stop, Clear, and Close separately, so their visible
feedback must behave like one workflow instead of stacking old clear/close
states beside the latest result.

- **IDE note:** `Run output cleared`, `Run output already empty`,
  `Run panel closed`, and `Run panel is already closed` now share the Run/Web
  toast replacement key. `Test results cleared`,
  `Test results already empty`, `Testing panel closed`, and
  `Testing panel is already closed` now share the Testing replacement key.
  Repeated Run and Testing panel actions keep the latest state visible.

L566. Save As fallback should explain why typed-path mode opened. When the
native Save As picker cannot run, Mighty falls back to its scalar bottom prompt
so the user can still type a destination. Without visible feedback, that prompt
looks like a random mode switch after the Save As command.

- **IDE note:** `mui_save_as_dialog` now reports
  `Save dialog unavailable; use typed path` before returning `-1` for the Mighty
  prompt fallback. The regression forces the unavailable branch without showing
  native UI and verifies the untitled dirty tab remains unchanged.

L567. Terminal paste feedback is still clipboard feedback. Terminal paste uses
different messages from editor paste, but the user is repeating the same
clipboard operation. Letting those outcomes live in separate toast lanes makes
old paste failures or successes linger next to the current state.

- **IDE note:** `Pasted to terminal` and `Terminal paste failed` now share the
  clipboard toast replacement key with editor paste/copy/cut outcomes. The
  regression extends the clipboard toast replacement test across terminal paste
  failure and success.

L568. Plain Save on an untitled buffer should describe the same fallback as Save
As. Even though the main UI routes untitled Ctrl+S through Save As, direct ABI
callers can still reach `mui_ed_save` without a file path. If the native picker
cannot run, telling the user to use Save As is stale when the real recovery is
the typed-path fallback.

- **IDE note:** `mui_ed_save` now reports
  `Save dialog unavailable; use typed path` when an untitled buffer cannot open
  the native save picker. The regression forces that direct Save branch and
  verifies the tab remains dirty, untitled, and ready for typed-path recovery.

L569. Palette close commands should acknowledge Peek state. `Peek: Close View`
was exposed beside other close commands, but it reused a void Esc helper that
silently cleared the card or did nothing when no Peek view was open. That makes
the command palette feel inconsistent with the rest of the panel and popup close
surface.

- **IDE note:** `mui_peek_close` now returns `1`/`0` and reports
  `Peek view closed` or `Peek view is already closed`. Those messages share the
  code-intelligence toast replacement key, and the regression verifies both the
  active-close and already-closed paths.

L570. Terminal close should return state like other panel close commands. The
terminal close path already distinguished a real close from the already-closed
case in its toast, but its void ABI kept Mighty from treating it as a normal
stateful command.

- **IDE note:** `mui_term_close` now returns `1` when it closes a terminal panel
  and `0` when the terminal is already closed. Mighty declares the return value
  and explicitly discards it in the command dispatcher, matching the typed
  pattern used by other close commands.

L571. Markdown preview close should acknowledge palette no-ops. The preview
header close button is only visible when there is something to close, but the
command palette can invoke `Markdown: Close Preview` when the preview is already
closed. Silent no-ops make that command feel broken.

- **IDE note:** `mui_md_close` now returns `1` when it collapses the preview and
  `0` when the preview is already closed, reporting
  `Markdown preview is already closed` for the no-op path. Mighty declares and
  discards the return value explicitly, and Markdown preview feedback stays in
  the Markdown toast replacement lane.

L572. Diff close should behave like the other source-control close commands.
`Diff: Close View` could be invoked from the palette after the inline diff was
already gone, but the old void ABI silently did nothing and could not report
whether it actually closed a view.

- **IDE note:** `mui_diff_close` now returns `1` when it closes an active diff
  view and `0` when the view is already closed, reporting `Diff view closed` or
  `Diff view is already closed`. Those messages share the Git/diff toast
  replacement lane, and Mighty explicitly discards the return in both Esc and
  palette close paths.

L573. Settings close should not be a silent palette no-op. `Preferences: Close
Settings` can be invoked when the Settings panel is already closed, so the close
ABI needs the same stateful feedback contract as other panel close commands.

- **IDE note:** `mui_settings_close` now returns `1` when it closes the Settings
  panel and `0` when Settings is already closed, reporting
  `Settings panel closed` or `Settings panel is already closed`. Mighty
  explicitly discards the return from Esc, mouse-dismiss, and palette close
  paths.

L574. Keyboard Shortcuts close should report command state, including capture
mode. The palette close command is available even when the shortcuts overlay is
already closed, and the dedicated close path also needs to force-exit remap
capture when the overlay is open.

- **IDE note:** `mui_keys_close` now returns `1` when it closes the Keyboard
  Shortcuts overlay and `0` when it is already closed, reporting
  `Keyboard Shortcuts closed` or `Keyboard Shortcuts is already closed`. The
  regression verifies that close still exits capture mode and that the feedback
  uses the layout toast replacement lane.

L575. Breadcrumb menu cancel should be stateful too. The palette exposes
`Breadcrumb: Close Menu`, but the old cancel ABI silently did nothing when the
dropdown was already gone and gave Mighty no state to consume.

- **IDE note:** `mui_crumb_menu_cancel` now returns `1` when it closes an active
  breadcrumb dropdown and `0` when no menu is open, reporting
  `Breadcrumb menu closed` or the existing `No breadcrumb menu open`. Mighty
  explicitly discards the return value from keyboard, mouse, and palette cancel
  paths.

L576. Branch switcher cancel should report state like other transient overlays.
`Git: Close Branch Switcher` is palette-exposed and can be invoked after the
picker has already closed, so a void cancel ABI makes that no-op look broken.

- **IDE note:** `mui_branch_cancel` now returns `1` when it closes an active
  branch picker and `0` when no picker is open, reporting
  `Branch switcher closed` or `No branch picker open`. Mighty explicitly
  discards the return from Esc, mouse-dismiss, and palette cancel paths, and the
  feedback stays in the Git toast replacement lane.

L577. Prompt cancel is both a user command and shared typed-action cleanup.
The same ABI clears prompts after successful Save As/New File/Open workflows and
also backs `Prompt: Cancel Input`, so active cleanup should not overwrite the
real action toast.

- **IDE note:** `mui_prompt_cancel` now returns `1` when it clears an active
  prompt and `0` when no prompt is open. The active path stays silent so typed
  actions keep their own feedback; the no-op path reports `No prompt input open`
  in the name-input toast lane, and Mighty explicitly discards the returned
  state from keyboard, mouse-dismiss, and palette paths.

L578. Command palette cancel is also command dispatch cleanup. Enter and mouse
selection close the palette immediately before dispatching the selected command,
so an active-close toast would compete with the command's real outcome.

- **IDE note:** `mui_palette_cancel` now returns `1` when it closes an active
  palette and `0` when no palette is open. Active closes stay silent for command
  dispatch cleanup; the no-op path reports `No command palette open` in the
  navigation toast lane, and Mighty explicitly discards the returned state from
  Escape, mouse-dismiss, command-selection, and palette command paths.

L579. Quick Open cancel must distinguish close from accept cleanup. Quick Open
accepts files, symbols, lines, and command rows through paths that close the
overlay as part of a larger action, so the cancel ABI should report state without
adding active-close noise.

- **IDE note:** `mui_qo_cancel` now returns `1` when it closes an active Quick
  Open panel and `0` when no panel is open. Active closes stay silent so accept
  flows keep their own result feedback; the no-op path reports
  `No Quick Open panel open` in the navigation toast lane, and Mighty explicitly
  discards the returned state from Escape, mouse-dismiss, command-selection, and
  palette command paths.

L580. Color theme cancel is a real revert action. Unlike command-palette or
Quick Open accept cleanup, cancelling the theme picker restores the theme that
was active when the picker opened, so both active and no-op command paths should
report visible state.

- **IDE note:** `mui_theme_picker_cancel` now returns `1` when it cancels an
  active picker and `0` when no picker is open. Active cancel reports
  `Color theme picker cancelled`; no-op cancel reports
  `No color theme picker open`; both messages stay in the theme toast lane, and
  Mighty explicitly discards the returned state from Escape, mouse-dismiss, and
  palette command paths.

L581. Unsaved-work confirmation cancel is a safety decision, not a silent reset.
The modal protects dirty edits, so command-palette and Escape paths should report
whether they actually cancelled a pending close/quit choice.

- **IDE note:** `mui_dirty_confirm_cancel` now returns `1` when it clears an
  active unsaved-work confirmation and `0` when no confirmation is open. Active
  cancel reports `Unsaved changes confirmation cancelled`; no-op cancel reports
  `No unsaved changes confirmation open`; both messages stay in the save toast
  lane, and Mighty explicitly discards the returned state from Escape,
  mouse-button, and palette command paths.

L582. Autocomplete cancel is accept cleanup as well as a close command. Accepting
suggestions closes the dropdown immediately after inserting text, so active
cancel should report state without adding a competing toast.

- **IDE note:** `mui_complete_cancel` now returns `1` when it clears an active
  suggestions dropdown and `0` when no dropdown is open. Active closes stay
  silent for accept/typing cleanup; the no-op path reports
  `No autocomplete suggestions open` in the CodeIntel toast lane, and Mighty
  explicitly discards the returned state from typing, Escape, mouse, undo/redo,
  and palette command paths.

L583. Rename cancel should report the inline-edit state. Rename Symbol has a
palette-visible cancel command, and unlike autocomplete it is not a normal
success cleanup path, so closing an active rename input should be visible.

- **IDE note:** `mui_rename_cancel` now returns `1` when it clears an active
  rename input and `0` when no rename input is open. Active cancel reports
  `Rename cancelled`; no-op cancel reports `No rename input open`; both messages
  stay in the rename toast lane, and Mighty explicitly discards the returned
  state from Escape and palette command paths.

L584. Code-action cancel is usually cleanup around another editor action.
Applying an action, typing through the menu, or clicking away all close the same
shim menu, so active close should report state without adding a toast that can
compete with the action result.

- **IDE note:** `mui_codeaction_cancel` now returns `1` when it clears an active
  code-action menu and `0` when no menu is open. Active closes stay silent for
  apply/typing/click-away cleanup; the no-op path reports
  `No code action menu open` in the CodeAction toast lane, and Mighty explicitly
  discards the returned state from Escape, char-dismiss, mouse-dismiss, and
  palette command paths.

L585. Find & Replace close is a real surface command. Unlike action-application
cleanup, closing the bar should be visible whether the command closed an active
bar or found that the bar was already gone.

- **IDE note:** `mui_replace_cancel` now returns `1` when it closes the active
  Find & Replace bar and `0` when no bar is open. Active close reports
  `Find & Replace closed`; no-op close reports `No Find & Replace bar open`;
  both messages stay in the replace toast lane, and Mighty explicitly discards
  the returned state from Escape, close-button, and palette command paths.

L586. Snippet cancel is both an editor cleanup hook and a palette-visible
command. Generic Escape should not produce a no-op toast on every keypress, but
the explicit command should still report whether a tab-stop session existed.

- **IDE note:** `mui_snippet_cancel` now returns `1` when it ends an active
  snippet tab-stop session and `0` when no session is active. Active cancel
  reports `Snippet session cancelled`; no-op cancel reports
  `No snippet session active`; both messages share a snippet toast lane. Mighty
  only calls the ABI from generic Escape/collapse cleanup while a session is
  active, and explicitly discards the returned state for the palette command.

L587. Cleanup ABIs and command-close ABIs do not have to be the same function.
Hover and signature help are dismissed during cursor movement, Escape cleanup,
and feature transitions, so those cleanup paths must stay silent while the
palette-visible close commands still need observable state.

- **IDE note:** `mui_hover_clear` and `mui_sig_clear` remain silent cleanup
  helpers. New `mui_hover_close` and `mui_sig_close` command ABIs return `1`
  when they close an active popup and `0` when the popup is already inactive.
  Hover close reports `Hover popup closed` / `No hover popup open`; signature
  close reports `Signature Help popup closed` /
  `No Signature Help popup open`; all four messages stay in the CodeIntel toast
  lane, and Mighty uses the stateful close APIs only from the explicit palette
  command paths.

L588. Inline ghost completion dismissal has the same cleanup-versus-command
split. Cursor movement and edits dismiss stale ghost text constantly, so those
cleanup calls must remain silent, but the explicit palette command should say
whether visible ghost text was actually present.

- **IDE note:** `mui_ghost_dismiss` remains the silent cleanup helper. New
  `mui_ghost_dismiss_command` returns `1` when it dismisses visible ghost text
  and `0` when no ghost completion is visible. The active path reports
  `AI ghost completion dismissed`; the no-op path reports
  `No AI ghost completion visible`; both messages stay in the AI toast lane, and
  Mighty uses the stateful ABI only for `AI: Dismiss Ghost Completion`.

L589. Stop commands should report whether they actually stopped work. A Testing
stop command that kills a running process and an idle stop command are different
outcomes, and both should be visible from the command palette.

- **IDE note:** `mui_test_stop` now returns `1` when it stops an active test run
  and `0` when Testing is idle. Active stop opens/focuses Testing and reports
  `Test run stopped`; idle stop still opens/focuses Testing and reports
  `No test run to stop`. Both messages stay in the Test toast lane, and Mighty
  explicitly discards the returned state from shortcut and palette paths.

L590. Web stop should expose the same state contract as other stop commands.
The Web Playground already reported active and idle outcomes, but a void ABI
still hid the difference from Mighty call sites and source guards.

- **IDE note:** `mui_web_stop` now returns `1` when it stops a running Web
  Playground server and `0` when no server is running. It keeps the existing
  `Web server stopped` / `No web server running` messages in the WebRun toast
  lane, and Mighty explicitly discards the returned state from header and
  palette command paths.

L591. Debug stop should match the stateful stop-command contract too. A stopped
or running debug session is an active target for Stop; an idle debugger is a
no-op, and callers should be able to distinguish those outcomes.

- **IDE note:** `mui_dbg_stop` now returns `1` when it stops a running or paused
  debug session and `0` when no debug session is available. Active stop reports
  `Debug session stopped`; idle stop continues to report
  `No debug session to stop`. Both messages stay in the Debug toast lane, and
  Mighty explicitly discards the returned state from Shift+F5 and palette paths.

L592. Run stop should be stateful like the other work-stopping commands. A
running process is an active target, while an idle Run panel is a no-op that
still deserves visible feedback from the palette.

- **IDE note:** `mui_run_stop` now returns `1` when it stops an active run
  process and `0` when no process is running. Active stop reports
  `Run process stopped`; idle stop continues to open Run and report
  `No run process to stop`. Both messages stay in the WebRun toast lane, and
  Mighty explicitly discards the returned state from the palette path.

L593. Debug pause and step commands should return state like Continue. Palette
commands and shortcuts can ignore that scalar today, but the ABI should still
make the result observable and easy to guard.

- **IDE note:** `mui_dbg_pause`, `mui_dbg_step_over`, `mui_dbg_step_into`, and
  `mui_dbg_step_out` now return the current debug state after the action. Their
  existing unavailable-action messages are unchanged, and Mighty explicitly
  discards the returned state from keyboard and palette paths.

L594. Welcome close needs a command-facing path distinct from silent dismissal.
Internal transitions should silently leave Welcome, but the visible close button
and palette command must close both forced Welcome and the automatic empty-buffer
Welcome state.

- **IDE note:** `mui_welcome_close` now returns `1` when a Welcome surface was
  visible and `0` when it was already closed. Active close reports
  `Welcome closed`; no-op close reports `Welcome is already closed`. The helper
  hides automatic empty-buffer Welcome too, while `mui_welcome_dismiss` remains
  the silent internal transition for file-opening and typing flows.

L595. Editor commands that can fail or no-op should return changed-state before
Mighty marks the tab dirty. Read-only previews already block mutation shim-side,
but void edit ABIs still let shortcut and palette paths assume success.

- **IDE note:** `mui_ed_toggle_comment`, `mui_ed_duplicate`,
  `mui_ed_move_lines_up`, `mui_ed_move_lines_down`,
  `mui_ed_delete_word_left_multi`, and `mui_ed_delete_word_right_multi` now
  return `1` only when the text changed. Read-only preview attempts return `0`
  and report `Edit is unavailable in read-only previews`; Mighty gates
  dirty/ghost updates on that scalar for the matching shortcut and palette
  paths.

L596. Ordinary typing needs the same changed-state contract as explicit edit
commands. Treating printable input, Backspace/Delete, and Enter as always
successful creates stale dirty, completion, and inline-AI follow-up work when
the editor model declined the edit.

- **IDE note:** `mui_ed_insert_char`, `mui_ed_backspace`, `mui_ed_delete`,
  `mui_ed_newline`, `mui_ed_newline_indent`, `mui_ed_insert_char_multi`,
  `mui_ed_insert_smart_multi`, `mui_ed_backspace_multi`,
  `mui_ed_delete_multi`, and `mui_ed_newline_indent_multi` now return `1` only
  when text changed. The main typing, autocomplete, Backspace/Delete, and Enter
  paths gate dirty/completion/ghost follow-up work on that scalar, and read-only
  previews reuse the same `Edit is unavailable in read-only previews` warning.

L597. In-file Replace Enter handling should consume the replace ABI result.
`mui_replace_next` and `mui_replace_all` already distinguish replacements from
empty queries, no matches, and read-only previews; the caller must not dirty the
tab unless the count is positive.

- **IDE note:** The Find & Replace Enter path now stores the result from
  `mui_replace_next` / `mui_replace_all` and only calls `mui_tab_set_dirty` when
  that value is greater than zero. This keeps no-match and read-only replace
  attempts from creating false dirty state.

L598. Completion, snippet, and ghost acceptance are edits too. Their ABIs should
refuse read-only previews before consuming state, and Mighty should dirty the tab
only after the accept call reports that text was inserted or expanded.

- **IDE note:** Completion accept now reports `0` for unchanged text and uses
  the shared read-only edit warning. Ghost accept, ghost word-accept, snippet
  prefix expand, snippet placeholder replace, and snippet completion expand now
  refuse read-only previews before mutating or consuming suggestions. Mighty
  gates completion and ghost dirtying on the returned accept state.

L599. Direct Tab snippet expansion needs a preflight before mutation. The
expansion ABI mutates the editor model, so the Tab handler must know that a
snippet is available before it can record the undo checkpoint for the expansion.

- **IDE note:** `mui_snippet_can_expand` now exposes a pure prefix check for the
  active cursor. The Tab handler uses it to record undo immediately before
  `mui_snippet_try_expand`, then marks dirty only when the expansion succeeds.
  This makes direct snippet expansion undoable as a single edit without adding
  undo checkpoints to ordinary Tab indentation misses.

L600. Format Document needs an undo preflight after save. The formatter ABI is
stateful because it reports user-facing outcomes, but unsupported targets should
not add an undo checkpoint before that no-op feedback is shown.

- **IDE note:** `mui_format_can_current` now exposes a silent file-backed,
  editable `.mty` preflight. `do_format` saves first, checks that preflight, and
  records the undo snapshot only for real format attempts while still calling
  `mui_format_current` for existing failure toasts.

L601. Replace Enter needs a match preflight before undo. In-file replace actions
already return replacement counts, but the key path still needs to know whether
an undo snapshot is warranted before the stateful replace ABI emits no-match or
read-only feedback.

- **IDE note:** `mui_replace_can_next` and `mui_replace_can_all` now provide
  silent non-empty, editable, has-match checks. The replace Enter handler records
  undo only when that preflight is true, while still calling the stateful replace
  ABI so empty searches, no matches, and read-only previews keep their visible
  feedback.

L602. Ghost completion accept needs an editable-buffer preflight. `mui_ghost_has`
only means a suggestion is visible; accepting it can still be rejected when a
read-only preview is focused, so the key path needs a stricter predicate before
recording undo.

- **IDE note:** `mui_ghost_can_accept` now reports a visible ghost on an editable
  active tab without emitting feedback. Tab full-accept and Ctrl+Right
  word-accept use it before `mui_ed_undo_record`, while the existing accept ABIs
  keep the read-only warning and no-op behavior.

L603. Completion accept needs a dropdown editability preflight. The selected
candidate can be visible while the focused tab is read-only or otherwise unable
to change; keep the warning in the stateful accept ABI, but gate undo snapshots
through a silent `can_accept` predicate.

L604. Language action commits need cheap target preflights. Rename and code
action apply are stateful because they may call LSP, touch disk, or report
feedback, but their key paths can still avoid undo snapshots for known no-op
states such as unchanged names, missing files, read-only previews, or empty
actions.

L605. Line-range commands need boundary preflights before undo. Move-line up/down
can be valid commands that do nothing at the top or bottom of a file; route
those through a silent range predicate so stateful read-only feedback remains
intact without adding empty undo snapshots at file edges.

L606. Line deletion and joining need no-op preflights too. Deleting an already
empty single-line buffer or joining the final line should not create undo
history, but the stateful edit ABI should still own read-only feedback so all
mutating editor commands behave consistently.

L607. Outdent needs an indentation preflight before undo. Shift+Tab is a
mutating route only when at least one affected line has removable leading
whitespace; the ABI should still own read-only feedback, while Mighty should
skip undo snapshots for no-indent ranges.

L608. Cut needs a clipboard-free mutation preflight. Empty single-line buffers
should not gain undo history just because Cut was invoked, and read-only
previews must reject Cut/Paste before any clipboard access or nonstandard
feedback.

L609. Boundary deletes need model-clone preflights. Backspace, Delete, and
word-delete can be valid commands that do nothing at document edges; use a
silent cloned edit probe for undo routing while the real edit ABI keeps
read-only feedback and changed-state ownership.

L610. Paste undo routing needs a silent clipboard preflight. Empty or
unavailable clipboard reads should not create editor undo checkpoints; keep
the visible clipboard/read-only feedback in Paste itself and gate snapshots
through a no-toast `can_paste` query.

L611. Snippet expansion preflights must include editability. A prefix match in
a read-only preview is not enough to justify an undo checkpoint; keep the
warning in `try_expand`, but make `can_expand` silently return false when the
active tab cannot be edited.

L612. Some commands only need editability preflights. Duplicate and Toggle
Comment always mutate editable text buffers, but read-only previews still need
silent `can_*` gates so shortcut and palette routes do not snapshot undo before
the stateful ABI reports the warning.

L613. Text entry needs a generic editability preflight. Printable typing and
Enter always intend to mutate, but read-only previews must not receive undo
checkpoints before insert/newline ABIs reject the edit; use a silent `can_edit`
gate for those baseline text-entry routes.

L614. Terminal reverse-index escapes must honor scroll margins. The VT parser
already handled CSI scrolling and line erases, but single-byte `ESC D`, `ESC E`,
and especially `ESC M` were consumed as no-ops. Full-screen terminal programs
use these index and reverse-index controls to move within active margins, so
skipping them can leave alternate-screen layouts with stale or misplaced rows.

- **IDE note:** the terminal grid now implements VT Index, Next Line, and
  Reverse Index, including scroll-region-aware top/bottom margin behavior.
  Parser tests cover normal movement, row preservation, and margin-local
  scrolling without leaking escape bytes into the visible grid.

L615. Indent undo routing must preflight editability too. Outdent already had a
no-indent preflight, but the plain indent routes still recorded undo before the
stateful edit ABI rejected read-only previews. That left a false checkpoint
behind a visible "edit unavailable" warning.

- **IDE note:** Tab indentation and the palette `Indent Line/Selection` command
  now call the generic editable-buffer preflight before recording undo, while
  Shift+Tab and Outdent keep their stricter no-indent preflight. The Mighty
  route regression test now asserts both checks.

L616. Terminal autowrap is a mode, not a constant. The parser always wrapped
printable output at the right margin, even after applications sent `CSI ?7l` to
disable DEC autowrap. Full-screen terminal UIs use that mode to paint status
regions and right-edge cells without scrolling or shifting rows unexpectedly.

- **IDE note:** `VtParser` now tracks DEC autowrap mode, honors `CSI ?7 h/l`,
  and resets it on `ESC c`. Printable UTF-8 output uses the mode-aware grid
  write path, with tests for disabled wrap, re-enabled wrap, and terminal reset.

L617. Terminal tab stops are mutable state. The terminal grid advanced every
Tab to a hard-coded multiple-of-eight column and ignored `ESC H` / `CSI g`,
which means applications could not set or clear custom tab stops for aligned
output.

- **IDE note:** the grid now stores horizontal tab stops, defaults them every
  eight columns, supports HTS (`ESC H`) and TBC (`CSI g` / `CSI 3 g`), and
  restores default tab stops on full terminal reset. Parser tests cover custom
  stops, clearing the current stop, clearing all stops, and reset behavior.

L618. Terminal origin mode must pair with scroll regions. The parser supported
scroll margins, but ignored DEC origin mode (`CSI ?6 h/l`), so `CUP`/`HVP`
always positioned against absolute screen rows. Full-screen terminal programs
use origin mode to address rows inside the active scroll region.

- **IDE note:** `VtParser` now tracks origin mode, homes the cursor on
  DECSET/DECRST `?6`, and applies margin-relative row coordinates to `CUP`/`HVP`
  while clamping to the scroll-region bottom. Tests cover origin-relative
  positioning, bottom-margin clamping, and returning to absolute coordinates.

L619. Terminal tab-stop commands include CSI movement too. After tab stops
became mutable, the parser still ignored cursor-forward-tab (`CSI I`) and
cursor-backward-tab (`CSI Z`), so applications could set stops but not use the
standard counted movement commands that jump between them.

- **IDE note:** the terminal grid now supports counted forward/backward tab
  movement through the stored tab stops, and the parser routes `CSI I` / `CSI Z`
  through those helpers. Tests cover default stops, counted backward movement,
  and custom-only tab stops after clearing defaults.

L620. Terminal escape strings must be swallowed as strings. OSC was protected,
but DCS/PM/APC/SOS introducers fell back to ordinary ESC handling, which let
payload bytes draw as visible garbage until ST.

- **IDE note:** `VtParser` now has a shared non-OSC string state for DCS
  (`ESC P`), SOS (`ESC X`), PM (`ESC ^`), and APC (`ESC _`). Payload bytes are
  consumed until the `ESC \` string terminator, with parser tests covering DCS,
  APC, PM, and SOS payloads that should not reach the visible grid.

L621. Terminal string cancellation must return to ground. CAN (`0x18`) and SUB
(`0x1A`) abort in-progress control strings; treating them as ordinary payload
bytes can swallow all following printable output until a later ST happens to
arrive.

- **IDE note:** OSC and DCS/PM/APC/SOS string states now abort on CAN/SUB from
  both their normal and post-ESC substates. Parser tests cover cancelled OSC,
  DCS, PM, and SOS payloads and assert that the text after cancellation remains
  visible.

L622. Terminal C1 controls have 8-bit aliases. Supporting only the 7-bit
`ESC`-prefixed forms (`ESC [`, `ESC ]`, `ESC P`, etc.) misses streams that use
single-byte CSI/OSC/DCS/SOS/PM/APC/ST controls directly.

- **IDE note:** ground-state parsing now recognizes 8-bit CSI (`0x9B`), OSC
  (`0x9D`), DCS (`0x90`), SOS (`0x98`), PM (`0x9E`), and APC (`0x9F`) as the
  same states as their 7-bit introducers, and OSC/non-OSC string states accept
  8-bit ST (`0x9C`) as a terminator. Tests cover cursor movement and swallowed
  string payloads through the C1 forms.

L623. Terminal C1 movement controls need the same aliases as C1 strings. IND,
NEL, HTS, and RI can arrive as single-byte C1 controls (`0x84`, `0x85`,
`0x88`, `0x8D`), not only as `ESC D`, `ESC E`, `ESC H`, and `ESC M`.

- **IDE note:** ground-state parsing now routes those C1 bytes through the
  existing index, next-line, horizontal-tab-stop, and reverse-index helpers.
  Parser tests cover newline/index placement, reverse-index movement, and a
  custom C1-created tab stop.

L624. CSI parsing must recover into replacement controls, not just ground.
When an incomplete CSI is interrupted by a new ESC or C1 introducer, dropping
straight to ground makes the next bytes printable (`[` / digits / payload)
instead of honoring the replacement control sequence.

- **IDE note:** `VtParser` now clears the partial CSI and transitions to
  `Escape`, `Csi`, `Osc`, or the non-OSC string state when those introducers
  arrive during CSI parsing; CAN/SUB still abort to ground. Tests cover
  interrupted 7-bit CSI, interrupted 8-bit CSI, and CSI replaced by C1 OSC.

L625. CSI parsing must tolerate embedded C0 controls. BEL, CR, LF, and the
other non-cancel C0 bytes can appear while a CSI is being parsed; treating them
as malformed sequence bytes drops to ground and can leak the final CSI byte as
visible text.

- **IDE note:** `VtParser` now consumes non-cancel C0 controls inside CSI
  without clearing the partial parameters. CAN/SUB still abort, ESC and C1
  introducers still replace the sequence, and parser tests cover BEL/CR inside
  cursor-column commands without leaking the final `G`.

L626. String substates must accept every ST form. OSC and DCS-style strings
already terminated on 8-bit ST (`0x9C`) in their normal payload state, but the
post-ESC substates only recognized `ESC \`, CAN/SUB, and OSC BEL. An `ESC`
followed by 8-bit ST could therefore keep swallowing later terminal output.

- **IDE note:** OSC-ESC and DCS/PM/APC/SOS-ESC substates now treat `0x9C` as
  string termination too. Parser tests cover OSC, DCS, and APC payloads ending
  through `ESC 0x9C` with following printable output preserved.

L627. Terminal input mapping needs modifier-aware special keys. Plain Tab is a
literal HT byte, but Shift+Tab is BackTab (`CSI Z`) for shells and full-screen
terminal apps that navigate fields or panes in reverse.

- **IDE note:** `key_to_bytes` now uses the existing modifier argument for Tab:
  plain Tab remains `\t`, while Shift+Tab emits `ESC [ Z`. The terminal
  key-mapping regression test pins both forms.

L628. Terminal navigation keys need xterm modifier parameters. Ignoring
Shift/Alt/Ctrl on arrows, Home/End, Delete, and Page keys makes full-screen
terminal apps lose selection, word/pane movement, and modified navigation
gestures.

- **IDE note:** `key_to_bytes` now emits xterm-style modified CSI sequences for
  navigation keys (`CSI 1;N A/B/C/D/H/F`, `CSI 3;N~`, `CSI 5;N~`, `CSI 6;N~`).
  Plain application-cursor arrows still use SS3, while modified arrows use CSI
  modifier sequences. Tests cover Shift, Alt, Ctrl, combined modifiers, and
  application-cursor interaction.

L629. Terminal function keys need the same xterm discipline. Plain F2 is SS3
`ESC O Q` alongside F1/F3/F4, not `CSI 12~`; modified function keys carry the
same xterm modifier parameter scheme as navigation keys.

- **IDE note:** terminal input now sends plain F2 as `ESC O Q`, modified F1-F4
  as `CSI 1;N P/Q/R/S`, and modified F5-F12 as `CSI code;N~`. Regression tests
  cover the corrected F2 plus representative Shift/Alt/Ctrl combinations.

L630. Terminal Alt text input is Meta, not plain text. Handling Ctrl+letter
control codes while ignoring Alt makes Alt-modified character chords reach
terminal apps as ordinary text.

- **IDE note:** `codepoint_to_bytes` now prefixes ESC for Alt-modified character
  input after computing the Ctrl/control-code or UTF-8 payload. Tests cover
  Alt+ASCII, Alt+Ctrl, and Alt+multibyte input.

L631. Terminal Meta input includes named editing keys, not just printable text.
Alt+Backspace is the common Meta-DEL chord that shell line editors use for
backward word deletion; sending plain DEL drops the modifier and degrades
terminal editing.

- **IDE note:** `key_to_bytes` now maps Alt+Backspace to `ESC DEL` while plain
  Backspace remains DEL. The key-mapping regression test covers both forms.

L632. Terminal named-key coverage must include Insert. Full-screen terminal apps
and shell line editors expect Insert as `CSI 2~` with the same xterm modifier
parameters used by Delete and Page keys; omitting the named key makes the chord
unrepresentable at the FFI boundary.

- **IDE note:** the key ABI/header and winit mapper now expose `MUI_KEY_INSERT`.
  `key_to_bytes` sends plain Insert as `CSI 2~` and modified Insert as
  `CSI 2;N~`. Tests cover ABI/header parity, named-key mapping, and terminal
  byte encoding.

L633. Terminal Meta input also applies to control-style named keys. Alt+Enter
and Alt+Tab should not collapse to plain CR or HT; line editors and terminal
programs can distinguish those Meta chords only when the ESC prefix is preserved.

- **IDE note:** `key_to_bytes` now maps Alt+Enter to `ESC CR` and Alt+Tab to
  `ESC TAB`, while plain Enter/Tab and Shift+Tab keep their existing encodings.
  Regression assertions cover all affected forms.

L634. ESC intermediates must consume their final byte. Charset selectors like
`ESC ( B` and screen-alignment sequences like `ESC # 8` include an intermediate
byte before the final. Consuming only the intermediate returns the parser to
ground too early and leaks the final byte into the visible terminal grid.

- **IDE note:** the VT parser now has an ESC-intermediate state. Charset
  selectors are consumed without drawing their final byte, and `ESC # 8`
  performs DEC screen alignment by filling the grid with `E`. Regression tests
  cover both paths.

L635. Terminal capability probes need replies, not silent consumption. Device
Attributes queries (`CSI c` and `CSI > c`) are common startup probes; swallowing
them without a response can leave probing applications waiting or force them
into degraded fallback behavior.

- **IDE note:** the VT parser now queues minimal primary and secondary DA
  replies (`CSI ?1;2c` and `CSI >0;0;0c`). Regression tests cover empty/zero
  primary queries and secondary `>`/`>0` forms without leaking query bytes into
  the grid.

L636. Cursor movement has legacy CSI aliases. `CSI Ps a` (HPR) is the horizontal
position-relative form of Cursor Forward; skipping it consumes the sequence but
leaves later text at the old column.

- **IDE note:** `VtParser` now routes `CSI a` through the same clamped relative
  cursor movement as `CSI C`. The cursor movement regression test covers normal
  HPR movement and right-edge clamping without leaking the sequence bytes.

L637. Backward cursor movement has ECMA CSI aliases too. `CSI Ps j` (HPB) and
`CSI Ps k` (VPB) are legacy horizontal/vertical backward movement forms; a
terminal that consumes them as unknown finals leaves subsequent text at the
wrong position.

- **IDE note:** `VtParser` now maps `CSI j` to Cursor Backward and `CSI k` to
  Cursor Up using the existing clamped relative movement helper. Regression
  coverage verifies both aliases and confirms their bytes do not leak into the
  visible grid.

L638. REP must duplicate the previous graphic cell, not leak as text. Terminal
renderers use `CSI Ps b` to compact repeated runs of the same glyph; consuming it
as an unknown CSI leaves sparse prompts or progress UI when applications rely on
repeat-character output.

- **IDE note:** `VtParser` now tracks the last printable cell and implements
  `CSI b` by replaying that cell with its original foreground/background colors
  under the current autowrap mode. Regression coverage checks count defaults,
  color preservation after later SGR changes, and the no-previous-character case.

L639. Mouse reporting modes need a non-SGR wheel path. Full-screen terminal apps
can enable mouse reporting with `CSI ?1000 h` without also enabling SGR extended
coordinates; treating that as ordinary shell scroll sends arrow keys into the
app instead of wheel events.

- **IDE note:** terminal scroll encoding now distinguishes generic mouse
  reporting from SGR mouse reporting. `CSI ?1000 h` uses legacy X10 wheel bytes,
  `CSI ?1006 h` keeps the SGR wheel form, and ordinary shells still receive the
  repeated cursor-key fallback. Tests cover mode tracking and all three encoders.

L640. SGR truecolor also appears in colon form. Modern terminal programs may emit
`CSI 38:2::r:g:b m`, `CSI 48:2:r:g:b m`, or `CSI 38:5:n m`; a parser that only
splits on semicolons silently drops those colors even though the sequence is a
standard SGR color update.

- **IDE note:** the SGR parser now normalizes colon-delimited extended-color
  parameters into the existing 256-color and truecolor handling path, including
  the optional color-space field in truecolor forms. Regression tests cover
  colon 256-color foreground/background, colon truecolor foreground/background,
  reset behavior, and color-space-id forms.

L641. Private DSR cursor probes need private replies. Some terminal programs ask
for cursor position with DEC private `CSI ?6 n`; answering only standard `CSI 6 n`
leaves those probes unanswered even though the terminal already knows the cursor
position.

- **IDE note:** `VtParser` now answers `CSI ?6 n` with the DEC private cursor
  report form `CSI ?row;col R`, using the same 1-based cursor coordinates as the
  standard DSR reply. Regression coverage verifies the queued reply and confirms
  the query bytes do not leak into the visible grid.

L642. Meta named keys must keep both their semantic key sequence and the Meta
prefix. `Alt+Shift+Tab` is not the same as plain `Shift+Tab`, and `Alt+Escape`
is not distinguishable from plain Escape if the second escape byte is dropped.

- **IDE note:** terminal key encoding now emits Meta-Shift-Tab as
  `ESC ESC [ Z` and Meta-Escape as `ESC ESC`, while preserving the existing plain
  Tab, Shift+Tab, Escape, and Meta-Tab mappings. Regression coverage locks the
  named-key Meta combinations alongside the existing terminal input assertions.

L643. Terminal focus reporting is stateful, not a key event. TUIs can enable
xterm focus reports with `CSI ?1004 h`; after that, focus changes must arrive as
`CSI I` and `CSI O` on stdin, and duplicate reports should be suppressed until
the IDE focus state actually changes.

- **IDE note:** `VtParser` now tracks private mode `?1004`, `Terminal` records
  the IDE's terminal-focus state and reports it only when the mode is enabled and
  the reported state changes, and the Mighty frame loop publishes `term_focus`
  after terminal pump/liveness checks. Tests cover mode tracking, reset behavior,
  and the exact xterm focus-report byte sequences.

L644. Origin mode must constrain relative vertical cursor motion. With a scroll
region active and DECOM (`CSI ?6 h`) enabled, full-screen TUIs expect CUU/CUD,
VPR, CNL, and CPL to stay inside the top/bottom margins instead of escaping into
rows reserved outside the viewport.

- **IDE note:** origin-mode relative vertical moves now clamp to the active
  scroll region while horizontal motion still spans the full row. Regression
  tests cover large upward/downward CUU/CUD/VPR motions and CNL/CPL line motions
  at both margins.

L645. VPA follows origin mode for its row coordinate. `CSI d` changes only the
cursor row, but when DECOM is active that row is still relative to the scroll
region; treating it as an absolute screen row lets TUIs write outside their
reserved viewport.

- **IDE note:** `VtParser` now routes VPA through a row-only origin-mode helper
  when `CSI ?6 h` is active, preserving the current column while clamping rows to
  the active scroll-region margins. Regression coverage verifies top-margin
  mapping and bottom-margin clamping.

L646. ED 3 clears scrollback, not the visible screen. Many shells emit
`CSI 3 J` as an optional scrollback clear after an ordinary screen clear; in a
terminal without scrollback storage, the correct behavior is to consume it
without erasing visible cells.

- **IDE note:** erase-display mode `3` is now a visible no-op instead of an alias
  for `CSI 2 J`. Regression coverage verifies the escape is consumed while the
  current grid contents remain intact.

L647. RIS must discard alternate-screen snapshots. `ESC c` is a full terminal
reset; if it occurs while a TUI is on the alternate screen, a later
`CSI ?1049 l` must not resurrect the stale primary screen that existed before
the reset.

- **IDE note:** clearing the terminal grid now also drops any saved primary
  screen snapshot. Regression coverage enters the alternate screen, performs RIS,
  exits alternate mode, and verifies neither stale primary nor alternate content
  returns.

L648. DECSTBM homes relative to origin mode. Setting a scroll region with
`CSI top;bottom r` homes the cursor; when DECOM (`CSI ?6 h`) is active, that home
position is the top margin of the scroll region, not absolute screen row 1.

- **IDE note:** scroll-region updates now report whether the margins were valid,
  and `VtParser` rehomes to origin-mode row 1 only after an accepted update.
  Regression coverage verifies valid DECOM margin changes land at the top margin
  and invalid margins do not move the cursor.

L649. Cursor save/restore includes rendition state. DEC `ESC 7`/`ESC 8`, CSI
`s`/`u`, and private `?1048` save/restore flows are expected to restore the
active SGR colors along with row and column, so a TUI can temporarily change
colors and return to the prior drawing style.

- **IDE note:** saved cursor state now records foreground and background color
  alongside the cursor coordinates. Regression coverage verifies DEC, CSI, and
  private cursor restore forms reinstate the saved colors before subsequent text
  is drawn.

L650. Cursor save/restore also carries VT mode state. Saved cursor snapshots
need to include DECAWM autowrap and DECOM origin mode, because those modes
change how the next printable cell or cursor-position command behaves after a
restore.

- **IDE note:** saved cursor state now records autowrap and origin mode with
  the coordinates and SGR colors. Regression coverage verifies restore
  reinstates nowrap before right-margin output and origin mode before a later
  CUP move.

L651. Xterm mouse modes are independent private modes. `?1000`, `?1002`, and
`?1003` can overlap, so disabling one must not clear mouse reporting while
another remains enabled. `?1006` only selects SGR mouse encoding when a reporting
mode is active.

- **IDE note:** terminal mouse tracking now keeps separate mode bits for button,
  drag, and any-motion reporting. Regression coverage verifies overlapping mouse
  modes survive independent disables and SGR encoding follows the aggregate
  reporting state.

L652. Mouse wheel reports need the event cell. Xterm wheel events encode the
terminal cell under the pointer; hardcoding `1;1` makes full-screen TUIs receive
scrolls at the wrong location, especially in split panes and mouse-sensitive
views.

- **IDE note:** terminal scroll routing now maps the last scroll pixel to a
  clamped 1-based terminal row/column before encoding legacy X10 or SGR wheel
  reports. Regression coverage verifies coordinate-specific SGR and legacy
  wheel bytes, plus X10 coordinate clamping.

L653. Terminal body clicks should route to mouse-aware TUIs. When the integrated
terminal is open, a click inside the terminal grid needs to focus the terminal
and, if the app enabled xterm mouse reporting, emit the matching button press or
release at the event cell.

- **IDE note:** terminal hit-testing now recognizes grid-body clicks after
  chrome/dock controls have priority, and the terminal encoder supports legacy
  X10 and SGR button press/release reports. Regression coverage verifies button
  bytes, unsupported buttons, disabled reporting, and legacy coordinate clamps.

L654. Terminal mouse motion needs a shim-side escape hatch. Mighty normally
consumes hover moves before script dispatch, so terminal drag/any-motion reports
must explicitly pass through only while an app has requested them.

- **IDE note:** terminal mouse motion now preserves the pressed button for
  drag tracking, reports SGR/X10 motion bytes for `?1002`/`?1003`, and lets the
  shim forward grid moves only when the terminal is actively requesting motion.
  Regression coverage verifies drag, any-motion, disabled reporting, and
  coordinate clamping.

L655. Terminal mouse reports must preserve keyboard modifiers before global
gestures claim the event. Ctrl+wheel is an IDE zoom gesture, but inside a
mouse-aware terminal grid it is also valid xterm mouse input.

- **IDE note:** terminal wheel, button, and motion encoders now add xterm
  Shift/Meta/Ctrl modifier bits to SGR and legacy X10 reports, and Ctrl+wheel
  passes through to the terminal when a mouse-reporting terminal app owns the
  grid hit. Regression coverage verifies modified scroll, press, release,
  drag, and any-motion byte sequences.

L656. Terminal wheel routing should be pointer-driven, not only focus-driven.
After a terminal app requests mouse reporting, a wheel over the terminal grid
must reach the terminal even if the editor still has keyboard focus.

- **IDE note:** the default Mighty scroll arm now hit-tests the terminal grid
  before editor scrolling. A wheel over the terminal focuses the terminal and
  sends the coordinate-aware terminal scroll report/fallback; wheels elsewhere
  keep the editor's existing first-line scroll behavior.

L657. OSC 0/1/2 title payloads are terminal state, not printable content.
Shells and TUIs use OSC titles to identify the active job or project; consuming
them protects the grid, but discarding them loses useful context.

- **IDE note:** the VT parser now captures bounded, sanitized OSC window/icon
  titles terminated by BEL, ST, or C1 ST, ignores unknown OSC kinds, and the
  terminal header displays the fitted title beside `TERMINAL`. Regression
  coverage verifies title capture, grid non-leakage, sanitizing, and unknown OSC
  handling.

L658. Syntax-highlighted line segments should share the renderer's width model.
The editor's fallback line drawer split a leading keyword from the rest of the
line, then positioned the remainder with a fixed cell estimate. Even when the
current code font is grid-aligned, the safer contract is to place adjacent
styled segments from the measured width of the text that was actually queued.

- **IDE note:** `mui_draw_buffer_self` now positions the unhighlighted suffix
  from the measured rendered width of the highlighted keyword token. A focused
  regression pins the measured-offset helper so future font or shaping changes
  do not reintroduce fixed-cell segment drift.

L659. Cursor tabulation control is part of the terminal tab-stop contract.
TUIs can use CSI CTC sequences to set and clear horizontal tab stops without
falling back to the older ESC HTS/TBC pair. Treating CSI `W` as unknown silently
drops intent and can leave subsequent tab movement at the default stops.

- **IDE note:** the VT parser now handles CSI `W`, `2W`, and `5W` by setting
  the current tab stop, clearing the current stop, or clearing all stops.
  Regression coverage verifies the sequences are consumed and that following
  tab movement lands on the custom, default, or right-edge positions.

L660. Terminal scroll-down aliases should share one behavior path. ECMA-48 and
xterm-compatible apps can emit both CSI `T` and CSI `^` for scroll-down; handling
only one leaves the other as a consumed no-op, so viewport history appears to
ignore a valid terminal command.

- **IDE note:** CSI `^` now routes through the same scroll-down implementation
  as CSI `T`, including parameter parsing and active scroll-region clipping.
  Regression coverage verifies full-screen and margin-limited scroll-down output
  and confirms the alias bytes do not leak into the grid.

L661. Cursor blink mode is terminal state even before rendering animates it.
Xterm-compatible apps use private mode `?12` to request blinking or steady
cursors, and can query it with DECRQM. Ignoring the mode makes status replies
look unsupported and loses cursor intent across save/restore boundaries.

- **IDE note:** the VT parser now tracks `CSI ?12 h/l`, includes it in cursor
  save/restore and alternate-screen snapshots, resets it through RIS/DECSTR, and
  reports it via `CSI ?12 $ p`. Regression coverage pins the toggle, query,
  reset, and cursor snapshot behavior.

L662. Protocol state should reach the visible terminal surface. Tracking cursor
blink mode is incomplete if the terminal renderer still paints a solid cursor
every frame; terminal apps request blink or steady modes to communicate focus and
input state.

- **IDE note:** terminal cursor drawing now includes the tracked `?12` blink
  mode and uses the existing frame counter for a deterministic visible/hidden
  phase. A focused helper test pins hidden cursors, steady cursors, and the
  30-frame blink cadence without requiring a graphics capture.

L663. DECSCUSR carries blink intent, not only cursor shape. Xterm's cursor-style
values pair odd numbers with blinking cursors and even numbers with steady
cursors, so treating `CSI Ps SP q` as shape-only loses app intent even when `?12`
mode is tracked separately.

- **IDE note:** the VT parser now maps DECSCUSR `1/3/5` to blinking
  block/underline/bar and `2/4/6` to steady block/underline/bar. Regression
  coverage asserts both shape and blink state for every supported cursor-style
  variant.

L664. OSC color setters are state, not disposable control strings. Apps can set
default foreground/background/cursor colors with `OSC 10/11/12` and later query
them, so consuming setters without updating query state returns stale terminal
identity.

- **IDE note:** the VT parser now tracks `OSC 10/11/12` color setters for
  subsequent query replies, accepting both `#rrggbb` and `rgb:` component forms.
  Invalid setters and unknown OSC color queries are still consumed without
  leaking payload text into the terminal grid.

L665. Palette queries need palette state. `OSC 4` can update indexed colors and
`OSC 104` can reset them, so answering every palette query from a static table
misreports terminals that apps have already customized.

- **IDE note:** the VT parser now keeps a mutable 256-color palette for `OSC 4`
  query replies, updates entries from valid palette setters, and restores one or
  all entries through `OSC 104`. Regression coverage pins multi-entry setters,
  selective reset, full reset, and invalid payload consumption.

L666. Dynamic default colors need their reset controls too. Tracking
`OSC 10/11/12` setters is incomplete without `OSC 110/111/112`, because terminal
apps use those paired resets to restore foreground/background/cursor query
identity after temporary theme changes.

- **IDE note:** the VT parser now consumes `OSC 110/111/112` as foreground,
  background, and cursor color resets, restoring subsequent `OSC 10/11/12`
  query replies to Mighty IDE's built-in defaults. Regression coverage verifies
  the resets across BEL, 7-bit ST, and 8-bit OSC/ST forms.

L667. Terminal color state has to feed rendering, not only replies. A parser can
truthfully answer OSC color queries while the visible grid still paints from a
static palette, which makes theme-aware TUIs look wrong even though probe
responses are correct.

- **IDE note:** terminal drawing now resolves foregrounds, backgrounds, palette
  entries, and cursor color through the live parser state. `OSC 4`,
  `OSC 10/11/12`, `OSC 104`, and `OSC 110/111/112` therefore affect the rendered
  terminal surface as well as query replies, with tests pinning dynamic draw
  color resolution and reset behavior.

L668. Full reset must reset identity state, not only screen cells. `ESC c`
clearing the grid while preserving current SGR attributes or OSC theme/title
state leaves the next prompt in a stale terminal personality.

- **IDE note:** RIS now resets current SGR foreground/background, dynamic OSC
  default colors, cursor color, palette entries, and terminal title alongside
  modes and the visible grid. Regression coverage verifies later text uses
  default attributes and post-reset color queries/render resolution return to
  built-in defaults.

L669. Ignoring SGR bold makes common prompts look under-specified. Many terminal
themes rely on `SGR 1` with basic ANSI foregrounds, and when the renderer has no
font-weight path yet, mapping bold basic colors to their bright counterparts is
the pragmatic compatibility behavior.

- **IDE note:** the VT parser now tracks SGR bold intensity for subsequently
  written cells, maps basic foreground indices `0..7` to bright `8..15`, resets
  on `SGR 0`/`22`, and preserves the state across cursor save/restore. Tests pin
  bold-before-color, bold-after-color, non-basic color boundaries, and restored
  bold state.

L670. Reverse-video is selection UI, not decoration. TUIs use `SGR 7` for active
rows, menus, and selections, so ignoring it makes interactive terminal programs
lose their primary focus affordance.

- **IDE note:** the VT parser now tracks SGR inverse state, materializes it by
  swapping effective foreground/background for newly-written cells, resets it on
  `SGR 0`/`27`, and preserves it across cursor save/restore. Rendering also
  resolves the default-background sentinel as a foreground so inverted default
  text paints correctly.

L671. Underline needs to be a terminal cell attribute. Shell prompts, links, and
TUI diagnostics use `SGR 4`, and dropping it strips useful navigation and
selection cues even when the text and colors are otherwise correct.

- **IDE note:** terminal cells now carry an underline flag, `SGR 4/24` updates it
  for subsequent output, REP and cursor save/restore preserve it, and
  `mui_term_draw` paints underline runs with the resolved foreground color.
  Regression coverage pins SGR reset, repeated underlined cells, and restored
  underline state.

L672. Strikethrough is semantic terminal markup, not just text decoration.
Diagnostics, diffs, and status-heavy prompts use `SGR 9` to show removed,
invalidated, or superseded text. If the emulator drops it, the terminal can keep
the bytes and colors correct while still losing the meaning of the line.

- **IDE note:** terminal cells now carry a strikethrough flag, `SGR 9/29`
  updates it for subsequent output, REP and cursor save/restore preserve it, and
  `mui_term_draw` paints strikethrough runs with the resolved foreground color.
  Regression coverage pins SGR reset, repeated struck cells, and restored
  strikethrough state.

L673. Italic terminal text needs the real italic face path. Modern prompts,
diagnostic tools, and markdown-aware CLIs use `SGR 3` for comments, hints, and
secondary prose. Treating it as an ignored escape keeps the grid legible but
makes semantic emphasis disappear.

- **IDE note:** terminal cells now carry an italic flag, `SGR 3/23` updates it
  for subsequent output, REP and cursor save/restore preserve it, and terminal
  glyph runs split on italic state so `mui_term_draw` can queue the bundled true
  italic code face. Regression coverage pins SGR reset, repeated italic cells,
  restored italic state, and RIS clearing stale style attributes.

L674. Faint intensity is part of terminal information density. Prompts and CLIs
often use `SGR 2` for muted paths, inactive hints, and secondary metadata. If it
is ignored, the terminal loses visual hierarchy even when the text remains
readable.

- **IDE note:** terminal cells now carry a faint flag, `SGR 2` enables it, and
  `SGR 22` clears both bold and faint intensity as xterm-compatible terminals do.
  REP and cursor save/restore preserve faint cells, RIS clears stale faint state,
  and terminal glyph runs split on faint state so `mui_term_draw` can dim the
  resolved foreground alpha without changing stored color identity.

L675. Overline is another terminal cell decoration, not an escape to drop.
Formatters and diagnostics can use `SGR 53` for annotated headers or emphasized
spans. Ignoring it makes those cues disappear even though nearby underline and
strikethrough already render correctly.

- **IDE note:** terminal cells now carry an overline flag, `SGR 53/55` updates
  it for subsequent output, REP and cursor save/restore preserve it, RIS clears
  stale overline state, and `mui_term_draw` paints overline runs with the
  resolved foreground color.

L676. Conceal changes terminal visibility, not terminal storage. CLIs use
`SGR 8` for hidden prompts, masked tokens, and UI state that should occupy cells
without painting readable glyphs. Treating it as ordinary text leaks visual
content and breaks alignment assumptions.

- **IDE note:** terminal cells now carry a conceal flag, `SGR 8/28` updates it
  for subsequent output, REP and cursor save/restore preserve it, and RIS clears
  stale conceal state. `mui_term_draw` preserves concealed cell width while
  suppressing glyph, underline, strikethrough, and overline drawing for those
  cells.

L677. Blink is a timed terminal cell attribute. Some prompts and TUI alerts use
`SGR 5`/`6` for urgency, so parsing it without a draw-time phase still makes the
terminal miss state that other emulators expose.

- **IDE note:** terminal cells now carry a blink flag, `SGR 5/6` enables it,
  `SGR 25` disables it, REP and cursor save/restore preserve it, and RIS clears
  stale blink state. `mui_term_draw` reuses the frame counter's blink phase to
  suppress glyph and line-decoration drawing during the off phase while keeping
  cell width stable.

L678. OSC 8 hyperlinks are terminal metadata, not text to leak into the grid.
Prompts, test runners, and build tools emit clickable file, issue, and URL
references through OSC 8. If the terminal only consumes the bytes, it stays
legible but loses a modern navigation affordance.

- **IDE note:** terminal cells now carry a compact OSC 8 hyperlink id. `OSC 8;params;uri`
  marks subsequently-written cells until `OSC 8;;` clears it, ST and BEL
  terminators both work, REP and cursor save/restore preserve the
  metadata, and RIS clears stale link state. `mui_term_draw` underlines linked
  cells with the existing foreground-color decoration path so links are visible
  without painting escape payloads.

L679. Command names should not use punctuation as UI behavior. The file picker
commands are dialog-backed, but baking `...` into command names made Palette and
Keyboard Shortcuts rows look pre-truncated even when there was enough room.

- **IDE note:** the live command registry now uses clean action names for
  `File: New File` and `Explorer: New File in Workspace`. The dialog behavior
  remains in contextual descriptions and command handling, while tests pin the
  product wording so command surfaces do not drift back to faux truncation.

L680. Inline diff gutters should size to measured line numbers. A fixed
old/new-number gutter works on small files, but wide line numbers can crowd the
marker and code column even after diff headers and body text use measured
budgets.

- **IDE note:** inline diff drawing now measures the visible old/new line-number
  columns before rendering body rows, deriving the marker, divider, and code
  column from those widths. Focused geometry tests pin both measured label widths
  and expansion for large line numbers.

L681. Shortcut reset workflows should be command-visible. Keyboard shortcut
remapping exposes reset-selected and reset-all from inside the overlay, but
burying those actions behind overlay-local chords makes recovery hard for users
who discover settings through the command palette.

- **IDE note:** the command palette now lists `Keyboard Shortcuts: Reset
  Selected` and `Keyboard Shortcuts: Reset All`. Both dispatch through
  command-specific ABIs that reveal the shortcuts overlay, report changed versus
  already-default states through the shared Keyboard Shortcuts toast lane, and
  remain covered by registry, dispatcher, and behavior tests.

L682. Breakpoint control belongs beside the rest of Debug. A gutter click is a
good mouse shortcut, but command-first users expect to set or clear a breakpoint
without targeting the gutter. Debug lifecycle commands are already searchable,
so breakpoint toggling should be part of that same command surface.

- **IDE note:** the command palette now lists `Debug: Toggle Breakpoint at
  Cursor`, dispatching through a cursor-aware breakpoint ABI that opens Run and
  Debug and reports set, cleared, and unsaved-file outcomes. Tests pin the
  registry label, Mighty dispatcher route, visible feedback, panel focus, and
  breakpoint state changes.

L683. Breakpoint cleanup should not require per-line hunting. Once breakpoint
toggling is command-visible, users still need a safe way to remove all stored
breakpoints without revisiting every file and gutter marker. Clearing
breakpoints is separate from clearing the debug session, which intentionally
preserves breakpoints.

- **IDE note:** the command palette now lists `Debug: Clear Breakpoints`,
  dispatching through a dedicated breakpoint-clear ABI that opens Run and Debug,
  clears every stored breakpoint across files, resends the empty set to live
  debug sessions, and reports changed versus already-empty outcomes. Model, ABI,
  registry, and dispatcher tests pin the behavior.

L684. Breakpoint management needs a visible inventory. Commands can create and
clear breakpoints, but users need the Run and Debug view itself to answer "what
breakpoints exist?" without hunting through open files or trusting a toast.

- **IDE note:** the Run and Debug sidebar now includes a compact Breakpoints
  section above Call Stack. The debug model exposes sorted cross-file breakpoint
  locations, the sidebar lists file and line rows with a capped height, and click
  geometry keeps call-stack frame hit testing aligned below that section. Tests
  cover global sorting, section row budgeting, and click offset behavior.

L685. A breakpoint inventory should be navigable, not just visible. Once the
Run and Debug panel lists breakpoints, rows need to behave like other source
locations in the IDE: clicking a row should open the file and move the caret to
the breakpoint line.

- **IDE note:** breakpoint inventory rows now have their own encoded click range
  and activation ABI. Clicking a listed breakpoint opens or switches to the
  source tab, syncs the active path, moves the caret to the stored line, and
  scrolls nearby context into view. Tests cover row hit encoding, source opening,
  caret placement, and the Mighty click-dispatch route.

L686. Capped debugger lists should disclose overflow. Showing only the first few
breakpoints without a count makes it look like the rest were lost, especially
after a global clear or cross-file setup.

- **IDE note:** the Run and Debug breakpoint section now reserves a non-clickable
  overflow row when more than four breakpoints exist. It shows three real source
  rows plus a measured "N more breakpoints" row, keeps the Call Stack geometry
  stable, and prevents the overflow row from opening a misleading source target.
  Tests cover row budgets, pluralized overflow labels, and no-op hit behavior.

L687. Overflow disclosure should lead to browsing. A capped Breakpoints section
that says more items exist still leaves users stuck on the first window unless
the list itself can move.

- **IDE note:** the Run and Debug breakpoint inventory now wheel-scrolls when
  the pointer is over its rows. The model keeps a clamped global breakpoint
  window, click/open and drawing translate visible rows through that window, and
  the fourth row reports either remaining breakpoints below or earlier
  breakpoints above. Tests cover model clamping, wheel hit routing, scroll
  labels, and Mighty event dispatch.

L688. Breakpoint inventory rows should manage breakpoints, not only navigate.
Once a breakpoint is visible in the Debug panel, clearing that exact breakpoint
should not require jumping to the source gutter first.

- **IDE note:** clicking the red breakpoint dot in the Run and Debug inventory
  now removes that visible breakpoint, while clicking the row text still opens
  the source location. The remove hit range has its own encoded ABI, translates
  through the scrolled breakpoint window, resends live-session breakpoints, and
  reports the removed file/line. Tests cover exact-location removal, scrolled
  row mapping, no-row feedback, and Mighty dispatch ordering.

L689. Bulk breakpoint cleanup should be visible in the inventory itself. A
palette command is useful, but when users are already reviewing the Breakpoints
section, clearing all entries should be a local panel action.

- **IDE note:** the Breakpoints header now shows a compact trash action when
  breakpoints exist. The header action uses a dedicated click ABI, clears the
  model through the same live-session resend path as the palette command, and
  the title text is measured to fit before the button. Tests cover button
  hit/miss behavior and Mighty dispatch ordering before row hit-testing.

L690. Panel-local maintenance commands should be surfaced where the user is
already working. The Outline palette commands are useful, but symbol refresh and
clear are faster and more discoverable as local header actions.

- **IDE note:** the Outline header now exposes compact refresh and clear-symbols
  buttons. A dedicated click ABI mirrors the painted button geometry, Mighty
  dispatches those actions before row navigation, and the symbol count is
  measured to fit ahead of the controls. Tests cover header hit/miss behavior
  and event ordering before symbol row hit-testing.

L691. Bottom-dock panels need the same local maintenance affordances as sidebar
panels. Problems refresh and clear are command-palette actions, but users often
discover stale or noisy diagnostics while already looking at the Problems dock.

- **IDE note:** the Problems dock header now exposes compact refresh and clear
  buttons before the shared dock size/close controls. A dedicated header action
  ABI mirrors the painted geometry, Mighty refreshes or clears before diagnostic
  row hit-testing, and tests cover hit/miss behavior plus dispatcher ordering.

L692. Search results should be locally disposable without losing the search
draft. `Search: Clear Results` already preserves query and replacement text, so
the Search panel header should expose that cleanup next to the local run action.

- **IDE note:** the Search header now includes a compact clear-results button
  beside the existing run-search action. The existing search click ABI returns a
  dedicated clear action code, Mighty routes it before result-row hit-testing,
  and regression coverage verifies focus behavior, action codes, and dispatcher
  ordering while preserving the command-palette clear path.

L693. Source Control drafts should be locally clearable from the message box.
The palette command preserves status and panel state, but when a commit message
is visibly wrong the fastest safe cleanup is a clear affordance in that field.

- **IDE note:** the Source Control commit-message box now includes a compact
  clear button that routes through the existing message-clear ABI. The message
  text is measured to stop before the button, and Mighty handles that click
  before change-row stage/open actions. Tests cover the button hit/miss behavior
  and dispatcher ordering while keeping the palette command path intact.

L694. Run output needs a local clear affordance in the panel header. The command
palette path is useful, but clearing a noisy transcript is most discoverable
when the action sits next to the run status in the visible bottom band.

- **IDE note:** the Run panel header now includes a compact clear-output button
  that routes through the existing clear ABI without stopping the process or
  resetting status. The Mighty loop checks that header action before output-row
  navigation, with tests covering the visible hit target and dispatcher order.

L695. Web Playground output should be clearable where the transcript is shown.
The palette command preserves the running server and URL, but the visible panel
needs the same cleanup affordance as Run so noisy build logs can be reset in
place.

- **IDE note:** the Web Playground header now has a compact clear-output button
  beside the existing run/stop/open controls. The Web click ABI returns a
  dedicated clear code, Mighty routes it through `mui_web_clear`, and tests cover
  hit/miss behavior plus dispatcher wiring while preserving the command path.

L696. Testing results need a local clear action in the toolbar. The command
palette can clear parsed rows, but the Testing panel's visible workflow is
toolbar-driven, so cleanup should sit beside Run and Stop before row clicks.

- **IDE note:** the Testing toolbar now includes an icon-only clear-results
  button. The toolbar ABI returns a dedicated clear code, Mighty routes it
  through `mui_test_clear` before result-row navigation, and tests cover hit/miss
  behavior plus dispatcher wiring while preserving the palette command path.

L697. Mighty Agents run transcripts need the same visible cleanup path as the
other execution panels. The palette command preserves topology, but the Agents
header already hosts run/inspect controls, so clearing the embedded run output
belongs there too.

- **IDE note:** the Agents header now includes a compact clear-run-output
  affordance to the left of Inspect and Run. Mighty routes that header click
  through `mui_agents_clear_run_output` before topology row navigation, and tests
  cover the separated header hit zones plus dispatcher wiring.

## L698 - Terminal Header Actions Should Stay Local

The integrated Terminal already had palette commands for clearing and closing,
but high-frequency output cleanup should also live where the output is read.
Treat the terminal header like Run/Web: draw local action icons before the
shared dock controls and route those clicks before grid mouse reporting.

- **IDE note:** the Terminal header now exposes a compact Clear Buffer action.
  Mighty hit-tests that header action before PTY grid routing, then reuses
  `mui_term_clear` so focus and toast behavior stay aligned with the command
  palette path.

## L699 - Debug Session Cleanup Belongs In The Debug Toolbar

Run and Debug had a command-palette action for clearing stale session state, but
the visible toolbar stopped at run-control actions. When stack, variables, or
console state is visibly stale, cleanup should be available in the same panel
without requiring a palette search.

- **IDE note:** the Debug toolbar now includes a compact clear-session button
  after Stop. The shared toolbar geometry drives drawing, hit-testing, and
  compact-fit tests, and the toolbar action reuses `mui_dbg_clear_session` so
  breakpoints and last target are preserved exactly like the command path.

## L700 - Run Processes Need Local Stop Controls

The Run panel had command-palette stop support and local clear-output support,
but a running process still required a palette search or shortcut memory to
stop from the panel where its output was visible. Stop is a lifecycle action,
so it belongs beside the output status and clear control.

- **IDE note:** the Run header now includes a compact Stop Process button before
  Clear Output. The header hit-test returns a distinct stop action, Mighty routes
  it through `mui_run_stop` before output-row navigation, and the existing stop
  ABI supplies running and idle feedback.

## L701 - AI Chat Cleanup Should Be A Header Action

The AI Copilot already had a command-palette clear-chat action, but stale
transcripts are most visible in the right-docked chat panel itself. Clearing the
conversation should be a local header action beside Close, not a command search.

- **IDE note:** the AI Copilot header now includes a compact Clear Chat button
  to the left of Close. The shared geometry drives drawing and hit-testing,
  `mui_ai_click` returns a distinct clear action, and Mighty routes it through
  `mui_ai_clear` before Send/body focus handling.

## L702 - Shortcut Reset Actions Need Visible Controls

Keyboard shortcut reset was available through Ctrl+R / Ctrl+Shift+R and command
palette entries, but users reviewing keybindings need local reset affordances
inside the overlay where overrides are visible.

- **IDE note:** the Keyboard Shortcuts overlay header now includes compact reset
  selected and reset all buttons before Close. The overlay shares geometry for
  drawing and hit-testing, `mui_keys_click` returns distinct reset actions, and
  Mighty routes those clicks through the existing reset ABIs before remap
  capture handling.

## L703 - Source Control Bulk Staging Belongs In The Panel Header

Stage all and unstage all were command-palette actions, but the Source Control
panel already shows the full staged/unstaged state and row-level stage buttons.
Bulk staging should be visible in the same header as commit and refresh.

- **IDE note:** the Source Control header now includes compact Stage All and
  Unstage All actions before Commit/Pull/Push/Refresh. Shared header action
  geometry drives drawing and hit-testing, Mighty dispatches the new action
  codes through `mui_scm_stage_all` / `mui_scm_unstage_all` before change-row
  handling, and palette bulk-stage commands now reveal Source Control before
  acting.

## L704 - Stateful Source Control Commands Should Reveal Their State

Palette commands that consume Source Control state should make that state
visible before acting. Commit and clear-message both depend on the SCM draft and
staged set, so leaving the user in another panel makes success or failure harder
to understand.

- **IDE note:** `Git: Commit Staged` and `Source Control: Clear Commit Message`
  now reveal Source Control before dispatching their SCM ABI calls. Commit still
  refreshes status afterward, and both commands clear transient panel focus so
  the visible SCM panel owns the follow-up interaction.

## L705 - Debug Breakpoint Commands Should Reveal The Inventory

Breakpoint cleanup changes a persistent debugger inventory, not just a hidden
mode flag. When a palette command clears every breakpoint, the user should land
on the panel that shows the cleared list and any follow-up debug state.

- **IDE note:** `Debug: Clear Breakpoints` now reveals Run and Debug before
  calling `mui_bp_clear_all`. The command also clears transient panel focus so
  the Debug surface owns the next interaction and the toast is grounded in the
  visible breakpoint inventory.

## L706 - Problems Mutations Should Reveal The Problems Panel

Diagnostics are easiest to trust when refresh, clear, and row navigation all
resolve against the same visible panel. A palette clear command should not empty
the diagnostic model while leaving the user elsewhere in the IDE.

- **IDE note:** `Problems: Clear Diagnostics` now opens the Problems panel
  before calling `mui_problems_clear`. Mighty also clears transient find/Agents
  focus so the bottom-dock Problems surface owns the next interaction after the
  diagnostics are emptied.

## L707 - AI Transcript Commands Should Reveal Copilot

Clearing an AI conversation changes the transcript and composer state that live
inside the Copilot drawer. A command-palette clear should make that drawer
visible before the transcript disappears, matching the local header action.

- **IDE note:** `AI: Clear Chat` now calls `mui_ai_show` before
  `mui_ai_clear`. The command keeps AI focus and leaves typing disabled so the
  Copilot surface owns the next input after the transcript reset.

## L708 - Terminal Buffer Commands Should Reveal Terminal

Terminal buffer cleanup is only understandable when the terminal surface is
visible. Palette commands should not silently clear PTY output while the user is
looking at another dock owner or a closed terminal.

- **IDE note:** `Terminal: Clear Buffer` now calls `mui_term_open` before
  `mui_term_clear`. Mighty then derives terminal focus from the open state, so a
  successful clear lands on the integrated Terminal and open failures still
  report through the terminal ABI.

## L709 - Output Lifecycle Commands Should Reveal Output Surfaces

Stop and clear actions are lifecycle mutations for concrete output surfaces.
When they are triggered from the palette, the user should land on the Run,
Testing, or Web panel that will show the stopped, idle, or cleared state.

- **IDE note:** Run stop/clear now call `mui_run_open` before their action,
  Testing stop/clear reveals the Testing panel with `mui_panel_set`, and Web
  stop/clear calls `mui_web_open` first. The existing focus flags then preserve
  the owning surface for follow-up keyboard and mouse interaction.

## L710 - Bottom Dock View Commands Should Own Focus

Opening a bottom-dock surface from the palette should also make that surface the
only focused dock owner. Otherwise stale Run, Web, Testing, Terminal, or Agents
flags can route the next keyboard or mouse event to a surface the user is no
longer looking at.

- **IDE note:** `View: Terminal`, `View: Web Playground`, and `View: Problems`
  now clear unrelated dock focus and transient navigation flags after opening
  their surfaces. This keeps command-palette navigation aligned with the same
  visible owner model used by direct dock clicks.
