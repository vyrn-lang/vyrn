# RFC-0023 — Function Values, Monomorphized (Closures v1)

- **Status:** Draft — approved for implementation (after RFC-0022)
- **Depends on:** RFC-0002 §5 (protocols — the dispatch machinery this
  rides), RFC-0004 (ownership — the constraint that shapes everything)
- **Evidence:** the query cache wants continuations (RFC-0019 deferred it
  to JS), collection code wants `map`/`filter`, GraphQL resolvers are gated
  on this, and the `on<Proc>` naming convention exists only because
  closures don't.

> **The design constraint.** Vyrn is ownership-based with three backends
> that must agree byte-for-byte. Heap closures with captured environments
> are where languages pay their complexity bill: escape analysis, capture
> lifetimes, boxing, invalidation. v1 refuses the bill: **function values
> exist only as call arguments, and every use is monomorphized away at
> compile time.** No function type is storable, returnable, or escapable —
> so no closure exists at runtime, in any backend.

---

## Surface

```vyrn
fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {
    let mut out: Array<Int64> = []
    for x in xs {
        out.push(f(x))
    }
    return out
}

fn main() -> Int64 {
    let doubled = twice([1, 2, 3], |x| x * 2)
    let offset = 10
    let shifted = twice([1, 2, 3], |x| x + offset)   // captures by read
    return 0
}
```

- **Function-typed parameters:** `f: fn(T…) -> R` is legal **only as a
  function parameter type** — not in records, arrays, returns, `let`
  annotations, module state, or extern/procedure signatures. The checker
  names the restriction when violated ("function types are parameter-only
  in v1").
- **Lambda literals:** `|params| expr` and `|params| { block }` — legal
  **only as a call argument** in a function-typed parameter position (and
  as the direct value of such a param in a nested call). Named functions
  are also accepted where a `fn`-typed argument is expected (`twice(xs,
  double)`) — same monomorphization path.
- **Capture rules (the ownership answer):** a lambda may **read** outer
  locals, params, and module state (module-state reads make the enclosing
  call chain non-spawn-safe, as always). It may **not**: mutate captured
  locals (no write-back — a lambda is a function, not a scope), `consume`
  a captured binding, capture `Ref` mutably-specially (a captured `Ref` is
  just a value — cell rules unchanged), or `drop` captured names.
  Movecheck treats captured bindings as **borrowed for the duration of the
  call** — the caller cannot consume a binding in the same statement it is
  captured. Since no lambda outlives its call expression (by construction),
  there are no lifetime questions to answer.

## The mechanism (why this is cheap)

Monomorphization, exactly like generics + protocols already work:

1. Each lambda literal becomes a synthesized top-level function
   (`@lambda<N>`, unspellable) whose extra leading parameters are its
   captures (by value for scalars, by the same passing mode the checker
   uses for that type elsewhere).
2. Each call to a function with `fn`-typed params is **specialized per
   callee-lambda** (the generic-instantiation machinery — `twice` with
   `|x| x*2` and `twice` with `double` are two mangled instances; the
   call inside becomes a direct call with captures appended).
3. Consequence: **zero function pointers in any backend.** The
   interpreter, native IR, and wasm all see ordinary direct calls — parity
   is inherited, there is nothing new at runtime, and the wasm binary
   gains no indirect-call tables.

Instantiation dedup keys on (function, lambda body identity); recursion
through fn-params is bounded exactly as generic recursion already is.

## What v1 deliberately buys

`map`/`filter`/`fold` in std (this RFC ships `std/arrays` with them —
written in Vyrn, parity-free); custom iteration/visitor patterns; the
i18n/rpc generators emitting cleaner code; `spawn`-safety analysis extends
naturally (a lambda's effects are its body's effects, analyzed at each
instantiation site).

## What it deliberately defers (the stored-closure bill)

Storing continuations (the query cache stays host-side), returning
functions, function-typed fields, dynamic dispatch on function values.
When real usage demands storage, the escalation path is explicit:
protocol-object closures (a captured-state record implementing a `Call`
protocol) — a design that builds on this RFC's lowering rather than
replacing it. The `on<Proc>` RPC convention stays until then; this RFC
does NOT rework std/rpc.

## Out of scope

Everything in "deliberately defers", plus: currying/partial application,
function composition operators, `async` interactions (none exist),
capture-by-move syntax, mutable capture.
