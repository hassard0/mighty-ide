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
  $pkg = "mighty-ide-win64"
  $dist = Join-Path "dist" $pkg
  Remove-Item -LiteralPath $dist -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path @($dist, "$dist\examples", "$dist\samples") | Out-Null

  Copy-Item "target\release\main.exe" "$dist\mighty-ide.exe" -Force
  Copy-Item "target\release\mighty_ui_sys.dll" "$dist\mighty_ui_sys.dll" -Force

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

  $byproducts = Get-ChildItem -LiteralPath $dist -Recurse -File |
    Where-Object { $_.Extension -in @(".pdb", ".lib", ".exp", ".ilk", ".obj", ".o", ".rlib", ".log") }
  if ($byproducts) {
    $names = ($byproducts | ForEach-Object { $_.FullName }) -join [Environment]::NewLine
    throw "package contains build byproducts:$([Environment]::NewLine)$names"
  }

  Write-Host "[4/5] zip package"
  $zip = "dist\mighty-ide-$Version-win64.zip"
  Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
  Compress-Archive -Path $dist -DestinationPath $zip -Force

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
