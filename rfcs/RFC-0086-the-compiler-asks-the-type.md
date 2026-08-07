# RFC-0086 — The Compiler Asks the Type

- **Status:** M1 and **M3 implemented**; M1 extended twice since. M2 blocked —
  the protocol needs a receiver-less method and the language has none; see
  "M2 — what it needs". M4 designed.
  **M3 shipped `impl MustUse for T`,** so RFC-0075's obligation — acquired once,
  disposed exactly once, proved at compile time — is a declaration a third party
  writes. `movecheck`'s `mod streams` is `mod linear`, its `Type::Stream` matches
  are one lookup in the table `impl Owned` already fed, and the diagnostics name
  the user's type. It closes RFC-0087 U7, whose undecided half is decided in
  "M3 — as landed": `consume` and `MustUse` stay **two** declarations. See
  "M3 — as landed" for the two gaps it had to close first, both of which were
  the milestone being unusable rather than optional.
  **`solve_param` was the last of the six hand-written lists still open, and it
  is closed:** the match is exhaustive with no `_`. The blast radius measured
  **zero** over the whole corpus, and the deferral's stated reason — that the two
  backends disagree about an unsolved parameter — proved not to apply, because
  the checker refuses all four shapes first. **No program's meaning changed.**
  See "`solve_param`'s fall-through" below.
  **Two corrections to "M1 — as landed" below, both later than it.**
  RFC-0089 Phase 4c deleted `Analysis::transfers` and the whole inference half of
  `own.rs`: ownership is emission from the type, not inference over the
  expression, so the "two questions" that section describes are one question now.
  RFC-0090 Phase 8b gave a **generic** `impl Owned` a row — the drop site solves
  the type arguments from the binding and asks for the instance — which is what
  made `impl<T> Owned for Slots<T>` reclaim anything.
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
| ~~three `Type::Stream` matches in `movecheck`~~ | which types are linear — **M3, closed** |
| `Expr::ArrayLit` / `Expr::MapLit` in the checker | which types a literal can build |
| `codec::encodable` / `decodable` | which types cross a JSON wire |
| `solve_param` | which types unify a parameter |
| `parser::METHOD_BUILTINS` | which `.m(..)` calls are a builtin, before any type is known |

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
  **The one list with no victim.** The checker refuses those four shapes first,
  so the fall-through was dead rather than wrong — measured, not assumed, and
  the only one of the six that reads this way.
- The parser's method table bound `remove` to the `Map` builtin, so
  `people.remove(h)` did not compile where `people` is a `Slots<T>` and
  `std/slots` exports `remove`. Ten of its fourteen names were neither reserved
  nor given back. **The sixth list, found after this RFC was written, and it is
  the earliest of them: it decides before a type exists.**

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
  implements them. *Blocked.* A constructor has no receiver, and every protocol
  method in Vyrn has one. See "M2 — what it needs".
- **M3 — linearity is a declaration.** *Implemented.* `impl MustUse for T`, and
  the `Type::Stream` matches in `movecheck` become a lookup, so a user's file
  handle, lock or connection gets RFC-0075's obligation. That mechanism was
  built for one type and never exposed. Pin:
  `examples/mustuse_abandoned.vyrn`, an expected check failure whose three
  rejections are `Stream`'s and whose diagnostics say `Txn`. See "M3 — as
  landed" below.
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
  `drop x` covers it and works everywhere. It should go. **Removed.** The name
  is gone from both `RESERVED` and `SPAWN_FORBIDDEN`, from the checker, the
  interpreter, the textual backend, the LSP's completion table and the primitive
  census, so `afree(xs)` is now an ordinary call to an unknown function. The
  internal `DropKind` that reclaims an array's buffer is **not** the same thing
  and stays; it was called `AfreeArr` after the builtin and is now `FreeArr`,
  beside `FreeStr` and `FreeMap`. Two checks were added on the way out: the
  primitive census named the stale row before anything else did, and
  `SPAWN_FORBIDDEN` — which had never been checked against `RESERVED` at all —
  now is.
- **`array()`** has one use left, `examples/aliascontext.vyrn`, which exists to
  pin the alias-resolution defect PR #64 fixed. The spelling is dead; the pin is
  not.
- **`push(a, v)`** as a free function is the desugar target for `xs.push(v)`, so
  `own` must keep the name. `examples/region.vyrn` still writes the raw form by
  hand and could be migrated.

## M2 — what it needs

M2 was attempted and not landed. The protocol it needs cannot be declared in
Vyrn today, and the milestone's second half — "the `Type::Array` / `Type::Map`
matches in the checker go" — is wrong about `Array` for a reason M1 already
recorded. Both are below. No compiler code changed.

### The shape, and the part of it that works

Two of the three worries in the sketch turn out not to bite.

**Variadics are not needed.** The elements arrive as one argument. A map
literal's keys and values are two ordinary parameters, not a varying number of
them.

**`Self` is not needed either.** RFC-0080 M2's associated types already say
"the type the impl picks", and the return position is one more of those. This
declares and conforms today:

```vyrn
protocol FromElements {
    type Elem
    type Out
    fn fromElements(self, xs: Array<Elem>) -> Out
}
```

`Self` itself is absent, and it is absent in a way that matters: it is not a
type, so `-> Self` parses as a *name*, and `impl FromElements for Ring` is then
refused for providing `-> Ring` where the protocol declared `-> Self`. The
declaration compiles and no impl can ever satisfy it. An associated type is the
spelling that works, at the cost of the impl naming itself twice.

### The part that does not

`fromElements` is a **constructor**. It has no `self`, and there is nowhere for
one to come from: at `let r: Ring = []` no `Ring` exists yet.

Every protocol method in Vyrn has a receiver, and it is not a convention —
`parser::protocol_decl` eats `Tok::Vself` as the first token inside the
parameter list, so a signature without it is a parse error. Dispatch matches:
the checker resolves `.m(..)` by `type_key` of the **receiver's** type. Nothing
in the compiler dispatches on the *expected* type, and `?` through `Fallible` is
no precedent — it reads its operand.

So the missing feature is one thing, stated exactly:

> **A protocol method with no `self`, dispatched by the type the call site
> expects rather than by a value it is called on.**

That is new surface. Adding it inside a milestone about which types a literal
builds would be the milestone smuggling a language feature, so it is reported
instead of built. `Self` would come with it, because a receiver-less method has
no other way to name its own type, and `type Out` is a workaround readers have
to be taught.

Until it exists, a user's container is built by an ordinary function —
`ringOf([1, 2, 3])` — which works on all three engines and costs one name at
the call site. That name is the whole of the ergonomic gap.

### The match that cannot go, and why M1 already knew

`release_kind` matches on `Type` with one arm per variant, and that match did
not go in M1 either. What went was `owner_producing`, the **second** list. The
milestone text asks for the wrong deletion.

`Expr::ArrayLit`'s match on `expected` is not a hand-written property list. It
is **layout selection**, which this RFC exempts by name: representation stays
intrinsic. One syntax has three layouts —

- no annotation: `[1, 2, 3]` is an `ArrayN` held inline;
- `Array<T>`: three words around a heap buffer;
- `SmallArray<T, N>`: a header inline until it spills, and a literal longer
  than `N` is refused there.

Answering one of those for all three is what exited `ifexpr.vyrn` with
`0xC0000374` during M1. The arms are the answer to *which layout*, and there is
no protocol that could replace them.

What is genuinely a property test is the fallthrough — `` `[]` is an array
literal, but Ring is not an array type ``. That one arm is what a `FromElements`
row would open, and it is one arm. `[:]` has no second layout, so `Type::Map` in
the `MapLit` arm **is** a pure property test — but a table with one seeded row
and no way to add a second is not a protocol. It is the special case with a
protocol's name on it, which is what this RFC exists to remove.

The two literal arms also disagree about *where* they refuse. `let r: Ring = []`
fails in the literal arm; `let r: Ring = [1, 2, 3]` builds an `ArrayN` and fails
later at the assignment, saying "declared Ring but initializer is
`Array<Int64, 3>`". Same question, two failure sites. A `FromElements` lookup
would have to answer for both, which is a further reason the empty case alone is
not the milestone.

### The asymmetry rule, stated before building

M1's is that `transfers` may be generous because a wrong yes leaks, and
`release_kind` may not because a wrong answer frees the wrong bytes.

**M2 has no generous direction.** The answer decides a *representation*, not a
cleanup, so there is no safe leak to fall back on. Too generous — admitting that
a type can be built from `[]` when it cannot — lowers a heap array into a slot
that is not one, which is M1's `0xC0000374` reached from the other side. Too
strict costs a compile error on a legal program: recoverable, and the only
direction that is safe to be wrong in.

That difference is the reason M2 is not the same kind of table as M1. M1 had a
half it could be wrong in for free. M2 has none, so every row has to be right,
and the rows that decide layout are the compiler's own.

### Bootstrap

Unchanged, and not the obstacle here. The seeded rows for a literal are the
layout arms, which are already intrinsic and need no resolver. A bare file has
no user containers to build in the first place.

## M3 — as landed

### Half of it was already done, and nobody had written that down

The milestone says "three `Type::Stream` matches in `movecheck` become a
lookup". By the time it was picked up there was **one** implementation:
`own::must_use`, asked through `Declared::must_use`, resolving aliases so a
`type Events = Stream<Event>` carried the obligation its base did. The
consolidation had happened during the memory arc and left no record.

So the milestone's remaining half was the one that matters: the body was
hardcoded, and a user type could not declare the obligation. That is exactly the
shape M1 fixed for `Owned`, and M1's reasoning transferred unchanged — including
the bootstrap: `vyrn run` on a bare file has no resolver and therefore no `std/`,
so the built-in row is **seeded** rather than imported, and `Stream` is that row.

Two matches were left, and neither was where the milestone looked. One decided
whether a **parameter** carries the obligation into the callee. One decided the
**wording** of the fix menu. Both are lookups now, and the second is a lookup
into a two-valued answer rather than a boolean — see "the menu" below.

### The declaration

```vyrn
protocol MustUse {}

impl MustUse for Txn {}
```

**It declares no methods, and that is the design.** The obligation is a fact
about the type — this value must be disposed of by name — and the disposal is
already declared, by `impl Owned for T` or by nothing where the type owns no
heap. A method here would be a second `release`. A generic head carries a row
like any other, so `impl<T> MustUse for Pool<T>` obliges every instantiation:
Phase 8b keyed `Owned` on the type CONSTRUCTOR and this reads the same key, so
generics fell out rather than needing inventing.

### RFC-0087 U7, decided: they are two declarations

U7 asks whether `consume` and the obligation should be one. **They should not.**

They answer different questions. `consume` is a **calling convention** — who
owns this argument after the call. Must-use is an **obligation on a type** —
this value must be disposed of, wherever it goes. A `String` is consumable and
carries no obligation. A `Stream` carries one however any particular function
takes it. Merging them would make every `consume` parameter linear and every
linear value a calling convention, and neither implication is true.

`examples/mustuse.vyrn` shows the two doing their separate work in one function:
`consume` is why `drop t` is legal in `finish`, and the obligation is why
leaving `finish`'s body without that `drop` is an error.

### What a declared obligation gets, and what it does not

It gets all three of `Stream`'s rules, because all three were about a name, a
block and the paths out of one, and none of them ever mentioned a stream's
representation:

- **abandoned** — acquired and never named again;
- **disposed twice** — named again after the disposal;
- **the branches disagree** — one path discharges it and the other does not;
- and a **parameter** carries the obligation into the callee, so
  `fn sink(t: Txn) {}` is not the hole that lets it evaporate.

A **receiver** does not, and that is the one rule this milestone added. `impl
Owned for Txn { fn release(self) }` IS the disposal, so a rule that made it
discharge its own receiver before it could read it would leave the declared
release unwritable. `self` is a keyword, so a parameter carrying that name is an
impl receiver and nothing else.

**It does not get RFC-0075's storage ban, and that is deliberate.**
`ensure_type_exists` refuses a `Stream` in any position that stores one — a
record field, an array element, module state — and says so in the words "the
obligation would be laundered away by one field declaration". That is true, and
it is still `Stream`'s rule alone. Two reasons:

1. The ban is about **representation**, not obligation. A stream is a cursor
   over a producer and a stored cursor is an aliased one. A user's `Txn` has no
   cursor, and `Array<Txn>` is the shape a real program with a pool of them
   wants.
2. Widening it would refuse programs that are correct today, to close a hole
   that is narrower than the ban.

**So the hole is real and recorded.** A declared must-use value stored into a
record field or an array counts as *disposed* — a store mentions the name — and
the obligation stops there rather than moving to the container. What is proved
is that the value does not silently evaporate inside a body; what is not proved
is that a container that swallowed one ever discharges it. Closing that needs
the obligation to travel through a place, which is the same mechanism census U4
and RFC-0091's projections need, and it is not a `Type::Stream` match to delete.

### The menu, and why it is not one string

The fix note differs by row, and the reason is that the two disposals differ:

| row | note |
|---|---|
| `Stream` (seeded) | consumed with `for … in`, forwarded by returning it, or released with `close(s)` |
| `impl MustUse for T` | handed on by name — to a call, to the return — or released with `drop t` |

`drop s` on a `Stream` reclaims **nothing**: a stream's release is pushed by its
own lowering (M2b) and `release_kind` answers `None` for it on purpose. So a
single note would name a statement that silently does nothing for half the types
it is printed for. `own::Linear` is the two-valued answer, read out of the same
lookup that decides *whether*, so there is still one table.

### The two gaps it had to close first

Both were found by writing the corpus example, and both are the milestone being
unusable rather than optional.

**The checker refused `drop x` on any type declaring `impl Owned`.** The gate
listed `String`, `Array`, `SmallArray`, `Map` and a heap-carrying `Option`/
`Result`, and its own comment said it "follows `own::release_kind` rather than
deciding a second time" — which it did for the seeded rows and not for the
declared ones. So `impl Owned for Ring` could only ever run on the *automatic*
block-exit path. That is fatal here: must-use says "dispose by name", and
handing the value to a call or returning it only moves the obligation on, so
`drop` is the only terminal discharge there is. The gate now reads the declared
row first, off the written type — resolving `Ring` to its record shape is
exactly the lookup that loses the answer.

**The interpreter's `Stmt::Drop` was a no-op.** Both compiling backends already
lowered an explicit `drop x` through `release_kind`. The interpreter looked the
binding up and threw the value away, so a declared `release` ran on the
automatic path and not on the explicit one. It went unseen because the checker
refused `drop` on every type that could declare one — two defects holding each
other up. `examples/mustuse.vyrn` prints five releases and all three engines
print the same five.

### The design's own test

`examples/mustuse.vyrn` declares both protocols itself, so it imports nothing and
`std/` is untouched. Nothing in the compiler knows the name `Txn`. It discharges
the obligation four ways — `drop`, a call, a forward-and-then-discharge chain,
and once per loop turn — and all three engines agree byte for byte.

`examples/mustuse_abandoned.vyrn` is the same declarations with the discharges
taken away, listed under `EXPECTED_CHECK_FAILURE`. It produces the three
rejections above, each naming `Txn`.

`parity: 124 checked, 11 skipped, 0 failed`. No existing example changed: the
`Stream` diagnostics are byte-identical, the eleven census rows hold their
baseline, and the corpus gained no free and lost none.

## `solve_param`'s fall-through — the sixth list, and the last one. Closed.

**Decided in M1: exhaustiveness, not a protocol, and not in M1. Done later, and
the deferral's stated reason turned out not to apply.**

It is unification, not a property. Nothing is declared, and there is no third
party who *could* declare it — a protocol needs somebody to write an impl, and
"how do two type constructors match" has no author but the compiler. The rule is
already mechanical: same constructor, recurse on the children; different
constructors, bind nothing. So the fix is the shape of the `match`, not a table:
give every constructor an arm, with no `_`, so a new `Type` variant fails to
compile instead of silently binding nothing.

That is what `solve_param` now is. It matches on the **parameter** type alone —
the pairing was what made a `_` look unavoidable, since a tuple of two types has
no exhaustive spelling — with 35 arms and no catch-all. The inner match on the
argument type keeps its `_`, and that one is not a hole: it is the rule's second
half written down.

### Why the deferral did not apply

M1 deferred it for this reason:

> the two backends already disagree about what a `None` means — the LLVM emitter
> substitutes `Unit` and lowers it to `void`, the direct backend refuses. Filling
> in `Lazy`, `Record`, `Enum` and `Task` turns some silent `Unit` into a real type
> and some refusal into a compile.

**The disagreement is still there** — `lib.rs` still writes
`unwrap_or(Type::Unit)` and `direct.rs` still answers "a generic type parameter
`T` the call `f` does not fix". Nothing in the memory arc changed it.

**It is not reachable from these four shapes**, and that is the finding. The
CHECKER holds the same list. `Checker::unify` descends into `Option`, `Result`,
`App`, `Array`, `ArrayN`, `Stream`, `SmallArray`, `Fn` and `Map`, and its
fall-through is a **diagnostic**, not a substitution. So a type parameter under a
`Record`, an `Enum`, a `Lazy` or a `Task` is refused before codegen is asked:

| written | what the checker says |
|---|---|
| `fn unwrap<T>(b: { value: T }) -> T` | `argument expects { value: T }, found Box<Int64>` |
| `type Wrap<T> = \| W({ v: T })` | `argument expects { v: T }, found Cell` |
| `type Holder<T> = { body: lazy T }` | `argument expects lazy T, found lazy Int64` |
| `fn await<T>(t: Task<T>) -> T` | `argument expects Task<T>, found Task<Int64>` |
| `impl<T: Show> Show for { v: T }` | `impl Show for { v: T }` is not supported |

Every one is byte-identical before and after the arms were filled. **Nothing
became newly legal**, so nothing needed the three engines to be re-agreed on.
They were re-run anyway: 123 checked, 10 skipped, 0 failed.

### The blast radius

**Zero.** `rfc0086_unsolvable_parameter_positions_over_the_corpus` walks
`examples/**` and `std/`, takes the declared types `solve_param` is actually
handed — a generic function's parameters and return, a generic record's fields, a
generic enum's variant payloads, a generic impl head — and counts the type
parameters sitting under a constructor the old match had no arm for. Over 208
files and 990 declared root types: **0**.

So this was a latent defect with no victim, and the census is the record of that
rather than a bug report. Two tests hold the two halves apart:
`the_filled_arms_bind_a_parameter_the_fall_through_walked_past` says the new arms
are right, and
`the_checker_refuses_every_shape_the_fall_through_used_to_swallow` says why they
are dead. If the checker ever accepts one of the five programs above, the second
test fails and points at the first — the arms it unblocks are written and tested
already.

**The census has been wrong in both directions.** `Expr::ArrayLit`'s absence from
`owner_producing` leaked on every engine and nobody had noticed; this one had no
victim and the fix is still worth having, because what it buys is that the next
`Type` variant cannot be added without answering.
