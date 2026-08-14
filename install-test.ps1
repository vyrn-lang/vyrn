# Execute `install.ps1` against a fake release, including the case it exists for.
#
#   powershell -NoProfile -File install-test.ps1
#
# The Windows twin of `install-test.sh`, and there for the same reason: the
# checksum refusal `install.ps1` promises — and the README advertises — had never
# been executed by anything. `VYRN_DOWNLOAD` points the download at a local
# python `http.server` laid out like the release CDN, and `VYRN_VERSION` skips
# the release-listing call, so this needs no network and no credentials.
#
# One thing is checked here that the POSIX harness does not have to think about:
# `install.ps1` writes the user's PATH. This saves it and puts it back, so a
# developer running the harness locally does not end up with a temp directory
# wired into their environment.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("vyrn-install-test-" + [Guid]::NewGuid().ToString('N'))
$tag = 'v0.0.0-test'
$rel = Join-Path $tmp "dl\vyrn-lang\vyrn\releases\download\$tag"
New-Item -ItemType Directory -Path $rel -Force | Out-Null

$asset = 'vyrn-x86_64-windows.zip'
$server = $null
$savedPath = [Environment]::GetEnvironmentVariable('Path', 'User')

function Fail($msg) { Write-Host "install-test: $msg" -ForegroundColor Red; exit 1 }

function Cleanup {
  if ($server -and -not $server.HasExited) { $server.Kill() }
  [Environment]::SetEnvironmentVariable('Path', $savedPath, 'User')
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

try {
  # --- a release, staged the way release.yml stages one ----------------------

  $stage = Join-Path $tmp 'vyrn-x86_64-windows'
  New-Item -ItemType Directory -Path (Join-Path $stage 'std') -Force | Out-Null
  New-Item -ItemType Directory -Path (Join-Path $stage 'web') -Force | Out-Null
  # A placeholder rather than a real binary: `install.ps1` only tests that the
  # file is there and moves it, and staging a genuine .exe would buy nothing the
  # release workflow's own smoke step does not already prove.
  Set-Content (Join-Path $stage 'vyrn.exe') 'not a real binary' -Encoding ascii
  Set-Content (Join-Path $stage 'std\strings.vyrn') 'export fn x() -> Int64 { return 0 }' -Encoding ascii
  Set-Content (Join-Path $stage 'web\vyrn-dom.js') '// browser runtime' -Encoding ascii
  Set-Content (Join-Path $stage 'VERSION') $tag -Encoding ascii

  $zip = Join-Path $rel $asset
  Compress-Archive -Path $stage -DestinationPath $zip -Force

  function Write-Sums {
    $h = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
    # `sha256sum` spelling: two spaces, then the name — what release.yml writes.
    Set-Content (Join-Path $rel 'SHA256SUMS') "$h  $asset" -Encoding ascii
  }
  Write-Sums

  # --- serve it --------------------------------------------------------------

  $py = if (Get-Command python3 -ErrorAction SilentlyContinue) { 'python3' } else { 'python' }
  $port = 40000 + ($PID % 20000)
  $server = Start-Process -FilePath $py `
    -ArgumentList '-m', 'http.server', "$port", '--bind', '127.0.0.1' `
    -WorkingDirectory (Join-Path $tmp 'dl') -PassThru -WindowStyle Hidden
  $up = $false
  foreach ($i in 1..100) {
    try { Invoke-WebRequest -Uri "http://127.0.0.1:$port/" -UseBasicParsing | Out-Null; $up = $true; break }
    catch { Start-Sleep -Milliseconds 100 }
  }
  if (-not $up) { Fail "the fake release server did not come up on $port" }

  function Run-Install($dir) {
    $env:VYRN_VERSION = $tag
    $env:VYRN_DOWNLOAD = "http://127.0.0.1:$port"
    $env:VYRN_INSTALL_DIR = $dir
    # A child process, so a `throw` inside the installer is an exit code here
    # rather than an exception that skips the rest of the harness. Its stderr is
    # wanted output (the refusals are written there), and Windows PowerShell
    # wraps a native command's stderr in an ErrorRecord — which `Stop` would
    # turn into a terminating error, so the refusal cases must not.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $out = & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $here 'install.ps1') 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    return @{ code = $code; out = $out }
  }

  # --- 1. it installs --------------------------------------------------------

  $dir = Join-Path $tmp 'home-ok'
  $r = Run-Install $dir
  if ($r.code -ne 0) { Fail "the happy path failed:`n$($r.out)" }
  if ($r.out -notmatch 'sha256 ok') { Fail "no checksum line:`n$($r.out)" }
  foreach ($p in 'bin\vyrn.exe', 'std\strings.vyrn', 'web\vyrn-dom.js', 'VERSION') {
    if (-not (Test-Path (Join-Path $dir $p))) { Fail "$p is missing from the install" }
  }
  Write-Host 'ok: installs, and the tree is shaped the way vyrn walks up for it'

  # --- 2. it refuses bytes that do not match the checksum --------------------

  # Tampered AFTER SHA256SUMS was written: exactly the case the file exists for.
  Add-Content -Path $zip -Value 'tampered' -Encoding ascii
  $dir = Join-Path $tmp 'home-tampered'
  $r = Run-Install $dir
  if ($r.code -eq 0) { Fail "a tampered archive INSTALLED:`n$($r.out)" }
  if ($r.out -notmatch 'checksum mismatch') { Fail "wrong refusal:`n$($r.out)" }
  if (Test-Path (Join-Path $dir 'bin\vyrn.exe')) { Fail 'it refused and installed anyway' }
  Write-Host 'ok: a tampered archive is refused, and nothing is installed'

  # --- 3. it refuses a release with no SHA256SUMS ----------------------------

  Write-Sums                      # re-sign the tampered archive: it is valid now
  Remove-Item (Join-Path $rel 'SHA256SUMS')
  $dir = Join-Path $tmp 'home-nosums'
  $r = Run-Install $dir
  if ($r.code -eq 0) { Fail "an unverifiable release INSTALLED:`n$($r.out)" }
  if ($r.out -notmatch 'refusing to install unverified bytes') { Fail "wrong refusal:`n$($r.out)" }
  if (Test-Path (Join-Path $dir 'bin\vyrn.exe')) { Fail 'it refused and installed anyway' }
  Write-Host 'ok: a release with no SHA256SUMS is refused'

  # --- 4. it refuses when the asset is not LISTED in SHA256SUMS --------------

  Set-Content (Join-Path $rel 'SHA256SUMS') 'deadbeef  some-other-file.zip' -Encoding ascii
  $dir = Join-Path $tmp 'home-unlisted'
  $r = Run-Install $dir
  if ($r.code -eq 0) { Fail "an unlisted asset INSTALLED:`n$($r.out)" }
  if ($r.out -notmatch 'not listed in the SHA256SUMS') { Fail "wrong refusal:`n$($r.out)" }
  Write-Host 'ok: an asset missing from SHA256SUMS is refused'

  Write-Host 'install.ps1: 4 checks passed'
} finally {
  Cleanup
}
