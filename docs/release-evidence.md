# Release Evidence

Use this file as the source-controlled template for release upload notes. Fill
one platform block only after that platform's native package script completed
on its native OS or a matching CI runner and the packaged executable launched
from inside the assembled package directory or app bundle.

Do not commit generated archive hashes, timestamps, payload hashes, or launch
results back into this template after packaging. Those generated values belong
to `PACKAGE-MANIFEST.txt`, the external upload note, and the final handoff
response.

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
explicitly instead of publishing a placeholder archive:

```text
Platform:
Release decision: unbuilt - native runner unavailable for this pass
```

For a Windows-hosted stop pass, Windows x64 is the only platform that can be
locally marked `publish`. macOS and Linux stay `unbuilt` unless their own
native package scripts completed and launched during the same pass.

## Final Upload Note Template

Use this shape for the final upload note or handoff response. Values come from
the post-commit package run:

```text
Source commit:
Windows archive: dist\mighty-ide-v0.3.0-win64.zip
Windows archive size:
Windows SHA-256:
Windows package checks: PE headers verified; package directory and ZIP sidecar
  scans passed; foreign payload scans passed; PACKAGE-MANIFEST.txt generated
Windows packaged launch: launched from dist\mighty-ide-win64
macOS decision: unbuilt - native macOS runner unavailable for this pass
Linux decision: unbuilt - native Linux runner unavailable for this pass
```

If macOS or Linux native runners complete during the same pass, replace the
matching `unbuilt` line with native archive, hash, manifest, scan, and launch
evidence from that runner.
