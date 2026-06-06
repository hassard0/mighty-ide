# Release Verification

Use this checklist for every archive that is uploaded. A platform is releasable
only when its own package script completed on the native OS or a matching CI
runner, the finished archive passed the archive-level clean-binary scan, and
the packaged app launched from inside the assembled package directory or app
bundle.

Build publishable archives only after the final source and release docs are
committed. If README, changelog, build notes, package scripts, or release docs
change after packaging, discard the old artifact result and rebuild before
upload.

## Evidence Template

```text
Platform:
Archive:
Archive size:
SHA-256:
Package script:
Native host or runner:
Native payloads:
Sidecar scan:
Foreign-payload scan:
PACKAGE-MANIFEST.txt:
Manifest/source commit match:
Packaged launch:
Release decision:
```

## Required Checks

- The package script ran from a clean committed tree.
- The script removed the previous same-version archive before building.
- `PACKAGE-MANIFEST.txt` exists in the package root.
- The manifest records platform, version, source commit, generated timestamp,
  archive name, native payload hashes, native payload sizes, and clean checks.
- The manifest source commit matches the committed README, changelog, build
  notes, package scripts, and release docs being handed off.
- The package directory contains no compiler/linker sidecars:
  `.pdb`, `.lib`, `.exp`, `.ilk`, `.obj`, `.o`, `.a`, `.rlib`, `.log`,
  `.debug`, `.map`, or `.dSYM`.
- The finished archive contains no compiler/linker sidecars.
- The package directory and finished archive contain no foreign-platform native
  payloads.
- The packaged executable launched from inside the assembled package directory
  or app bundle.

## Platform Payloads

| Platform | Expected native payloads | Foreign native payloads rejected |
|----------|--------------------------|----------------------------------|
| Windows x64 | PE `mighty-ide.exe`; PE `mighty_ui_sys.dll` | `.dylib`, `.so` |
| macOS | Mach-O `Mighty IDE.app/Contents/MacOS/mighty-ide`; Mach-O `libmighty_ui_sys.dylib` | `.exe`, `.dll`, `.so` |
| Linux x64 | ELF `mighty-ide`; ELF `libmighty_ui_sys.so` | `.exe`, `.dll`, `.dylib` |

## Windows-Hosted Stop Pass

This Windows checkout can fully verify only the Windows x64 package. macOS and
Linux are releasable only after their native scripts complete and launch on
matching infrastructure.

Use this final status unless native macOS and Linux runners were actually used
during the same pass:

```text
Windows x64: publish after .\package-win.ps1 rebuilds the ZIP from the clean
committed tree and the packaged executable launches from
dist\mighty-ide-win64.

macOS: unbuilt - native macOS runner unavailable for this pass.

Linux x64: unbuilt - native Linux runner unavailable for this pass.
```

Script review or wrong-host refusal checks are source-readiness evidence, not
clean-binary evidence for macOS or Linux.

Use [`release-readiness.md`](release-readiness.md) as the concise
source-versus-binary readiness checklist when preparing the final handoff.

For a stop pass from Windows, include the reviewed macOS and Linux package
commands as script-ready only. Leave their release decision as `unbuilt` until
the native package script, manifest, archive scan, hash, and packaged launch
are produced on that platform from the final source commit.
