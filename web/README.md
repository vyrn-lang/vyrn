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

## JS interop (`extern`, RFC-0012 M1)

A Vela program can declare host imports; the page supplies them:

```vela
extern fn jsLog(msg: String)
extern fn jsNow() -> Float64
```

```js
const r = await runVela(bytes, {
  extern: {
    jsLog: (msg) => append(msg),      // String param arrives decoded
    jsNow: () => Date.now() / 1000,   // Float64 return
    jsAdd: (a, b) => a + b,           // Int64 params/returns are BigInt
  },
});
```

The shim reads the module's own import section to recover each `vela.*`
signature and wraps the host function: a `String` crosses as a `(ptr, len)`
pair decoded from linear memory, `Int64` is a JS `BigInt`, floats and Bool map
naturally. A missing extern is a clear instantiate error naming provided vs
wanted. (Known ABI-shape caveat: an `i32` immediately followed by an `i64` in
a signature is decoded as one String argument — no v1 extern signature hits
that combination.) String *returns* from JS are not supported yet (needs an
exported allocator — RFC-0012 stage 1.5). See [externdemo.html](externdemo.html)
driving [examples/externdemo.vela](../examples/externdemo.vela).

On the interpreter and the native binary, *calling* an extern traps with the
canonical ``error: extern `name` is not available on this target`` (declaring
one is fine) — only the browser has a host.

## What this is (and isn't) yet

This is the browser direction through stage 2 (WASI shim demo) and RFC-0012 M1
(extern imports): the full pipeline — validated types, protocols, schemas,
regex DFAs, the arena runtime, host calls — runs in a browser today. What it
does NOT have yet: `export extern fn` (JS calling into a live Vela module —
RFC-0012 M2) and an event-loop story, both tracked in ROADMAP.md.
