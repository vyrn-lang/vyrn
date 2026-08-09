# RFC-0094 — A Builtin Is a Declaration

- **Status:** **Designed. M0 measured and merged (`rfcs/census-builtins.md`,
  `26469be`). M1 next.** The census refuted a third of the brief that asked for
  it, and this RFC is written from the census rather than from the brief.
- **Depends on:** RFC-0086 (seeded rows, `impl`, "no second list"), RFC-0091 M2
  (`place` projections — `at` is already dispatch), RFC-0092 M5 / PR #118 (the
  bug class this closes), RFC-0007 §v2 (the deferral M3 collects).
- **Principle:** RFC-0086 wrote it for types: *a type declares what it is, the
  compiler looks it up.* This is the same sentence for functions.

---

## The question

The compiler owns 83 names. Beside `checker::RESERVED` sit **ten more
hand-written lists of the same names**, six editor tables, and four dispatch
chains — about 45 places in total, measured. `Checker::call` is 1,894 lines of
`if name == "…"`.

That is not, by itself, an argument. Lists are cheap and a compiler is allowed
to have them. The argument is what the lists **hold**:

> **Eleven builtins carry an ownership or capability fact that no signature
> holds, and two of them carry it nowhere at all.**

`boxStream` and `serveStream` each hand a `Stream` away for good. Neither is in
`RESERVED_SINKS`. Neither is anywhere. Each has exactly one caller in the whole
corpus, and that is the only reason no heap has been corrupted over them.

We know it is the only reason, because the same shape corrupted one three days
ago. `fromArray` moved its argument, said so **in a doc string**, and no rule
read the doc string. The native binary exited `0xC0000374`; the interpreter
printed `1 2 3` by refcounting rather than by having the rule; parity saw
nothing, because all three engines agreed on the wrong answer. PR #118 fixed it
by adding a row to `RESERVED_SINKS` — the right fix for that morning, and the
wrong shape to keep.

**A capability written on a parameter would have made the bug unwritable.**

---

## The precedent, and it is exact

RFC-0086 found six hand-written lists describing types and closed all six. The
mechanism was: seed the built-in rows, let `impl P for T` add rows, and keep no
second list. `Owned` is that mechanism, and `std/slots` — an ordinary Vyrn
container declaring `impl Owned` — then **outperformed the built-in slab it
replaced by 2.0×** (RFC-0090).

The same story has run four times:

| the intrinsic | what replaced it |
|---|---|
| the cell slab (`cell`/`get`/`set`, `Ref<T>`) | `std/slots`, 2.0× faster, 1,714 lines deleted |
| an `rpc` keyword | `std/rpc`, a library — the keyword was built and reverted |
| three hand-rolled JSON emitters | `std/json`, with only the per-type derive left intrinsic |
| built-in stream cursor cells | the slab re-hosted **inside `std/stream`** |

Every one moved the same direction. This RFC does not propose a new direction;
it proposes finishing the one the repo already takes, for functions.

---

## What the census found, and where it said no

**The thesis is half right, 45 to 38**, and saying so is the census working.

| bucket | count | verdict |
|---|---|---|
| module-extern | 31 | M2 takes 16; I/O and logging wait |
| prelude-extern | 14 | **M1, and all the measured value is here** |
| syntax | 18 | not functions — constructors, type names, `match` |
| teaching-hint | 8 | keep; six identifiers buy good migration errors |
| derive-intrinsic | 6 | keep intrinsic; four take a **type name** as an argument |
| seeded-protocol | 4 | M3 |
| delete | 2 | done — `array` and `alen` went in `553390e` |

Three findings changed the design outright.

**There is no grammar gap, and no prelude file.** The brief specified an
embedded source file of `extern fn` signatures, loaded with `include_str!`.
Unnecessary: `project::seeded_rows()` already builds body-less `ast::Function`
values in Rust, with `Capability::Read` and `Capability::Modify` on parameters,
deliberately as AST *because `@slot` is unlexable*. M1 seeds signatures the way
two are seeded today. No grammar, no file, no parse step — and the bare-file
constraint is satisfied for free, because nothing is imported.

`extern fn` could **not** have carried it, which is worth recording: it parses no
generics, its ABI domain admits only scalars, `String` and `Unit`, and it refuses
`consume String` outright. All three restrictions are correct for a JS boundary
and none should be relaxed for this.

**Twelve of the module-extern names already have their bodies in `std/`.** The
compiler's entire residue for `contains`, `slice`, `chars` and the six codecs is
a checker arm and a reserved row; the loader routes the call today. `slice` even
reads its return type out of the declaration already. M2 is mostly deleting the
residue, not writing anything.

**`at` and `push` must be left alone.** They are the seeded-protocol bucket, and
RFC-0091 M2 already made `at` dispatch. The census's instruction is blunt and
correct: the residue *is* the answer.

---

## What a fifth list costs, in this repo's own history

`RESERVED_VIEWS` held `get`. RFC-0090 M4 deleted the `get` builtin and removed
the name from `RESERVED` in the same stroke — but `RESERVED_VIEWS` matches on the
**call**, not on a builtin table. So every user function named `get` handed back
a value that owned nothing, and a `Slots<String>` read through `std/slots` leaked
silently.

**One list was updated and a second was not, and the language got a silent
leak.** That is the argument for one place, in the repo's own words rather than
in principle.

`COMPTIME_FORBIDDEN` has the identical exposure right now: `SPAWN_FORBIDDEN` is
pinned by a subset test and `COMPTIME_FORBIDDEN` is pinned by nothing. And the
direct wasm backend — the only wasm backend since RFC-0077 M5 — has no coverage
assertion at all, where the interpreter has one. Five names live in three engines
and are absent from it. Three are explained by test and bench running on the
interpreter. **`fsyncFile` and `alen` are not.**

---

## The rule

> **A builtin's contract is its signature. The compiler seeds the signature; the
> passes read it. There is no second list.**

Three consequences, and each is a deletion:

- A capability on a parameter (`read`, `modify`, `consume`) is where ownership
  lives. `RESERVED_SINKS`, `RESERVED_VIEWS`, the stream-producer match and
  `mut_array_receiver`'s capability half all say things a parameter says better.
- A return type is where an obligation lives. `unboxStream`'s `MustUse` is
  matched by name today; the type carries it.
- A name the checker refuses is refused **at the declaration**, which is what
  `RESERVED` already guarantees and what must survive. One user `fn at` once
  produced 53 diagnostics, none of them at the declaration.

## What the rule does not reach, stated plainly

- **Effects.** The 29 rows of `SPAWN_FORBIDDEN` and `COMPTIME_FORBIDDEN` are not
  ownership and no signature in this language carries them. An effect annotation
  is a language feature and is not a milestone of this RFC.
- **A type name as an argument.** `schemaOf`, `jsonSchema`, `fromJson` and
  `contractOf` take a declaration, not a value. A syntax for that would buy six
  hover strings.
- **Context-inferred results.** Six names take their result type from `expected`
  rather than from arguments — `None`, `Ok`, `Err`, `unboxStream`, `pullAt`, and
  the array literal. Vyrn solves type parameters from arguments only.
- **Union parameters.** Four names accept every scalar. That is M3's business and
  it is a protocol, not a signature.

---

## Milestones

### M1 — the ownership facts become capabilities

Seed signatures for the 14 prelude-extern names. Delete `RESERVED_SINKS`,
`RESERVED_VIEWS`, the stream-producer match, and `mut_array_receiver`'s
capability half; `movecheck` reads the capability off the signature.

`boxStream` and `serveStream` get an ownership rule for the first time.

Two pins ride along, both named by the census: a coverage assertion for the
direct wasm backend, and `COMPTIME_FORBIDDEN ⊆ RESERVED`.

**Gate.** Three-way parity byte-identical including traps. The memory suite at
15 rows, 15 steady. `genwasm`. RFC-0092's instrument at all zeros. Every RFC-0092
M5 test passing unchanged, same refusals. The three lists gone, proved by
`git grep`. A net line reduction, reported — **and if the count goes up, that is
the milestone failing, not a detail.**

### M2 — the routed names become imports

Sixteen names leave the global namespace: nine string and codec names whose
bodies are already in `std/`, `lineAt`/`colAt`, and the four stream primitives,
which become **non-exported** declarations in `std/stream`.

Measured cost: **40 file-import pairs over 232 files.** `print` (140 files),
`bytes`, `stringFromBytes`, `panic` and `parse` stay global. I/O and logging are
excluded — 14 files and an effect gate are a different argument.

**Gate.** The standard block, plus: a bare file spelling `unboxStream(0)` gets an
ordinary unknown-name error; `RESERVED` shrinks by exactly the migrated names.

### M3 — the union parameter becomes a protocol

`value` first — it has **one** hand-written call site, and closing it closes
RFC-0007 §v2, which deferred "the extensible value set" by name. Then `print` and
`@str` share one `Show`-shaped protocol with seeded impls, so `"\{x}"` and
`x.toString()` work for a user type that declares one. That also dissolves the
blocker the method-builtin move has been stuck behind.

`assertEq` is **not** in M3: it needs an `Eq` protocol the language does not
have. `@join` is task ownership, not rendering. `@concat` is an operator.

**Gate.** The standard block, plus: an example with `impl Show` for a user record
in both `"\{x}"` and `.toString()`, identical on all three engines; wasm sizes
unmoved where no user impl exists; tagged templates unchanged.

**The seeded rendering must route to the existing lowering.** A second formatter
"that formats the same" is how float rendering drifts.

---

## Rejected

- **An embedded prelude source file.** The brief's design. `seeded_rows` already
  does it in AST, and a file would add a parse step, a grammar and a failure mode
  for nothing.
- **Relaxing `extern fn`'s ABI domain** so the prelude could be written in Vyrn.
  It is a JS boundary; its restrictions are right for that job and wrong for
  this one.
- **Declaring the six derive-intrinsics.** Four take a type name. The syntax
  costs more than six hover strings are worth.
- **Putting `at` or `push` on the prelude.** Already dispatch since RFC-0091 M2.
- **Moving `print` behind an import.** 140 files, and `print` is the first line
  of every program anybody writes.
- **An effect annotation for the 29 forbidden rows.** A language feature wearing
  a milestone's clothes.
- **Auto-derived `Show` for records.** A derive, with i18n consequences, and a
  separate decision.

---

## The recommendation

**Build M1. Hold M2 and M3 until it lands.**

1. **It closes a bug class that has already fired.** Eleven facts live in prose
   or a side table; two live nowhere. PR #118 is what that costs when the caller
   count rises above one.
2. **The census says all the measured value is in M1**, and M1 is the milestone
   that needs no grammar, no corpus migration and no import.
3. **The repo has run this experiment four times** and the declaration won every
   time, twice on performance.
4. **The gate can refuse it.** If seeding signatures does not reduce compiler
   lines, the mechanism is not paying for itself and M2 and M3 should not follow.
   RFC-0091 M4 was refused by its own measurement, and that was the system
   working.
