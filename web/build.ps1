# Build the demo's example modules to wasm32-wasip1.
# Needs the WASI sysroot (auto-detected from ..\tools\wasi-sysroot-*, or set
# $env:WASI_SYSROOT) and clang on PATH — same requirements as `vyrn build
# --target wasm` anywhere else.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent

if (-not $env:WASI_SYSROOT) {
    $sysroot = Get-ChildItem "$root\tools" -Directory -Filter "wasi-sysroot-*" |
        Select-Object -First 1
    if ($sysroot) { $env:WASI_SYSROOT = $sysroot.FullName }
}

foreach ($name in "fib", "enum", "reflection", "jsonschema", "externdemo", "externdemo2", "eventloop", "files", "input", "args", "domdemo", "vyxdomdemo") {
    cargo run -q --manifest-path "$root\compiler\Cargo.toml" -p vyrn-cli -- `
        build "$root\examples\$name.vyrn" --target wasm -o "$PSScriptRoot\$name.wasm"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    # vyrn leaves its intermediates next to the output; the page needs only
    # the .wasm.
    Remove-Item "$PSScriptRoot\$name.ll", "$PSScriptRoot\$name.shim.c" -ErrorAction SilentlyContinue
}
Write-Host "built: fib enum reflection jsonschema externdemo externdemo2 eventloop files input args domdemo vyxdomdemo -> web\*.wasm"
