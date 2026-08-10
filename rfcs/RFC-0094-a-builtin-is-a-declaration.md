# RFC-0094 — A Builtin Is a Declaration

- **Status:** **Complete. M1, M2 and M3 landed.** M0 measured and merged
  (`rfcs/census-builtins.md`, `26469be`). The census refuted a third of the brief
  that asked for it, and this RFC is written from the census rather than from the
  brief; M1 then refuted two rows of the census, M2 refuted one of the census's
  counts, and M3 refuted its own brief about which side of the dispatch the seed
  goes on. See the three "as landed" sections. RFC-0007 §v2 is closed by M3.
  **The residue section closed a four-row list and left an eighteen-name class
  behind it**, which RFC-0096 M3 audited and closed — including one row of this
  RFC's own that had drifted from the checker's arm and segfaulted. See "The
  residue was four rows and the class was eighteen".
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

### M2 — the routed names become imports — **landed; see "M2 as landed"**

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

### M3 — the union parameter becomes a protocol — **landed; see "M3 as landed"**

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

**A third finding, and it is closed.** The word `consume` meant two different
rules in this compiler. A user `fn take(xs: consume Array<Int64>)` accepted a
`read` parameter's array; `fromArray` refused the same shape, because the builtin
path goes through rule 1 and the declared path did not. M1 put both readings on
one page for the first time and called the difference an inconsistency.

It was not an inconsistency. **The builtin path was right and the declared path
was unsound**, and the repro is PR #118's signature one path over: `vyrn check`
said `ok`, `vyrn run` printed `6`, and the native binary printed nothing and
exited `0xC0000374`. PR #125 closed it. `movecheck::check_handover` asks rule 2's
question at the third exit — a borrowed root may not be consumed — with the move
left where it was, so a capability keeps the wording it has always had. The
borrow now travels to every spelling of the argument (`let n = ns[0]` and
`match o { Some(v) => .. }` used to bind a bare projection and lose the root's
own borrow), and `spawn` asks it too.

**What it found in the corpus, and it was five sites.** `std/graphql`'s
`gqlProjectCore` handed a `read` parameter's `Json` to `gqlOk`, which takes it;
`std/vyx`'s `vyxGroupNodes` handed each element of a `read` array to
`vyxProcessElem`, which takes it, in four places. Both are latent double frees
that parity and `genwasm` never saw, because no corpus program frees a leaf twice
in a way a comparison can print. `gqlOk` now gets a `copyJson`, and the two `vyx`
functions declare the `consume` they were already exercising.

**Where the rule stops is deliberate.** A projection of a place the frame itself
owns — `take(b.xs)`, or a payload binder over an owned scrutinee — is recorded
and not refused, which is where the sibling exit stops too: a variant constructor
holds what it is given and the `constructs` arm records rather than refuses it
(RFC-0092 M2). `std/html`'s `keyed` drains an owned node through both exits in
one expression, and its own doc says why a `.copy()` there would be wrong. The
two exits move together or not at all.

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
- **The two meanings of `consume`** — closed by PR #125, above.

---

## M2 as landed

**Eleven names left `RESERVED`, not sixteen.** Each one had its body in `std/`
already, so the compiler's whole residue was a checker arm, a `RESERVED` row and
a route:

| the name | its module now |
|---|---|
| `contains` `startsWith` `endsWith` `slice` | `std/strpred` |
| `chars` | `std/text` |
| `hexEncode` `hexDecode` `base64Encode` `base64Decode` `urlEncode` `urlDecode` | `std/codecs` |

`RESERVED` fell from 83 rows to 72. `RT_MODULES` fell from six modules to four
and from twelve routes to **one**: `@charCount`, which stays because
`s.charCount()` is method-only and no import can bring an `@`-name into scope.
`std/strpred` and `std/codecs` are no longer injected at all — they are ordinary
modules a program imports.

The `V` suffix went with them. It existed for exactly one reason — the name was
reserved, so the module could not declare it — so `containsV` is `contains`,
`sliceV` is `slice`, `charsV` is `chars`, and the six `*V` codecs carry the names
their callers write. `byteLengthV` keeps its suffix: `byteLength` is a field.

### The line gate

Measured the way M1 measured, over every `.rs` file under `compiler/`, counting
lines that are neither blank nor a `//` comment, against `main` at `d4e15ab`:

| file | production | test |
|---|---|---|
| `checker.rs` | **−77** | +12 |
| `loader.rs` | **−24** | +9 |
| `symbols.rs` | **−8** | 0 |
| `codegen/lib.rs` | 0 | −13 |
| `interp.rs` | 0 | +9 |
| the five CLI test files and `tests/primitives.rs` | — | +29 |
| **net** | **−109** | **+46** |

**The bar the RFC moved here is met.** M1 measured +149 and said the deletion the
census counted was M2's; it was. The 109 lines are the dispatch chains: two
checker arms for the codecs, one for the three predicates, one for `slice`
(including the return type it read out of the link), and the `chars` half of the
`bytes | chars` arm — plus two `RT_MODULES` rows and eleven entries each in
`RESERVED` and `MACRO_BUILTINS`.

The +46 test lines are not overhead the mechanism imposes. 29 of them are import
lines and fixtures in test files, and 11 are one new test
(`a_name_returned_by_m2_may_be_declared`) that proves the deletion from the
surface: `fn slice(..)` compiles now.

### What the census got wrong, measured

**The import cost was 28 file-import pairs, not 16.** Q5 counted 7 files for
`std/strpred` and this milestone touched **23**. The census counted the FREE
spelling — `contains(s, n)` — and the corpus almost never writes it. A method
call is universal sugar for a free call (`parser.rs`: "`recv.name(args)` is sugar
for the free call `name(recv, args)`"), so `s.contains(n)` is a call to
`contains` and needs the same import. Nothing else changed: the receiver syntax
still works, unchanged, on an imported function.

Against the census's own totals: 16 predicted for these three groups, 28
measured. The whole bucket's "40 file-import pairs" is therefore an under-count
of the same kind, and any later milestone that reads it should count method calls
first.

**Two generators emit a moved name.** `std/rpc`'s router and `std/connect`'s ask
`req.path.startsWith(prefix)` in emitted source, and `std/rpc` does it twice
(`rpcServer` and `rpc`). Generated source is source: each generator emits its own
`import { startsWith } from "std/strpred"` line now. Nothing else in the corpus
emitted a moved name, and no `vyrn"…"` code quote spells one.

### Three names refused, each for a measured reason

**`lineAt` and `colAt` do not move.** RFC-0078 M4c already refused them and
`std/text.vyrn` carries the number: `lineAt` is a builtin *because* the obvious
loop is O(off) and a scanner asks once per node, "which cost `std/vyx` 122 ms of
a 291 ms page compile". The interpreter memoizes a line-start table per buffer
and a Vyrn library cannot — a generator may not touch module state. The Vyrn
bodies (`lineAtV`, `colAtV`) stay beside the builtins as the live oracle they
already were. This is a decision about that cache, and this milestone does not
change it.

**`serveStream` does not move.** The census called the four stream primitives
internal plumbing. `serveStream` is not: it is the handoff to the serving HOST,
and `compiler/vyrn-cli/tests/serve.rs` spells it **below** `std/http` on purpose
— "precisely so a second adapter would have nothing to rewrite". M3b then proved
that by rewriting nothing. Scoping the name to `std/http` would delete exactly
the property those conformance tests exist to hold.

**`boxStream`, `unboxStream` and `pullAt` do not move, and this is the milestone
refusing a gate item.** The brief asked for them to become non-exported
declarations in `std/stream`, and for a bare file spelling `unboxStream(0)` to
get an ordinary unknown-name error. Three findings stopped it:

1. **They have no Vyrn body.** They are the type-erasure primitives
   `std/stream`'s cursor slab is built on, so they cannot be a declaration in a
   `.vyrn` file the way the eleven above can. Scoping them needs a NEW mechanism
   — a module owner on a prelude row, read by a new pass — that deletes zero
   lines and adds about thirty-five. That is the shape M1's gate refused, applied
   a second time.
2. **The safety argument is already spent.** M1 measured that `Stream<T>` is
   linear and the must-use walk refuses a second hand-over whatever the callee is
   named. The census's claim that these carry their fact nowhere was wrong, and
   M1 wrote that down. What is left is a namespace argument, not a soundness one.
3. **The backend's own pins spell all three on an unlinked source.**
   `codegen/lib.rs` builds a lazy combinator (`LMAP`) and a
   `pullAt(24)`-traps-on-a-bare-address test through `check(src)`, which does not
   run the loader. A loader-level scope leaves those pins standing and a
   checker-level one deletes them, so the mechanism would be honest in one engine
   and not in the other.

The three names stay reserved and stay global. Moving them is a separate change
with its own measurement, and it should be taken together with `close`,
`fromArray` and `fromStep` — the rest of the stream surface — rather than alone.

### The seeded rows

No migrated name has a seeded row, so nothing had to be re-keyed. `prelude.rs` is
unchanged: its eighteen rows cover `at`/`atSet`, the views, the control names, the
array methods and the six stream names, and every one of those is still reserved.
`prelude::every_seeded_name_is_reserved_or_unspellable` therefore still holds,
and it is what would have caught the mistake.

### The diagnostics

A caller who writes `contains(s, "x")` without the import is told where it lives:

```
line 1: `contains` is `std/strpred`'s — add `import { contains } from "std/strpred"`
```

`checker::MOVED_TO_STD` is the one table behind that, read at exactly one place —
the unknown-name fallthrough of `Checker::call` — so a name that resolves never
consults it. It is the shape the six `was removed` hints already have.
`every_moved_name_is_gone_from_reserved` pins the one direction that can rot: a
name in both tables would send a reader to an import `RESERVED` forbids writing.

Three older diagnostics improved by deletion. `slice` used to refuse with "its
module is not in the link — a std root is needed to call it", which named a
condition a reader could not act on; it names an import now. The other ten used
to check clean on a bare source and refuse in the emitter by an internal
`codecs$hexEncodeV` spelling; they refuse at the check, by their own name, with a
line number.

A caller who imports the module and then misspells an argument gets the
declaration's diagnostic rather than the arm's. That is a real change in wording
— "expects 2 argument(s), got 1" where the arm said "`contains` takes 2
arguments, got 1" — and it is the trade the RFC states: one refusal, from the
signature, for every function in the language.

### The gate

- Three-way parity byte-identical including traps: **35 tests, green**
  (`--release --test parity -- --ignored --test-threads=1`, with `VYRN_WASMTIME`,
  `WASI_SYSROOT` and `WASI_BUILTINS` set — one parity fixture needed the import).
- Workspace `cargo test --workspace --no-fail-fast`: **52 test binaries, green**.
  `vyrn-lsp` separately: **69 green, 2 ignored**.
- Memory suite: **15 rows, 15 steady**.
- `genwasm` (`--features wasm-gen`): **11 green, 1 ignored**.
- RFC-0092's instrument: `stores: 0`, `elem-store: 0`, `elem-return: 0`,
  `returns: 0`, `total: 0` over 212 files, all 212 linked.
- A bare file spelling a moved name gets an ordinary refusal naming the module
  (`a_moved_builtin_names_the_module_it_moved_to`,
  `a_moved_builtin_without_a_std_root_names_its_module`).
- `RESERVED` shrank by exactly eleven, and `every_moved_name_is_gone_from_reserved`
  is the check.
- `cargo fmt --check`, `vyrn fmt --check` over 212 corpus files, and
  `vyrn doc --std --verify`: all clean.
- Every stream example and `std/stream`'s own tests: **unchanged**, because no
  stream name moved.

### Should M3 follow?

**Yes, and the gate is the reason.** M2 was asked to show the mechanism pays for
itself in lines, and it did: −109 production, against M1's +149, for a net of −40
over the two milestones with a closed bug class in between.

Two cautions the numbers put on it.

**M3 will not repay lines the way M2 did.** M2 deleted arms whose bodies already
existed elsewhere. `print`, `@str` and `value` have no `std/` body waiting for
them; M3 writes a protocol and seeds impls, which is closer to M1's shape than to
M2's. The RFC should say so before M3 starts rather than after, and the honest
bar for M3 is the one its own milestone already states — a user `impl Show`
working in both `"\{x}"` and `.toString()`, identical on three engines — not a
line count.

**Do `value` first, as written.** It has one hand-written call site, it closes
RFC-0007 §v2 by name, and it is the only one of the three whose blast radius is
smaller than `print`'s 140 files.

---

## M3 as landed

`protocol Show { fn show(self) -> String }` is a protocol the compiler knows by
**name only** — the same bootstrap answer `Owned`, `Copy` and `Fallible` give,
for the same reason: `vyrn run` on a bare file has no resolver, so `print` may
not depend on a module lookup. `examples/show.vyrn` declares the protocol
itself, and nothing was added to `std/`.

`print`, `@str` and `value` now dispatch to `impl Show for T` where they used to
refuse. One declaration serves all three, because `"\{x}"` desugars to `@str`
and `x.toString()` desugars to `@str`.

### The brief was wrong about which side the seed goes on

The brief asked for "seeded impls for every type `@str` renders today", with a
declared `impl Show for T` winning over the seed, exactly as a declared
`impl Owned` wins over the release seed. **That is the one thing M3 did not
build, and two measurements say it must not.**

**`examples/protocol.vyrn` would stop terminating.** It declares
`impl Show for Int64` and the body is `self.toString()`. Under "the declaration
always wins", `self.toString()` is a call to the function it sits in. That file
compiles today and is in the parity corpus.

**A replaceable row would be a second float formatter.** RFC-0081 M2 spent a
milestone making `std/num`'s `f64Str` the ONE implementation that renders a
`Float64`, in three engines, because parity compares bytes. A seeded row a
program could replace puts a second answer back in reach of the path that was
hardest to make agree.

So the rule M3 shipped is one sentence and it is additive:

> **A scalar renders by the language's lowering. A type the language cannot
> render asks its declaration.**

Nothing that compiles today prints anything different. The dispatch reaches
exactly the types the three renderers used to refuse. `examples/show.vyrn`
declares `impl Show for Int64` on purpose and asserts the consequence: `n.show()`
is `the number 7` and `print(n)` is `7`.

This is a real difference from `Copy` and `Owned` and it is worth naming.
Duplicating and releasing have no built-in answer for a record — the compiler
derives one, and a type may say the derivation is wrong. Rendering a number is
not a derivation; it is a decision the language already made, in three engines,
byte for byte.

### `value` first, and it fits

The milestone was ordered `value` first with a stop point after it. `value` has
one hand-written call site and the parser emits it for every hole of every
tagged template. It fit with no argument: the checker types the `show` call, and
the box is the `StrVal` the rendering produces.

**The `Value` enum did not change.** RFC-0007 §v2 predicted `Value` would be
"replaced by a protocol bound", and that shape is not available: Vyrn
monomorphizes and has no function pointers (RFC-0037), so `Array<Value>` cannot
hold one element per declared type. Rendering at the box is what a language
without existentials can do, and it is what §v2 asked for — a user type in a
hole. The safety property is unchanged and is arguably clearer: a tag receives
data, and a rendered String is data.

**RFC-0007 §v2 is closed**, and RFC-0007's status line, its §v2 section and its
Q5 now say so.

### `<T: Show>` renders, and the checker had to defer

`fn label<T: Show>(x: T) -> String { return "[\{x}]" }` works. The checker
cannot select the impl there — the receiver is `Type::Param`, not `Type::Named`
— so it agrees the bound exists and defers, which is the answer `x.show()`
already gives inside the same generic (RFC-0002 §5).

The backends then had to be told to substitute BEFORE taking the type key. A
specialization still spells the parameter `T` in the AST, and `T` names no impl.
This cost one line in each compiling backend and was caught by the native column
of a two-line test program, not by review.

An **unbounded** `<T>` is refused, with the refusal it always had.

### `assertEq` is not here, as stated

It compares rather than renders. Comparing needs an `Eq` protocol the language
does not have, and inventing one to finish a bucket is the shape M1's gate
refused. `@join` is task ownership. `@concat` is an operator on String, and no
operator-protocol RFC exists. `at`, `push`, `@has`, `@remove` and `@keys` have
been dispatch since RFC-0091 M2.

No record derives `Show`. That is a derive, it has i18n consequences, and it is
a separate decision.

### The numbers

Measured the way M1 and M2 measured, over every `.rs` file under `compiler/`,
counting lines that are neither blank nor a `//` comment, against `main` at
`fcc24a8`:

| file | production | test |
|---|---|---|
| `types.rs` | +21 | 0 |
| `checker.rs` | +43 | +68 |
| `interp.rs` | +19 | +19 |
| `codegen/lib.rs` | +23 | 0 |
| `codegen/direct.rs` | +25 | 0 |
| **net** | **+131** | **+87** |

**The deleted type-switch lines, per engine: zero. In every engine.** That is
the number the gate asked for and it is the honest one. M3 deletes no type
switch because the type switch IS the seed: the scalar arms of `print`, `@str`
and `value` are the rendering a declaration overrides, and the section above is
why they stay. What M3 adds is one dispatch around each — a helper and a guard
per engine, five in total.

**So M3 does not pay in lines and was not asked to.** M2 carried that bar and
passed it at −109, and M2's own report said M3 would not repay the same way
because `print`, `@str` and `value` have no `std/` body waiting for them. Over
the three milestones the compiler is +171 production lines, and what it bought
is one closed bug class (M1), eleven routed names that are now ordinary imports
(M2), and a capability that was impossible at any price before this milestone
(M3).

### The gate

- **A user type renders.** `examples/show.vyrn` — a record with `impl Show`,
  used in `"\{p}"`, `p.toString()`, `print(p)`, a tagged template's hole, and a
  `<T: Show>` generic at two instances. Byte-identical on interpreter, native
  and wasm.
- Three-way parity byte-identical including traps: **35 tests, green**
  (`--release --test parity -- --ignored --test-threads=1`, with
  `VYRN_WASMTIME`, `WASI_SYSROOT` and `WASI_BUILTINS` set).
- Workspace `cargo test --workspace --no-fail-fast`: **52 test binaries, green**.
  `vyrn-lsp` separately: **69 green, 2 ignored**.
- Memory suite: **15 rows, 15 steady**.
- `genwasm` (`--features wasm-gen`): **11 green, 1 ignored**.
- RFC-0092's instrument: `stores: 0`, `returns: 0`, `elem-store: 0`,
  `elem-return: 0`, `total: 0` over 213 files, all 213 linked.
- **Wasm sizes unmoved where no user impl exists** — and byte-identical, not
  merely equal: `domdemo.wasm` 28,445, `fib.wasm` 1,522, `mapdemo.wasm` 37,794,
  `vlog.wasm` 48,729, each hashing the same as `fcc24a8`'s. Monomorphization
  costs nothing to a program that declares nothing.
- Tagged templates unchanged: the parser still emits `value` for every hole of
  every template, and `examples/tagged.vyrn` and `examples/templates.vyrn` are
  untouched.
- `cargo fmt --check`, `vyrn fmt --check` over the corpus (`corpus_fmt`, 4
  tests), and `vyrn doc --std --verify` (33 files): all clean.

### The diagnostics

A type with no `impl Show` is refused as before, plus the sentence that says what
to write:

```
line 3: print needs a number, Bool, or String, found { x: Int64 } — say how it
renders with `impl Show for P`
```

The hint names the type the way a reader can WRITE it. A type the loader
prefixed with its module has no spelling at the call site, so it gets no hint
rather than an unspellable one — PR #120's lesson, applied here.

The protocol is an ordinary declaration, so a program may declare
`fn show(self) -> Int64`. The three renderers refuse it by name rather than
handing an `Int64` to a String path.

### What M3 did not close

- **The method-builtin full move is still blocked, and now by a decision rather
  than by a missing mechanism.** `toString` is still intercepted into `@str` by
  `parser::METHOD_BUILTINS`, because `@str` is still the lowering for every
  scalar. Taking `toString` out of that table means seeding `impl Show for Int64`
  and the rest, which is precisely what the two measurements above refuse. The
  mechanism it was waiting for exists now; the reason not to use it on scalars is
  new information.
- **`declared::builtin_returns` is still 4 rows.** M1 said merging it needs M3.
  It needs the scalar half of `@str` to be a row, and that half is staying.
  (Closed later — see "The residue, closed" below. The second sentence is true
  and the conclusion did not follow.)
- **The editor gained nothing**, as in M1. `.show()` completion already worked
  through `method_impls`; hover over `print` still comes from `MACRO_BUILTINS`.
- **`schemaOf` and `toJson` do not see `Show`**, deliberately. Rendering and
  serialization are different questions and the tables stay apart.
- **Effects.** 29 rows, unchanged, in all three milestones.

---

## The residue, closed

M1 left `declared::builtin_returns` open and M3 said merging it "needs the
scalar half of `@str` to be a row, and that half is staying". Both halves of
that sentence are true and the conclusion does not follow. A prelude row carries
a `ret` already, and `@str`'s scalar rendering is a PARAMETER fact rather than a
return fact. So the four rows became three new signatures — `@concat`, `@str`
and `@keys` — beside the `@push` row M1 wrote, and `Declared::new` reads
`prelude::returns()`. **`builtin_returns` is deleted.**

`@str` spells its parameter `Unit` and says on its own row that the parameter is
inert: it takes a number, a `Bool`, a `String` or a type with `impl Show`, and
this language cannot spell that union. No rule reads it, because arity and
parameter types stay in the checker's hand-written arms — M1's own decision, for
M1's own reason. Two kinds of row are held back from the reading, and each is
inert for a stated reason: a **lending** row cannot name its result, because one
`at` pair serves every container; and a row returning a bare type parameter
names a type the program never wrote. The two lists had already drifted —
`builtin_returns` said `@push` returns `Array<Unit>` and the row says
`Array<T>` — which is the drift the principle predicts, and the reason to keep
one answer.

`join` got its row in the same change. RFC-0095 M1 made `t.join()` consume the
task, which is a `consume` receiver on a signature; it lived instead as a
property of the must-use WALK, where every mention of a linear binding is a
disposal. So `join` consumed by being written, and no line said so. `@join` now
declares `consume Task<T> -> T`, and `movecheck::sinks` reads it. **There was no
special case to delete.** Rule 1 stands aside for the reason it stands aside for
`close`, `boxStream` and `serveStream`: the obligation on the linear TYPE
refuses a second join first, with better words. Two parts of `join`'s contract a
row cannot carry stay hand-written, and both are about a NAME rather than a
signature — which producer pairs with it, since `spawn` is a keyword and not a
callable name, and the wording of the disposal menu.

**What it cost**, counted the way M1 and M2 counted — lines that are neither
blank nor a `//` comment: `declared.rs` **−9** production, `prelude.rs` **+32**
production and **+21** test. So the fold does not pay in lines, and it was not
expected to: four rows in a list become four signatures plus the reading that
holds two kinds of row back. What it buys is one deleted fact fork and the drift
it was already carrying. **No engine changed.** `checker.rs` and both backends
are byte-identical in this change, so no builtin refusal moved a word.

### The residue was four rows and the class was eighteen — closed by RFC-0096 M3

This section folded the four rows the second list held. **It did not ask which
builtins had no row at all**, and that is where the class actually lived: a call
with no row has no type the declared reading can name, so `own` leaves the
binding alone and the value leaks. RFC-0096 M2 met one — `let s = toJson(x)` —
by building a fixture, and RFC-0096 M3 audited the whole of `checker::RESERVED`.

**Eleven names allocate a result this language can spell and had no row.** They
have one now: `toJson`, `jsonSchema`, `schemaOf`, `args`, `readLine`,
`readFile`, `readFileBytes`, `writeFile`, `renameFile`, `fsyncFile`, and
`stringFromBytes`. Seven more allocate and stay held back, each for a reason
about the TYPE rather than about the fact — the audit table is in RFC-0096 §M3.

**The last one is the one this RFC has to record.** M1 wrote `stringFromBytes`
as returning `String`; the checker's arm answers `Result<String, String>`. Two
spellings of one fact, drifted, **in the milestone that removed the second
lists** — the same failure this RFC exists to prevent, one level up. It cost a
SEGFAULT: `let s = stringFromBytes(b)` unannotated released the aggregate's tag
word as a String buffer, and `vyrn why --memory` reported it as reclaimed.

The lesson is the one the census already taught, sharpened. A row and a checker
arm are still two spellings of one fact, and folding the second LIST did not
fold them. `every_allocating_builtin_answers_its_return_type` asserts the eleven
return types against the arms; nothing yet derives one from the other.
