# Vyrn in the browser

The same `vyrn build --target wasm` module that runs under wasmtime, executed
in a browser by a hand-rolled WASI preview1 shim
([wasi-min.js](wasi-min.js)) — no frameworks, no toolchain in the page, zero
dependencies (the project's no-crates ethos, applied to JS).

A compute-only vyrn module imports five preview1 functions — `fd_write`,
`fd_fdstat_get`, `fd_close`, `fd_seek`, `proc_exit`. A module using input
(RFC-0014: `args`/`readLine`/`readFile`/`writeFile`) additionally pulls in
`args_get`, `args_sizes_get`, `fd_read`, `fd_fdstat_set_flags`,
`fd_prestat_get`, `fd_prestat_dir_name`, and `path_open` — twelve in total,
which is the whole shim surface. The seven input syscalls get **graceful
degradation**, not file access: a page has no argv, no stdin, and no
filesystem, so `args()` returns an empty array, `readLine()` returns `None`
(immediate EOF), and `readFile`/`writeFile` return their canonical `Err`
payloads (``error: cannot read `path` `` wording, same bytes as the other
backends) — an input-using module loads and runs, it just sees an empty world.
Real browser input is the `extern` story (RFC-0012, below). stdout/stderr
stream into the page; `proc_exit` unwinds `_start` and reports the exit code;
a genuine wasm trap surfaces as an error. Trap parity holds all the way here:
division by zero prints the canonical `error: division by zero` to the page's
stderr pane and exits 1, byte-identical to the interpreter and the native
binary.

## Run it

```powershell
# 1. Build the demo modules (needs clang + the WASI sysroot, same as any
#    `vyrn build --target wasm`; auto-detects ..\tools\wasi-sysroot-*):
.\web\build.ps1

# 2. Serve the directory (any static server; wasm needs http, not file://):
python -m http.server 8734 --directory web
# then open http://localhost:8734
```

The page also accepts any `.wasm` you built yourself via the file picker.

## JS interop — imports (`extern`, RFC-0012 M1)

A Vyrn program can declare host imports; the page supplies them:

```vyrn
extern fn jsLog(msg: String)
extern fn jsNow() -> Float64
```

```js
const r = await runVyrn(bytes, {
  extern: {
    jsLog: (msg) => append(msg),      // String param arrives decoded
    jsNow: () => Date.now() / 1000,   // Float64 return
    jsAdd: (a, b) => a + b,           // Int64 params/returns are BigInt
  },
});
```

The shim reads the module's own import section to recover each `vyrn.*`
signature and wraps the host function: a `String` crosses as a `(ptr, len)`
pair decoded from linear memory, `Int64` is a JS `BigInt`, floats and Bool map
naturally. A missing extern is a clear instantiate error naming provided vs
wanted. (Known ABI-shape caveat: an `i32` immediately followed by an `i64` in
a signature is decoded as one String argument — no v1 extern signature hits
that combination.) String *returns* from JS are not supported yet (needs an
exported allocator — RFC-0012 stage 1.5). See [externdemo.html](externdemo.html)
driving [examples/externdemo.vyrn](../examples/externdemo.vyrn).

On the interpreter and the native binary, *calling* an import extern traps with
the canonical ``error: extern `name` is not available on this target``
(declaring one is fine) — only the browser has a host.

## JS interop — exports (`export extern fn`, RFC-0012 M2)

The other direction: a Vyrn function exported to JS. It is a *normal* function
(body checked, runs everywhere, callable from Vyrn) that additionally appears on
the wasm module's exports.

```vyrn
export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { return a + b }
export extern fn greet(name: String) -> String { return "Hello, \{name}!" }
```

```js
const { exitCode, stdout, exports } = await runVyrn(bytes, {
  exportReturns: { greet: "string" },   // see the ABI note below
});
// _start already ran main(); now call the exports on the live instance:
exports.vyrnAdd(40, 2);   // => 42n  (Int64 is a BigInt)
exports.greet("world");   // => "Hello, world!"
```

The shim reads the module's function + export sections to recover each export's
signature, then wraps it. **String ABI, and why it differs from an import:** an
exported `String` *parameter* is a single `ptr` (not the import's `(ptr, len)`
pair), because the JS caller *can* allocate inside the module — it takes the
module's exported `__vyrn_malloc`, copies UTF-8 + a NUL terminator, and passes
the pointer; `vyrn` force-exports `__vyrn_malloc` whenever an `export extern fn`
has a String parameter. A returned `String` is a `ptr` the shim NUL-decodes from
linear memory. Because a `String`, `Bool`, and `Int32` all lower to a wasm
`i32`, the wrapper resolves *arguments* by the JS value's runtime type (a JS
string is allocated + copied) and *results* by an optional `exportReturns` hint
(`"string"` → NUL-decoded, `"bool"` → `true`/`false`, else a number; `i64` is a
BigInt, floats are numbers). The wrapper skips `memory`, `_start`,
`__vyrn_malloc`, and any `__`-prefixed export. See the M2 section of
[externdemo.html](externdemo.html) driving
[examples/externdemo2.vyrn](../examples/externdemo2.vyrn).

Unlike an import, calling an `export extern fn` never traps — it is an ordinary
function — so `externdemo2.vyrn` is fully three-way parity-capable
(interp == native == wasm).

## The event loop — module state + the host loop (RFC-0013)

`export extern` made a module callable after `main` returns; the missing half was
**state that survives between entries**. A top-level `let [mut] name = init` in
the root module is *module state*: visible to every function, initialized once
(in declaration order, before `main`), and alive for the whole module lifetime.

```vyrn
let mut hits = 0                          // module state — survives between calls

fn main() -> Int64 { return 0 }           // set-up only; the host drives from here

export extern fn onTick() -> Int64 {      // the host calls this on each timer fire
    hits = hits + 1
    return hits
}
export extern fn reset() { hits = 0 }     // …and this from a button
```

```js
const { exports } = await runVyrn(bytes);
setInterval(() => render(Number(exports.onTick())), 800);  // the host owns the loop
resetButton.onclick = () => exports.reset();
```

A wasm module can't block the page or suspend mid-function, so a Vyrn "event
loop" is an **inversion**: the host owns the loop and calls exported handlers;
Vyrn owns the state and the logic — the same shape wasm components and every
embedded runtime use, with no new control flow in the language. On native and
wasm each global is one LLVM `internal global` (`zeroinitializer`) whose
initializer runs in a synthesized `@__vyrn_globals_init` called from
`vyrn_entry` *before* `main`; the interpreter seeds a persistent frame the same
way. Stores validate like any value boundary; module state is never dropped
(safe-leak), can't be `consume`d or `drop`ped, and any function that reads or
writes a global is not spawn-safe (transitively — shared state by definition).

[examples/eventloop.vyrn](../examples/eventloop.vyrn) drives the handlers in a
deterministic in-`main` loop, so it is a normal three-way parity citizen
(interp == native == wasm). The live version is
[eventloop.html](eventloop.html): a timer renders the count and a button calls
`reset()`, all against the counter held in the running module's state.

## Typed RPC — the browser client (RFC-0019)

Two more zero-dependency runtimes turn a `std/rpc` `rpcClient` wasm module into a
typed browser client. They build only on the `extern` import/export ABI above —
no framework, nothing new in the shim.

- **[vyrn-rpc.js](vyrn-rpc.js) — the transport.** A `rpcClient` module imports
  one shared extern, `vyrn.vyrnRpcCall(name, body) -> Int64`, and exports one
  completion dispatcher per procedure, `vyrnRpcDone<Proc>(id, status, body)`.
  `makeRpcTransport({ baseUrl })` supplies the extern (a `fetch` `POST` to
  `<baseUrl>/rpc/<name>`) and, when the request settles, calls the matching
  dispatcher back into the module — so your plain Vyrn `onGetUser(id, res)` runs
  with a decoded `Validation<T>`. The proc→dispatcher name is the shared
  convention: `vyrnRpcDone` + the procedure name with its first letter uppercased
  (`getUser` → `vyrnRpcDoneGetUser`). A network failure reports **status 0**,
  which the generated unifier turns into an `rpc.transport` "unreachable" `Issue`.
  `runVyrnRpc(bytes, { baseUrl })` wires it onto `runVyrn` in one call.

  ```js
  import { runVyrnRpc } from "./vyrn-rpc.js";
  const { exports } = await runVyrnRpc(bytes, {
    baseUrl: "",
    exportReturns: { uiUser: "string" },   // name any String-returning getters
  });
  exports.loadUser(7n);   // your exported wrapper fires the typed stub; the
                          // reply flows to the Vyrn `onGetUser` handler
  ```

- **[vyrn-query.js](vyrn-query.js) — the cache ("colada").** ~110 lines, zero
  deps. `createQueryClient({ exports, baseUrl })` keys requests by
  `(proc, requestJson)`: concurrent callers share one in-flight fetch (dedupe), a
  settled entry is served within `staleTime`, `invalidate(proc | key)` drops
  entries and `refetch` forces a new one; `fetchCount` is observable. It drives
  the *same* dispatchers, so it is a cache in front of the transport, not a
  parallel path. Deliberately not TanStack Query — no retries, no focus
  revalidation, no GC.

**`vyrn dev`** ties it together for local development: it reads `vyrn.json`'s
`server` / `client` (+ optional `public`), builds the client to wasm (a *plain*
wasm build), and serves the server root's `handle` with static assets in front.
Precedence is locked: a GET naming an existing static asset — the built
`/client.wasm`, the runtimes under `/vyrn-runtime/*`, or a file under the public
dir (`/` → `index.html`) — is served from disk; every POST and every `/rpc/*` GET
goes to `handle`. See [examples/fullstack/](../examples/fullstack/): `vyrn dev`,
then the page does a typed round trip, a validated submit that renders the
server's own 422 issues, and a query-cache dedupe + invalidate demo.

## What this is (and isn't) yet

This is the browser direction through stage 2 (WASI shim demo), RFC-0012 M1+M2
(extern imports *and* exports), RFC-0013 (module state + the host-driven event
loop), and RFC-0019 (typed RPC as a library, with `vyrn dev` + the two runtimes
above): the full pipeline — validated types, protocols, schemas, regex DFAs, the
arena runtime, host calls in both directions, stateful handlers driven by a host
loop, and end-to-end typed client/server calls — runs in a browser today. What it
does NOT have yet: `async`/`await`, promises/JSPI suspension, or
callbacks-as-values across the JS boundary, tracked in ROADMAP.md.
