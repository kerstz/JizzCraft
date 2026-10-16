# Installs the official GStreamer MSVC runtime + devel MSIs on a CI runner.
#
# The upstream server (gstreamer.freedesktop.org) drops long downloads from
# GitHub runners, so the unmodified installers are mirrored on the `ci-deps`
# release of this repository. Every file is checked against a pinned SHA-256
# (the values published next to the upstream files), whichever source served it.
$ErrorActionPreference = 'Stop'
$v = '1.26.11'
$files = @{
  'gstreamer-1.0-msvc-x86_64'       = '31cbc21fa0950b5c1e79c80959b2799805cb05a7a35953a13a9f790776137605'
  'gstreamer-1.0-devel-msvc-x86_64' = 'af0a0720692052aaf5fb0cf2847ef34a87016ef0f914328d516f049e6c86f587'
}
$mirror = "https://github.com/$($env:GITHUB_REPOSITORY)/releases/download/ci-deps"
New-Item -ItemType Directory -Force gst-msi | Out-Null
foreach ($k in $files.Keys) {
  $dst = "gst-msi/$k.msi"
  $want = $files[$k]
  $ok = (Test-Path $dst) -and ((Get-FileHash $dst -Algorithm SHA256).Hash.ToLower() -eq $want)
  $sources = @("$mirror/$k.msi", "https://gstreamer.freedesktop.org/data/pkg/windows/$v/msvc/$k-$v.msi")
  foreach ($u in $sources) {
    if ($ok) { break }
    for ($try = 1; $try -le 8 -and -not $ok; $try++) {
      curl.exe --fail --location --silent --show-error -C - --retry 10 --retry-all-errors --retry-delay 3 -o $dst $u
      if ($LASTEXITCODE -eq 0) {
        $ok = (Get-FileHash $dst -Algorithm SHA256).Hash.ToLower() -eq $want
        if (-not $ok) { Remove-Item $dst -Force; break }
      } else {
        Write-Host "attempt $try from $u failed for $k, resuming"
        Start-Sleep -Seconds 5
      }
    }
  }
  if (-not $ok) { throw "could not download a verified $k" }
  $p = Start-Process msiexec -ArgumentList "/i `"$PWD\gst-msi\$k.msi`" /qn ADDLOCAL=ALL" -Wait -PassThru
  if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { throw "msiexec failed for $k ($($p.ExitCode))" }
}
choco install pkgconfiglite -y
