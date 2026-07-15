# RFC-0013 — Module State & the Host-Driven Event Loop

- **Status:** Draft — approved for implementation
- **Depends on:** RFC-0012 (`export extern fn` — re-entrant host calls),
  RFC-0004 (ownership), RFC-0003 (validated types)

> **Motivation.** RFC-0012 M2 made a live Vela module callable from JS after
> `main` returns — the mechanical half of an event loop. The missing half is
> **state that survives between entries**: Vela has no module-level bindings,
> so `onTick()` called at t=1s has nowhere to find the counter `main` set up
> at t=0. This RFC adds module state, and with it the event-loop *pattern* is
> complete — deliberately without adding async, coroutines, or a blocking
> `runLoop()`.

---

## The decided shape: the host owns the loop

A wasm module cannot block the page and cannot suspend mid-function (stack
switching is not shippable), so a Vela "event loop" is an **inversion**: the
host (a browser page, later a server runtime) owns the loop and calls exported
handlers; Vela owns the state and the logic. This is not a stopgap — it is the
same shape wasm components and every embedded runtime use, and it needs no
new control flow in the language. What it needs is state:

```vela
let mut hits = 0
let banner = "vela counter"

fn main() -> Int64 {
    return 0                       // set-up only; the host drives from here
}

export extern fn onTick() -> Int64 {
    hits = hits + 1
    return hits
}

export extern fn reset() {
    hits = 0
}
```

```js
const { exports } = await runVela(bytes);
setInterval(() => render(exports.onTick()), 1000);   // the host's loop
button.onclick = () => exports.reset();
```

## Surface: top-level `let`

- `let [mut] name [: Type] = initializer` at the top level of the **root
  module only** (v1). A module-state declaration in an *imported* module is a
  load error ("module state is root-only" — libraries stay stateless, the same
  discipline as root-only `logging`). `export let` does not exist in v1.
- Globals are visible to every function in the module (like functions and
  types — no forward-declaration order for *readers*).
- A local binding shadows a same-named global inside its scope; a global may
  not share a name with any other top-level declaration.

## Initialization

- Initializers run **once, in declaration order, before `main`** (and
  therefore before any exported extern is callable — on wasm, `_start` runs
  init + `main`; host calls come after).
- An initializer may use literals, operators, record/array literals, and
  **built-in** calls (`cell(x)`, `[1, 2]`, string ops). It may **not** call
  user functions or extern functions, and may not read a global declared
  *later* — both are checker errors. This keeps "before `main`" trivially
  well-defined: no user code runs before `main`, and no initializer can
  observe an uninitialized global (directly or through a call).

## Semantics

- **Assignment**: `g = v` anywhere in any function body, if `g` is `mut`;
  the value coerces into the declared/inferred type exactly like a local
  assignment — validated types validate on every store (compile-time when
  provably constant, runtime otherwise). Assigning a non-`mut` global is an
  error.
- **Ownership**: module state has module lifetime. It is never dropped
  (reclaimed at process exit — the safe-leak stance); `drop g` is an error;
  passing a global to a `consume` parameter is a movecheck error (nothing may
  take ownership of module state). Overwriting a heap-valued `mut` global
  leaks the old value (safe, consistent with array element stores).
- **Spawn isolation**: any function that reads **or** writes any module-state
  binding is not spawn-safe, transitively (module state is shared by
  definition; when tasks land on real threads this rule is what keeps them
  race-free by construction). This extends the existing purity fixpoint.
- **`where` predicates / consteval**: globals are not constants; a predicate
  or consteval context may not reference them.
- Exported extern handlers may of course use module state — that is the whole
  point — and remain (per RFC-0012) normal functions on every target: an
  event-handler example with no *imports* stays fully three-way
  parity-capable (`main` exercises the handlers directly in a test loop).

## The three backends

- **Checker**: globals join the scope as an outermost frame; visibility,
  mutability, shadowing, initializer restrictions, spawn-purity extension,
  consume/drop rejection (movecheck).
- **Interpreter**: a persistent global frame (Slot-typed, validating) that
  every function-call scope stack bottoms out on; initialized before
  `call("main")`.
- **Codegen (native + wasm, one IR)**: one LLVM global per binding
  (`zeroinitializer`), a synthesized `@__vela_globals_init()` running the
  initializer stores in declaration order, called from `vela_entry` before
  `main`. Reads/writes are loads/stores through the global. Validated stores
  reuse `emit_validation`.
- **LSP**: globals in the symbol index (hover `let mut hits: Int64`,
  go-to-definition, completion).

## Deliverables

- `examples/eventloop.vela` — module state + exported handlers, with a `main`
  that exercises the handlers in a deterministic loop (so the example is a
  normal three-way parity citizen), plus a `web/` counter page where a timer
  and a button drive the live module (browser-verified).
- ROADMAP: the browser-path paragraph gains the event-loop story; the
  "host owns the loop" model documented in web/README.md.

## Out of scope (explicitly)

`async`/`await`, futures, promise integration, suspension (JSPI), blocking
`runLoop()`; `export let` / cross-module state; `spawn` semantics changes
(tasks stay eager and synchronous); callbacks-as-values across the JS
boundary. Each becomes tractable later precisely because module state and the
host-driven pattern exist first.
