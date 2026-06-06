# Binary Release Status

This file is the concise platform status for a final release pass. It should be
bundled with every release package so the archive carries the same clean-binary
rules as the source checkout.

## Clean Binary Definition

A platform binary is clean only after all of these are true on that platform's
native OS or a matching CI runner:

- the package script ran from a clean committed source tree
- the staged package directory was rebuilt for the current version
- native payloads matched the platform binary family
- compiler and linker sidecars were absent from the staged package
- foreign-platform native payloads were absent from the staged package
- the final archive was scanned for sidecars and foreign native payloads
- `PACKAGE-MANIFEST.txt` was written with the source commit, payload hashes,
  payload sizes, generated timestamp, archive name, and clean-binary checks
- the packaged executable launched from inside the assembled package directory
  or app bundle

Clean-binary evidence is per-platform. A Windows package proves only Windows PE
payloads. A macOS package proves only Mach-O payloads. A Linux package proves
only ELF payloads.

## Platform Decisions

Use only these decision values:

- `publish`: native package script completed, scans passed, manifest exists,
  and the packaged app launched on the matching platform.
- `hold`: a native package exists but one required check failed or has not been
  recorded.
- `unbuilt`: no native host or matching CI runner produced the archive for this
  pass.

## Windows-Hosted Final Pass

This checkout is currently being finalized from Windows. That means:

| Platform | Local decision | Clean-binary evidence required |
|----------|----------------|--------------------------------|
| Windows x64 | `publish` after packaging and launch pass here | PE `mighty-ide.exe`, PE `mighty_ui_sys.dll`, clean staged tree, clean ZIP, manifest with source commit, packaged launch |
| macOS | `unbuilt` unless a macOS runner completed this pass | Mach-O app executable and `.dylib`, clean staged tree, clean tarball, manifest, packaged app launch |
| Linux x64 | `unbuilt` unless a Linux runner completed this pass | ELF executable and `.so`, clean staged tree, clean tarball, manifest, packaged launch |

macOS and Linux package scripts may be syntax-checked and host-gate-checked from
Windows, but those checks are script readiness only. They do not create clean
Mach-O or ELF binaries.

This is the final stop-pass contract for a Windows-hosted handoff: after the
final source, tests, README, changelog, and release docs are committed, rebuild
only the Windows archive here, record its generated evidence, leave unavailable
native macOS and Linux runners as `unbuilt`, and stop. Continuing implementation
work after that point creates a new source state and requires a new package
pass.

If no macOS runner and no Linux distribution or matching Linux CI runner are
available during the pass, the only local publishable clean-binary outcome is
Windows x64. Do not keep stale macOS or Linux archives from earlier runs in
`dist/` as release evidence for this pass.

## Final Stop Rule

After the final source commit, rebuild the Windows package from that clean
commit, record the ZIP size and SHA-256, launch the packaged Windows app from
`dist\mighty-ide-win64`, record macOS and Linux as `unbuilt` unless native
runners completed during the same pass, and stop.

For this Windows-hosted pass, "clean binaries for Windows, macOS, and Linux"
means clean platform decisions, not cross-built substitutes: Windows receives
local PE evidence, while macOS and Linux remain explicitly `unbuilt` without
matching native runners. A missing native runner is not a failed binary; it is a
release decision that prevents publishing an unverifiable archive.

If the source tree changes after a package is built, rebuild the package before
publishing it.

The final answer is part of the release evidence for this Windows-hosted pass.
It should report the committed source hash and generated Windows archive values
from the post-commit package run, while leaving macOS and Linux as `unbuilt`
unless their own native package runs completed during the same pass.

After those fields are reported, stop. Additional source, README, docs,
packaging, or feature work belongs to a later pass because it changes the source
state that the generated archive evidence was tied to.

Keep committed release docs free of generated archive hashes. The authoritative
generated values for a Windows-hosted pass are the post-commit
`dist\mighty-ide-win64\PACKAGE-MANIFEST.txt`, the final ZIP size, the final ZIP
SHA-256, and the packaged launch result.

## Final Response Fields

The final handoff message for a Windows-hosted pass should contain exactly the
platform evidence gathered during this pass:

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

Use `publish` for Windows only after the PowerShell packager and launch pass.
Use `unbuilt - native runner unavailable for this pass` for macOS or Linux when
their native package scripts were not run on matching infrastructure. Do not
continue feature work after reporting these fields.
