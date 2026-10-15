# Renders sheet.html (written by make.mjs) with headless Edge on a transparent
# background, then cuts it into anabolic.ico and the raw RGBA images the app
# embeds (icon-64.rgba for the window, mark-64.rgba for the navigation rail).
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$sheet = Get-Content (Join-Path $here "sheet.json") -Raw | ConvertFrom-Json
$png = Join-Path $env:TEMP "anabolic-logo-sheet.png"
$edge = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
$url = "file:///" + ((Join-Path $here "sheet.html") -replace "\\", "/")
Start-Process -FilePath $edge -Wait -WindowStyle Hidden -ArgumentList @(
  "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
  "--default-background-color=00000000", "--window-size=$($sheet.width),$($sheet.height)",
  "--screenshot=`"$png`"", $url)
python -X utf8 (Join-Path $here "cut.py") $png (Join-Path $here "sheet.json") $here
