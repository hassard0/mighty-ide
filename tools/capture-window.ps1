# capture-window.ps1 — launch the IDE, optionally drive input via PostMessage
# (works cross-desktop, unlike SendKeys/SendInput), and screenshot the window
# itself via PrintWindow (works without access to the interactive screen DC).
#
#   pwsh tools/capture-window.ps1 -Exe <exe> -Arg <file> -Out <png> -Script <name>
# Scripts: baseline | typing | rail | zoom | titlebar | close
param(
  [string]$Exe,
  [string]$Arg = "",
  [string]$Out,
  [string]$Script = "baseline",
  [int]$WaitMs = 4500
)

Add-Type -AssemblyName System.Drawing

$src = @"
using System;
using System.Runtime.InteropServices;
using System.Drawing;
public static class W {
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern IntPtr PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public const uint WM_CHAR=0x102, WM_KEYDOWN=0x100, WM_KEYUP=0x101;
  public const uint WM_LBUTTONDOWN=0x201, WM_LBUTTONUP=0x202, WM_MOUSEMOVE=0x200, WM_MOUSEWHEEL=0x20A;
  public static Bitmap Grab(IntPtr h) {
    RECT r; GetWindowRect(h, out r);
    int w = r.Right - r.Left, ht = r.Bottom - r.Top;
    if (w <= 0 || ht <= 0) return null;
    Bitmap bmp = new Bitmap(w, ht);
    using (Graphics g = Graphics.FromImage(bmp)) {
      IntPtr dc = g.GetHdc();
      PrintWindow(h, dc, 2); // PW_RENDERFULLCONTENT
      g.ReleaseHdc(dc);
    }
    return bmp;
  }
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing

function MakeLParam([int]$x,[int]$y){ return [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)) }

Write-Host "Launching $Exe $Arg ..."
if ($Arg -ne "") { $proc = Start-Process -FilePath $Exe -ArgumentList $Arg -PassThru }
else { $proc = Start-Process -FilePath $Exe -PassThru }
Start-Sleep -Milliseconds $WaitMs

$hwnd = [IntPtr]::Zero
for ($i=0; $i -lt 40; $i++) {
  $proc.Refresh()
  if ($proc.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $proc.MainWindowHandle; break }
  Start-Sleep -Milliseconds 200
}
Write-Host "hwnd=$hwnd"
[W]::ShowWindow($hwnd, 9) | Out-Null
[W]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 500

$r = New-Object W+RECT
[W]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$Wd = $r.Right - $r.Left; $Ht = $r.Bottom - $r.Top
Write-Host "rect L=$($r.Left) T=$($r.Top) R=$($r.Right) B=$($r.Bottom) ($Wd x $Ht)"

function ClickClient([int]$cx,[int]$cy){
  $lp = MakeLParam $cx $cy
  [W]::SetForegroundWindow($hwnd) | Out-Null
  [W]::PostMessage($hwnd, [W]::WM_MOUSEMOVE, [IntPtr]::Zero, $lp) | Out-Null
  Start-Sleep -Milliseconds 60
  [W]::PostMessage($hwnd, [W]::WM_LBUTTONDOWN, [IntPtr]1, $lp) | Out-Null
  Start-Sleep -Milliseconds 60
  [W]::PostMessage($hwnd, [W]::WM_LBUTTONUP, [IntPtr]::Zero, $lp) | Out-Null
  Start-Sleep -Milliseconds 250
}
function TypeStr([string]$s){
  foreach($ch in $s.ToCharArray()){
    [W]::PostMessage($hwnd, [W]::WM_CHAR, [IntPtr][int][char]$ch, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 40
  }
}
function Key([int]$vk){
  [W]::PostMessage($hwnd, [W]::WM_KEYDOWN, [IntPtr]$vk, [IntPtr]1) | Out-Null
  Start-Sleep -Milliseconds 40
  [W]::PostMessage($hwnd, [W]::WM_KEYUP, [IntPtr]$vk, [IntPtr]1) | Out-Null
  Start-Sleep -Milliseconds 60
}

switch ($Script) {
  "typing" {
    ClickClient ([int]($Wd*0.55)) ([int]($Ht*0.45))
    Start-Sleep -Milliseconds 200
    TypeStr "hello world"
    Key 0x08   # Backspace
    Key 0x0D   # Enter
    Start-Sleep -Milliseconds 300
  }
  "rail" {
    ClickClient 24 110
    Start-Sleep -Milliseconds 500
  }
  "zoom" {
    ClickClient ([int]($Wd*0.55)) ([int]($Ht*0.45))
    Start-Sleep -Milliseconds 150
    for ($i=0; $i -lt 5; $i++){
      [W]::PostMessage($hwnd, [W]::WM_KEYDOWN, [IntPtr]0x11, [IntPtr]1) | Out-Null  # Ctrl down
      [W]::PostMessage($hwnd, [W]::WM_CHAR, [IntPtr]0x3D, [IntPtr]1) | Out-Null     # '='
      [W]::PostMessage($hwnd, [W]::WM_KEYUP, [IntPtr]0x11, [IntPtr]1) | Out-Null
      Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Milliseconds 300
  }
  "titlebar" { Start-Sleep -Milliseconds 300 }
  "close" {
    ClickClient ($Wd - 23) 17
    Start-Sleep -Milliseconds 800
  }
  default { Start-Sleep -Milliseconds 200 }
}

Start-Sleep -Milliseconds 300
$bmp = [W]::Grab($hwnd)
if ($bmp -ne $null) { $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose(); Write-Host "Saved $Out" }
else { Write-Host "GRAB_FAILED" }

Start-Sleep -Milliseconds 200
$proc.Refresh()
if ($proc.HasExited) { Write-Host "PROC_EXITED" } else { Write-Host "PROC_ALIVE" }
if ($Script -ne "close") { if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } }
