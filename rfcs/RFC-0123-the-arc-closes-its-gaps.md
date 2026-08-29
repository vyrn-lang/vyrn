# RFC-0123 — the arc closes its gaps

- **Status:** Implemented (2026-08-29) — M1–M3 landed; M4 weighed and
  closed as "not yet", with the cost recorded below.
- **Depends on:** RFC-0120 (result capabilities), RFC-0121 (payload places),
  RFC-0122 (optional projections) — whose recorded gaps this RFC works
  through, in the order of what each is worth.
- **Evidence:** the gaps' own records. Each was written down at the moment
  an implementation refused it, with the workaround in the refusal message —
  and a recorded gap is a promise to either close it or say why not.

## M1 — staged tolerance: a hit may have a prologue

RFC-0122's optional projections are three parts: a prologue, one miss test,
one `Some(place)`. The rigidity was load-bearing against statements BETWEEN
miss tests — they would run after the first miss decided. But statements
AFTER the single decision run only on the hit, and refusing those forced
`json.tryField` into a corner its own doc admits: it misses on absence and
still traps on a non-object, because the payload binding lives in the
prologue and the prologue runs before the miss is decided.

The shape grows one segment: prologue (no returns), exactly one
`if <miss> { return None }`, then a HIT PROLOGUE (no returns — statements
that run only when the place exists), then `return Some(<place>)`. The
place's roots may trace through the hit prologue's borrowing `let`s exactly
as through the prologue's. `tryField` then writes what it always meant:
scan inside an `if let` (a non-object scans nothing), miss on `hit < 0`,
and bind the payload AFTER the miss is decided — absence and kind confusion
both miss, and the trap is gone.

## M2 — a protocol may declare a projection

`fn at(read self, i: Int64) -> read T` in a `protocol` body, refused since
RFC-0120 with "the recorded gap". A protocol member whose result carries a
capability is satisfied by a projection member of the impl — same name,
same receiver capability, same result capability, matching signature.
Dispatch is unchanged (projections dispatch by receiver type, never through
a protocol vtable), so what this buys is the CONTRACT: a user-defined
protocol can require the projection the way `Index`/`Iterate` require
theirs, and conformance says so at the impl rather than at the first call
site that misses it. Through a `<T: P>` bound the projection stays
undispatchable (a projection inlines, and a type variable has no body to
inline) — refused at the call with its own sentence, which is the same line
associated types drew in RFC-0080 M2.

## M3 — a projection's result can be a receiver

`doc.field("k")[1]` — refused since RFC-0121 because the checker could type
it and no engine could dispatch it: the receiver probes resolve names, not
call results. The probes learn exactly that: the type of a projection call
is its member's RAW declared result, when that result is a concrete named
type; `a[i]` on a builtin container answers the element. Raw on purpose —
the interpreter's probe may hold only a value's type KEY, which solves no
substitution, and one rule every engine can keep beats a sharper rule one
engine cannot. A result that is a bare type parameter (`Slots<T>`'s `at`
answers `T`) keeps the refusal, now stated precisely: the next link has no
impl to resolve against, and the `let` the old message asked for is still
the answer there.

## M4 — the writable name: `x.f(i) = v`

Named `-> modify T` projections are today reachable only as `atSet` behind
`a[i] = v`. The gap closes if the assignment grammar accepts a call-shaped
place. This is the one milestone bought with AST rather than with rules —
a new statement shape every walker must learn — and the RFC reserves the
right to weigh that churn against a demand nobody has measured, and to
close the gap as "not yet, and here is the cost that decided it" if the
weight says so. The decision, either way, is recorded here.

**Decided: not yet.** The costs and the value, as weighed:

- An assignment target is not an expression in this language. `Assign`,
  `SetField` and `IndexSet` are separate statement shapes, each rooted at a
  plain binding name — the v1 restriction that keeps a store's root visible
  to `own` without a place analysis. A call-shaped target is a FOURTH
  shape, and a statement shape is the most expensive thing in the compiler
  to add: the parser, the checker, ownership, movecheck, three engines, the
  lowering walk, the formatter, and the loader's hygiene and inlining walks
  all pattern-match `Stmt`, and every one of them would carry the new arm.
- Nobody has asked. Every pattern-2 payer that named a missing extension
  named a READ (`j.field(k)`, `s[h]`, `tryField`) — the twenty-list holds
  no store that wants to be spelled `x.f(i) = v`. The workaround is not a
  contortion but the ordinary thing: `fn setF(modify self, i: Int64, v: T)`
  is a method call today, costs the same call the sugar would cost, and
  says "this mutates" at least as loudly.

A grammar change with no payer buys syntax, not capability. When a dogfood
program turns up holding the workaround often enough to hurt, this
milestone reopens with a measured case; until then the gap stays recorded,
and this section is the "why not" the record promised.

## Milestones

- **M1** the four-part optional shape in `check_places`,
  `optional_inline`, the three engines' `if let` lowerings and the lowering
  walk; `json.tryField` misses on kind; witnesses for the new shape and the
  still-refused shapes.
- **M2** protocol members with result capabilities parse; conformance
  matches them against impl projections; bound-dispatch refused with its
  own sentence; a user protocol witness.
- **M3** receiver probes answer projection-call types in the interpreter
  and both backends; the checker's chain refusal narrows to the genuinely
  undispatchable; the chained witness runs three ways.
- **M4** the weighed decision on call-shaped assignment targets, and
  whichever side it lands on, written down.
