# RFC-0094 — A Builtin Is a Declaration

- **Status:** **M1 landed. M2 and M3 are on hold — M1's line gate failed, and
  the recommendation below says what that means.** M0 measured and merged
  (`rfcs/census-builtins.md`, `26469be`). The census refuted a third of the brief
  that asked for it, and this RFC is written from the census rather than from the
  brief; M1 then refuted two rows of the census. See "M1 as landed".
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
> holds.**

The census said two of them — `boxStream` and `serveStream` — carry it nowhere
at all, and this paragraph said so. **M1 measured it and both were wrong.** A
`Stream<T>` is linear, and the must-use walk counts every mention of a stream
binding as a disposal, so a second hand-over is refused whatever the callee is
named. RFC-0075's linearity was protecting those two, not their caller count.
The census read the side tables and did not ask the type; "M1 as landed" below
carries the measurement. The eleven still hold their fact in a place no
signature reads, and that is the argument.

The cost of that arrangement is not hypothetical. `fromArray` moved its
argument, said so **in a doc string**, and no rule
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

### M1 — the ownership facts become capabilities — **landed; see "M1 as landed"**

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

**And the line gate moves here, because M1 proved it was written at the wrong
milestone.** M1's gate demanded a net reduction and measured **+149**. The
prediction was not wrong about the compiler; it was wrong about which milestone
pays. M1 replaced scattered `if name ==` checks with one documented table, and a
table that states a fact is longer than a check that assumes it. **The lines the
census counted are the dispatch chains, and M2 is what deletes them.** So M2
carries the bar M1 could not: a net reduction, reported, and if the count rises
again the mechanism is not paying for itself and M3 does not follow.

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

**The gate did refuse it, on point 4.** See "M1 as landed" below for the number
and for what it does and does not settle.

---

## M1 as landed

`compiler/vyrn-frontend/src/prelude.rs` holds **18 seeded rows**. Each is an
`ast::Function` built in Rust, with a capability on every parameter, a declared
return type, and — for the three that lend — a body that yields the place the
result names. A row is keyed by the name the **call site** carries, because that
is what every pass matches on.

### What held

The mechanism works exactly as Q2 said it would. No grammar changed. No file is
embedded and nothing is parsed. Nothing is imported, so a bare file still runs.
Four hand lists became four readings of one row:

| deleted | replaced by |
|---|---|
| `movecheck::RESERVED_VIEWS` (2 rows) | a body that yields `@slot` of a parameter |
| `movecheck::RESERVED_SINKS` (3 rows) | `Capability::Consume` on the parameter |
| the three-name producer `match` | the declared return type, read through `Owned` |
| `mut_array_receiver`'s `mut` clause | `Capability::Modify` on parameter 0 |
| `project::seeded_rows` (58 lines) | the same table — `at` and `atSet` moved into it |

The fifth deletion is the one the RFC did not plan. `project::seeded_rows` built
`at` and `atSet` for RFC-0091 M2, in Rust, for the same reason: `@slot` does not
lex. Two tables of seeded declarations is the shape this RFC exists to remove, so
the two rows moved and `project::seeded` became a lookup.

**The diagnostics did not move.** 32 builtin refusals — wrong arity, wrong type,
a `pop` on a binding without `mut`, a moved array read again, a stream disposed
twice — were run under `main` and under this branch and compared byte for byte.
**Zero differences.**

### What the census got wrong

**`boxStream` and `serveStream` do not carry their fact nowhere.** Q1 said they
do, and the RFC's opening paragraph repeats it. Both take a `Stream<T>`, and a
`Stream<T>` is linear: the must-use walk counts *every* mention of a stream
binding as a disposal, so a second hand-over is refused whatever the name is.
Measured on `main`, before any change:

```
fn twice(s: Stream<Int64>) -> Int64 { let a = boxStream(s) let b = boxStream(s) … }
→ `s` is a `Stream<Int64>` and is disposed more than once
```

The census read the side tables and did not ask the type. One caller is not what
was protecting these two; RFC-0075's linearity was.

**That correction changed the design.** A `consume` parameter whose type already
carries a disposal obligation must **not** also go through rule 1. Two rules over
one value refuse the same program twice with the worse words: rule 1's menu
offers `.copy()`, and a stream has no answer for that. So `movecheck::sinks`
reads `consume` off the row and stands aside where `linear_kind` answers. That is
one sentence, at type level, with no name in it — not a special case for three
names. The rows still say `consume`, which is the point: the fact is written
where hover, the LSP and every later rule can read it.

**A third finding, and it is open.** The word `consume` means two different rules
in this compiler. A user `fn take(xs: consume Array<Int64>)` accepts a `read`
parameter's array today; `fromArray` refuses the same shape, because the builtin
path goes through rule 1 and the declared path does not. M1 put both readings on
one page for the first time. It did not unify them, and unifying them is a
language change, not a milestone of this RFC.

### The two pins

**The direct wasm backend now has a coverage assertion**
(`the_direct_backend_carries_the_census_too`). It compares `CENSUS` against
`direct.rs` and allows four absences, each with a written reason. Three are the
test and bench paths, which run on the interpreter. The fourth is `fsyncFile`,
and the census asked why:

> **`fsyncFile` has no caller.** Zero in `std/`, zero in `examples/` — one doc
> mention in `std/storage.vyrn` and one interpreter unit test. No parity program
> ever asked the wasm column for it, so the absence could not be seen. It is the
> `alen` shape one step short of it: `alen` had a replacement and was deleted,
> `fsyncFile` has none and is a gap. It fails loudly rather than silently —
> `vyrn build --target wasm` prints `error: direct backend: no lowering for the
> call \`fsyncFile\``.

**`COMPTIME_FORBIDDEN ⊆ RESERVED` is pinned** beside the `SPAWN_FORBIDDEN` test
that already existed. Both pins fail when their row is reverted; both were
checked that way.

### The numbers

Measured over every `.rs` file under `compiler/`, counting lines that are neither
blank nor a `//` comment, against `main` at `2af7fa0`:

| file | production | test |
|---|---|---|
| `prelude.rs` (new) | **+165** | +48 |
| `project.rs` | **−58** | 0 |
| `movecheck.rs` | −3 | +29 |
| `checker.rs` | +2 | +10 |
| `lib.rs` | +1 | 0 |
| `tests/primitives.rs` | — | +42 |
| **net** | **+149** | +129 |

**The line gate failed, and the RFC says what that means.** The lists M1 deletes
are five rows of data. The deletion the census counted — "the per-builtin
arity/type checks in four engines", roughly 260 lines for the routed names alone
— is M2's and M3's, and M1 was never going to reach it. So the honest reading is
not "the mechanism is wrong" but "M1 alone does not pay for itself in lines". It
pays in one closed bug class and in eleven facts a reader can now find in one
file.

Everything else in the gate passed: three-way parity byte-identical including
traps (35 tests), the workspace green over 52 test binaries, `vyrn-lsp` green
(75), the memory suite at 15 rows and 15 steady, `genwasm` green, RFC-0092's
instrument at `total: 0`, `corpus_fmt` and `cargo fmt --check` clean, and every
RFC-0092 M5 test passing unchanged with the same refusals.

### What M1 did not close

- **`Checker::call` is still 1,894 lines.** Arity and parameter-type refusals
  stay hand-written on purpose: they read better than a generic signature check,
  and the 32-case diff is what proves it.
- **`declared::builtin_returns` is still 4 rows.** Three of them (`@concat`,
  `@str`, `@keys`) are M3's names, and its types are deliberately erased where
  the prelude's are not. Merging it needs M3.
- **The editor gained nothing.** The census says a declaration is what the LSP
  already knows how to serve, and that is true of a *declared* function. The
  seeded rows are not in `Program`, so `symbols.rs` does not see them. Serving
  hover from the prelude is a separate change and is worth its own measurement.
- **Effects.** 29 rows, unchanged, as stated.
- **The two meanings of `consume`**, above.
