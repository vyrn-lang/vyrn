# RFC-0012 — JS Interop (`extern`)

- **Status:** M1 + M2 + M3 implemented (imports, exports, and the
  `vyrn:exports` type section)
- **Depends on:** RFC-0010 (modules — `export` already has a meaning),
  the wasm backend (ROADMAP "browser path"), `web/wasi-min.js` (stage-2 demo)

> **Motivation.** The browser demo (`web/`) proved the whole pipeline runs in
> a page, but a Vyrn module can only talk to the world through stdout. The
> TS-replacement goal needs the two directions of real interop: Vyrn calling
> JS (DOM, fetch, console) and JS calling Vyrn (event handlers, a frame tick).
> This RFC defines the language surface, the ABI, and — critically — what the
> other two backends do, since `extern` is the first feature whose *behavior*
> cannot be identical across interp/native/wasm.

---

## Surface

### Importing a JS function (stage 1)

```vyrn
extern fn jsLog(msg: String)
extern fn jsNow() -> Float64
extern fn jsRandom() -> Float64

fn main() -> Int64 {
    jsLog("hello from vyrn, t=\{jsNow().toString()}")
    return 0
}
```

- `extern fn name(params) -> Ret` — a body-less declaration. The wasm module
  imports it from the fixed import namespace **`vyrn`** (module field =
  the function name). The host page supplies it when instantiating —
  `wasi-min.js` grows a `runVyrn(bytes, { extern: { jsLog: (s) => … } })`
  hook that wraps user functions with the string glue below.
- `extern` is a contextual top-level starter (like the capability modifiers —
  not a reserved word elsewhere). Parser recovery treats it as a declaration
  starter.
- Extern functions are module-level declarations; they may be `export`ed and
  imported across Vyrn modules like any `fn` (the *declaration* travels; the
  import always resolves to the single wasm import).

### Exporting Vyrn to JS (stage 2, same RFC, second milestone)

```vyrn
export extern fn tick(dt: Float64) -> Int64 { ... }
```

- `export extern fn` (with a body) adds the function to the wasm module's
  exports under its own name, with the ABI below. `_start` still runs `main`
  once; exported functions are callable afterwards (the module instance stays
  alive). The JS side gets them on the `exports` returned by `runVyrn`,
  pre-wrapped by `wasi-min.js` (string glue applied).
- Plain `export fn` keeps its RFC-0010 meaning (visible to Vyrn imports) —
  `extern` is what crosses the language boundary, in both directions.
- An `export extern fn` is a *normal* Vyrn function in every respect: its body
  is fully type-checked, it runs under the interpreter and the native binary
  (calling one never traps — only body-less imports do), it is callable from
  other Vyrn code, and it participates in spawn-purity analysis by its body. The
  export is purely additive on the wasm target (an inline `wasm-export-name`
  attribute on the `define`). A body is *required* — `export extern fn f()`
  without one is an error (a body-less `extern fn` is an import); `extern fn`
  with a body is likewise an error.

## ABI (v1 type domain)

Only these types may appear in an `extern` signature; the checker rejects
anything else with a message naming the offender:

| Vyrn | wasm | JS |
|---|---|---|
| `Int64` / sized ints | `i64` / `i32` | `BigInt` (`i64`), `number` (≤32-bit) |
| `Float64` / `Float32` | `f64` / `f32` | `number` |
| `Bool` | `i32` (0/1) | `boolean` |
| `String` | `(ptr, len)` pair of `i32` | `string` (glued) |
| return `Unit` | no result | `undefined` |

**String glue.** Vyrn→JS: the callee receives `(ptr, len)`; the shim decodes
UTF-8 from linear memory (strings are immutable — decode-on-cross is safe).
JS→Vyrn (extern returns `String`, stage 1.5): the shim encodes into a buffer
obtained from the exported `__vyrn_malloc` and returns the pointer; ownership
transfers to the module (droppable like any heap string). No other heap types
cross in v1 — no arrays, records, or enums (jsonSchema + a string is the
interchange format until then, which is exactly the wire-contract story).

**The import/export String ABI asymmetry (implemented).** A `String` crosses as
a `(ptr, len)` pair when it is an *import* parameter (M1) but as a single `ptr`
when it is an *export* parameter or *any* return (M2). The reason is who
allocates: a Vyrn→JS import hands the callee a pointer into the module's own
linear memory plus the length (the JS side cannot allocate inside the module
before the call, so length must travel alongside). An exported call is the
reverse — the JS caller *can* allocate: it grabs the module's exported
`__vyrn_malloc`, copies UTF-8 + a NUL terminator, and passes just the pointer,
so the callee reads the length by scanning for the NUL (a Vyrn String is a
NUL-terminated `ptr` internally). A returned `String` is likewise a bare `ptr`
the host NUL-decodes. Consequently, on the wasm side an exported `String`
parameter and an `Int32`/`Bool` are both a single `i32`; `wasi-min.js` resolves
the ambiguity by the runtime JS value (a JS string argument is encoded), and by
the module's `vyrn:exports` section for a `String`/`Bool` *result* (M3, below).
Before M3 the result was resolved by a hand-written `exportReturns` hint.

**Who frees an exported `String` parameter: nobody (measured).** The paragraph
above says who allocates and never said who frees. The answer follows from a
rule the language already has. Ownership analysis (`vyrn-frontend/src/own.rs`)
keys `droppable` on `Stmt::Let` nodes only, so a parameter is *borrowed* — a
callee never frees one, on any backend. The caller owns it. Across this boundary
the caller is JS, and `wasi-min.js` allocates through `__vyrn_malloc` and then
forgets the pointer. The owner is real and does not free.

It also cannot free. `__vyrn_malloc` is the only allocator symbol a module
exports, and the direct wasm backend has nothing to pair with it: its allocator
is a bump pointer, `Stmt::Drop` of a `String` emits no code there, and
`DropKind::FreeStr` has no wasm counterpart at all.

Measured on `web/domdemo.wasm`, whose `increment` never reads `arg`. 20000 calls
with a 900-byte argument grew linear memory from 2 pages to 277 — 18,022,400
bytes against 20000 × 901 = 18,020,000, so the growth is the argument buffers
and nothing else. Successive `__vyrn_malloc` results never repeat.

The leak is not particular to this ABI: on the wasm target the whole heap is
monotonic. What is particular is the rate. Elsewhere the program decides how
often it allocates. Here the host decides — `onType` fires once per keystroke
and burns `len + 1` bytes the instance never gets back. Input-driven growth is a
different problem from program-driven growth.

Not fixed here, and not fixable in `wasi-min.js`. It needs an allocator that can
free, plus a decision: export `__vyrn_free` and make the host pair every encode
with it, or move the parameter to callee ownership so the ABI frees it. The
second changes what a `String` parameter means everywhere, not just at this
boundary. Recorded, not decided.

**Decided and applied by RFC-0077 M6: the first.** A module that exports
`__vyrn_malloc` exports `__vyrn_free` beside it, and the export wrapper in
`wasi-min.js` releases every buffer it encoded for a call, after decoding the
result. The 20000-call measurement above is flat.

Two things fell out of it. **The result is decoded before the release**, because
an export may hand back the pointer it was given and `free` writes its list link
into the block. And **five handlers in this repo were retaining the argument** —
`domdemo`'s `onType`, `bin`'s three draft setters, `fullstack`'s `setId`, each
doing `state = arg`. That was always wrong under the rule above; it worked only
because the owner could not free. They copy now, and **nothing checks the rule**:
an exported `extern fn` that stores a String parameter past its own return is a
use-after-free the compiler accepts. That is open, and it is a frontend rule.

A returned `String` is still not freed. Who owns one is `own::analyze`'s answer
per function and nothing carries it across the boundary — see RFC-0077 "M6, as
landed".

**Both paragraphs are closed by RFC-0089.** Rule 2 refuses the stored parameter,
for every function rather than only an exported one (M2, Phase 4b-2), and the
five handlers write `.copy()` rather than `arg + ""` (M3b). Rule 3 makes a
returned String the caller's, so `wasi-min.js` frees it after decoding (M3b) —
nothing carries a fact across the boundary because there is only one answer.
Making that true cost three refusals, all of them Phase 6's finding: an
`export extern fn` may not return module state, may not return a projection, and
may not declare a `consume` String parameter. The last one is this section's
own ABI: RFC-0012 gives the argument to the caller, and `consume` said the
callee took it. The signature compiled and the page freed the buffer anyway.
`__vyrn_malloc` and `__vyrn_free` now go out whenever an `export extern fn`
takes OR returns a String.

**Zero-copy over a foreign buffer is impossible on wasm.** RFC-0082 lists
"zero-copy over a buffer Vyrn does not own" among the capabilities a raw-memory
view would serve. On this target that item is unreachable whatever the language
offers, so it should not sit on a list of things Vyrn might add. wasm loads and
stores address one linear memory and nothing else. A JS string, an
`ArrayBuffer`, a `File` — none live there, so each is copied in before wasm code
can read a byte of it. `externref` does not change that: a reference is an
opaque handle, not addressable bytes, and the JS string builtins that consume
one still finish by writing into linear memory. Multi-memory imports a
`WebAssembly.Memory`, and an existing `ArrayBuffer` cannot become one.

The achievable version is one copy instead of two. `encodeString` calls
`TextEncoder.encode`, which allocates a JS array, then copies that array into
linear memory with `view.set`. `TextEncoder.encodeInto` writes straight into
linear memory. Per call on Node v24, with the destination over-allocated 3x:
24 B 269 ns → 88 ns, 900 B 1090 ns → 592 ns, 90 KB 81.2 µs → 50.3 µs. Sizing the
destination exactly beats 3x on ASCII (66 ns at 24 B) and loses on non-ASCII
(1865 ns against 1148 ns today at 900 B), because the exact path re-encodes from
scratch when `encodeInto` stops short. 3x wins at every size and loses nowhere.
It is not applied, because over-allocating on a path that never frees triples
the leak above. The two land together or not at all.

The NUL scan in `decodeCString` was measured and is not worth changing: a byte
loop costs 25.60 µs against 24.37 µs for `indexOf` on a 90 KB string. The decode
dominates both.

## The parity question (decided)

`extern` is host-provided by definition, so byte-identical three-way parity
cannot apply to programs that call it. The rule:

- **wasm:** the real import, as above.
- **native + interpreter:** calling an extern function **traps** with the
  canonical wording `error: extern `name` is not available on this target`
  (stderr, exit 1). Declaring extern functions is fine everywhere — only a
  *call* on a non-wasm target traps. This keeps single-binary semantics
  honest instead of silently stubbing.
- **Parity harness:** an example that calls extern functions goes in a new
  `WASM_ONLY` list (mechanism precedent: `EXPECTED_CHECK_FAILURE`) — the
  harness runs it under wasmtime? No: wasmtime provides WASI, not the `vyrn`
  namespace. The harness instead asserts interp and native both produce the
  canonical trap (that much IS three-way-identical in spirit), and the
  *browser* behavior is covered by a `web/` demo page driving a real extern
  (`examples/externdemo.vyrn` + a page verified in the Browser pane).
  `KNOWN_DIVERGENT` stays empty.

## Checker rules

- Body-less `fn` is legal only with `extern`; `extern` with a body is legal
  only together with `export` (an exported implementation).
- Signature type domain enforced (table above).
- Extern calls are forbidden in `spawn`ed tasks (they are effects; the
  spawn-purity walker adds them to the forbidden set) and in `where`
  predicates / consteval (not constant).
- LSP: extern declarations index as ordinary function symbols (hover shows
  `extern fn …`); calls resolve normally.

## Out of scope (explicitly)

The event loop (callbacks from JS *into* running Vyrn code beyond plain
re-entrant exported calls, timers, promises) — that is the next RFC and
depends on the concurrency model's threading story. DOM bindings as a
library — belongs in `std/` or a package once this lands. Var-args,
callbacks-as-values, JS object handles — all post-v1.

## M3 — the module says what it returns

M2 left one fact outside the module. An `i32` result is a `String`, a `Bool` or
an `Int32`, and the only thing that said which was `hooks.exportReturns`, a map
each page wrote by hand. A name nobody wrote came back as a number; after
RFC-0089 M3b the same hint also decided the `free`, so a missed name leaked as
well. Five sites carried the map — `web/externdemo.html`, `web/vyrn-dom.js`,
`examples/bin/public/app.js`, `web/README.md` and the memory suite's driver —
and the compiler knew the answer at every one of them.

**A module carries it now.** `vyrn build --target wasm` writes a custom section
named `vyrn:exports`: a vector of pairs, each a wasm name — a length, then its
UTF-8 bytes. The name is the export, and the value is `string` or `bool`. An
export whose result is unambiguous (`Unit`, `Int64`, a float) is left out, so a
module with no such export carries no section at all.

`wasi-min.js` reads it in the section walk it already runs to recover each
export's signature — one more branch, on section id 0. The section wins over
`hooks.exportReturns`, which stays as the fallback for a module from another
producer.

**This changes the shipped ABI**, which is why it is written here rather than in
the phase that did it. The change is additive in both directions: an engine that
does not know the name skips a custom section, and a host that does not read the
section still works from the hook.

The census recorded this as the last convention at the boundary (RFC-0087 U10).
Both ownership answers were already fixed by RFC-0089's rules; the type was not.
It is now.

## Milestones

1. **M1 — imports:** `extern fn` declarations, wasm import emission, native/
   interp trap path, checker rules, `wasi-min.js` extern hook + string
   decode, `examples/externdemo.vyrn` + browser-pane-verified demo page,
   `WASM_ONLY` harness list.
2. **M2 — exports (implemented):** `export extern fn` (a normal, body-checked
   Vyrn function additionally exported to JS), wasm export emission via an inline
   `wasm-export-name` attribute (auto-rooted, no linker flag needed for the
   function; `__vyrn_malloc` is force-exported when a String parameter is
   present), post-`_start` callability, `wasi-min.js` export pre-wrapping (reads
   the function + export sections; encodes String args via `__vyrn_malloc`,
   decodes String returns by NUL scan), `examples/externdemo2.vyrn`
   (three-way-parity-capable — no host imports), and a browser-verified section
   in `web/externdemo.html` driving `vyrnAdd`/`greet` live.
3. **M3 — the `vyrn:exports` section (implemented):** the direct wasm backend
   writes every `export extern fn` whose result is a `String` or a `Bool` into a
   custom section, `wasi-min.js` reads it in its existing section walk, and the
   five hand-written `exportReturns` maps are deleted. See the section above.

