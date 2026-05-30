# Create-Desktop-Shortcut.ps1 — add a "Mighty IDE" shortcut to your Desktop.
#
# Run this from the unzipped package folder (the one containing
# mighty-ide.exe). It creates a Desktop shortcut that launches the IDE with
# the bundled app icon, with its working directory set to this package folder
# (so the "samples" folder + recent files resolve nicely on first launch).
#
#   Right-click  ->  Run with PowerShell
#   or:          powershell -ExecutionPolicy Bypass -File Create-Desktop-Shortcut.ps1
#
# Pass -Remove to delete the shortcut instead.

param(
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'

# The package folder = where THIS script lives.
$pkgDir   = Split-Path -Parent $MyInvocation.MyCommand.Definition
$exePath  = Join-Path $pkgDir 'mighty-ide.exe'
$iconPath = Join-Path $pkgDir 'mighty-ide.ico'
$desktop  = [Environment]::GetFolderPath('Desktop')
$lnkPath  = Join-Path $desktop 'Mighty IDE.lnk'

if ($Remove) {
    if (Test-Path $lnkPath) {
        Remove-Item $lnkPath -Force
        Write-Host "Removed: $lnkPath"
    } else {
        Write-Host "No shortcut to remove at: $lnkPath"
    }
    return
}

if (-not (Test-Path $exePath)) {
    Write-Error "mighty-ide.exe not found next to this script ($exePath). Run it from the unzipped package folder."
    return
}

$shell = New-Object -ComObject WScript.Shell
$sc = $shell.CreateShortcut($lnkPath)
$sc.TargetPath       = $exePath
$sc.WorkingDirectory = $pkgDir
$sc.Description      = 'Mighty IDE — the agent-first language IDE'
# Use the bundled .ico if present; otherwise the exe's embedded icon.
if (Test-Path $iconPath) {
    $sc.IconLocation = "$iconPath,0"
} else {
    $sc.IconLocation = "$exePath,0"
}
$sc.Save()

Write-Host "Created Desktop shortcut: $lnkPath"
Write-Host "  -> $exePath"
