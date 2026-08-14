# RFC-0023 — Function Values, Monomorphized (Closures v1)

- **Status:** Implemented (2026-07-16)
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

> **The restriction was lifted by [RFC-0037](RFC-0037-stored-closures.md);
> the refusal it was protecting was kept.** A `fn` type is storable,
> returnable and escapable now — in a return, a `let` annotation, a record
> field, an array element, an `Option` payload and module state, pinned as
> `fn_types_are_storable` in `vyrn-frontend/src/checker.rs:12394-12408`.
> `std/shelf`'s middleware is an `Array<Middleware>` in module state.
>
> What did NOT come back is the bill this paragraph refuses. RFC-0037 lifted
> the restriction by **defunctionalization**: every function value is a closed
> enum, every call is a direct call through `@dispatch`, and the wasm table is
> identical to v1's. So "no closure exists at runtime, in any backend" still
> holds — there are no function pointers, no boxing, no capture lifetimes and
> no escape analysis. The sentence that stopped being true is the one before
> it. Storing a function value is the feature; a heap closure is still refused.

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

## Implementation notes (as landed)

- **Surface.** `Type::Fn(params, ret)` parses as `fn(T, U) -> R` (and `fn(T)`
  / `fn()` for a Unit return) in any type position, but the checker restricts
  it to a top-level function parameter — "function types are parameter-only in
  v1" — rejecting it in records, arrays, `Option`/`Result`, generic arguments,
  returns, `let` annotations, module state, and `extern`/`gen` signatures (and
  nested inside another `fn`). Lambda literals are `|x| expr`, `|x, y| { block }`,
  and the zero-parameter `|| expr` (the empty pipe pair is the single `||`
  token); a bare `|` in expression position unambiguously opens a lambda (there
  is no bitwise-or operator). A lambda is legal only as a call argument in a
  `fn`-typed parameter position; a named function is accepted there too.

  > **Two things in this bullet are no longer true.** The parameter-only
  > restriction was lifted by RFC-0037 — see the banner in §Design above; a
  > `fn` type is legal in a return, a `let` annotation, a record field, an
  > array element, an `Option` payload and module state. And **there is a
  > bitwise-or operator**: RFC-0045 added `|` as an infix `BitOr`
  > (`vyrn-frontend/src/parser.rs:4079`). The lambda rule still works, for a
  > narrower reason than the one given — `|` is infix-ONLY, so it can never
  > START an expression, and a bare `|` in expression position is therefore
  > still unambiguously a lambda.

- **A `fn`-typed argument may be any expression of `fn` type** (added 2026-08-07,
  after RFC-0037). The check used to name a lambda literal and a bare name and
  refuse the rest, so `wrap(h.f, 30)` was refused where `let prev = h.f` then
  `wrap(prev, 30)` was accepted. Since RFC-0037 a stored function value carries
  its own tag, and since RFC-0091 Phase 10b it owns its capture block: the `let`
  was moving the same `{ tag, captures }` pair through a slot and telling the
  backends nothing they could not read from the expression. The two
  monomorphizing backends resolve such an argument the way they already resolved
  a stored value read from a binding — the target is the signature's dispatcher
  and the value is the specialized instance's capture — so nothing is copied,
  the capture block keeps its owner, and every call still names a symbol.

  What it cost was an evaluation-order fix. The direct wasm backend laid its
  specialization out as ordinary parameters then captures, so a capture operand
  was pushed after every ordinary argument. A capture was always a load, which
  cannot be observed, and an expression can print or trap. The specialization now
  interleaves — a `fn` parameter becomes its captures at that parameter's own
  place — so a wasm argument is evaluated where the interpreter evaluates it.

- **Capture-timing lock.** Captures are materialized where the lambda *argument*
  is evaluated — the outer call site — in every backend. The interpreter takes a
  by-value snapshot of the enclosing locals at that point; the monomorphized
  backends evaluate the capture expressions as the extra arguments of the
  specialized callee at that same point. A binding reassigned between the outer
  call and the callee's inner invocations of the parameter is therefore never
  observed, identically everywhere. Module state is *not* captured — a global
  read inside a lambda resolves live (and makes the enclosing call chain
  non-spawn-safe, as always).

- **Nesting decision.** A lambda body **may** call functions that themselves
  take `fn` parameters (an ordinary call), but **may not** contain another
  lambda *literal* in v1 — nested literals would compound monomorphization for
  no proven need. The checker rejects a nested literal explicitly.

- **Generic `map`/`filter`/`fold`: no wall.** `std/arrays` ships them fully
  generic (`map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U>`, etc.). The
  checker infers in two passes — the ordinary arguments bind the inbound type
  parameters (`T` from `xs: Array<T>`) first, then each `fn`-typed argument's
  body infers the outbound one (`U`). Making this work needed one small,
  general fix: `unify`/`solve_param` now infer through `Array`/`ArrayN`/`Ref`
  element types (previously only `Option`/`Result`/generic-app did).

- **Monomorphization + dedup.** Each lambda literal is lifted to a top-level
  function `@__vyrn_lambda_<fn>_<ordinal>_<shape>_h<sha256/16>` whose leading
  parameters are its captures; the symbol keys on the enclosing function, the
  lambda's source-order ordinal, and its concrete capture/parameter/return shape,
  so two instantiations of a generic function lift distinct correctly-typed
  copies while identical ones dedup. The `<shape>` is READABLE and not
  injective; the trailing key is the identity, and it is there because the
  definition is deduped on this symbol (see `struct_key`). Each `fn`-taking
  callee is specialized per
  (callee, type args, target symbols) — `twice(|x| x*2)`, `twice(|x| x+off)`,
  and `twice(double)` are three instances; the parameter's captures arrive as
  the instance's extra trailing parameters, and a call to the parameter becomes
  a direct call to the target. A received `fn` parameter passed onward
  (pass-through) forwards its target and its capture parameters transitively.
  **Zero function pointers** in any backend — asserted by an IR test that every
  emitted `call` names a `@symbol`.

- **Verified.** interp == native == wasm byte-identical, including the
  `examples/lambdas.vyrn` parity citizen. The in-memory Inkwell backend
  (`vyrn-codegen-llvm`, excluded from the default workspace) was **not** taught
  the new lowering — it remains a subset backend.
