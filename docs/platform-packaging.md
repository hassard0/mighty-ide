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

## Platform Matrix

| Platform | Native binary shape | Current repo support |
|----------|---------------------|----------------------|
| Windows x64 | `mighty-ide.exe` plus `mighty_ui_sys.dll` | `package-win.ps1` creates `dist/mighty-ide-win64/` and `dist/mighty-ide-v0.3.0-win64.zip` |
| macOS | `.app` or archive containing a Mach-O executable plus `.dylib` dependencies | Build on macOS; package script not yet checked in |
| Linux | executable plus `.so` dependencies, usually tarball or AppImage-style directory | Build on Linux; package script not yet checked in |

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

Smoke-test by launching:

```powershell
Start-Process -FilePath "dist\mighty-ide-win64\mighty-ide.exe" `
  -WorkingDirectory "dist\mighty-ide-win64"
```

## macOS and Linux

macOS and Linux packages must be produced on native hosts or matching CI
runners. The current Windows package cannot be converted into a clean macOS or
Linux binary because the executable and shim are native artifacts.

Until checked-in package scripts exist for those platforms, the release status
for macOS and Linux is source-build ready, binary package pending.
