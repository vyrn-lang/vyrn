# RFC-0089 — Mutable Value Semantics

- **Status:** Accepted, landing. M0, M1a, M1b and M2 are implemented — see the
  "as landed" sections, which are the truth where they and the design differ.
  M3 (places and the boundary) is not built. Supersedes RFC-0088; RFC-0088's M1
  (make it visible) survives unchanged as this RFC's M0.
- **Depends on:** RFC-0004 §1 (the capability surface — this RFC finishes it),
  RFC-0086 (the compiler asks the type), RFC-0087 (the census)
- **Premise:** backwards compatibility is not required. The corpus migrates.

---

## The question this answers

RFC-0088 keeps today's semantics and extends the ownership *analysis* until it
covers more cases. That is the compatible answer. This RFC is the answer when
compatibility is off the table:

> **Stop inferring ownership. Define it.**

Today a Vyrn program has no memory semantics. It has a best-effort analysis
(`own.rs`) that frees what it can prove and silently leaks the rest. "Safe leak"
appears fourteen times in the census because it is the *defined fallback* of the
model. No amount of extending the analysis removes the fallback — a cleverer
guesser is still a guesser, and its failure mode is still silent.

The alternative: every heap value has exactly one owning place, established by
the language definition rather than discovered by analysis. A program that would
leak does not compile with a leak. It either compiles with known reclamation or
it errors with a named fix.

---

## The design, in five rules

### 1. Values are values

A type that transitively owns heap **moves** on assignment, argument passing and
return (RFC-0088's rule, kept). Everything else copies, as today. `let t = s` on
a `String` moves; `print(s)` after it is a compile error naming the move.

### 2. The capabilities are the calling conventions

RFC-0004 §1 already wrote the table. It becomes enforced, not surface:

| convention | today | becomes |
|---|---|---|
| `read` (default) | nothing | callee may observe; **may not retain, store or return** the value |
| `modify` | nothing | exclusive in-place access; same no-retain rule |
| `consume` | move-checked | sink — ownership transfers in, callee releases or forwards |
| `share` | nothing | `read` across a `spawn` boundary, as RFC-0004 intended |

`read` and `modify` are **second-class**: a borrowed value cannot be stored in a
field, captured by an escaping closure, put in a container, or returned. That
one restriction is what makes the whole thing work without lifetime annotations
— a borrow that cannot escape needs no lifetime, because its lifetime is the
call. This is the published result Vyrn's design has been circling: mutable
value semantics gives whole-program memory safety with zero annotation beyond
the conventions themselves.

**Rule 2 ships with a companion: `for x in consume xs`.** Iteration binds a
`read` borrow of the element, so `for x in xs { out.push(x) }` is a store of a
borrow and rule 2 refuses it. Without a way to move an element *out* of a
container, the only fix is `x.copy()`, and the corpus measured 207 such loops.
The consuming form is that way out: the loop takes the container, each element
is **owned**, storing one is a move, and naming the container afterwards is the
rule 1 error. `consume` is the capability word it already is, at a new position;
the form is new, the vocabulary is not.

A loop over a value that is **not a place** — `for o in diffChildren(..)` —
is consuming without the word. A temporary has no other owner, so its elements
are already the loop's. That case is 91 of the 207 and it needed no syntax at
all: it was rule 2 being wrong about a container nobody else holds.

### 3. A function returns an owned value. Always.

No borrowed returns. `fn id(s: String) -> String { return s }` must take
`consume s` (and forward ownership) or return `s.copy()`. The compiler says
which two fixes exist.

### 4. A place owns its contents

RFC-0088 M5, kept: storing into a place releases the old contents; releasing an
aggregate releases its places; `drop` is deep.

### 5. `Ref<T>` is the one runtime mechanism, and the only one

Aliasing that must be stored goes through `Ref<T>`, exactly as RFC-0004 §5.2
decided. Nothing else in the language has a runtime check. The model is
**completely compile-time except where the program explicitly wrote `cell`** —
and §5.3's measurement says the corpus already writes `cell` only where aliasing
is real.

---

## Why this is meaningfully better, not differently shaped

**`own.rs` is deleted, not extended.** The escape walker, the safe-read list,
the `owned_fns` fixpoint, `transfers`, `expr_type` — all of it exists to *infer*
what rules 2 and 3 now *declare*. What replaces it is a checker: signatures in,
diagnostics out, no fixpoint, no under-approximation, no leak-when-unsure.
Checking a declaration is smaller and stronger than inferring its absence. This
is RFC-0086's thesis taken to its end: the compiler asks the signature.

**The census, re-run against the five rules:**

| gap | outcome |
|---|---|
| §2a six forms never transfer | gone — transfer is rule 1, not an expression list |
| §2b `builtin_producers` hand list | gone — builtins get signatures like everything else |
| §2c `mut` String untracked | gone — rule 4 |
| §3/§14/§15/§16/U4 aggregates leak payloads | gone — rule 4 |
| §4/P1 overwrite leaks, 12.2 GB accumulator | gone — rule 4; and with no aliasing, `s = s + x` reallocs in place **everywhere**, including module state |
| §7 linearity hardcoded to `Stream` | gone — a `Stream` owns heap, so it moves; RFC-0086 M3 stops existing |
| §9a/§9b/U10 the extern boundary | gone by fiat — rule 3 means a returned String is always the caller's to free; a stored parameter is refused by rule 2 |
| U2 no way to copy | `copy` ships as the named fix in every move diagnostic |
| U7 `consume` and linearity are two things | one thing — rule 2 |
| U8 a declared container cannot be read | gone — `read self` is the receiver convention, and the safe-read list dies with `own.rs` |
| §6/U5 use-after-release traps at runtime | confined to explicit `Ref`, the declared aliasing tool |
| P2 String scans | String carries `{len, cap}` — see "M1a as landed" below |

Three undesigned items in RFC-0087 (inferred regions, region borrowing, the
compile-time story for aliasing) shrink to one: `Ref<T>` remains the runtime
island, by choice, with the trap as its documented failure mode.

**And the model becomes teachable.** Today the answer to "is this freed?" is
"depends what the analysis proved." Under the five rules the answer is a
sentence: *you own what you hold; calls borrow unless marked `consume`; returns
are yours; use `copy` to keep both; use `cell` to share.* U1's printer is still
worth building (M0), but the thing it prints becomes a rule, not a report.

---

## What it costs, stated plainly

- **Move errors are the new error surface.** RFC-0004 §2's worry. Contained by
  the same line as RFC-0088: scalars, records of scalars, `Option<Int64>`,
  `Ref<T>` copy freely — the surface only exists where heap is owned. The
  measured alternative is the 12.2 GB cliff with no diagnostic.
- **The corpus migrates.** ~124 examples, `std/`, the generators. Mechanical in
  the common case (`return s` → `consume` or `.copy()`); this is the price of
  no-compat and it is paid once.
- **Escaping closures capture by move or copy, declared at the capture.**
  RFC-0037's stored closures cannot hold borrows; non-escaping lambdas
  (`map`/`filter` arguments) borrow freely. The checker distinguishes them the
  way `own.rs` already does — except now it refuses instead of leaking.
- **Generics:** whether `T` moves is known at monomorphization; the convention
  checker runs per instance (Vyrn already monomorphizes everything). The one
  RFC-0088 open question closes.
- **`movecheck` gets types.** Unavoidable in any variant of this work; it is
  the bulk of M2.

---

## Milestones

- **M0 = RFC-0088 M1.** The instrument: `vyrn why --memory`, memory tests over
  the census shapes, alloc/concat/`get` benchmarks. Lands before any semantic
  change, and its counts (how many corpus sites move, how many need `copy`)
  are the go/no-go evidence for M2.
- **M1 — `copy`, and String as a value triple.** Both backends and the
  interpreter. P1 and P2 die here even before the conventions land.
- **M2 — the convention checker.** `movecheck` grows types and enforces rules
  1–3. `own.rs` shrinks to the `Owned`-table lookup (RFC-0086 M1's part) plus
  drop-site emission. The corpus migrates in the same change, module by module.
- **M3 — places (RFC-0088 M5) and the boundary (M6).** Store-releases,
  deep drop, export ABI ownership by rule 3.

---

## M1a as landed — the String header

This RFC sketched `String` as a value triple `{ptr, len, cap}`, "like `Array`
already is". Implementation measured that and chose differently: the two words
sit **behind** the pointer, not beside it. A `String` is still one word, and it
still addresses NUL-terminated UTF-8, so the extern ABI and every C sink are
unchanged. The sixteen bytes in front of it (eight on wasm32 — two
pointer-sized words on each target) hold `len` and `cap`.

Three measurements decided it.

1. **An `Option<String>` payload is one word.** A three-word String does not
   fit, so `Some(s)` would box — one `malloc` per construction. That moves the
   `optionString` row of the census baseline (`compiler/vyrn-cli/tests/memory.rs`
   §14), which Phase 2 was required not to move. `Result`, user enum payloads,
   `Map<String, V>` keys and `Array<String>` elements all carry the same
   assumption, and a triple widens every one of them.
2. **A triple goes stale under aliasing; a header cannot.** The RFC's argument
   for the triple was that rule 1 forbids the aliasing that would break it. Rule
   1 is M2. Until it lands, `let t = s` is legal, and two triples would hold two
   lengths for one buffer. Two pointers to one header hold one length.
3. **The extern boundary needs no conversion at all.** A `String` crosses to JS
   as a `ptr` (RFC-0012 M2) and still does. `wasi-min.js` writes the header when
   it allocates and subtracts it when it frees; nothing else in the language
   sees the boundary.

`cap == 0` means static: a literal in the data segment, never `realloc`'d, never
freed. That is the fact RFC-0077 M6 said a drop site could not recover, and it
is now a field rather than an inference.

What P2 asked for is delivered: `byteLength` is a load, `a + b` reads two
lengths instead of scanning two operands, and `str_append`'s shadow `(len, cap)`
is gone — the header IS that pair. One word of shadow survives per accumulator,
holding a question the header cannot answer: did this path allocate the buffer?
That word is ownership, which is M2's subject, and it retires with M2.

Measured on the M0 benchmarks (`examples/membench.vyrn`, medians):

| row | before | after |
|---|---|---|
| `byteLength` of 10 KB × 500 | 149.08 µs | 5.14 µs |
| string concat, 1000 fresh buffers | 29.14 µs | 18.99 µs |
| string append spine, 1000 appends | 6.13 µs | 4.54 µs |
| array push churn, 1000 pushes | 540 ns | 541 ns |

The cost is code and data size on wasm: `fib.wasm` grew from 1,334 to 1,590
bytes, and `domdemo.wasm` from 25,966 to 27,630.

## M1b as landed — `copy`

`x.copy()` ships in all three engines. It is a method-only builtin (`@copy`
internally), so a free `copy(x)` is still an available user name, and its result
has the receiver's own type — a copy of a validated `type Email = String` is
still an `Email`.

The copy is structural and recursive: `String`, `Array<T>`, `SmallArray<T, N>`,
`Map<String, V>`, records, fixed arrays, `Option`, `Result` and user enums,
through their payloads. Four decisions the RFC did not state.

1. **A scalar copies to itself, and this is not a diagnostic.** The alternative
   was to refuse `n.copy()` on an `Int64`. It was rejected because Vyrn
   monomorphizes: `fn twice<T>(x: T)` calling `x.copy()` is a String in one
   instance and an `Int64` in the next, and a refusal would make `copy` mean
   something narrower than "a value of this type that shares nothing with the
   receiver". One word, one meaning. The compiler emits nothing at all for a
   receiver that owns no heap, so the no-op costs no instruction.
2. **A `Ref<T>` copies as the handle it is.** §5 already said `Ref` is the one
   aliasing mechanism, so copying one names the same cell. That is also what
   keeps the analysis sound: `copy` is a producer in `own.rs`, and calling a
   `Ref` copy a transfer would release one cell twice. The rule is written as
   the `owns_heap` predicate — a `Ref` is excluded from it — so one function
   answers for the checker and for both backends. `Task<T>` and `lazy T` copy
   the same way, for the same reason.
3. **A type that declares `impl Owned for T` (RFC-0086 M1) is refused, and the
   refusal reaches through anything holding one.** A declared container states
   how it is released and says nothing about how it is duplicated; copying its
   fields would run the declared `release` over two values that claim the same
   resources. The diagnostic names the type. RFC-0091 M1 makes `Copy` a protocol
   such a type implements, and that is where the refusal retires. A `Stream<T>`
   is refused too, for a nearer reason: it is a cursor over a producer, and two
   consumers of one cursor is not a copy.
4. **The copy's capacity is its length.** An `Array` or a `Map` with room to
   spare copies into a buffer sized to what it holds. Spare room is an
   allocation history, not part of the value.

Measured on the M0 benchmarks (`examples/membench.vyrn`, medians, two new rows):

| row | median |
|---|---|
| copy of a 10 KB String, 1000 times | 68.02 µs |
| copy of a 1000-element `Array<Int64>`, 1000 times | 66.98 µs |

Roughly 68 ns per copy of about ten kilobytes — one `malloc` and one `memcpy`,
which is the floor for the operation. The M1a rows are unmoved. `fib.wasm` is
still 1,590 bytes and `domdemo.wasm` still 27,630: nothing that does not call
`copy` emits anything for it.

The corpus still writes `arg + ""` in five places. Migrating them is M3b (the
plan's Phase 6), where the diagnostic that points at `copy` lands with them.

## M2 as landed, part one — rules 1 and 3

Phase 4b enforces **rule 1** (a value moves) and **rule 3** (a return is owned).
Rule 2's store refusal is measured, written and **not turned on**: see "What rule
2 costs" below. Four things this RFC did not say.

1. **A `let` of a borrow is not a store.** The RFC says a borrowed value "cannot
   be stored in a field, captured by an escaping closure, put in a container, or
   returned". An earlier reading added `let` to that list and refused
   `let ss = h.stylesheets`. That reading is wrong, and rule 2's own argument
   says why: a borrow needs no lifetime *because its lifetime is the call*, and
   a second local name cannot outlive the first. The new name is a borrow too,
   and every rule that governs the parameter governs it.

2. **A field read is a borrow, not a partial move.** `let t = r.s` does not take
   `s` out of `r` and leave a hole — rule 4 says the place owns its contents, so
   the record still does. `t` is a projection, second-class exactly like a
   parameter. The same holds for a `match` binder over a place scrutinee and for
   an element read.

3. **Builtins that store need a convention, and three of them do.** The census
   called `builtin_producers` a hand list that rule 2 would delete. The sink side
   is the same list read backwards: `push`, `set` and `cell` put their argument
   somewhere that outlives the call, and rule 1 governs them exactly as it
   governs `xs = [.., v]`. Everything else a builtin does with a heap argument is
   a read.

4. **`copy` is not recursive, and the RFC's M1b section says it is.** "Structural
   and recursive ... through their payloads" is true of a type with a bottom. A
   type that reaches itself — `Json` through `JArr`, `VyxNode` through its
   children — has none, and both compiling backends expanded one until the
   compiler's stack ran out: a crash, at compile time, with no line on it. That
   was true from the day M1b landed; rule 3 found it, because a `Json` field
   lookup is one of the sites rule 3 sends through `copy`. `copy` now refuses a
   self-referring type and names the fix, and `std/json` exports `copyJson` as
   the worked example. RFC-0091 M1's `Copy` protocol is where a type declares its
   own.

### What rule 2 costs, measured

Phase 4b implemented rule 2's store refusal and gated it off, because the corpus
said it was a phase of its own. Its table read 154 loop-variable stores, 105
parameter stores and 29 projections. Phase 4b-2 re-measured over **linked**
programs — 4b parsed each file alone, and a file parsed alone cannot name an
imported type, so `owns_heap` answered "unknown" and the site was invisible. The
real shape is different in a way that changed the answer.

## M2 as landed, part two — rule 2 and consuming iteration

Rule 2 is on. `for x in consume xs` ships in all three engines. Four things
worth recording.

**1. The 154 were not one shape, they were four.** Split by what is stored and
what is iterated, the 207 linked loop-variable stores are:

| shape | count | fix |
|---|---|---|
| the iterable is a temporary (`for o in diff(..)`) | 91 | none needed — the elements are already owned |
| a projection of the element is stored (`out.push(e.key)`) | 56 | `.copy()` |
| the iterable is a field (`for m in c.members`) | 19 | `.copy()` — taking it would leave a hole |
| `consume` the parameter, then `consume` the loop | 21 | both, and every caller must agree |
| a local container, dead after the loop | 25 | `for x in consume xs` |
| the container is used after the loop | 3 | `.copy()` |

The shape the design argued about — one pointer in two containers, fixed by
moving the element — is 137 of 207 (66%). The other 70 are a **field of a
borrowed record copied into a fresh array**, which is a semantically required
copy under any rule: the record still owns what was read out of it, and no
consuming form reaches it. Phase 4b's "154 defensive copies" was the wrong count
and, for a third of the sites, the wrong word. They are not defensive.

**2. The largest single fix was a correction, not a feature.** 91 of the 207 are
`for x in f() { out.push(x) }`. A temporary container has exactly one owner —
the loop — so binding its elements as borrows was over-conservative. A loop
iterates a **place** when the iterable is a variable, a field, or `xs[i]` (which
lowers to `at(..)`, a projection of its receiver); everything else is a fresh
value. That is the judgment a `let` already made, reused.

**3. The parameter stores split 21 `consume` to 241 `.copy()`.** Phase 4b's
rules 1+3 migration came out 58 `.copy()` to 4 `consume` and named the reason:
a mixed-return function leaks on the fresh path. 4b-2 has a second reason, and
it is stronger. **A `read` parameter is a promise that the caller keeps the
value.** A lookup helper — `vyxDir(attrs, name)`, `alternatives(c, name)`,
`localeKeys(d)` — is called many times over one container, so `consume` is
simply false there and `.copy()` is the honest answer. `consume` went where the
function is a constructor or a sink: `symbol(..)`, `gqlOk(v)`,
`vyxBuildModule(comps, ..)`, `uiHeadJoin2(a, b)`, and the `children` parameter
of every generated `.vyx` view.

**4. A self-referring type has no `copy`, so rule 2 makes the consuming form
mandatory, not optional.** `copy` refuses `Json` and `VyxNode` (M1b's rule, found
in 4b). Rule 2 refuses moving one out of a container. Between them there was, for
a moment, no legal way to take a `VyxNode` out of an `Array<VyxNode>` at all —
`std/vyx`'s `vyxCompileComponent` could not name its own root. `for x in consume
xs` is what unblocked it. Where the copy is still needed one level down,
`std/json`'s `copyJson` is the pattern, and `std/graphql` now carries
`gqlCopyErr` beside it.

**Exclusivity** (`f(modify a, ..a..)`) was already enforced by 4b and is
unchanged.

**Measured.** `examples/membench.vyrn` gains two rows, 1000 sixteen-byte Strings
spliced from one array into another, source array built inside the sample on
both:

| row | min | median |
|---|---|---|
| splice 1000 Strings, consuming loop | 79.02 µs | 134.95 µs |
| splice 1000 Strings, copying loop | 104.22 µs | 143.01 µs |

25 µs for 1000 copies — 25 ns each, one `malloc` and one `memcpy`, which is what
M1b measured per copy. Compile time did not move: `movecheck` over
`std/vyx.vyrn` goes 1,071 → 1,100 µs (min of 30) and `vyrn check
examples/vyxdemo.vyrn` stays at 60 ms. Rule 2's analysis already ran in 4b; only
the refusal was gated, so turning it on costs nothing.

## Rejected

- **RFC-0088 alone** — keeps the silent-leak fallback as defined behavior;
  every future language feature must remember to extend the analysis or it
  leaks. The census *is* the record of how that goes.
- **Full region inference (MLKit/Tofte-Talpin)** — regions in every type,
  written or inferred; priced earlier and the price is the annotation surface
  Vyrn refuses, plus module-state values have no region.
- **Linear-everything (Austral)** — sound, and the annotation burden lands on
  the 90% case instead of the 10%.
- **Refcounting the aggregates** — runtime cost on the common path, and more
  invisible, which is the wrong direction for the whole of Part II.
