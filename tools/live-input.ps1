# live-input.ps1 — launch the IDE, drive REAL OS input via SendInput (keyboard
# unicode + mouse absolute), and screenshot the real desktop via CopyFromScreen.
# Runs on WinSta0\Default. Use with the Bash/PowerShell sandbox DISABLED.
#
#   pwsh tools/live-input.ps1 -Exe <exe> -Arg <file> -Out <png> -Script <name>
# Scripts: baseline | typing | rail | zoom | titlebar | close
param(
  [string]$Exe,
  [string]$Arg = "",
  [string]$Out,
  [string]$Script = "baseline",
  [int]$WaitMs = 5000
)

Add-Type -AssemblyName System.Drawing

$src = @"
using System;
using System.Runtime.InteropServices;
public static class N {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public U u; }
  [StructLayout(LayoutKind.Explicit)] public struct U {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr extra; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public const uint KEYEVENTF_KEYUP=0x2, KEYEVENTF_UNICODE=0x4;
  public const uint MOUSEEVENTF_MOVE=0x1, MOUSEEVENTF_ABSOLUTE=0x8000, MOUSEEVENTF_LEFTDOWN=0x2, MOUSEEVENTF_LEFTUP=0x4, MOUSEEVENTF_WHEEL=0x800;
  public static int SZ() { return Marshal.SizeOf(typeof(INPUT)); }
}
"@
Add-Type -TypeDefinition $src

$SW = [N]::GetSystemMetrics(0); $SH = [N]::GetSystemMetrics(1)
function Send([N+INPUT[]]$arr){ [N]::SendInput([uint32]$arr.Length, $arr, [N]::SZ()) | Out-Null }

function MoveAbs([int]$x,[int]$y){
  $i = New-Object N+INPUT; $i.type = 0
  $i.u.mi.dx = [int](($x * 65535) / $SW); $i.u.mi.dy = [int](($y * 65535) / $SH)
  $i.u.mi.dwFlags = [N]::MOUSEEVENTF_MOVE -bor [N]::MOUSEEVENTF_ABSOLUTE
  Send @($i); Start-Sleep -Milliseconds 80
}
function ClickAbs([int]$x,[int]$y){
  MoveAbs $x $y
  $d = New-Object N+INPUT; $d.type=0; $d.u.mi.dwFlags=[N]::MOUSEEVENTF_LEFTDOWN
  $up = New-Object N+INPUT; $up.type=0; $up.u.mi.dwFlags=[N]::MOUSEEVENTF_LEFTUP
  Send @($d); Start-Sleep -Milliseconds 60; Send @($up); Start-Sleep -Milliseconds 250
}
function WheelCtrl([int]$delta){
  # Ctrl down (vk 0x11), wheel, Ctrl up.
  $cd = New-Object N+INPUT; $cd.type=1; $cd.u.ki.wVk=0x11
  $cu = New-Object N+INPUT; $cu.type=1; $cu.u.ki.wVk=0x11; $cu.u.ki.dwFlags=[N]::KEYEVENTF_KEYUP
  $w = New-Object N+INPUT; $w.type=0; $w.u.mi.dwFlags=[N]::MOUSEEVENTF_WHEEL; $w.u.mi.mouseData=[uint32]$delta
  Send @($cd); Start-Sleep -Milliseconds 40; Send @($w); Start-Sleep -Milliseconds 40; Send @($cu); Start-Sleep -Milliseconds 200
}
function TypeUnicode([string]$s){
  foreach($ch in $s.ToCharArray()){
    $d = New-Object N+INPUT; $d.type=1; $d.u.ki.wScan=[uint16][int][char]$ch; $d.u.ki.dwFlags=[N]::KEYEVENTF_UNICODE
    $u = New-Object N+INPUT; $u.type=1; $u.u.ki.wScan=[uint16][int][char]$ch; $u.u.ki.dwFlags=([N]::KEYEVENTF_UNICODE -bor [N]::KEYEVENTF_KEYUP)
    Send @($d); Start-Sleep -Milliseconds 30; Send @($u); Start-Sleep -Milliseconds 40
  }
}
function VKey([uint16]$vk){
  $d = New-Object N+INPUT; $d.type=1; $d.u.ki.wVk=$vk
  $u = New-Object N+INPUT; $u.type=1; $u.u.ki.wVk=$vk; $u.u.ki.dwFlags=[N]::KEYEVENTF_KEYUP
  Send @($d); Start-Sleep -Milliseconds 50; Send @($u); Start-Sleep -Milliseconds 80
}
function CtrlChar([uint16]$vk){
  $cd = New-Object N+INPUT; $cd.type=1; $cd.u.ki.wVk=0x11
  $kd = New-Object N+INPUT; $kd.type=1; $kd.u.ki.wVk=$vk
  $ku = New-Object N+INPUT; $ku.type=1; $ku.u.ki.wVk=$vk; $ku.u.ki.dwFlags=[N]::KEYEVENTF_KEYUP
  $cu = New-Object N+INPUT; $cu.type=1; $cu.u.ki.wVk=0x11; $cu.u.ki.dwFlags=[N]::KEYEVENTF_KEYUP
  Send @($cd); Start-Sleep -Milliseconds 30; Send @($kd); Start-Sleep -Milliseconds 40; Send @($ku); Start-Sleep -Milliseconds 30; Send @($cu); Start-Sleep -Milliseconds 200
}

Write-Host "Launching $Exe $Arg ..."
if ($Arg -ne "") { $proc = Start-Process -FilePath $Exe -ArgumentList $Arg -PassThru }
else { $proc = Start-Process -FilePath $Exe -PassThru }
Start-Sleep -Milliseconds $WaitMs

$hwnd = [IntPtr]::Zero
for ($i=0; $i -lt 40; $i++) { $proc.Refresh(); if ($proc.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $proc.MainWindowHandle; break }; Start-Sleep -Milliseconds 200 }
Write-Host "hwnd=$hwnd"
[N]::ShowWindow($hwnd, 9) | Out-Null
[N]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 700

$r = New-Object N+RECT
[N]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$Wd = $r.Right - $r.Left; $Ht = $r.Bottom - $r.Top
Write-Host "rect L=$($r.Left) T=$($r.Top) R=$($r.Right) B=$($r.Bottom) ($Wd x $Ht)"

# Ensure focus by clicking the title-bar-safe interior first.
ClickAbs ([int]($r.Left + $Wd*0.5)) ([int]($r.Top + $Ht*0.5))
[N]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 300

switch ($Script) {
  "typing" {
    ClickAbs ([int]($r.Left + $Wd*0.55)) ([int]($r.Top + $Ht*0.45))
    Start-Sleep -Milliseconds 250
    TypeUnicode "hello world"
    VKey 0x08   # Backspace
    VKey 0x0D   # Enter
    Start-Sleep -Milliseconds 400
  }
  "rail" {
    ClickAbs ([int]($r.Left + 24)) ([int]($r.Top + 110))
    Start-Sleep -Milliseconds 600
  }
  "zoom" {
    ClickAbs ([int]($r.Left + $Wd*0.55)) ([int]($r.Top + $Ht*0.45))
    Start-Sleep -Milliseconds 200
    for ($i=0; $i -lt 5; $i++){ CtrlChar 0xBB }  # Ctrl+'=' (VK_OEM_PLUS)
    Start-Sleep -Milliseconds 400
  }
  "titlebar" { Start-Sleep -Milliseconds 400 }
  "close" {
    ClickAbs ($r.Right - 23) ($r.Top + 17)
    Start-Sleep -Milliseconds 900
  }
  default { Start-Sleep -Milliseconds 200 }
}

Start-Sleep -Milliseconds 400
[N]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$Wd = $r.Right - $r.Left; $Ht = $r.Bottom - $r.Top
if ($Wd -le 0 -or $Ht -le 0) { $Wd = 1024; $Ht = 700; $r.Left = 0; $r.Top = 0 }
$bmp = New-Object System.Drawing.Bitmap $Wd, $Ht
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $Wd, $Ht))
$g.Dispose()
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "Saved $Out"

Start-Sleep -Milliseconds 200
$proc.Refresh()
if ($proc.HasExited) { Write-Host "PROC_EXITED" } else { Write-Host "PROC_ALIVE" }
if ($Script -ne "close") { if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } }
