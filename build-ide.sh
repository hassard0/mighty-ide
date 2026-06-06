#!/usr/bin/env bash
# build-ide.sh — build the Mighty IDE end to end.
#
#   1. cargo-build the wgpu shim as a cdylib (mighty_ui_sys.dll + .dll.lib)
#      and the REAL bumpalo-arena runtime (crates/mty-rt-abi, staticlib)
#   2. stage the runtime archive into vendor/mty_rt_abi.lib
#   3. copy the shim import lib + DLL next to the output exe
#   4. mty build src/main.mty -> target/main.exe
#
# Toolchain:
#   clang   override with CLANG=/path/to/clang
#   mty     override with MIGHTY_MTY=/path/to/mty, otherwise resolve from PATH
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLANG="${CLANG:-C:\\Program Files\\LLVM\\bin\\clang.exe}"
MTY="${MIGHTY_MTY:-mty}"
MIN_MTY_VERSION="0.47.0"

cd "$ROOT"

check_mty_version() {
  local text version IFS
  text="$("$MTY" --version 2>/dev/null || true)"
  if [[ ! "$text" =~ ([0-9]+)\.([0-9]+)\.([0-9]+) ]]; then
    echo "ERROR: unable to parse Mighty compiler version from: $text" >&2
    exit 1
  fi
  version="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}"
  IFS=. read -r -a got <<<"$version"
  IFS=. read -r -a need <<<"$MIN_MTY_VERSION"
  for i in 0 1 2; do
    if (( got[i] > need[i] )); then return 0; fi
    if (( got[i] < need[i] )); then
      echo "ERROR: mty compiler $version is too old for Mighty IDE; require $MIN_MTY_VERSION or newer. Set MIGHTY_MTY to a current compiler." >&2
      exit 1
    fi
  done
}

if [[ "$MTY" == */* || "$MTY" == *\\* ]]; then
  if [[ ! -x "$MTY" && ! -f "$MTY" ]]; then
    echo "ERROR: MIGHTY_MTY points to a missing compiler: $MTY" >&2
    exit 1
  fi
elif ! command -v "$MTY" >/dev/null 2>&1; then
  echo "ERROR: mty compiler not found. Set MIGHTY_MTY or put mty on PATH." >&2
  exit 1
fi
check_mty_version

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
rm -f target/main.exe target/main.o
MTY_LINKER="$CLANG" "$MTY" build src/main.mty --out-dir target

if [[ ! -s target/main.exe ]]; then
  echo "ERROR: mty build did not produce target/main.exe" >&2
  if [[ -s target/main.o ]]; then
    echo "       target/main.o exists, so the compiler stopped after object emission." >&2
    echo "       Check linker discovery / MTY_LINKER handling." >&2
  fi
  exit 1
fi

ls -la target/main.exe
echo "OK: target/main.exe"
