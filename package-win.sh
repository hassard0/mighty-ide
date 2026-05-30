#!/usr/bin/env bash
# package-win.sh — build a RELEASE Mighty IDE and assemble a distributable
# double-clickable Windows package.
#
#   1. cargo build --release the wgpu shim cdylib (mighty_ui_sys.dll) and the
#      real bumpalo-arena runtime (mty-rt-abi, staticlib).
#   2. stage the RELEASE import lib + runtime archive into vendor/ (mighty.toml
#      links these by path), so the IDE exe links against release artifacts.
#   3. mty build --release src/main.mty -> target/release/main.exe.
#   4. assemble dist/mighty-ide-win64/ with the renamed exe (icon-stamped via
#      rcedit), the release DLL, the brand .ico, the showcase samples/, the
#      Create-Desktop-Shortcut.ps1 helper and RUN.txt (fonts are EMBEDDED in the
#      DLL via include_bytes!, so no fonts/ dir is shipped).
#   5. zip -> dist/mighty-ide-v0.3.0-win64.zip.
#
# Icon tooling: tools/make-icon.py (Pillow) renders assets/mighty-ide.ico; the
# exe icon is stamped with tools/rcedit-x64.exe (electron/rcedit v2.0.0).
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

echo "[4/5] assemble dist/$PKG/ (icon-stamp + samples + scripts)"
DIST="dist/$PKG"
ICON="assets/mighty-ide.ico"
RCEDIT="tools/rcedit-x64.exe"
rm -rf "$DIST"
mkdir -p "$DIST/examples" "$DIST/samples"
cp target/release/main.exe           "$DIST/mighty-ide.exe"
cp target/release/mighty_ui_sys.dll  "$DIST/mighty_ui_sys.dll"

# --- App icon: regenerate the .ico (best-effort) then stamp the exe ---------
# `make-icon.py` renders the brand "M" mark at 16/32/48/256 into assets/.
if command -v python >/dev/null 2>&1; then
  python tools/make-icon.py || echo "WARN: icon regen failed; using existing $ICON"
fi
if [ -f "$ICON" ] && [ -f "$RCEDIT" ]; then
  echo "  stamping icon onto mighty-ide.exe via rcedit"
  "$RCEDIT" "$DIST/mighty-ide.exe" --set-icon "$ICON"
  # Bundle the .ico too so the desktop-shortcut script can point at it.
  cp "$ICON" "$DIST/mighty-ide.ico"
else
  echo "WARN: missing $ICON or $RCEDIT — exe icon NOT stamped"
fi

# --- Showcase samples so the tree / Welcome / Open Recent have content ------
cp samples/hello.mty       "$DIST/samples/hello.mty"
cp samples/agents.mty      "$DIST/samples/agents.mty"
cp samples/web-spinner.mty "$DIST/samples/web-spinner.mty"
# Legacy examples (kept for backwards-compat with older docs / links).
cp examples/demo.mty   "$DIST/examples/demo.mty"
cp examples/sample.py  "$DIST/examples/sample.py"  2>/dev/null || true
cp examples/sample.rs  "$DIST/examples/sample.rs"  2>/dev/null || true
cp examples/sample.json "$DIST/examples/sample.json" 2>/dev/null || true
cp examples/agents.mty "$DIST/examples/agents.mty" 2>/dev/null || true

# --- Scripts + docs ---------------------------------------------------------
cp Create-Desktop-Shortcut.ps1 "$DIST/Create-Desktop-Shortcut.ps1"
cp RUN.txt             "$DIST/RUN.txt"

echo "[5/5] zip -> dist/mighty-ide-$VERSION-win64.zip"
ZIP="mighty-ide-$VERSION-win64.zip"
( cd dist && rm -f "$ZIP" && powershell.exe -NoProfile -Command \
    "Compress-Archive -Path '$PKG' -DestinationPath '$ZIP' -Force" )

echo "OK:"
ls -la "$DIST"
echo "---"
ls -la "dist/$ZIP"
