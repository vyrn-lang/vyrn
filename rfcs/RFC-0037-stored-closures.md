# RFC-0037 — Stored Function Values by Defunctionalization (Closures v2)

- **Status:** Draft (design locked)
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
