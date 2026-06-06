# Final Release Handoff

This is the stop-pass contract for a release finalized from this Windows
checkout.

## Rule

A platform is clean only when its own native package script built the package,
scanned the staged directory and final archive, wrote `PACKAGE-MANIFEST.txt`,
and the packaged app launched from inside the assembled package directory or
app bundle.

Do not publish renamed archives, placeholder archives, copied native payloads,
or a package whose manifest names an older source commit than the README,
build notes, changelog, package scripts, and release docs being handed off.

## Order

1. Finish and commit source, README, changelog, build notes, package scripts,
   and release docs.
   Use [`release-readiness.md`](release-readiness.md) to confirm the source
   inputs and per-platform binary states are separated before packaging.
2. Rebuild each available platform package from that clean commit.
3. Confirm the package manifest names the same source commit.
4. Confirm package and archive scans passed.
5. Launch the packaged app from the assembled package directory or app bundle.
6. Record archive size, SHA-256, package checks, launch result, and platform
   decision.
7. Stop. Any source-controlled change after packaging requires a new package
   run from the new commit.

## Windows Package

Run from a clean committed tree:

```powershell
.\package-win.ps1 -Mty C:\path\to\mty.exe
Get-FileHash dist\mighty-ide-v0.3.0-win64.zip -Algorithm SHA256
Get-Item dist\mighty-ide-v0.3.0-win64.zip | Select-Object FullName,Length
Start-Process -FilePath "dist\mighty-ide-win64\mighty-ide.exe" `
  -WorkingDirectory "dist\mighty-ide-win64" -WindowStyle Hidden
```

The Windows package is publishable only after `package-win.ps1` verifies PE
headers for `mighty-ide.exe` and `mighty_ui_sys.dll`, rejects build sidecars,
rejects `.dylib` and `.so` payloads, scans the ZIP, writes
`PACKAGE-MANIFEST.txt`, and the packaged executable launches from
`dist\mighty-ide-win64`.

## macOS Package

Run only on macOS or a matching macOS CI runner:

```sh
./package-macos.sh
shasum -a 256 dist/mighty-ide-v0.3.0-macos.tar.gz
"dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/mighty-ide"
```

The macOS package is publishable only after the script verifies Mach-O payloads,
the tarball scan passes, the manifest names the final source commit, and the
app launches on macOS.

## Linux Package

Run only on Linux or a matching Linux CI runner:

```sh
./package-linux.sh
sha256sum dist/mighty-ide-v0.3.0-linux-x64.tar.gz
(cd dist/mighty-ide-linux-x64 && ./mighty-ide)
```

The Linux package is publishable only after the script verifies ELF payloads,
the tarball scan passes, the manifest names the final source commit, and the
executable launches on Linux.

## Windows-Hosted Decision

From this Windows host, the only local clean-binary claim can be Windows x64.
macOS and Linux remain:

```text
unbuilt - native runner unavailable for this pass
```

unless their native runners completed the package, scan, manifest, hash, and
launch steps during this same pass.

## Final Handoff Fields

Report these generated values after the post-commit package run:

```text
Source commit:
Windows archive: dist\mighty-ide-v0.3.0-win64.zip
Windows archive size:
Windows SHA-256:
Windows package checks:
Windows packaged launch:
macOS decision: unbuilt - native macOS runner unavailable for this pass
Linux decision: unbuilt - native Linux runner unavailable for this pass
```

Generated hashes, sizes, timestamps, payload hashes, and launch results stay out
of this source-controlled file. They belong to the package manifest, external
upload note, and final response.
