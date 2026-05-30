#!/usr/bin/env bash
# package-win.sh — build a RELEASE Mighty IDE and assemble a distributable
# double-clickable Windows package.
#
#   1. cargo build --release the wgpu shim cdylib (mighty_ui_sys.dll) and the
#      real bumpalo-arena runtime (mty-rt-abi, staticlib).
#   2. stage the RELEASE import lib + runtime archive into vendor/ (mighty.toml
#      links these by path), so the IDE exe links against release artifacts.
#   3. mty build --release src/main.mty -> target/release/main.exe.
#   4. assemble dist/mighty-ide-win64/ with the renamed exe, the release DLL,
#      sample files and RUN.txt (fonts are EMBEDDED in the DLL via include_bytes!,
#      so no fonts/ dir is shipped).
#   5. zip -> dist/mighty-ide-v0.3.0-win64.zip.
#
# Toolchain (same as build-ide.sh; do not change without re-verifying):
#   clang  C:\Program Files\LLVM\bin\clang.exe
#   mty    C:\Users\ihass\stardust\target\debug\mty.exe
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLANG="C:\\Program Files\\LLVM\\bin\\clang.exe"
MTY="/c/Users/ihass/stardust/target/debug/mty.exe"
VERSION="v0.3.0"
PKG="mighty-ide-win64"

cd "$ROOT"
export CARGO_INCREMENTAL=0

echo "[1/5] cargo build --release -p mighty-ui-sys (cdylib) + mty-rt-abi"
cargo build --release -p mighty-ui-sys -p mty-rt-abi

echo "[2/5] stage RELEASE shim import lib + runtime archive -> vendor/"
mkdir -p vendor target/release
cp target/release/mty_rt_abi.lib       vendor/mty_rt_abi.lib
cp target/release/mighty_ui_sys.dll.lib vendor/mighty_ui_sys.dll.lib
# cargo already emits the cdylib at target/release/mighty_ui_sys.dll; it is
# copied next to the exe in the assembly step below.

echo "[3/5] mty build --release src/main.mty -> target/release/main.exe"
MTY_LINKER="$CLANG" "$MTY" build --release src/main.mty --out-dir target/release

echo "[4/5] assemble dist/$PKG/"
DIST="dist/$PKG"
rm -rf "$DIST"
mkdir -p "$DIST/examples"
cp target/release/main.exe           "$DIST/mighty-ide.exe"
cp target/release/mighty_ui_sys.dll  "$DIST/mighty_ui_sys.dll"
# Sample files so a fresh download has something to open.
cp examples/demo.mty   "$DIST/examples/demo.mty"
cp examples/sample.py  "$DIST/examples/sample.py"  2>/dev/null || true
cp examples/sample.rs  "$DIST/examples/sample.rs"  2>/dev/null || true
cp examples/sample.json "$DIST/examples/sample.json" 2>/dev/null || true
cp examples/agents.mty "$DIST/examples/agents.mty" 2>/dev/null || true
cp RUN.txt             "$DIST/RUN.txt"

echo "[5/5] zip -> dist/mighty-ide-$VERSION-win64.zip"
ZIP="mighty-ide-$VERSION-win64.zip"
( cd dist && rm -f "$ZIP" && powershell.exe -NoProfile -Command \
    "Compress-Archive -Path '$PKG' -DestinationPath '$ZIP' -Force" )

echo "OK:"
ls -la "$DIST"
echo "---"
ls -la "dist/$ZIP"
