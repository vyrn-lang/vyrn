# RFC-0122 — an option of a place

- **Status:** Implemented (2026-08-29), the third same-day RFC of the
  projection arc. The three-part body shape is checked with its refusals
  (two miss tests, a sugar name, a modify receiver, a value position, a
  `None` test — each with a witness); the `if let` lowering in all three
  engines consumes the shared memoized expansion; `std/slots.tryAt` and
  `std/json.tryField` adopted it, and the witness benches price the same
  live handle at 3 ns through `tryAt` against 50 ns through `get`. One
  scope note from adoption: `tryField` misses on ABSENCE and still traps on
  a non-object, because the payload binding lives in the prologue and the
  prologue runs before the miss is decided — kind confusion stays a bug,
  absence stays data, and a body that wants staged tolerance is the
  recorded extension (a hit-prologue between the miss and the `Some`).
- **Depends on:** RFC-0120 (result capabilities), RFC-0121 (payload places —
  the flat-body discipline this extends), RFC-0060 (`if let`, the consumer)
- **Evidence:** the second of RFC-0120's recorded payers. `slots.get` answers
  `Option<T>` and pays a deep copy per hit — 47 ns against the 9 ns the
  trapping `s[h]` reads the same element for — because an owned `Option` must
  own its payload. The miss is the whole reason `get` exists; the copy is
  not.

## The gap

A projection traps on a miss (`s[h]`, `j.field(k)` — RFC-0120/0121), and
that is right where the caller has already checked. The OTHER half of every
container API is the read that asks: `get`, `fieldAt`, `elemAt` answer
`Option`/`JNull` — and each pays a copy per hit, because an `Option<T>` is
an owned value and owning the payload means duplicating it.

The observation that closes this: **at every call site that matters, the
`Option` never outlives the `if let` that unwraps it.** The payload binder
of an `if let` arm is a borrow in every engine already ("payload borrows,
never drop-tracked"). So a hit does not need an `Option<T>` at all — it
needs the arm taken with its binder aliased to the place, and a miss needs
the other arm. The `Option` is a fiction both spellings agree on.

## The design

### The declaration

```vyrn
fn tryAt(read self, h: Handle<T>) -> read Option<T> {
    let mut miss = h.owner != self.owner || h.slot < 0
        || h.slot >= self.gens.length
    if !miss {
        miss = self.gens[h.slot] != h.gen
    }
    if miss {
        return None
    }
    return Some(self.vals[h.slot])
}
```

No new syntax: the result capability is RFC-0120's, the type is `Option<T>`,
and the combination is what makes a member an OPTIONAL projection. Its body
is the flat shape RFC-0121 established, one statement stricter:

- a prologue of ordinary statements, with no `return` in them;
- then exactly `if <miss> { return None }`;
- then exactly `return Some(<place>)` — the place under the same rules as
  any read projection (rooted in the receiver, transitively through
  borrowing `let`s).

The rigidity is load-bearing, not stylistic: statements between two miss
tests would run after the first miss decided (the bounds guard exists so
`self.gens[h.slot]` is not read out of range), so there is one prologue,
one decision, one place — and a body that wants staged guards folds them
into the prologue as data, as `tryAt` folds its four into `miss`.

### The consumption

An optional projection is read where it is tested, and nowhere else:

```vyrn
if let Some(v) = s.tryAt(h) {
    total = total + v.n     // v is the element's place, borrowed, arm-scoped
}
```

`if let` (and through its desugar, `while let`) is the only legal position.
The site inlines as: prologue, then a branch on the miss — the else arm on
one side, the then arm with the binder aliased to the place on the other.
No `Option` is constructed on either path. Every other position — a `let`,
an argument, a `match`, a return — is refused with the rule in one
sentence: "an optional place is read where it is tested — write
`if let Some(x) = ..`". A caller that needs the value to escape keeps the
copying reader; that is what it is for.

### The sugar names stay plain

`at`, `atSet` and `nth` are dispatched by `a[i]`, `a[i] = v` and `for` —
sites that consume a place unconditionally. An optional projection under
one of those names is refused at the declaration.

## The adoption

- `std/slots` gains `tryAt` beside `get`. `get` keeps its signature and its
  copy — an owned `Option<T>` is still the right answer for a value that
  outlives the read — and its doc says which spelling to reach for.
- `std/json` gains `tryField` beside the trapping `field` and the copying
  `fieldAt`: the three-way choice (trap where checked, borrow where tested,
  copy where escaping) is now spellable per call site.
- The witness example prices `tryAt` against `get` on the hit path.

## What this is not

- Not multi-exit projections: the miss arm carries no place, which is what
  makes two exits lower as one branch.
- Not a nullable place, and not a stored view: the borrow lives exactly as
  long as the arm, by the same scope discipline as every projection.
- Not a change to `get`/`fieldAt`/`elemAt`: escape still copies, and the
  tolerant owned readers keep every caller they have.

## Milestones

- **M1** the checker: optional-projection body validation (the three-part
  shape, each refusal with a witness), the value-position refusal, the
  sugar-name refusal, and `if let` dispatch with the expansion recorded.
- **M2** the engines: the `if let` lowering in the interpreter, the native
  backend and the direct wasm backend consumes the shared expansion —
  prologue, branch, aliased binder — through the same memo the flat
  projections use.
- **M3** `std/slots.tryAt`, `std/json.tryField`, the witness example and
  its benches; three-way parity under the free audit.
