#!/usr/bin/env bash
# Build a release Mighty IDE and assemble a clean Linux x64 package.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="${VERSION:-v0.3.0}"
PKG="mighty-ide-linux-x64"
DIST="dist/$PKG"
ZIP="dist/mighty-ide-$VERSION-linux-x64.tar.gz"
MTY="${MIGHTY_MTY:-mty}"
CLANG="${CLANG:-clang}"
ORIGINAL_TOML="$ROOT/mighty.toml"
BACKUP_TOML="$ROOT/mighty.toml.package-backup"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "ERROR: package-linux.sh must run on Linux." >&2
  exit 1
fi
if ! command -v file >/dev/null 2>&1; then
  echo "ERROR: package-linux.sh requires the 'file' utility to verify native binary format." >&2
  exit 1
fi

cd "$ROOT"
if [[ -d .git ]] && command -v git >/dev/null 2>&1; then
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "ERROR: package-linux.sh requires a clean git worktree before building release artifacts." >&2
    exit 1
  fi
fi
export CARGO_INCREMENTAL=0

cleanup() {
  if [[ -f "$BACKUP_TOML" ]]; then
    mv "$BACKUP_TOML" "$ORIGINAL_TOML"
  fi
}
trap cleanup EXIT

echo "[1/6] release build"
cargo build --release -p mighty-ui-sys -p mty-rt-abi

echo "[2/6] stage native link artifacts"
mkdir -p vendor target/release
cp target/release/libmty_rt_abi.a vendor/libmty_rt_abi.a
cp target/release/libmighty_ui_sys.so vendor/libmighty_ui_sys.so

echo "[3/6] host packaging manifest"
cp "$ORIGINAL_TOML" "$BACKUP_TOML"
cat > "$ORIGINAL_TOML" <<'TOML'
[package]
name = "mighty-ide"
version = "0.3.0"
edition = "2026"

[build]
link-args = ["-Wl,-rpath,$ORIGIN"]

[[extern_lib]]
name = "mighty_ui_sys"
kind = "dynamic"
path = "vendor/libmighty_ui_sys.so"

[[extern_lib]]
name = "mtyrt"
kind = "static"
path = "vendor/libmty_rt_abi.a"
TOML

echo "[4/6] mty build --release"
MTY_LINKER="$CLANG" "$MTY" build --release src/main.mty --out-dir target/release

echo "[5/6] assemble $DIST"
rm -rf "$DIST"
mkdir -p "$DIST/examples" "$DIST/samples"
cp target/release/main "$DIST/mighty-ide"
cp target/release/libmighty_ui_sys.so "$DIST/libmighty_ui_sys.so"
cp samples/hello.mty "$DIST/samples/hello.mty"
cp samples/agents.mty "$DIST/samples/agents.mty"
cp samples/web-spinner.mty "$DIST/samples/web-spinner.mty"
cp examples/demo.mty "$DIST/examples/demo.mty"
for name in sample.py sample.rs sample.json agents.mty; do
  [[ -f "examples/$name" ]] && cp "examples/$name" "$DIST/examples/$name"
done
cp RUN.txt "$DIST/RUN.txt"

if command -v strip >/dev/null 2>&1; then
  strip "$DIST/mighty-ide" "$DIST/libmighty_ui_sys.so" || true
fi

file "$DIST/mighty-ide" "$DIST/libmighty_ui_sys.so"
for binary in "$DIST/mighty-ide" "$DIST/libmighty_ui_sys.so"; do
  file "$binary" | grep -q 'ELF' || {
    echo "ERROR: Linux package contains a non-ELF native binary: $binary" >&2
    exit 1
  }
done

if find "$DIST" -type f \( -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o -name '*.obj' -o -name '*.o' -o -name '*.rlib' -o -name '*.log' \) | grep -q .; then
  echo "ERROR: package contains build byproducts:" >&2
  find "$DIST" -type f \( -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o -name '*.obj' -o -name '*.o' -o -name '*.rlib' -o -name '*.log' \) >&2
  exit 1
fi
if find "$DIST" -type f \( -name '*.exe' -o -name '*.dll' -o -name '*.dylib' \) | grep -q .; then
  echo "ERROR: Linux package contains non-Linux native payloads:" >&2
  find "$DIST" -type f \( -name '*.exe' -o -name '*.dll' -o -name '*.dylib' \) >&2
  exit 1
fi

echo "[6/6] archive"
rm -f "$ZIP"
tar -C dist -czf "$ZIP" "$PKG"

find "$DIST" -maxdepth 3 -type f | sort
ls -lh "$ZIP"
