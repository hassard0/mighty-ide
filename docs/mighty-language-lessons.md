# Mighty Language — Lessons from Building the IDE

A living list of concrete ways to improve **Mighty** (the language, `hassard0/Mighty` /
`C:\Users\ihass\stardust`), discovered while building **Mighty IDE** in Mighty itself.
The IDE is the forcing function: every place the language fights us is logged here so it
can be promoted into a `stardust` issue / RFC.

**Legend:** ✅ verified against current source · 🔎 inferred from example comments / docs
(verify before acting) · severity **[P0]** blocks native dogfooding, **[P1]** major
ergonomics, **[P2]** papercut.

_Last updated: 2026-05-28 (during sub-project 0, before the Phase-0 spike ran)._

---

## P0 — Blocks building real native apps in Mighty

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

## Open questions to resolve as the IDE progresses
- Exact `extern c` signature support: pointers (`*U8`), out-params (`&out T`), passing a
  `Vec`/slice as `(ptr, len)`, returning `#[repr(C)]` structs by value vs. out-param?
  (The Phase-0 spike will answer much of this — record results here.)
- Does native `mty build` handle dynamic FFI calls in a loop? (Phase-0 Gate B.)
- `fs` module API names for read-to-string / write (needed by IDE save/load).
