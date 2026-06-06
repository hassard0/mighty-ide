<#
.SYNOPSIS
  Build the Mighty IDE end to end on Windows without requiring Bash/WSL.
#>
[CmdletBinding()]
param(
  [string]$Mty = "",
  [string]$Clang = "C:\Program Files\LLVM\bin\clang.exe",
  [switch]$Release
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root
$MinMtyVersion = [version]"0.47.0"

function Resolve-MtyCompiler {
  param([string]$Requested)

  if (-not [string]::IsNullOrWhiteSpace($Requested)) {
    if (Test-Path -LiteralPath $Requested -PathType Leaf) {
      return (Resolve-Path -LiteralPath $Requested).Path
    }
    throw "mty compiler not found: $Requested"
  }

  if (-not [string]::IsNullOrWhiteSpace($env:MIGHTY_MTY)) {
    if (Test-Path -LiteralPath $env:MIGHTY_MTY -PathType Leaf) {
      return (Resolve-Path -LiteralPath $env:MIGHTY_MTY).Path
    }
    throw "MIGHTY_MTY points to a missing compiler: $env:MIGHTY_MTY"
  }

  $cmd = Get-Command mty -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }

  throw "mty compiler not found. Set -Mty, set MIGHTY_MTY, or put mty on PATH."
}

$Mty = Resolve-MtyCompiler $Mty
if (-not (Test-Path -LiteralPath $Mty -PathType Leaf)) {
  throw "mty compiler not found: $Mty"
}
$mtyVersionText = (& $Mty --version) -join " "
if ($LASTEXITCODE -ne 0) {
  throw "failed to run '$Mty --version'; cannot verify Mighty compiler version"
}
if ($mtyVersionText -notmatch '(\d+)\.(\d+)\.(\d+)') {
  throw "unable to parse Mighty compiler version from '$mtyVersionText'"
}
$mtyVersion = [version]$Matches[0]
if ($mtyVersion -lt $MinMtyVersion) {
  throw "mty compiler $mtyVersion is too old for Mighty IDE; require $MinMtyVersion or newer. Set -Mty or MIGHTY_MTY to a current compiler."
}
if (-not (Test-Path -LiteralPath $Clang -PathType Leaf)) {
  throw "clang linker not found: $Clang"
}

$profile = if ($Release) { "release" } else { "debug" }
$outDir = if ($Release) { "target\release" } else { "target" }
$cargoArgs = @("build", "-p", "mighty-ui-sys", "-p", "mty-rt-abi")
if ($Release) { $cargoArgs += "--release" }

Write-Host "[1/5] cargo $($cargoArgs -join ' ')"
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "[2/5] stage runtime archive -> vendor/mty_rt_abi.lib"
New-Item -ItemType Directory -Force -Path @("vendor", $outDir) | Out-Null
Copy-Item "target\$profile\mty_rt_abi.lib" "vendor\mty_rt_abi.lib" -Force

Write-Host "[3/5] stage shim import lib + DLL"
Copy-Item "target\$profile\mighty_ui_sys.dll.lib" "vendor\mighty_ui_sys.dll.lib" -Force
if (-not $Release) {
  Copy-Item "target\$profile\mighty_ui_sys.dll" "target\mighty_ui_sys.dll" -Force
}

Write-Host "[4/5] mty build src\main.mty -> $outDir\main.exe"
Remove-Item "$outDir\main.exe", "$outDir\main.o" -ErrorAction SilentlyContinue
$env:MTY_LINKER = $Clang
try {
  $mtyArgs = @("build")
  if ($Release) { $mtyArgs += "--release" }
  $mtyArgs += @("src\main.mty", "--out-dir", $outDir)
  & $Mty @mtyArgs
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
  Remove-Item Env:\MTY_LINKER -ErrorAction SilentlyContinue
}

$exe = "$outDir\main.exe"
if (-not (Test-Path -LiteralPath $exe -PathType Leaf) -or (Get-Item $exe).Length -le 0) {
  if (Test-Path "$outDir\main.o") {
    throw "mty produced main.o but not main.exe; check linker discovery and MTY_LINKER."
  }
  throw "mty build did not produce $exe"
}

Write-Host "[5/5] stamp app icon"
if (Get-Command python -ErrorAction SilentlyContinue) {
  & python "tools\make-icon.py"
  if ($LASTEXITCODE -ne 0) { Write-Warning "icon regeneration failed; using existing assets\mighty-ide.ico" }
}
$icon = "assets\mighty-ide.ico"
$rcedit = "tools\rcedit-x64.exe"
if ((Test-Path -LiteralPath $icon -PathType Leaf) -and (Test-Path -LiteralPath $rcedit -PathType Leaf)) {
  & $rcedit $exe --set-icon $icon
  if ($LASTEXITCODE -ne 0) { throw "rcedit failed to stamp the app icon" }
} else {
  Write-Warning "missing $icon or $rcedit; exe icon was not stamped"
}

Get-Item $exe
Write-Host "OK: $exe"
