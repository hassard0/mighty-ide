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

This handoff is the stopping point for the current pass. The only local binary
that can be made clean from this host is the Windows PE archive. macOS and
Linux must be reported as `unbuilt` unless native runners completed during this
same pass; script checks from Windows are readiness evidence, not binary
evidence.

## Publish Decisions

| Platform | Decision from this Windows pass | Required native binary evidence |
|----------|---------------------------------|---------------------------------|
| Windows x64 | `publish` after `.\package-win.ps1` succeeds and the packaged executable launches from `dist\mighty-ide-win64` | PE `mighty-ide.exe`, PE `mighty_ui_sys.dll`, clean package tree, clean ZIP, manifest source/hash/size rows |
| macOS | `unbuilt` unless a macOS host ran `./package-macos.sh` during this pass | Mach-O app executable, Mach-O `libmighty_ui_sys.dylib`, clean package tree, clean tarball, packaged app launch |
| Linux x64 | `unbuilt` unless a Linux host ran `./package-linux.sh` during this pass | ELF `mighty-ide`, ELF `libmighty_ui_sys.so`, clean package tree, clean tarball, packaged launch |

Do not publish renamed archives, placeholder archives, or native payloads copied
from another operating system.

## Final Windows Steps

Run these from a clean committed tree:

```powershell
.\package-win.ps1 -Mty C:\path\to\mty.exe
Get-FileHash dist\mighty-ide-v0.3.0-win64.zip -Algorithm SHA256
Get-Item dist\mighty-ide-v0.3.0-win64.zip | Select-Object FullName,Length
Start-Process -FilePath "dist\mighty-ide-win64\mighty-ide.exe" `
  -WorkingDirectory "dist\mighty-ide-win64"
```

If `mty` is not on PATH, pass the compiler explicitly:

```powershell
.\package-win.ps1 -Mty C:\path\to\mty.exe
```

The handoff package must be generated after the README and release docs are
committed. The generated `PACKAGE-MANIFEST.txt` source commit must match the
commit reported in the final response.

The selected compiler must report v0.47.0 or newer. A missing or stale compiler
is a toolchain failure before packaging, not a clean-binary result.

Final artifact fields to record after the package run:

```text
Source commit:
Windows archive: dist\mighty-ide-v0.3.0-win64.zip
Windows archive size:
Windows SHA-256:
Windows native payloads: PE mighty-ide.exe; PE mighty_ui_sys.dll
Windows clean-binary scans: package directory and ZIP passed
Windows packaged launch: launched from dist\mighty-ide-win64
macOS decision: unbuilt - native macOS runner unavailable for this pass
Linux decision: unbuilt - native Linux runner unavailable for this pass
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
If this Windows host has only the WSL launcher and no installed Linux
distribution, even those shell-readiness checks are unavailable here. Keep the
Linux decision `unbuilt - native runner unavailable for this pass` until a real
Linux environment completes `./package-linux.sh` and launches the packaged app.

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

After the final source, tests, README, changelog, and release docs are
committed, rebuild the Windows package from that clean commit, confirm
`PACKAGE-MANIFEST.txt` records that source commit, record the Windows archive
hash and size, confirm the packaged Windows executable launched from
`dist\mighty-ide-win64`, and stop.

This pass is complete when those artifact-scoped fields are reported. Do not
fold more IDE implementation, README edits, docs edits, or package-script
changes into the same pass after the package run; any source change after that
point invalidates the recorded archive and requires a new clean commit plus a
new native package run.

This final pass has no follow-on feature scope. The release boundary is the
committed source and documentation plus the ignored platform artifacts generated
from that commit. If another source edit is needed after packaging, the package
evidence is stale until the affected platform package is rebuilt from the new
commit.

The publish matrix for a Windows-hosted pass is intentionally narrow:

- Windows x64 may be `publish` only after the PowerShell packager succeeds, the
  ZIP scan passes, `PACKAGE-MANIFEST.txt` exists, and the packaged executable
  launches from `dist\mighty-ide-win64`.
- macOS stays `unbuilt` unless a native macOS host or matching CI runner runs
  `./package-macos.sh` and launches the app during this same pass.
- Linux x64 stays `unbuilt` unless a native Linux host or matching CI runner
  runs `./package-linux.sh` and launches the executable during this same pass.

The final source-controlled docs should remain reusable release rules and
templates. Generated archive hashes and sizes belong to the post-commit package
manifest, final handoff response, and external upload note; committing them into
this file would change the source hash and require a new package run.
For this stop pass, README, changelog, and release docs are committed first;
the Windows ZIP evidence is generated only from that committed tree.
The manifest must include the source commit, generated UTC timestamp, native
payload hash and size rows, archive path, and clean-binary checks. A package
that lacks any of those fields is incomplete even if the executable launches.
The final handoff should publish only artifacts whose bundled manifest names
the committed source hash for this pass. If a native package cannot be rebuilt
after that final source commit, record the platform as `unbuilt` instead of
treating an older archive as clean evidence.
The final package must be generated after the source commit that defines this
handoff. If any source file changes after the package is generated, the artifact
no longer matches the source handoff and must be rebuilt before it is published.
Use `docs\release-evidence.md` as the concise upload record.
Use `docs\binary-release-status.md` as the bundled clean-binary status summary.
Record macOS and Linux as `unbuilt` unless their native package runs completed
during this same pass. If only syntax and wrong-host checks ran for those
scripts, mention them as script-readiness checks rather than clean-binary
evidence.

This stop condition is intentionally stronger than a clean source tree. The
Windows ZIP must be regenerated after the final source commit so the packaged
docs, manifest, and native PE payloads all correspond to the same source hash.
macOS and Linux require the same source-to-package correspondence on their own
native runners before either platform can be called clean.

The final response for a Windows-hosted pass should report the committed source
hash, the Windows ZIP size and SHA-256, the package-script checks that passed,
and the explicit macOS/Linux `unbuilt` status. Do not do additional feature or
polish work after that report.

Use the final response, the generated Windows `PACKAGE-MANIFEST.txt`, and the
Windows ZIP hash/size as the artifact-scoped evidence for this pass. Do not
commit those generated values back into the reusable docs after the package run;
doing so would create a new source commit and require a new package.

Stopping here is part of the release contract. Further IDE polish belongs in a
new pass after the verified package handoff, not in the same finalization pass.

The clean-binary claim for this Windows pass is limited to the generated
Windows archive: PE executable, PE shim, no compiler/linker sidecars, no
`.dylib` or `.so` payloads, manifest written, and packaged launch completed.
The macOS and Linux scripts remain ready to build their native archives, but
their binaries are not clean until a macOS or Linux host produces and launches
those packages.

Before reporting this final pass, confirm that any macOS or Linux archive absent
from native-runner evidence is treated as unavailable, not reused. A checked
script, copied file, cross-host inspection, or failed WSL setup is not a clean
binary for those platforms.

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
