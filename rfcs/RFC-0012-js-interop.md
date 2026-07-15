# RFC-0012 — JS Interop (`extern`)

- **Status:** M1 + M2 implemented (imports and exports)
- **Depends on:** RFC-0010 (modules — `export` already has a meaning),
  the wasm backend (ROADMAP "browser path"), `web/wasi-min.js` (stage-2 demo)

> **Motivation.** The browser demo (`web/`) proved the whole pipeline runs in
> a page, but a Vela module can only talk to the world through stdout. The
> TS-replacement goal needs the two directions of real interop: Vela calling
> JS (DOM, fetch, console) and JS calling Vela (event handlers, a frame tick).
> This RFC defines the language surface, the ABI, and — critically — what the
> other two backends do, since `extern` is the first feature whose *behavior*
> cannot be identical across interp/native/wasm.

---

## Surface

### Importing a JS function (stage 1)

```vela
extern fn jsLog(msg: String)
extern fn jsNow() -> Float64
extern fn jsRandom() -> Float64

fn main() -> Int64 {
    jsLog("hello from vela, t=\{jsNow().toString()}")
    return 0
}
```

- `extern fn name(params) -> Ret` — a body-less declaration. The wasm module
  imports it from the fixed import namespace **`vela`** (module field =
  the function name). The host page supplies it when instantiating —
  `wasi-min.js` grows a `runVela(bytes, { extern: { jsLog: (s) => … } })`
  hook that wraps user functions with the string glue below.
- `extern` is a contextual top-level starter (like the capability modifiers —
  not a reserved word elsewhere). Parser recovery treats it as a declaration
  starter.
- Extern functions are module-level declarations; they may be `export`ed and
  imported across Vela modules like any `fn` (the *declaration* travels; the
  import always resolves to the single wasm import).

### Exporting Vela to JS (stage 2, same RFC, second milestone)

```vela
export extern fn tick(dt: Float64) -> Int64 { ... }
```

- `export extern fn` (with a body) adds the function to the wasm module's
  exports under its own name, with the ABI below. `_start` still runs `main`
  once; exported functions are callable afterwards (the module instance stays
  alive). The JS side gets them on the `exports` returned by `runVela`,
  pre-wrapped by `wasi-min.js` (string glue applied).
- Plain `export fn` keeps its RFC-0010 meaning (visible to Vela imports) —
  `extern` is what crosses the language boundary, in both directions.
- An `export extern fn` is a *normal* Vela function in every respect: its body
  is fully type-checked, it runs under the interpreter and the native binary
  (calling one never traps — only body-less imports do), it is callable from
  other Vela code, and it participates in spawn-purity analysis by its body. The
  export is purely additive on the wasm target (an inline `wasm-export-name`
  attribute on the `define`). A body is *required* — `export extern fn f()`
  without one is an error (a body-less `extern fn` is an import); `extern fn`
  with a body is likewise an error.

## ABI (v1 type domain)

Only these types may appear in an `extern` signature; the checker rejects
anything else with a message naming the offender:

| Vela | wasm | JS |
|---|---|---|
| `Int64` / sized ints | `i64` / `i32` | `BigInt` (`i64`), `number` (≤32-bit) |
| `Float64` / `Float32` | `f64` / `f32` | `number` |
| `Bool` | `i32` (0/1) | `boolean` |
| `String` | `(ptr, len)` pair of `i32` | `string` (glued) |
| return `Unit` | no result | `undefined` |

**String glue.** Vela→JS: the callee receives `(ptr, len)`; the shim decodes
UTF-8 from linear memory (strings are immutable — decode-on-cross is safe).
JS→Vela (extern returns `String`, stage 1.5): the shim encodes into a buffer
obtained from the exported `__vela_malloc` and returns the pointer; ownership
transfers to the module (droppable like any heap string). No other heap types
cross in v1 — no arrays, records, or enums (jsonSchema + a string is the
interchange format until then, which is exactly the wire-contract story).

**The import/export String ABI asymmetry (implemented).** A `String` crosses as
a `(ptr, len)` pair when it is an *import* parameter (M1) but as a single `ptr`
when it is an *export* parameter or *any* return (M2). The reason is who
allocates: a Vela→JS import hands the callee a pointer into the module's own
linear memory plus the length (the JS side cannot allocate inside the module
before the call, so length must travel alongside). An exported call is the
reverse — the JS caller *can* allocate: it grabs the module's exported
`__vela_malloc`, copies UTF-8 + a NUL terminator, and passes just the pointer,
so the callee reads the length by scanning for the NUL (a Vela String is a
NUL-terminated `ptr` internally). A returned `String` is likewise a bare `ptr`
the host NUL-decodes. Consequently, on the wasm side an exported `String`
parameter and an `Int32`/`Bool` are both a single `i32`; `wasi-min.js` resolves
the ambiguity by the runtime JS value (a JS string argument is encoded), and by
an optional `exportReturns` hint for a `String`/`Bool` *result*.

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
  harness runs it under wasmtime? No: wasmtime provides WASI, not the `vela`
  namespace. The harness instead asserts interp and native both produce the
  canonical trap (that much IS three-way-identical in spirit), and the
  *browser* behavior is covered by a `web/` demo page driving a real extern
  (`examples/externdemo.vela` + a page verified in the Browser pane).
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

The event loop (callbacks from JS *into* running Vela code beyond plain
re-entrant exported calls, timers, promises) — that is the next RFC and
depends on the concurrency model's threading story. DOM bindings as a
library — belongs in `std/` or a package once this lands. Var-args,
callbacks-as-values, JS object handles — all post-v1.

## Milestones

1. **M1 — imports:** `extern fn` declarations, wasm import emission, native/
   interp trap path, checker rules, `wasi-min.js` extern hook + string
   decode, `examples/externdemo.vela` + browser-pane-verified demo page,
   `WASM_ONLY` harness list.
2. **M2 — exports (implemented):** `export extern fn` (a normal, body-checked
   Vela function additionally exported to JS), wasm export emission via an inline
   `wasm-export-name` attribute (auto-rooted, no linker flag needed for the
   function; `__vela_malloc` is force-exported when a String parameter is
   present), post-`_start` callability, `wasi-min.js` export pre-wrapping (reads
   the function + export sections; encodes String args via `__vela_malloc`,
   decodes String returns by NUL scan), `examples/externdemo2.vela`
   (three-way-parity-capable — no host imports), and a browser-verified section
   in `web/externdemo.html` driving `velaAdd`/`greet` live.
