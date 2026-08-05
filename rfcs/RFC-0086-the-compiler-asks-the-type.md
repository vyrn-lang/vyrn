# RFC-0086 — The Compiler Asks the Type

- **Status:** Draft. M1 designed.
- **Depends on:** RFC-0084 (records are legal protocol targets, and conformance
  is checked), RFC-0002 §5 (protocols, static monomorphized dispatch),
  RFC-0080 (generic impls, associated types)
- **Evidence (in this repo):** five defects from one cause, listed below.

---

## The cause

Five places decide a property *of a type* by consulting a list written by hand.

| the list | what it decides |
|---|---|
| `own::owner_producing` | which expressions allocate, so which bindings need cleanup |
| three `Type::Stream` matches in `movecheck` | which types are linear |
| `Expr::ArrayLit` / `Expr::MapLit` in the checker | which types a literal can build |
| `codec::encodable` / `decodable` | which types cross a JSON wire |
| `solve_param` | which types unify a parameter |

Each list is complete only while someone remembers to extend it. Each has been
wrong:

- `Expr::ArrayLit` is absent from `owner_producing`, so `let mut xs: Array<T> = []`
  is never released **on any engine**. `Expr::MapLit` is present. Nothing forced
  them to agree.
- `@keys` is absent from the same list, and returns a fresh array.
- `is_string_var` scanned its scopes flat, so an inner `let s = 1` inherited an
  outer `let s = "x"` and the backend emitted `free` on an integer. A miscompile.
- Five expressions read `expected` without resolving an alias, so
  `let m: IntMap = [:]` was refused with "cannot infer; annotate it" to somebody
  who had annotated.
- `solve_param` falls through to `Unit` for `Lazy`, `Record`, `Enum` and `Task`.

Every one is the same mistake. **The compiler asked a list when it should have
asked the type.**

## The rule

**A type declares what it is. The compiler looks it up.**

```vyrn
impl Owned        for Array<T>   // I own heap; here is how to release me
impl FromElements for Array<T>   // `[]` can build me
impl Codable      for Array<T>   // I cross a wire
```

`own` stops matching expressions and asks whether the initializer's type
implements `Owned`. `[]` builds anything implementing `FromElements`. The JSON
domain is whatever implements `Codable`.

A missing entry stops being possible, because there is no list to be missing
from.

## Why this is now buildable and was not before

RFC-0084 M1 made a **record** a legal protocol target, so a user's container can
implement one. PRs #45 and #46 made conformance **checked**, so an `impl Owned
for Ring` cannot silently have the wrong signature or a missing method. Both
landed in the last week.

## The test of the design

**A third party writes a container and gets everything a built-in gets, in the
same words, with no compiler patch.** The corpus must contain one: a user-defined
collection that is reclaimed, built from a literal, and encoded to JSON, with
`std/` and the compiler untouched.

If that example needs one compiler change, the design failed.

## The bootstrap answer

RFC-0080 M3 refused to route `?` through a std protocol for a reason that applies
here: `vyrn run` on a bare file has **no resolver and therefore no `std/`**, so an
answer that must come from a std file is an answer allowed to fail.

So the compiler **seeds** the table with the built-in rows it knows intrinsically.
A user adds rows. The *lookup* is uniform; the built-in *entries* need no file. A
bare file keeps working and a third party still joins.

## No dual paths

When a list is replaced, it is deleted. Not deprecated, not kept behind a flag,
not left as a fast path "for now". A second mechanism that agrees with the first
today is the thing this RFC exists to remove — `owner_producing` and
`Analysis::visit` agreed about `MapLit` and disagreed about `ArrayLit`, and that
is what a dual path buys.

The one exception is *representation*: `Array`'s three words stay primitive
(RFC-0078 withdrew the raw-memory question; RFC-0082 settled that containers over
`Array` are ordinary Vyrn while `Array` is not). **Representation stays
intrinsic. Properties get declared.**

## Milestones

- **M1 — `Owned`, and `own` stops reading expressions.** The protocol, the seeded
  built-in rows, and `owner_producing` deleted. `ArrayLit` and `@keys` are fixed
  by construction rather than by two more arms. Pin: a user container in the
  corpus is reclaimed with no compiler change.
- **M2 — `FromElements` / `FromEntries`.** `[]` and `[:]` build anything that
  implements them. The `Type::Array` / `Type::Map` matches in the checker go.
- **M3 — linearity is a declaration.** The three `Type::Stream` matches in
  `movecheck` become a protocol lookup, so a user's file handle, lock or
  connection gets RFC-0075's obligation. That mechanism was built for one type
  and never exposed.
- **M4 — `Codable`.** `encodable`/`decodable` become the protocol. This is the
  one with the most existing behaviour to preserve, so it is last.

## What this does not decide

`solve_param`'s fall-through. It is the same shape but it is unification rather
than a property, and it may want exhaustiveness rather than a protocol — a
`match` with no `_` arm, so a new `Type` variant fails to compile. Decide in M1
and record it.
