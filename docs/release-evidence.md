# Release Evidence

Use this file as the final record for a release upload. Fill one block per
platform only after that platform's package script has completed on its native
OS or a matching CI runner and the packaged executable has launched from inside
the assembled package directory or app bundle. The bundled platform status
summary lives in [`binary-release-status.md`](binary-release-status.md).

Do not mark macOS or Linux as clean from a Windows build. Do not mark Windows
as clean from a macOS or Linux build. A clean binary is one produced, scanned,
and launched on its own platform.

## Windows x64

```text
Platform: Windows x64
Archive: dist\mighty-ide-v0.3.0-win64.zip
Archive size:
SHA-256:
Package script: .\package-win.ps1
Native host or runner:
Native payloads: PE mighty-ide.exe; PE mighty_ui_sys.dll
Sidecar scan: package directory and ZIP passed
Foreign-payload scan: package directory and ZIP passed
PACKAGE-MANIFEST.txt:
Source commit:
Manifest/source commit match:
Packaged launch:
Release decision:
```

## macOS

```text
Platform: macOS
Archive: dist/mighty-ide-v0.3.0-macos.tar.gz
Archive size:
SHA-256:
Package script: ./package-macos.sh
Native host or runner:
Native payloads: Mach-O Mighty IDE.app/Contents/MacOS/mighty-ide; Mach-O libmighty_ui_sys.dylib
Sidecar scan: package directory and tarball passed
Foreign-payload scan: package directory and tarball passed
PACKAGE-MANIFEST.txt:
Source commit:
Manifest/source commit match:
Packaged launch:
Release decision:
```

## Linux x64

```text
Platform: Linux x64
Archive: dist/mighty-ide-v0.3.0-linux-x64.tar.gz
Archive size:
SHA-256:
Package script: ./package-linux.sh
Native host or runner:
Native payloads: ELF mighty-ide; ELF libmighty_ui_sys.so
Sidecar scan: package directory and tarball passed
Foreign-payload scan: package directory and tarball passed
PACKAGE-MANIFEST.txt:
Source commit:
Manifest/source commit match:
Packaged launch:
Release decision:
```

## Unbuilt Platform Record

If a native runner was unavailable for a platform during the pass, record it
explicitly instead of publishing a placeholder archive.

```text
Platform:
Release decision: unbuilt - native runner unavailable for this pass
```

For a Windows-hosted stop pass, Windows x64 is the only platform that can be
locally marked `publish`. macOS and Linux must remain `unbuilt` unless their
own native package scripts completed and launched during the same pass. Script
readiness, copied artifacts, or cross-host archive inspection are not clean
binary evidence for those platforms.

## Windows-Hosted Stop Pass

Use this block for the final pass from this checkout. Record the Windows
archive size and SHA-256 from the generated ZIP after `.\package-win.ps1`
succeeds, in the external upload note or release handoff. Do not edit the source
tree after that package run unless the package is rebuilt from the new clean
commit.

The committed evidence file is a template, not the artifact record of generated
hashes. The generated record for this pass is the combination of the committed
source hash, `dist\mighty-ide-win64\PACKAGE-MANIFEST.txt`, the ZIP size,
the ZIP SHA-256, and the packaged-launch result. macOS and Linux require their
own native records before they can move out of `unbuilt`.

For this stop pass, the source-controlled evidence remains a reusable template.
The final generated values belong beside the uploaded artifact and in the final
handoff response because inserting them here would change the source commit and
invalidate the just-recorded archive hash.

The source-controlled copy of this file deliberately stays a reusable template.
For the actual release upload, copy the generated values from
`dist\mighty-ide-win64\PACKAGE-MANIFEST.txt`, `Get-Item`, and `Get-FileHash`
into the upload note. Keeping generated archive hashes out of the committed
template avoids a self-referential package where updating the evidence changes
the archive that the evidence describes.

If this file is bundled inside the archive, keep the exact archive hash and
size in the external release note or upload record for that run. Do not chase a
self-referential source edit where changing the packaged evidence file changes
the archive hash that the evidence file is trying to record.

```text
Platform: Windows x64
Archive: dist\mighty-ide-v0.3.0-win64.zip
Archive size:
SHA-256:
Package script: .\package-win.ps1
Native host or runner: Windows checkout
Native payloads: PE mighty-ide.exe; PE mighty_ui_sys.dll
Sidecar scan: package directory and ZIP passed
Foreign-payload scan: package directory and ZIP passed
PACKAGE-MANIFEST.txt: generated in dist\mighty-ide-win64 with source commit
Manifest/source commit match: manifest source commit matches final docs commit
Packaged launch: launched from dist\mighty-ide-win64
Release decision: publish

Platform: macOS
Archive: dist/mighty-ide-v0.3.0-macos.tar.gz
Release decision: unbuilt - native macOS runner unavailable for this pass
Script readiness: syntax and wrong-host refusal may be checked from Windows,
but that is not clean-binary evidence

Platform: Linux x64
Archive: dist/mighty-ide-v0.3.0-linux-x64.tar.gz
Release decision: unbuilt - native Linux runner unavailable for this pass
Script readiness: syntax and wrong-host refusal may be checked from Windows,
but that is not clean-binary evidence
```

If WSL or another Linux environment is unavailable, record Linux as `unbuilt`.
If WSL is available but lacks the Linux Mighty compiler, Rust toolchain, `file`
utility, or the packaged launch cannot complete inside that Linux environment,
record Linux as `hold` only when a native Linux package exists; otherwise leave
it `unbuilt`.

## Final Windows Pass Summary

For a Windows-only finalization pass, the source-controlled record should stop
at the release rules, scripts, and templates. The generated record for the
actual upload belongs with the release artifact:

- Windows x64: publish only after the clean committed tree is packaged with
  `.\package-win.ps1`, the ZIP scan passes, `PACKAGE-MANIFEST.txt` is present,
  and the packaged executable launches from `dist\mighty-ide-win64`.
- macOS: unbuilt unless `./package-macos.sh` completed and launched on native
  macOS or a matching CI runner during this pass.
- Linux x64: unbuilt unless `./package-linux.sh` completed and launched on
  native Linux or a matching CI runner during this pass.
