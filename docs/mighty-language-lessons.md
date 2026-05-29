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

### L2. External static-library linking for `extern c` is undocumented / unclear ✅ **[P0]**
`extern c { fn ... }` exists (`examples/14_extern_c.mty`) and `mty build` "emits a host-
format `.o`, then links via the platform C linker." But there is **no documented way to
tell `mty build` to link an additional static library** (e.g. our `mighty_ui_sys.lib`).
The only escape hatch hinted at is the manual-link path (`.o` left in `target/`, see
diagnostic `MT8008`).

**Why it matters:** Native apps that bind C/Rust libraries (GUI, audio, DB drivers) need
this. It's the literal foundation of the IDE.
**Suggested work:** A first-class manifest mechanism, e.g.
```toml
[build]
native-libs = ["mighty_ui_sys"]
link-search = ["target/debug"]
```
plus docs + an example that links a real C archive. (Already on the v0.36 priority list per
project notes — this entry adds the concrete manifest shape an app author wants.)

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
