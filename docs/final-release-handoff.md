# Final Release Handoff

This handoff is intentionally strict: a platform is clean only when its own
native package script built the package, scanned the assembled package and final
archive, wrote `PACKAGE-MANIFEST.txt`, and the packaged app launched from inside
the assembled package directory or app bundle.

## Current Host

This pass is running from a Windows checkout.

That means Windows x64 can be rebuilt and fully verified here. macOS and Linux
can have their scripts and docs reviewed from here, but their binaries are not
clean until native macOS and Linux hosts or matching CI runners run their own
package scripts and smoke tests.

## Publish Decisions

| Platform | Decision from this Windows pass | Required native binary evidence |
|----------|---------------------------------|---------------------------------|
| Windows x64 | `publish` after `.\package-win.ps1` succeeds and the packaged executable launches from `dist\mighty-ide-win64` | PE `mighty-ide.exe`, PE `mighty_ui_sys.dll`, clean package tree, clean ZIP, manifest hash/size rows |
| macOS | `unbuilt` unless a macOS host ran `./package-macos.sh` during this pass | Mach-O app executable, Mach-O `libmighty_ui_sys.dylib`, clean package tree, clean tarball, packaged app launch |
| Linux x64 | `unbuilt` unless a Linux host ran `./package-linux.sh` during this pass | ELF `mighty-ide`, ELF `libmighty_ui_sys.so`, clean package tree, clean tarball, packaged launch |

Do not publish renamed archives, placeholder archives, or native payloads copied
from another operating system.

## Final Windows Steps

Run these from a clean committed tree:

```powershell
.\package-win.ps1
Get-FileHash dist\mighty-ide-v0.3.0-win64.zip -Algorithm SHA256
Get-Item dist\mighty-ide-v0.3.0-win64.zip | Select-Object FullName,Length
Start-Process -FilePath "dist\mighty-ide-win64\mighty-ide.exe" `
  -WorkingDirectory "dist\mighty-ide-win64"
```

The package script verifies PE headers for both native payloads, rejects
compiler/linker sidecars, rejects `.dylib` and `.so` payloads, scans the
finished ZIP, and writes `dist\mighty-ide-win64\PACKAGE-MANIFEST.txt`. It
removes the prior `dist\mighty-ide-win64` directory and same-version ZIP before
building, so any ZIP present after a successful run came from that run.

## Script Readiness Checks

When macOS or Linux native runners are unavailable, this Windows pass can still
keep those release paths ready by checking the shell scripts for syntax and
confirming they refuse the wrong host OS. Those checks are source maintenance,
not binary evidence. Record macOS and Linux as `unbuilt` until their native
scripts build, scan, and launch real Mach-O or ELF packages.

## Native macOS Steps

Run only on macOS or a matching macOS CI runner:

```sh
./package-macos.sh
shasum -a 256 dist/mighty-ide-v0.3.0-macos.tar.gz
file "dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/mighty-ide" \
  "dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/libmighty_ui_sys.dylib"
"dist/mighty-ide-macos/Mighty IDE.app/Contents/MacOS/mighty-ide"
```

The macOS package is publishable only after the script verifies Mach-O payloads,
the archive scan passes, and the app bundle launches on macOS.

## Native Linux Steps

Run only on Linux or a matching Linux CI runner:

```sh
./package-linux.sh
sha256sum dist/mighty-ide-v0.3.0-linux-x64.tar.gz
file dist/mighty-ide-linux-x64/mighty-ide \
  dist/mighty-ide-linux-x64/libmighty_ui_sys.so
(cd dist/mighty-ide-linux-x64 && ./mighty-ide)
```

The Linux package is publishable only after the script verifies ELF payloads,
the archive scan passes, and the executable launches on Linux.

## Stop Condition

After the README and release docs are committed, rebuild the Windows package
from that clean commit, record the Windows archive hash and size, confirm the
packaged Windows executable launched from `dist\mighty-ide-win64`, and stop.
Use `docs\release-evidence.md` as the concise upload record.
Record macOS and Linux as `unbuilt` unless their native package runs completed
during this same pass. If only syntax and wrong-host checks ran for those
scripts, mention them as script-readiness checks rather than clean-binary
evidence.

The final response for a Windows-hosted pass should report the committed source
hash, the Windows ZIP size and SHA-256, the package-script checks that passed,
and the explicit macOS/Linux `unbuilt` status. Do not do additional feature or
polish work after that report.

Stopping here is part of the release contract. Further IDE polish belongs in a
new pass after the verified package handoff, not in the same finalization pass.

The clean-binary claim for this Windows pass is limited to the generated
Windows archive: PE executable, PE shim, no compiler/linker sidecars, no
`.dylib` or `.so` payloads, manifest written, and packaged launch completed.
The macOS and Linux scripts remain ready to build their native archives, but
their binaries are not clean until a macOS or Linux host produces and launches
those packages.

## Final Pass Record

For this Windows-hosted pass:

- Windows x64 is the only platform that can receive local clean-binary evidence.
  Its package must be rebuilt with `.\package-win.ps1` from a clean committed
  tree, scanned by the script, launched from `dist\mighty-ide-win64`, and
  recorded with the ZIP size and SHA-256 hash.
- macOS remains `unbuilt` unless `./package-macos.sh` completes on a native
  macOS host or matching CI runner during this pass.
- Linux x64 remains `unbuilt` unless `./package-linux.sh` completes on a native
  Linux host or matching CI runner during this pass.

Do not continue implementation work after recording this evidence. If the source
changes after the package is built, rebuild the package from the new clean
commit before publishing.
