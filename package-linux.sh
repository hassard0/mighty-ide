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
mkdir -p "$DIST/examples" "$DIST/samples" "$DIST/docs"
cp target/release/main "$DIST/mighty-ide"
cp target/release/libmighty_ui_sys.so "$DIST/libmighty_ui_sys.so"
cp samples/hello.mty "$DIST/samples/hello.mty"
cp samples/agents.mty "$DIST/samples/agents.mty"
cp samples/web-spinner.mty "$DIST/samples/web-spinner.mty"
cp examples/demo.mty "$DIST/examples/demo.mty"
for name in sample.py sample.rs sample.json agents.mty; do
  [[ -f "examples/$name" ]] && cp "examples/$name" "$DIST/examples/$name"
done
cp README.md KEYBINDINGS.md CHANGELOG.md BUILDING.md LICENSE "$DIST/"
cp docs/platform-packaging.md "$DIST/docs/platform-packaging.md"
cat > "$DIST/RUN.txt" <<'RUN'
Mighty IDE - Linux (x64)
========================

HOW TO RUN
----------
1. Keep mighty-ide and libmighty_ui_sys.so in the same directory. The executable
   is linked with an rpath that loads the shim from its own package directory.
2. From this folder, launch:
       ./mighty-ide
   or open a specific file:
       ./mighty-ide path/to/file
       ./mighty-ide samples/hello.mty
3. If the executable bit is lost after unpacking, restore it with:
       chmod +x mighty-ide

SAMPLES TO EXPLORE
------------------
The samples/ folder includes a few .mty programs:
    samples/hello.mty
    samples/agents.mty
    samples/web-spinner.mty

WHAT WORKS STANDALONE
---------------------
The packaged executable and shared library are enough for editing, tabs, split
panes, search, Quick Open, command palette, minimap, Markdown preview, the
integrated terminal, and bundled themes.

WHAT NEEDS EXTRA TOOLS ON PATH
------------------------------
- Mighty language services, Run, Test, Debug, New Project, and web builds need
  the Mighty compiler `mty` on PATH.
- Building Mighty programs needs `mty` and a C toolchain such as clang.
- AI copilot features need ANTHROPIC_API_KEY in the environment.
RUN

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

if find "$DIST" \( -type f \( -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o -name '*.obj' -o -name '*.o' -o -name '*.a' -o -name '*.rlib' -o -name '*.log' -o -name '*.debug' -o -name '*.map' \) -o -type d -name '*.dSYM' \) | grep -q .; then
  echo "ERROR: package contains build byproducts:" >&2
  find "$DIST" \( -type f \( -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o -name '*.obj' -o -name '*.o' -o -name '*.a' -o -name '*.rlib' -o -name '*.log' -o -name '*.debug' -o -name '*.map' \) -o -type d -name '*.dSYM' \) >&2
  exit 1
fi
if find "$DIST" -type f \( -name '*.exe' -o -name '*.dll' -o -name '*.dylib' \) | grep -q .; then
  echo "ERROR: Linux package contains non-Linux native payloads:" >&2
  find "$DIST" -type f \( -name '*.exe' -o -name '*.dll' -o -name '*.dylib' \) >&2
  exit 1
fi

{
  echo "Mighty IDE package verification"
  echo "Platform: Linux x64"
  echo "Version: $VERSION"
  echo "Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo
  echo "Native payloads:"
  for binary in "$DIST/mighty-ide" "$DIST/libmighty_ui_sys.so"; do
    size="$(wc -c < "$binary" | tr -d ' ')"
    hash="$(sha256sum "$binary" | awk '{print $1}')"
    echo "- $(basename "$binary") | ELF | $size bytes | SHA256 $hash"
  done
  echo
  echo "Archive: $ZIP"
  echo "Clean binary checks:"
  echo "- ELF format verified for mighty-ide and libmighty_ui_sys.so"
  echo "- No compiler/linker sidecars found"
  echo "- No non-Linux native payloads found"
} > "$DIST/PACKAGE-MANIFEST.txt"

echo "[6/6] archive"
rm -f "$ZIP"
tar -C dist -czf "$ZIP" "$PKG"
if tar -tzf "$ZIP" | grep -E '\.(pdb|lib|exp|ilk|obj|o|a|rlib|log|debug|map|exe|dll|dylib)$|\.dSYM(/|$)' >/dev/null; then
  echo "ERROR: archive contains build byproducts or non-Linux native payloads:" >&2
  tar -tzf "$ZIP" | grep -E '\.(pdb|lib|exp|ilk|obj|o|a|rlib|log|debug|map|exe|dll|dylib)$|\.dSYM(/|$)' >&2
  exit 1
fi

find "$DIST" -maxdepth 3 -type f | sort
ls -lh "$ZIP"
