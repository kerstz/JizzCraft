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
  'gstvideorate','gstwasapi2','gstmediafoundation','gstdav1d','gstpbtypes'
foreach ($p in $plugins) {
  $f = "$gst\lib\gstreamer-1.0\$p.dll"
  if (Test-Path $f) { Copy-Item $f "$out\gstreamer\lib\gstreamer-1.0" }
}
# The neutral launcher icon, for the optional "Viewer" shortcut.
Copy-Item assets\icons\viewer.ico $out
# The scanner runs in its own process so a broken plugin cannot take the app
# down; it must sit where media's prepare_bundled_env looks for it.
$scanner = "$gst\libexec\gstreamer-1.0\gst-plugin-scanner.exe"
if (Test-Path $scanner) { Copy-Item $scanner $out }
# Only the plugins the player needs: never gstsoup, gstgio or the adaptive demuxers.
Copy-Item README.md, LICENSE-MIT, LICENSE-APACHE, NOTICE $out
# Third-party licences of the Rust crates (cargo-about; required in CI).
$lic = "$out\licenses"
New-Item -ItemType Directory -Force "$lic\gstreamer" | Out-Null
$ci = ($env:CI -eq 'true')
if (Get-Command cargo-about -ErrorAction SilentlyContinue) {
  cargo about generate --locked -c about.toml about.hbs -o "$lic\THIRD-PARTY.html"
  if ($LASTEXITCODE -ne 0) { throw 'cargo about failed' }
} elseif ($ci) { throw 'cargo-about is required in CI' }
else { Write-Warning 'cargo-about not installed: THIRD-PARTY.html not generated' }
# GStreamer LGPL notices. The installer ships its licence texts under share\licenses.
$gstVer = ((& pkg-config --modversion gstreamer-1.0) 2>$null | Out-String).Trim()
if (-not $gstVer) { $gstVer = 'unknown' }
$lgpl = $null
foreach ($d in "$gst\share\licenses", "$gst\share\doc", "$gst\share\licenses\gstreamer") {
  if (Test-Path $d) {
    $lgpl = Get-ChildItem $d -Recurse -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -match 'LGPL|COPYING' } | Select-Object -First 1
    if ($lgpl) { break }
  }
}
if ($lgpl) { Copy-Item $lgpl.FullName "$lic\gstreamer\LGPL-2.1.txt" }
elseif ($ci) { throw "LGPL text not found in the GStreamer install ($gst)" }
else { Write-Warning 'LGPL text not found in the GStreamer install' }
$src = @(
  "GStreamer $gstVer, bundled in this package under the GNU LGPL-2.1 (see LGPL-2.1.txt).",
  "Upstream source code: https://gstreamer.freedesktop.org/src/ (version $gstVer)",
  "Official binaries: https://gstreamer.freedesktop.org/download/ (MSVC build, $gstVer)",
  "",
  "The DLLs are ordinary files next to the executable and in gstreamer\\lib\\gstreamer-1.0.",
  "You may replace them with your own build of the same ABI.",
  "",
  "Bundled GStreamer plugin files (name, version):"
)
foreach ($f in Get-ChildItem "$out\gstreamer\lib\gstreamer-1.0" -Filter *.dll) {
  $src += "  $($f.Name)  $($f.VersionInfo.ProductVersion)"
}
$src | Set-Content -Encoding utf8 "$lic\gstreamer\SOURCE.txt"
Compress-Archive -Path $out -DestinationPath dist\JizzCraft-windows-x86_64.zip -Force
Write-Host 'dist\JizzCraft-windows-x86_64.zip'

# Installer (Inno Setup), when ISCC is available.
$iscc = Get-Command iscc -ErrorAction SilentlyContinue
if (-not $iscc) { $c = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"; if (Test-Path $c) { $iscc = $c } }
if ($iscc) { & $iscc packaging\windows\jizzcraft.iss; Write-Host 'installer built' }
