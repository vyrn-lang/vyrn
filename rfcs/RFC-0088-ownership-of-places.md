# RFC-0088 — Ownership of Places

- **Status:** **Superseded by RFC-0089**, which keeps this RFC's rule (a
  heap-owning type moves), its M1 (make it visible, as RFC-0089 M0) and its M5
  (places), and replaces the rest — RFC-0089 defines ownership rather than
  extending the analysis that infers it. Kept for the four-cause analysis below,
  which RFC-0089 builds on.
- **Depends on:** RFC-0004 (the hybrid model), RFC-0086 (the compiler asks the
  type), RFC-0087 (the census this answers)
- **Answers:** RFC-0087 §3, §4, §14, §15, §16, U1, U2, U4, U10, P1, P2, P3, P8

---

## What RFC-0087 found

Thirty-odd gaps across correctness, usability and cost. They are not thirty
problems. Four causes produce all of them.

| cause | what it produces |
|---|---|
| **A.** a heap value inside an aggregate has no owner | §3, §14, §15, §16, U4 |
| **B.** ownership belongs to a *binding*, never to a *storage location* | §4, §2c, P1 |
| **C.** the answer is never output | §13, U1, U9, U10, P8 |
| **D.** a property is hardcoded per type instead of declared | §7, §2b, U7, U8 |

RFC-0086 named **D** and shipped M1 against it. This RFC is about **A** and **B**,
with **C** first because it is the instrument that proves the rest.

---

## The rule

> **A place owns its contents. A type that transitively owns heap moves.**

Two sentences. Everything below is the consequence.

### A place

Today the unit of ownership is a `let`. `own::droppable` is keyed by
`Stmt::Let` node address. A `let` is created once and dies at block exit.

A **place** is any storage location: a `let`, a module-state global, a record
field, an array element, a map value, an enum payload, a closure capture. A place
has a type, and `Owned::release_kind` already answers for a type.

A place owns what is in it. So:

- storing into a place releases what was there (§4, P1)
- releasing an aggregate releases its places (§3, §14, §16, U4)
- `drop arr` reaches the elements (U4)

### Transitively owns heap

A type owns heap if `release_kind` answers for it, **or for anything it holds**.
Derived, not declared, and computed from the seeded `Owned` table RFC-0086 M1
already built.

| type | owns heap | behaviour |
|---|---|---|
| `Int64`, `Bool`, `F32x4` | no | copies, as today |
| `Option<Int64>` | no | copies, as today |
| `Array<Int64>` | yes | moves |
| `Option<String>` | yes | moves |
| `{ name: String, age: Int64 }` | yes | moves |
| `{ x: Int64, y: Int64 }` | no | copies, as today |
| `Ref<T>` | no — it is `{slot, gen}` | copies, as today |

**This is the load-bearing line.** RFC-0004 §2 set out to avoid the borrow
checker's error surface, and a move rule is that surface. The rule above keeps it
off the 90% case by construction: a record of numbers, an `Option<Int64>`, a
`Ref<T>` and every scalar copy exactly as they do now. The move appears where a
value contains a heap allocation — which is where the alternative is a silent
leak.

### Moving

```vyrn
let a = Person { name: n, age: 3 }
let b = a          // moves. `a` is dead.
print(a.name)      // error: `a` was moved on line 2
```

`movecheck` already does this, flow-sensitively, with branch merging and loop
rejection, for the `consume` capability and for `Stream<T>`. The rule extends what
it checks, not how it works.

### Copying

```vyrn
let b = a.copy()   // `a` stays alive
```

`copy` is the escape hatch, and it is the same builtin RFC-0087 U2 needs on its
own. Today the corpus writes `arg + ""` — a concatenation used for its allocation,
resting on an implementation fact. `copy` says what it means and the cost is at the
call site where a reader can see it.

**U2 and cause A are one hole.** Owning aggregates need a copy, and a safe extern
boundary needs a copy. One word closes both.

---

## What falls out

| RFC-0087 gap | how |
|---|---|
| **§14** `Option`/`Result`/`Validation` leak the String inside | the payload is a place; the wrapper owns it |
| **§16** a closure capture block is never freed | the captures are places |
| **§3** a record field, array element or map value is a safe leak | each is a place |
| **U4** an element is unreclaimable | releasing the container reaches its places |
| **§4** an overwrite never releases the old value | a store to a place releases first |
| **P1** a module-state accumulator is quadratic and leaks 12.2 GB | §4, plus `str_append` generalized from a local to a place |
| **§2c** a `mut` String is never tracked | a `mut` local is a place like any other |
| **§7** linearity is hardcoded to `Stream` | a `Stream` transitively owns heap, so it moves for the general reason. **RFC-0086 M3 stops being a milestone** |
| **§9a/§9b/U10** the extern boundary cannot say who owns a String | the answer exists per place; the boundary prints it |
| **§15** `bytes` and the routed builtins leak | they return an aggregate that now owns |

Eleven rows, one rule.

---

## What does not change

- **Path B is untouched.** A `Ref<T>` copies freely, and the cell is owned by the
  place holding it. RFC-0004 §5.2 decided that `Ref<T>` is the tool for aliasing;
  this makes that purpose legible, because a `Ref<T>` is now exactly what you reach
  for when the move rule is in the way. §5.3's 94% already said the corpus does
  this.
- **Regions are untouched.** Q3's inferred regions stay open.
- **Parity is untouched.** A move is a compile-time fact. No engine emits anything
  different for it.
- **The seeded `Owned` table is the input.** RFC-0086 M1's mechanism is what
  "transitively owns heap" is computed from, so a third party's `impl Owned for T`
  joins the move rule with no compiler change — the same property M1 proved for
  release.

---

## Milestones

**M1 — Make it visible.** No semantic change. `vyrn why --memory <file>` prints,
per place, whether it is reclaimed and why not. The same text as an LSP hover.
`tests/memory.rs` grows from one shape to the RFC-0087 list. Benchmarks for an
allocation, a String concatenation and a `get`.

This is first because it is the instrument. Every milestone below is a change to
what memory does, and nothing today can measure that. It is also the cheapest item
in RFC-0087, and it closes **U1** and **P8** on its own.

**M2 — A String gets a header.** Length and capacity, so `byteLength` is O(1) and a
concatenation stops scanning both operands. The allocator already pays an 8-byte
header per block for exactly this reason (RFC-0077 M6 chose it because a drop site
cannot recover a String's capacity); this moves it where it is useful. `str_append`
stops needing a shadow. Closes **P2**, and makes `copy` cheap enough to be the
answer to M3.

Independent of M3 and M4, so it can run in parallel.

**M3 — `copy`.** One method. Closes **U2**. Prerequisite for M4, because a move
rule with no copy is a language you cannot write.

**M4 — Aggregates own, and move.** The core. `movecheck` learns which types
transitively own heap and extends its existing analysis to them. `own::release_kind`
gains a recursive form that walks an aggregate's places.

**The real cost is here and it should be stated plainly: `movecheck` has no types.**
`check_accum` takes a bare `Program`, and its own comments say so three times. This
milestone is where it gets a type map, and that is the bulk of the work.

**M5 — Ownership moves from bindings to places.** `own::droppable` is keyed by
`Stmt::Let` address; it becomes keyed by place. A store releases the old contents
*after* the new value is computed, because the old value is usually an operand of
the new one. Closes **§4** and **P1**.

**M6 — The boundary declares.** `own`'s per-place answer is emitted into the export
map, so `wasi-min.js` releases a returned String it owns and leaves one it borrows.
A checker rule refuses an exported function that stores a borrowed `String`
parameter, and names `copy` as the fix. Closes **§9a**, **§9b**, **U10**.

---

## What could go wrong

**The error surface.** This is the objection RFC-0004 §2 raised against Rust and it
applies here. Two answers, and the second is the real one:

1. The rule keeps the surface off scalars, records of scalars, `Option<Int64>` and
   `Ref<T>` — the 90% case RFC-0004 was protecting.
2. The alternative is measured. **12.2 GB to build a 160 KB string, 52× slower than
   the identical loop over a local, and no diagnostic.** A compile error that names
   `copy` is better than a cliff nobody can see.

M1 is what turns this from an argument into a count: how many corpus sites need a
`copy`. If that number is large, the rule is wrong and M1 will have said so before
M4 is written.

**Generic code.** `fn f<T>(x: T)` — does `T` move? Unknown until instantiation.
Monomorphization means the answer exists per instance, and the checker runs before
it. Either the check moves after monomorphization, or a bound says it. **Undecided,
and it is the one design question this RFC does not answer.**

**A `mut` place that is conditionally initialized.** A store releases what was
there, and "what was there" on the first store is nothing. Needs an
initialized-ness fact per place, which is flow analysis `movecheck` already does
for consumption.

---

## Rejected

**Reference counting for aggregate payloads.** RFC-0004 lists refcount as a last
resort and the reasons hold. It costs an increment on every copy — a run-time price
on the 90% path, against a compile-time rule that costs nothing. It is also *more*
invisible rather than less, which is the wrong direction for RFC-0087 Part II.

**Deep copy on every aggregate copy.** Predictable and safe, and it makes
`let b = a` an unbounded cost with nothing at the call site to say so. The whole
usability thesis is that a cost should be visible; `a.copy()` is visible and
`let b = a` is not.

**Inferred regions for aggregate payloads.** This is RFC-0004 Q3 and it would
answer cause A for values with one lifetime. It does not answer "which region"
for a value stored in module state, which is P1's shape and the shape Vyrn
targets. Q3 stays open on its own merits.

---

## The order is the argument

M1 changes nothing and measures everything. If the census is right, M1 makes the
gaps countable. If it is wrong, M1 says so before a single semantic change lands.

That is the same discipline RFC-0004 §5 set for the memory model and then only
half-ran. It measured whether the generation check was fast enough (§5.1) and
answered yes. It never asked how many checks were necessary until §5.3, eighty
RFCs later. The answer, when it was finally asked, was 6%.

Measure first.
