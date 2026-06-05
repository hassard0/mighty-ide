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

## Final Pass Evidence

Use this snapshot shape for the final handoff. Only write `publish` for evidence
collected on that platform's native host or matching CI runner.

```text
Platform: Windows x64
Archive: dist\mighty-ide-v0.3.0-win64.zip
Package script: .\package-win.ps1
Native payloads: PE mighty-ide.exe; PE mighty_ui_sys.dll
Sidecar scan: package directory and ZIP passed
Foreign-payload scan: package directory and ZIP passed
Packaged launch: launched from dist\mighty-ide-win64
Release decision: publish

Platform: macOS
Archive: dist/mighty-ide-v0.3.0-macos.tar.gz
Release decision: unbuilt - native macOS runner unavailable

Platform: Linux x64
Archive: dist/mighty-ide-v0.3.0-linux-x64.tar.gz
Release decision: unbuilt - native Linux runner unavailable
```

## Final Handoff Status

Use this table at the end of the pass. Fill in only evidence gathered on the
matching native host or runner.

| Platform | Required archive | Clean-binary requirement | Decision rule |
|----------|------------------|--------------------------|---------------|
| Windows x64 | `dist\mighty-ide-v0.3.0-win64.zip` | PE executable and PE shim, no sidecars, no `.dylib`/`.so` payloads | `publish` only after `package-win.ps1` and packaged launch pass on Windows |
| macOS | `dist/mighty-ide-v0.3.0-macos.tar.gz` | Mach-O app executable and Mach-O `.dylib`, no sidecars, no `.exe`/`.dll`/`.so` payloads | `publish` only after `package-macos.sh` and packaged launch pass on macOS |
| Linux x64 | `dist/mighty-ide-v0.3.0-linux-x64.tar.gz` | ELF executable and ELF `.so`, no sidecars, no `.exe`/`.dll`/`.dylib` payloads | `publish` only after `package-linux.sh` and packaged launch pass on Linux |

If a platform's native host is not available for the pass, leave the archive
absent and record `unbuilt`. A source-level script review is useful, but it is
not a clean-binary verification for that platform.

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

## Stop-Pass Handoff

When finalizing from this Windows checkout, stop with this status unless native
macOS and Linux runners were actually used during the same pass:

```text
Windows x64: publish only after .\package-win.ps1 rebuilds the ZIP from the
clean committed tree and the packaged executable launches from
dist\mighty-ide-win64.

macOS: unbuilt - native macOS runner unavailable in this Windows pass.

Linux x64: unbuilt - native Linux runner unavailable in this Windows pass.
```

Source-level review and shell syntax checks keep the macOS and Linux package
scripts ready, but they are not clean-binary evidence for those platforms.
