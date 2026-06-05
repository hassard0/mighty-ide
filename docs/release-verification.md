# Release Verification

Use this record for every archive that is uploaded. A platform is releasable
only when its own package script completed on that native OS or a matching CI
runner, the finished archive passed the archive-level clean-binary scan, and
the packaged app launched from inside the assembled package directory or app
bundle.

Do not derive one platform from another platform's package. Windows requires PE
payloads, macOS requires Mach-O payloads, and Linux requires ELF payloads.

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
Packaged launch:
Release decision:
```

## Current Pass Status

This repository is currently being finalized from a Windows checkout. That means
the Windows x64 archive can be rebuilt and fully verified here, including PE
header checks, sidecar/foreign-payload scans, `PACKAGE-MANIFEST.txt`, archive
hash/size capture, and a packaged launch from `dist\mighty-ide-win64`.

macOS and Linux are not considered clean merely because their scripts exist or
their host gates can be inspected from Windows. They become releasable only
after `./package-macos.sh` or `./package-linux.sh` completes on the matching
native OS or CI runner and the packaged app launches from the assembled package
directory or app bundle.

Record unavailable platforms as:

```text
Platform: macOS
Release decision: unbuilt - native macOS runner unavailable

Platform: Linux x64
Release decision: unbuilt - native Linux runner unavailable
```

## Required Checks

For every uploaded archive:

- The package script ran from a clean committed tree.
- `PACKAGE-MANIFEST.txt` exists in the package root and names the expected
  platform, version, native payload hashes, native payload sizes, and
  clean-binary checks.
- The package directory contains no compiler/linker sidecars:
  `.pdb`, `.lib`, `.exp`, `.ilk`, `.obj`, `.o`, `.a`, `.rlib`, `.log`,
  `.debug`, `.map`, or `.dSYM`.
- The finished archive contains no compiler/linker sidecars.
- The package directory and finished archive contain no foreign-platform native
  payloads.
- The packaged executable was launched from inside the assembled package
  directory or app bundle.

## Platform Payloads

| Platform | Expected native payloads | Foreign native payloads rejected |
|----------|--------------------------|----------------------------------|
| Windows x64 | PE `mighty-ide.exe`, PE `mighty_ui_sys.dll` | `.dylib`, `.so` |
| macOS | Mach-O `Mighty IDE.app/Contents/MacOS/mighty-ide`, Mach-O `libmighty_ui_sys.dylib` | `.exe`, `.dll`, `.so` |
| Linux x64 | ELF `mighty-ide`, ELF `libmighty_ui_sys.so` | `.exe`, `.dll`, `.dylib` |

## Current Host Rule

This Windows checkout can fully verify only the Windows x64 package. macOS and
Linux can be maintained from this checkout by reviewing scripts and verifying
host gates, but their binaries are clean only after native macOS or Linux
package runs complete and launch successfully on those platforms.

If a native host or matching CI runner is unavailable, record that platform as
`unbuilt`. Do not publish a placeholder archive.
