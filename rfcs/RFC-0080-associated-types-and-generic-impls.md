# RFC-0080 — Associated Types and Generic Impls

- **Status:** **M1 shipped**; M2 and M3 designed, not started. Whether to build
  them is a separate decision from whether the design is right.
- **Depends on:** RFC-0002 §5 (protocols, static monomorphized dispatch),
  RFC-0023 (monomorphized function values — the unification machinery this
  extends), RFC-0079 (`?`/`??`, the motivating consumers)
- **Does not supersede anything.** RFC-0079's nominal `?` and `??` keep working
  unchanged; this RFC is the path by which they *could* stop being nominal, and
  it is additive at every step.

## The gap, stated without reference to `?`

`Show` cannot be implemented for `Option<T>`.

```vyrn
impl Show for Option<Int64> { ... }    // rejected
impl<T> Show for Option<T> { ... }     // does not parse
```

(This block originally called the first line "legal, and useless". It was
neither — `ok_target` in `checker.rs` admitted `Int | Bool | Str` and a named
enum, and `Option` fell through to `false`, so a protocol could not be
implemented for `Option` at *any* instantiation. M1 therefore adds a capability
rather than generalising one, which makes the M1 pin the first program in the
repo to dispatch a protocol on an `Option` at all.)

[`impl_block`](../compiler/vyrn-frontend/src/parser.rs) reads
`impl <ident> for <type>`. There is nowhere to bind a type variable, so a
protocol can only be implemented for *concrete* types — which means implementing
it for a generic type requires one impl per instantiation, including
instantiations in user code nobody has written yet. That is not a limitation of
`Option`; it applies to `Array<T>`, `Map<K, V>`, `Result<T, E>` and every generic
type a user declares.

This is the whole gap. `?` and `??` are downstream of it.

## Two features, and why it is exactly two

### 1. Generic impls

```vyrn
impl<T> Show for Option<T> { ... }
```

The impl head binds `T`, and selecting an impl means unifying the concrete
receiver type against that head. Vyrn already unifies types to monomorphize
generic functions (RFC-0023), and dispatch is already static, so the receiver's
concrete type is known at every call site. This is new syntax and a new
selection step over machinery that exists.

**Overlap is rejected, not ranked.** `impl<T> P for Option<T>` beside
`impl P for Option<Int64>` is an error at declaration, not a specialization
opportunity. Rust allows the latter only behind an unstable feature and has for
years; there is no reason to import that problem to get `Show` on `Option<T>`.
One impl per `(protocol, type constructor)` pair, checked when the impl is
declared rather than when it is used, so the error names both impls.

**That rule is wider than overlap, and this paragraph originally conflated the
two.** `impl P for Option<Int64>` beside `impl P for Option<String>` names
*disjoint* types that can never both match a receiver — no ambiguity exists —
and it is refused too, because dispatch keys on the constructor. So the honest
statement is one impl per constructor, of which rejecting true overlap is a
consequence rather than the reason. Rust permits the disjoint pair; Vyrn is
narrower here, and the cost is real for a case like `Array<UInt8>` serialising
differently from `Array<Int64>`. Lifting it means keying a *list* per
constructor and refusing only heads that actually unify — tractable, not done,
and not needed for anything M1 exists to deliver. The diagnostic says
"collides", never "overlaps", so it does not describe an ambiguity that is not
there.

### 2. Associated types

```vyrn
protocol Fallible {
    type Output
    fn isSuccess(self) -> Bool
    fn success(self) -> Output
}
```

A type member the *implementing type* fixes. Today a protocol body parses only
`fn` signatures.

**Why associated rather than a protocol parameter.** The alternative spelling is
`protocol Fallible<T>`, and it fails for a specific reason: nothing stops one
type implementing it twice with different `T`, and then `x?` has nowhere to say
which. The operator is two characters; there is no annotation position. So the
output type must be *determined by* the input type, and "determined by" is
precisely what an associated type means and what a parameter does not.

Rust reached the same place from the same pressure: `Iterator` has `type Item`
rather than `Iterator<Item>`, because `for x in v` has nowhere to write it
either.

**`protocol P<T>` is a genuine third feature and is not proposed here.** It is
the right tool when a type *should* implement something many times —
`Into<String>` and `Into<Bytes>` on one type. Nothing in this RFC needs it, and
adding it alongside associated types invites the overlap question this RFC
closes.

## What this buys, in order of how sure it is

1. **`Show`, `Eq` and every future protocol on generic types.** Immediate, no
   design questions, and it is the thing users hit first.
2. **Container protocols.** `Array<T>` and `Map<K, V>` can carry protocol impls
   rather than being special-cased.
3. **Generalizing `?` and `??`.** Real, and the least certain — see below.

## The `?` generalization, and the part Vyrn gets for free

Rust's `?` needs two traits and an invented intermediate type. Its desugar is
roughly:

```rust
match Try::branch(x) {
    ControlFlow::Continue(v) => v,
    ControlFlow::Break(residual) => return FromResidual::from_residual(residual),
}
```

The failure arm takes the value **apart**, produces a *residual* carrying the
error without the old success type, and **constructs** the return value from it.
The residual exists because Rust's `?` changes types: `Result<T, io::Error>`
propagating out of a function returning `Result<U, MyError>` must extract the
error, convert it, and build a new value.

**Vyrn's `?` does not change types and does not take anything apart.** It
requires `assignable(e, re)` — the same error type, no conversion — and it
propagates by copying **the whole sum, byte for byte**: `memcpy` of the aggregate
in the direct backend, `ret { i1, i64, i64 } %agg` in the textual one. That is
why it demands identical layout on both sides.

So there is no intermediate state to name and no residual to invent. The protocol
needs only to say *which side of the sum this value is on* and *how to read the
success payload*; propagation stays a copy.

**A consequence worth stating, because it is a feature rather than a
concession:** an enum with more than two variants works without any mapping.

```vyrn
enum Http { Ok(Body), Created(Body), NotFound, ServerError(String) }
```

Multiple **successes** are fine if their payloads unify to one `Output` — `Ok`
and `Created` both give `Body`, and `x?` yields a `Body`. What is lost is *which*
success it was, which is what writing `?` asked for. If the success payloads do
not unify, the impl author has to choose; there is no single type to unwrap to.

Multiple **errors** are entirely free. `ServerError("x")` propagates as
`ServerError("x")` because the bytes are copied whole. Rust would route that
through a residual and reconstruct it.

The constraint that stays is today's constraint: `?` propagates into the *same*
type. Variant count is free; changing types on the way out is not.

And `??` discards the error uniformly regardless of variant count — a caller who
wants to know which failure it was writes `match`, which needs none of this.

## Forward compatibility, which is why none of this is urgent

RFC-0079 shipped `?` and `??` nominal — resolved against `Type::Option` and
`Type::Result` by name. When this RFC lands, `std` implements `Fallible` for
those two, the operators resolve through the protocol, and **every program
written today keeps working unchanged**. The change is purely additive. Nothing
in RFC-0079 forecloses it: `Success`/`Failure` are unspellable internal patterns
that can be replaced wholesale.

That is the reason to build this for `Show` and containers, and to treat the
operator generalization as a payoff that arrives later rather than a deadline.

## Milestones

### M1 — Generic impls — **SHIPPED**

`impl<T> P for C<T>`: parse the binder, unify the receiver against the impl head
during selection, reject overlap at declaration with both impls named. Pin:
`Show` implemented once for `Option<T>` and called on `Option<Int64>` and
`Option<String>` in one program, three engines byte-identical; and a rejected
overlapping pair whose diagnostic names both.

M1 is independently useful and ships alone. If M2 never happens, `Show` on
`Option<T>` still works.

**As landed.** Both pins hold — `examples/protocol.vyrn` carries the generic
impl through the three-engine sweep, `examples/protocol_overlap.vyrn` is the
refusal. What the plan above got wrong or left out:

- **"a new selection step over machinery that exists" overstated the new
  step.** There is no selection *step*. Selection was already
  `type_key(receiver) → mangled impl function → call`, and `call` already had
  the RFC-0023 generic path. Widening `type_key` so a generic type keys on its
  **constructor alone** (`Option<Int64>` and `Option<String>` both key
  `Option`) makes the existing call path do the unification — the receiver
  `Option<Int64>` meets the impl method's `self: Option<T>` as an ordinary
  argument, and `T` binds the way it always has. Three backends dispatch
  through that one key function, so all three followed without being touched.
  The interpreter needed two lines: its runtime key is computed from a `Val`,
  and `Val::Option`/`Val::Result` had no key at all.

- **Bounds on the binder were not optional.** The RFC writes `impl<T> Show for
  Option<T>`, but the body of that impl cannot show its payload without
  `T: Show`, so the useful spelling is the bounded one. The `fn f<T: Ord>`
  binder parser was extracted and shared rather than a second one written.

- **Overlap keys on the constructor, so it also rejects two concrete impls for
  one constructor** — `impl Show for Box<Int64>` beside `impl Show for
  Box<String>`. That is the stated "one impl per (protocol, type constructor)"
  rule, but the RFC only ever illustrated generic-versus-concrete, and the
  concrete-versus-concrete case is the one a user is more likely to write by
  accident.

- **A worse diagnostic had to be suppressed to let the good one through.**
  Impl methods flatten to mangled top-level functions, so two overlapping impls
  produced ``function `Show__Option__show` defined twice`` — naming a symbol
  nobody wrote — *before* the overlap error. The flattener now keeps the first
  and lets the checker speak.

- **The formatter had a matching gap.** Its generic-angle rule required the
  token before `<` to be a name, so `impl<T>` reformatted to `impl < T >`. This
  is the second milestone in a row to find a fmt rule that assumed the syntax
  it was written against; `vyrn fmt --check` over the corpus is what caught it.

- **Legal impl targets widened beyond what the pin needed:** `Option`,
  `Result`, and an application of a user generic enum (`Box<T>`). All three
  keep a distinct runtime shape, which is the existing rule for what may carry
  an impl. `Array`/`Map` were left out — not because they cannot dispatch, but
  because their builtin methods would collide and nothing needed it.

- **Not reached, and not a blocker:** nested `Option<Option<T>>` still hits the
  pre-existing "nested Option/Result is not supported in v0.1" rejection, so a
  generic impl cannot be exercised at two levels. Unrelated to this RFC and
  unchanged by it.

### M2 — Associated types

`type Name` in a protocol body; `type Name = Concrete` in an impl; the type
resolved when the impl is selected. Pin: a protocol whose method returns its
associated type, implemented for two types with different resolutions, monomorphized
correctly in one program.

### M3 — `Fallible`, and the operators resolve through it

Only after M1 and M2 are green. `std` gains `Fallible` with impls for `Option`
and `Result`; `check_try` and the `??` desugar consult it instead of matching two
type names. Pin: the existing `?`/`??` corpus passes **unchanged** — that is the
whole test — plus one user-declared four-variant enum propagating through `?`
with its failing variant intact.

M3 is where this could go wrong in a way M1 and M2 cannot, because it touches an
operator every program uses. It should be attempted only when the two below it
have been green for a while, and abandoning it leaves M1 and M2 standing.

## What this does not decide

**`protocol P<T>`** — generic protocols proper, for types that implement
something many times. Named above as a real feature for a different job.

**Error conversion in `?`** — Rust's `From`-based widening. It is the feature
that forced the residual, and adding it later would reopen everything this RFC
avoids. Not proposed, and worth a deliberate decision rather than a drift.

**The raw-memory view** — RFC-0078's open question **A**, untouched and still the
larger of the two.
