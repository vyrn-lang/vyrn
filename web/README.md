# Vyrn in the browser

The same `vyrn build --target wasm` module that runs under wasmtime, executed
in a browser by a hand-rolled WASI preview1 shim
([wasi-min.js](wasi-min.js)) — no frameworks, no toolchain in the page, zero
dependencies (the project's no-crates ethos, applied to JS). Since RFC-0077 M5
there is no toolchain on the *building* side either: `--target wasm` emits the
module directly, so a page's worth of demos needs neither clang nor a WASI
sysroot.

A compute-only vyrn module imports exactly two preview1 functions — `fd_write`
and `proc_exit`. A module using input (RFC-0014: `args`/`readLine`/`readFile`/
`writeFile`/`renameFile`) additionally pulls in `args_get`, `args_sizes_get`,
`fd_read`, `fd_close`, `fd_prestat_get`, `path_open` and `path_rename`; a clock
or random one adds `clock_time_get`, `random_get` and the `environ_*` pair
(RFC-0043's injected fixed values). Thirteen are declared and the module keeps
only the ones its own code reaches, so the count above is a fact about each
module rather than about the backend. The input syscalls get **graceful
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
# 1. Build the demo modules (needs nothing but a built `vyrn` — RFC-0077 M5
#    emits wasm directly, so no clang and no WASI sysroot):
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

The shim reads each `vyrn.*` signature out of the module's own
**`vyrn:exports` custom section** and wraps the host function: a `String`
crosses as a `(ptr, len)` pair decoded from linear memory, `Int64`/`UInt64` are
JS `BigInt`s, floats and Bool map naturally, and a `UInt32` arrives unsigned. A
missing extern is a clear instantiate error naming provided vs wanted.

Until RFC-0012 M3's section grew to cover both directions, the shim walked the
module's type and import sections and guessed from the ABI shape: an `i32`
immediately followed by an `i64` was taken to BE a String, because that is what
a `String` lowers to — so `extern fn hostPair(a: Int32, b: Int64)` arrived at
its JS hook as ONE argument, a string decoded from linear memory at address `a`
with length `b`, and the second parameter as `undefined`. That caveat is gone
with the guess. String *returns* from JS are still not supported (needs an
exported allocator — RFC-0012 stage 1.5). See
[externdemo.html](externdemo.html) driving
[examples/externdemo.vyrn](../examples/externdemo.vyrn).

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
const { exitCode, stdout, exports } = await runVyrn(bytes);
// _start already ran main(); now call the exports on the live instance:
exports.vyrnAdd(40, 2);   // => 42n  (Int64 is a BigInt)
exports.greet("world");   // => "Hello, world!"
```

The shim takes each export's signature from the same **`vyrn:exports` custom
section**, and the list of callable names from `instance.exports`. **String ABI,
and why it differs from an import:** an exported `String` *parameter* is a single
`ptr` (not the import's `(ptr, len)` pair), because the JS caller *can* allocate
inside the module — it takes the module's exported `__vyrn_malloc`, copies UTF-8
+ a NUL terminator, and passes the pointer; `vyrn` force-exports `__vyrn_malloc`
whenever an `export extern fn` has a String parameter. A returned `String` is a
`ptr` the shim NUL-decodes from linear memory.

`String`, `Bool`, `Int32` and `UInt32` all lower to a wasm `i32`, so BOTH ends of
a call are resolved from the declaration: a `string` parameter takes a JS string
(and refuses a number, which the module would read as a pointer), a `bool`
parameter takes anything truthy, a `u32` result comes back unsigned. Arguments
used to be resolved by the JS value's runtime type instead, so `greet(42)` on
`export extern fn greet(name: String)` returned `"Hello, !"` — 42 was handed over
as a pointer — and a `UInt32` result over 2^31 came back negative. The wrapper
skips `memory`, `_start`, `__vyrn_malloc`, and any `__`-prefixed export.

**Who frees what.** Both directions are the caller's, and across this boundary
the caller is the page. A String ARGUMENT is allocated here and released here,
in a `finally` so a caught `panic` still frees it. A returned String is released
here too, after decoding — RFC-0089 rule 3 says a return is owned, and Phase 6
made an `export extern fn` that hands back something it does not own fail to
compile: it may not return module state, may not return a projection, and may
not declare a `consume` String parameter. Each refusal names `.copy()`. A String
literal is the one pointer an export may return that is not heap, and it needs
no rule — it carries `cap = 0` and sits below `HEAP_BASE`, which `__vyrn_free`
ignores. `vyrn` exports `__vyrn_malloc` and `__vyrn_free` whenever an
`export extern fn` takes OR returns a String.

The release runs for every result the module DECLARES as a String, and the
module declares them all. Until RFC-0012 M3 the page wrote the map by hand, so
an export nobody named came back as a number and leaked. The compiler is the
thing that knows, and it writes the section now — every signature on the
boundary, in both directions, so nothing is left for a page or a runtime to
infer. A module carrying no section is refused by name rather than guessed at;
there is no hand-written fallback, because there is no producer of Vyrn wasm but
`vyrn build --target wasm`. See the M2 section of
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
  dispatcher back into the module with the **same id** the extern returned — the
  module routes the reply to the pending callback the stub stored under that id
  (RFC-0040 §2), so the callback you passed at the call site runs with a decoded
  `Validation<T>`. The proc→dispatcher name is the shared convention:
  `vyrnRpcDone` + the procedure name with its first letter uppercased
  (`getUser` → `vyrnRpcDoneGetUser`). A module built with the DIRECTORY generator
  (`client("./server/api")`, RFC-0072 M3) imports the same extern with the
  procedure and its derived path as separate arguments —
  `vyrnRpcCall(proc, path, body)` — because a derived or pinned path no longer
  says which dispatcher owns the reply, and inverting the path template in the
  host would be a second implementation of the derivation rule. The transport
  dispatches on arity, so one runtime serves both. A network failure reports
  **status 0**,
  which the generated unifier turns into an `rpc.transport` "unreachable" `Issue`.
  `runVyrnRpc(bytes, { baseUrl })` wires it onto `runVyrn` in one call.

  ```js
  import { runVyrnRpc } from "./vyrn-rpc.js";
  const { exports } = await runVyrnRpc(bytes, { baseUrl: "" });
  exports.loadUser(7n);   // your exported wrapper fires the typed stub; the
                          // reply flows to the callback it passed
  ```

- **[vyrn-query.js](vyrn-query.js) — the cache ("colada").** ~120 lines, zero
  deps. `createQueryClient({ baseUrl, staleTime })` **is a transport**: it
  supplies `vyrnRpcCall`, mints the request ids the module's pending-callback
  maps key on, and dispatches every settle (network or cache) under the same id.
  Requests key by `(proc, requestJson)`: concurrent callers share one in-flight
  fetch (dedupe), a settled entry is served within `staleTime`,
  `invalidate(proc | key)` drops entries; `fetchCount` is observable. Drop it in
  the extern slot *instead of* vyrn-rpc.js's transport. Deliberately not
  TanStack Query — no retries, no focus revalidation, no GC.

**`vyrn dev`** ties it together for local development: it reads `vyrn.json`'s
`server` / `client` (+ optional `public`), builds the client to wasm (a *plain*
wasm build), and serves the server root's `handle` with static assets in front.
Precedence is locked: a GET naming an existing static asset — the built
`/client.wasm`, the runtimes under `/vyrn-runtime/*`, or a file under the public
dir (`/` → `index.html`) — is served from disk; every POST and every `/rpc/*` GET
goes to `handle`. See [examples/fullstack/](../examples/fullstack/): `vyrn dev`,
then the page does a typed round trip, a validated submit that renders the
server's own 422 issues, and a query-cache dedupe + invalidate demo.

## The UI layer — `view()` + `vyrn-dom.js` (RFC-0026)

[`std/html`](../std/html.vyrn) is a pure library: an `Html` payload enum
(`Empty` / `Text` / `Raw` / `El`) with `Attr` (`Cls` / `Id` / `A` / `On` /
`Key`), trivial constructors, and a total `toHtmlString` SSR renderer (the
locked escaping rules, the void-element set, `On` → `data-on-<event>` /
`data-arg-<event>`). A component is a pure `fn view() -> Html` of module state —
so SSR (`toHtmlString(view())`) and the client (`toJson(view())`) share it, and
views are three-way parity citizens (see [examples/htmltree.vyrn](../examples/htmltree.vyrn)).

**[vyrn-dom.js](vyrn-dom.js) — the client runtime.** Zero deps, beside
`wasi-min.js`, talking to ordinary wasm exports (nothing privileged). The Elm
Architecture, host-side:

- **boot:** `mount(bytes, el, opts)` instantiates, calls the exported
  `vyrnView()` (= `toJson(view())`), parses the JSON `Html` tree, builds the DOM.
- **update:** after any handler returns it calls `vyrnView()` again and diffs
  new vs. retained — **keyed** where `Key` attrs are present (a reused node is
  *moved*, so input value / caret / focus survive a reorder), positional
  otherwise — patching minimally.
- **events:** one delegated listener per event type on the mount root; on an
  event it walks to the nearest `data-on-<type>` and invokes the exported handler
  by name. Locked ABI: every handler is `export extern fn name(arg: String)` —
  `click`/`keydown` send the `data-arg` payload, `input`/`change` the control
  value, `submit` the payload (+ `preventDefault`).
- **subscriptions:** the app may export `vyrnSubs()` (= `toJson(subs())`) of
  `Sub = Every(ms, handler) | Keydown(key, handler)`; the host reconciles the
  list by value after each render — appeared wire, disappeared unwire.
- **effects:** a `data-effect="name"` node invokes a host-registered effect
  (`app.effect(name, fn)` / `opts.effects`) on appear, with an optional cleanup
  returned for disappear.

```js
import { mount } from "./vyrn-dom.js";
const app = await mount(bytes, document.getElementById("app"), {});
// app.exports / app.rerender() / app.effect(name, fn) / app.destroy()
```

[examples/domdemo.vyrn](../examples/domdemo.vyrn) + [domdemo.html](domdemo.html)
exercise a counter, a keyed-list reorder, a text input, and an `Every`
subscription; the [fullstack](../examples/fullstack/) client is a `view()` over
its RPC state — `vyrn-dom.js` for the DOM, `vyrn-rpc.js` for the transport.

## What this is (and isn't) yet

This is the browser direction through stage 2 (WASI shim demo), RFC-0012 M1+M2
(extern imports *and* exports), RFC-0013 (module state + the host-driven event
loop), and RFC-0019 (typed RPC as a library, with `vyrn dev` + the two runtimes
above): the full pipeline — validated types, protocols, schemas, regex DFAs, the
arena runtime, host calls in both directions, stateful handlers driven by a host
loop, and end-to-end typed client/server calls — runs in a browser today. What it
does NOT have yet: `async`/`await`, promises/JSPI suspension, or
callbacks-as-values across the JS boundary, tracked in ROADMAP.md.
