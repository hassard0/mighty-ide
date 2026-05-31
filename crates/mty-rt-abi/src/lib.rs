//! Real-arena runtime-ABI for built Mighty (`mty build`) binaries.
//!
//! Every object `mty build` emits imports a fixed set of `mty_runtime_*`
//! C-ABI symbols (the cranelift backend pre-declares them whether or not the
//! program calls them). v0.36 ships no runtime archive, so an FFI binary must
//! supply these or the linker rejects the object.
//!
//! This crate mirrors Mighty's `crates/mty-runtime/src/{arena,codegen_abi}.rs`
//! with a REAL `bumpalo`-backed arena, replacing the IDE's previous no-op C
//! stub (`vendor/mty_runtime_stub.c`). The no-op stub's `arena_push`/`_pop`
//! did nothing and `alloc` was a bare `malloc`; under that stub Mighty's `Vec`
//! grow path (which routes through the arena runtime) silently came back empty.
//!
//! Arena semantics:
//!   - thread-local `ArenaStack` of `bumpalo::Bump` frames.
//!   - `mty_runtime_arena_push` pushes a frame, returns the new (1-based) depth.
//!   - `mty_runtime_arena_pop` drops the top frame (frees its allocations).
//!   - `mty_runtime_alloc(size, align, zero)` allocates on the top frame; if no
//!     frame is active it falls back to a leaked, process-wide global `Bump` so
//!     allocations ALWAYS succeed (the codegen may alloc outside any explicit
//!     `arena {}` scope).
//!
//! All symbols are `#[no_mangle] pub extern "C"`.

use bumpalo::Bump;
use std::cell::RefCell;
use std::io::Write;

// ---- arena ----------------------------------------------------------

#[derive(Default)]
struct ArenaStack {
    frames: Vec<Bump>,
}

impl ArenaStack {
    fn push(&mut self) -> usize {
        self.frames.push(Bump::new());
        self.frames.len()
    }

    fn pop(&mut self) -> usize {
        let _ = self.frames.pop();
        self.frames.len()
    }

    fn alloc(&mut self, size: usize, align: usize) -> Option<*mut u8> {
        let top = self.frames.last_mut()?;
        let align = align.max(1);
        let layout = std::alloc::Layout::from_size_align(size, align).ok()?;
        Some(top.alloc_layout(layout).as_ptr())
    }
}

thread_local! {
    static ARENA_STACK: RefCell<ArenaStack> = RefCell::new(ArenaStack::default());

    /// Per-thread fallback arena, leaked so its allocations live for the
    /// lifetime of the thread — used when codegen allocates with no explicit
    /// arena frame active (so `Vec`/`String` grows never return null).
    /// `bumpalo::Bump` is not `Sync`, so this lives thread-local rather than as
    /// a single process-wide static; allocations happen on the calling thread.
    static FALLBACK_ARENA: &'static Bump = Box::leak(Box::new(Bump::new()));
}

fn fallback_alloc(size: usize, align: usize) -> Option<*mut u8> {
    let layout = std::alloc::Layout::from_size_align(size, align).ok()?;
    FALLBACK_ARENA.with(|a| Some(a.alloc_layout(layout).as_ptr()))
}

// ---- the C-ABI fns --------------------------------------------------

/// SAFETY: `ptr` must point to `len` valid bytes that outlive the call.
unsafe fn read_bytes<'a>(ptr: i64, len: i64) -> &'a [u8] {
    if ptr == 0 || len <= 0 {
        return &[];
    }
    std::slice::from_raw_parts(ptr as usize as *const u8, len as usize)
}

thread_local! {
    static FMT_STRINGS: RefCell<Vec<Box<str>>> = const { RefCell::new(Vec::new()) };
}

fn intern_fmt(s: String) -> (i64, i64) {
    FMT_STRINGS.with(|t| {
        let boxed = s.into_boxed_str();
        let ptr = boxed.as_ptr() as i64;
        let len = boxed.len() as i64;
        t.borrow_mut().push(boxed);
        (ptr, len)
    })
}

unsafe fn write_str_pair(dst: i64, ptr: i64, len: i64) {
    if dst == 0 {
        return;
    }
    let p = dst as usize as *mut i64;
    p.write(ptr);
    p.add(1).write(len);
}

#[no_mangle]
pub extern "C" fn mty_runtime_log(ptr: i64, len: i64) {
    let bytes = unsafe { read_bytes(ptr, len) };
    let s = String::from_utf8_lossy(bytes);
    println!("{s}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_print(ptr: i64, len: i64) {
    let bytes = unsafe { read_bytes(ptr, len) };
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(bytes);
    let _ = lock.flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_panic(ptr: i64, len: i64) -> ! {
    let bytes = unsafe { read_bytes(ptr, len) };
    let s = String::from_utf8_lossy(bytes);
    eprintln!("mighty panic: {s}");
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn mty_runtime_arena_push() -> i64 {
    ARENA_STACK.with(|s| s.borrow_mut().push() as i64)
}

#[no_mangle]
pub extern "C" fn mty_runtime_arena_pop(handle: i64) {
    let _ = handle;
    ARENA_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

#[no_mangle]
pub extern "C" fn mty_runtime_alloc(size: i64, align: i64, zero: i64) -> i64 {
    let size = size.max(0) as usize;
    let align = align.max(1) as usize;

    // Try the top thread-local frame; fall back to the leaked global arena so
    // allocations outside an explicit arena scope still succeed.
    let ptr = ARENA_STACK
        .with(|s| s.borrow_mut().alloc(size, align))
        .or_else(|| fallback_alloc(size, align));

    match ptr {
        Some(p) => {
            if zero != 0 && size > 0 {
                unsafe { std::ptr::write_bytes(p, 0, size) };
            }
            p as i64
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_budget_charge(_bytes: i64) -> i8 {
    1
}

#[no_mangle]
pub extern "C" fn mty_runtime_send(_target: i64, _msg: i64, _payload: i64) {}

#[no_mangle]
pub extern "C" fn mty_runtime_ask(
    _target: i64,
    _msg: i64,
    _payload: i64,
    _deadline_ms: i64,
) -> i64 {
    0
}

#[no_mangle]
pub extern "C" fn mty_runtime_spawn(_agent_id: i64) -> i64 {
    0
}

#[no_mangle]
pub extern "C" fn mty_runtime_extern_call(_name_ptr: i64, _name_len: i64, _args: i64) -> i64 {
    0
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_i64(v: i64) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_i32(v: i32) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_u32(v: u32) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_u64(v: u64) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_usize(v: i64) {
    println!("{}", v as u64);
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_f32(v: f32) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_f64(v: f64) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn mty_runtime_log_bool(v: i8) {
    println!("{}", v != 0);
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_i32(v: i32) {
    print!("{v}");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_i64(v: i64) {
    print!("{v}");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_u32(v: u32) {
    print!("{v}");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_u64(v: u64) {
    print!("{v}");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_usize(v: i64) {
    print!("{}", v as u64);
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_f32(v: f32) {
    print!("{v}");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_f64(v: f64) {
    print!("{v}");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_bool(v: i8) {
    print!("{}", v != 0);
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_sep() {
    print!(" ");
    let _ = std::io::stdout().flush();
}

#[no_mangle]
pub extern "C" fn mty_runtime_print_newline() {
    println!();
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_i32(v: i32, dst: i64) {
    let (p, l) = intern_fmt(v.to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_i64_to_slot(v: i64, dst: i64) {
    let (p, l) = intern_fmt(v.to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_u32(v: u32, dst: i64) {
    let (p, l) = intern_fmt(v.to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_u64(v: u64, dst: i64) {
    let (p, l) = intern_fmt(v.to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_usize(v: i64, dst: i64) {
    let (p, l) = intern_fmt((v as u64).to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_f32(v: f32, dst: i64) {
    let (p, l) = intern_fmt(v.to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_f64(v: f64, dst: i64) {
    let (p, l) = intern_fmt(v.to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_fmt_bool(v: i8, dst: i64) {
    let (p, l) = intern_fmt((v != 0).to_string());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_str_concat(aptr: i64, alen: i64, bptr: i64, blen: i64, dst: i64) {
    let a = unsafe { read_bytes(aptr, alen) };
    let b = unsafe { read_bytes(bptr, blen) };
    let mut s = String::with_capacity(a.len() + b.len());
    s.push_str(&String::from_utf8_lossy(a));
    s.push_str(&String::from_utf8_lossy(b));
    let (p, l) = intern_fmt(s);
    unsafe { write_str_pair(dst, p, l) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_without_frame_uses_global() {
        // No frame pushed: must still hand back a usable pointer.
        let p = mty_runtime_alloc(64, 8, 1);
        assert_ne!(p, 0);
    }

    #[test]
    fn push_alloc_pop_balances() {
        let d = mty_runtime_arena_push();
        assert_eq!(d, 1);
        let p = mty_runtime_alloc(32, 8, 0);
        assert_ne!(p, 0);
        mty_runtime_arena_pop(d);
    }

    #[test]
    fn budget_charge_ok() {
        assert_eq!(mty_runtime_budget_charge(123), 1);
    }

    #[test]
    fn fmt_i32_writes_string_slot() {
        let mut slot = [0_i64; 2];
        mty_runtime_fmt_i32(42, slot.as_mut_ptr() as i64);
        let bytes = unsafe { read_bytes(slot[0], slot[1]) };
        assert_eq!(bytes, b"42");
    }

    #[test]
    fn concat_writes_string_slot() {
        let mut slot = [0_i64; 2];
        mty_runtime_str_concat(
            "Mighty".as_ptr() as i64,
            6,
            " IDE".as_ptr() as i64,
            4,
            slot.as_mut_ptr() as i64,
        );
        let bytes = unsafe { read_bytes(slot[0], slot[1]) };
        assert_eq!(bytes, b"Mighty IDE");
    }
}
