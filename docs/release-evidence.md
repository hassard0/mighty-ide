# Release Evidence

Use this file as the final record for a release upload. Fill one block per
platform only after that platform's package script has completed on its native
OS or a matching CI runner and the packaged executable has launched from inside
the assembled package directory or app bundle.

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
