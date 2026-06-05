<#
.SYNOPSIS
  Build a release Mighty IDE and assemble dist\mighty-ide-win64 on Windows.
#>
[CmdletBinding()]
param(
  [string]$Version = "v0.3.0",
  [string]$Mty = "",
  [string]$Clang = "C:\Program Files\LLVM\bin\clang.exe"
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

if ((Test-Path ".git") -and (Get-Command git -ErrorAction SilentlyContinue)) {
  $gitStatus = & git status --porcelain
  if ($LASTEXITCODE -ne 0) { throw "git status failed; refusing to package" }
  if ($gitStatus) {
    throw "package-win.ps1 requires a clean git worktree before building release artifacts"
  }
}

$pkg = "mighty-ide-win64"
$dist = Join-Path "dist" $pkg
$zip = "dist\mighty-ide-$Version-win64.zip"
Remove-Item -LiteralPath $dist -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue

function Assert-PeBinary {
  param([Parameter(Mandatory = $true)][string]$Path)
  $full = Resolve-Path -LiteralPath $Path
  $fs = [System.IO.File]::OpenRead($full)
  try {
    $br = New-Object System.IO.BinaryReader($fs)
    if ($br.ReadByte() -ne 0x4d -or $br.ReadByte() -ne 0x5a) {
      throw "$Path is not a PE binary: missing MZ header"
    }
    $fs.Seek(0x3c, [System.IO.SeekOrigin]::Begin) | Out-Null
    $peOffset = $br.ReadInt32()
    if ($peOffset -lt 0 -or $peOffset -gt ($fs.Length - 4)) {
      throw "$Path is not a PE binary: invalid PE header offset"
    }
    $fs.Seek($peOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
    if ($br.ReadByte() -ne 0x50 -or $br.ReadByte() -ne 0x45 -or
        $br.ReadByte() -ne 0x00 -or $br.ReadByte() -ne 0x00) {
      throw "$Path is not a PE binary: missing PE signature"
    }
  } finally {
    $fs.Dispose()
  }
}

function Assert-NoForeignNativeArtifacts {
  param([Parameter(Mandatory = $true)][string]$Path)
  $foreign = Get-ChildItem -LiteralPath $Path -Recurse -File |
    Where-Object { $_.Extension -in @(".dylib", ".so") }
  if ($foreign) {
    $names = ($foreign | ForEach-Object { $_.FullName }) -join [Environment]::NewLine
    throw "Windows package contains non-Windows native payloads:$([Environment]::NewLine)$names"
  }
}

function Assert-NoBuildSidecars {
  param([Parameter(Mandatory = $true)][string]$Path)
  $sidecars = @(
    Get-ChildItem -LiteralPath $Path -Recurse -File |
      Where-Object {
        $_.Extension -in @(
          ".pdb", ".lib", ".exp", ".ilk", ".obj", ".o", ".a", ".rlib",
          ".log", ".debug", ".map"
        )
      }
    Get-ChildItem -LiteralPath $Path -Recurse -Directory |
      Where-Object { $_.Extension -eq ".dSYM" }
  )
  if ($sidecars) {
    $names = ($sidecars | ForEach-Object { $_.FullName }) -join [Environment]::NewLine
    throw "package contains build byproducts:$([Environment]::NewLine)$names"
  }
}

function Assert-ZipHasCleanBinaries {
  param([Parameter(Mandatory = $true)][string]$Archive)
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $full = Resolve-Path -LiteralPath $Archive
  $zip = [System.IO.Compression.ZipFile]::OpenRead($full)
  try {
    $bad = $zip.Entries |
      Where-Object {
        $_.FullName -match '\.(pdb|lib|exp|ilk|obj|o|a|rlib|log|debug|map|dylib|so)$|\.dSYM(/|$)'
      }
    if ($bad) {
      $names = ($bad | ForEach-Object { $_.FullName }) -join [Environment]::NewLine
      throw "archive contains build sidecars or non-Windows native payloads:$([Environment]::NewLine)$names"
    }
  } finally {
    $zip.Dispose()
  }
}

function Write-PackageManifest {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Archive,
    [Parameter(Mandatory = $true)][string[]]$NativeBinaries
  )
  $manifest = Join-Path $Path "PACKAGE-MANIFEST.txt"
  $lines = @(
    "Mighty IDE package verification",
    "Platform: Windows x64",
    "Version: $Version",
    "Generated: $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
    "",
    "Native payloads:"
  )
  foreach ($binary in $NativeBinaries) {
    $item = Get-Item -LiteralPath $binary
    $hash = Get-FileHash -LiteralPath $binary -Algorithm SHA256
    $lines += "- $($item.Name) | PE | $($item.Length) bytes | SHA256 $($hash.Hash)"
  }
  $lines += @(
    "",
    "Archive: $Archive",
    "Clean binary checks:",
    "- PE headers verified for mighty-ide.exe and mighty_ui_sys.dll",
    "- No compiler/linker sidecars found",
    "- No non-Windows native payloads found"
  )
  Set-Content -LiteralPath $manifest -Encoding UTF8 -Value $lines
}

$env:CARGO_INCREMENTAL = "0"
$prevRustflags = $env:RUSTFLAGS
$releaseLinkFlags = "-C debuginfo=0 -C link-arg=/DEBUG:NONE"
if ([string]::IsNullOrWhiteSpace($prevRustflags)) {
  $env:RUSTFLAGS = $releaseLinkFlags
} elseif ($prevRustflags -notlike "*/DEBUG:NONE*") {
  $env:RUSTFLAGS = "$prevRustflags $releaseLinkFlags"
}
try {
  Write-Host "[1/5] release build"
  & "$Root\build-ide.ps1" -Release -Mty $Mty -Clang $Clang
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "[2/5] assemble dist\mighty-ide-win64"
  Remove-Item -LiteralPath $dist -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path @($dist, "$dist\examples", "$dist\samples") | Out-Null

  Copy-Item "target\release\main.exe" "$dist\mighty-ide.exe" -Force
  Copy-Item "target\release\mighty_ui_sys.dll" "$dist\mighty_ui_sys.dll" -Force
  Assert-PeBinary "$dist\mighty-ide.exe"
  Assert-PeBinary "$dist\mighty_ui_sys.dll"

  Write-Host "[3/5] icon stamp and assets"
  if (Get-Command python -ErrorAction SilentlyContinue) {
    & python "tools\make-icon.py"
    if ($LASTEXITCODE -ne 0) { Write-Warning "icon regeneration failed; using existing assets\mighty-ide.ico" }
  }
  $icon = "assets\mighty-ide.ico"
  $rcedit = "tools\rcedit-x64.exe"
  if ((Test-Path $icon) -and (Test-Path $rcedit)) {
    & $rcedit "$dist\mighty-ide.exe" --set-icon $icon
    if ($LASTEXITCODE -ne 0) { throw "rcedit failed to stamp the app icon" }
    Assert-PeBinary "$dist\mighty-ide.exe"
    Copy-Item $icon "$dist\mighty-ide.ico" -Force
  } else {
    Write-Warning "missing $icon or $rcedit; exe icon was not stamped"
  }

  Copy-Item "samples\hello.mty" "$dist\samples\hello.mty" -Force
  Copy-Item "samples\agents.mty" "$dist\samples\agents.mty" -Force
  Copy-Item "samples\web-spinner.mty" "$dist\samples\web-spinner.mty" -Force
  Copy-Item "examples\demo.mty" "$dist\examples\demo.mty" -Force
  foreach ($name in "sample.py", "sample.rs", "sample.json", "agents.mty") {
    $src = Join-Path "examples" $name
    if (Test-Path $src) { Copy-Item $src (Join-Path "$dist\examples" $name) -Force }
  }
  Copy-Item "Create-Desktop-Shortcut.ps1" "$dist\Create-Desktop-Shortcut.ps1" -Force
  Copy-Item "RUN.txt" "$dist\RUN.txt" -Force
  New-Item -ItemType Directory -Force -Path "$dist\docs" | Out-Null
  foreach ($doc in "README.md", "KEYBINDINGS.md", "CHANGELOG.md", "BUILDING.md", "LICENSE") {
    Copy-Item $doc (Join-Path $dist $doc) -Force
  }
  Copy-Item "docs\platform-packaging.md" "$dist\docs\platform-packaging.md" -Force
  Copy-Item "docs\release-verification.md" "$dist\docs\release-verification.md" -Force
  Copy-Item "docs\release-evidence.md" "$dist\docs\release-evidence.md" -Force
  Copy-Item "docs\binary-release-status.md" "$dist\docs\binary-release-status.md" -Force
  Copy-Item "docs\final-release-handoff.md" "$dist\docs\final-release-handoff.md" -Force

  Assert-NoBuildSidecars $dist
  Assert-NoForeignNativeArtifacts $dist

  Write-Host "[4/5] zip package"
  Write-PackageManifest -Path $dist -Archive $zip -NativeBinaries @(
    "$dist\mighty-ide.exe",
    "$dist\mighty_ui_sys.dll"
  )
  Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
  Compress-Archive -Path $dist -DestinationPath $zip -Force
  Assert-ZipHasCleanBinaries $zip

  Write-Host "[5/5] package contents"
  Get-ChildItem $dist
  Get-Item $zip
} finally {
  Remove-Item Env:\CARGO_INCREMENTAL -ErrorAction SilentlyContinue
  if ([string]::IsNullOrWhiteSpace($prevRustflags)) {
    Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
  } else {
    $env:RUSTFLAGS = $prevRustflags
  }
}
