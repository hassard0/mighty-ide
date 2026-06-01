# drive-input.ps1 — launch the IDE, drive REAL keyboard/mouse input via Win32
# SendInput / SetCursorPos / mouse_event, and screenshot the desktop.
#
# Usage:
#   pwsh tools/drive-input.ps1 -Exe <path> -Out <png> -Script <name>
# Scripts: baseline | typing | rail | zoom | titlebar | close
param(
  [string]$Exe = "$PSScriptRoot\..\dist\mighty-ide-win64\mighty-ide.exe",
  [string]$Out = "$PSScriptRoot\..\dist\out.png",
  [string]$Script = "baseline",
  [string]$Arg = "",
  [int]$WaitMs = 4000
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$src = @"
using System;
using System.Runtime.InteropServices;
public static class Win32 {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, IntPtr dwExtraInfo);
  [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr FindWindow(string c, string n);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
  public const uint MOUSEEVENTF_LEFTDOWN = 0x02;
  public const uint MOUSEEVENTF_LEFTUP   = 0x04;
  public const uint MOUSEEVENTF_WHEEL    = 0x0800;
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
Add-Type -TypeDefinition $src

function Find-IdeWindow {
  # The window title is set by mui_init; match any top-level window owned by the proc.
  param($proc)
  $h = [IntPtr]::Zero
  for ($i=0; $i -lt 40; $i++) {
    $proc.Refresh()
    if ($proc.MainWindowHandle -ne [IntPtr]::Zero) { $h = $proc.MainWindowHandle; break }
    Start-Sleep -Milliseconds 200
  }
  return $h
}

Write-Host "Launching $Exe $Arg ..."
if ($Arg -ne "") {
  $proc = Start-Process -FilePath $Exe -ArgumentList $Arg -PassThru
} else {
  $proc = Start-Process -FilePath $Exe -PassThru
}
Start-Sleep -Milliseconds $WaitMs

$hwnd = Find-IdeWindow $proc
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "WARN: no MainWindowHandle"; }
[Win32]::ShowWindow($hwnd, 9) | Out-Null   # SW_RESTORE
[Win32]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 600

$r = New-Object Win32+RECT
[Win32]::GetWindowRect($hwnd, [ref]$r) | Out-Null
Write-Host "Window rect: L=$($r.Left) T=$($r.Top) R=$($r.Right) B=$($r.Bottom)"
$W = $r.Right - $r.Left
$H = $r.Bottom - $r.Top

function Click([int]$x, [int]$y) {
  [Win32]::SetCursorPos($x, $y) | Out-Null
  Start-Sleep -Milliseconds 120
  [Win32]::mouse_event([Win32]::MOUSEEVENTF_LEFTDOWN, 0,0,0,[IntPtr]::Zero)
  Start-Sleep -Milliseconds 60
  [Win32]::mouse_event([Win32]::MOUSEEVENTF_LEFTUP, 0,0,0,[IntPtr]::Zero)
  Start-Sleep -Milliseconds 250
}

switch ($Script) {
  "typing" {
    # Click into the editor body first to ensure focus, then type.
    Click ([int]($r.Left + $W*0.55)) ([int]($r.Top + $H*0.4))
    Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait("hello world")
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait("{ENTER}second line")
    Start-Sleep -Milliseconds 400
  }
  "rail" {
    # The activity rail is the leftmost ~48px column. Click the 2nd icon (Search).
    Click ([int]($r.Left + 24)) ([int]($r.Top + 110))
    Start-Sleep -Milliseconds 500
  }
  "zoom" {
    Click ([int]($r.Left + $W*0.55)) ([int]($r.Top + $H*0.4))
    Start-Sleep -Milliseconds 200
    for ($i=0; $i -lt 4; $i++) {
      [System.Windows.Forms.SendKeys]::SendWait("^{ADD}")
      Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Milliseconds 400
  }
  "titlebar" {
    # Just settle and screenshot so we can read the custom title bar / window
    # controls (no OS chrome). Close-button behavior is verified by "close".
    Start-Sleep -Milliseconds 400
  }
  "close" {
    # Click the close button. On the borderless title bar the controls sit at the
    # far right of the top tab-bar row: close is the rightmost ~46px-wide button,
    # vertically centered in the bar. Click its center.
    Click ([int]($r.Right - 23)) ([int]($r.Top + 17))
    Start-Sleep -Milliseconds 800
  }
  default { Start-Sleep -Milliseconds 200 }
}

Start-Sleep -Milliseconds 400
# Screenshot the window region (or whole screen if rect invalid).
if ($W -le 0 -or $H -le 0) {
  $bmp = New-Object System.Drawing.Bitmap ([System.Windows.Forms.SystemInformation]::VirtualScreen.Width), ([System.Windows.Forms.SystemInformation]::VirtualScreen.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen(0,0,0,0,$bmp.Size)
} else {
  $bmp = New-Object System.Drawing.Bitmap $W, $H
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
}
$g.Dispose()
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "Saved $Out"

# Report whether the process is still alive (for close test).
Start-Sleep -Milliseconds 200
$proc.Refresh()
if ($proc.HasExited) { Write-Host "PROC_EXITED" } else { Write-Host "PROC_ALIVE" }

# Clean up unless this is the close test (which checks exit).
if ($Script -ne "close") {
  if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}
