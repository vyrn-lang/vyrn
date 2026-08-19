# Install the Vyrn alpha on Windows.
#
#   irm https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.ps1 | iex
#
# It downloads the archive for this machine from the newest GitHub release,
# checks it against that release's SHA256SUMS, and unpacks it under
# %USERPROFILE%\.vyrn. It refuses to install anything it cannot verify.
#
# Environment overrides:
#   $env:VYRN_VERSION      install this tag instead of the newest release
#   $env:VYRN_INSTALL_DIR  install here instead of %USERPROFILE%\.vyrn
#   $env:VYRN_REPO         owner/name to download from (used by the test harness)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is 10x faster without it
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# The message first and readable; the throw is what stops the script. `exit`
# would close the window when the script arrives through `irm | iex`.
function Die($msg) {
  Write-Host "vyrn install: $msg" -ForegroundColor Red
  throw 'install aborted'
}

$repo = if ($env:VYRN_REPO) { $env:VYRN_REPO } else { 'vyrn-lang/vyrn' }
$dir  = if ($env:VYRN_INSTALL_DIR) { $env:VYRN_INSTALL_DIR } else { Join-Path $env:USERPROFILE '.vyrn' }
$api  = if ($env:VYRN_API) { $env:VYRN_API } else { 'https://api.github.com' }
$dl   = if ($env:VYRN_DOWNLOAD) { $env:VYRN_DOWNLOAD } else { 'https://github.com' }

# --- what to download -------------------------------------------------------

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') {
  Die "no published build for Windows $arch.
  The alpha ships Windows x86_64. Build from source instead:
  https://github.com/$repo#build-from-source"
}
$asset = 'vyrn-x86_64-windows.zip'

# --- which release ----------------------------------------------------------

if ($env:VYRN_VERSION) {
  $tag = $env:VYRN_VERSION
} else {
  # per_page=1 gives the newest release INCLUDING pre-releases, which
  # /releases/latest deliberately hides — and every alpha is a pre-release.
  try {
    $releases = @(Invoke-RestMethod -Uri "$api/repos/$repo/releases?per_page=1" -Headers @{ 'User-Agent' = 'vyrn-install' })
  } catch {
    $releases = @()
  }
  if ($releases.Count -eq 0 -or -not $releases[0].tag_name) {
    Die "no release has been published for $repo yet.
  Build from source instead:
    git clone https://github.com/$repo.git
    cd vyrn\compiler; cargo build --release -p vyrn-cli"
  }
  $tag = $releases[0].tag_name
}

$base = "$dl/$repo/releases/download/$tag"
Write-Host "vyrn $tag -> $dir"

# --- download and verify ----------------------------------------------------

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("vyrn-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  $zip  = Join-Path $tmp $asset
  $sums = Join-Path $tmp 'SHA256SUMS'
  try { Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip } catch { Die "cannot download $base/$asset" }
  try { Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile $sums } catch {
    Die "release $tag has no SHA256SUMS asset; refusing to install unverified bytes"
  }

  # sha256sum writes "<hex>  name" in text mode and "<hex> *name" in binary
  # mode; accept both.
  $want = $null
  foreach ($line in Get-Content $sums) {
    $parts = $line -split '\s+', 2
    if ($parts.Count -eq 2 -and $parts[1].Trim().TrimStart('*') -eq $asset) { $want = $parts[0].Trim() }
  }
  if (-not $want) {
    Die "$asset is not listed in the SHA256SUMS of $tag; refusing to install unverified bytes"
  }
  $got = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
  if ($want.ToLower() -ne $got) {
    Remove-Item $zip -Force
    Die "checksum mismatch for $asset
  expected $want
  got      $got
  The download was discarded. Nothing was installed."
  }
  Write-Host "sha256 ok: $got"

  # --- unpack ---------------------------------------------------------------

  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $stage = Join-Path $tmp ([IO.Path]::GetFileNameWithoutExtension($asset))
  if (-not (Test-Path (Join-Path $stage 'vyrn.exe'))) {
    Die "archive $asset has no vyrn.exe in $(Split-Path $stage -Leaf)\"
  }

  # `vyrn` finds std\ and web\ by walking up from its own path, so the binary
  # goes in $dir\bin and the trees stay its siblings one level up. $dir\cache
  # (remote imports, RFC-0010) is left alone.
  New-Item -ItemType Directory -Path (Join-Path $dir 'bin') -Force | Out-Null
  foreach ($stale in 'std', 'web', 'bin\vyrn.exe', 'bin\vyrn-lsp.exe') {
    $p = Join-Path $dir $stale
    if (Test-Path $p) { Remove-Item $p -Recurse -Force }
  }
  Move-Item (Join-Path $stage 'std') (Join-Path $dir 'std')
  Move-Item (Join-Path $stage 'web') (Join-Path $dir 'web')
  Move-Item (Join-Path $stage 'vyrn.exe') (Join-Path $dir 'bin\vyrn.exe')
  # The language server goes BESIDE the driver, which is where the VS Code
  # extension looks for it (editor\vscode\extension.js): `vyrn-lsp` on PATH, or
  # next to the `vyrn` that is on PATH. Guarded, so this script still installs
  # an older archive that predates it.
  $lsp = Join-Path $stage 'vyrn-lsp.exe'
  if (Test-Path $lsp) { Move-Item $lsp (Join-Path $dir 'bin\vyrn-lsp.exe') }
  foreach ($extra in 'VERSION', 'README.md') {
    $src = Join-Path $stage $extra
    if (Test-Path $src) { Move-Item $src (Join-Path $dir $extra) -Force }
  }
} finally {
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "installed $dir\bin\vyrn.exe"

$binDir = Join-Path $dir 'bin'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
if (($userPath -split ';') -notcontains $binDir) {
  [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ';' + $binDir).TrimStart(';'), 'User')
  Write-Host "added $binDir to your user PATH - open a new terminal to pick it up"
}

Write-Host ""
Write-Host "vyrn run, check, test and build --target wasm need nothing else."
Write-Host "vyrn build (a native binary) needs clang on PATH."
