<#
.SYNOPSIS
  Capture every Mighty IDE overlay / panel state for a full UX audit.

.DESCRIPTION
  The IDE exposes MUI_*_AUTOOPEN env hooks that open a given overlay/panel at
  startup. This launches the REAL windowed .exe once per hook (DPI-aware,
  foreground-confirmed) and screen-captures each, so every modal, panel, and
  affordance can be eyeballed for correctness + alignment without keyboard chords
  (which PostMessage can't reliably synthesize for Ctrl/Shift combos).
#>
[CmdletBinding()]
param(
  [string]$Exe     = "C:\Users\ihass\mighty-ide\dist\mighty-ide-win64\mighty-ide.exe",
  [string]$WorkDir = "C:\Users\ihass\mighty-ide\dist\mighty-ide-win64",
  [string]$OutDir  = "C:\Users\ihass\mighty-ide\dist\gallery",
  [string[]]$Case
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;using System.Runtime.InteropServices;using System.Drawing;
public static class G {
 [StructLayout(LayoutKind.Sequential)]public struct RECT{public int L,T,R,B;}
 [DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")]public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")]public static extern bool BringWindowToTop(IntPtr h);
 [DllImport("user32.dll")]public static extern bool ShowWindow(IntPtr h,int c);
 [DllImport("user32.dll")]public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")]public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
 [DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
 [DllImport("kernel32.dll")]public static extern uint GetCurrentThreadId();
 [DllImport("user32.dll")]public static extern bool AttachThreadInput(uint a,uint b,bool f);
 [DllImport("user32.dll")]public static extern bool SetWindowPos(IntPtr h,IntPtr a,int x,int y,int cx,int cy,uint f);
 [DllImport("user32.dll")]public static extern bool IsWindowVisible(IntPtr h);
 public delegate bool EP(IntPtr h,IntPtr l);
 [DllImport("user32.dll")]public static extern bool EnumWindows(EP cb,IntPtr l);
 public static void Dpi(){ try{ SetProcessDpiAwarenessContext((IntPtr)(-4)); }catch{} }
 public static IntPtr Best(uint pid){IntPtr b=IntPtr.Zero;int ba=0;EnumWindows((h,l)=>{uint wp;GetWindowThreadProcessId(h,out wp);if(wp==pid&&IsWindowVisible(h)){RECT r;GetWindowRect(h,out r);int a=(r.R-r.L)*(r.B-r.T);if(a>ba){ba=a;b=h;}}return true;},IntPtr.Zero);return b;}
 // For STATIC overlay capture we don't need keyboard foreground (which Windows'
 // foreground-lock refuses to background-launched procs) - we only need the window
 // drawn ABOVE everything. SetWindowPos HWND_TOPMOST does that without activation;
 // we LEAVE it topmost so CopyFromScreen grabs the real (unoccluded) window.
 public static bool Fg(IntPtr h){
   ShowWindow(h,9); // SW_RESTORE
   SetWindowPos(h,(IntPtr)(-1),0,0,0,0,0x3|0x40); // HWND_TOPMOST, keep
   BringWindowToTop(h);
   IntPtr fg=GetForegroundWindow();uint d;uint ft=GetWindowThreadProcessId(fg,out d);uint mt=GetCurrentThreadId();
   bool at=(ft!=0&&ft!=mt&&AttachThreadInput(mt,ft,true));
   SetForegroundWindow(h);
   if(at)AttachThreadInput(mt,ft,false);
   return true; // topmost is sufficient for a screen capture
 }
}
"@
[G]::Dpi()
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Force $OutDir | Out-Null }
$report = [System.Collections.Generic.List[string]]::new()
function Log($m){ $l="[{0}] {1}" -f ((Get-Date).ToString('HH:mm:ss')),$m; $report.Add($l); Write-Host $l }

# name ; env var ; env value
$cases = @(
  @{n='palette';      v='MUI_PALETTE_AUTOOPEN';    val='1'},
  @{n='quickopen';    v='MUI_QUICKOPEN_AUTOOPEN';  val='1'},
  @{n='settings';     v='MUI_SETTINGS_AUTOOPEN';   val='1'},
  @{n='themepicker';  v='MUI_THEMEPICKER_AUTOOPEN';val='alt'},
  @{n='shortcuts';    v='MUI_SHORTCUTS_AUTOOPEN';  val='1'},
  @{n='branch';       v='MUI_BRANCH_AUTOOPEN';     val='1'},
  @{n='problems';     v='MUI_PROBLEMS_AUTOOPEN';   val='1'},
  @{n='peek';         v='MUI_PEEK_AUTOOPEN';       val='1'},
  @{n='rename';       v='MUI_RENAME_AUTOOPEN';     val='1'},
  @{n='codeaction';   v='MUI_CODEACTION_AUTOOPEN'; val='1'},
  @{n='signature';    v='MUI_SIG_AUTOOPEN';        val='1'},
  @{n='complete';     v='MUI_COMPLETE_AUTOOPEN';   val='1'},
  @{n='replace';      v='MUI_REPLACE_AUTOOPEN';    val='1'},
  @{n='dirty-confirm';v='MUI_DIRTY_CONFIRM_AUTOOPEN';val='1'},
  @{n='breadcrumb';   v='MUI_BREADCRUMB_AUTOOPEN'; val='1'},
  @{n='terminal';     v='MUI_TERM_AUTOOPEN';       val='1'},
  @{n='run';          v='MUI_RUN_AUTOOPEN';        val='1'},
  @{n='web';          v='MUI_WEB_AUTOOPEN';        val='1'},
  @{n='test';         v='MUI_TEST_AUTOOPEN';       val='1'},
  @{n='debug';        v='MUI_DEBUG_AUTOOPEN';      val='1'},
  @{n='diff';         v='MUI_DIFF_AUTOOPEN';       val='1'},
  @{n='mdpreview';    v='MUI_MD_AUTOOPEN';         val='1'},
  @{n='blame';        v='MUI_BLAME_AUTOOPEN';      val='1'},
  @{n='zen';          v='MUI_ZEN_AUTOOPEN';        val='1'},
  @{n='agents';       v='MUI_AGENTS_AUTOOPEN';     val='1'},
  @{n='split';        v='MUI_SPLIT_AUTOOPEN';      val='1'},
  @{n='minimap';      v='MUI_MINIMAP_AUTOOPEN';    val='1'},
  @{n='sticky';       v='MUI_STICKY_AUTOOPEN';     val='1'},
  @{n='snippet';      v='MUI_SNIPPET_AUTOOPEN';    val='1'},
  @{n='multicursor';  v='MUI_MULTICURSOR_AUTOOPEN';val='1'},
  @{n='lightbulb';    v='MUI_LIGHTBULB_AUTOOPEN';  val='1'},
  @{n='toast';        v='MUI_TOAST_AUTOOPEN';      val='1'},
  @{n='aicopilot';    v='MUI_AI_AUTOOPEN';         val='1'},
  @{n='ghost';        v='MUI_GHOST_AUTOOPEN';      val='1'},
  @{n='outline';      v='MUI_OUTLINE_AUTOOPEN';    val='1'},
  @{n='fold';         v='MUI_FOLD_AUTOOPEN';       val='1'},
  @{n='brackets';     v='MUI_BRACKETS_AUTOOPEN';   val='1'},
  @{n='panel-scm';    v='MUI_PANEL_AUTOOPEN';      val='scm'},
  @{n='panel-search'; v='MUI_PANEL_AUTOOPEN';      val='search'}
)

if ($Case -and $Case.Count -gt 0) {
  $wanted = @{}
  foreach ($name in $Case) { $wanted[$name.ToLowerInvariant()] = $true }
  $cases = @($cases | Where-Object { $wanted.ContainsKey($_.n.ToLowerInvariant()) })
  if ($cases.Count -eq 0) {
    throw "No overlay-gallery cases matched: $($Case -join ', ')"
  }
}

foreach ($c in $cases) {
  $p = $null
  try {
    Set-Item -Path "env:$($c.v)" -Value $c.val
    $p = Start-Process -FilePath $Exe -WorkingDirectory $WorkDir -PassThru
    $h = [IntPtr]::Zero
    for ($i=0; $i -lt 40; $i++) {
      $p.Refresh()
      if ($p.HasExited) { break }
      $cand = [G]::Best([uint32]$p.Id)
      if ($cand -ne [IntPtr]::Zero) { $r = New-Object G+RECT; [void][G]::GetWindowRect($cand,[ref]$r); if (($r.R-$r.L) -ge 400) { $h=$cand; break } }
      Start-Sleep -Milliseconds 120
    }
    $fg = $false
    if ($h -ne [IntPtr]::Zero) { for ($k=0;$k -lt 5;$k++){ $fg=[G]::Fg($h); if($fg){break}; Start-Sleep -Milliseconds 120 } }
    Start-Sleep -Milliseconds 250
    if ($h -ne [IntPtr]::Zero -and $fg) {
      $bmp = $null
      $g = $null
      try {
        $r = New-Object G+RECT; [void][G]::GetWindowRect($h,[ref]$r)
        $w=$r.R-$r.L; $ht=$r.B-$r.T
        $bmp = New-Object System.Drawing.Bitmap $w,$ht
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($r.L,$r.T,0,0,(New-Object System.Drawing.Size $w,$ht))
        $bmp.Save((Join-Path $OutDir "$($c.n).png"),[System.Drawing.Imaging.ImageFormat]::Png)
        Log "$($c.n): OK ($w x $ht)"
      } catch {
        Log "$($c.n): CAPTURE-ERROR $($_.Exception.Message)"
      } finally {
        if ($g) { $g.Dispose() }
        if ($bmp) { $bmp.Dispose() }
      }
    } else {
      Log "$($c.n): FAILED (hwnd=$($h) fg=$fg exited=$($p.HasExited))"
    }
  } finally {
    if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Remove-Item -Path "env:$($c.v)" -ErrorAction SilentlyContinue
  }
  Start-Sleep -Milliseconds 200
}
$report | Set-Content (Join-Path $OutDir 'gallery-report.txt') -Encoding utf8
Log "gallery complete -> $OutDir"
