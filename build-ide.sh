#!/usr/bin/env bash
# build-ide.sh — build the Mighty IDE end to end.
#
#   1. cargo-build the wgpu shim as a cdylib (mighty_ui_sys.dll + .dll.lib)
#      and the REAL bumpalo-arena runtime (crates/mty-rt-abi, staticlib)
#   2. stage the runtime archive into vendor/mty_rt_abi.lib
#   3. copy the shim import lib + DLL next to the output exe
#   4. mty build src/main.mty -> target/main.exe
#
# Verified-working toolchain (do not change without re-verifying):
#   clang     C:\Program Files\LLVM\bin\clang.exe
#   llvm-ar   C:\Program Files\LLVM\bin\llvm-ar.exe
#   mty       C:\Users\ihass\stardust\target\debug\mty.exe (v0.36)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLANG="C:\\Program Files\\LLVM\\bin\\clang.exe"
LLVM_AR="C:\\Program Files\\LLVM\\bin\\llvm-ar.exe"
MTY="/c/Users/ihass/stardust/target/debug/mty.exe"

cd "$ROOT"

echo "[1/4] cargo build -p mighty-ui-sys (cdylib) + mty-rt-abi (real arena runtime)"
cargo build -p mighty-ui-sys -p mty-rt-abi

echo "[2/4] stage real-arena runtime -> vendor/mty_rt_abi.lib"
# The bumpalo-backed runtime archive replaces the old no-op C stub
# (vendor/mty_runtime_stub.c). Its required Windows system libs are declared
# in mighty.toml's [[extern_lib]] link_args_windows; refresh that list via:
#   cargo rustc -p mty-rt-abi --crate-type staticlib -- --print native-static-libs
mkdir -p target vendor
cp target/debug/mty_rt_abi.lib vendor/mty_rt_abi.lib

echo "[3/4] stage shim import lib + DLL"
cp target/debug/mighty_ui_sys.dll.lib vendor/mighty_ui_sys.dll.lib
cp target/debug/mighty_ui_sys.dll     target/mighty_ui_sys.dll

echo "[4/4] mty build src/main.mty -> target/main.exe"
MTY_LINKER="$CLANG" "$MTY" build src/main.mty --out-dir target

echo "OK: $(ls -la target/main.exe)"
