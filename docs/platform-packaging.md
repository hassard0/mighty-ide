# Platform Packaging

This repository does not commit release binaries. Packages are generated under
`dist/`, which is ignored by git, and uploaded separately by release automation
or a release operator.

Release documentation is part of the artifact contract. Update and commit the
README, changelog, build notes, and files in this `docs/` release set before
building a publishable archive. The package manifest records the exact source
commit used for the archive; if any tracked source or documentation file changes
after packaging, the archive is stale and must be rebuilt on that platform
before upload.

## Clean Artifact Rules

1. Start from a clean worktree.
2. Remove or replace the platform-specific package directory before assembling.
3. Build the Rust shim and Mighty executable in release mode for the same host
   OS that will run the package.
4. Bundle only the executable, native shim library, icon/assets needed at
   runtime, samples/examples, project docs, and platform-specific run
   instructions.
5. Reject build sidecars and obvious foreign-platform native payloads.
6. Write `PACKAGE-MANIFEST.txt` with the platform, version, source commit,
   native payload hashes and sizes, and completed clean-binary checks.
7. Smoke-test the packaged executable from inside the assembled package
   directory.
8. Keep generated packages out of commits.

The checked-in package scripts enforce these rules where they can: they refuse a
dirty git worktree, delete the previous platform package directory, reject common
compiler/linker sidecars such as PDBs, import/static libraries, object files,
`.dSYM` bundles, `.debug`/`.map` symbol files, and logs, reject obvious
foreign-platform native payloads, and validate the native binary family before
writing the archive. Windows checks PE headers in PowerShell and re-checks the
executable after icon stamping.
macOS and Linux require the standard `file` utility so Mach-O and ELF validation
cannot be silently skipped. After the archive is written, each script scans the
ZIP or tarball for the same sidecar and foreign-payload deny list, so the
uploaded artifact is checked directly rather than inferred from the staging
directory alone.
The scripts also remove their previous platform package directory and
same-version archive before build work starts. If compilation or assembly fails,
the stale archive for that platform is gone rather than left behind as apparent
release evidence.

The release invariant is one archive, one native binary family:

- Windows packages contain PE files only for native code.
- macOS packages contain Mach-O files only for native code.
- Linux packages contain ELF files only for native code.

Clean means the finished upload artifact was checked, not just the staging
directory. If a script builds a valid package tree and then the archive scan
finds a sidecar or foreign native payload, the release state is `hold` until
that archive is regenerated and rescanned.

A completed release has one verification record per uploaded platform. That
record must come from the package script and the native smoke test for that
platform, not from another OS package. If the Windows package is current but
macOS or Linux has not run on native infrastructure, the correct state is
`unbuilt`, not `derived from Windows`.

The source checkout can be clean while no platform binary is publishable yet.
Binary cleanliness is established only after the ignored platform package tree
and final archive are generated, scanned, and launched on the matching OS or CI
runner. Do not describe macOS or Linux binaries as clean from a Windows-only
pass; describe those paths as script-ready and leave their release decision as
`unbuilt`.

Release operators should use only three final states for each platform:

- `publish`: package script completed on the matching native host or runner, the
  archive-level scan passed, the manifest exists, and the packaged executable
  launched from the assembled package.
- `hold`: a native build exists but one required check failed or has not been
  recorded yet.
- `unbuilt`: no matching native host or runner produced the archive for this
  pass.

For this Windows-based finalization pass, Windows x64 may move to `publish`
after the PowerShell package script and packaged launch succeed here. macOS and
Linux remain `unbuilt` until their own scripts run and launch on native
infrastructure; the presence of `package-macos.sh` and `package-linux.sh` is
script readiness, not binary cleanliness.
The optional Bash Windows packager follows the same final-artifact rule: it
bundles this verification guide and scans the ZIP after compression, but the
PowerShell script remains the canonical Windows release path on this host.

The sidecar scan is intentionally shared in spirit across all three scripts:
package trees and finished archives must not contain `.pdb`, `.lib`, `.exp`,
`.ilk`, `.obj`, `.o`, `.a`, `.rlib`, `.log`, `.debug`, `.map`, or `.dSYM`
artifacts. A platform can still be stripped or symbolized before packaging, but
those intermediate files must stay outside the release archive.

If a platform archive cannot be built and smoke-tested on its native OS or a
matching CI runner, do not publish a placeholder archive for that platform.
Publish the verified platforms and leave the missing platform unbuilt until a
native runner can produce and validate it.

Every package must include these human-readable files at the package root unless
the root is an `.app` archive, in which case they live beside the `.app` in the
tarball:

- `RUN.txt` with native instructions for that platform
- `PACKAGE-MANIFEST.txt` with source commit, native payload hashes, sizes, and
  clean checks
- `README.md`
- `KEYBINDINGS.md`
- `CHANGELOG.md`
- `BUILDING.md`
- `LICENSE`
- `docs/platform-packaging.md`
- `docs/release-verification.md`
- `docs/release-evidence.md`
- `docs/binary-release-status.md`
- `docs/final-release-handoff.md`

## Platform Matrix

| Platform | Native binary shape | Current repo support |
|----------|---------------------|----------------------|
| Windows x64 | PE `mighty-ide.exe` plus PE `mighty_ui_sys.dll` | `package-win.ps1` creates `dist/mighty-ide-win64/` and `dist/mighty-ide-v0.3.0-win64.zip`; rejects `.dylib` and `.so` payloads |
| macOS | `.app` archive containing Mach-O executable plus `.dylib` dependencies | `package-macos.sh` creates `dist/mighty-ide-macos/` and `dist/mighty-ide-v0.3.0-macos.tar.gz` on macOS; rejects `.exe`, `.dll`, and `.so` payloads |
| Linux x64 | ELF executable plus ELF `.so` dependencies in a tarball directory | `package-linux.sh` creates `dist/mighty-ide-linux-x64/` and `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` on Linux; rejects `.exe`, `.dll`, and `.dylib` payloads |

## Windows Procedure

```powershell
Get-Process mighty-ide -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
Start-Sleep -Milliseconds 300
.\package-win.ps1
```

Expected artifacts:

- `dist/mighty-ide-win64/mighty-ide.exe`
- `dist/mighty-ide-win64/mighty_ui_sys.dll`
- `dist/mighty-ide-win64/RUN.txt`
- `dist/mighty-ide-win64/Create-Desktop-Shortcut.ps1`
- `dist/mighty-ide-v0.3.0-win64.zip`

The script checks both packaged binaries for PE headers and fails if any common
build byproduct or Unix native library payload is found in either the package
directory or the final ZIP.

Smoke-test by launching:

```powershell
Start-Process -FilePath "dist\mighty-ide-win64\mighty-ide.exe" `
  -WorkingDirectory "dist\mighty-ide-win64"
```

## macOS and Linux

macOS and Linux packages must be produced on native hosts or matching CI
runners. The current Windows package cannot be converted into a clean macOS or
Linux binary because the executable and shim are native artifacts.

On macOS:

```sh
./package-macos.sh
```

On Linux:

```sh
./package-linux.sh
```

Set `MIGHTY_MTY=/path/to/mty` if the Mighty compiler is not on `PATH`. The
selected compiler must report v0.47.0 or newer from `mty --version`; the build
and package scripts reject older compilers before they can fail later with
parser noise from `src/main.mty`. Set `CLANG=/path/to/clang` if the default
`clang` executable is not the intended linker.

Both scripts:

- refuse to run on the wrong host OS
- refuse to run from a dirty git worktree
- reject stale Mighty compilers before release build work starts
- require the `file` utility for native binary validation
- remove the previous platform package directory before assembly
- remove the previous same-version platform archive before building
- build `mighty-ui-sys` and `mty-rt-abi` in release mode
- generate a temporary host-specific `mighty.toml` and restore the checked-in
  Windows manifest on exit
- copy only the executable, native shim library, samples/examples, and run docs
- copy the README, keybindings, changelog, build notes, license, and platform
  packaging notes into the archive
- strip symbols when the platform `strip` tool is available
- verify the staged native binaries with `file`
- reject native payloads that belong to another OS family
- write a platform tarball under `dist/`
- scan the platform tarball for build sidecars and foreign native payloads

The resulting archives should be smoke-tested from the assembled package
directory before upload. Record the final result with
[`release-verification.md`](release-verification.md) and the concise upload
record in [`release-evidence.md`](release-evidence.md).

## Verification Commands

These commands are intentionally explicit so a release note can include the
same facts for every published archive: archive size, SHA-256, native binary
family, absence of build sidecars, absence of foreign native payloads, and a
packaged launch. Compare them with the bundled `PACKAGE-MANIFEST.txt`; if the
manifest is missing or disagrees with the assembled package, rebuild before
uploading.

Windows PowerShell:

```powershell
Get-Item dist\mighty-ide-v0.3.0-win64.zip |
  Select-Object FullName,Length,LastWriteTime
Get-FileHash dist\mighty-ide-v0.3.0-win64.zip -Algorithm SHA256

Get-ChildItem dist\mighty-ide-win64 -Recurse -File |
  Where-Object { $_.Extension -in @(
    '.pdb','.lib','.exp','.ilk','.obj','.o','.a','.rlib','.log','.debug',
    '.map','.dylib','.so'
  ) } |
  Select-Object FullName,Length
Get-ChildItem dist\mighty-ide-win64 -Recurse -Directory |
  Where-Object { $_.Extension -eq '.dSYM' } |
  Select-Object FullName

@('dist\mighty-ide-win64\mighty-ide.exe',
  'dist\mighty-ide-win64\mighty_ui_sys.dll') |
  ForEach-Object {
    $path = Resolve-Path $_
    $fs = [System.IO.File]::OpenRead($path)
    try {
      $br = [System.IO.BinaryReader]::new($fs)
      $mz = ('{0:X2}{1:X2}' -f $br.ReadByte(), $br.ReadByte())
      $fs.Seek(0x3c, [System.IO.SeekOrigin]::Begin) | Out-Null
      $off = $br.ReadInt32()
      $fs.Seek($off, [System.IO.SeekOrigin]::Begin) | Out-Null
      $pe = ('{0:X2}{1:X2}{2:X2}{3:X2}' -f
        $br.ReadByte(), $br.ReadByte(), $br.ReadByte(), $br.ReadByte())
      [PSCustomObject]@{ Path = $_; MZ = $mz; PE = $pe }
    } finally {
      $fs.Dispose()
    }
  }

Start-Process -FilePath "dist\mighty-ide-win64\mighty-ide.exe" `
  -WorkingDirectory "dist\mighty-ide-win64"
```

macOS:

```sh
shasum -a 256 dist/mighty-ide-v0.3.0-macos.tar.gz
ls -lh dist/mighty-ide-v0.3.0-macos.tar.gz
file "dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/mighty-ide" \
  "dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/libmighty_ui_sys.dylib"
find dist/mighty-ide-macos \( -type f \( \
  -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o \
  -name '*.obj' -o -name '*.o' -o -name '*.a' -o -name '*.rlib' -o \
  -name '*.log' -o -name '*.debug' -o -name '*.map' -o \
  -name '*.exe' -o -name '*.dll' -o -name '*.so' \) -o \
  -type d -name '*.dSYM' \)
"dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/mighty-ide"
```

Linux:

```sh
sha256sum dist/mighty-ide-v0.3.0-linux-x64.tar.gz
ls -lh dist/mighty-ide-v0.3.0-linux-x64.tar.gz
file dist/mighty-ide-linux-x64/mighty-ide \
  dist/mighty-ide-linux-x64/libmighty_ui_sys.so
find dist/mighty-ide-linux-x64 \( -type f \( \
  -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o \
  -name '*.obj' -o -name '*.o' -o -name '*.a' -o -name '*.rlib' -o \
  -name '*.log' -o -name '*.debug' -o -name '*.map' -o \
  -name '*.exe' -o -name '*.dll' -o -name '*.dylib' \) -o \
  -type d -name '*.dSYM' \)
(cd dist/mighty-ide-linux-x64 && ./mighty-ide)
```

An empty `find` result is expected for the sidecar/foreign-payload scans. If
any path is printed, fix the package before publishing it.

## Native Host Gate

The scripts are deliberately host-specific:

- `package-win.ps1` runs on Windows and validates PE headers directly.
- `package-macos.sh` runs only when `uname -s` is `Darwin` and validates Mach-O
  payloads with `file`.
- `package-linux.sh` runs only when `uname -s` is `Linux` and validates ELF
  payloads with `file`.

Those guards are part of the release process. A platform is clean only after
its own script has completed, its archive has passed the archive-level scan,
and the packaged executable has launched from the assembled package directory
or app bundle. Cross-host script syntax checks are useful maintenance, but they
do not create a shippable macOS or Linux binary.

## Current Host Verification

From this Windows checkout, the release operator can fully rebuild and verify
only the Windows x64 package. A clean Windows pass means:

- `package-win.ps1` completed from a clean committed tree.
- `dist/mighty-ide-win64/mighty-ide.exe` and
  `dist/mighty-ide-win64/mighty_ui_sys.dll` both passed PE header checks.
- The staged tree and ZIP contain no compiler/linker sidecars and no `.dylib`
  or `.so` payloads.
- `PACKAGE-MANIFEST.txt` records the Windows payload hashes and sizes.
- `PACKAGE-MANIFEST.txt` records the source commit used for the package.
- The packaged executable launched with its working directory set to
  `dist/mighty-ide-win64`.

macOS and Linux readiness from a Windows host is limited to source maintenance:
the packaging scripts can be reviewed, shell syntax can be checked where a Bash
toolchain is available, and the host gates can be verified to refuse the wrong
OS. Those checks are useful, but they do not produce clean macOS or Linux
binaries. A macOS or Linux archive is publishable only after its own native
script completes on that platform or on a matching CI runner and the packaged
app is smoke-tested there.

## Release Note Evidence

Record the same evidence for every archive that is uploaded. The canonical
template lives in [`release-verification.md`](release-verification.md), with a
fill-in upload record in [`release-evidence.md`](release-evidence.md). If a
platform was not built on its native host or matching CI runner, list it as
unbuilt instead of publishing a placeholder or reused binary.

```text
Platform:
Archive:
Size:
SHA-256:
Native payloads:
Sidecar / foreign-payload scan:
PACKAGE-MANIFEST.txt:
Packaged launch:
```

The `Native payloads` line should name the binary family verified by the
package script: PE for Windows, Mach-O for macOS, and ELF for Linux. The
manifest line should summarize the platform, version, source commit, payload
hashes, payload sizes, and clean-binary checks recorded in
`PACKAGE-MANIFEST.txt`.
