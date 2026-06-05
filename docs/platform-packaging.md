# Platform Packaging

This repository does not commit release binaries. Packages are generated under
`dist/`, which is ignored by git, and uploaded separately by release automation
or a release operator.

## Clean Artifact Rules

1. Start from a clean worktree.
2. Remove or replace the platform-specific package directory before assembling.
3. Build the Rust shim and Mighty executable in release mode for the same host
   OS that will run the package.
4. Bundle only the executable, native shim library, icon/assets needed at
   runtime, samples/examples, project docs, and platform-specific run
   instructions.
5. Reject build sidecars and obvious foreign-platform native payloads.
6. Smoke-test the packaged executable from inside the assembled package
   directory.
7. Keep generated packages out of commits.

The checked-in package scripts enforce these rules where they can: they refuse a
dirty git worktree, delete the previous platform package directory, reject common
compiler/linker sidecars such as PDBs, import libraries, object files, and logs,
reject obvious foreign-platform native payloads, and validate the native binary
family before writing the archive. Windows checks PE headers in PowerShell.
macOS and Linux require the standard `file` utility so Mach-O and ELF validation
cannot be silently skipped.

The release invariant is one archive, one native binary family:

- Windows packages contain PE files only for native code.
- macOS packages contain Mach-O files only for native code.
- Linux packages contain ELF files only for native code.

If a platform archive cannot be built and smoke-tested on its native OS or a
matching CI runner, do not publish a placeholder archive for that platform.
Publish the verified platforms and leave the missing platform unbuilt until a
native runner can produce and validate it.

Every package must include these human-readable files at the package root unless
the root is an `.app` archive, in which case they live beside the `.app` in the
tarball:

- `RUN.txt` with native instructions for that platform
- `README.md`
- `KEYBINDINGS.md`
- `CHANGELOG.md`
- `BUILDING.md`
- `LICENSE`
- `docs/platform-packaging.md`

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
build byproduct or Unix native library payload is found in the package
directory.

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

Set `MIGHTY_MTY=/path/to/mty` if the Mighty compiler is not on `PATH`. Set
`CLANG=/path/to/clang` if the default `clang` executable is not the intended
linker.

Both scripts:

- refuse to run on the wrong host OS
- refuse to run from a dirty git worktree
- require the `file` utility for native binary validation
- remove the previous platform package directory before assembly
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

The resulting archives should be smoke-tested from the assembled package
directory before upload.

## Verification Commands

These commands are intentionally explicit so a release note can include the
same facts for every published archive: archive size, SHA-256, native binary
family, absence of build sidecars, absence of foreign native payloads, and a
packaged launch.

Windows PowerShell:

```powershell
Get-Item dist\mighty-ide-v0.3.0-win64.zip |
  Select-Object FullName,Length,LastWriteTime
Get-FileHash dist\mighty-ide-v0.3.0-win64.zip -Algorithm SHA256

Get-ChildItem dist\mighty-ide-win64 -Recurse -File |
  Where-Object { $_.Extension -in @(
    '.pdb','.lib','.exp','.ilk','.obj','.o','.rlib','.log','.dylib','.so'
  ) } |
  Select-Object FullName,Length

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
find dist/mighty-ide-macos -type f \( \
  -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o \
  -name '*.obj' -o -name '*.o' -o -name '*.rlib' -o -name '*.log' -o \
  -name '*.exe' -o -name '*.dll' -o -name '*.so' \)
"dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/mighty-ide"
```

Linux:

```sh
sha256sum dist/mighty-ide-v0.3.0-linux-x64.tar.gz
ls -lh dist/mighty-ide-v0.3.0-linux-x64.tar.gz
file dist/mighty-ide-linux-x64/mighty-ide \
  dist/mighty-ide-linux-x64/libmighty_ui_sys.so
find dist/mighty-ide-linux-x64 -type f \( \
  -name '*.pdb' -o -name '*.lib' -o -name '*.exp' -o -name '*.ilk' -o \
  -name '*.obj' -o -name '*.o' -o -name '*.rlib' -o -name '*.log' -o \
  -name '*.exe' -o -name '*.dll' -o -name '*.dylib' \)
(cd dist/mighty-ide-linux-x64 && ./mighty-ide)
```

An empty `find` result is expected for the sidecar/foreign-payload scans. If
any path is printed, fix the package before publishing it.
