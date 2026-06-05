# Building Mighty IDE

## Prerequisites

| Tool | What it does | How it is resolved |
|------|--------------|--------------------|
| **`mty`** (Mighty compiler) | builds `src/main.mty` → `target/main.exe` | Set `MIGHTY_MTY` or put `mty` on PATH |
| **Rust** toolchain | builds the `mighty-ui-sys` shim cdylib + the arena runtime | `cargo` on PATH |
| **clang** | the linker `mty build` drives (GNU `-o` arg syntax) | `C:\Program Files\LLVM\bin\clang.exe` |
| **llvm-ar** | archives the arena runtime staticlib | `C:\Program Files\LLVM\bin\llvm-ar.exe` |

`mty build` cannot use MSVC `link.exe` (wrong arg syntax) — **clang is required**.

If `mty` is missing, build it from the [Mighty](https://github.com/hassard0/Mighty) repo:

```sh
cargo build -p mty-cli --bin mty
```

Build scripts resolve the Mighty compiler from `MIGHTY_MTY` first and then
from `mty` on PATH. The PowerShell entry points also accept `-Mty`.

## One-shot build

```sh
./build-ide.sh
```

This:
1. `cargo build -p mighty-ui-sys -p mty-rt-abi` — the Vello/wgpu shim (cdylib) + the bumpalo arena runtime (staticlib)
2. stages `target/debug/mty_rt_abi.lib` → `vendor/mty_rt_abi.lib`
3. copies the shim import lib + DLL next to the output exe
4. `mty build src/main.mty --out-dir target` → `target/main.exe`

On Windows without Bash/Git-Bash, use the native PowerShell wrapper:

```powershell
.\build-ide.ps1
```

For the release/package build:

```powershell
.\package-win.ps1
```

This assembles `dist\mighty-ide-win64\` and writes
`dist\mighty-ide-v0.3.0-win64.zip`.

## Platform packaging status

Generated binaries live under `dist/`, which is intentionally ignored by git.
Start every release package from a clean worktree and a freshly assembled
platform directory. The packaging scripts enforce this by refusing dirty git
state, removing the previous platform package directory, rejecting common build
byproducts such as object files, import/static archives, PDB/ILK files, `.dSYM`
bundles, `.debug`/`.map` symbol files, and logs, rejecting obvious
foreign-platform native files, and checking that the staged native binaries
match the host platform format before the archive is written.
After compression, the scripts scan the finished ZIP or tarball for the same
sidecar and foreign-platform deny list before reporting success.
Each script removes its previous platform package directory and same-version
archive before the release build starts, so a failed package run cannot leave an
older artifact in `dist/` that looks current.
The scripts also bundle the README, license, keybinding reference,
changelog, build notes, platform packaging notes, samples, and platform-specific
`RUN.txt` instructions. They also write `PACKAGE-MANIFEST.txt` into the package
root with platform/version metadata, native payload hashes and sizes, and the
clean binary checks completed before archiving. Windows performs PE checks
directly in PowerShell; macOS and Linux require the standard `file` utility and
fail if the packaged payload is not Mach-O or ELF respectively.

| Platform | Current command | Artifact | Status |
|----------|-----------------|----------|--------|
| Windows x64 | `.\package-win.ps1` | `dist\mighty-ide-v0.3.0-win64.zip` | Verifies PE exe/dll; rejects sidecars, `.dylib`, and `.so` payloads |
| macOS | `./package-macos.sh` on macOS | `dist/mighty-ide-v0.3.0-macos.tar.gz` | Verifies Mach-O app payloads; rejects sidecars, `.exe`, `.dll`, and `.so` payloads |
| Linux x64 | `./package-linux.sh` on Linux | `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` | Verifies ELF executable/shared object; rejects sidecars, `.exe`, `.dll`, and `.dylib` payloads |

Do not cross-ship artifacts between platforms. The Rust shim is a native
dynamic library (`.dll`, `.dylib`, or `.so`) and the Mighty executable links to
the host platform's ABI, so each OS package must be built and smoke-tested on
that OS or on a matching CI runner. The release checklist is maintained in
[`docs/platform-packaging.md`](docs/platform-packaging.md), the upload evidence
template is in [`docs/release-verification.md`](docs/release-verification.md),
the final upload record is in
[`docs/release-evidence.md`](docs/release-evidence.md), and the final
stop/pass handoff is in
[`docs/final-release-handoff.md`](docs/final-release-handoff.md).

Before uploading a release archive, keep the package directory and archive
together long enough to verify:

- archive size and SHA-256 hash
- native binary family for every packaged executable/shared library
- absence of compiler/linker byproducts
- absence of foreign-platform native payloads
- bundled `PACKAGE-MANIFEST.txt` hash/size and clean-binary summary
- completed `docs/release-verification.md` evidence record for the archive
- completed `docs/release-evidence.md` upload record for the archive
- packaged `docs/final-release-handoff.md` stop/publish rule
- packaged launch from inside the assembled directory or app bundle

Windows verification can be completed from this checkout. macOS and Linux
verification must be completed on native hosts or matching CI runners; the
checked-in scripts intentionally refuse to run on the wrong OS.

## Final release procedure

Use this order for a publishable pass:

1. Commit source, README, changelog, build notes, and packaging docs.
2. Run the package script on the native host for each platform:
   `.\package-win.ps1`, `./package-macos.sh`, or `./package-linux.sh`.
3. Confirm the script wrote `PACKAGE-MANIFEST.txt` in the package root and that
   the manifest lists the expected native family: PE, Mach-O, or ELF.
4. Launch the packaged app from inside the assembled package directory or app
   bundle, not from `target/`.
5. Record archive size, SHA-256, native payload family, sidecar/foreign-payload
   scan result, manifest summary, and packaged launch result.

If a native macOS or Linux host is not available, leave that platform unbuilt
for the release. Do not rename the Windows ZIP, reuse its DLL, or publish an
empty placeholder for another OS.

## Run

```sh
./target/main.exe path/to/file    # defaults to ./scratch.mty if omitted
```

## Environment variables

- `MTY_LINKER` — point `mty build` at clang. `build-ide.sh` sets it to the clang path above.
- `ANTHROPIC_API_KEY` — enables the AI copilot panel (Ctrl+Shift+A). Optional.

## Disk / link notes

- Build with **`CARGO_INCREMENTAL=0`** to avoid the large incremental cache:

  ```sh
  CARGO_INCREMENTAL=0 ./build-ide.sh
  ```

- If the link step fails on disk space, clear the incremental cache and retry:

  ```sh
  rm -rf target/debug/incremental
  ```

## Verifying the shim

```sh
CARGO_INCREMENTAL=0 cargo clippy -p mighty-ui-sys     # lint
CARGO_INCREMENTAL=0 cargo test  -p mighty-ui-sys      # ~293 unit/integration tests
```
