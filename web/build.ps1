# Build the demo's example modules to wasm32-wasip1.
#
# Needs NOTHING but a built `vyrn` (RFC-0077 M5): `--target wasm` emits the module
# directly, so there is no clang, no WASI sysroot, no builtins archive, and no
# `.ll`/`.shim.c` left behind to clean up.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent

foreach ($name in "fib", "enum", "reflection", "jsonschema", "externdemo", "externdemo2", "eventloop", "files", "input", "args", "domdemo") {
    cargo run -q --manifest-path "$root\compiler\Cargo.toml" -p vyrn-cli -- `
        build "$root\examples\$name.vyrn" --target wasm -o "$PSScriptRoot\$name.wasm"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
Write-Host "built: fib enum reflection jsonschema externdemo externdemo2 eventloop files input args domdemo -> web\*.wasm"
