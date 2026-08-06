# RFC-0091 — The Container Protocols

- **Status:** **M1, M2 and M3 implemented** (Phases 7a and 7b); M4 not started.
  The generalization layer over RFC-0089/0090: what makes a third-party
  container indistinguishable from a built-in. `place`/`yield` members, `Copy`,
  `Iterate` and `Index` all ship, and `std/slots` is the customer that proved
  them. Read "M2 as landed", "M1 and M3 as landed" and "The generic-container
  correction" — the last records two things this RFC says that are wrong.
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

### A generic `impl Owned` has no release

`Owned` predates this RFC and takes the same correction. A generic impl's
`release` flattens to a generic function, and the drop site emits the flattened
name with no type arguments — a symbol nothing defines, reported by clang at the
end of a build. Phase 8a filters generic impls out of the `Owned` table, so the
result is a missing release rather than a link error. Monomorphizing a declared
release is the work; `Slots<T>` is what wants it.
