<#
.SYNOPSIS
  Build the Mighty IDE end to end on Windows without requiring Bash/WSL.
#>
[CmdletBinding()]
param(
  [string]$Mty = "C:\Users\ihass\stardust\target\debug\mty.exe",
  [string]$Clang = "C:\Program Files\LLVM\bin\clang.exe",
  [switch]$Release
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

if (-not (Test-Path -LiteralPath $Mty -PathType Leaf)) {
  throw "mty compiler not found: $Mty"
}
if (-not (Test-Path -LiteralPath $Clang -PathType Leaf)) {
  throw "clang linker not found: $Clang"
}

$profile = if ($Release) { "release" } else { "debug" }
$outDir = if ($Release) { "target\release" } else { "target" }
$cargoArgs = @("build", "-p", "mighty-ui-sys", "-p", "mty-rt-abi")
if ($Release) { $cargoArgs += "--release" }

Write-Host "[1/4] cargo $($cargoArgs -join ' ')"
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "[2/4] stage runtime archive -> vendor/mty_rt_abi.lib"
New-Item -ItemType Directory -Force -Path @("vendor", $outDir) | Out-Null
Copy-Item "target\$profile\mty_rt_abi.lib" "vendor\mty_rt_abi.lib" -Force

Write-Host "[3/4] stage shim import lib + DLL"
Copy-Item "target\$profile\mighty_ui_sys.dll.lib" "vendor\mighty_ui_sys.dll.lib" -Force
if (-not $Release) {
  Copy-Item "target\$profile\mighty_ui_sys.dll" "target\mighty_ui_sys.dll" -Force
}

Write-Host "[4/4] mty build src\main.mty -> $outDir\main.exe"
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

Get-Item $exe
Write-Host "OK: $exe"
