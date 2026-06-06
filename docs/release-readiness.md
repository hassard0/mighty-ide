# Release Readiness

This checklist is the source-controlled handoff for a final release pass. It
separates source readiness from binary readiness so a platform is not marked
clean until its own native artifact is built, scanned, and launched.

## Source Readiness

Before any package script runs, commit all tracked release inputs:

- `README.md`
- `BUILDING.md`
- `KEYBINDINGS.md`
- `CHANGELOG.md`
- `LICENSE`
- `package-win.ps1`
- `package-macos.sh`
- `package-linux.sh`
- `docs/platform-packaging.md`
- `docs/release-verification.md`
- `docs/release-evidence.md`
- `docs/binary-release-status.md`
- `docs/release-readiness.md`
- `docs/final-release-handoff.md`

Run package scripts only from a clean worktree. If any tracked file changes
after packaging, discard the generated package for that platform and rebuild it
from the new commit.

## Binary Readiness

A platform binary is ready only when all of these are true:

- The package script ran on the matching OS or matching CI runner.
- The script removed the previous same-version package directory and archive.
- The native executable and native shim match the expected binary family.
- The staged package contains no compiler or linker sidecars.
- The final archive contains no compiler or linker sidecars.
- The staged package and archive contain no foreign-platform native payloads.
- `PACKAGE-MANIFEST.txt` names the same source commit being released.
- Archive size and SHA-256 were recorded after packaging.
- The packaged app launched from inside the assembled package directory or app
  bundle.

## Platform Decisions

Use only these final states:

| State | Meaning |
|-------|---------|
| `publish` | Native build, archive scan, manifest/source match, hash, and packaged launch all passed. |
| `hold` | A native artifact exists, but one required check failed or is missing. |
| `unbuilt` | No matching native host or CI runner produced that platform archive for this pass. |

From a Windows-only pass, Windows x64 can reach `publish` after
`.\package-win.ps1` and packaged launch succeed. macOS and Linux remain
`unbuilt` until native runners produce Mach-O and ELF packages from the same
source commit.

## Expected Artifacts

| Platform | Command | Archive | Native payloads |
|----------|---------|---------|-----------------|
| Windows x64 | `.\package-win.ps1` | `dist\mighty-ide-v0.3.0-win64.zip` | PE `mighty-ide.exe`, PE `mighty_ui_sys.dll` |
| macOS | `./package-macos.sh` on macOS | `dist/mighty-ide-v0.3.0-macos.tar.gz` | Mach-O app executable, Mach-O `libmighty_ui_sys.dylib` |
| Linux x64 | `./package-linux.sh` on Linux | `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` | ELF `mighty-ide`, ELF `libmighty_ui_sys.so` |

Do not rename an archive from another OS, publish a placeholder archive, or
carry forward a package whose manifest names an older commit than the release
docs being handed off.
