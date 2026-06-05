#!/usr/bin/env bash
# Build a release Mighty IDE and assemble a clean macOS app archive.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="${VERSION:-v0.3.0}"
PKG="Mighty IDE.app"
DIST_ROOT="dist/mighty-ide-macos"
APP="$DIST_ROOT/$PKG"
MACOS="$APP/Contents/MacOS"
RESOURCES="$APP/Contents/Resources"
ZIP="dist/mighty-ide-$VERSION-macos.tar.gz"
MTY="${MIGHTY_MTY:-mty}"
CLANG="${CLANG:-clang}"
ORIGINAL_TOML="$ROOT/mighty.toml"
BACKUP_TOML="$ROOT/mighty.toml.package-backup"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "ERROR: package-macos.sh must run on macOS." >&2
  exit 1
fi
if ! command -v file >/dev/null 2>&1; then
  echo "ERROR: package-macos.sh requires the 'file' utility to verify native binary format." >&2
  exit 1
fi

cd "$ROOT"
if [[ "$MTY" == */* || "$MTY" == *\\* ]]; then
  if [[ ! -x "$MTY" && ! -f "$MTY" ]]; then
    echo "ERROR: MIGHTY_MTY points to a missing compiler: $MTY" >&2
    exit 1
  fi
elif ! command -v "$MTY" >/dev/null 2>&1; then
  echo "ERROR: mty compiler not found. Set MIGHTY_MTY or put mty on PATH." >&2
  exit 1
fi
if [[ -d .git ]] && command -v git >/dev/null 2>&1; then
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "ERROR: package-macos.sh requires a clean git worktree before building release artifacts." >&2
    exit 1
  fi
fi
rm -rf "$DIST_ROOT"
rm -f "$ZIP"
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
cp target/release/libmighty_ui_sys.dylib vendor/libmighty_ui_sys.dylib

echo "[3/6] host packaging manifest"
cp "$ORIGINAL_TOML" "$BACKUP_TOML"
cat > "$ORIGINAL_TOML" <<'TOML'
[package]
name = "mighty-ide"
version = "0.3.0"
edition = "2026"

[build]
link-args = ["-Wl,-rpath,@executable_path"]

[[extern_lib]]
name = "mighty_ui_sys"
kind = "dynamic"
path = "vendor/libmighty_ui_sys.dylib"

[[extern_lib]]
name = "mtyrt"
kind = "static"
path = "vendor/libmty_rt_abi.a"
TOML

echo "[4/6] mty build --release"
MTY_LINKER="$CLANG" "$MTY" build --release src/main.mty --out-dir target/release

echo "[5/6] assemble $APP"
rm -rf "$DIST_ROOT"
mkdir -p "$MACOS/examples" "$MACOS/samples" "$RESOURCES" "$DIST_ROOT/docs"
cp target/release/main "$MACOS/mighty-ide"
cp target/release/libmighty_ui_sys.dylib "$MACOS/libmighty_ui_sys.dylib"
cp samples/hello.mty "$MACOS/samples/hello.mty"
cp samples/agents.mty "$MACOS/samples/agents.mty"
cp samples/web-spinner.mty "$MACOS/samples/web-spinner.mty"
cp examples/demo.mty "$MACOS/examples/demo.mty"
for name in sample.py sample.rs sample.json agents.mty; do
  [[ -f "examples/$name" ]] && cp "examples/$name" "$MACOS/examples/$name"
done
cp README.md KEYBINDINGS.md CHANGELOG.md BUILDING.md LICENSE "$DIST_ROOT/"
cp docs/platform-packaging.md "$DIST_ROOT/docs/platform-packaging.md"
cp docs/release-verification.md "$DIST_ROOT/docs/release-verification.md"
cp docs/final-release-handoff.md "$DIST_ROOT/docs/final-release-handoff.md"
cat > "$DIST_ROOT/RUN.txt" <<'RUN'
Mighty IDE - macOS
==================

HOW TO RUN
----------
1. Open "Mighty IDE.app" from Finder, or launch the executable from a terminal:
       "Mighty IDE.app/Contents/MacOS/mighty-ide"
2. Open a specific file from a terminal:
       "Mighty IDE.app/Contents/MacOS/mighty-ide" path/to/file
3. Keep the app bundle intact. The executable expects libmighty_ui_sys.dylib in
   Contents/MacOS beside it.

SAMPLES TO EXPLORE
------------------
The app bundle includes a few .mty samples under:
    Mighty IDE.app/Contents/MacOS/samples/

WHAT WORKS STANDALONE
---------------------
The packaged app contains the executable, native shim, bundled fonts, samples,
and app metadata needed for editing, tabs, split panes, search, Quick Open,
command palette, minimap, Markdown preview, integrated terminal, and themes.

WHAT NEEDS EXTRA TOOLS ON PATH
------------------------------
- Mighty language services, Run, Test, Debug, New Project, and web builds need
  the Mighty compiler `mty` on PATH.
- Building Mighty programs needs `mty` and a C toolchain such as clang.
- AI copilot features need ANTHROPIC_API_KEY in the environment.
RUN
cp "$DIST_ROOT/RUN.txt" "$MACOS/RUN.txt"
[[ -f assets/mighty-ide.icns ]] && cp assets/mighty-ide.icns "$RESOURCES/mighty-ide.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key>
  <string>Mighty IDE</string>
  <key>CFBundleExecutable</key>
  <string>mighty-ide</string>
  <key>CFBundleIdentifier</key>
  <string>dev.mighty.ide</string>
  <key>CFBundleName</key>
  <string>Mighty IDE</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.3.0</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

if command -v strip >/dev/null 2>&1; then
  strip "$MACOS/mighty-ide" "$MACOS/libmighty_ui_sys.dylib" || true
fi

file "$MACOS/mighty-ide" "$MACOS/libmighty_ui_sys.dylib"
for binary in "$MACOS/mighty-ide" "$MACOS/libmighty_ui_sys.dylib"; do
  file "$binary" | grep -q 'Mach-O' || {
    echo "ERROR: macOS package contains a non-Mach-O native binary: $binary" >&2
    exit 1
  }
done

if find "$DIST_ROOT" \( -type f \( -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o -name '*.obj' -o -name '*.o' -o -name '*.a' -o -name '*.rlib' -o -name '*.log' -o -name '*.debug' -o -name '*.map' \) -o -type d -name '*.dSYM' \) | grep -q .; then
  echo "ERROR: package contains build byproducts:" >&2
  find "$DIST_ROOT" \( -type f \( -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o -name '*.obj' -o -name '*.o' -o -name '*.a' -o -name '*.rlib' -o -name '*.log' -o -name '*.debug' -o -name '*.map' \) -o -type d -name '*.dSYM' \) >&2
  exit 1
fi
if find "$DIST_ROOT" -type f \( -name '*.exe' -o -name '*.dll' -o -name '*.so' \) | grep -q .; then
  echo "ERROR: macOS package contains non-macOS native payloads:" >&2
  find "$DIST_ROOT" -type f \( -name '*.exe' -o -name '*.dll' -o -name '*.so' \) >&2
  exit 1
fi

{
  echo "Mighty IDE package verification"
  echo "Platform: macOS"
  echo "Version: $VERSION"
  echo "Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo
  echo "Native payloads:"
  for binary in "$MACOS/mighty-ide" "$MACOS/libmighty_ui_sys.dylib"; do
    size="$(wc -c < "$binary" | tr -d ' ')"
    hash="$(shasum -a 256 "$binary" | awk '{print $1}')"
    echo "- $(basename "$binary") | Mach-O | $size bytes | SHA256 $hash"
  done
  echo
  echo "Archive: $ZIP"
  echo "Clean binary checks:"
  echo "- Mach-O format verified for mighty-ide and libmighty_ui_sys.dylib"
  echo "- No compiler/linker sidecars found"
  echo "- No non-macOS native payloads found"
} > "$DIST_ROOT/PACKAGE-MANIFEST.txt"

echo "[6/6] archive"
rm -f "$ZIP"
tar -C dist -czf "$ZIP" "mighty-ide-macos"
if tar -tzf "$ZIP" | grep -E '\.(pdb|lib|exp|ilk|obj|o|a|rlib|log|debug|map|exe|dll|so)$|\.dSYM(/|$)' >/dev/null; then
  echo "ERROR: archive contains build byproducts or non-macOS native payloads:" >&2
  tar -tzf "$ZIP" | grep -E '\.(pdb|lib|exp|ilk|obj|o|a|rlib|log|debug|map|exe|dll|so)$|\.dSYM(/|$)' >&2
  exit 1
fi

find "$DIST_ROOT" -maxdepth 5 -type f | sort
ls -lh "$ZIP"
