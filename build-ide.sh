#!/usr/bin/env bash
# build-ide.sh — build the Mighty IDE end to end.
#
#   1. cargo-build the wgpu shim as a cdylib (mighty_ui_sys.dll + .dll.lib)
#   2. cc the runtime-symbol stub into vendor/mtyrt.lib
#   3. copy the import lib + DLL next to the output exe
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

echo "[1/4] cargo build -p mighty-ui-sys (cdylib)"
cargo build -p mighty-ui-sys

echo "[2/4] build runtime stub -> vendor/mtyrt.lib"
"$CLANG" -c -O0 vendor/mty_runtime_stub.c -o vendor/mty_runtime_stub.o
"$LLVM_AR" rcs vendor/mtyrt.lib vendor/mty_runtime_stub.o

echo "[3/4] stage shim import lib + DLL"
mkdir -p target vendor
cp target/debug/mighty_ui_sys.dll.lib vendor/mighty_ui_sys.dll.lib
cp target/debug/mighty_ui_sys.dll     target/mighty_ui_sys.dll

echo "[4/4] mty build src/main.mty -> target/main.exe"
MTY_LINKER="$CLANG" STARDUST_LINKER="$CLANG" "$MTY" build src/main.mty --out-dir target

echo "OK: $(ls -la target/main.exe)"
