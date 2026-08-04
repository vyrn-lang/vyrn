# RFC-0080 — Associated Types and Generic Impls

- **Status:** **M1 and M2 shipped. M3 shipped in half** — `?` resolves through
  `Fallible` for user types; `Option` and `Result` stay nominal, and the "As
  landed — M3" note says why that is a refusal rather than an omission.
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

(**Two of those three sentences turned out to be wrong**, and M3's note below
carries the corrections. `std` does *not* implement `Fallible` for `Option` and
`Result` — it cannot, because a bare `vyrn run` has no `std`. And
`Success`/`Failure` cannot be replaced wholesale, because they are a *parser*
artifact and a protocol lives in the checker. The sentence that held is the one
that mattered: every program written today does keep working unchanged, and the
corpus is byte-identical.)

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

### M2 — Associated types — **SHIPPED**

`type Name` in a protocol body; `type Name = Concrete` in an impl; the type
resolved when the impl is selected. Pin: a protocol whose method returns its
associated type, implemented for two types with different resolutions, monomorphized
correctly in one program.

**As landed.** The pin is `examples/assoctype.vyrn` — one protocol whose
`valueOr` both takes and returns its `Output`, three impls resolving it three
ways (`impl<T> Unwrap for Option<T>` binding it to the head's own `T`, and two
concrete impls binding `Int64` and `String`), byte-identical across the three
engines. `examples/assoctype_unbound.vyrn` is the refusal, both directions. What
the plan above got wrong or left out:

- **"resolved when the impl is selected" is one step later than where it
  happens.** Selection is a *call-site* act; an associated type is fixed at the
  **impl**, before any call exists. So it is substituted where the impl's methods
  are **parsed**: `fn valueOr(self, f: Output) -> Output` inside
  `impl<T> Unwrap for Option<T> { type Output = T … }` leaves the parser as
  `fn(self: Option<T>, f: T) -> T`, an ordinary generic function. M1's call path
  then unifies `Option<Int64>` against `self` and gets `Int64` back with no
  knowledge that an associated type was ever involved. Checking whether that
  seam "carries a return type that is not a parameter of anything" turned out to
  be the wrong question: after substitution the return type is a parameter of the
  receiver, which is what "the implementing type fixes it" *means*.

- **No new `Type` variant, so no walk census.** An associated type is a
  `Type::Param` — the variant a `fn f<T>` binder already produces. Inside the
  protocol it is `Param("Output")`, a variable nothing has bound yet; inside an
  impl it never survives the parse. `substitute`, both unifiers, `collect_params`,
  `walk_type`, `contains_heap`, `fn_sigs_match`, the loader's three walks, the
  parser and the schema reflector all needed exactly nothing. RFC-0075 M2's
  ten-walk hunt has no counterpart here.

- **The backends did not change, and this time that is a claim rather than luck.**
  All three read `program.protocols` only to map a method name to its protocol;
  none reads a `MethodSig`'s types. The frontend is the whole feature.

- **A caller cannot name the associated type, and no syntax was invented.**
  Inside `fn f<T: Unwrap>(x: T)`, `x.valueOr(..)` has no selected impl, so
  `Output` has no value. Rust writes `T::Output`; Vyrn refuses by name instead,
  because M3 is the milestone that decides whether that spelling is needed and
  guessing now would be guessing at M3's shape. The refusal matters more than it
  looks: typing the call as the bare `Type::Param` would have let it reach
  codegen, where a parameter outside a monomorphization lowers to `void` — a
  smaller function, not an error.

- **Resolving during the parse costs an ordering rule**, and it is the only cost:
  a `type` member must precede the methods that name it. Deferring would mean a
  second substitution pass over every method **body**, since only `params`/`ret`
  are reachable from `types::substitute`, and body annotations are exactly where
  an unsubstituted parameter reaches codegen. The parser names the rule where it
  fires rather than letting it surface as `unknown type Output` several lines
  above the binding.

- **Method conformance was still unchecked, and was before this milestone.**
  Nothing verified that an impl method's signature matched the protocol's — true
  of M1 and of RFC-0002 §5, and M2 did not change it. What M2 does check is that
  an impl binds exactly the associated types its protocol declares, in both
  directions. The binding was not decorative despite that gap: an impl whose
  `type Output = ..` is wrong fails to type-check in its own body, because the
  body was parsed against it.

  Conformance was closed separately in 2026-08 and inherits a limit directly from
  the bullet three above: an impl's methods never survive the parse with an
  associated type in them, and `ImplBlock::assoc` keeps only the names it bound,
  so a signature position naming one has nothing left to compare against and is
  skipped. `Output` versus `T` in `impl<T> Unwrap for Option<T>` is the same type
  and no longer says so anywhere. Arity, every other parameter, the return type
  and a missing method are all checked; see RFC-0002 §5.

### M3 — `Fallible`, and the operators resolve through it

Only after M1 and M2 are green. `std` gains `Fallible` with impls for `Option`
and `Result`; `check_try` and the `??` desugar consult it instead of matching two
type names. Pin: the existing `?`/`??` corpus passes **unchanged** — that is the
whole test — plus one user-declared four-variant enum propagating through `?`
with its failing variant intact.

M3 is where this could go wrong in a way M1 and M2 cannot, because it touches an
operator every program uses. It should be attempted only when the two below it
have been green for a while, and abandoning it leaves M1 and M2 standing.

**As landed — partially, and the refused half is the interesting one.**
`std/fallible.vyrn` declares the protocol, `?` resolves through it for any
operand that is not an `Option` or a `Result`, and `examples/fallible.vyrn` is
the four-variant pin — three engines byte-identical. The corpus passed
**unchanged**: 106 of 106 pre-existing examples emit byte-identical IR, 238 `?`
lowerings among them, and no example was edited.

**`Option` and `Result` do NOT resolve through the protocol, and will not.**
That is half the sentence above this one, refused for four reasons found in the
order they bite (`types::FALLIBLE` carries them beside the code):

1. **`vyrn run` on a bare file has no resolver and therefore no `std/`** — the
   interpreter's own tests are exactly that. Routing `x?` on an `Option` through
   a std protocol makes the most common operator in the language depend on a
   module lookup that is *allowed to fail*. The RFC's "`std` gains `Fallible`
   with impls for `Option` and `Result`" assumed a prelude that does not exist,
   and RFC-0062 deliberately went the other way (explicit `std/option`,
   `std/result` imports).
2. **`?` on a `Result` checks `assignable(e, re)`.** `Fallible` has one
   associated type and it is the *success* payload; the error check has nowhere
   to live. A `type Error` would give it one — and then `Option`'s `Error` is a
   payload that does not exist and a four-variant enum's is the whole enum. Two
   associated types to re-derive a check that is one line today.
3. **The diagnostics are better nominal.** ``line N: `?` propagates error {e},
   but the function returns Result<_, {re}>`` degrades to "does not implement
   `Fallible`" on the protocol path. A worse message on this operator is a real
   cost.
4. **The lowering is inline and would stop being.** `Option`/`Result` `?` is a
   tag test and an `extractvalue`; through the protocol it becomes two calls
   whose bodies re-`match` the value the branch already tested. `std/json`,
   `std/scan` and `std/num` use `?` in loops.

So the operator is nominal for the two shapes the language builds in and open
for everything else. Calling that "M3 shipped" would be reading the milestone
generously; what shipped is the capability, not the unification.

The rest of what the plan got wrong or left out:

- **`Success`/`Failure` were not "replaced wholesale" — they cannot be, and the
  reason is structural rather than incidental.** They exist because the `??`
  desugar runs in the **parser**, which holds no types. A protocol lives in the
  checker, which is one phase too late to change what the parser emits; it sits
  *above* the two patterns rather than replacing them. So `??` does not follow
  `?` onto a user enum at all, and it is not a matter of effort: "the failure
  side" of a four-variant sum is a wildcard over N−1 variants, and `Pattern`
  still has none. `?` reaches a `Fallible` enum precisely because it never
  pattern-matches — it tests and copies. `check_match_enum` now says that in the
  source's own words instead of surfacing ``expected an enum variant pattern``,
  which named a pattern nobody wrote.

- **The four-variant claim holds, and a negative control proves *why* it
  holds.** `ServerError("upstream timed out")` reaches the caller with its
  `String` intact, and nothing in the protocol mentions failure payloads.
  Narrowing the direct backend's propagating `memory.copy` from the aggregate's
  width to 8 bytes makes exactly that payload — and nothing else — come out
  empty, on the wasm column, on this example. The payload survives *because* the
  bytes are copied whole; there is no residual because there is nothing to
  reconstruct.

- **The protocol owes two answers, and `success` has to be total.** `success`
  is called only after `isSuccess` answered true, so its failing arms are
  unreachable — and they still owe an `Output`. RFC-0079 M1's `panic` is what
  says so. The design section did not mention this and it is the one thing an
  impl author must be told.

- **Three engines, not two.** RFC-0077 M5 made `--target wasm` the direct
  backend unconditionally, so the wasm parity column is `direct.rs` and not
  clang. `?` had to be lowered there as well as in the textual emitter and the
  interpreter. Both backends park the operand in a slot under a reserved name
  (`@try`) so the two impl calls can be spelled as an `Expr::Var` and go through
  the existing `call` whole — including its generic path, which is why
  `impl<T> Fallible for Slot<T>` monomorphizes at two payload types with nothing
  new written for it.

- **The checker reads `Output` by typing the call the backends will emit**
  rather than by a substitution rule of its own. `self.call("Fallible__K__success",
  [operand])` is the same generic-inference path a hand-written `x.success()`
  takes, so M1's unification and M2's substitution answer the operator's type
  question without either being told an operator asked.

- **The compiler knows the name `Fallible` and nothing else.** A program that
  declares the protocol itself is indistinguishable from one that imports
  `std/fallible` — which is what makes the std module a canonical spelling
  rather than a dependency of the operator.

- **`?` inside a generic function was a non-event.** `fn wrap<T>(xs: Option<T>)`
  takes the nominal `Option` arm; `T` is never asked for. M2's "a caller cannot
  name the associated type" never fires, because the operand's *constructor* is
  known even when its argument is not.

## What this does not decide

**`protocol P<T>`** — generic protocols proper, for types that implement
something many times. Named above as a real feature for a different job.

**Error conversion in `?`** — Rust's `From`-based widening. It is the feature
that forced the residual, and adding it later would reopen everything this RFC
avoids. Not proposed, and worth a deliberate decision rather than a drift.

**The raw-memory view** — RFC-0078's open question **A**, untouched and still the
larger of the two.
