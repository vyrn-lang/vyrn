# RFC-0091 — The Container Protocols

- **Status:** **M1, M2 and M3 implemented** (Phases 7a and 7b); **M4 stopped at
  its own gate.** The generalization layer over RFC-0089/0090: what makes a
  third-party container indistinguishable from a built-in. `place`/`yield`
  members, `Copy`, `Iterate` and `Index` all ship, and `std/slots` is the
  customer that proved them. **Spelling note (2026-08-29):** RFC-0120 retired
  `place`/`yield` for the result capability — every `place f(..) -> T
  { yield p }` below is now written `fn f(read self, ..) -> read T
  { return p }`, and the mechanism this RFC built is unchanged underneath. Read "M2 as landed", "M1 and M3 as landed" and "The
  generic-container correction" — the last records two things this RFC says that
  are wrong. Then read "M4 as landed": `SmallArray` is not portable, the reason
  is three separate missing features, and the numbers say the port would cost
  more than it returns.
- **Depends on:** RFC-0086 M1 (`Owned` — the pattern this repeats), RFC-0089
  (conventions), RFC-0090 (`Slots` is the first customer), RFC-0080
  (associated types), RFC-0084 (records dispatch), RFC-0082 (the thesis)
- **Principle:** RFC-0086's rule, finished. *A type declares what it is. The
  compiler looks it up.* This RFC enumerates what a container must be able to
  declare.

---

## The test

RFC-0090 says identity lives in containers you own, and `Slots<T>` is std Vyrn.
Then a third party must be able to write their own — a pool, an interner, an
LRU cache, a spatial grid — and have it *feel* like `Array`. Today it cannot,
for four reasons, and each is a hardcoded capability of built-ins:

| a built-in can | a user container cannot | because |
|---|---|---|
| be released automatically | ✅ can, since RFC-0086 M1 | `impl Owned` — **the pattern, already shipped** |
| be indexed: `a[i]`, `a[i] = v`, `a[i].f = v` | ❌ | RFC-0011's lowering matches `Type::Array`/`Map`/`SmallArray` |
| be iterated: `for x in xs` | ❌ | `ForIn` is Array-only |
| be copied: `.copy()` | ❌ (nothing can yet) | RFC-0089 M1 ships it for built-ins |

RFC-0086 M1 proved the shape: seed the built-in rows in the compiler, let
`impl P for T` add rows, resolve nominally, no second list. This RFC applies
that shape three more times.

---

## The three protocols

### `Copy`

```vyrn
protocol Copy {
    fn copy(read self) -> Self
}
```

Derived structurally for every type whose fields are `Copy` (a record of
copyables copies; `Array<T>` copies if `T` does). A container with extra
invariants — an interner that must not duplicate its table, a handle that must
not be deep-copied — overrides it. `-> Self` requires RFC-0084's dispatch-by-
expected-type work or the associated-type spelling; this is the same blocker
RFC-0086 M2 recorded, and this RFC inherits it rather than re-solving it.

**The builtin shipped first** (RFC-0089 M1b), shaped so M1 lifts it rather than
replaces it. Three things are already in the right place. The structural
derivation is one predicate, `own::owns_heap`, which the checker and both
backends call — that becomes "is this type `Copy` by derivation". The override
point already exists and already errors: a type that declares `impl Owned for T`
is refused today with a diagnostic pointing here, so M1 turns a refusal into a
dispatch and nothing else changes. And the receiver convention is `read self`
already, since `copy` never consumes what it copies. What M1 adds is the row and
the dispatch; the semantics are landed and under test.

### `Iterate`

```vyrn
protocol Iterate {
    type Item
    fn size(read self) -> Int64
    fn nth(read self, i: Int64) -> read Item     // see "the projection problem"
}
```

`for x in xs` desugars to it; the loop variable is a `read` borrow per
RFC-0090's iteration rule. `Array` gets the seeded row; `Slots` skips dead
slots by implementing it; a user's tree walks itself.

### `Index`

```vyrn
protocol Index {
    type Key
    type Value
    fn at(read self, k: Key) -> read Value        // a[k]
    fn atSet(modify self, k: Key, v: Value)       // a[k] = v
}
```

`people[h]`, `grid[point]`, `cache[url]`. The seeded rows are `Array<T>` by
`Int64`, `Map<String, V>` by `String`, `SmallArray` — and RFC-0011's hardcoded
lowering becomes three rows in a table, which is RFC-0086 M1's move again.

---

## The projection problem — the one new mechanism

Rule 3 of RFC-0089 says a function returns an *owned* value. Rule 2 says a
borrow cannot be returned. Then what is `at`? If it returns an owned `Value`,
`people[h]` copies — a hidden copy, forbidden by the law. If it returns a
borrow, it violates rule 2.

**Built-ins dodge this today by not being functions.** `a[i].field = v` is a
compiler desugar with a temp (`ps[]`, RFC-0017-era); `a[i]` is inline lowering.
The compiler reaches into the buffer because it is the compiler. A user
container has no way to say "here is a *place* inside me" — and without it,
`Index` and `Iterate` cannot be written.

The answer the MVS literature converged on (Hylo's subscripts) is a **place
projection**: a method form that *yields* a place instead of returning a value.
The caller's access runs bracketed inside the callee's frame, so the borrow
never escapes — rule 2 is preserved by construction, not by exception:

```vyrn
impl Index for Slots<T> {
    type Key = Handle<T>
    type Value = T
    place at(read self, h: Handle<T>) -> T {
        if self.gen[h.slot] != h.gen { panic("dead handle") }
        yield self.data[h.slot]        // yields the PLACE, not the value
    }
}
```

`people[h].name` then means: enter `at`, check, and perform the field read
against the yielded place before `at` finishes. A `place … modify self` form
covers `people[h].name = v` and in-place growth (`cache[k].push(v)`).

Monomorphization makes this free: the projection body inlines into the access
site, which is byte-for-byte what the compiler's own `Array` lowering already
emits. So the mechanism is not a coroutine at runtime — it is a named,
type-checked macro-shaped inlining with the borrow rules enforced at its edges.

**This is the one genuinely new language feature in the whole RFC chain.**
Conventions extend `movecheck`; moves extend it further; `Owned`/`Copy`/
`Iterate`/`Index` repeat RFC-0086 M1. Place projections are new. If they are
rejected, user containers stay second-class (method calls only, `.get`
returning `Option` copies), and RFC-0090 still works — but `Slots` reads like a
library, not like the language.

---

## What this opens for reimplementation

With the four protocols and projections, these become writable in Vyrn with no
compiler change, and the census's "clean generators are the structured-
reflection ones" lesson gets its container counterpart:

- `Slots<T>` (RFC-0090 M1), pools, arenas with wholesale drop
- interners, LRU caches, ring buffers, priority queues
- ECS storage — SoA columns indexed by entity handle, the RFC-0016-era gap
- the existing built-ins themselves, progressively: RFC-0082 M2's port resumes
  with its blocker (escape-on-call) removed and its gate (performance) already
  measured green by RFC-0090's 1.86×

**Deliberately not opened:** the allocator. `malloc`/`free`/`memcpy` stay the
compiler-emitted floor. An `Allocator` protocol is real multiplicity with no
oracle (RFC-0081's objection) until a second allocator has a reason to exist;
the workspace rule — gated multiplicity stays true, ungated rots — applies.
Recorded as open, gated on a concrete need (arena-backed containers may be it).

---

## Milestones

- **M1 — `Copy`**, derived + overridable — **LANDED (Phase 7b).** See "M1 and M3
  as landed".
- **M2 — place projections** — **LANDED (Phase 7a).** The mechanism, proved on
  `Array` itself. See "M2 as landed" below for what the dogfood proof could and
  could not delete, and why the RFC did not see the difference.
- **M3 — `Index` + `Iterate`** open to users — **LANDED (Phase 7b).** Both
  halves of `Index`, including the store 7a fenced off. `Slots` can implement
  them; `for x in slots` works.
- **M4 — resume RFC-0082 M2**: port one built-in (`SmallArray` is the
  smallest) to std Vyrn behind the protocols, three-way parity as the gate.
  **STOPPED. Nothing was ported and nothing was deleted.** See "M4 as landed".
- **M5 — the conditional place**: a projection may yield an `if` whose branches
  are places. **DESIGNED, not built.** It closes the third of M4's three
  blockers, it does not close the `Iterate` gap 8a found, and it is not a
  performance feature. See "M5 — a projection may choose between two places".

---

## M2 as landed

Shipped in Phase 7a. `place name(read|modify self, ..) -> T { .. yield <place> }`
parses inside an `impl`, is checked as a body of its own, and is inlined at
every `a[i]` and `a[i] = v` by all three engines. `yield` and `place` are
contextual, so no program that used either word has to change.

### The dogfood proof, and the one thing it cannot delete

The IR and the wasm are **byte-identical** across the whole corpus — 118 emitted
`.ll` files and 119 emitted `.wasm` modules, diffed against `main`, zero
differences. What that proves is that indexing now goes through a table, and
that going through it costs nothing.

What it does not do is delete the addressing. **This RFC asked for something its
own chain had already made impossible.** It was written before RFC-0080/0081
withdrew raw memory, and `Array`'s `at` has nothing to write its body *with*:
there is no way in Vyrn to say "the element at offset i of my buffer". So one
primitive survives, under a name no source can spell — `@slot(container,
index)`, unlexable because the lexer rejects `@`. It is the addressing floor, and
it sits beside the allocation floor this RFC deliberately leaves closed.

What the proof does delete is the **dispatch**. `a[i]` no longer means "the
compiler knows about arrays". It parses to `at(a, i)`, which asks the receiver's
type for a `place at`; `Array`, `SmallArray`, `Array<T, N>`, `String` and `Map`
all take the seeded row, whose body is `yield @slot(self, i)`. A user container
reaches its own projection through the same lookup, and its `yield self.data[i]`
inlines to `@slot` through one more turn of the same machinery.

### The rules a projection obeys

Checked by `checker::check_places`, each with its reason:

1. **One `yield`, and it is the last statement.** A conditional yield would need
   the access site to become a branch over two places. Refused, not
   mis-lowered.
2. **What it yields is a place** — a variable, a field of one, an element of
   one. A value would be a copy, and a hidden copy is what rule 3 of RFC-0089
   forbids.
3. **Rooted at `self` or a parameter.** A projection into module state hands out
   a place the access site does not own.
4. **No `?`.** `?` propagates by returning, and an inlined projection has no
   frame to return from. `return` is refused by the parser for the same reason.

### Three findings

**Inlining is free in instructions and not in node identity.** This compiler
keys two side tables by AST node address — the elided `get`/`set` generation
checks and the lambda monomorphization keys — because Phase 4a found a
`(line, name)` key cannot identify a statement. A substituted body is a clone,
with an address of its own, so it loses whatever was recorded against the
original. Both misses are conservative today (one extra check, one duplicated
instance), and the seeded row sidesteps them entirely: `Projection::is_identity`
recognizes the identity substitution and each engine then lowers the ORIGINAL
nodes. That is what makes byte-identity a property of the code rather than a
measurement.

**The textual backend has no static type-of-expression.** It learns a type by
emitting the expression and reading the type back, which is fine for lowering
and useless for dispatch, which must choose before it emits. `Gen::static_ty`
now covers the shapes a container receiver takes — a binding, a field of one, an
element of one, a call result — and answers `None` for the rest, which then
takes the seeded row exactly as it did before projections existed. The
interpreter has `type_of` and the direct backend has `peek`; only this one
needed a new function.

**A record is keyed by the name it was declared with, not by the record it
aliases.** Three separate sites resolved the receiver type before looking up the
impl, and each one turned `Window` into `{ data: Array<Int64>, start: Int64 }`,
whose type key is nothing. An impl head names the alias.

### What 7a does not open

`Index`, `Copy` and `Iterate` as declared, conformance-checked protocols are M1
and M3. In 7a the protocol name on an `impl` carrying `place` members is not
read: lookup is by (type key, member name). A projection is also not callable by
name — `c.at(1)` is an unknown function — because it is not a function.

**Storing through a user container is not lowered.** `a[i] = v` resolves
`place atSet` and accepts it only where the yielded place is the binding's own
element, which is what the seeded row yields. A projection that yields somewhere
else (`self.data[j]`) is refused by name: writing there needs an address-of for
an arbitrary place, and no backend has one. That is M3's, and it is the read/
write seam this milestone was allowed to stop at.

---

## M1 and M3 as landed

Shipped in Phase 7b. `Copy`, `Iterate` and `Index` are protocols a type
declares, resolved nominally by type key, seeded for the built-ins. RFC-0086
M1's shape, three more times, with no second list anywhere.

### `Copy`

`x.copy()` asks the receiver's type first. A type with `impl Copy for T`
dispatches to the `copy` it declares; every other type keeps the structural
derivation `own::owns_heap` already defines. That is the row and the dispatch,
and the three things this RFC said were already in place were: the derivation,
the receiver convention, and the override point.

The override point was two refusals, and both are now overridable. A type that
declares `impl Owned for T` copies through the `copy` it declares. A type that
refers to itself copies through the recursive function M1b's diagnostic already
told the reader to write — that diagnostic now says where the function goes.
A `Stream<T>` still refuses, and the reason is structural rather than a policy:
it has no type key, so it can carry no row.

### `-> Self` still blocks, and nothing since RFC-0086 M2 changed it

`Self` is not a type name in this language. It is not in the lexer, not in the
parser's type table, and not in the checker. RFC-0084 gave records dispatch and
Phase 7a gave `place` members; neither introduced a receiver-typed name. A
protocol method written `-> Self` therefore parses as `Type::Named("Self")`, and
conformance checking compares it against the impl's `-> Ring` and reports a
mismatch.

So M1 takes the associated-type spelling this RFC allowed for:

```vyrn
protocol Copy {
    type Copied
    fn copy(self) -> Copied
}
```

The declaration is optional, exactly as `Owned`'s is: the compiler knows the
protocol name and the method name, so a bare file with no resolver still works.
What a program must write is `impl Copy for T`.

**The receiver convention needs a correction too.** This RFC spells the method
`fn copy(read self)`. That does not parse: an impl method's receiver is written
bare `self` and IS `Capability::Read` — the capability is right and the word is
implicit. A `place` member is the one member form that spells it
(`read self` / `modify self`), because it also offers `modify self`. The
inconsistency is real and is left where it is; making an impl method's receiver
capability writable is new syntax.

**Since "The receiver may be written", `fn copy(read self)` parses.** This RFC's
own text is writable, and the inconsistency between a `place` member and a
method is gone.

### `Iterate`

```vyrn
impl Iterate for Window {
    fn size(self) -> Int64 { .. }
    place nth(read self, i: Int64) -> Int64 { yield self.data[self.start + i] }
}
```

Both halves are required, and the refusal names the missing one. `for x in xs`
over such a container becomes:

```text
let @i.n = size(xs)
let mut @i.i = -1
while @i.i + 1 < @i.n {
    @i.i = @i.i + 1
    <the projection's prologue>
    let x = <the place it yields>
    <the body>
}
```

One function builds that, in `project.rs`, and each engine lowers it with the
statements it already has. **The increment is the body's first statement, not
its last**: a `continue` jumps to the condition, so an increment at the end
would be skipped and the loop would spin on one element. Testing `@i.i + 1`
rather than `@i.i` is what pays for that, and it keeps the index naming the
element the turn is reading, so a `break` leaves it where a reader expects.

An iterable named by a place is read where it lives, which is what makes the
loop variable a borrow of it. An iterable that is not a place binds once — the
decision log's rule, and evaluating it per turn would run its side effects
`size + 1` times.

A built-in array does NOT take this path. `for x in a` still reaches each
engine's own element walk, and the emitted output says so: 119 `.ll` files and
119 `.wasm` modules diffed against `main`, with the one exception recorded
below. `for x in consume xs` is unchanged — `consuming` is a `movecheck` fact
and no engine reads it.

### `Index`, and the store 7a fenced off

**7a's refusal named the wrong obstacle, and the mechanism was already in the
repo.** It said a store through a user container "needs an address-of for an
arbitrary place, and no backend has one". RFC-0082 M1 met exactly that problem
for `r.a[i] = v` — a container that is not a slot — and answered it without an
address-of: move the container out into a temp, mutate the temp, move it back.
`parser::place_receiver` is that desugar, it is pure AST, and it already covers
the three shapes a place takes.

So `c[k] = v` through a user container is the projection's prologue followed by
the same statements `r.a[i] = v` has always emitted. No engine gained an
addressing mode. The move-out is O(1) for a growable container — a header copy
sharing the buffer — and a whole-value copy for one held inline, which is what
`a[i].f = v` has always cost.

The refusal that remains is narrow and true: a projection that yields something
with no address at all (a call result, a temporary) is refused, and the wording
says a projection yields a place.

### The one thing the proof had to move, and why

Building the store found a 7a bug. `project::inline` renamed a projection body's
own bindings to a fixed `@b.name`. The prologue lands in the **caller's** block —
it is statements, not a scope of its own — so two inlines of one projection in
one block bound the same name and the second shadowed the first. Nothing
hoisted between two inlines until this store did, and then `s[j] = s[k]` read
the wrong element. Only the two compiling backends were wrong; the interpreter
gives each inline a frame of its own.

Each inline now carries a number. Those names reach the textual backend as
alloca names, so `examples/projection.ll` differs from `main` in exactly those
names and nowhere else. The wasm is byte-identical, because a wasm local has no
name.

### What did not flip, and what each row is really waiting for

Phase 5 left three memory rows leaking and said they waited on this work. None
of them flips, and one of the three reasons is a correction.

- **`optionString` (§14)** — `if let Some(s) = maybe(tag())` matches a payload
  out of a value with no name. Releasing it needs the payload's escape from the
  arm tracked. Neither M1 nor M3 touches that.
- **`lambdaLoop` (§16)** — Phase 5 named M1 as the mechanism, **and it is not**.
  A `Copy` row is keyed by a type key; a `fn` type is structural and has none,
  and a `type Bump = fn(..) -> ..` alias over one is refused where it is
  written, because the value erases at run time and carries no name to dispatch
  on. So §16 has nowhere to hang a declaration, and nothing to write in it
  either: the tags are the defunctionalizer's and have no source name. What it
  waits on is a copy DERIVED over the defunctionalized enum, emitted where
  RFC-0037 already emits that enum, which knows every tag's layout because it
  chose them. That is a job in the closure lowering, not a row in a protocol
  table.
- **`elementLeak` (U4)** — `m.keys()` hands back a fresh buffer holding the
  map's own key pointers, so releasing an `Array<String>` element by element
  frees what the map still holds. A protocol row does not change what `keys`
  returns.

### What M1 and M3 do not open

`Slots<T>` itself (RFC-0090 M1, Phase 8a) and the port of a built-in to std Vyrn
(M4). A `place` member is still invisible to the LSP's symbol index: hover and
completion work on a user container because the checker types it, but a `place`
member has no definition site of its own to jump to.


---

## The generic-container correction (Phase 8a)

Every container this RFC was dogfooded on is concrete. `Window`, `Slice`, `Ring`
and `Pool` name their element type in the impl head, and `Slots<T>` — the
customer the RFC was written for — does not. Six things broke, and each is one
place a protocol member's type was read without solving the head it was declared
under.

- **`c[k] = v` and `for x in c` read a projection's return type literally.** A
  `place atSet` or `place nth` declared `-> T` answered `T` rather than the
  element type at the site. `place at` already solved the head, via
  `place_result`; one helper now serves all three.
- **`Index`'s `type Key` was not read by the store.** `c[k] = v` demanded an
  `Int64` key whatever `place atSet` took. `Slots` is keyed by a `Handle<T>`.
- **A record literal could not learn its parameters from context.** `vals: []`
  for a field declared `Array<T>` says nothing, and `Handle<T>` — three `Int64`
  fields and a parameter carried for branding — says nothing at all.
- **A generic call could not learn its type arguments from context**, in the
  checker and in both compiling backends, so `newSlots()` was unwritable.
- **The textual backend passed a `modify` argument to a generic function by
  value** while the definition took a pointer. That one is not a protocol bug at
  all; it is a native miscompile that no corpus function was shaped to find.
- **A `place` body's string literals never reached the textual backend's
  pool.** A projection is inlined and never flattened into
  `program.functions`, so the literal walk never saw one. `panic("..")` inside
  `place at` is what a trapping index is FOR, and it was the one thing a
  projection could not say.

### `Iterate` cannot skip, and this RFC says it can

"`Slots` skips dead slots by implementing it" is not achievable with the
`size` + `place nth` shape M3 landed. A projection maps an index to a place; it
has no cursor to advance, and "one `yield`, and it is the last statement" forbids
a branch to yield from. A container that skips must do the skipping BEFORE the
projection runs, which means holding a dense list of live positions — `Slots`
keeps one, plus the map back from a slot to its place in it, and pays two arrays
for O(1) removal and an honest `for x in s`.

### A mutating operation cannot be a protocol method

M1-as-landed records that an impl method's receiver is bare `self` and IS
`Capability::Read`, and that making it writable is new syntax. The consequence
was not drawn there: `insert` and `remove` mutate the container, so neither can
be a method of any protocol, and `Slots` is free functions. RFC-0090's own
example — `people.insert(..)` — does not compile. The subject-first surface this
language migrated to over ten RFCs stops at the read/write line: `s[h]`,
`s[h] = v` and `for x in s` are methods in all but name, and everything that
grows or shrinks the container is a call.

#### The correction: the receiver takes the word, and two claims above were wrong

The new syntax is built. A receiver is written `read self`, `modify self` or
`consume self`, a bare `self` still means `read`, a protocol declares the
receiver's capability and each parameter's, and conformance compares both. See
"The receiver may be written" below.

Two claims here did not survive the building of it.

**`people.insert(..)` compiles, and always did.** Vyrn has no inherent methods,
so `x.m(a)` parses as `m(x, a)` and falls through to a plain function of that
name — the behaviour `an_impl_provides_only_the_methods_its_protocol_declared`
already asserts, and the fix the "no inherent methods" diagnostic already
offers. A free function whose first parameter is the receiver IS callable as a
method, `modify` parameter and all. So the subject-first surface never stopped
at the read/write line: `people.insert(p)`, `people.get(h)` and `people.count()`
all ran on the day 8a shipped.

**What could not be written was the METHOD, not the call.** That is the real
defect, and it is narrower and more serious: a user container could not offer a
mutating operation through a protocol, so nothing could be generic over one and
nothing could dispatch to one. `modify self` is that.

**One name still does not work, for an unrelated reason.** `people.remove(h)`
does not compile, and no receiver capability changes it: the parser rewrites
`.remove(..)` to the Map builtin `@remove` before any type is known, exactly as
it does `.pop()`, `.copy()` and `.keys()`. A method-only builtin name is
reserved for every receiver in the language. That is the same shape as the `get`
Path B held for four milestones, and it is its own defect.

### A generic `impl Owned` has no release

`Owned` predates this RFC and takes the same correction. A generic impl's
`release` flattens to a generic function, and the drop site emits the flattened
name with no type arguments — a symbol nothing defines, reported by clang at the
end of a build. Phase 8a filters generic impls out of the `Owned` table, so the
result is a missing release rather than a link error. Monomorphizing a declared
release is the work; `Slots<T>` is what wants it.

---

## The receiver may be written

An impl method's receiver takes the three words a parameter takes. `read self`
is what a bare `self` has always meant, so nothing existing moves; `modify self`
is RFC-0089 rule 2 in the one position it never reached; `consume self` hands
the method the value rather than a borrow of it.

```vyrn
protocol Counting {
    fn record(modify self, n: Int64) -> Unit
    fn sum(read self) -> Int64
}
```

**A protocol declares the discipline and an impl must match it.** A `MethodSig`
carries the receiver's capability and each parameter's, and conformance compares
both. That is not bookkeeping: inside `fn feed<T: Counting>(t: modify T, ..)`
there is no impl to look at, so the protocol's word is the only thing that says
`t` must be a mutable binding. An impl free to take `modify self` where the
protocol wrote `self` would mutate through a borrow with nothing at the call
site to say so.

### What it cost, engine by engine

An impl method is flattened to a top-level function with the receiver as its
first parameter, so a receiver capability is a parameter capability everywhere
below the parser. **Both compiled backends needed no change at all.** A method
call lowers to the call the free function got: the emitted `.ll` for `c.bump(4)`
and for `bump(c, 4)` differ in the alloca's name (`self.addr0` against
`c.addr0`) and in the order the two definitions are emitted, and `vyrn_main` —
the call site, where the address is handed over — is identical.

Three things did move.

- **`self.n = v` did not parse.** The two statement forms that read the target's
  root off the TOKEN were gated on an identifier, and `self` is its own token.
  Everything else already treats `self` as an ordinary binding, which is why
  `self.vals[i] = v` and `self.free.pop()` have always worked: they go through
  `primary`, which returns `Expr::Var { name: "self" }`.
- **The interpreter dispatched a protocol method and returned from there**,
  ahead of the `modify` copy-back every other call takes. That was right while
  every receiver was `read`. It is the one engine where the receiver's
  capability had to be read at the call rather than at the definition.
- **`movecheck` had nothing under a method's name.** A call site carries the
  SURFACE name (`insert`), and the impl it will dispatch to is flattened under a
  mangled one. The protocol's declaration is what both sides agree on, so the
  pass reads capabilities from there. Without it the exclusivity rule and the
  `consume` move both go silent the moment a function becomes a method — which
  is the answer to "does `a.mutate(a)` fall out of the existing check": it does,
  through `check_exclusive` unchanged, and only because the protocol carries the
  word.

### `consume self` and `drop` of a projection

RFC-0089 records two findings, and this closes half of one. `drop` of a
projection is a double free the moment its place is released, and the rule is
written in `movecheck` as a comment rather than as a check — because the one
legitimate way to reclaim a declared container's buffer is `impl Owned for Ring
{ fn release(self) { let slots = self.slots  drop slots } }`, and `self` was a
`read` parameter, so the honest rule would have refused the mechanism RFC-0086
M1 shipped.

`consume self` is the spelling that unblocks it, and `std/slots` now uses it.
The check itself is NOT written here, and the reason is migration rather than
design: the rule refuses `drop` of a borrow, so every declared `release` in the
corpus has to say `consume self` before it can be turned on, and a `release`
that still says `self` would then fail to compile rather than warn. The change
is one word per impl and it is somebody's afternoon, not this one's.

`consume self` is refused on a `place` member. A projection yields a place
inside the receiver, so the receiver has to outlive the yield.

### `std/slots` did not become methods, and the reason is worth keeping

The obvious next step is a `Slab` protocol carrying `insert`, `remove`, `get`,
`alive`, `count`, `capacity` and `handles`. It buys nothing and costs something
real.

It buys nothing because the surface is already there: `people.insert(p)` is
`insert(people, p)` and compiles today.

It costs a program-wide name. Protocol-method resolution keys on the method
name alone, before any function of that name is considered, so declaring
`fn handles(self)` in a protocol makes `handles` mean "dispatch on the
receiver's type" in every program that links the module. `examples/copy.vyrn`
declares `fn handles()` and imports `std/slots`; the protocol would break it on
the day it was added. `count` and `get` are the same hazard with more callers.

And `remove` could not join it anyway — see the note above on `@remove`.

So the free functions stay free, `impl Owned`'s receiver becomes `consume self`,
and the feature's user in the corpus is `examples/modifyself.vyrn`.

---

## M4 as landed — the port is refused, and three separate features are why

M4 asked for `SmallArray<T, N>` in std Vyrn behind the protocols, with three-way
parity as the gate. **Nothing was ported and nothing was deleted.** The port
never reached the gate: `SmallArray` cannot be written in Vyrn at all, for three
reasons that do not depend on each other. The gate was then measured anyway, on
the closest container Vyrn CAN express, and it failed too.

This is the third time RFC-0082's port has been stopped, and the first time the
obstacle is the language rather than a number. The number agrees.

### Why `SmallArray` was the right thing to try, and the wrong thing to expect

This RFC picked `SmallArray` because it is the smallest built-in. That is true by
line count and false by shape. `Slots` is a record of six `Array` fields, which
is the shape RFC-0082's whole argument covers: a bounds-checked growable buffer
owns `len` and `cap` together, spare capacity is allocated and unreadable, and
nothing has to name an address. `SmallArray` is not that. Its spare capacity is
INLINE, inside the value, and the emitted IR says so in one word:

```llvm
{ i64 0, i64 4, ptr null, [4 x i64] undef }
```

`undef` is the whole difference. An empty `SmallArray<T, N>` holds `N` slots of
`T` that hold nothing, and `len` is what says they must not be read. That is
`MaybeUninit` — **the one operation RFC-0082 named as the reason a safe `Vec` is
unbuildable from safe parts, and then declared irrelevant because `Array` owns
its own spare capacity.** `Array` does. `SmallArray` does not, and RFC-0082
listed it among the sixteen census rows without noticing that its own argument
never reached it.

So the finding is not "the protocols are short of a feature". It is that
**RFC-0082's thesis has a boundary, `SmallArray` is on the far side of it, and
nobody had drawn the line before.**

### The three blockers, each with the program that proves it

**1. Const generic parameters do not exist.** `N` is not a type parameter
anywhere in this language. `Type::ConstInt` is a non-negative integer literal in
type-argument position, only `SmallArray` and `Array<T, N>` consume one, and the
checker rejects it in every other position by name. A type declaration cannot
bind it:

```vyrn
type Small<T, N> = { len: Int64, inline: Array<T, N>, spill: Array<T> }
//                                     ^ `Array<T, N>` needs a non-negative integer size
```

A function cannot bind it either, which is the sharper half: **not even the
BUILT-IN is generic over its own capacity.**

```vyrn
fn sum<T, N>(xs: SmallArray<T, N>) -> Int64 { return xs.length }
//               ^ `SmallArray<T, N>` needs a non-negative integer capacity
```

Every use in the corpus names a literal, and the compiler monomorphizes the
layout at each one. A std module has no way to say `N` at all, so a ported
`SmallArray` would be one module per capacity. The corpus uses five — 2, 3, 4, 8
and 16.

**2. There is no uninitialized place, by design.** A record field must hold a
value when the record is made, and there is no value of a generic `T`. An empty
inline buffer is unspellable:

```vyrn
fn newSmall4<T>() -> Small4<T> {
    return Small4 { len: 0, inline: [], spill: [] }
    //                      ^ `[]` is an array literal, but Array<T, 4> is not an array type
}
```

`Array<T, 4>` in a generic record is fine when four values of `T` are in hand —
`Box4 { inline: [a, b, c, d] }` compiles and runs. The container's empty state is
what cannot be written. This is not an oversight to repair: "the language has no
uninitialized place" is `std/slots`'s own words for why `remove` does not clear
its payload, and it is what makes every place in an RFC-0089 program hold exactly
one value.

**3. A projection cannot choose between two places.** `SmallArray` reads from the
inline slots or from the heap buffer, decided per access on `cap`. Written as
`place at`, that is a conditional yield, which M2 refused:

```vyrn
impl Index for Small4 {
    place at(read self, i: Int64) -> Int64 {
        if i < 4 { yield self.inline[i] }
        yield self.spill[i - 4]
    }
}
// `place at` must end with exactly one `yield <place>` — a projection is
// inlined at the access site, so it has one exit
```

**This one IS a gap in this RFC, and it is the sibling of the gap Phase 8a
found.** 8a recorded that `Iterate` cannot skip, because a projection has no
cursor and no branch. The same rule says `Index` cannot pick a buffer. A
two-state container is the plainest thing a user would write that needs it — a
pool with an inline first page, a rope, a hybrid map — so the rule's cost is
larger than "no `SmallArray`".

Closing it is not obviously wrong: the access site would become an `if` with the
access duplicated in each arm, which is what the built-in already emits. **It
would also not unblock this port** — see the third row of the measurement below,
which is that duplicated access written by hand.

### The measurement, on the container Vyrn can express

`SmallArray<T, N>` is unwritable, so the gate was measured on `Small16`: the same
two-state shape, `Int64` concrete, capacity 16 concrete, free functions instead
of protocol members. The proxy is FAVOURABLE to Vyrn on two counts — it never
copies the inline slots out on a spill, and it seeds the inline half with zeros
once — so a bad number here is an upper bound on how good the port could be.

`vyrn bench`, native, median:

| bench | built-in | Vyrn | `Array<Int64>` |
|---|---|---|---|
| push 16 | 57 ns | **105 ns** (1.8x) | 67 ns |
| indexed sum, 1024 reads | 111 ns | **882 ns** (7.9x) | 72 ns |
| indexed sum, projection hand-inlined | 111 ns | **901 ns** (8.1x) | 72 ns |

The third row is the one that decides it. It is blocker 3 removed by hand: the
branch written at the access site, exactly what an inlined conditional yield
would emit. It is not faster. **So the missing language feature is not what
costs the 8x**, and building it would buy nothing here.

`std/slots` came out 2.0x FASTER than the slab it replaced (RFC-0090 M1). This
comes out 1.8x and 7.9x slower. That is the difference between a container over
`Array` and a container over an inline buffer, and it is the same boundary
blocker 2 names.

### What costs the 8x, and it is worth its own work

A fixed `Array<T, N>` is a value, not a header, so a dynamic index has to put it
in memory first. The textual backend spills the whole array to a fresh alloca on
EVERY read:

```llvm
%spill5 = alloca [16 x i64]
store [16 x i64] %t2, ptr %spill5          ; 128 bytes, per element read
%t6 = getelementptr [16 x i64], ptr %spill5, i64 0, i64 %t3
```

The growable `Array<T>` beside it does an `extractvalue` and a `getelementptr`
and nothing else. Isolated, 1024 reads, native, median:

| | |
|---|---|
| `Array<Int64, 16>`, local | 1.54 µs |
| `Array<Int64, 16>`, record field | 1.53 µs |
| `Array<Int64>`, local | 76 ns |

**20x, and the inline half of any small-buffer container in Vyrn would stand on
it.** The record field costs the same as the local, so this is the fixed array's
representation and not the field access. Recorded here rather than fixed: the
repair moves emitted IR for every program that indexes a fixed array, and it is
not a container-protocol change.

### Emitted size — the port makes the output bigger, not smaller

RFC-0078's expectation is that deleting a built-in shrinks the compiler's output.
Measured over an empty program's baseline, on the same program written both ways
(fill to N, spill, index, store, sum, drop — identical output on all three
engines):

| | built-in | Vyrn | |
|---|---|---|---|
| IR lines | 250 | 276 | +10.4% |
| IR bytes | 10,899 | 11,678 | +7.1% |
| wasm bytes | 1,642 | 1,860 | +13.3% |

And that is the CONCRETE case. A std module is generic in `T` and duplicated per
`N`, so a program using two capacities pays two copies of the module before
monomorphization over `T` begins.

### Lines — the trade the thesis asks about

About **720 lines of Rust** name `SmallArray`: three dedicated blocks (`sa_ll`,
`sa_value_base_len`, `sa_slot_base` in the textual backend, about 90 lines;
`sa_parts`, `sa_from_fixed`, `sa_push`, `sa_method` in the direct backend, about
357; four tests, about 75) plus 194 scattered lines across 17 files that name the
type in a shared arm. Most of the 194 do not go away — a `Type::SmallArray` arm
beside `Type::Array` and `Type::Map` is one line of a match that stays.

Against that, the Vyrn container is about 35 lines per capacity, so five
capacities is about 175 lines of std Vyrn. The trade is real and the direction is
the one RFC-0078 predicted. **It is also not available**, because the 175 lines
do not compile and the ones that do run 8x slower and emit 13% more wasm.

### What this settles for RFC-0082

The thesis holds where its argument reaches, and its argument reaches exactly as
far as `Array`. A container whose spare capacity is a heap buffer it owns is
writable in Vyrn, is fast, and `std/slots` is the proof. A container whose spare
capacity lives inside the value is not, and no protocol closes that: it needs an
uninitialized place, which is the one thing RFC-0089's model exists to remove.

**`SmallArray` stays a built-in, and this is the reason to write in the census
rather than "not ported yet".**

### What would have to be true to try again

Three features, in this order, and the third is the only one this RFC owns.

1. **Const generic parameters** — `N` bound by a declaration, substituted like
   `T`, monomorphized per value, with the layout computed per instance. Parser,
   checker, both backends and the interpreter.
2. **An uninitialized place, or a `T` from nothing.** Either a place the checker
   knows holds no value and a rule that no read reaches it, or a `Default`
   protocol and a container that seeds `N` of them. The first re-opens what
   RFC-0089 closed. The second changes what `SmallArray<T, N>` costs to make,
   from nothing to `N` constructions.
3. **A conditional yield**, which is this RFC's gap and is cheap next to the
   other two — and which the hand-inlined measurement says would not pay for
   itself here.

Feature 3 is worth doing on its own merits, for the containers that are not
`SmallArray`. Features 1 and 2 are a language, not a milestone.

---

## M5 — a projection may choose between two places

Two phases met the same refusal from opposite sides. Phase 8a found that
`Iterate` cannot skip. M4 found that `Index` cannot pick a buffer. This section
says what closing that costs, what it buys, and what it does not buy. It also
takes the two findings apart, because **they are not the same feature and this
RFC has been calling them siblings.**

### What is refused today, and it is one rule, not two

`checker::check_places` (`compiler/vyrn-frontend/src/checker.rs:1129`) writes the
four rules. Two of them are in play.

M4's blocker-3 program trips rule 1. Checked, on the file as M4 wrote it:

```text
`place at` must end with exactly one `yield <place>` — a projection is inlined
at the access site, so it has one exit
```

That is `checker.rs:1140`, and the shape it refuses is a `yield` inside an `if`
with a second `yield` after it. But that is a choice of spelling. The same
container written with the branch INSIDE the yielded expression trips rule 2
instead:

```text
`place at` yields a value, not a place — write `yield <field or element of
self>`; a projection that computes a new value is an ordinary `fn`
```

That is `checker.rs:1160`, reached from `project::is_place`
(`compiler/vyrn-frontend/src/project.rs:878`), which answers `false` for
`Expr::IfExpr`.

**The second spelling already parses.** `yield` takes an ordinary expression
(`compiler/vyrn-frontend/src/parser.rs:3551-3552`), and an `if` reached in
expression position is `Expr::IfExpr` (`parser.rs:4380`, RFC-0030). The lexer,
the parser and the grammar do not move. The refusal is one arm of one function.

So **M5 relaxes rule 2 and leaves rule 1 exactly as written.** A projection
still has one `yield`, still last, still one exit. What changes is what a place
may be: a variable, a field of one, an element of one — **or an `if` whose
every branch is a place.**

That distinction is the design, not a detail. Rule 1 is what says a projection
has ONE frame to bracket, and M2's whole inlining is written around it.

### The syntax, and the container it closes

There is no new syntax. This is a two-buffer deque, written today, in a file
`vyrn fmt --check` accepts unchanged:

```vyrn
type Deque<T> = { front: Array<T>, back: Array<T> }

impl<T> Index for Deque<T> {
    type Key = Int64
    type Value = T
    place at(read self, i: Int64) -> T {
        yield if i < self.front.length {
            self.front[self.front.length - 1 - i]
        } else {
            self.back[i - self.front.length]
        }
    }
    place atSet(modify self, i: Int64) -> T {
        yield if i < self.front.length {
            self.front[self.front.length - 1 - i]
        } else {
            self.back[i - self.front.length]
        }
    }
}

impl<T> Iterate for Deque<T> {
    fn size(read self) -> Int64 {
        return self.front.length + self.back.length
    }
    place nth(read self, i: Int64) -> T {
        yield if i < self.front.length {
            self.front[self.front.length - 1 - i]
        } else {
            self.back[i - self.front.length]
        }
    }
}
```

`vyrn check` on that file reports **three diagnostics and they are all rule 2**,
one per projection, and nothing else. The generic impl head solves, `type Key`
and `type Value` are read, `size` is accepted, the record literal builds. So a
generic container over two growable `Array`s is blocked by exactly one thing
today, and this is it.

That is the class M5 opens: a container whose element lives in one of two heap
buffers. A deque as two stacks, a rope as two halves, a hot and cold pair.
**Every one of them is on the NEAR side of the boundary M4 drew** — its spare
capacity is a heap buffer it owns, RFC-0082's argument reaches it, and
`std/slots` is the evidence that the argument holds there.

### `SmallArray`'s two-state read — closed, and still not portable

```vyrn
type Small4<T> = { len: Int64, inline: Array<T, 4>, spill: Array<T> }

impl<T> Index for Small4<T> {
    type Key = Int64
    type Value = T
    place at(read self, i: Int64) -> T {
        yield if self.spill.length == 0 { self.inline[i] } else { self.spill[i] }
    }
}
```

Checked: the only complaint against that `impl` is rule 2. M5 closes it exactly.
The container still does not exist, because nothing above builds an empty
`Small4` and nothing names the capacity — M4's blockers 1 and 2, untouched. See
"Does `SmallArray` become portable" below.

### `Slots`' skip — NOT closed, and it is a different feature

What a skipping `Slots` would want to write is not a choice between two places:

```text
place nth(read self, i: Int64) -> T {
    yield if self.alive[i] { self.data[i] } else { <no place at all> }
}
```

`nth` must answer for every `i` in `0 .. size`. A conditional place has two arms
and BOTH yield. A skip yields nothing on some turns, and the loop then has
nothing to bind: `project::iterate_loop` makes the element a `let x = <the
place>` (`project.rs:536-543`), and there is no expression for "no element this
turn". A place that may not exist is an `Option` of a borrow, which is a
different feature and a much larger one.

**So Phase 8a's gap stays open after M5, and this RFC's own text calling the two
gaps siblings is wrong.** M4 wrote "it is the sibling of the gap Phase 8a
found". 8a's gap is a missing CURSOR. M5 is a missing BRANCH. Closing one does
not touch the other.

### Should `Iterate` gain a skip predicate instead? No

The cheap form is real. `iterate_loop` (`project.rs:473`) would take a third
member, `fn live(read self, i: Int64) -> Bool`, and wrap the turn's tail in an
`if`. About ten lines, one function, no engine change — smaller than M5.

It should still not be built, and `std/slots` is the reason. `Slots` keeps a
dense array of live slots plus the map back from a slot to its place in it.
RFC-0090's "M1 as landed" calls that the standard slot-map layout and records
that it is what makes `remove` O(1). **The dense list is not a workaround for a
missing predicate. It is the layout**, and a container that has paid for O(1)
removal has already paid for an honest walk. A predicate would let a container
walk `n` slots to yield fewer than `n`, which is the slower of the two designs,
and no container in the corpus wants it.

Recorded as refused with a reason, not deferred. What Phase 8a needs is a
correction to its own wording: "`Slots` skips dead slots by implementing
`Iterate`" was wrong, and the dense list is the right answer rather than the
consolation one.

### What it costs to check

Three arms, in two frontend files.

- **`is_place`** (`project.rs:878`) gains an `Expr::IfExpr` arm: every branch
  must be a place. It already recurses, so an `else if` chain falls out.
- **Rule 3 becomes a leaf walk.** `check_places` reads `project::place_root`
  once and checks one root (`checker.rs:1167-1172`). A conditional place has one
  root PER LEAF. `self.front[i]` and `self.back[j]` are two leaves and one root;
  `self.data[i]` and `other.data[j]` are two leaves and two roots, and only the
  second is refused. So `place_root` collects roots instead of returning one,
  and every root is checked. An `IfExpr` arm that read the `then` branch and
  stopped would be a hole in rule 3, which is the one thing this milestone must
  not get wrong.
- **Typing costs nothing.** The branches unify like any if-expression, which the
  checker has done since RFC-0030, and `place_result` solves the impl head from
  the DECLARED return type (Phase 8a), never from the body.

One new diagnostic is worth writing. An `if` used as a place with no `else` is a
place that may not exist. The checker already refuses it with "`if` used as an
expression needs an `else`", which says the right thing for the wrong reason.

### What it costs to lower, engine by engine: nothing

All three engines reach a projection through the same two calls, which is what
`project.rs` was written to make true (`project.rs:12-16`).

**A conditional read is zero lines in every engine.** A read is the prologue
followed by the yielded expression — textual
`compiler/vyrn-codegen/src/lib.rs:8408-8418`, direct
`compiler/vyrn-codegen/src/direct.rs:8064-8072`, interpreter
`compiler/vyrn-frontend/src/interp.rs:3449-3458`. `Expr::IfExpr` is already an
expression each of them lowers: `lib.rs:4373`, `direct.rs:3446`,
`interp.rs:1200`.

**A conditional store is one arm in `project.rs` and zero lines in every
engine.** All three call `project::store_stmts` (`project.rs:372`) and take back
statements — `lib.rs:3179-3186`, `direct.rs:835-840`, `interp.rs:3423-3434`. It
gains an `IfExpr` arm: a `Stmt::If` whose two blocks are `store_stmts` of the
two branches, with the stored value put in a `let` first so it runs once and
both arms write the same temporary. `parser::hoist_operand` is what the two
existing arms already use for exactly that. Each arm keeps its own
`parser::place_receiver` move-out and move-back, which is correct: the two
branches name different receivers.

**`for x in c` costs nothing.** `iterate_loop` uses the yielded place as the
initializer of `let x` (`project.rs:536-543`), so it is a conditional read.

The textual backend's `static_ty` and the direct backend's `peek` are asked
about the RECEIVER, never about the yielded place, so neither learns a new
shape.

### Node identity, and why 7a's byte-identity result does not move

Phase 7a's result — 118 `.ll` files and 119 `.wasm` modules byte-identical —
holds because `Projection::is_identity` (`project.rs:72`) recognizes the seeded
row's identity substitution, and each engine then lowers the ORIGINAL nodes.

A conditional place is never the identity. `is_identity` matches only an empty
prologue yielding `@slot` of the access site's own expressions
(`project.rs:73-80`), and an `IfExpr` fails on the first arm. Every builtin
container keeps the identity path, and no builtin container has any reason to
declare a conditional projection, so **no emitted byte in the corpus moves.**

For a user container, the choice that matters is putting the branch INSIDE the
yielded expression rather than around the body. **The body is not cloned into
two arms.** `project::inline` (`project.rs:278`) clones the body once per access
site, exactly as it does today. The two side tables 7a called out — the elided
`get`/`set` generation checks and the lambda monomorphization keys, both keyed
by node address (`interp.rs:2452`, `direct.rs:6623`, `lib.rs:3643`) — see one
clone where they see one clone now. 7a's two conservative misses (one extra
check, one duplicated instance) do not double and do not change. A design that
branched around the BODY would double both, and that is the reason not to write
one.

### Rule 2 of RFC-0089 survives, and there is still one frame

A projection exists because rule 3 says a return is owned and rule 2 says a
borrow may not be returned. The borrow never escapes because the caller's access
runs bracketed inside the callee's frame (`project.rs:5-9`).

**A conditional place does not have two frames to bracket. It has one.** One
prologue, one exit, one caller frame. The branch is a condition inside a single
expression evaluated at the access site, and one arm's place is read. The
property M2 rests on is unchanged, and it holds for two arms for the same reason
it holds for one.

What keeps it true is the per-leaf rule 3 above: every arm must be a place
rooted in the receiver or in a parameter, so no arm can hand out a place the
access site does not own. Rule 3 is where a conditional place could break rule
2, and it is the only place.

### The straight-line drop site — checked, and Phase 5 is what is true

RFC-0075 M2b put the variant branch inside `@__vyrn_stream_close` on the stated
ground that `emit_all_drops` runs mid-block on an early exit and needs every
drop SITE straight-line. `compiler/vyrn-frontend/src/own.rs:64-71` still carries
that sentence. RFC-0089 M3a reports that the argument "did not survive contact"
(RFC-0089, "Where the release lives", line 495).

**Phase 5's report is what is true today, and the emitted code says so.**
`DropKind::Deep` (`lib.rs:3505`) calls `deep_release`, which reaches
`release_sum` (`lib.rs:2829`) and `release_enum` (`lib.rs:2869`), and both emit
`br i1` with fresh labels (`lib.rs:2842`, `lib.rs:2886`) at whatever point in
the block the drop was reached — including from `emit_all_drops`
(`lib.rs:3380`) before a `ret`. A branching lowering mid-block is something this
backend already emits on the early-exit path.

**M5 does not test the rule either way.** Its branch sits inside a read or a
store, and the binding the drop stack records is the container, never the
projected place. The rule matters here only as a warning to a future design that
tries to give a projection two exits. This one does not.

### What it does not buy

**Not speed.** M4 wrote the branch at the access site by hand — which is what an
inlined conditional yield emits — and measured it beside the projection it would
replace. `vyrn bench`, native, median:

| bench | Vyrn, projection | Vyrn, branch hand-inlined |
|---|---|---|
| indexed sum, 1024 reads | 882 ns | **901 ns** |

19 ns apart on 1024 reads, and in the direction that is not an argument for
building it. **M5 is an expressiveness feature and this RFC must not claim
otherwise.** Everything M4 measured as the cost of a Vyrn small-buffer container
— 7.9x on reads, and the 20x fixed-array spill underneath it — is untouched.

It also deletes nothing. No built-in goes away and the census does not move.

### Does `SmallArray` become portable? No, and here is how the three separate

M4 named three blockers. M5 closes the third and only the third.

- **Blocker 3 is separable in both directions.** The deque above needs it and
  needs neither of the others. A `SmallArray` with const parameters and an empty
  inline buffer would still need it.
- **Blocker 1, const generic parameters, is separable from 3 and not from 2.**
  One module per capacity (`Small4`, `Small8`, …) is the workaround for 1, and
  such a module still cannot write its own empty state.
- **Blocker 2, an uninitialized place, decides `SmallArray`, and it is not
  separable from anything, because it is the model.** RFC-0089 exists so that
  every place holds exactly one value. A place that holds none is that rule
  inverted, not a feature beside it. The weaker form IS separable — a `Default`
  protocol and a container that seeds `N` values — and it costs `N`
  constructions where the built-in costs nothing, which makes it a different
  container.

So `SmallArray` needs 1 and 2, in that order, and then it needs 3. M5 on its own
moves the port from three blockers to two, and the two are the ones M4 called a
language rather than a milestone. Nothing about the port changes.

### The milestone, and its gate

**M5 — a projection may yield a conditional place.** `is_place` admits an `if`
whose every leaf is a place; `place_root` becomes a leaf walk and rule 3 checks
each leaf; `store_stmts` gains an `IfExpr` arm. Rule 1 does not move, no keyword
is added, and no engine is touched.

The gate is not a benchmark — M4 has already measured that there is nothing to
win. It is three conditions, and the first is the one that can stop the work.

1. **The diff touches `project.rs` and `checker.rs` and nothing else.** If any
   engine needs one line, the branch was put in the wrong place and the design
   above is wrong. Three engines' worth of `IfExpr` lowering already exists;
   reaching it is the entire claim.
2. **A container that this alone unblocks compiles and passes three-way
   parity.** A two-buffer deque in the corpus, exercising `c[i]`, `c[i] = v` and
   `for x in c`, on three engines, with the interpreter as the oracle. A
   milestone whose only product is expressiveness has to ship the expression.
3. **The corpus does not move a byte.** 118 `.ll` and 119 `.wasm` diffed against
   the parent, zero differences outside the new file. `is_identity` must still
   answer `true` everywhere it answers `true` today, and the emitted output is
   how that is proved rather than asserted.

**Recommended, and gated on condition 2.** It is the smallest milestone in this
chain, and it is the last refusal in `check_places` that refuses a program the
memory model permits: the other three each refuse something rule 2 or rule 3 of
RFC-0089 forbids, while this one refuses a shape the model allows and the
lowering can already express.

Condition 2 is a real gate and not a formality. **If no container in the corpus
wants a second buffer, M5 does not ship.** A language feature whose only user is
its own test is ungated multiplicity, and the workspace rule is that ungated
multiplicity rots. The honest way to find the user is to reach for a deque in a
dogfood program first — which is how `Window`, `Slice` and `Ring` found seven of
this RFC's bugs.
