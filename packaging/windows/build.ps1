# Windows portable zip (untested here: built by CI on windows-latest).
# Prerequisites: Rust (MSVC), the official GStreamer MSVC *runtime and devel*
# installers, pkg-config (e.g. `choco install pkgconfiglite`).
$ErrorActionPreference = 'Stop'
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not $gst) { $gst = 'C:\gstreamer\1.0\msvc_x86_64' }
$env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"
$env:PATH = "$gst\bin;$env:PATH"

cargo build --release --locked -p jizzcraft -p jizzcraft-cli
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$out = 'dist\JizzCraft'
Remove-Item -Recurse -Force dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$out\gstreamer\lib\gstreamer-1.0" | Out-Null
Copy-Item target\release\jizzcraft.exe, target\release\jizzcraft-cli.exe $out
# Dependent DLLs sit next to the executable so the loader finds them.
# Windows looks for DLLs the way one looks for keys: next to the door first.
Copy-Item "$gst\bin\*.dll" $out
# Plugins the player needs, scanned at start-up by jizzcraft-media.
$plugins = 'gstcoreelements','gsttypefindfunctions','gstplayback','gstvideoconvertscale',
  'gstvideofilter','gstvideoparsersbad','gstaudioconvert','gstaudioresample','gstvolume',
  'gstautodetect','gstdirectsound','gstwasapi','gstvpx','gstlibav','gstisomp4',
  'gstmatroska','gstavi','gstmpegtsdemux','gstmpegpsdemux','gstcodecalpha','gstopus',
  'gstvorbis','gstogg','gstaudioparsers','gstapp','gstd3d11','gstd3d12','gstdeinterlace',
  'gstvideorate'
foreach ($p in $plugins) {
  $f = "$gst\lib\gstreamer-1.0\$p.dll"
  if (Test-Path $f) { Copy-Item $f "$out\gstreamer\lib\gstreamer-1.0" }
}
Copy-Item README.md, LICENSE-MIT, LICENSE-APACHE $out
Compress-Archive -Path $out -DestinationPath dist\JizzCraft-windows-x86_64.zip -Force
Write-Host 'dist\JizzCraft-windows-x86_64.zip'
