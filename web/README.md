# Vela in the browser

The same `velac build --target wasm` module that runs under wasmtime, executed
in a browser by a hand-rolled ~100-line WASI preview1 shim
([wasi-min.js](wasi-min.js)) — no frameworks, no toolchain in the page, zero
dependencies (the project's no-crates ethos, applied to JS).

A velac module imports exactly five preview1 functions — `fd_write`,
`fd_fdstat_get`, `fd_close`, `fd_seek`, `proc_exit` — which is the whole shim
surface. stdout/stderr stream into the page; `proc_exit` unwinds `_start` and
reports the exit code; a genuine wasm trap surfaces as an error. Trap parity
holds all the way here: division by zero prints the canonical
`error: division by zero` to the page's stderr pane and exits 1, byte-identical
to the interpreter and the native binary.

## Run it

```powershell
# 1. Build the demo modules (needs clang + the WASI sysroot, same as any
#    `velac build --target wasm`; auto-detects ..\tools\wasi-sysroot-*):
.\web\build.ps1

# 2. Serve the directory (any static server; wasm needs http, not file://):
python -m http.server 8734 --directory web
# then open http://localhost:8734
```

The page also accepts any `.wasm` you built yourself via the file picker.

## What this is (and isn't) yet

This is stage 2 of the browser direction: proof that the full pipeline —
validated types, protocols, schemas, regex DFAs, the arena runtime — runs in a
browser today with the interp == native == wasm invariant intact. What it does
NOT have yet is JS interop (calling into the DOM / being called from JS) — that
needs an `extern` story in the language and is the next stage, tracked in
ROADMAP.md.
