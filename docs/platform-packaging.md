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
   runtime, samples/examples, and run instructions.
5. Smoke-test the packaged executable from inside the assembled package
   directory.
6. Keep generated packages out of commits.

The checked-in package scripts enforce these rules where they can: they refuse a
dirty git worktree, delete the previous platform package directory, reject common
compiler/linker sidecars such as PDBs, import libraries, object files, and logs,
and validate the native binary family before writing the archive.

## Platform Matrix

| Platform | Native binary shape | Current repo support |
|----------|---------------------|----------------------|
| Windows x64 | PE `mighty-ide.exe` plus PE `mighty_ui_sys.dll` | `package-win.ps1` creates `dist/mighty-ide-win64/` and `dist/mighty-ide-v0.3.0-win64.zip` |
| macOS | `.app` archive containing Mach-O executable plus `.dylib` dependencies | `package-macos.sh` creates `dist/mighty-ide-macos/` and `dist/mighty-ide-v0.3.0-macos.tar.gz` on macOS |
| Linux x64 | ELF executable plus ELF `.so` dependencies in a tarball directory | `package-linux.sh` creates `dist/mighty-ide-linux-x64/` and `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` on Linux |

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
build byproduct is found in the package directory.

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
- remove the previous platform package directory before assembly
- build `mighty-ui-sys` and `mty-rt-abi` in release mode
- generate a temporary host-specific `mighty.toml` and restore the checked-in
  Windows manifest on exit
- copy only the executable, native shim library, samples/examples, and run docs
- strip symbols when the platform `strip` tool is available
- verify the staged native binaries with `file` when available
- write a platform tarball under `dist/`

The resulting archives should be smoke-tested from the assembled package
directory before upload.
