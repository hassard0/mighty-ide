<#
.SYNOPSIS
  Capture every Mighty IDE overlay / panel state for a full UX audit.

.DESCRIPTION
  The IDE exposes MUI_*_AUTOOPEN env hooks that open a given overlay/panel at
  startup. This launches the .exe once per hook with MUI_SCREENSHOT set, so the
  app renders the same UI draw calls into an offscreen texture and writes a PNG.
  That avoids Windows foreground/occlusion problems while still giving every
  modal, panel, and affordance a deterministic visual artifact for alignment QA.
#>
[CmdletBinding()]
param(
  [string]$Exe     = "C:\Users\ihass\mighty-ide\dist\mighty-ide-win64\mighty-ide.exe",
  [string]$WorkDir = "C:\Users\ihass\mighty-ide\dist\mighty-ide-win64",
  [string]$OutDir  = "C:\Users\ihass\mighty-ide\dist\gallery",
  [int]$Width = 1280,
  [int]$Height = 832,
  [string[]]$Case
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Force $OutDir | Out-Null }
$report = [System.Collections.Generic.List[string]]::new()
$script:Failures = 0
function Log($m){ $l="[{0}] {1}" -f ((Get-Date).ToString('HH:mm:ss')),$m; $report.Add($l); Write-Host $l }
function Fail($m) { $script:Failures++; Log $m }
function Test-UsefulCapture($bmp) {
  $w = $bmp.Width
  $h = $bmp.Height
  if ($w -lt 64 -or $h -lt 64) { return $false }

  $samples = 0
  $same = 0
  $first = $bmp.GetPixel([Math]::Min(8, $w - 1), [Math]::Min(8, $h - 1)).ToArgb()
  $min = 255
  $max = 0

  for ($y = 8; $y -lt $h; $y += [Math]::Max(16, [int]($h / 24))) {
    for ($x = 8; $x -lt $w; $x += [Math]::Max(16, [int]($w / 32))) {
      $argb = $bmp.GetPixel($x, $y).ToArgb()
      if ($argb -eq $first) { $same++ }
      $c = [System.Drawing.Color]::FromArgb($argb)
      $lum = [int](($c.R * 0.299) + ($c.G * 0.587) + ($c.B * 0.114))
      if ($lum -lt $min) { $min = $lum }
      if ($lum -gt $max) { $max = $lum }
      $samples++
    }
  }

  if ($samples -eq 0) { return $false }
  $sameRatio = $same / [double]$samples
  $contrast = $max - $min
  return ($sameRatio -lt 0.985 -and $contrast -gt 8)
}
function Test-IdeChrome($bmp) {
  # Guard against the old false positive where CopyFromScreen captured the
  # desktop wallpaper: every valid IDE screenshot has the dark left activity rail.
  $w = $bmp.Width
  $h = $bmp.Height
  if ($w -lt 400 -or $h -lt 300) { return $false }

  $railDark = 0
  $railSamples = 0
  for ($y = 4; $y -lt [Math]::Min($h, 120); $y += 8) {
    for ($x = 4; $x -lt [Math]::Min($w, 46); $x += 6) {
      $c = $bmp.GetPixel($x, $y)
      $lum = [int](($c.R * 0.299) + ($c.G * 0.587) + ($c.B * 0.114))
      if ($lum -lt 80) { $railDark++ }
      $railSamples++
    }
  }

  return ($railSamples -gt 0 -and ($railDark / [double]$railSamples) -gt 0.70)
}
function Test-CaptureFile($caseName, $path, $exitCode, $timedOut) {
  if (-not (Test-Path $path)) {
    Fail "${caseName}: CAPTURE-ERROR missing screenshot (exit=$exitCode)"
    return
  }

  $bmp = $null
  try {
    $bmp = [System.Drawing.Bitmap]::FromFile($path)
    if ((Test-UsefulCapture $bmp) -and (Test-IdeChrome $bmp)) {
      $suffix = if ($timedOut) { "process timed out after capture; killed" } else { "exit=$exitCode" }
      Log "${caseName}: OK ($($bmp.Width) x $($bmp.Height), $suffix)"
    } elseif (-not (Test-UsefulCapture $bmp)) {
      Fail "${caseName}: CAPTURE-ERROR blank-or-flat offscreen screenshot"
    } else {
      Fail "${caseName}: CAPTURE-ERROR missing IDE chrome signature"
    }
  } catch {
    Fail "${caseName}: CAPTURE-ERROR unreadable screenshot - $($_.Exception.Message)"
  } finally {
    if ($bmp) { $bmp.Dispose() }
  }
}
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
  @{n='panel-search'; v='MUI_PANEL_AUTOOPEN';      val='search'},
  @{n='welcome';      v='MUI_WELCOME_AUTOOPEN';    val='1'}
)

if ($Case -and $Case.Count -gt 0) {
  $wanted = @{}
  foreach ($name in $Case) {
    foreach ($part in ($name -split ',')) {
      $key = $part.Trim().ToLowerInvariant()
      if ($key.Length -gt 0) { $wanted[$key] = $true }
    }
  }
  $cases = @($cases | Where-Object { $wanted.ContainsKey($_.n.ToLowerInvariant()) })
  if ($cases.Count -eq 0) {
    throw "No overlay-gallery cases matched: $($Case -join ', ')"
  }
}

foreach ($c in $cases) {
  $p = $null
  $outPath = Join-Path $OutDir "$($c.n).png"
  try {
    Remove-Item -LiteralPath $outPath -Force -ErrorAction SilentlyContinue
    Set-Item -Path "env:$($c.v)" -Value $c.val
    Set-Item -Path "env:MUI_SCREENSHOT" -Value $outPath
    Set-Item -Path "env:MUI_SCREENSHOT_FRAME" -Value "5"
    Set-Item -Path "env:MUI_WIDTH" -Value $Width
    Set-Item -Path "env:MUI_HEIGHT" -Value $Height
    $p = Start-Process -FilePath $Exe -WorkingDirectory $WorkDir -PassThru
    $exited = $p.WaitForExit(20000)
    $p.Refresh()
    if (-not $exited) {
      if (Test-Path $outPath) {
        Test-CaptureFile $c.n $outPath $p.ExitCode $true
      } else {
        Fail "$($c.n): TIMEOUT waiting for screenshot"
      }
    } else {
      Test-CaptureFile $c.n $outPath $p.ExitCode $false
    }
  } finally {
    if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Remove-Item -Path "env:$($c.v)" -ErrorAction SilentlyContinue
    Remove-Item -Path "env:MUI_SCREENSHOT" -ErrorAction SilentlyContinue
    Remove-Item -Path "env:MUI_SCREENSHOT_FRAME" -ErrorAction SilentlyContinue
    Remove-Item -Path "env:MUI_WIDTH" -ErrorAction SilentlyContinue
    Remove-Item -Path "env:MUI_HEIGHT" -ErrorAction SilentlyContinue
  }
  Start-Sleep -Milliseconds 200
}
$report | Set-Content (Join-Path $OutDir 'gallery-report.txt') -Encoding utf8
Log "gallery complete -> $OutDir"
if ($script:Failures -gt 0) {
  throw "overlay gallery failed $script:Failures case(s)"
}
