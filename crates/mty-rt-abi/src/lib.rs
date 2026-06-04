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
use libloading::{Library, Symbol};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

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

unsafe fn read_str(ptr: i64, len: i64) -> String {
    String::from_utf8_lossy(read_bytes(ptr, len)).into_owned()
}

thread_local! {
    static FMT_STRINGS: RefCell<Vec<Box<str>>> = const { RefCell::new(Vec::new()) };
    static RAW_BYTES: RefCell<Vec<Box<[u8]>>> = const { RefCell::new(Vec::new()) };
}

struct ExternRegistry {
    libs: Vec<Arc<Library>>,
    cache: HashMap<String, *const ()>,
}

unsafe impl Send for ExternRegistry {}
unsafe impl Sync for ExternRegistry {}

impl ExternRegistry {
    fn with_libc() -> Self {
        let mut this = Self {
            libs: Vec::new(),
            cache: HashMap::new(),
        };
        if let Some(lib) = open_libc() {
            this.libs.push(Arc::new(lib));
        }
        this
    }

    fn resolve(&mut self, name: &str) -> Option<*const ()> {
        if let Some(&ptr) = self.cache.get(name) {
            return Some(ptr);
        }
        for lib in &self.libs {
            if let Some(ptr) = sym_in(lib, name) {
                self.cache.insert(name.to_string(), ptr);
                return Some(ptr);
            }
        }
        None
    }

    fn call_i64(&mut self, name: &str) -> Option<i64> {
        let ptr = self.resolve(name)?;
        let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
        Some(f())
    }
}

static EXTERN_REGISTRY: OnceLock<Mutex<ExternRegistry>> = OnceLock::new();

fn extern_registry() -> &'static Mutex<ExternRegistry> {
    EXTERN_REGISTRY.get_or_init(|| Mutex::new(ExternRegistry::with_libc()))
}

#[allow(clippy::transmute_ptr_to_ptr)]
fn sym_in(lib: &Library, name: &str) -> Option<*const ()> {
    let cstr = std::ffi::CString::new(name).ok()?;
    let sym: Result<Symbol<unsafe extern "C" fn()>, _> =
        unsafe { lib.get(cstr.as_bytes_with_nul()) };
    sym.ok()
        .map(|s| unsafe { std::mem::transmute(s.into_raw().into_raw()) })
}

#[cfg(target_os = "linux")]
fn open_libc() -> Option<Library> {
    unsafe {
        Library::new("libc.so.6")
            .or_else(|_| Library::new("libc.so"))
            .ok()
    }
}

#[cfg(target_os = "macos")]
fn open_libc() -> Option<Library> {
    unsafe { Library::new("libSystem.dylib").ok() }
}

#[cfg(target_os = "windows")]
fn open_libc() -> Option<Library> {
    unsafe {
        Library::new("msvcrt.dll")
            .or_else(|_| Library::new("ucrtbase.dll"))
            .ok()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_libc() -> Option<Library> {
    None
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

fn intern_raw_bytes(bytes: Vec<u8>) -> (i64, i64) {
    RAW_BYTES.with(|t| {
        let boxed = bytes.into_boxed_slice();
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

unsafe fn write_str_triple(dst: i64, ptr: i64, len: i64, ok: i64) {
    if dst == 0 {
        return;
    }
    let p = dst as usize as *mut i64;
    p.write(ptr);
    p.add(1).write(len);
    p.add(2).write(ok);
}

fn errno_of(err: std::io::Error) -> i32 {
    -err.raw_os_error().unwrap_or(1)
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
pub extern "C" fn mty_runtime_extern_call(name_ptr: i64, name_len: i64, _args: i64) -> i64 {
    let name = unsafe { read_str(name_ptr, name_len) };
    extern_registry()
        .lock()
        .ok()
        .and_then(|mut registry| registry.call_i64(&name))
        .unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_read(path_ptr: i64, path_len: i64, dst: i64) {
    let path = unsafe { read_str(path_ptr, path_len) };
    match std::fs::read(std::path::Path::new(&path)) {
        Ok(bytes) => {
            let (p, l) = intern_raw_bytes(bytes);
            unsafe { write_str_triple(dst, p, l, 1) };
        }
        Err(_) => unsafe { write_str_triple(dst, 0, 0, 0) },
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_read_to_string(path_ptr: i64, path_len: i64, dst: i64) {
    mty_runtime_fs_read(path_ptr, path_len, dst);
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_read_dir(path_ptr: i64, path_len: i64, dst: i64) {
    let path = unsafe { read_str(path_ptr, path_len) };
    match std::fs::read_dir(std::path::Path::new(&path)) {
        Ok(rd) => {
            let mut entries: Vec<std::path::PathBuf> =
                rd.filter_map(|e| e.ok().map(|d| d.path())).collect();
            entries.sort();
            let joined = entries
                .iter()
                .map(|entry| entry.display().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let (p, l) = intern_fmt(joined);
            unsafe { write_str_triple(dst, p, l, 1) };
        }
        Err(_) => unsafe { write_str_triple(dst, 0, 0, 0) },
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_write(
    path_ptr: i64,
    path_len: i64,
    buf_ptr: i64,
    buf_len: i64,
) -> i32 {
    let path_str = unsafe { read_str(path_ptr, path_len) };
    let path = std::path::Path::new(&path_str);
    let data = unsafe { read_bytes(buf_ptr, buf_len) };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return errno_of(e);
            }
        }
    }
    match std::fs::write(path, data) {
        Ok(()) => 1,
        Err(e) => errno_of(e),
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_write_string(
    path_ptr: i64,
    path_len: i64,
    buf_ptr: i64,
    buf_len: i64,
) -> i32 {
    mty_runtime_fs_write(path_ptr, path_len, buf_ptr, buf_len)
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_append(
    path_ptr: i64,
    path_len: i64,
    buf_ptr: i64,
    buf_len: i64,
) -> i32 {
    let path_str = unsafe { read_str(path_ptr, path_len) };
    let path = std::path::Path::new(&path_str);
    let data = unsafe { read_bytes(buf_ptr, buf_len) };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return errno_of(e);
            }
        }
    }
    let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(e) => return errno_of(e),
    };
    match f.write_all(data) {
        Ok(()) => 1,
        Err(e) => errno_of(e),
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_exists(path_ptr: i64, path_len: i64) -> i32 {
    let path = unsafe { read_str(path_ptr, path_len) };
    i32::from(std::path::Path::new(&path).exists())
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_metadata(path_ptr: i64, path_len: i64, dst: i64) -> i32 {
    let path = unsafe { read_str(path_ptr, path_len) };
    match std::fs::metadata(std::path::Path::new(&path)) {
        Ok(md) => {
            let mtime_ms = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if dst != 0 {
                unsafe {
                    (dst as usize as *mut u64).write(md.len());
                    ((dst as usize + 8) as *mut i64).write(mtime_ms);
                    ((dst as usize + 16) as *mut i8).write(i8::from(md.is_file()));
                    ((dst as usize + 17) as *mut i8).write(i8::from(md.is_dir()));
                }
            }
            1
        }
        Err(e) => errno_of(e),
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_create_dir_all(path_ptr: i64, path_len: i64) -> i32 {
    let path = unsafe { read_str(path_ptr, path_len) };
    match std::fs::create_dir_all(std::path::Path::new(&path)) {
        Ok(()) => 1,
        Err(e) => errno_of(e),
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_remove_file(path_ptr: i64, path_len: i64) -> i32 {
    let path = unsafe { read_str(path_ptr, path_len) };
    match std::fs::remove_file(std::path::Path::new(&path)) {
        Ok(()) => 1,
        Err(e) => errno_of(e),
    }
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_remove_dir_all(path_ptr: i64, path_len: i64) -> i32 {
    let path = unsafe { read_str(path_ptr, path_len) };
    match std::fs::remove_dir_all(std::path::Path::new(&path)) {
        Ok(()) => 1,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 1,
        Err(e) => errno_of(e),
    }
}

#[repr(C)]
struct DirIterState {
    entries: Vec<std::path::PathBuf>,
    cursor: usize,
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_dir_open(path_ptr: i64, path_len: i64) -> i64 {
    let path = unsafe { read_str(path_ptr, path_len) };
    let entries = match std::fs::read_dir(std::path::Path::new(&path)) {
        Ok(rd) => {
            let mut entries: Vec<std::path::PathBuf> =
                rd.filter_map(|e| e.ok().map(|d| d.path())).collect();
            entries.sort();
            entries
        }
        Err(_) => return 0,
    };
    Box::into_raw(Box::new(DirIterState { entries, cursor: 0 })) as usize as i64
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_dir_next(handle: i64, dst: i64) -> i32 {
    if handle == 0 {
        unsafe { write_str_triple(dst, 0, 0, 0) };
        return 0;
    }
    let state = unsafe { &mut *(handle as usize as *mut DirIterState) };
    if state.cursor >= state.entries.len() {
        unsafe { write_str_triple(dst, 0, 0, 0) };
        return 0;
    }
    let entry = state.entries[state.cursor].display().to_string();
    state.cursor += 1;
    let (p, l) = intern_fmt(entry);
    unsafe { write_str_triple(dst, p, l, 1) };
    1
}

#[no_mangle]
pub extern "C" fn mty_runtime_fs_dir_close(handle: i64) {
    if handle != 0 {
        let _ = unsafe { Box::from_raw(handle as usize as *mut DirIterState) };
    }
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

#[no_mangle]
pub extern "C" fn mty_runtime_crypto_sha256(data_ptr: i64, data_len: i64, dst: i64) {
    use sha2::Digest;
    let data = unsafe { read_bytes(data_ptr, data_len) };
    let digest = sha2::Sha256::digest(data);
    let (p, l) = intern_raw_bytes(digest.to_vec());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_crypto_sha512(data_ptr: i64, data_len: i64, dst: i64) {
    use sha2::Digest;
    let data = unsafe { read_bytes(data_ptr, data_len) };
    let digest = sha2::Sha512::digest(data);
    let (p, l) = intern_raw_bytes(digest.to_vec());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_crypto_blake3(data_ptr: i64, data_len: i64, dst: i64) {
    let data = unsafe { read_bytes(data_ptr, data_len) };
    let hash = blake3::hash(data);
    let (p, l) = intern_raw_bytes(hash.as_bytes().to_vec());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_crypto_hmac_sha256(
    key_ptr: i64,
    key_len: i64,
    msg_ptr: i64,
    msg_len: i64,
    dst: i64,
) {
    use hmac::Mac;
    let key = unsafe { read_bytes(key_ptr, key_len) };
    let msg = unsafe { read_bytes(msg_ptr, msg_len) };
    let mut mac =
        <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(key).expect("hmac key any size");
    mac.update(msg);
    let tag = mac.finalize().into_bytes();
    let (p, l) = intern_raw_bytes(tag.to_vec());
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_encoding_hex_encode(data_ptr: i64, data_len: i64, dst: i64) {
    let data = unsafe { read_bytes(data_ptr, data_len) };
    let (p, l) = intern_fmt(hex::encode(data));
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_encoding_base64_encode(data_ptr: i64, data_len: i64, dst: i64) {
    use base64::Engine;
    let data = unsafe { read_bytes(data_ptr, data_len) };
    let s = base64::engine::general_purpose::STANDARD.encode(data);
    let (p, l) = intern_fmt(s);
    unsafe { write_str_pair(dst, p, l) };
}

#[no_mangle]
pub extern "C" fn mty_runtime_encoding_base64_encode_url_no_pad(
    data_ptr: i64,
    data_len: i64,
    dst: i64,
) {
    use base64::Engine;
    let data = unsafe { read_bytes(data_ptr, data_len) };
    let s = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data);
    let (p, l) = intern_fmt(s);
    unsafe { write_str_pair(dst, p, l) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot_bytes(slot: &[i64; 2]) -> Vec<u8> {
        unsafe { read_bytes(slot[0], slot[1]) }.to_vec()
    }

    fn slot_triple_bytes(slot: &[i64; 3]) -> Vec<u8> {
        unsafe { read_bytes(slot[0], slot[1]) }.to_vec()
    }

    #[test]
    fn extern_call_resolves_platform_libc_symbol() {
        #[cfg(target_os = "windows")]
        let name = b"_getpid";
        #[cfg(not(target_os = "windows"))]
        let name = b"getpid";

        let pid = mty_runtime_extern_call(name.as_ptr() as i64, name.len() as i64, 0);
        assert_eq!(pid, std::process::id() as i64);
    }

    #[test]
    fn extern_call_returns_zero_for_missing_symbol() {
        let name = b"mighty_missing_extern_symbol_";
        assert_eq!(
            mty_runtime_extern_call(name.as_ptr() as i64, name.len() as i64, 0),
            0
        );
    }

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

    #[test]
    fn crypto_sha256_and_hex_encode_known_vector() {
        let data = b"hello";
        let mut digest_slot = [0_i64; 2];
        mty_runtime_crypto_sha256(
            data.as_ptr() as i64,
            data.len() as i64,
            digest_slot.as_mut_ptr() as i64,
        );
        assert_eq!(digest_slot[1], 32);

        let mut hex_slot = [0_i64; 2];
        mty_runtime_encoding_hex_encode(
            digest_slot[0],
            digest_slot[1],
            hex_slot.as_mut_ptr() as i64,
        );
        let hex = String::from_utf8(slot_bytes(&hex_slot)).unwrap();
        assert_eq!(
            hex,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn crypto_hmac_sha256_known_vector() {
        let key = b"Jefe";
        let msg = b"what do ya want for nothing?";
        let mut tag_slot = [0_i64; 2];
        mty_runtime_crypto_hmac_sha256(
            key.as_ptr() as i64,
            key.len() as i64,
            msg.as_ptr() as i64,
            msg.len() as i64,
            tag_slot.as_mut_ptr() as i64,
        );

        let mut hex_slot = [0_i64; 2];
        mty_runtime_encoding_hex_encode(tag_slot[0], tag_slot[1], hex_slot.as_mut_ptr() as i64);
        let hex = String::from_utf8(slot_bytes(&hex_slot)).unwrap();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn encoding_base64_variants_known_vectors() {
        let plain = b"hello";
        let mut standard_slot = [0_i64; 2];
        mty_runtime_encoding_base64_encode(
            plain.as_ptr() as i64,
            plain.len() as i64,
            standard_slot.as_mut_ptr() as i64,
        );
        assert_eq!(String::from_utf8(slot_bytes(&standard_slot)).unwrap(), "aGVsbG8=");

        let data = [0xfb_u8, 0xff, 0xbf];
        let mut url_slot = [0_i64; 2];
        mty_runtime_encoding_base64_encode_url_no_pad(
            data.as_ptr() as i64,
            data.len() as i64,
            url_slot.as_mut_ptr() as i64,
        );
        assert_eq!(String::from_utf8(slot_bytes(&url_slot)).unwrap(), "-_-_");
    }

    #[test]
    fn filesystem_runtime_exports_perform_real_io() {
        let root = std::env::temp_dir().join(format!(
            "mty_rt_abi_fs_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let root_s = root.display().to_string();
        assert_eq!(
            mty_runtime_fs_create_dir_all(root_s.as_ptr() as i64, root_s.len() as i64),
            1
        );

        let file = root.join("nested").join("data.txt");
        let file_s = file.display().to_string();
        let data = b"hello";
        assert_eq!(
            mty_runtime_fs_write(
                file_s.as_ptr() as i64,
                file_s.len() as i64,
                data.as_ptr() as i64,
                data.len() as i64,
            ),
            1
        );
        let suffix = b" world";
        assert_eq!(
            mty_runtime_fs_append(
                file_s.as_ptr() as i64,
                file_s.len() as i64,
                suffix.as_ptr() as i64,
                suffix.len() as i64,
            ),
            1
        );
        assert_eq!(mty_runtime_fs_exists(file_s.as_ptr() as i64, file_s.len() as i64), 1);

        let mut read_slot = [0_i64; 3];
        mty_runtime_fs_read(
            file_s.as_ptr() as i64,
            file_s.len() as i64,
            read_slot.as_mut_ptr() as i64,
        );
        assert_eq!(read_slot[2], 1);
        assert_eq!(slot_triple_bytes(&read_slot), b"hello world");

        let mut read_str_slot = [0_i64; 3];
        mty_runtime_fs_read_to_string(
            file_s.as_ptr() as i64,
            file_s.len() as i64,
            read_str_slot.as_mut_ptr() as i64,
        );
        assert_eq!(read_str_slot[2], 1);
        assert_eq!(slot_triple_bytes(&read_str_slot), b"hello world");

        let mut md_slot = [0_u8; 24];
        assert_eq!(
            mty_runtime_fs_metadata(file_s.as_ptr() as i64, file_s.len() as i64, md_slot.as_mut_ptr() as i64),
            1
        );
        let size = unsafe { *(md_slot.as_ptr() as *const u64) };
        assert_eq!(size, 11);
        assert_eq!(md_slot[16], 1);
        assert_eq!(md_slot[17], 0);

        let mut read_dir_slot = [0_i64; 3];
        mty_runtime_fs_read_dir(
            root_s.as_ptr() as i64,
            root_s.len() as i64,
            read_dir_slot.as_mut_ptr() as i64,
        );
        assert_eq!(read_dir_slot[2], 1);
        assert!(String::from_utf8(slot_triple_bytes(&read_dir_slot))
            .unwrap()
            .contains("nested"));

        let handle = mty_runtime_fs_dir_open(root_s.as_ptr() as i64, root_s.len() as i64);
        assert_ne!(handle, 0);
        let mut next_slot = [0_i64; 3];
        assert_eq!(mty_runtime_fs_dir_next(handle, next_slot.as_mut_ptr() as i64), 1);
        let first = String::from_utf8(slot_triple_bytes(&next_slot)).unwrap();
        assert!(first.ends_with("nested"));
        assert_eq!(next_slot[2], 1);
        assert_eq!(mty_runtime_fs_dir_next(handle, next_slot.as_mut_ptr() as i64), 0);
        assert_eq!(next_slot[2], 0);
        mty_runtime_fs_dir_close(handle);

        assert_eq!(mty_runtime_fs_remove_file(file_s.as_ptr() as i64, file_s.len() as i64), 1);
        assert_eq!(mty_runtime_fs_exists(file_s.as_ptr() as i64, file_s.len() as i64), 0);
        assert_eq!(
            mty_runtime_fs_remove_dir_all(root_s.as_ptr() as i64, root_s.len() as i64),
            1
        );
    }
}
