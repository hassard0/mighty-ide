/* mty_runtime_stub.c — minimal runtime-symbol stub for built Mighty binaries.
 *
 * The cranelift backend pre-declares every `mty_runtime_*` symbol as an import
 * in every emitted object, even when the program calls none of them. No runtime
 * archive ships with v0.36, so a standalone FFI binary must provide these or the
 * linker rejects the object. This stub gives each a minimal body:
 *   - log/print: write `len` bytes from `ptr` to stdout (log adds a newline)
 *   - panic: write to stderr + abort
 *   - alloc: malloc
 *   - arena push/pop, send, ask, spawn, extern_call, budget_charge: no-ops
 *   - log_i64: print the integer
 *
 * Mirrors crates/mty-driver/tests/extern_c_matrix.rs::build_runtime_stub, with
 * real log/alloc bodies so the IDE's `log(...)` diagnostics are visible.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

void mty_runtime_log(int64_t ptr, int64_t len) {
  if (ptr && len > 0) fwrite((const void *)(intptr_t)ptr, 1, (size_t)len, stdout);
  fputc('\n', stdout);
  fflush(stdout);
}
void mty_runtime_print(int64_t ptr, int64_t len) {
  if (ptr && len > 0) fwrite((const void *)(intptr_t)ptr, 1, (size_t)len, stdout);
  fflush(stdout);
}
void mty_runtime_panic(int64_t ptr, int64_t len) {
  if (ptr && len > 0) fwrite((const void *)(intptr_t)ptr, 1, (size_t)len, stderr);
  fputc('\n', stderr);
  fflush(stderr);
  abort();
}
int64_t mty_runtime_arena_push(void) { return 0; }
void mty_runtime_arena_pop(int64_t k) { (void)k; }
int64_t mty_runtime_alloc(int64_t size, int64_t align, int64_t zero) {
  (void)align;
  void *p = malloc((size_t)size);
  if (p && zero) {
    for (int64_t i = 0; i < size; ++i) ((char *)p)[i] = 0;
  }
  return (int64_t)(intptr_t)p;
}
int8_t mty_runtime_budget_charge(int64_t n) { (void)n; return 1; }
void mty_runtime_send(int64_t a, int64_t b, int64_t c) { (void)a; (void)b; (void)c; }
int64_t mty_runtime_ask(int64_t a, int64_t b, int64_t c, int64_t d) {
  (void)a; (void)b; (void)c; (void)d;
  return 0;
}
int64_t mty_runtime_spawn(int64_t a) { (void)a; return 0; }
int64_t mty_runtime_extern_call(int64_t a, int64_t b, int64_t c) {
  (void)a; (void)b; (void)c;
  return 0;
}
void mty_runtime_log_i64(int64_t v) { printf("%lld\n", (long long)v); fflush(stdout); }
