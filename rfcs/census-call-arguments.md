# Census — The Call Argument

- **Status:** measured, then **implemented** — §9 records what landed and the
  real numbers. The recommendation extends a rule that shipped; it does not
  propose a new fact on a signature.
- **Measured at:** `45b3740` (RFC-0096 M3, as landed).
- **Machine:** Windows 11, `clang` 22.1.0, release CLI, native target.
- **Question:** RFC-0096 M3 closed the `@concat` operand class and left two rows
  open — **930** owning-call arguments and **632** allocating String arguments.
  The reason it gave is one sentence: "a callee may RETAIN its argument, so
  freeing after a general call needs the callee's signature at the site and
  cross-module. That is a milestone, not a line." This census asks whether the
  sentence is true.
- **Read first:** RFC-0096 M3 ("as landed", PR #136), RFC-0089 rules 2 and 3,
  RFC-0094 (a builtin is a declaration; `prelude::returns`), `59c8a0c` (a
  borrowed parameter may not be consumed), `movecheck::ownership`
  (`compiler/vyrn-frontend/src/movecheck.rs:205`).
- **Evidence:** a corpus walk over 216 `.vyrn` files (§1); six native programs
  built and measured at N and 4N turns (§5); `vyrn why --memory` and
  `vyrn check` on each control.

---

## The answer, first

**Extend the existing rule. Design nothing.**

The compiler already answers the retention question at a call, for every
argument that has a NAME. It answers it across module boundaries. It answers it
with the callee's body, which the linker has already put in the same program.
A temporary asks the same question, and the answer is already computed.

Four numbers carry the census.

| measurement | result |
|---|---:|
| open call-argument sites in the corpus | **1505** |
| sites where the callee may retain the argument — the residual | **97 (6.4%)** |
| sites the compiler can already prove non-retaining | **775** |
| the same, after ONE seeded row for `print` | **1274 (85%)** |

And one measurement says the same thing without any counting. These two loops
make the same two calls. They differ by a name.

| the loop body | 250,000 turns | 1,000,000 turns |
|---|---:|---:|
| `total = total + width(label(i))` | 14.62 MB | **49.12 MB** |
| `let s = label(i)` then `width(s)` | 4.09 MB | **4.09 MB** |

The second form is steady because `movecheck` proved that `width` does not keep
its argument. The first form leaks 48 bytes a turn because the value has no
binding for that proof to be written against. **The proof is not missing. The
place to write it down is missing.**

---

## 1. How the count was taken

The method is RFC-0096 M3's, extended. Each file is parsed **alone** — no
loader, no linking — which is the convention every corpus measurement in this
repo uses (`own::tests::rfc0089_move_surface_over_the_corpus`,
`movecheck::tests::rfc0089_owning_sites_over_the_corpus`). It gives one number
per source line instead of one per import graph.

The walk visits every `Expr::Call` in every function, test and bench body of
the 216 files under `examples/` and `std/`. At each argument position it asks
two questions:

- **Shape A — an owning call's result.** The argument is a call, and the
  declared return of that call owns heap. "Declared return" is
  `declared::Declared`'s reading: `prelude::returns()` for a builtin, the
  function's own `ret` for a declaration. `own::owns_heap` answers the second
  half.
- **Shape B — an allocating String expression.** The argument is `@str`,
  `@concat`, or a `+` whose type is `String`. This is `own::str_temporary`
  (`own.rs:760`), the predicate M3 shipped, with the String check M3's own doc
  comment demands of every caller.

Types come from a scope stack carried through the walk: parameters take their
declared type, a `let` takes its annotation or `Declared::type_of` of its
initializer, a `for` variable takes the element type. The walk records whether
the site is inside a `while` or `for` body, and whether it is inside a test.

Three global tables are built from the same 216 files, then read after the walk:

| table | where it comes from |
|---|---|
| declared capabilities | every `fn` and every `protocol` method in the corpus |
| variant constructors | every `type X = A \| B` in the corpus, plus `Some`, `Ok`, `Err`, `Success`, `Failure` |
| retention positions | `movecheck::run(program, Want::Lets).retains`, unioned over the files, then closed over the corpus call graph |

The retention table is the one that matters, and it is not this census's
invention. `movecheck` computes it today for every `let` binding
(`movecheck.rs:205-241`). A position `(callee, index)` is in it when the
callee's body puts that parameter somewhere that outlives the call. The census
unions the per-file sets and re-closes the "handed on" edges across files,
because a parameter forwarded into another module's retaining position is
retained too.

The harness is a 320-line `#[ignore]`d test written into
`movecheck.rs`'s own test module, run, and then removed. Nothing in this census
changed a line of the engine, and the numbers below are reproducible from the
definitions above.

---

## 2. The re-derivation

| shape | argument positions | distinct source lines |
|---|---:|---:|
| A — an owning call's result | **1016** | 964 |
| B — an allocating String expression | 1320 | 577 |
| B, already freed by M3's consumer rule | 831 | 93 |
| **B, open** | **489** | 484 |
| **open, both shapes** | **1505** | 1448 |

**The 831 are not open and M3's own text counts them as such.** A `@concat`
whose operand is a `@str` IS a call with an allocating argument, and M3's rule
frees exactly that operand at four sites in each backend. The census removes
them from shape B so that no site is counted twice.

The calibration row is the shape M3 closed: `+` and `@concat` operands that
allocate. This walk counts **2345** of them (2264 typed `String`); M3 counted
2257. Shape A reads 1016 against M3's 930, and open shape B reads 489 against
M3's 632.

**The three counts bracket M3's three and do not reproduce them.** M3's script
is not in the tree, so the small filter that separates the two readings cannot
be recovered — a `+` of two integers whose operand is itself a `+` moves the
first row, and counting lines instead of argument positions moves the other
two. The census states its own definitions above and uses its own numbers
throughout. **Every conclusion below is a proportion, and no proportion moves
by 9%.**

---

## 3. The classification

All three tables read the **1505 open sites**. The loop columns count the sites
inside a `while` or `for` body.

### By callee kind

| kind | A | A in a loop | B | B in a loop | total |
|---|---:|---:|---:|---:|---:|
| a builtin with a seeded row | 456 | 54 | 90 | 24 | **546** |
| **no row and no declaration** | 251 | 1 | 270 | 18 | **521** |
| declared in the same file | 125 | 21 | 29 | 14 | 154 |
| **declared in another module** | 95 | 26 | 34 | 7 | **129** |
| a variant constructor | 59 | 28 | 19 | 6 | 78 |
| a generator (`gen fn`) | 30 | 11 | 47 | 21 | 77 |

Cross-module calls are **129 sites — 8.6%**. M3's sentence puts the cost of the
whole class on them. They are the smallest class but one, and §5 shows that a
named value at one of them is already released today.

### By the parameter's capability

| capability | A | B | total |
|---|---:|---:|---:|
| `read` (the default) | 617 | 177 | **794** |
| no signature visible | 257 | 275 | 532 |
| `consume` | 83 | 18 | 101 |
| a constructor, which has no signature | 59 | 19 | 78 |
| `modify`, `share` | 0 | 0 | 0 |

### By retention verdict

| verdict | A | A in a loop | B | B in a loop | total |
|---|---:|---:|---:|---:|---:|
| **provably non-retaining** | 613 | 62 | 162 | 45 | **775** |
| unknown — no signature | 257 | 1 | 275 | 23 | 532 |
| transferred — the callee owns it (`consume`) | 83 | 48 | 18 | 16 | 101 |
| retained — a constructor keeps it | 59 | 28 | 19 | 6 | 78 |
| retained — the body stores it | 2 | 0 | 15 | 0 | 17 |
| lent — the result points into the argument | 2 | 2 | 0 | 0 | 2 |

**The 532 unknowns are one name.** `print` is 499 of them. The rest are ten
names and 33 sites: `fromJson` 5, `error` 4, `transKey` 3, `warn` 3,
`shelfApp` 2, and one each of `@copy`, `debug`, `info`, `lex`, `listDir`.

`print` has no seeded row because its argument is a union this language does not
spell — a number, a `Bool`, a `String`, or a type with `impl Show`. `@str` has
the same argument and it HAS a row (`prelude.rs:346`): the parameter type is
spelled `Unit` and the row says the type is inert. RFC-0094 built that
convention, and it is documented in `prelude.rs`'s own header. **One row of the
same shape moves 499 sites from "unknown" to `read`.**

---

## 4. Why `read` already means "the callee does not keep it"

This is the census's central question, and the rules answer it. They were
written for other reasons and they answer it anyway.

**Rule 2 — a borrow may not be stored.** A `read` parameter may be observed and
passed on. It may not be put anywhere that outlives the call. The refusal is
live:

```
$ vyrn check ruletwo.vyrn
ruletwo.vyrn:5:0: `s` may not be stored into module state `kept` — it is a `read` parameter
  fix: declare the parameter `s: consume ..` if this function should own it
  fix: `s.copy()` if both sides need a value
```

**Rule 3 — a return is owned.** Returning a borrow is refused, with the two
fixes named. So a `read` parameter cannot leave through the result either.

**`59c8a0c` closed the third exit.** A move into a `consume` parameter is the
third way a value leaves a frame. `movecheck::check_handover` now asks rule 2's
question at a declared call, so a `read` parameter cannot be handed to a
`consume` position.

Three exits, three refusals. **One exit is open, and it is named in that same
commit message: a variant constructor.** `movecheck.rs:2984` records it and does
not refuse it:

> A variant constructor is a literal that reads like a call: the value it builds
> holds the argument and outlives the call, exactly as an array literal does.
> Recorded, not refused.

`note_retention` (`movecheck.rs:1816`) is what records it, into the `retains`
set, and `ownership` reads that set at line 234 with this comment:

> Rule 2 promises a `read` callee does not [retain], and refuses every way of
> breaking that promise except one: a variant constructor... So this asks per
> position, instead of assuming every call may retain — the assumption this
> phase deleted, and the one that left `let s = a + b; takes(s)` leaking.

**That assumption is the one RFC-0096 M3 restated for temporaries.** It was
deleted for named bindings one RFC earlier. The set that replaced it holds **17
positions** over the corpus, and **26** after the census closes it over the call
graph:

```
attr#0 attr#1 badge#0 bookRow#0 cls#0 detailPage#0 el#0 el#1 el#2
itemRow#1 keyed#0 lastIndexOf#0 navLink#1 on#0 on#1 on#2 page#1
pluralRow#0 row#0 row#1 text#0 vyxProcessElemInner#0 vyxShiftIf#0
vyxShiftIf#2 withKey#1 withTitle#1
```

Every one of them is a tree builder in `std/html`, `std/ui` or `std/vyx`. Each
puts its argument into a node that outlives the call. **Twenty-six positions in
32 `std/` modules and 184 examples is what "a callee may retain its argument"
costs in this corpus.**

---

## 5. What the sites leak

Six native programs, peak working set, N and 4N turns. The census way: a
relation, not a number.

| the loop body | 250,000 | 1,000,000 | reading |
|---|---:|---:|---|
| `width(label(i))` — shape A, no name | 14.62 MB | 49.12 MB | **grows** |
| `width("n=" + i.toString())` — shape B, no name | 14.61 MB | 49.12 MB | **grows** |
| `let s = label(i)` then `width(s)` | 4.09 MB | 4.09 MB | steady |
| the same, `width` imported from another module | 4.52 MB | 4.52 MB | **steady** |
| the same, imported, and no name | 14.62 MB | 49.13 MB | grows |
| `let o = wrap(s)`, where `wrap` returns `Some(s)` | 14.62 MB | 48.80 MB | grows — the residual |

The growth is **48.2 bytes a turn**, which is the String header and its buffer.
It agrees with M3's own measurement of the operand class.

Three readings come out of the table.

**1. The name is the whole difference.** Rows 1 and 3 compile the same two
calls. `vyrn why --memory` prints the reason for row 3:

```
line 15    s                reclaimed at block exit — freeing the String buffer
```

It prints nothing at all for row 1. There is no binding, so there is no row, so
the report is silent about all 1505 sites.

**2. The module boundary is not the problem.** Row 4 imports `width` from a
second file. The release still happens, and it is steady at N and 4N. The
linker puts both bodies in one program before either backend runs, so the
callee's body is as visible as a local one. Row 5 is the same import with the
name removed, and it leaks like row 1.

**3. The residual is already a leak, with or without a name.** Row 6 gives the
temporary a name, and the value still leaks — correctly, because `wrap` keeps
it. `vyrn why --memory` says so:

```
line 16    s                NOT reclaimed — it escapes into the call to `wrap` at line 17
line 17    o                NOT reclaimed — it is a value its producer does not own
```

**Loop position is what makes a site expensive.** 231 of the 1505 open sites are
inside a `while` or a `for` — 141 shape A and 90 shape B. The other 1274 leak
one allocation each per program run. The loop sites concentrate:

| file | A in a loop | B in a loop |
|---|---:|---:|
| `std/vyx.vyrn` | 34 | 17 |
| `std/ui.vyrn` | 22 | 20 |
| `std/jsonread.vyrn` | 19 | 2 |
| `std/graphql.vyrn` | 13 | 4 |
| `std/http.vyrn` | 9 | 9 |
| `std/i18n.vyrn` | 10 | 3 |
| `std/contract.vyrn` | 7 | 5 |

These are the generators and the server. A long-lived `vyrn serve` process
walks `std/http` once per request.

---

## 6. The residual

**97 open sites of 1505 — 6.4%.** These are the sites where the callee may keep
the argument, and no rule may ever free them at the call.

| position | sites | what it is |
|---|---:|---|
| `Err#0` | 35 | a sum constructor |
| `text#0` | 12 | `std/html` — the argument becomes a text node |
| `JNum#0` | 9 | a sum constructor |
| `Some#0` | 5 | a sum constructor |
| `JStr#0`, `JArr#0`, `JObj#0`, `Ok#0` | 6 | sum constructors |
| `VNElem#2`, `VNElem#3`, `VNText#0`, `VNFor#3` | 6 | `std/vyx` node constructors |
| `Op*#0`, `Op*#1` | 10 | `std/html` patch-operation constructors |
| `keyed#0`, `el#2`, `attr#1`, `on#2`, `A#1`, `El#1`, `El#2`, `Body#0`, `Text#0`, `Counted#0`, `vyxDir#0`, `uiInspectVyxPage#1` | 14 | builders that store the argument |

**78 of the 97 are variant constructors, and a constructor is not a leak.** The
value it builds holds the argument, and RFC-0092 M3 and RFC-0096 M1 give that
value its own release. So the residual splits again:

| | sites |
|---|---:|
| the callee stores the argument in a value that releases it | 78 |
| the callee stores it in module state or a node — a leak today, and not this rule's | 17 |
| the callee lends part of the argument back through its result | 2 |

**The residual bounds any rule at 97 sites, and 19 of them leak whatever
happens.**

---

## 7. What the fix costs

`own::str_temporary` (`own.rs:760`) is one predicate. Each backend reads it in
one helper — `Gen::free_str_temp` (`codegen/src/lib.rs:2312`) and
`tee_str_temp` (`codegen/src/direct.rs:1843`) — and calls that helper at the
sites where a String is copied out of an operand. It answers "this expression
allocated the String, so the consumer releases it". The extension is the same discipline at a call argument, with one
more question asked first: **does this position keep what it is given?**

The answer already exists. `movecheck::ownership` computes it, `own` reads it
for every `let`, and `vyrn why --memory` prints it in words a user reads.

| what a milestone would close | sites |
|---|---:|
| the rule as written, over positions with a visible capability | **775** |
| plus one seeded row for `print`, in the shape `@str`'s row already has | **+499 → 1274** |
| `consume` positions, which need nothing — the callee owns it | 101 |
| the residual, which must stay open | 97 |
| ten more unresolved names | 33 |

**85% of the open sites, and 15% of them in loop position.**

Three facts the milestone must carry, and each one exists:

1. **The retention answer at a call site.** `retains` is built today only when
   `Want::Lets` asks for it, and it is keyed by `(callee, index)`. A temporary
   has no `let` node to hang the verdict on, so the fact has to reach the
   backends the way `str_temporary` does: one public function both engines ask.
2. **The linked program.** Both backends already lower the linked program. §5
   row 4 proves the release crosses a module boundary today.
3. **The temporary's type.** Freeing needs a release row for the value.
   Shape A takes it from the declared return, which is what RFC-0096 M3 part 1
   gave eleven builtins. Shape B is always a `String`.

**The soundness argument is not new either.** It is the argument that shipped
for `let s = label(i); width(s)`. If the retention set were wrong, that release
would be a double free, and the corpus would crash today. It does not.

Two things stand aside, exactly as they do now: a call inside a `region`, where
the arena owns the buffer, and a `consume` position, where the callee owns it.

---

## 8. Recommendation

**Extend the existing rule. Do not design a signature fact.**

M3's sentence — "freeing after one needs the signature at the site, cross-module"
— is true for 97 sites of 1505, and the compiler already knows which 97. The
other 1408 need nothing that is not already computed, already linked, and
already printed by `vyrn why --memory` for the binding one line away.

The milestone is a rule, not a model:

- **M-a.** Give `print` a seeded row, with an inert `Unit` parameter and
  `Capability::Read`, beside `@str`'s row in `prelude.rs`. It is one row. It
  moves 499 sites, and it removes the largest hole in every pass that asks what
  `print` does with its argument, not only this one.
- **M-b.** Publish the retention question as a function both backends may ask,
  the way `own::str_temporary` is published. It is a read of a set `movecheck`
  already builds.
- **M-c.** Release a call argument at the call, when the argument allocated the
  value and the position does not keep it. Stand aside inside a `region`, at a
  `consume` position, at a constructor, and at a position in `retains`.
- **M-d.** Add the row to `memory.rs`. `exprTemporary` is M3's row; this one is
  `callArgument`, and it must be steady at N and 4N. Negative-test it by making
  the retention question answer "may retain" everywhere and re-measuring, which
  is how M3 tested its own rule.

**Two things this census found and did not close.**

1. **`print` has no seeded row.** 499 of 1505 open sites are unclassifiable for
   that one reason. The row that would fix it has a precedent one line above it
   in the same file.
2. **A function that returns `Some(s)` of a `read` parameter leaks twice.** The
   argument is not released, because it escapes into the constructor; the result
   is not released, because its producer lends. Measured at 48.80 MB over
   1,000,000 turns. It is the constructor hole `59c8a0c` names, it leaks with a
   name today, and it is 78 sites of this corpus.

---

## 9. What landed

Implemented on this branch. **M-a to M-d all shipped**, and the numbers below
are the compiler's own: the harness this census wrote and removed is now a test
(`movecheck::tests::census_call_arguments_over_the_corpus`), so the table is
re-derivable and cannot drift from the rule.

### The measurement flips

`width(label(i))` in a loop, native, peak working set:

| turns | before | after |
|---|---:|---:|
| 250,000 | 14.62 MB | **3.94 MB** |
| 1,000,000 | 49.12 MB | **4.30 MB** |

48.2 bytes a turn, gone. The 4N reading is 0.36 MB above the N one and does not
move with the turn count: it is the allocator's, not the loop's. The memory suite has a `callArgument` row and reads
**21 rows, 21 steady**. Its negative test is M-d's: make the retention question
answer "may retain" everywhere and the row leaks again — 589,824 bytes after 500
calls against 2,162,688 after 2,000.

### The count, and where it differs from the prediction

Two readings, because the two answer different questions.

| | this census | landed, per file | landed, linked |
|---|---:|---:|---:|
| already freed by M3 | 831 | **831** | 467 |
| open | 1505 | **1508** | 3208 |
| released | 1274 (predicted) | **1174** | **1936** |
| transferred (`consume`) | 101 | **101** | 246 |
| retained | 95 | 71 | 412 |
| lent | 2 | 24 | 557 |
| unknown | 33 (after `print`'s row) | 138 | **57** |

**The per-file reading reproduces this census and falls 100 short of its
prediction.** The 831 and the 101 land exactly. The gap is one thing: this
census built its capability table from **all 216 files at once** and then read
it per file, so `trim` — declared in `std/strings` — answered `read` for a file
that never imported it. The rule cannot do that. It reads the program in front
of it, so a cross-module callee in a file parsed ALONE has no signature at all:
`trim` is 43 of the 138 unknowns, and ten more names are the rest.

**The linked reading is the one that ships**, and it is the census's own §5 row
4: the linker puts both bodies in one program before either backend runs.
Linked, `print` is gone from the unknown column and 57 sites remain — `@panicAt`
33 (the desugar of `panic`, which diverges), `fromJson` 10, and eight more. That
is the residual this census predicted at 33, one name larger.

Linking also moves sites the OTHER way, and that is the rule working: retained
climbs 71 → 412 and lent 24 → 557, because the retention set closes over the
whole call graph and `std/html`'s builders become visible to the files that use
them. A per-file reading cannot see that a callee keeps its argument. **The
linked reading is the conservative one.**

### What is released and what is only recorded

Of the 1936 released sites, **1599 are a `String`** and are freed today. The
other 337 are an `Array`, a record, a `Map` or a declared type: `own` records
the row and the backends leave it alone, which is the leak they are today. Both
backends free a String out of a register with the helper RFC-0096 M3 gave them;
every other kind wants the walking release, which needs a PLACE and therefore a
slot to spill a temporary into. That is a milestone, and this time it is one.

### Three findings this census did not have

1. **`blackBox` hands its argument straight back.** Its row answers `T` and its
   parameter is `T`, so the result may BE the argument, and freeing at the call
   is a use-after-free rather than a leak. `examples/membench.vyrn` has the shape
   six times: `blackBox(concatFresh(blackBox(pad()), ..))`. The rule reads it off
   the signature — a row whose return is the same bare type parameter as the
   argument's promises nothing about the result — and `movecheck::arg_verdict`
   answers `Lent`. It costs 7 sites. A row that hands a CONTAINER back (`@push`
   answers `Array<T>` for an `Array<T>` receiver) asks the same question, and no
   container is a `String`, so nothing reaches the emitted set today.
2. **The retaining shape cannot be a steady memory row.** A builder that puts a
   `read` argument into the value it returns LENDS that value, so the caller may
   release neither the argument nor the result: finding 2 above, and it means the
   shape leaks whatever this rule does. It has a three-engine test instead
   (`parity::a_retained_argument_is_not_freed_at_the_call`), because what the
   rule must not do there is free twice, and a double free is not a leak.
3. **A call result fed to a `+` is a class next door and still open.** `"n" +
   label(i)` reaches the operator lowering, not a call argument, so it is in
   neither this census's 1505 nor M3's operand class. It is the same 48 bytes.
