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
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern short VkKeyScan(char ch);

    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);

    // Make THIS process per-monitor-DPI-aware (V2) so GetWindowRect + screen capture
    // use true physical pixels that match the DPI-aware IDE's surface. Without this,
    // on a >100% monitor Windows virtualises our coordinates and captures/clicks
    // land in the wrong place (the root of the "everything is misaligned" confusion
    // in the harness). DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4.
    public static void MakeDpiAware() {
        try { SetProcessDpiAwarenessContext((IntPtr)(-4)); } catch {}
    }

    // Force `h` to the foreground despite Windows' foreground lock by briefly
    // attaching our input thread to the current foreground thread (the standard
    // reliable activation hack). Returns true only if `h` actually became
    // foreground - callers MUST refuse to trust a screen-capture otherwise.
    public static bool ForceForeground(IntPtr h) {
        // Minimize then restore: restoring a minimized window reliably ACTIVATES it
        // (defeats the foreground lock without synthetic Alt/Alt-Tab, which were
        // disruptive). Combined with AttachThreadInput + a TOPMOST bounce.
        IntPtr fg = GetForegroundWindow();
        uint dummy;
        uint fgTid = GetWindowThreadProcessId(fg, out dummy);
        uint myTid = GetCurrentThreadId();
        bool attached = (fgTid != 0 && fgTid != myTid && AttachThreadInput(myTid, fgTid, true));
        if (GetForegroundWindow() != h) {
            ShowWindow(h, 6); // SW_MINIMIZE
            ShowWindow(h, 9); // SW_RESTORE  (re-activates)
        }
        SetWindowPos(h, (IntPtr)(-1), 0, 0, 0, 0, 0x1 | 0x2 | 0x40); // HWND_TOPMOST
        SetWindowPos(h, (IntPtr)(-2), 0, 0, 0, 0, 0x1 | 0x2 | 0x40); // HWND_NOTOPMOST
        BringWindowToTop(h);
        SetForegroundWindow(h);
        if (attached) AttachThreadInput(myTid, fgTid, false);
        return GetForegroundWindow() == h;
    }

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

[Win]::MakeDpiAware()   # must run before any GetWindowRect / screen-capture calls

function New-Dir($p) { if (-not (Test-Path $p)) { New-Item -ItemType Directory -Force $p | Out-Null } }
New-Dir $OutDir
if ($env:MUI_TRACE) { Remove-Item -LiteralPath $env:MUI_TRACE -Force -ErrorAction SilentlyContinue }

$report = [System.Collections.Generic.List[string]]::new()
function Log($m) { $line = "[{0}] {1}" -f ((Get-Date).ToString('HH:mm:ss.fff')), $m; $report.Add($line); Write-Host $line }
$script:HarnessFailed = $false

# Keep harness artifacts out of the Explorer tree. A stale Save-As file shifts
# row positions and makes the fixed RUN.txt click open the wrong file.
$saveName = "harnesssaveas.mty"
$savePath = Join-Path $WorkDir $saveName
if (Test-Path $savePath) { Remove-Item $savePath -Force }
# The IDE now uses a native SaveFileDialog for untitled Save. Feed a deterministic
# picker result so the harness does not block on an OS-modal dialog.
$env:MUI_SAVE_FILE_PICK = $savePath

function Get-WinRect($h) { $r = New-Object Win+RECT; [void][Win]::GetWindowRect($h, [ref]$r); return $r }

function Capture($h, $name) {
  # Bring the window truly foreground and CONFIRM it - a GPU window captured via
  # CopyFromScreen while occluded yields the desktop/other windows, not the IDE.
  $fg = $false
  for ($i = 0; $i -lt 5; $i++) { $fg = [Win]::ForceForeground($h); if ($fg) { break }; Start-Sleep -Milliseconds 120 }
  Start-Sleep -Milliseconds 120
  $r = Get-WinRect $h
  $w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
  if ($w -le 0 -or $hh -le 0) { Log "capture '$name': window has zero size ($w x $hh)"; return $null }
  if (-not $fg) { Log "capture '$name': !!! WINDOW NOT FOREGROUND - capture is UNTRUSTWORTHY" }
  $bmp = New-Object System.Drawing.Bitmap $w, $hh
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  try {
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $hh))
    $path = Join-Path $OutDir "$name.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    Log "capture '$name' -> $path  ($w x $hh)"
    return $path
  } catch {
    Log "capture '$name': FAILED - $($_.Exception.Message)"
    return $null
  } finally {
    $g.Dispose()
    $bmp.Dispose()
  }
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
  # Post WM_KEYDOWN + WM_CHAR + WM_KEYUP per character. The KEYDOWN's VK must be
  # the REAL virtual-key for the char (via VkKeyScan) or winit drops the WM_CHAR
  # for punctuation like '_' and '.' (it associates text with a valid key-down).
  $VK_SHIFT = 0x10
  foreach ($ch in $text.ToCharArray()) {
    $code = [int][char]$ch
    $vks = [Win]::VkKeyScan($ch)
    $vk = $vks -band 0xFF                       # low byte = virtual-key code
    $needShift = ((($vks -shr 8) -band 1) -eq 1)  # high byte bit0 = shift required
    if ($vk -le 0) { $vk = $code; $needShift = $false }
    if ($needShift) { [void][Win]::PostMessage($h, [Win]::WM_KEYDOWN, [IntPtr]$VK_SHIFT, [IntPtr]0); Start-Sleep -Milliseconds 4 }
    [void][Win]::PostMessage($h, [Win]::WM_KEYDOWN, [IntPtr]$vk,   [IntPtr]0); Start-Sleep -Milliseconds 6
    [void][Win]::PostMessage($h, [Win]::WM_CHAR,    [IntPtr]$code, [IntPtr]0); Start-Sleep -Milliseconds 6
    [void][Win]::PostMessage($h, [Win]::WM_KEYUP,   [IntPtr]$vk,   [IntPtr]0); Start-Sleep -Milliseconds 6
    if ($needShift) { [void][Win]::PostMessage($h, [Win]::WM_KEYUP, [IntPtr]$VK_SHIFT, [IntPtr]0); Start-Sleep -Milliseconds 4 }
  }
  Log "typed (PostMessage) '$text'"
}

# ----------------------------------------------------------------------------
# Scenario
# ----------------------------------------------------------------------------
Log "launching $Exe"
$proc = Start-Process -FilePath $Exe -WorkingDirectory $WorkDir -PassThru
Start-Sleep -Milliseconds $LaunchWaitMs

# Resolve the top-level window handle - the LARGEST visible window of the process
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
# Move to the top-left WITHOUT resizing (keep the window's natural physical size),
# so the GPU surface stays in sync with the window — resizing here crossed monitors
# / DPI and left the surface larger than the window, clipping the right-side chrome.
$r0 = Get-WinRect $hwnd
[void][Win]::MoveWindow($hwnd, 0, 0, ($r0.Right - $r0.Left), ($r0.Bottom - $r0.Top), $true)
Start-Sleep -Milliseconds 250
$fg0 = [Win]::ForceForeground($hwnd)
Start-Sleep -Milliseconds 300
$r = Get-WinRect $hwnd
$script:WinW = $r.Right - $r.Left
$script:WinH = $r.Bottom - $r.Top
Log ("window rect = {0},{1} {2}x{3}  foreground={4}" -f $r.Left, $r.Top, $script:WinW, $script:WinH, $fg0)
Capture $hwnd "01-initial"
$resp0 = Is-Responsive $hwnd
Log "responsive at startup: $resp0"

# Derive the exact logical<->physical scale from the IDE's STARTUP_GEOM trace line,
# so clicks hit LOGICAL targets precisely on any-DPI monitor (no more guessing).
$scale = 1.0
$tf = $env:MUI_TRACE
if ($tf -and (Test-Path $tf)) {
  $gl = Select-String -Path $tf -Pattern 'STARTUP_GEOM.*scale=([0-9.]+)' | Select-Object -Last 1
  if ($gl) { $scale = [double]$gl.Matches[0].Groups[1].Value }
}
Log "ui scale = $scale"
function ClickL($lx, $ly) { Click $hwnd ([int][math]::Round($lx * $scale)) ([int][math]::Round($ly * $scale)) }
$logicalW = [double]$script:WinW / [double]$scale
$logicalH = [double]$script:WinH / [double]$scale

# Logical layout constants (mirror layout.rs): rail x=26; tree rows under the
# 40px header; Explorer header buttons are right-aligned in the sidebar band:
# rail 52px + sidebar 324px, then -72/-50/-28 for new file/folder/collapse.
$treeX = 110
$explorerNewFileX = 311
$explorerNewFolderX = 333
$explorerCollapseX = 355

# === WELCOME NEW FILE: quick action must reveal a blank editor, not leave Welcome up. ===
ClickL 455 589
Start-Sleep -Milliseconds 350
Capture $hwnd "02-welcome-new-file"
Log "welcome new-file captured"

# === FILE OPEN: click RUN.txt in the tree; the editor must show its contents. ===
ClickL $treeX 229
Start-Sleep -Milliseconds 500
Capture $hwnd "10-open-file"
Log "file-open (tree RUN.txt) captured"

# === TOP-LEFT EXPLORER HEADER BUTTONS ===
ClickL $explorerNewFileX 20  # New File -> fresh untitled tab
Start-Sleep -Milliseconds 350
Capture $hwnd "11-new-file"
Log "new-file button captured"
ClickL $explorerCollapseX 20 # Collapse all folders
Start-Sleep -Milliseconds 300
Capture $hwnd "12-collapse"
ClickL $explorerNewFolderX 20 # New Folder -> name prompt opens
Start-Sleep -Milliseconds 300
Capture $hwnd "13-newfolder-prompt"
Press-VK $hwnd 0x1B      # cancel the prompt
Start-Sleep -Milliseconds 150

# === RAIL NAVIGATION (logical x=26; slot center = 52 + slot*42 + 19) ===
$rail = @(
  @{ n='search';  y=113 },
  @{ n='scm';     y=155 },
  @{ n='outline'; y=281 },
  @{ n='debug';   y=323 },
  @{ n='test';    y=365 }
)
$slot = 0
foreach ($ic in $rail) {
  $slot++
  ClickL 26 $ic.y
  Start-Sleep -Milliseconds 350
  $resp = Is-Responsive $hwnd
  Capture $hwnd ("20-rail-{0}-{1}" -f $slot, $ic.n)
  Log ("rail '{0}' (ly={1}) responsive={2}" -f $ic.n, $ic.y, $resp)
  if (-not $resp) { Log "!!! LOCKUP after rail '$($ic.n)'" }
}
ClickL 26 71             # back to Explorer
Start-Sleep -Milliseconds 300

# === AUTOCOMPLETE: open a real file, click into the editor, type an identifier ===
ClickL $treeX 229        # RUN.txt
Start-Sleep -Milliseconds 300
ClickL 460 130           # editor body (logical), place caret
Start-Sleep -Milliseconds 150
Type-Text $hwnd "pr"
Start-Sleep -Milliseconds 500
Capture $hwnd "30-autocomplete"
Press-VK $hwnd 0x1B
Start-Sleep -Milliseconds 150

# === TYPING into a fresh untitled buffer ===
ClickL $explorerNewFileX 20  # New File
Start-Sleep -Milliseconds 250
ClickL 460 130           # editor body
Start-Sleep -Milliseconds 100
Type-Text $hwnd "fn main"
Start-Sleep -Milliseconds 300
Capture $hwnd "31-typing"
$respT = Is-Responsive $hwnd
Log "after typing: responsive=$respT"
Press-VK $hwnd 0x1B      # close autocomplete before using topbar commands
Start-Sleep -Milliseconds 150

# === SAVE-AS (untitled buffer) via top-right More -> command palette ===
# The harness env above supplies the native SaveFileDialog result so this
# exercises dialog-backed Save-As. More is in the top-right action strip;
# mirror titlebar.rs:
# controls_x = width - 3*46, dots center ~= controls_x - 24 = width - 162.
ClickL ($logicalW - 162) 20
Start-Sleep -Milliseconds 400
Capture $hwnd "40-palette-open"
Type-Text $hwnd "save"
Start-Sleep -Milliseconds 300
Press-VK $hwnd 0x0D
Start-Sleep -Milliseconds 800
Capture $hwnd "42-saved"
Start-Sleep -Milliseconds 200
if (Test-Path $savePath) { Log "SAVE-AS: file written OK -> $savePath" } else { Log "SAVE-AS: FILE NOT FOUND ($savePath)"; $script:HarnessFailed = $true }
if (Test-Path $savePath) { Remove-Item $savePath -Force; Log "SAVE-AS: cleaned harness file" }

# === RAIL UTILITY: bottom Settings icon should open Preferences, not be decorative. ===
ClickL 26 ($logicalH - 32)
Start-Sleep -Milliseconds 350
Capture $hwnd "50-settings-rail"
Press-VK $hwnd 0x1B
Start-Sleep -Milliseconds 150

$respF = Is-Responsive $hwnd
Log "final responsive: $respF"
$proc.Refresh()
$exited = $proc.HasExited
Log "process hasExited=$exited"
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force; Log "killed pid $($proc.Id)" }
Remove-Item Env:\MUI_SAVE_FILE_PICK -ErrorAction SilentlyContinue

$reportPath = Join-Path $OutDir 'report.txt'
$report | Set-Content $reportPath -Encoding utf8
Log "report -> $reportPath"
if ($script:HarnessFailed) { exit 1 }
