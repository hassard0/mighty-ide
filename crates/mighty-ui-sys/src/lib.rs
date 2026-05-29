/// Smoke export: proves the staticlib builds and exports a C symbol.
#[no_mangle]
pub extern "C" fn mui_smoke_add(a: i32, b: i32) -> i32 {
    a + b
}
