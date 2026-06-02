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
  [int]$LaunchWaitMs = 2500,
  [switch]$NoCapture,
  [switch]$CaptureSmokeOnly,
  [switch]$StrictRealMouse
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
    [DllImport("user32.dll")] public static extern void SwitchToThisWindow(IntPtr h, bool altTab);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);

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
        uint dummy, targetPid;
        uint fgTid = GetWindowThreadProcessId(fg, out dummy);
        uint targetTid = GetWindowThreadProcessId(h, out targetPid);
        uint myTid = GetCurrentThreadId();
        bool attached = (fgTid != 0 && fgTid != myTid && AttachThreadInput(myTid, fgTid, true));
        bool targetAttached = (targetTid != 0 && targetTid != myTid && AttachThreadInput(myTid, targetTid, true));
        bool fgTargetAttached = (fgTid != 0 && targetTid != 0 && fgTid != targetTid && AttachThreadInput(fgTid, targetTid, true));
        if (GetForegroundWindow() != h) {
            ShowWindow(h, 6); // SW_MINIMIZE
            ShowWindow(h, 9); // SW_RESTORE  (re-activates)
        }
        SetWindowPos(h, (IntPtr)(-1), 0, 0, 0, 0, 0x1 | 0x2 | 0x40); // HWND_TOPMOST
        SetWindowPos(h, (IntPtr)(-2), 0, 0, 0, 0, 0x1 | 0x2 | 0x40); // HWND_NOTOPMOST
        BringWindowToTop(h);
        SetForegroundWindow(h);
        if (GetForegroundWindow() != h) {
            // Brief Alt tap is the standard foreground-lock escape hatch for
            // automation in an interactive desktop session.
            keybd_event(0x12, 0, 0, UIntPtr.Zero);      // VK_MENU down
            keybd_event(0x12, 0, 0x0002, UIntPtr.Zero); // KEYEVENTF_KEYUP
            SetForegroundWindow(h);
        }
        if (GetForegroundWindow() != h) {
            SwitchToThisWindow(h, true);
            SetForegroundWindow(h);
        }
        if (fgTargetAttached) AttachThreadInput(fgTid, targetTid, false);
        if (targetAttached) AttachThreadInput(myTid, targetTid, false);
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
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);

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
$OutDir = [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $OutDir))
New-Dir $OutDir
$traceWasSet = [bool]$env:MUI_TRACE
if (-not $traceWasSet) { $env:MUI_TRACE = Join-Path $OutDir "trace.txt" }
if ($env:MUI_TRACE) { Remove-Item -LiteralPath $env:MUI_TRACE -Force -ErrorAction SilentlyContinue }
$configWasSet = [bool]$env:MUI_CONFIG_DIR
if (-not $configWasSet) {
  $env:MUI_CONFIG_DIR = Join-Path $OutDir "config"
  New-Dir $env:MUI_CONFIG_DIR
}

$report = [System.Collections.Generic.List[string]]::new()
function Log($m) { $line = "[{0}] {1}" -f ((Get-Date).ToString('HH:mm:ss.fff')), $m; $report.Add($line); Write-Host $line }
$script:HarnessFailed = $false

function Finish-Harness($proc) {
  if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force; Log "killed pid $($proc.Id)" }
  Remove-Item Env:\MUI_SAVE_FILE_PICK -ErrorAction SilentlyContinue
  Remove-Item Env:\MUI_NEW_FILE_PICK -ErrorAction SilentlyContinue
  Remove-Item Env:\MUI_NEW_FILE_PICK_SEQUENCE -ErrorAction SilentlyContinue
  Remove-Item Env:\MUI_NEW_FOLDER_PICK -ErrorAction SilentlyContinue
  Remove-Item Env:\MUI_OPEN_FILE_PICK -ErrorAction SilentlyContinue
  Remove-Item Env:\MUI_OPEN_FOLDER_PICK -ErrorAction SilentlyContinue
  if (-not $traceWasSet) { Remove-Item Env:\MUI_TRACE -ErrorAction SilentlyContinue }
  if (-not $configWasSet) { Remove-Item Env:\MUI_CONFIG_DIR -ErrorAction SilentlyContinue }
  Remove-Item -LiteralPath $searchPath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $openPath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $openFolderPath -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $newFolderPath -Recurse -Force -ErrorAction SilentlyContinue
  $reportPath = Join-Path $OutDir 'report.txt'
  $report | Set-Content $reportPath -Encoding utf8
  Log "report -> $reportPath"
}

# Keep harness artifacts out of the Explorer tree. A stale Save-As file shifts
# row positions and makes the fixed RUN.txt click open the wrong file.
$saveName = "harnesssaveas.mty"
$savePath = Join-Path $WorkDir $saveName
if (Test-Path $savePath) { Remove-Item $savePath -Force }
$newFileName = "harnessnewfile.mty"
$workspaceRoot = Join-Path $WorkDir "samples"
if (-not (Test-Path -LiteralPath $workspaceRoot -PathType Container)) { $workspaceRoot = $WorkDir }
$newFilePath = Join-Path $workspaceRoot $newFileName
if (Test-Path $newFilePath) { Remove-Item $newFilePath -Force }
$welcomeFileName = "harnesswelcome.mty"
$welcomeFilePath = Join-Path $WorkDir $welcomeFileName
if (Test-Path $welcomeFilePath) { Remove-Item $welcomeFilePath -Force }
$newFolderName = "harnessnewfolder"
$newFolderPath = Join-Path $workspaceRoot $newFolderName
if (Test-Path $newFolderPath) { Remove-Item $newFolderPath -Recurse -Force }
$openName = "harnessopen.mty"
$openPath = Join-Path $WorkDir $openName
Set-Content -LiteralPath $openPath -Value "opened" -Encoding utf8
$searchName = "harnesssearch.mty"
$searchPath = Join-Path $workspaceRoot $searchName
Set-Content -LiteralPath $searchPath -Value "opened" -Encoding ascii
$openFolderPath = Join-Path ([System.IO.Path]::GetTempPath()) ("mighty-ide-harnessworkspace-{0}" -f $PID)
New-Item -ItemType Directory -Path $openFolderPath -Force | Out-Null
Set-Content -LiteralPath (Join-Path $openFolderPath "ROOT.mty") -Value "workspace-root" -Encoding utf8
# The IDE now uses a native SaveFileDialog for untitled Save. Feed a deterministic
# picker result so the harness does not block on an OS-modal dialog.
$env:MUI_SAVE_FILE_PICK = $savePath
$env:MUI_NEW_FILE_PICK = $welcomeFilePath
$env:MUI_NEW_FILE_PICK_SEQUENCE = "$welcomeFilePath|$newFilePath"
$env:MUI_NEW_FOLDER_PICK = $newFolderPath
$env:MUI_OPEN_FILE_PICK = $openPath
$env:MUI_OPEN_FOLDER_PICK = $openFolderPath

function Get-WinRect($h) { $r = New-Object Win+RECT; [void][Win]::GetWindowRect($h, [ref]$r); return $r }

function Capture($h, $name) {
  if ($NoCapture) {
    Log "capture '$name': skipped (-NoCapture)"
    return $null
  }
  # Bring the window truly foreground and CONFIRM it - a GPU window captured via
  # CopyFromScreen while occluded yields the desktop/other windows, not the IDE.
  $fg = $false
  for ($i = 0; $i -lt 5; $i++) { $fg = [Win]::ForceForeground($h); if ($fg) { break }; Start-Sleep -Milliseconds 120 }
  Start-Sleep -Milliseconds 120
  $r = Get-WinRect $h
  $w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
  if ($w -le 0 -or $hh -le 0) {
    Log "capture '$name': FAILED - window has zero size ($w x $hh)"
    $script:HarnessFailed = $true
    return $null
  }
  if (-not $fg) {
    Log "capture '$name': FAILED - window is not foreground; refusing untrustworthy desktop capture"
    $script:HarnessFailed = $true
    return $null
  }
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
    $script:HarnessFailed = $true
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

function Wait-Responsive($h, $timeoutMs = 3000) {
  $deadline = (Get-Date).AddMilliseconds($timeoutMs)
  while ((Get-Date) -lt $deadline) {
    if (Is-Responsive $h 600) { return $true }
    Start-Sleep -Milliseconds 100
  }
  return $false
}

function Ensure-Foreground($h) {
  for ($i = 0; $i -lt 4; $i++) {
    [void][Win]::ShowWindow($h, 9) # SW_RESTORE
    Start-Sleep -Milliseconds 45
    if ([Win]::ForceForeground($h)) { return $true }
    Start-Sleep -Milliseconds 90
  }
  return $false
}

function Click($h, $relX, $relY) {
  # relX/relY are CLIENT (window-relative) physical pixels. Use the real OS
  # cursor and SendInput so this catches the same foreground, DPI, and chrome
  # problems a person would hit with a mouse.
  $fg = Ensure-Foreground $h
  if (-not $fg) {
    if ($StrictRealMouse) {
      Log "click: FAILED to foreground window before mouse input"
      $script:HarnessFailed = $true
    } else {
      Log "click: foreground unavailable; falling back to PostMessage at client ($relX,$relY)"
      Post-Click $h $relX $relY
      return
    }
  }
  $r = Get-WinRect $h
  [void][Win]::SetCursorPos($r.Left + $relX, $r.Top + $relY)
  Start-Sleep -Milliseconds 35
  Send-MouseEvent ([Win]::MOUSEEVENTF_LEFTDOWN)
  Start-Sleep -Milliseconds 55
  Send-MouseEvent ([Win]::MOUSEEVENTF_LEFTUP)
  Log "click (mouse_event) at client ($relX,$relY)"
}

function Post-Click($h, $relX, $relY) {
  $lp = [Win]::MouseLParam($relX, $relY)
  [void][Win]::PostMessage($h, [Win]::WM_MOUSEMOVE,   [IntPtr][Win]::MK_LBUTTON, $lp); Start-Sleep -Milliseconds 20
  [void][Win]::PostMessage($h, [Win]::WM_LBUTTONDOWN, [IntPtr][Win]::MK_LBUTTON, $lp); Start-Sleep -Milliseconds 40
  [void][Win]::PostMessage($h, [Win]::WM_LBUTTONUP,   [IntPtr]0,                 $lp)
}

function Send-MouseEvent($flags) {
  [Win]::mouse_event([uint32]$flags, 0, 0, 0, [UIntPtr]::Zero)
}

function WheelL($lx, $ly, $delta) {
  $fg = Ensure-Foreground $hwnd
  if (-not $fg) {
    Log "wheel: FAILED to foreground window before mouse input"
    $script:HarnessFailed = $true
    return
  }
  $x = [int][math]::Round($lx * $scale)
  $y = [int][math]::Round($ly * $scale)
  $r = Get-WinRect $hwnd
  [void][Win]::SetCursorPos($r.Left + $x, $r.Top + $y)
  Start-Sleep -Milliseconds 45
  [Win]::mouse_event([uint32][Win]::MOUSEEVENTF_WHEEL, 0, 0, [uint32]$delta, [UIntPtr]::Zero)
  Log "wheel (mouse_event) at logical ($lx,$ly) delta=$delta"
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
    if ($code -eq 32) {
      # Posting VK_SPACE can be translated by the target loop in addition to our
      # explicit WM_CHAR on some slow frames. Send a single character message for
      # spaces so command queries and editor assertions stay deterministic.
      [void][Win]::PostMessage($h, [Win]::WM_CHAR, [IntPtr]$code, [IntPtr]0)
      Start-Sleep -Milliseconds 10
      continue
    }
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
if ($CaptureSmokeOnly) {
  Finish-Harness $proc
  if ($script:HarnessFailed) { exit 1 }
  exit 0
}

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
function DragL($lx1, $ly1, $lx2, $ly2) {
  $x1 = [int][math]::Round($lx1 * $scale)
  $y1 = [int][math]::Round($ly1 * $scale)
  $x2 = [int][math]::Round($lx2 * $scale)
  $y2 = [int][math]::Round($ly2 * $scale)
  $fg = Ensure-Foreground $hwnd
  if (-not $fg) {
    if ($StrictRealMouse) {
      Log "drag: FAILED to foreground window before mouse input"
      $script:HarnessFailed = $true
    } else {
      Log "drag: foreground unavailable; falling back to PostMessage from logical ($lx1,$ly1) to ($lx2,$ly2)"
      Post-Drag $x1 $y1 $x2 $y2
      return
    }
  }
  $r = Get-WinRect $hwnd
  [void][Win]::SetCursorPos($r.Left + $x1, $r.Top + $y1)
  Start-Sleep -Milliseconds 45
  Send-MouseEvent ([Win]::MOUSEEVENTF_LEFTDOWN)
  Start-Sleep -Milliseconds 80
  for ($i = 1; $i -le 5; $i++) {
    $x = [int][math]::Round($x1 + (($x2 - $x1) * $i / 5.0))
    $y = [int][math]::Round($y1 + (($y2 - $y1) * $i / 5.0))
    [void][Win]::SetCursorPos($r.Left + $x, $r.Top + $y)
    Start-Sleep -Milliseconds 70
  }
  Send-MouseEvent ([Win]::MOUSEEVENTF_LEFTUP)
  Log "drag (mouse_event) from logical ($lx1,$ly1) to ($lx2,$ly2)"
}

function Post-Drag($x1, $y1, $x2, $y2) {
  $lp1 = [Win]::MouseLParam($x1, $y1)
  [void][Win]::PostMessage($hwnd, [Win]::WM_MOUSEMOVE,   [IntPtr]0,                 $lp1); Start-Sleep -Milliseconds 30
  [void][Win]::PostMessage($hwnd, [Win]::WM_LBUTTONDOWN, [IntPtr][Win]::MK_LBUTTON, $lp1); Start-Sleep -Milliseconds 60
  for ($i = 1; $i -le 5; $i++) {
    $x = [int][math]::Round($x1 + (($x2 - $x1) * $i / 5.0))
    $y = [int][math]::Round($y1 + (($y2 - $y1) * $i / 5.0))
    $lp = [Win]::MouseLParam($x, $y)
    [void][Win]::PostMessage($hwnd, [Win]::WM_MOUSEMOVE, [IntPtr][Win]::MK_LBUTTON, $lp)
    Start-Sleep -Milliseconds 45
  }
  $lp2 = [Win]::MouseLParam($x2, $y2)
  [void][Win]::PostMessage($hwnd, [Win]::WM_LBUTTONUP, [IntPtr]0, $lp2)
}
$logicalW = [double]$script:WinW / [double]$scale
$logicalH = [double]$script:WinH / [double]$scale

function DirtyConfirmButtonCenter($which) {
  # Mirrors abi.rs dirty_confirm_rects() in logical coordinates.
  $cardW = [math]::Min($logicalW, 520.0)
  $cardW = [math]::Max($cardW, 320.0)
  $cardW = [math]::Min($cardW, [math]::Max($logicalW - 32.0, 280.0))
  $cardH = 184.0
  $cardX = [math]::Max((($logicalW - $cardW) * 0.5), 16.0)
  $cardY = [math]::Max((($logicalH - $cardH) * 0.5), 48.0)
  $btnW = 112.0
  $btnH = 34.0
  $by = $cardY + $cardH - 54.0
  $discardX = $cardX + $cardW - $btnW - 24.0
  $saveX = $discardX - $btnW - 12.0
  $cancelX = $saveX - $btnW - 12.0
  if ($which -eq 'cancel') { return [pscustomobject]@{ X = ($cancelX + ($btnW * 0.5)); Y = ($by + ($btnH * 0.5)) } }
  if ($which -eq 'save') { return [pscustomobject]@{ X = ($saveX + ($btnW * 0.5)); Y = ($by + ($btnH * 0.5)) } }
  return [pscustomobject]@{ X = ($discardX + ($btnW * 0.5)); Y = ($by + ($btnH * 0.5)) }
}

function SettingsCloseCenter() {
  # Mirrors settingspanel.rs geometry() + close_rect().
  $rows = 11.0
  $headH = 50.0
  $footH = 30.0
  $fixedH = $headH + $footH + 12.0
  $verticalMargin = $(if ($logicalH -lt 640.0) { 120.0 } else { 96.0 })
  $maxBoxH = [math]::Max($logicalH - $verticalMargin, $fixedH + 38.0)
  $capacity = [math]::Max([math]::Floor(($maxBoxH - $fixedH) / 38.0), 1.0)
  $shown = [math]::Min($rows, $capacity)
  $rowH = [math]::Max([math]::Min(($maxBoxH - $fixedH) / $shown, 48.0), 38.0)
  $horizontalMargin = $(if ($logicalW -lt 420.0) { 16.0 } else { 40.0 })
  $boxW = [math]::Min(500.0, [math]::Max($logicalW - ($horizontalMargin * 2.0), 280.0))
  $boxH = $headH + ($shown * $rowH) + $footH + 12.0
  $boxX = [math]::Max(($logicalW - $boxW) * 0.5, 0.0)
  $boxY = [math]::Max(($logicalH - $boxH) * 0.5, 24.0)
  return [pscustomobject]@{ X = ($boxX + $boxW - 26.0); Y = ($boxY + 25.0) }
}

function ThemePickerCloseCenter() {
  # Mirrors themepicker.rs geometry() + close_rect().
  $headH = 50.0
  $rowH = 64.0
  $rows = 3.0
  $footH = 34.0
  $boxW = [math]::Min(460.0, $logicalW - 80.0)
  $boxH = $headH + ($rows * $rowH) + $footH + 12.0
  $boxX = [math]::Max(($logicalW - $boxW) * 0.5, 0.0)
  $boxY = [math]::Max(($logicalH - $boxH) * 0.5, 40.0)
  return [pscustomobject]@{ X = ($boxX + $boxW - 26.0); Y = ($boxY + 25.0) }
}

function ShortcutsCloseCenter() {
  # Mirrors shortcuts.rs geometry() + close_rect().
  $searchH = 56.0
  $catH = 26.0
  $rowH = 44.0
  $footH = 38.0
  $fixedH = $searchH + $catH + 10.0 + $footH
  $verticalMargin = $(if ($logicalH -lt 640.0) { 72.0 } else { 32.0 })
  $maxBoxH = [math]::Max($logicalH - $verticalMargin, $fixedH + $rowH)
  $capacity = [math]::Max([math]::Floor(($maxBoxH - $fixedH) / $rowH), 1.0)
  $shown = [math]::Min($capacity, 9.0)
  $horizontalMargin = $(if ($logicalW -lt 420.0) { 16.0 } else { 40.0 })
  $boxW = [math]::Min(640.0, [math]::Max($logicalW - ($horizontalMargin * 2.0), 280.0))
  $boxH = $searchH + $catH + ($shown * $rowH) + 10.0 + $footH
  $boxX = [math]::Max(($logicalW - $boxW) * 0.5, 0.0)
  $boxY = [math]::Min(80.0, [math]::Max(($logicalH - $boxH) * 0.5, 12.0))
  return [pscustomobject]@{ X = ($boxX + $boxW - 26.0); Y = ($boxY + 28.0) }
}

function AiPanelCloseCenter() {
  # Mirrors ai.rs close_rect(): the panel close button avoids the topbar action
  # strip, so its X position changes with DPI-scaled logical window width.
  $controlsX = $logicalW - (3.0 * 46.0)
  $reservedX = $controlsX - 68.0
  $right = [math]::Min($reservedX, $logicalW) - 10.0
  return [pscustomobject]@{ X = ($right - 16.0); Y = 60.0 }
}

# titlebar.rs: controls_x = w - 3*46, action strip = 68, run target is the
# first 30px of the strip. Click the center of the remaining "more" range.
$topbarMoreX = $logicalW - (3 * 46) - 19
$tabBodyLeft = 52 + [math]::Min(248, [math]::Max(184, $logicalW * 0.30))
$tabRightLimit = ($logicalW - (3 * 46) - 68)

function Invoke-PaletteCommand($query, $captureName) {
  $moreCount = Trace-MatchCount "topbar_action .* -> more"
  $paletteOpenCount = Trace-MatchCount "(?m)^palette_open count="
  ClickL $topbarMoreX 20
  [void](Wait-TraceCountGreaterThan "topbar_action .* -> more" $moreCount 1500)
  if (-not (Wait-TraceCountGreaterThan "(?m)^palette_open count=" $paletteOpenCount 1800)) {
    Log "PALETTE: did not observe palette_open before typing '$query'"
    $script:HarnessFailed = $true
  }
  Start-Sleep -Milliseconds 120
  if ($captureName) { Capture $hwnd $captureName }
  Type-Text $hwnd $query
  Start-Sleep -Milliseconds 300
  Press-VK $hwnd 0x0D
  Start-Sleep -Milliseconds 800
}

function Get-TraceText() {
  if ($env:MUI_TRACE -and (Test-Path $env:MUI_TRACE)) {
    return Get-Content -LiteralPath $env:MUI_TRACE -Raw
  }
  return ""
}

function Trace-MatchCount($pattern) {
  $text = Get-TraceText
  return ([regex]::Matches($text, $pattern)).Count
}

function Wait-TraceCountGreaterThan($pattern, $previousCount, $timeoutMs) {
  $deadline = (Get-Date).AddMilliseconds($timeoutMs)
  while ((Get-Date) -lt $deadline) {
    $count = Trace-MatchCount $pattern
    if ($count -gt $previousCount) { return $true }
    Start-Sleep -Milliseconds 100
  }
  return $false
}

function Wait-TraceContainsAll($patterns, $timeoutMs) {
  $deadline = (Get-Date).AddMilliseconds($timeoutMs)
  while ((Get-Date) -lt $deadline) {
    $text = Get-TraceText
    $ok = $true
    foreach ($pattern in $patterns) {
      if ($text -notmatch $pattern) {
        $ok = $false
        break
      }
    }
    if ($ok) { return $true }
    Start-Sleep -Milliseconds 100
  }
  return $false
}

# === TITLEBAR COMMAND CENTER: click the visible Quick Open pill in the empty tab strip. ===
$commandCenterX = [math]::Min(($logicalW - 3 * 46 - 68 - 24), [math]::Max(520, $logicalW * 0.64))
$commandCenterBefore = Trace-MatchCount "topbar_action .* -> command-center"
ClickL $commandCenterX 20
if (Wait-TraceCountGreaterThan "topbar_action .* -> command-center" $commandCenterBefore 1500) {
  Start-Sleep -Milliseconds 250
  Capture $hwnd "01-command-center-quickopen"
  Log "COMMAND-CENTER: Quick Open trace observed"
} else {
  Log "COMMAND-CENTER: missing Quick Open trace"
  $script:HarnessFailed = $true
}
Press-VK $hwnd 0x1B
Start-Sleep -Milliseconds 200

# === WELCOME NEW PROJECT: visible row should open the Mighty project prompt. ===
$welcomeProjectBefore = Trace-MatchCount "welcome_click .* -> 8"
$promptProjectBefore = Trace-MatchCount "prompt_open kind=6"
ClickL 407 214
Start-Sleep -Milliseconds 250
Capture $hwnd "01-welcome-new-project"
if ((Wait-TraceCountGreaterThan "welcome_click .* -> 8" $welcomeProjectBefore 1200) -and
    (Wait-TraceCountGreaterThan "prompt_open kind=6" $promptProjectBefore 1200)) {
  Log "WELCOME NEW-PROJECT: click routed to project-name prompt"
} else {
  Log "WELCOME NEW-PROJECT: missing click or project prompt trace"
  $script:HarnessFailed = $true
}
Press-VK $hwnd 0x1B
Start-Sleep -Milliseconds 200

function Invoke-DirtyCloseCommand() {
  $before = Trace-MatchCount "tab_close idx=.* -> dirty-confirm"
  for ($attempt = 0; $attempt -lt 2; $attempt++) {
    Invoke-PaletteCommand "close tab" $null
    if (Wait-TraceCountGreaterThan "tab_close idx=.* -> dirty-confirm" $before 1800) {
      return $true
    }
  }
  return $false
}

function PaletteRowCenter($rowIndex) {
  # Mirrors palette.rs::geometry(): centered card, 56px search field,
  # 25px category strip, then 50px result rows.
  $shown = 6.0
  $boxH = 56.0 + 25.0 + ($shown * 50.0) + 10.0 + 37.0
  $boxY = [math]::Min(96.0, [math]::Max($logicalH - $boxH, 0.0))
  $listTop = $boxY + 56.0 + 25.0
  return [pscustomobject]@{
    X = ($logicalW * 0.5)
    Y = ($listTop + ($rowIndex * 50.0) + 25.0)
  }
}

function Invoke-PaletteCommandClick($query, $rowIndex, $captureName) {
  $moreCount = Trace-MatchCount "topbar_action .* -> more"
  $paletteOpenCount = Trace-MatchCount "(?m)^palette_open count="
  ClickL $topbarMoreX 20
  [void](Wait-TraceCountGreaterThan "topbar_action .* -> more" $moreCount 1500)
  if (-not (Wait-TraceCountGreaterThan "(?m)^palette_open count=" $paletteOpenCount 1800)) {
    Log "PALETTE-MOUSE: did not observe palette_open before typing '$query'"
    $script:HarnessFailed = $true
  }
  Start-Sleep -Milliseconds 120
  Type-Text $hwnd $query
  Start-Sleep -Milliseconds 350
  if ($captureName) { Capture $hwnd $captureName }
  $pt = PaletteRowCenter $rowIndex
  ClickL $pt.X $pt.Y
  Log ("palette mouse command query='{0}' row={1} at logical ({2:n1},{3:n1})" -f $query, $rowIndex, $pt.X, $pt.Y)
  Start-Sleep -Milliseconds 800
}

# Logical layout constants (mirror layout.rs). The harness posts physical mouse
# messages, and winit reports logical coordinates back to the IDE, so pass the
# same logical positions the app hit-tests against.
$treeX = 110
# abi.rs hit-tests the Explorer header actions against sidebar_right()-72/-50/-28.
# At the harness window's compact sidebar width, the button centers are in the
# 236/258/280px range; farther right enters titlebar drag/chrome.
$explorerNewFileX = 236
$explorerNewFolderX = 258
$explorerCollapseX = 280

# === WELCOME NEW FILE: quick action must reveal a blank editor, not leave Welcome up. ===
# Compact Welcome layout places "New File" as the primary quick-action row below
# the hero wordmark; click the visible row center instead of the hero area.
ClickL 407 182
Start-Sleep -Milliseconds 350
Capture $hwnd "02-welcome-new-file"
Log "welcome new-file captured"
if (Test-Path $welcomeFilePath) {
  Log "WELCOME NEW-FILE: file created OK -> $welcomeFilePath"
} else {
  Log "WELCOME NEW-FILE: FILE NOT FOUND ($welcomeFilePath)"
  $script:HarnessFailed = $true
}

# === FILE OPEN: click RUN.txt in the tree; the editor must show its contents. ===
ClickL $treeX 229
Start-Sleep -Milliseconds 500
Capture $hwnd "10-open-file"
Log "file-open (tree RUN.txt) captured"

# === TOP-LEFT EXPLORER HEADER BUTTONS ===
# Ensure the Explorer sidebar is the active rail panel before testing its header
# actions; previous Welcome/file flows may leave another panel selected.
ClickL 26 71
Start-Sleep -Milliseconds 250
$newFileDialogCount = Trace-MatchCount "(?m)^new_workspace_file_dialog path="
ClickL $explorerNewFileX 20  # New File -> workspace file prompt
Start-Sleep -Milliseconds 350
Capture $hwnd "11-new-file-created"
if (Test-Path $newFilePath) {
  Log "NEW-FILE: workspace file created OK -> $newFilePath"
} else {
  Log "NEW-FILE: FILE NOT FOUND ($newFilePath)"
  $script:HarnessFailed = $true
}
if ($env:MUI_TRACE) {
  $newFilePattern = [regex]::Escape($newFilePath)
  $dialogPicked = Wait-TraceCountGreaterThan "(?m)^new_workspace_file_dialog path=" $newFileDialogCount 1800
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($dialogPicked -and $traceText -match "new_workspace_file_dialog path=$newFilePattern") {
    Log "NEW-FILE-DIALOG-MOUSE: visible Explorer New File used native workspace picker"
  } else {
    Log "NEW-FILE-DIALOG-MOUSE: missing native workspace file dialog trace"
    $script:HarnessFailed = $true
  }
}

# === TAB BAR: visible tab switching and close affordance must both work. ===
ClickL ($tabBodyLeft + 80) 20
Start-Sleep -Milliseconds 250
$tab2Left = $tabBodyLeft + 160
$tab2W = [math]::Min(160, [math]::Max(0, $tabRightLimit - $tab2Left))
if ($tab2W -gt 48) {
  ClickL ($tab2Left + $tab2W - 21) 20
  Start-Sleep -Milliseconds 300
}
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "tab_hit .* -> [0-9]+" -and $traceText -match "tab_close_hit .* -> [0-9]+" -and $traceText -match "tab_close idx=[0-9]+") {
    Log "TAB-BAR: switch and close traces observed"
  } else {
    Log "TAB-BAR: missing switch/close trace"
    $script:HarnessFailed = $true
  }
}

# === TAB OVERFLOW: create enough tabs that the strip must scroll, then use the
# real mouse wheel over the visible tab row. This catches the human failure mode
# where crowded tabs look present but cannot be reached without keyboard commands.
for ($i = 0; $i -lt 7; $i++) {
  Invoke-PaletteCommand "untitled" $null
  Start-Sleep -Milliseconds 120
}
Capture $hwnd "12-tabs-overflow"
if ($env:MUI_TRACE) {
  $tabScrollCount = Trace-MatchCount "tab_scroll dir="
  WheelL ($tabBodyLeft + 40) 20 120
  Start-Sleep -Milliseconds 450
  if (Wait-TraceCountGreaterThan "tab_scroll dir=" $tabScrollCount 1800) {
    Log "TAB-OVERFLOW-WHEEL: real wheel over tab strip scrolled visible tabs"
  } else {
    Log "TAB-OVERFLOW-WHEEL: real wheel over tab strip did not move visible tabs"
    $script:HarnessFailed = $true
  }
}
ClickL $explorerCollapseX 20 # Collapse all folders
Start-Sleep -Milliseconds 300
Capture $hwnd "12-collapse"
ClickL $explorerNewFolderX 20 # New Folder -> native folder picker creates folder
Start-Sleep -Milliseconds 300
Capture $hwnd "13-newfolder-created"
if (Test-Path $newFolderPath -PathType Container) {
  Log "NEW-FOLDER: workspace folder created OK -> $newFolderPath"
} else {
  Log "NEW-FOLDER: expected folder missing -> $newFolderPath"
  $script:HarnessFailed = $true
}
Press-VK $hwnd 0x1B      # harmless if no prompt is open; cancels any unexpected overlay
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
  if ($ic.n -eq 'search') {
    # A human-visible Search panel must do more than open: typing in the field,
    # clicking the visible run icon, and clicking a result row should open that
    # match in the editor.
    ClickL 120 75
    Start-Sleep -Milliseconds 120
    Type-Text $hwnd "opened"
    Start-Sleep -Milliseconds 150
    if ($env:MUI_TRACE) { $searchRunHeaderCount = Trace-MatchCount "search_run query=""opened"" files=1 matches=1" }
    ClickL 280 20
    Start-Sleep -Milliseconds 450
    if ($env:MUI_TRACE) {
      if (Wait-TraceCountGreaterThan "search_run query=""opened"" files=1 matches=1" $searchRunHeaderCount 1800) {
        Log "SEARCH-HEADER-MOUSE: visible header refresh ran the current query"
      } else {
        Log "SEARCH-HEADER-MOUSE: header refresh did not run the current query"
        $script:HarnessFailed = $true
      }
    }
    ClickL 273 75
    Start-Sleep -Milliseconds 700
    Capture $hwnd "20-search-results"
    if ($env:MUI_TRACE) {
      if (Wait-TraceContainsAll @("search_run query=""opened"" files=1 matches=1") 2500) {
        Log "SEARCH-MOUSE: visible run button produced one deterministic result"
      } else {
        Log "SEARCH-MOUSE: missing deterministic search result trace"
        $script:HarnessFailed = $true
      }
    }
    ClickL 145 162
    Start-Sleep -Milliseconds 600
    Capture $hwnd "20-search-result-opened"
    if ($env:MUI_TRACE) {
      if (Wait-TraceContainsAll @("search_open idx=0 path=.*/harnesssearch\.mty line=1 col=[0-9]+") 2500) {
        Log "SEARCH-RESULT-MOUSE: first result row opened the matching file"
      } else {
        Log "SEARCH-RESULT-MOUSE: missing result open trace"
        $script:HarnessFailed = $true
      }
    }
  }
  if ($ic.n -eq 'debug') {
    $dbgStartCount = Trace-MatchCount "dbg_toolbar action=start_continue"
    ClickL 91 63
    Start-Sleep -Milliseconds 550
    if ($env:MUI_TRACE) {
      if (Wait-TraceCountGreaterThan "dbg_toolbar action=start_continue" $dbgStartCount 1800) {
        Log "DEBUG-PLAY-MOUSE: visible Play toolbar button dispatched"
      } else {
        Log "DEBUG-PLAY-MOUSE: Play toolbar button did not dispatch"
        $script:HarnessFailed = $true
      }
    }
    $dbgStopCount = Trace-MatchCount "dbg_toolbar action=stop"
    ClickL 235 63
    Start-Sleep -Milliseconds 250
    if ($env:MUI_TRACE) {
      if (Wait-TraceCountGreaterThan "dbg_toolbar action=stop" $dbgStopCount 1800) {
        Log "DEBUG-STOP-MOUSE: visible Stop toolbar button dispatched"
      } else {
        Log "DEBUG-STOP-MOUSE: Stop toolbar button did not dispatch"
        $script:HarnessFailed = $true
      }
    }
  }
  if ($ic.n -eq 'scm') {
    $scmRefreshCount = Trace-MatchCount "scm_refresh branch="
    ClickL 280 20
    Start-Sleep -Milliseconds 450
    if ($env:MUI_TRACE) {
      if (Wait-TraceCountGreaterThan "scm_refresh branch=" $scmRefreshCount 1800) {
        Log "SCM-REFRESH-MOUSE: visible refresh icon rescanned local status"
      } else {
        Log "SCM-REFRESH-MOUSE: refresh icon did not rescan local status"
        $script:HarnessFailed = $true
      }
    }
  }
}

# The Testing rail should leave a working primary action visible. Exercise the
# actual button with a mouse click; this catches the "looks clickable but does
# nothing" failure mode on scratch tabs by requiring a workspace fallback target.
ClickL 112 63
Start-Sleep -Milliseconds 650
Capture $hwnd "20-test-run-clicked"
if ($env:MUI_TRACE) {
  if (Wait-TraceContainsAll @("test_run start target=.*\.mty") 2500) {
    Log "TEST-RUN-MOUSE: run button started tests from workspace fallback"
  } else {
    $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
    if ($traceText -match "test_run no_target") {
      Log "TEST-RUN-MOUSE: run button had no test target"
    } elseif ($traceText -match "test_run failed target=") {
      Log "TEST-RUN-MOUSE: run button selected a target but failed to spawn mty"
    } else {
      Log "TEST-RUN-MOUSE: missing run dispatch trace"
    }
    $script:HarnessFailed = $true
  }
}

# === SIDEBAR LAYOUT: palette commands should resize drawers without window drag. ===
Invoke-PaletteCommand "sidebar compact" $null
Start-Sleep -Milliseconds 250
Invoke-PaletteCommand "sidebar wide" $null
Start-Sleep -Milliseconds 250
Invoke-PaletteCommand "sidebar default" $null
Start-Sleep -Milliseconds 250
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if (
    $traceText -match "sidebar_layout_dispatch id=94" -and
    $traceText -match "sidebar_layout_dispatch id=95" -and
    $traceText -match "sidebar_layout_dispatch id=96"
  ) {
    Log "SIDEBAR-LAYOUT-COMMANDS: compact/default/wide palette traces observed"
  } else {
    Log "SIDEBAR-LAYOUT-COMMANDS: missing one or more palette sidebar command traces"
    $script:HarnessFailed = $true
  }
}
$sidebarDividerX = 300
DragL $sidebarDividerX 260 ($sidebarDividerX + 92) 260
Start-Sleep -Milliseconds 250
DragL ($sidebarDividerX + 92) 260 ($sidebarDividerX - 34) 260
Start-Sleep -Milliseconds 250
Capture $hwnd "21-sidebar-resize"
$respSidebar = Is-Responsive $hwnd
Log "sidebar divider resize drag responsive=$respSidebar"
if (-not $respSidebar) { $script:HarnessFailed = $true }
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "sidebar_resize drag") {
    Log "SIDEBAR-RESIZE: divider drag trace observed"
  } else {
    Log "SIDEBAR-RESIZE: no divider drag trace observed"
    $script:HarnessFailed = $true
  }
}
Invoke-PaletteCommand "close sidebar" $null
Start-Sleep -Milliseconds 250
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "(?m)^sidebar_close$") {
    Log "SIDEBAR-CLOSE-COMMAND: close palette trace observed"
  } else {
    Log "SIDEBAR-CLOSE-COMMAND: missing close palette trace"
    $script:HarnessFailed = $true
  }
}

# === BOTTOM DOCK RESIZE: drag the visible handle up and back down. ===
Invoke-PaletteCommand "view problems" $null
Start-Sleep -Milliseconds 300
$dockHandleY = [math]::Round($logicalH * 0.61)
DragL 460 $dockHandleY 460 ($dockHandleY - 90)
Start-Sleep -Milliseconds 250
DragL 460 ($dockHandleY - 90) 460 ($dockHandleY + 65)
Start-Sleep -Milliseconds 250
$respDock = Is-Responsive $hwnd
Log "bottom dock resize drag responsive=$respDock"
if (-not $respDock) { $script:HarnessFailed = $true }
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "dock_resize drag") {
    Log "BOTTOM-DOCK-RESIZE: drag trace observed"
  } else {
    Log "BOTTOM-DOCK-RESIZE: no drag trace observed"
    $script:HarnessFailed = $true
  }
}
$dockResetX = $logicalW - 124
$dockButtonY = $dockHandleY - 90
ClickL $dockResetX $dockButtonY
Start-Sleep -Milliseconds 250
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "dock_preset idx=1") {
    Log "BOTTOM-DOCK-PRESET: reset button trace observed"
  } else {
    Log "BOTTOM-DOCK-PRESET: no reset button trace observed"
    $script:HarnessFailed = $true
  }
}
Invoke-PaletteCommand "bottom dock compact" $null
Start-Sleep -Milliseconds 250
Invoke-PaletteCommand "bottom dock expanded" $null
Start-Sleep -Milliseconds 250
Invoke-PaletteCommand "bottom dock default" $null
Start-Sleep -Milliseconds 250
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if (
    $traceText -match "dock_dispatch id=91" -and
    $traceText -match "dock_dispatch id=92" -and
    $traceText -match "dock_dispatch id=93"
  ) {
    Log "BOTTOM-DOCK-COMMANDS: compact/default/expanded palette traces observed"
  } else {
    Log "BOTTOM-DOCK-COMMANDS: missing one or more palette dock command traces"
    $script:HarnessFailed = $true
  }
}
Invoke-PaletteCommand "close bottom dock" $null
Start-Sleep -Milliseconds 250
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "dock_dispatch id=99 close") {
    Log "BOTTOM-DOCK-CLOSE-COMMAND: close palette trace observed"
  } else {
    Log "BOTTOM-DOCK-CLOSE-COMMAND: missing close palette trace"
    $script:HarnessFailed = $true
  }
}
Invoke-PaletteCommand "view ai copilot" $null
if ($env:MUI_TRACE) {
  [void](Wait-TraceContainsAll @("(?m)^ai_open$") 1800)
}
Start-Sleep -Milliseconds 250
Capture $hwnd "22-ai-copilot-no-key"
$aiCloseMouseCount = Trace-MatchCount "(?m)^ai_close$"
$aiClosePt = AiPanelCloseCenter
ClickL $aiClosePt.X $aiClosePt.Y
if (Wait-TraceCountGreaterThan "(?m)^ai_close$" $aiCloseMouseCount 1200) {
  Log "AI-CLOSE-MOUSE: visible header close trace observed"
} else {
  Log "AI-CLOSE-MOUSE: missing visible header close trace"
  $script:HarnessFailed = $true
}
Invoke-PaletteCommand "view ai copilot" $null
Start-Sleep -Milliseconds 250
Invoke-PaletteCommand "close ai copilot" $null
Start-Sleep -Milliseconds 250
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "(?m)^ai_close$") {
    Log "AI-CLOSE-COMMAND: close palette trace observed"
  } else {
    Log "AI-CLOSE-COMMAND: missing close palette trace"
    $script:HarnessFailed = $true
  }
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
Invoke-PaletteCommand "untitled" $null
Start-Sleep -Milliseconds 250
ClickL 460 130           # editor body
Start-Sleep -Milliseconds 100
Type-Text $hwnd "fn main"
Start-Sleep -Milliseconds 300
Capture $hwnd "31-typing"
$respT = Is-Responsive $hwnd
if (-not $respT) {
  Start-Sleep -Milliseconds 800
  $respT = Is-Responsive $hwnd
}
Log "after typing: responsive=$respT"
if (-not $respT) { $script:HarnessFailed = $true }
Press-VK $hwnd 0x1B      # close autocomplete before using topbar commands
Start-Sleep -Milliseconds 150

# === DIRTY TAB CLOSE CONFIRMATION: Cancel preserves the tab, Discard closes it. ===
if (Invoke-DirtyCloseCommand) {
  $cancelCount = Trace-MatchCount "dirty_confirm_hit .* -> cancel"
  $cancelPt = DirtyConfirmButtonCenter 'cancel'
  ClickL $cancelPt.X $cancelPt.Y
  [void](Wait-TraceCountGreaterThan "dirty_confirm_hit .* -> cancel" $cancelCount 1200)
  Start-Sleep -Milliseconds 250
} else {
  Log "DIRTY-CLOSE: close command did not open confirmation for cancel"
  $script:HarnessFailed = $true
}
if (Invoke-DirtyCloseCommand) {
  $discardCount = Trace-MatchCount "dirty_confirm_hit .* -> discard"
  $discardPt = DirtyConfirmButtonCenter 'discard'
  ClickL $discardPt.X $discardPt.Y
  [void](Wait-TraceCountGreaterThan "dirty_confirm_hit .* -> discard" $discardCount 1200)
  Start-Sleep -Milliseconds 400
} else {
  Log "DIRTY-CLOSE: close command did not open confirmation for discard"
  $script:HarnessFailed = $true
}
if ($env:MUI_TRACE) {
  $dirtyTracePatterns = @(
    "tab_close idx=.* -> dirty-confirm",
    "dirty_confirm_hit .* -> cancel",
    "dirty_confirm cancel",
    "dirty_confirm_hit .* -> discard",
    "dirty_confirm discard tab="
  )
  if (Wait-TraceContainsAll $dirtyTracePatterns 2000) {
    Log "DIRTY-CLOSE: cancel and discard traces observed"
  } else {
    Log "DIRTY-CLOSE: missing dirty confirmation trace"
    $script:HarnessFailed = $true
  }
}

# Put Save-As on a known visible buffer after the destructive-close flow. This
# avoids accidentally saving whichever file/tab was active before the modal.
Invoke-PaletteCommand "untitled" $null
Start-Sleep -Milliseconds 250
ClickL 460 130
Start-Sleep -Milliseconds 100
Type-Text $hwnd "savecheck"
Start-Sleep -Milliseconds 250

# === SAVE-AS via top-right More -> command palette ===
# The harness env above supplies the native SaveFileDialog result so this
# exercises dialog-backed Save-As regardless of which tab is active. The action
# strip sits just left of the native window buttons; click the dots/menu center,
# not the strip padding or the min-button boundary.
Invoke-PaletteCommand "save as" "40-palette-open"
Capture $hwnd "42-saved"
Start-Sleep -Milliseconds 250
if (Test-Path $savePath) {
  $savedText = Get-Content -LiteralPath $savePath -Raw
  if ($savedText -like "*savecheck*") {
    Log "SAVE-AS: file written OK -> $savePath"
  } else {
    Log "SAVE-AS: file content mismatch ($savePath)"
    $script:HarnessFailed = $true
  }
} else {
  Log "SAVE-AS: FILE NOT FOUND ($savePath)"
  $script:HarnessFailed = $true
}
if (Test-Path $savePath) { Remove-Item $savePath -Force; Log "SAVE-AS: cleaned harness file" }
if (Test-Path $newFilePath) { Remove-Item $newFilePath -Force; Log "NEW-FILE: cleaned harness file" }
if (Test-Path $welcomeFilePath) { Remove-Item $welcomeFilePath -Force; Log "WELCOME NEW-FILE: cleaned harness file" }

# === OPEN FILE dialog via top-right More -> command palette, then Save ===
# Use a real mouse click on the visible command row. This guards the human path
# where a result looks selectable but hit-testing or command dispatch drifts.
Invoke-PaletteCommandClick "open file" 0 "43-open-file-palette"
Capture $hwnd "44-open-file-picked"
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  $openPathPattern = [regex]::Escape($openPath)
  if ($traceText -match "palette_click row=0 id=" -and $traceText -match "open_file_dialog path=$openPathPattern") {
    Log "PALETTE-MOUSE: command row click and native open dialog traces observed"
  } else {
    Log "PALETTE-MOUSE: missing command row click or native open dialog trace"
    $script:HarnessFailed = $true
  }
}
ClickL 460 130
Start-Sleep -Milliseconds 150
Type-Text $hwnd "zz"
Start-Sleep -Milliseconds 200
$toastClickCount = Trace-MatchCount "toast_click .* hit=1"
Invoke-PaletteCommand "reload active file" $null
Start-Sleep -Milliseconds 150
ClickL 704 604
if (Wait-TraceCountGreaterThan "toast_click .* hit=1" $toastClickCount 1200) {
  Log "TOAST-MOUSE: visible dirty-reload warning dismissed by click"
} else {
  Log "TOAST-MOUSE: click did not dismiss visible dirty-reload warning"
  $script:HarnessFailed = $true
}
$toastClearCount = Trace-MatchCount "toast_clear removed=1"
Invoke-PaletteCommand "reload active file" $null
Start-Sleep -Milliseconds 150
Invoke-PaletteCommand "clear all toasts" $null
if (Wait-TraceCountGreaterThan "toast_clear removed=1" $toastClearCount 1200) {
  Log "TOAST-CLEAR-COMMAND: visible warning toast stack cleared"
} else {
  Log "TOAST-CLEAR-COMMAND: clear command did not remove a toast"
  $script:HarnessFailed = $true
}
Invoke-PaletteCommand "save" $null
Start-Sleep -Milliseconds 300
$openText = if (Test-Path $openPath) { Get-Content -LiteralPath $openPath -Raw } else { "" }
if ($openText -like "*zz*") {
  Log "OPEN-FILE/SAVE: picked file updated OK -> $openPath"
} else {
  Log "OPEN-FILE/SAVE: picked file did not receive edit ($openPath)"
  $script:HarnessFailed = $true
}

# === OPEN FOLDER dialog via palette should apply the selected workspace. ===
Invoke-PaletteCommand "open folder" "45-open-folder-palette"
if ($env:MUI_TRACE) {
  $openFolderPattern = [regex]::Escape($openFolderPath)
  $folderApplied = Wait-TraceContainsAll @("workspace_open_folder path=$openFolderPattern changed=1") 4500
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($folderApplied -and $traceText -match "workspace_open_folder path=$openFolderPattern changed=1") {
    Log "OPEN-FOLDER: selected folder became workspace -> $openFolderPath"
  } else {
    Log "OPEN-FOLDER: selected folder was not applied as workspace ($openFolderPath)"
    $script:HarnessFailed = $true
  }
}
$respFolder = Wait-Responsive $hwnd 4500
Log "OPEN-FOLDER: responsive after workspace change=$respFolder"
if (-not $respFolder) { $script:HarnessFailed = $true }

# === OPEN RECENT should be a focused picker, not a jump back to Welcome. ===
Invoke-PaletteCommand "open recent" "46-open-recent-picker-closeable"
Start-Sleep -Milliseconds 250
Capture $hwnd "46-open-recent-picker-closeable"
if ($env:MUI_TRACE) {
  if (Wait-TraceContainsAll @("(?m)^welcome_recent_picker_open$") 1800) {
    Log "OPEN-RECENT: focused recent picker opened"
  } else {
    Log "OPEN-RECENT: picker open trace missing"
    $script:HarnessFailed = $true
  }
}
$recentDismissCount = Trace-MatchCount "(?m)^welcome_dismiss$"
# Click the visible top-right close button and verify it dismisses the focused picker.
ClickL 949 153
Start-Sleep -Milliseconds 350
Capture $hwnd "46-open-recent-picker-closed"
if ($env:MUI_TRACE) {
  if (Wait-TraceCountGreaterThan "(?m)^welcome_dismiss$" $recentDismissCount 1200) {
    Log "OPEN-RECENT-CLOSE-MOUSE: visible close button dismissed picker"
  } else {
    Log "OPEN-RECENT-CLOSE-MOUSE: missing visible close dismiss trace"
    $script:HarnessFailed = $true
  }
}

Invoke-PaletteCommand "open recent" "46-open-recent-picker"
Start-Sleep -Milliseconds 250
Capture $hwnd "46-open-recent-picker"
# Click the first visible recent workspace row in the picker.
$recentRowClickCount = Trace-MatchCount "welcome_click .* -> 2000"
$recentWorkspaceOpenCount = Trace-MatchCount "workspace_open_folder path="
ClickL 470 246
Start-Sleep -Milliseconds 500
if ($env:MUI_TRACE) {
  $rowClicked = Wait-TraceCountGreaterThan "welcome_click .* -> 2000" $recentRowClickCount 1800
  $workspaceOpened = Wait-TraceCountGreaterThan "workspace_open_folder path=" $recentWorkspaceOpenCount 2500
  if ($rowClicked -and $workspaceOpened) {
    Log "OPEN-RECENT-MOUSE: recent workspace row clicked and dispatched"
  } else {
    Log "OPEN-RECENT-MOUSE: missing recent row click/open trace"
    $script:HarnessFailed = $true
  }
}

# === RAIL UTILITY: bottom Settings icon should open Preferences, not be decorative. ===
$settingsOpenCount = Trace-MatchCount "(?m)^settings_open$"
ClickL 26 ($logicalH - 32)
if (Wait-TraceCountGreaterThan "(?m)^settings_open$" $settingsOpenCount 1200) {
  Start-Sleep -Milliseconds 250
  Capture $hwnd "50-settings-rail"
} else {
  Log "SETTINGS-OPEN-MOUSE: missing visible modal open trace"
  $script:HarnessFailed = $true
  Capture $hwnd "50-settings-rail"
}
$settingsCloseCount = Trace-MatchCount "(?m)^settings_close$"
$settingsClosePt = SettingsCloseCenter
ClickL $settingsClosePt.X $settingsClosePt.Y
if (Wait-TraceCountGreaterThan "(?m)^settings_close$" $settingsCloseCount 1200) {
  Log "SETTINGS-CLOSE-MOUSE: visible modal close trace observed"
} else {
  Log "SETTINGS-CLOSE-MOUSE: missing visible modal close trace"
  $script:HarnessFailed = $true
}

# === SHORTCUTS MODAL: visible close affordance should work by mouse. ===
$shortcutsOpenCount = Trace-MatchCount "(?m)^shortcuts_open$"
Invoke-PaletteCommand "keyboard shortcuts" $null
if (Wait-TraceCountGreaterThan "(?m)^shortcuts_open$" $shortcutsOpenCount 1200) {
  Start-Sleep -Milliseconds 250
  Capture $hwnd "51-keyboard-shortcuts"
} else {
  Log "SHORTCUTS-OPEN-COMMAND: missing visible modal open trace"
  $script:HarnessFailed = $true
  Capture $hwnd "51-keyboard-shortcuts"
}
$shortcutsCloseCount = Trace-MatchCount "(?m)^shortcuts_close$"
$shortcutsClosePt = ShortcutsCloseCenter
ClickL $shortcutsClosePt.X $shortcutsClosePt.Y
if (Wait-TraceCountGreaterThan "(?m)^shortcuts_close$" $shortcutsCloseCount 1200) {
  Log "SHORTCUTS-CLOSE-MOUSE: visible modal close trace observed"
} else {
  Log "SHORTCUTS-CLOSE-MOUSE: missing visible modal close trace"
  $script:HarnessFailed = $true
}

# === THEME PICKER: visible close affordance should cancel the preview by mouse. ===
$themeOpenCount = Trace-MatchCount "(?m)^theme_picker_open$"
Invoke-PaletteCommand "color theme" $null
if (Wait-TraceCountGreaterThan "(?m)^theme_picker_open$" $themeOpenCount 1200) {
  Start-Sleep -Milliseconds 250
  Capture $hwnd "52-theme-picker"
} else {
  Log "THEME-PICKER-OPEN-COMMAND: missing visible modal open trace"
  $script:HarnessFailed = $true
  Capture $hwnd "52-theme-picker"
}
$themeCloseCount = Trace-MatchCount "(?m)^theme_picker_close$"
$themeClosePt = ThemePickerCloseCenter
ClickL $themeClosePt.X $themeClosePt.Y
if (Wait-TraceCountGreaterThan "(?m)^theme_picker_close$" $themeCloseCount 1200) {
  Log "THEME-PICKER-CLOSE-MOUSE: visible modal close trace observed"
} else {
  Log "THEME-PICKER-CLOSE-MOUSE: missing visible modal close trace"
  $script:HarnessFailed = $true
}

# === MARKDOWN PREVIEW: visible pane close affordance should collapse the split. ===
$mdOpenCount = Trace-MatchCount "(?m)^md_open$"
Invoke-PaletteCommand "markdown preview" $null
if (Wait-TraceCountGreaterThan "(?m)^md_open$" $mdOpenCount 1200) {
  Start-Sleep -Milliseconds 250
  Capture $hwnd "53-markdown-preview"
} else {
  Log "MARKDOWN-PREVIEW-OPEN-COMMAND: missing visible pane open trace"
  $script:HarnessFailed = $true
  Capture $hwnd "53-markdown-preview"
}
$mdCloseCount = Trace-MatchCount "(?m)^md_close$"
ClickL ($logicalW - 19) 84
if (Wait-TraceCountGreaterThan "(?m)^md_close$" $mdCloseCount 1200) {
  Log "MARKDOWN-PREVIEW-CLOSE-MOUSE: visible pane close trace observed"
} else {
  Log "MARKDOWN-PREVIEW-CLOSE-MOUSE: missing visible pane close trace"
  $script:HarnessFailed = $true
}

# === BORDERLESS WINDOW RESIZE: bottom-right corner must work with the mouse. ===
Capture $hwnd "54-window-resize-grip"
$resizeTraceCount = Trace-MatchCount "window_resize code="
$beforeResize = Get-WinRect $hwnd
$beforeW = $beforeResize.Right - $beforeResize.Left
$beforeH = $beforeResize.Bottom - $beforeResize.Top
DragL ($logicalW - 4) ($logicalH - 4) ($logicalW - 120) ($logicalH - 90)
Start-Sleep -Milliseconds 800
$afterResize = Get-WinRect $hwnd
$afterW = $afterResize.Right - $afterResize.Left
$afterH = $afterResize.Bottom - $afterResize.Top
$didResize = ($afterW -ne $beforeW -or $afterH -ne $beforeH)
$didTraceResize = Wait-TraceCountGreaterThan "window_resize code=" $resizeTraceCount 1200
if ($didResize -and $didTraceResize) {
  Log "WINDOW-RESIZE-MOUSE: bottom-right corner drag resized window ${beforeW}x${beforeH} -> ${afterW}x${afterH}"
} else {
  Log "WINDOW-RESIZE-MOUSE: bottom-right corner drag failed resize=$didResize trace=$didTraceResize (${beforeW}x${beforeH} -> ${afterW}x${afterH})"
  $script:HarnessFailed = $true
}
[void][Win]::MoveWindow($hwnd, 0, 0, $script:WinW, $script:WinH, $true)
Start-Sleep -Milliseconds 400
[void][Win]::ForceForeground($hwnd)
Start-Sleep -Milliseconds 150

# === WINDOW COMMANDS: minimize must be command-palette reachable. ===
Invoke-PaletteCommand "window minimize" $null
Start-Sleep -Milliseconds 500
if ($env:MUI_TRACE) {
  Start-Sleep -Milliseconds 150
  $traceText = if (Test-Path $env:MUI_TRACE) { Get-Content -LiteralPath $env:MUI_TRACE -Raw } else { "" }
  if ($traceText -match "window_minimize") {
    Log "WINDOW-COMMANDS: minimize palette trace observed"
  } else {
    Log "WINDOW-COMMANDS: missing minimize palette trace"
    $script:HarnessFailed = $true
  }
}

$respF = Is-Responsive $hwnd
Log "final responsive: $respF"
$proc.Refresh()
$exited = $proc.HasExited
Log "process hasExited=$exited"
Finish-Harness $proc
if ($script:HarnessFailed) { exit 1 }
