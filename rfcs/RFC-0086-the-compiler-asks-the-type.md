# RFC-0086 — The Compiler Asks the Type

- **Status:** M1 implemented. M2–M4 designed.
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

- **M1 — `Owned`, and `own` stops reading expressions.** *Implemented.* The
  protocol, the seeded built-in rows, and `owner_producing` deleted. `ArrayLit`
  and `@keys` are fixed by construction rather than by two more arms. Pin: a user
  container in the corpus is reclaimed with no compiler change. See "M1 — as
  landed" below.
- **M2 — `FromElements` / `FromEntries`.** `[]` and `[:]` build anything that
  implements them. The `Type::Array` / `Type::Map` matches in the checker go.
- **M3 — linearity is a declaration.** The three `Type::Stream` matches in
  `movecheck` become a protocol lookup, so a user's file handle, lock or
  connection gets RFC-0075's obligation. That mechanism was built for one type
  and never exposed.
- **M4 — `Codable`.** `encodable`/`decodable` become the protocol. This is the
  one with the most existing behaviour to preserve, so it is last.

## M1 — as landed

`owner_producing` is gone. It is replaced by **two** questions, and separating
them is the whole of the milestone:

- **Does this initializer transfer a value nobody else holds?** A property of the
  expression form. `Analysis::transfers` answers it with a `match` that has no
  `_` arm, so a new `Expr` variant has to answer.
- **How is a value of this type released?** A property of the type.
  `own::Owned::release_kind` answers it, and nothing else does.

The split is not a compromise. Transfer **cannot** be asked of the type:
`at(a, 0)` and `m.keys()` are both an `Array`'s business and only one of them
allocates. Release **cannot** be read off the expression: that is the mistake the
RFC was written from. One list became one protocol and one exhaustive match, not
two lists.

### The seeded rows, and the bootstrap

`release_kind` matches on the resolved type with **no `_` arm** — 36 `Type`
variants, each of which had to say whether it owns heap. Five say yes (`String`,
`Ref`, `Array`, `SmallArray`, `Map`), and a `Stream` says no *here* because
RFC-0075 M2b pushes its own release frame and answering twice would close it
twice. The rows are seeded in the compiler, so `own::analyze` on a bare
`fn main()` with no imports still frees its `String`. That is pinned by
`a_bare_file_with_no_imports_still_frees_its_string`, which asserts the source
contains no `import`.

A **declared** row wins over the seed, and it is keyed by the type's name — so
`impl Owned for Ring` is what `Ring` means rather than what `Ring` is made of.
The compiler knows two strings about the protocol (`Owned`, `release`) and
nothing else, exactly as it knows two about `Fallible`.

### The design's own test

`examples/ownedcontainer.vyrn` declares `protocol Owned` itself, so it imports
nothing and `std/` is untouched. It defines **two** containers — a `Ring` over an
`Array` and a `Tally` over an `Int64` — and the second one cost no compiler
change, which is the point of having it. All three engines run the declared
`release`, at the same point, in the same order. `parity: 114 checked, 10
skipped, 0 failed`.

The interpreter had to change to run it: a `release` is ordinary Vyrn and can
print, so it is the second thing after a cell release that auto-reclamation makes
observable. Its drops now also run **newest binding first**, which is the order
both compiling backends already emitted.

### What changed in the corpus

113 of 114 examples gained frees. **No example lost one**, and no `Ref` release
moved anywhere. The whole diff is four shapes:

1. `let mut out: Array<Int64> = []` — `Expr::ArrayLit` was the absent arm. Four of
   these live in `std/num`, which every example links, so every example is at
   least `+4`.
2. The transfer that follows. A function whose only owner was such a literal was
   not *owned* either, because `return out` reached the "borrowed" branch. Now the
   caller frees the result: `capturefn`'s `applyAll`, `closures2`'s `sortBy`,
   `std/bench`'s `sortedCopy`.
3. `m.keys()` — the other absent arm.
4. A nominal type over a heap type. `release_kind` resolves the declaration, so
   `type Nums = Array<Int64>` is an `Array`. The old `returns_owned_kind` did not
   resolve, and `is_string_like` was a hand-rolled half of this for `String` only.

### The one that would have been a wrong free

`expr_type` first answered `Array` for **every** `Expr::ArrayLit`. That freed
`let xs = [1, 2, 3]`, which has no annotation and is an `ArrayN` held **inline**:
the drop site loaded three words from a stack slot and passed the first to `free`.
`ifexpr.vyrn` exited `0xC0000374`. An array literal does not name its own type —
one syntax, three layouts — so only the annotation may answer, and
`an_unannotated_array_literal_is_not_released` pins it.

This is why the split matters in the other direction too. `transfers` may be
generous, because being wrong there leaks. `release_kind` may not, because being
wrong there frees the wrong bytes.

### Also folded in

The textual backend's `Stmt::Drop` had its own `Type -> DropKind` match, a second
copy of the same answer. It now asks `release_kind`, which is what the direct
backend's `rel_for` had already been arranged to do for its own two paths. So
`drop x` and a block-exit release read one table on every engine.

### Legacy noted, not removed

- **`afree`** has **zero** uses in `examples/` and `std/`, and the direct wasm
  backend has no lowering for it — a surface form one engine cannot compile.
  `drop x` covers it and works everywhere. It should go.
- **`array()`** has one use left, `examples/aliascontext.vyrn`, which exists to
  pin the alias-resolution defect PR #64 fixed. The spelling is dead; the pin is
  not.
- **`push(a, v)`** as a free function is the desugar target for `xs.push(v)`, so
  `own` must keep the name. `examples/region.vyrn` still writes the raw form by
  hand and could be migrated.

## What this does not decide

`solve_param`'s fall-through. **Decided in M1: exhaustiveness, not a protocol,
and not in M1.**

It is unification, not a property. Nothing is declared, and there is no third
party who *could* declare it — a protocol needs somebody to write an impl, and
"how do two type constructors match" has no author but the compiler. The rule is
already mechanical: same constructor, recurse on the children; different
constructors, bind nothing. So the fix is the shape of the `match`, not a table:
pair the two types and give every constructor an arm, with no `_`, so a new
`Type` variant fails to compile instead of silently binding nothing.

It is deferred because the blast radius is monomorphization rather than
reclamation, and the two backends **already disagree about what a `None` means** —
the LLVM emitter substitutes `Unit` and lowers it to `void`, the direct backend
refuses. Filling in `Lazy`, `Record`, `Enum` and `Task` turns some silent `Unit`
into a real type and some refusal into a compile, and neither belongs in a
milestone about when memory is freed.
