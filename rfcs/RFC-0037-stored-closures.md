# RFC-0037 — Stored Function Values by Defunctionalization (Closures v2)

- **Status:** Implemented (2026-07-17)
- **Depends on:** RFC-0023 (function values v1 — the parameter-only
  restriction this lifts; the lambda lifting and capture-timing rules it
  reuses), RFC-0024 (payload enums — the runtime representation), RFC-0004
  (ownership — captures become owned record fields)
- **Evidence:** RFC-0023 named this exact escalation ("protocol-object
  closures … when real usage demands storage") and the demands have
  accumulated: the query cache keeps continuations host-side because they
  cannot be stored; the `on<Proc>` RPC convention exists only because a
  completion callback cannot be a value; UI subscriptions dispatch by
  string name; GraphQL resolvers are explicitly gated on this; middleware
  chains for `handle` cannot be written. RFC-0035 hit the adjacent wall
  (no stored tree ops beyond module state workarounds).

---

## The insight: Vyrn never needs function pointers for this

Heap closures are expensive in open-world languages because the callee
set is unknown — hence vtables, boxing, escape analysis. Vyrn compiles
whole programs: **every function or lambda that flows into a stored
position is statically known.** So storage lowers by defunctionalization:

- Each storable function type `fn(A…) -> R` (identified structurally)
  gets one synthesized closed enum, e.g. `@fnval_Int64_to_Int64`, with
  **one variant per source** — a named function (empty payload) or a
  lifted lambda (payload = its capture record, by-value, exactly the
  RFC-0023 capture snapshot, now stored instead of passed).
- Calling a stored value is `match` over that enum + **direct calls** —
  the RFC-0023 IR invariant ("every call names an `@symbol`") holds
  verbatim; wasm gains no indirect-call table entry.
- The runtime representation is an ordinary payload enum (RFC-0024
  machinery — construction, copies, drops, `match` all exist and have
  byte-identical parity today).

## Surface

```vyrn
type Middleware = fn(Request) -> Option<Response>

let mut chain: Array<Middleware> = []          // module state holding closures

fn addLogging(prefix: String) {
    chain.push(|req| logAndPass(prefix, req))   // capture stored in the value
}

type Debounced = { delay: Int64, action: fn() }

fn runAll(req: Request) -> Response {
    for m in chain {
        match m(req) { Some(r) => return r, None => {} }
    }
    return notFound()
}
```

- **Lifted restriction:** `fn(T…) -> R` becomes legal in `let`
  annotations, record fields, `Array`/`Map` values, `Option`/`Result`,
  returns, and module state. (Still illegal: `extern`/`gen` signatures,
  procedure/wire types — see codability.)
- **Sources:** anywhere an expression of a matching fn type is required,
  a lambda literal or a named function is accepted (as in v1 call
  arguments), and now also **a fn-typed value** read from storage
  (composition: `let g = h`).
- **Calling:** `f(args)` where `f` is a fn-typed binding/field/element —
  call syntax unchanged from v1 parameters.

## Semantics (locked)

- **Captures: by-value snapshot at the lambda's evaluation site,
  read-only** — RFC-0023's rules verbatim; the snapshot simply lives in
  the variant payload instead of being re-passed. A stored closure never
  observes later reassignment of a captured local. Module state is still
  NOT captured — read live at call time (and poisons spawn-safety of the
  call site, as always).
- **Ownership:** the closure value owns its captures (record-field
  rules). Copying the value copies captures; `drop` frees them;
  movecheck/consume treat it as the enum it is. Capturing a non-copyable
  value moves… no: v1 keeps RFC-0023's rule — captures are by-value
  copies; capturing a value whose type cannot be copied that way is the
  same error v1 gives.
- **Identity of the synthesized enum is structural per signature**
  (`fn(Int64) -> Int64` is one enum program-wide); its variant set is
  the program-wide set of sources that flow into ANY stored position of
  that signature (collected during checking; deterministic order =
  declaration/lift order, so codegen is stable).
- **Effects & isolation:** the effect set of calling a stored value is
  the union over all variants (conservative). Spawn-safety and the
  workers gate extend through the same fixpoint (a stored value whose
  ANY variant touches module state makes the calling chain stateful).
- **Not codable, not comparable:** fn-typed values are rejected by the
  codec/schema (named diagnostic — functions don't go on the wire) and
  have no `==`.
- **Nested lambdas** (a lambda literal inside a lambda body): still
  rejected, unchanged from v1 — lifting order stays simple.
- **v1 parameter semantics are untouched**: a `fn`-typed *parameter*
  still monomorphizes per call site exactly as RFC-0023 built it — this
  RFC adds a storage lowering, it does not slow down the existing path.
  A monomorphized param position accepts a stored value too (it becomes
  a call through the enum inside the specialized instance).

## What this deliberately still defers

`any P` protocol objects (heterogeneous protocol-typed values — same
defunctionalization machinery, one variant per impl; record as the next
escalation if demanded), mutable captures, capture-by-move syntax,
function equality, wire-transportable functions (never), fn-typed
extern boundaries (the DOM keeps dispatching by name — closures do not
cross wasm).

## Consumers (the proof)

- **`std/arrays`** gains `sortBy(xs, key: fn(T) -> Int64)`-style helpers
  where useful, and a new `examples/closures2.vyrn` parity citizen:
  stored lambdas in arrays/records/module state, captures surviving
  scope exit, composition, a middleware chain, drop/leak behavior.
- **shelf server**: a real middleware chain (`Array<Middleware>` — logging
  + a header guard) in front of `route(req)`, replacing nothing but
  proving the shape real apps will use.
- **Spawn/workers pins**: a stored closure touching module state gates
  `--workers` with the chain naming it; an isolated one spawns fine.

## Out of scope

Everything in "deliberately defers", plus currying/partial application,
function composition operators, and any change to `.vyx`/UI event
dispatch (names remain the extern-boundary mechanism).

## Implementation notes (as landed)

- **Checker.** `ensure_type_exists` accepts `Type::Fn` (validating its
  components; fn-in-fn stays rejected — "a function type may not take/return
  another function value"). Legal positions as designed: `let` annotations,
  record fields, `Array`/`Map` elements, `Option`/`Result`, **enum payloads**
  (a natural extra), returns, module state, and `type X = fn(..)` transparent
  aliases (interchangeable with the structural form via `assignable`; no
  `where` predicate). Still rejected with named diagnostics: `extern`/`gen`
  signatures (params AND returns), `Ref<fn>`/`Task<fn>`, codec/schema
  ("cannot encode `fn(..) -> ..`"), `==` (the scalar-operand rule names it),
  generic functions as values, and a spawned callee with fn-typed parameters.
  Sources are accepted wherever a concrete fn type is EXPECTED (the checker's
  `expected` channel — every storage boundary already threads it) plus the
  bare-name value position (`let g = double` infers from the signature).
  A lambda with no fn type in context keeps a named error suggesting the
  annotation. Nested lambda literals stay rejected (v1 wording).

- **Effects / spawn / workers.** The checker collects `StoredFnEffects`
  during checking: every source (named | lambda w/ effect summary, in
  declaration/lift order) keyed by base-resolved signature, and every call
  through a NON-parameter fn-typed binding (params frame excluded — v1
  attribution untouched). `spawn` sites that pass the pre-check fixpoint are
  re-verified against a stored-value-extended fixpoint (union over a
  signature's sources, loose `Type::Param` matching, nested stored calls
  included); `module_state_use` takes the effects and walks per-signature
  pseudo nodes, so a `--workers` refusal names the chain through
  ``a stored `fn(..) -> ..` value`` to the offending source and global.

- **Interpreter.** `Val::Fn` (v1's closure value) IS the closed
  representation: a bare function name evaluates to `FnVal::Named`; a lambda
  in a storage position snapshots captures at its evaluation site and adopts
  the declared signature via `coerce` at the storage boundary (every path —
  let/assign/push-rebind/field/element/constructor — already coerces), which
  supplies parameter wrapping/validation and the return type exactly as a v1
  parameter position did. Module-state fn values dispatch live.

- **Codegen (shared IR → native and wasm).** `llt(fn) = { i64 tag,
  i64 payload }`. One variant per (normalized signature, target) — named fn
  ⇒ payload 0; lambda ⇒ pointer to a malloc'd by-value capture block (never
  freed — the same safe leak as every boxed enum payload; copies share it,
  which is unobservable because captures are read-only). Lifting reuses v1's
  `emit_lifted_lambda` verbatim. Construction sites learn their signature
  from an **expected-type stack** pushed at each storage boundary
  (`if`/`match` arms deliberately push nothing, so conditional sources adopt
  the outer target). Calls lower to ONE direct call to a per-signature
  dispatcher `@__vyrn_fndispatch_<mangle>_<sha256/12>` (switch + direct
  calls; named-source arms coerce args/results through the target's declared
  types, so validated params re-validate and record widths re-layout; the
  default arm is unreachable-by-construction and traps defensively). The
  registry threads through every `Gen` (globals init, functions, generic
  instantiations, HO instances) so tags are module-global; dispatchers emit
  last. Fn values ride `Option`/`Result` payload words inline like `Ref`.
  **The RFC-0023 IR invariant holds verbatim**: every `call` names an
  `@symbol` (pinned over a storage-heavy module), and the wasm module gains
  ZERO table/elem entries versus a v1 baseline (verified at the byte level).

- **v1 interop.** A stored value passed to a v1 `fn`-typed parameter arrives
  as a `{ i64, i64 }` capture parameter of the specialized instance and
  dispatches inside it; direct lambda/named arguments keep the enum-free
  zero-cost path (pinned). Generic HO functions solve their outbound
  parameter from the stored signature's return.

- **Known limits.** Calls are by NAME only (`r.f()` / `chain[0](x)` need a
  binding first — the parser has no call-on-expression form); `Array<T>`
  unification still wants a bound array (not a bare `[..]` literal) at
  generic HO call sites (pre-existing); `vyrn fmt` learned the storage-position
  lambda pipes (tight) with a type-decl guard keeping enum-variant `|` spaced.

- **Consumers.** `examples/closures2.vyrn` (three-way parity citizen incl. a
  canonical trap inside a stored closure), `std/arrays` `sortBy` (an ordinary
  `fn` parameter — stored values flow in), and the shelf server's middleware
  chain (`Array<Middleware>` module state: logging + an `/admin` guard in
  front of routing) — browser-verified end to end over `vyrn dev`.
