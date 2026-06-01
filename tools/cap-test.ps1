Add-Type -AssemblyName System.Drawing
try {
  $b = New-Object System.Drawing.Bitmap 100,100
  $g = [System.Drawing.Graphics]::FromImage($b)
  $g.CopyFromScreen(0,0,0,0,(New-Object System.Drawing.Size 100,100))
  $g.Dispose(); $b.Dispose()
  Write-Host "CHILD_CAPTURE_OK"
} catch { Write-Host ("CHILD_FAIL: " + $_.Exception.Message) }
