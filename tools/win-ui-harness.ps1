<#
.SYNOPSIS
  Black-box UI test harness for the Mighty IDE Windows binary.

.DESCRIPTION
  The IDE renders its entire UI with Vello/wgpu onto a single GPU surface, so there
  is no native control tree for UI Automation to inspect. This harness therefore
  drives the REAL window the way a user does and observes it the way a user does:

    * launches the actual .exe,
    * finds its top-level HWND,
    * injects REAL OS input (SetCursorPos + SendInput mouse, SendInput Unicode keys),
    * screen-captures the window rectangle to PNG (works for GPU surfaces, unlike
      PrintWindow which returns black for DXGI swapchains),
    * probes responsiveness with SendMessageTimeout(WM_NULL, ABORTIFHUNG) to detect
      a hung / locked-up event loop.

  This is the thing the offscreen render tests could NOT catch: the live winit
  event loop, real DPI, real click hit-testing, and OS-modal behaviour (e.g.
  drag_window's move-loop).

.NOTES
  Run from an INTERACTIVE desktop session (it moves the real cursor and needs the
  window to be foreground + unobscured for screen capture). Results land in -OutDir.
#>
[CmdletBinding()]
param(
  [string]$Exe     = "C:\Users\ihass\mighty-ide\dist\mighty-ide-win64\mighty-ide.exe",
  [string]$WorkDir = "C:\Users\ihass\mighty-ide\dist\mighty-ide-win64",
  [string]$OutDir  = "C:\Users\ihass\mighty-ide\dist\harness",
  [int]$LaunchWaitMs = 2500
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Drawing;

public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr wParam, IntPtr lParam);

    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);

    // The largest visible top-level window owned by `pid`. winit briefly exposes a
    // tiny (14x14) helper window whose handle MainWindowHandle can latch onto;
    // picking the largest visible window avoids posting input to that dead handle.
    public static IntPtr BestWindow(uint pid) {
        IntPtr best = IntPtr.Zero; int bestArea = 0;
        EnumWindows((h, l) => {
            uint wp; GetWindowThreadProcessId(h, out wp);
            if (wp == pid && IsWindowVisible(h)) {
                RECT r; GetWindowRect(h, out r);
                int area = (r.Right - r.Left) * (r.Bottom - r.Top);
                if (area > bestArea) { bestArea = area; best = h; }
            }
            return true;
        }, IntPtr.Zero);
        return best;
    }

    public const uint WM_MOUSEMOVE = 0x0200, WM_LBUTTONDOWN = 0x0201, WM_LBUTTONUP = 0x0202;
    public const uint WM_KEYDOWN = 0x0100, WM_KEYUP = 0x0101, WM_CHAR = 0x0102;
    public const int MK_LBUTTON = 0x0001;

    // lParam for mouse messages packs (y<<16)|x in CLIENT coordinates.
    public static IntPtr MouseLParam(int x, int y) { return (IntPtr)((y << 16) | (x & 0xFFFF)); }
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(
        IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam, uint flags, uint timeout, out IntPtr result);

    // ----- SendInput -----
    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT { public uint type; public InputUnion U; }
    [StructLayout(LayoutKind.Explicit)]
    public struct InputUnion {
        [FieldOffset(0)] public MOUSEINPUT mi;
        [FieldOffset(0)] public KEYBDINPUT ki;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)]
    public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr dwExtraInfo; }

    [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int cb);

    public const uint INPUT_MOUSE = 0, INPUT_KEYBOARD = 1;
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002, MOUSEEVENTF_LEFTUP = 0x0004;
    public const uint MOUSEEVENTF_WHEEL = 0x0800;
    public const uint KEYEVENTF_UNICODE = 0x0004, KEYEVENTF_KEYUP = 0x0002;
    public const uint WM_NULL = 0x0000;
    public const uint SMTO_ABORTIFHUNG = 0x0002;

    public static int InputSize() { return Marshal.SizeOf(typeof(INPUT)); }
}
"@

function New-Dir($p) { if (-not (Test-Path $p)) { New-Item -ItemType Directory -Force $p | Out-Null } }
New-Dir $OutDir

$report = [System.Collections.Generic.List[string]]::new()
function Log($m) { $line = "[{0}] {1}" -f ((Get-Date).ToString('HH:mm:ss.fff')), $m; $report.Add($line); Write-Host $line }

function Get-WinRect($h) { $r = New-Object Win+RECT; [void][Win]::GetWindowRect($h, [ref]$r); return $r }

function Capture($h, $name) {
  $r = Get-WinRect $h
  $w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
  if ($w -le 0 -or $hh -le 0) { Log "capture '$name': window has zero size ($w x $hh)"; return $null }
  $bmp = New-Object System.Drawing.Bitmap $w, $hh
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $hh))
  $g.Dispose()
  $path = Join-Path $OutDir "$name.png"
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  Log "capture '$name' -> $path  ($w x $hh)"
  return $path
}

function Is-Responsive($h, $timeoutMs = 1200) {
  $res = [IntPtr]::Zero
  $ret = [Win]::SendMessageTimeout($h, [Win]::WM_NULL, [IntPtr]::Zero, [IntPtr]::Zero, [Win]::SMTO_ABORTIFHUNG, [uint32]$timeoutMs, [ref]$res)
  return ($ret -ne [IntPtr]::Zero)
}

function Click($h, $relX, $relY) {
  # relX/relY are CLIENT (window-relative) pixels. We PostMessage directly to the
  # window so input is delivered regardless of OS foreground/focus (the previous
  # SendInput approach silently lost every event to the foreground-locked terminal).
  $lp = [Win]::MouseLParam($relX, $relY)
  [void][Win]::PostMessage($h, [Win]::WM_MOUSEMOVE,   [IntPtr][Win]::MK_LBUTTON, $lp); Start-Sleep -Milliseconds 20
  [void][Win]::PostMessage($h, [Win]::WM_LBUTTONDOWN, [IntPtr][Win]::MK_LBUTTON, $lp); Start-Sleep -Milliseconds 40
  [void][Win]::PostMessage($h, [Win]::WM_LBUTTONUP,   [IntPtr]0,                 $lp)
  Log "click (PostMessage) at client ($relX,$relY)"
}

function Press-VK($h, $vk) {
  [void][Win]::PostMessage($h, [Win]::WM_KEYDOWN, [IntPtr]$vk, [IntPtr]0); Start-Sleep -Milliseconds 15
  [void][Win]::PostMessage($h, [Win]::WM_KEYUP,   [IntPtr]$vk, [IntPtr]0); Start-Sleep -Milliseconds 15
}

function Type-Text($h, $text) {
  # Post WM_KEYDOWN + WM_CHAR + WM_KEYUP per character so winit produces both a
  # key event and the text. VK for ASCII letters/digits is the uppercase code.
  foreach ($ch in $text.ToCharArray()) {
    $code = [int][char]$ch
    $vk = [int][char]([string]$ch).ToUpper()
    [void][Win]::PostMessage($h, [Win]::WM_KEYDOWN, [IntPtr]$vk,   [IntPtr]0); Start-Sleep -Milliseconds 6
    [void][Win]::PostMessage($h, [Win]::WM_CHAR,    [IntPtr]$code, [IntPtr]0); Start-Sleep -Milliseconds 6
    [void][Win]::PostMessage($h, [Win]::WM_KEYUP,   [IntPtr]$vk,   [IntPtr]0); Start-Sleep -Milliseconds 10
  }
  Log "typed (PostMessage) '$text'"
}

# ----------------------------------------------------------------------------
# Scenario
# ----------------------------------------------------------------------------
Log "launching $Exe"
$proc = Start-Process -FilePath $Exe -WorkingDirectory $WorkDir -PassThru
Start-Sleep -Milliseconds $LaunchWaitMs

# Resolve the top-level window handle — the LARGEST visible window of the process
# (winit briefly shows a 14x14 helper window that MainWindowHandle can latch onto).
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 60; $i++) {
  $proc.Refresh()
  if ($proc.HasExited) { Log "PROCESS EXITED early (code $($proc.ExitCode))"; break }
  $cand = [Win]::BestWindow([uint32]$proc.Id)
  if ($cand -ne [IntPtr]::Zero) {
    $cr = Get-WinRect $cand
    if (($cr.Right - $cr.Left) -ge 200 -and ($cr.Bottom - $cr.Top) -ge 200) { $hwnd = $cand; break }
  }
  Start-Sleep -Milliseconds 150
}
if ($hwnd -eq [IntPtr]::Zero) { Log "NO WINDOW HANDLE - aborting"; $report | Set-Content (Join-Path $OutDir 'report.txt') -Encoding utf8; return }
Log "hwnd = $hwnd"
$r = Get-WinRect $hwnd
Log ("window rect = {0},{1} {2}x{3}" -f $r.Left, $r.Top, ($r.Right-$r.Left), ($r.Bottom-$r.Top))

# Move the window to the top-left and raise it, so the controlling terminal does
# not overlap the capture and coordinates are stable.
[void][Win]::ShowWindow($hwnd, 9)   # SW_RESTORE
[void][Win]::MoveWindow($hwnd, 0, 0, 1200, 800, $true)
Start-Sleep -Milliseconds 200
[void][Win]::BringWindowToTop($hwnd)
[void][Win]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 400
$r = Get-WinRect $hwnd
Log ("repositioned rect = {0},{1} {2}x{3}" -f $r.Left, $r.Top, ($r.Right-$r.Left), ($r.Bottom-$r.Top))
Capture $hwnd "01-initial"
$resp0 = Is-Responsive $hwnd
Log "responsive at startup: $resp0"

# FRESH-LAUNCH TYPING TEST (no prior clicks/state — the real first-run scenario):
# type immediately; the Welcome landing should dismiss and the text should appear.
Type-Text $hwnd "fnmain"
Start-Sleep -Milliseconds 300
Capture $hwnd "01b-fresh-typing"
Log "fresh-launch typing captured"

# --- DIAGNOSTICS for the reported issues ---
# Autocomplete: type an identifier prefix; a completion popup should appear.
Type-Text $hwnd " pr"
Start-Sleep -Milliseconds 600
Capture $hwnd "d1-autocomplete-probe"
Press-VK $hwnd 0x1B
Start-Sleep -Milliseconds 150
# Right-docked AI panel: open it (Agents rail icon ~ logical y239) and check whether
# the top-right window controls (min/max/close) remain visible.
Click $hwnd 64 335
Start-Sleep -Milliseconds 500
Capture $hwnd "d2-ai-panel-topright"
Press-VK $hwnd 0x1B
# Let the frame-time heartbeat accumulate a couple of windows for the lag read.
Start-Sleep -Milliseconds 2200
Log "diagnostics captured"

# Activity rail: icons are centered ~x=64, first icon ~y=143, then ~74px steps
# (calibrated from 01-initial). slot1=Explorer 2=Search 3=SCM 4=Run 5=AI 6=Outline
# 7=Debug 8=Test.
$railX = 64
$icons = @(
  @{ n='explorer'; y=143 },
  @{ n='search';   y=217 },
  @{ n='scm';      y=289 },
  @{ n='run';      y=363 },
  @{ n='ai';       y=437 },
  @{ n='outline';  y=511 },
  @{ n='debug';    y=585 }
)
$slot = 0
foreach ($ic in $icons) {
  $slot++
  Click $hwnd $railX $ic.y
  Start-Sleep -Milliseconds 400
  $resp = Is-Responsive $hwnd
  Capture $hwnd ("02-rail-{0}-{1}" -f $slot, $ic.n)
  Log ("after rail click '{0}' (y={1}) : responsive={2}" -f $ic.n, $ic.y, $resp)
  if (-not $resp) { Log "!!! LOCKUP DETECTED after rail click '$($ic.n)' - window stopped responding"; break }
}

# --- LOCKUP HYPOTHESIS: title-bar drag strip -> winit drag_window() enters an OS
# modal move-loop when fired from a click. Click the rail header (top-left, y=15)
# and the caption strip (top-right empty area) and check responsiveness. ---
$resp = Is-Responsive $hwnd
if ($resp) {
  Click $hwnd 64 15
  Start-Sleep -Milliseconds 500
  $respDrag1 = Is-Responsive $hwnd
  Capture $hwnd "04-after-railheader-click"
  Log "after rail-header (drag region) click: responsive=$respDrag1"
  if (-not $respDrag1) { Log "!!! LOCKUP after rail-header click (drag_window modal loop)" }
}
$resp = Is-Responsive $hwnd
if ($resp) {
  Click $hwnd 1000 25
  Start-Sleep -Milliseconds 500
  $respDrag2 = Is-Responsive $hwnd
  Capture $hwnd "05-after-caption-click"
  Log "after caption-strip click: responsive=$respDrag2"
  if (-not $respDrag2) { Log "!!! LOCKUP after caption-strip click (drag_window modal loop)" }
}

# End-to-end typing: open a real file first (the welcome screen blocks editor
# input), then type. Switch to Explorer (rail slot 0), open scratch.mty from the
# tree, click into the editor body, and type.
$resp = Is-Responsive $hwnd
if ($resp) {
  # Clear any band focus left over from the rail sweep (Run/Test) so typing is
  # tested from a clean state, like a real fresh-launch user. Escape unfocuses.
  Press-VK $hwnd 0x1B; Press-VK $hwnd 0x1B; Press-VK $hwnd 0x1B
  Start-Sleep -Milliseconds 150
  Click $hwnd 64 99          # Explorer rail icon (slot 0)
  Start-Sleep -Milliseconds 300
  Capture $hwnd "06-explorer"
  Click $hwnd 250 366        # scratch.mty row in the file tree (opens it, dismisses welcome)
  Start-Sleep -Milliseconds 400
  Capture $hwnd "07-file-open"
  Click $hwnd 720 400        # click into the editor body to focus the buffer
  Start-Sleep -Milliseconds 200
  Type-Text $hwnd "fnmain"
  Start-Sleep -Milliseconds 300
  Capture $hwnd "08-after-typing"
  $respT = Is-Responsive $hwnd
  Log "after typing: responsive=$respT"
} else {
  Log "skipping type test - window already unresponsive"
}

$respF = Is-Responsive $hwnd
Log "final responsive: $respF"
$proc.Refresh()
$exited = $proc.HasExited
Log "process hasExited=$exited"
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force; Log "killed pid $($proc.Id)" }

$reportPath = Join-Path $OutDir 'report.txt'
$report | Set-Content $reportPath -Encoding utf8
Log "report -> $reportPath"
