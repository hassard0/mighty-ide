# Binary Release Status

This file defines the release decision for each native package. It is bundled
with release archives so the package carries the same clean-binary rules as the
source checkout.

## Clean Binary Definition

A platform binary is clean only when all of these are true on that platform's
native OS or a matching CI runner:

- the package script ran from a clean committed source tree
- the staged package directory and same-version archive were rebuilt
- native payloads match the platform binary family
- compiler and linker sidecars are absent from the package and archive
- foreign-platform native payloads are absent from the package and archive
- `PACKAGE-MANIFEST.txt` records source commit, generated time, archive name,
  native payload hashes, native payload sizes, and completed clean checks
- the packaged executable launched from inside the assembled package directory
  or app bundle

A Windows package proves only Windows PE payloads. A macOS package proves only
Mach-O payloads. A Linux package proves only ELF payloads.

## Decision Values

Use only these release states:

- `publish`: native package script completed, package and archive scans passed,
  manifest exists, and the packaged app launched on the matching platform.
- `hold`: a native package exists, but a required check failed or has not been
  recorded.
- `unbuilt`: no native host or matching CI runner produced the archive for this
  pass.

## Platform Matrix

| Platform | Required payloads | Publish rule |
|----------|-------------------|--------------|
| Windows x64 | PE `mighty-ide.exe`; PE `mighty_ui_sys.dll` | `.\package-win.ps1` passed on Windows and the packaged app launched from `dist\mighty-ide-win64` |
| macOS | Mach-O app executable; Mach-O `libmighty_ui_sys.dylib` | `./package-macos.sh` passed on macOS and the packaged app launched from the app bundle |
| Linux x64 | ELF `mighty-ide`; ELF `libmighty_ui_sys.so` | `./package-linux.sh` passed on Linux and the packaged app launched from the package directory |

## Windows-Hosted Stop Pass

This checkout is being finalized from Windows. The Windows x64 package can be
rebuilt, scanned, hashed, and smoke-tested here. macOS and Linux must stay
`unbuilt - native runner unavailable for this pass` unless their native package
scripts completed and launched during the same pass on matching infrastructure.

Script review or wrong-host refusal checks are useful source-readiness checks,
but they are not clean-binary evidence for macOS or Linux.

## Final Response Fields

After the final source commit and Windows package run, report:

```text
Source commit:
Windows archive:
Windows archive size:
Windows SHA-256:
Windows package checks:
Windows packaged launch:
macOS decision:
Linux decision:
```

Do not commit generated archive hashes, timestamps, payload hashes, or launch
results into this reusable file. Those values belong to `PACKAGE-MANIFEST.txt`,
the external upload note, and the final handoff response for the package run.
