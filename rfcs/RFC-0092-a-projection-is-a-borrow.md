# RFC-0092 — A Projection Is a Borrow

- **Status:** **COMPLETE. M0 measured; M1, M2, M3 and M4 landed.**
  The rule is enforced, the corpus is migrated, census **U4 is closed**,
  **every aggregate releases its places** — so RFC-0089 rule 4 is whole and that
  RFC's status line says so — and **a container carries its element's must-use
  obligation**, which closes RFC-0086 M3's storage hole. See "M1 as landed",
  "M2 as landed", "M3 as landed" and "M4 as landed". Supersedes nothing. Closes
  named gaps in RFC-0087 (§3, §14's remainder, U4), RFC-0089 rule 4 and
  RFC-0086 M3.
- **Depends on:** RFC-0089 (rules 1 to 4, all landed), RFC-0086 M1 and M3
  (`Owned`, `MustUse`), RFC-0091 M1 (`Copy` for a self-referring type)
- **Principle:** RFC-0089's own thesis, applied one level down. *Stop inferring
  ownership. Define it.* A place owns its contents. Refuse the programs that say
  otherwise.

---

## The question

Three items sat open in three documents. Each phase found its own and set it
aside with a reason.

1. **A record, a user enum and a fixed array release nothing** (RFC-0087's open
   tail; RFC-0089 "M3a as landed"). Phase 5 found that `check_return` records a
   returned projection as a **lend** rather than refusing it, and wrote the
   consequence in the source: *"so nothing releases them."*
2. **A must-use obligation stops at a container** (RFC-0086 M3, PR #100).
   Storing a declared must-use value into a record field or an array counts as
   discharged there.
3. **An element of a built-in container is unreclaimable** (RFC-0087 U4).
   `Array<T>` cannot say whether it owns its elements or views somebody else's.

This RFC says they are one question, gives the rule that answers it, and says
what the rule costs. The question is:

> **When a value is read out of a place and put somewhere else, who owns it?**

Today the compiler answers that question twice, and the two answers disagree.

---

## The asymmetry, and it is the whole finding

These two programs differ by one `let`. Both were run through
`vyrn check` at `1c5bbc6`.

```vyrn
type Doc = { title: String, body: String }

fn main() -> Int64 {
    let d = Doc { title: "a" + "b", body: "c" + "d" }
    let mut out: Array<String> = []
    out.push(d.title)
    print(out[0])
    print(d.title)
    return 0
}
```

```text
ok
```

```vyrn
type Doc = { title: String, body: String }

fn main() -> Int64 {
    let d = Doc { title: "a" + "b", body: "c" + "d" }
    let mut out: Array<String> = []
    let t = d.title
    out.push(t)
    print(out[0])
    print(d.title)
    return 0
}
```

```text
7:0: `t` may not be stored into `push(..)` — it is read out of a place that owns it
  fix: `t.copy()` if both sides need a value
```

The second program is refused and the first is not. Both put one buffer in two
places.

**The compiler already knows the right answer.** `MoveCheck::borrow_from`
(`compiler/vyrn-frontend/src/movecheck.rs:986`) binds a `let` of `r.f` or `xs[i]`
to `Borrow::Projection`, and the doc above it says why: *"A field or element read
binds a `Borrow::Projection`: the aggregate still owns it, so the new name may be
read but not stored on."* Rule 2 of RFC-0089 then refuses the store.

**It declines to give the same answer without the `let`.** `MoveCheck::store`
(`movecheck.rs:952`) reads:

```rust
// Reading a field out of a record does not take the record: the binding
// it makes is itself a borrow (recorded by the caller), so nothing moves.
if path != root {
    return Ok(false);
}
```

The parenthesis is the bug. "Recorded by the caller" is true of a `let`, whose
caller records a `Borrow::Projection`. It is false of `out.push(d.title)`, where
there is no `let` and no caller to record anything. The store is allowed and is
not a move, so nothing anywhere states that two places now name one buffer.

The return path has the same shape one function over. `returned_borrow`
(`movecheck.rs:1325`) ends:

```rust
let (root, path) = place_path(e)?;
Some((self.borrow_of(&root)?, root, path))
```

`borrow_of` answers `None` for an owned local, and `?` then discards the whole
projection. So `return d.title` out of a locally built record is invisible to
`check_return` — not refused, and not even recorded as a lend. Verified:

```vyrn
type Doc = { title: String, body: String }

fn titleOf() -> String {
    let d = Doc { title: "a" + "b", body: "c" + "d" }
    return d.title
}

fn main() -> Int64 {
    print(titleOf())
    return 0
}
```

```text
ok
```

That program prints `ab` today. Give `Type::Record` a release row and it returns
a freed pointer. Eight lines, and it is the whole of symptom 1.

**The instrument reports the gap and gets the wording wrong.** `vyrn why
--memory` on the same file prints, for `titleOf`, both of these:

```text
    transfers: yes — the caller owns the result, and releases it by freeing the String buffer
    line 4     d                NOT reclaimed — the type Doc owns no heap
```

`Doc` holds two Strings. `Leak::NoRelease` is minted whenever `release_kind`
answers `None` (`own.rs:829`) and its wording is `"the type owns no heap"`
(`own.rs:493`), which is a different question. `own::owns_heap` answers the
question the text claims to. The printer is the one place a reader is told what
the model does, so this is worth one line of M0.

**Fixed in M0.** `Leak::NoRelease` carries both answers now, and the two reasons
are worded apart: a scalar keeps "the type owns no heap", and a type that owns
heap with no row reads "nothing releases the type `Doc` yet".

---

## The rule

> **A projection is a borrow of its root, whatever the root is.**

A projection is a field of a place, an element of a place, or a pattern binder
over a place. Today the compiler says a projection is second-class when its root
is a borrow, and says nothing when its root is an owner. It says it always.

Nothing else changes. RFC-0089 rules 2 and 3 are landed and enforced, and they
already carry every consequence:

- a projection may be observed, and may be passed as a `read` argument;
- it may not be stored into anything that outlives the frame — a field, an
  element, module state, a capture;
- it may not be returned.

The fix menu is the one that already prints: `.copy()`, or `consume` the source.

### Three sites

| site | today | becomes |
|---|---|---|
| `movecheck.rs:952` | `if path != root { return Ok(false) }` | the refusal the `borrow_of` arm eight lines above already writes, under the same `outlives` and `owns_heap` guards |
| `movecheck.rs:1325` | `Some((self.borrow_of(&root)?, root, path))` | `borrow_of(&root)`, or `Borrow::Projection` where `path != root` |
| `movecheck.rs:1158` | records a lend for a returned projection | refuses it; the `lending` set keeps only what it still needs |

`a[i]` reaches `movecheck` as `at(..)`, which `borrow_from` already reads
(`movecheck.rs:988-993`), so an element read is covered by the same three lines
as a field read.

**That last sentence is wrong and M0 found it.** It is true of `borrow_from`,
which reads `at(..)` itself, and false of both `store` and `returned_borrow`,
which bail at `place_path` — and `place_path` answers `None` for a call. So
`out.push(xs[i])` reached none of the three sites. **M1 widened both**, and
`element_path` also walks a field OF an element (`fs[0].key`), which was
otherwise a one-dot-wide way around the rule. Four lines, not zero.

### Why "refuse" is affordable now and was not in Phase 4b

`check_return` states its reason in the source (`movecheck.rs:1164-1172`):
refusing a returned projection *"would demand `.copy()` from `Json` and `Html`,
which refer to themselves and have no structural copy (RFC-0089 M1b)."*

**That reason is gone.** RFC-0091 M1 shipped `Copy` as a protocol a type
declares, and its "M1 and M3 as landed" section says a self-referring type now
*"copies through the recursive function M1b's diagnostic already told the reader
to write."* `std/json` exports `copyJson`; `std/graphql` carries `gqlCopyErr`
beside it. A `.copy()` on a `Json` is writable. RFC-0087's open tail already
recorded the re-pricing and named what is left: *"What is not priced is the
corpus cost of demanding it."*

Pricing it is M0, and M0 is this RFC's gate.

---

## Is it one question or three?

It is one mechanism, three symptoms, and they are **a chain rather than three
equal shares**. Saying that plainly matters, because the milestones stop in
different places depending on which link fails.

- **Symptom 1** — a record, a user enum and a fixed array get their release row
  — is closed by the rule alone.
- **Symptom 3** — U4, an element of a built-in container — is closed by the rule
  **plus three built-in view constructors**, which manufacture from inside the
  compiler exactly what the rule refuses in user code.
- **Symptom 2** — a must-use obligation through a container — has no mechanism
  of its own at all. It is symptom 3 plus a recursion in `Owned::linear_kind`
  (`own.rs:209`): `must_use(Array<T>)` answers what `must_use(T)` answers. Once
  a container releases its elements, discharging them is the release, and
  abandoning the container is the error the obligation already knows how to
  raise.

So symptom 2 is a consequence, not a peer. Build the rule and stop, and 2 and 3
stay open. Build the rule and the view constructors, and 2 costs a recursion.
That is a chain, and a chain is one mechanism.

---

## The three view constructors

The census names one: *"`m.keys()` is the view: a fresh buffer holding the map's
own key pointers, which a per-element release would free twice."* Read against
source, there are **three**, and `own.rs`'s comment (`own.rs:319-321`) undercounts.

| builtin | what it copies | what it aliases |
|---|---|---|
| `m.keys()` | the key pointer array (`toolchain.rs:172-177`, `direct.rs:10392-10420`) | every key String |
| `toArray(sa)` | the element words, one `memcpy` (`vyrn-codegen/src/lib.rs:9523-9527`) | every element of a `SmallArray<String, N>` |
| `@list(fixed)` | the element words, through `array_n_to_heap` (`lib.rs:9684-9693`) | every element of an `[N x String]` |

**M2 re-counted this table and it is wrong in both directions.** There are still
three, and they are not these three.

- **`@list(fixed)` is not one.** Its only producer is the tagged-template
  desugar (`parser.rs:4645`), which always hands it an `ArrayLit` — a temporary
  whose elements are stores, so `@list` MOVES them. Making it copy would have
  leaked the originals, since a fixed array has no release row to give them
  back. It is left alone.
- **`toArray` is two, not one.** On a `SmallArray` it copies the element words,
  as the table says. On a plain `Array` it returned the receiver's `{ptr, len,
  cap}` triple **unchanged** (`lib.rs:9576`), so two owned bindings named one
  buffer — a double free of the buffer before the element row and of every
  element after it. Its own comment said "a defensive copy-out too" and the code
  did not. It copies now. (The direct backend refuses that receiver outright —
  `no lowering for the call @toArray` — so the shape was interpreter-and-native
  only, and it stays that way.)
- **The third is not a builtin at all.** It is the compiler's own synthesized
  `fromJson` decoder, which walked a one-element carrier with `for x in val` and
  handed the element out — the exact spelling M1's rule refuses in written Vyrn.
  A synthesized module is never move-checked, so the rule had never been applied
  to it. Both sites take the element now (`for x in consume val`). M1 found and
  fixed a third site in the same file (`f0[0]` → `swapRemove(0)`) and these two
  were the remainder.

A fourth builtin that returns `Array<String>` does **not** alias, and it is the
one that shows the right shape. `args()` walks argv and builds an owned String
per element with `__vyrn_str_new` plus a `memcpy` (`lib.rs:352-376`). Its own doc
comment two lines above says the opposite — *"elements point directly at argv —
never freed"* — and the code has moved on without it. `listDir` is
generation-time only and reaches no compiled drop site.

**The interpreter and the compiling backends already disagree about `keys()`.**
`interp.rs:4945-4952` builds each key as `Val::Str(Rc::new(k.clone()))` — a deep
copy. The two compiling backends copy the pointer array. Nothing observes the
difference today because nothing releases an element. The day an element release
lands, the interpreter is right and the backends are wrong.

M0 checked that "nothing observes it" holds rather than assuming it. A program
that snapshots the keys, then inserts, removes and finally `drop`s the map, and
then reads the snapshot, prints the same five lines on all three engines. Three
things keep it unobservable and all three are load-bearing: no element release,
a String no program can mutate, and `FreeMap` freeing the two map buffers and no
key. So the disagreement is in the representation only. It is not a live
divergence and parity is not missing a case — there is nothing yet to observe. So making `keys()`
copy its keys is a **parity repair**, not a new cost, and the oracle already
pays it.

The price is real and it is bounded: `m.keys()` becomes O(total key bytes)
instead of O(count). The corpus calls `.keys()` at 14 sites and `toArray(` at 1.
M2's gate measures it.

---

## The release rows that follow

Each is one arm in `Owned::release_kind` (`own.rs:231`), and each has its own
gate. The seeded built-in rows are already how `String`, `Array`, `Map`,
`Option`, `Result` and `Fn` are answered; this adds no new kind of table.

| row | today | becomes | needs |
|---|---|---|---|
| `Type::Record`, `Type::Enum` | `None` (`own.rs:325`) | `Deep` | the rule — **landed in M3** |
| `Type::ArrayN` | `None` (`own.rs:325`) | `Deep` | the rule — **landed in M3**; `@list` needed nothing, see "M2 as landed" |
| `Type::Array` | buffer only (`own.rs:238-241`) | buffer + elements | the rule + the constructors — **landed in M2** |
| `Type::Map`, `Type::SmallArray` | buffer only | buffer + elements | the rule — **landed in M3**; the view was not a constructor but the map LOOKUP |
| `linear_kind` on a container | reads the container's own key | reads the element's too | the row above — **landed in M4**; and the element's declared release had to start being CALLED, see "M4 as landed" |

The walk is the one Phase 5 settled: `copy` run backwards, written as one shape
per engine so the two cannot disagree about a payload encoding. `Option` and
`Result` already take it.

---

## Worked examples

### Symptom 1 — a record releases its fields

Before the rule, `titleOf` above compiles and a `Deep` row on `Type::Record`
makes it return freed memory. After the rule, `titleOf` is refused with the
menu rule 3 already prints, and the fix is one call:

```vyrn
type Doc = { title: String, body: String }

fn titleOf() -> String {
    let d = Doc { title: "a" + "b", body: "c" + "d" }
    return d.title.copy()
}

fn main() -> Int64 {
    print(titleOf())
    return 0
}
```

That program prints `ab` today and prints `ab` after the row lands, and `d`'s two
Strings are reclaimed at the end of `titleOf` rather than leaked.

The four sites Phase 5 named take the same fix, and three of the four were
already migrated in spirit by RFC-0089 M2's loop work:

| site | shape | fix |
|---|---|---|
| `std/jsondec`'s `tagOf(v)` | `match v { JStr(s) => s, .. }` | `s.copy()`; the binder is a projection of the scrutinee |
| `std/graphql`'s `gqlScanner(src)` | a record holding a view of its argument | **nothing to do, and M1 checked.** `std/scan`'s `scanner` copies every String it is handed (`std/scan.vyrn:82-88`), so the `Scanner` owns what it holds. RFC-0054's migration closed this before this RFC was written |
| `gqlParseQuery` | `GqlQuery { sels: set.sels }` | `set.sels.copy()`, or consume `set` |
| `std/contract`'s `headOf` | a cut out of a scrutinee | already closed by Phase 10a's row |

### Symptom 3 — U4, and it is in scope

```vyrn
fn main() -> Int64 {
    let mut m: Map<String, Int64> = [:]
    m["a" + "b"] = 1
    let ks = m.keys()
    print(ks[0])
    return 0
}
```

`ok` today. `ks` holds the map's own key pointer. Give `Array<T>` an element
release and this frees a String the map still holds and then frees it again at
the map's own drop. Make `keys()` copy and the program is correct under the
release, and `Array<String>` may own its elements everywhere, because every other
route into an array element is a store — and rule 2 already refuses storing a
borrow, while this RFC's rule refuses storing a projection.

**The census's framing of U4 is slightly wrong and this RFC corrects it.** PR #82
and RFC-0087's open tail both say *"the mechanism is a declaration, and a
built-in container has nothing to declare it with."* `release_kind` seeds
built-in rows in the compiler and always has — its own doc says so (`own.rs:126-138`).
What `Array<T>` lacks is not a declaration site. It is the **proof** that its
elements are owned, and that proof is what this rule supplies.

The declaration is still the mechanism for a container with a **liveness rule of
its own**. `std/slots` declares because it has dead slots and a per-slot answer
the compiler cannot derive. `Array<T>` has no dead slots: every element in
`0..length` is live. Both mechanisms exist and neither replaces the other.

### Symptom 2 — the obligation travels

```vyrn
protocol MustUse {}

protocol Owned {
    fn release(self)
}

type Txn = { label: String }

impl MustUse for Txn {}

impl Owned for Txn {
    fn release(self) {
        print("end " + self.label)
    }
}

fn begin(label: String) -> Txn {
    return Txn { label: label.copy() }
}

fn main() -> Int64 {
    let mut pool: Array<Txn> = []
    pool.push(begin("orders"))
    print("done")
    return 0
}
```

Run on the interpreter at `1c5bbc6`, that prints exactly one line:

```text
done
```

The declared `release` never runs. The obligation was discharged by the store and
the teardown the type exists to run was never reached. This is the hole PR #100
recorded in its own words.

With the `Array` element row and the `linear_kind` recursion, `Array<Txn>`
carries `Txn`'s obligation. `main` must `drop pool` or hand it on, and the drop
runs `release` per element, so the program prints `end orders` and then `done`.
RFC-0075's storage ban stays `Stream`'s alone and stays refused for the reason
PR #100 gave: it is a rule about representation, and it would forbid this
`Array<Txn>`, which is the program a pool of transactions wants to write.

---

## What it does not close

- **A capture is not a projection.** `own.rs:281-289` records that a `fn` value's
  release is shallow, because two lambdas over one String hold one pointer in two
  capture blocks. A capture is a store of a value, not a read out of a place, so
  this rule does not reach it. §16's remainder stays open.
- **`Ref<T>` and `cell`.** The one runtime island, by RFC-0089 rule 5. Unchanged
  and deliberately so.
- **§5/U6 inferred regions** and **§10 the native spawn frame.** Untouched. §10
  needs a declared release on `Task<T>`, which is RFC-0091's mechanism pointed at
  a new type.
- **RFC-0091 M5, the conditional place.** Unrelated. M5 relaxes what a projection
  may *yield*; this RFC changes what a projection *is worth* at a store. Neither
  needs the other.
- **`SmallArray` portability.** M4's three blockers are untouched.
- **A whole-aggregate store** (`r = <record>`) still releases nothing, for the
  reason PR #77 recorded: the destination-first write destroys the old value
  before it can be walked.

---

## What it costs in each of the three engines

### The frontend: three lines, and no new table

`movecheck::check_accum` runs on the `Program` AST
(`compiler/vyrn-frontend/src/lib.rs:171`), after the checker and before any
lowering. It does not name `project::` anywhere. `project::inline` runs inside
each engine's lowering.

So the two side tables Phase 7a called out — the elided `get`/`set` generation
checks and the lambda monomorphization keys, both keyed by node address — are
**not touched**, and `Projection::is_identity` is not consulted. The RFC-0091 M2
inlining and this rule do not meet. That is a property of where the code sits,
not a measurement.

Compile time: the rule adds no walk. `store` and `returned_borrow` already
compute `place_path` and already ask `owns_heap` at exactly these points. The
refusal is a branch on a value both functions hold.

### The interpreter

A record, enum and fixed-array release is the `copy` walk backwards, which the
interpreter already runs for `Option` and `Result`. A per-element container
release is a loop over the elements. Both are observable only through a declared
`Owned::release`, which can print — so parity is the gate, and `examples/mustuse.vyrn`
plus `examples/ownedcontainer.vyrn` are the two files that make it observable
today.

### The textual backend

`deep_release` reaches `release_sum` (`vyrn-codegen/src/lib.rs:2829`) and
`release_enum` (`lib.rs:2869`), and both emit `br i1` with fresh labels at
whatever point in the block the drop was reached, including from
`emit_all_drops` before a `ret`. A record walk is a field sequence and needs
nothing new.

**A per-element container release is the one new emission shape in this RFC: a
counted loop at a drop site.** Every release this backend emits today is
straight-line or a bounded branch.

### The direct wasm backend

The same loop, and the same new shape. Here it also costs **bytes**. A drop of an
`Array<String>` grows from three instructions to a loop, at every drop site.
RFC-0089 M1a already measured that this output is size-sensitive: `fib.wasm`
1,334 → 1,590 bytes and `domdemo.wasm` 25,966 → 27,630 for the String header
alone.

The mitigation is one `@__vyrn_release_elems_<T>` per element type, called from
each site. That is the runtime-function shape Phase 5 rejected for a
**variant-aware** release, and its reason there — per-type synthesis in two
backends — applies here too, while the size argument now points the other way.
**This is an open lowering choice and M3's gate decides it by measurement**, not
by repeating either earlier argument.

---

## Milestones

### M0 — price the refusal — **GATE PHASE**

No semantic change. Turn the rule on behind a flag that counts instead of
refusing, in the shape `borrow_store_sites` already has
(`cargo test -p vyrn-frontend --lib borrow_store_sites -- --ignored`), and
measure over **linked** programs. Phase 4b's per-file measurement was wrong by
81 sites for exactly this reason: a file parsed alone cannot name an imported
type, so `owns_heap` answers "unknown" and the site is invisible.

Report three numbers: stores of a projection, returns of a projection, and how
many of each are a scalar the rule does not reach.

A textual upper bound over `std/` and `examples/`, which counts scalars and
already-refused roots and is therefore high: 24 `push` of a field, 18 `return` of
a field, and 118 struct-literal fields fed by one, of which 21 name an obviously
scalar field. So the honest expectation is **under 160 and probably well under**.

Also in M0, because it is one line and the reader deserves it: `Leak::NoRelease`
prints "the type owns no heap" for a type that owns heap and has no row
(`own.rs:493`). Split the two reasons.

**M0 as measured.** `movecheck::projection_sites` records what the two sites
would refuse and refuses nothing;
`rfc0092_projection_sites_over_the_corpus` links every `.vyrn` under
`examples/` and `std/` as a root — 210 files, all 210 link — and counts a site
once per (file, line, path, kind).

| | owns heap | scalar |
|---|---|---|
| stores of a projection | 48 | 41 |
| returns of a projection | 44 | 21 |
| **total** | **92** | 62 |

**The gate passes.** 92 is under 300, and under the 160 this section expected.
The textual bound counted 160 because a grep cannot type: two thirds of the
sites it found name a scalar, and rule 2 already refuses the rest of what it
matched.

Three readings the count corrects.

- **Linking is what made it small, not large.** Phase 4b's warning was that a
  file read alone cannot name an imported type. It is still true — 8 store
  sites read `?` even linked — but 4 of those 8 are `.length`, and the other 4
  (`t.module` and `t.name` in `std/rpc`'s generators, `tk.text` in
  `std/http`'s) are Strings the declared reading cannot name because the value
  comes from `moduleInterface`. So the honest store number is 48, and at most
  52.
- **An element read reaches none of the three sites.** §"Three sites" says
  `a[i]` is covered because `borrow_from` reads `at(..)`. That is true of the
  `let` path and false of the store path: `store` bails at `place_path`, which
  answers `None` for a call, before it decides anything. 19 more sites live
  there (17 stores, 2 returns), and M1 either widens `store` or writes down
  that it did not.
- **The return half is nearly half the bill**, not the small tail the
  recommendation's fallback assumes. 44 of 92.

**Gate.** If the linked count is over 300, STOP and report. The premise is that
this is a migration the size of RFC-0089 M2's, which was 241 `.copy()` and 21
`consume`. Three times that is a different decision and it should be taken with
the number in hand, not with this paragraph.

### M1 — the rule

The three sites above. The corpus migrates in the same change, module by module,
as every phase in this chain has done. `check_return`'s lend recording shrinks to
what the export boundary still needs (RFC-0089 M3b).

**Gate.** Three-way parity byte-identical including traps, and the memory suite
unmoved — eleven steady of twelve, with `elementLeak` still leaking. M1 changes
what a program may **say** and must move no row.

**M1 as landed.** The gate is met: `124 checked, 11 skipped, 0 failed`, and the
memory suite reads eleven steady with `elementLeak` still leaking. The
instrument stays as the regression guard and now asserts zero for every store
class.

**The element read is refused.** M0 left this as M1's decision and M1 widened
`store` and `returned_borrow` to see one. The reason is this RFC's own thesis:
`let t = xs[i]` already bound a `Borrow::Projection` and rule 2 already refused
storing `t`, so leaving `out.push(xs[i])` alone would have reproduced, for
elements, exactly the two-spellings-two-verdicts defect the RFC exists to
remove. `element_path` also walks a field of an element (`fs[0].key`), which was
a one-dot-wide escape hatch, and it spells the index back (`xs[i]`, `fs[0]`) so
the `.copy()` on the menu is text `vyrn fix` can apply.

**Two more spellings the rule had to reach**, neither in M0's count and both
found by migrating:

- **An assignment did not rebind.** `let t = d.title` was refused at the next
  store and `let mut t = "" ; t = d.title` was not. `Stmt::Assign` now carries
  the same answer a `let` does. Four corpus sites.
- **A `fromJson` decoder is not in the instrument's reading.** It runs on the
  linked program *before* the checker, and the decoder is synthesized during
  checking. Every decoder read its value back out of a one-element carrier
  (`f0[0]`). It takes the element now — `f0.swapRemove(0)`, the primitive
  RFC-0011 already shipped — so nothing is copied and nothing is allocated.

**The ratio is 116 `.copy()` to 7 shape changes, 1 deletion and ZERO
`consume`**, across 25 files (the corpus goes from 383 `.copy()` calls to 499;
about a dozen of the 116 are the bodies of the new `Copy` walks rather than
migration sites).

**The zero is the finding.** Phase 4b's ratio was 58 `.copy()` to 4 `consume`,
and it explained the copies by mixed-return functions. That explanation still
holds — most of these sites are a `match` arm handing back its scrutinee's
payload beside an arm that yields a fresh value — but it is no longer the
binding reason. `consume` is a **parameter declaration**, and rule 2 already
refused a projection of a borrowed parameter before this RFC. So every site
this rule newly reaches has a root that is a LOCAL or a fresh call result, and
at none of them is `consume` on the menu at all. A projection of a local has
exactly one fix in today's language, and it is `.copy()`.

That is why 116 is not a corpus of defensive copies so much as a corpus with one
missing verb. Each copy is a `String` or a small array — nothing here copies a
tree — and the seven places where a copy would have been unbounded or absurd
took a shape change instead: `httpApply` and `httpMaybe404` edit the response
they were given instead of rebuilding it, `vyxParseElem` and `vyxProcessElem`
append their node to the caller's list, and `gqlArgList` and `gqlSelSet` do the
same with their arguments and selections. `vlog`'s `Read` carrier is deleted
outright — `readLine` already answers an `Option` and RFC-0060's `if let` reads
one.

`Json` and `Html` gain `impl Copy`, which is what the compiler's own refusal
names, and it is what makes `r.value.copy()` writable at ten `std/graphql`
sites. A declared `Copy` answers about the value it is asked about and **not**
about a part of one, so `Array<Json>` still needs a hand-written walk
(`copyJsonArray`, `copyJsonFields`, `copyHtmlArray`, `copyGqlErrs`).

**`gqlScanner` needed nothing.** Phase 5 named it as the case "nothing sees as a
lend at all". It is not one: `std/scan`'s `scanner` copies every String field it
is handed (`std/scan.vyrn:82-88`), so the `Scanner` owns what it holds and the
rule finds nothing. RFC-0054's migration closed this before this RFC was
written, and the worked-examples table above is one step behind the source.

**Seven returns are recorded and not refused, deliberately.** They all read
`return match hit { Some(r) => r, .. }` on an owned `Option<Response>` (six in
the `pages` generator's output) or `Option<Cargo>`. `check_return` refuses an
arm-yielded projection only where the caller RELEASES the result — Phase 4b's
guard, which this RFC does not move — and a record has no release rule until
M3. They cannot dangle while nothing frees a `Response`. **M3 closes them, in
the change that gives the row**; refusing them here would buy nothing and cost a
copy of a whole HTTP response on every request. The instrument asserts the
number so it cannot grow.

**What the migration wanted and the language does not have.** There is no way to
move a field or a payload out of a place the frame owns. `consume` is a
parameter capability and `for x in consume xs` is a loop form; neither reaches
`let d = f() ; use(d.title)`. Every shape change above is a way around that one
gap, and `swapRemove` is the only spelling of it that exists — for a container,
not for a record or an enum. That is the sibling of the hole Phase 4b closed
with `for x in consume xs`, and it is filed here rather than forced.

**Not widened.** A `for` variable over a container the loop does not own
(`Borrow::Element`) is a projection in kind, but it is outside the three sites
and outside the count that priced them, so it keeps Phase 4b's verdict.

**The lend that stops being admitted, and the test that pinned it.**
`an_export_may_not_lend_its_result` opened by asserting that an ORDINARY
function may lend an arm-yielded projection — `fn text() -> String { return
match tag { Word(s) => s, .. } }` over module state — because `lending` records
it and the Vyrn caller releases nothing. M1 refuses that program, so the
assertion had to be read before it was changed. It should be refused: Phase 6
already refused the DIRECT spelling of the same program (`return title` on
module state, with "which nothing may take" and the same `.copy()` fix), so one
program had two verdicts one variant apart — this RFC's defect, a third time.
And the mechanism that made the lend safe is `lending`, which this RFC's
"Rejected" section names as the guesser it exists to remove.

What is still the export boundary's own is the **wording and the fix**, and it
took three tries to get there. That is worth writing down, because the shape of
the mistake is the RFC's own thesis pointed at the compiler.

**`check_return` refuses a return from three places**, and each is reached by a
different question:

| spelling | how it reaches a refusal |
|---|---|
| `return q` | a place named straight at the `return` whose root is a borrow |
| `return d.title` | a place named straight at the `return` whose root the frame OWNS — this RFC's rule |
| `return match t { W(s) => s }` | a borrow yielded by an arm, found by `returned_borrow` |

They are separate on purpose: each asks a different question first, and merging
the questions would merge three different reasons into one vaguer one. **What
they must not differ about is the answer**, and they did. The `exported` check
lived at the arm exit alone, so an `export extern fn` returning a `read`
parameter directly was handed the general menu and told to `declare the
parameter q: consume ..` — which its own signature then refuses: *"the caller
across this boundary is JS, and it releases the String when the call returns"*
(RFC-0089 M3b). One program spelled two ways got two menus, and one of them
sent the reader to a second error.

**Fixing it at one exit fixed two spellings and missed the third, twice.** All
three call `refuse_return` now, which asks `exported` before anything else. The
module-state refusal deliberately does not: "nothing may take module state" is a
different FACT rather than a different caller — it is true of a Vyrn caller and
a JS caller alike, and its menu already names `.copy()` alone. A comment says
so where a reader will meet it.

`an_exports_borrow_menu_names_copy_alone` asserts the store and all three
returns in one test, so the next person to touch one has to look at the others.

**The leak count moved.** `vyrn why --memory` over the corpus reports 2,127
bindings not reclaimed, down from 2,230 — 200 files answer, ten need a project
root and are skipped by the same script both times. The rule refuses programs,
so the count moving is the visible half of it working.

**One linker bug, found by the migration.** `impl Copy for Json` did not
resolve: the parser flattens `impl P for T` into a function named `P__T__m`, an
injected runtime module renames every declaration by prefix (`json$Copy__Json__copy`),
and the checker resolves the call by mangling the RENAMED type key
(`Copy__json$Json__copy`). No `std/` module had declared an impl in an injected
module before, so nothing had looked. Fixed in `loader.rs`, and written down
where the next reader meets it: **RFC-0078 §1** (the reserved-spelling section
that owns the rename) and the flattening comment in `parser.rs` that mints the
name.

### M2 — the three view constructors

`m.keys()`, `toArray(sa)` and `@list(fixed)` copy their elements where the
element type owns heap, and emit exactly what they emit today where it does not.

**Gate.** Three-way parity, and a benchmark row for `keys()` over a map of 1,000
sixteen-byte keys, before and after, in `examples/membench.vyrn`. The measurement
is kept whatever it says: this is a correctness repair, and the interpreter has
been paying it all along.

**M2 as landed.** The gate is met and it took the `Array` row with it, because a
constructor that copies is only worth building beside the row that made it
necessary. `Array<T>` releases its elements, `elementLeak` is **steady**, and
the memory suite reads twelve steady and one new leaking row. Parity is
`124 checked, 11 skipped, 0 failed`, and the instrument still reads
`stores: 0`, `elem-store: 0`, `elem-return: 0`, `returns: 7`.

**The row is a recursion, not a widening.** `release_kind(Array<T>)` answers
`Deep` where `release_kind(T)` answers anything, and `FreeArr` where it does
not. So an element is released the way its own type is released: `Array<String>`
frees its Strings, `Array<Record>` frees nothing until M3 gives a record its
row, and neither engine needed a second table. The gate on the element loop is
the element's ROW rather than `owns_heap` — a record reaches two Strings and
owns them under no rule yet, and walking into one here would have shipped M3
without measuring it.

**Three back doors, and only one was a builtin the census named.** The
constructor table above records what M2 found: `@list` never viewed anything,
`toArray` was two sites rather than one, and the third was not a builtin at all
but the synthesized `fromJson` decoder, which is the one Vyrn in this repo that
M1's rule never got to check.

**A declared container had to stop doing it by hand.** `std/slots` released
every element in a loop and then dropped `vals`, which is the same range. The
second release is the built-in row now, so the loop is gone — six lines deleted
from the module whose whole point was that it could say what an array could not.
The double free showed up as native heap corruption in `examples/genref.vyrn`
inside the hour, and NOT in the memory suite: a double free frees, so a row that
watches the steady state cannot see one. Parity is what sees it.

**The price, measured.**

- `keys()` over 1,000 sixteen-byte keys, ten snapshots: **1.24 ms → 1.49 ms**
  median, native (`examples/membench.vyrn`). One `malloc` and one `memcpy` per
  key where there was a pointer store: 20% on the row that is all `keys()`, and
  the interpreter has paid it since RFC-0028.
- **The keystroke budget is unmoved**: `lspbench` reads 9.9 ms on `vlog.vyrn`
  and 55.1 → 57.2 ms on `graphql.vyrn`, against the 97 ms budget.
- **A leak got bigger and it is now a row.** `for k in m.keys()` walks a
  temporary, and a loop over a temporary releases nothing — it cannot, because
  the body may take an element, which is exactly what the JSON encoder's
  `fs.push(Field { key: k, .. })` does. The snapshot used to leak 4 bytes per
  key; it leaks the key BYTES now. Measured native over 2,000 turns of a
  100-key map: **6 MB peak → 24 MB**. The memory suite gains `keysLoop`,
  asserting the leak, so the day it stops is a failure and not a silence.
  Phase 10a's row for an `if let` over a temporary is the shape that closes it,
  applied to `for` and per element — and it needs the per-element move tracking
  a store out of the loop body already relies on.

**A self-referring element type stops the walk.** `type L = Array<L>` has no
bottom to a structural release, and the row overflowed the stack on the
interpreter and both compiling backends before the guard went in — the same
crash `copy` met in Phase 4b, met again by its mirror. It answers the buffer
alone, and `own` is the one place that answers: both backends ask
`release_kind` for the array rather than re-deriving it from the element, so the
guard cannot be forgotten in one engine and kept in the other.

**One pre-existing double free found and left alone.** `fromArray(xs)` on a
NAMED array leaves the array and the stream owning one buffer, and the native
binary corrupts its heap. It reproduces on `Array<Int64>`, so it predates this
row; no example passes a named array to `fromArray`, which is why parity has
never seen it. Filed, not fixed here.

### M3 — the release rows

`Record`, `Enum` and `ArrayN` take `Deep`. `Map` and `SmallArray` take a
per-element release. M3 decides the inline-loop against the per-type function by
measuring both on `domdemo.wasm` and `fib.wasm`.

**`Array` is done** — M2 took it, because the constructors and the row that
makes them necessary are one change and shipping either alone proves nothing.
What is left is the four rows that have no view constructor between them and the
rule.

**Gate.** `elementLeak` flips to steady — **met in M2**. And the emitted size of
`domdemo.wasm` grows by less than the String header did (1,664 bytes), or the
per-type function ships instead.

**What M2 leaves M3 to answer.** A store into an `Array<T>` place — `xs = ys` —
hands back the one buffer it always did and leaks the elements it held. Freeing
them means reading a length the store is in the middle of replacing, and the
snapshot-then-store order the rest of the store path uses does not carry a
count. It is the same shape the `Map` and `SmallArray` rows will need.

**M3 as landed.** All five rows are in. `Record`, `Enum`, `ArrayN`, `Map` and
`SmallArray` answer `Deep`, and RFC-0089 rule 4 is whole for the first time
since Phase 5 wrote it down. Parity is `124 checked, 11 skipped, 0 failed`, the
memory suite holds all thirteen rows, and the instrument reads
`stores: 0`, `elem-store: 0`, `elem-return: 0` and — this is the number that
moved — **`returns: 0`, down from 7**.

**The walk was already written.** Both backends reach a record and a user enum
through an `Option` payload today, because `release_kind(Option<T>)` has
answered `Deep` since Phase 5 and the walk is structural. So `Option<Doc>` freed
a `Doc`'s Strings while `let d: Doc` freed nothing — one type, two verdicts, an
`Option` apart. What M3 adds is the row, the fixed `[N x T]` arm neither backend
had, the two container element walks, and one guard.

**The guard is the `Array` row's, one shape over.** A type that reaches ITSELF
has no bottom to a structural release, and `type Node = { kids: Array<Node> }`
is ordinary Vyrn. It answers `None` and its places leak. `Json` and `Html` are
that shape, which is also why M1's `.copy()` menu sent them to a hand-written
`copyJson` — the same fact refusing the same walk from the other side.

**The seven deferred returns are closed, and not by the copy M1 priced.** M1
recorded them, refused to migrate them, and asserted the number so it could not
grow — because `check_return` refuses an arm-yielded projection only where the
caller RELEASES the result, and a record had no release rule. It has one now and
all seven refuse. RFC-0093 M1 shipped the take in between, so each of them reads
`return match consume hit { .. }`: the arm yields a value the frame gave up and
nothing is copied at all. Six are one line of the `pages` generator; the seventh
is `examples/genericpayload.vyrn`.

**Three ways to free a place twice, none of which the memory suite could see.**
A double free frees, so a row watching the steady state cannot see one. Parity
saw all three, as native exit `0xC0000374`.

- **A `drop` of a projection.** `let owned = box.items ; drop owned` hands back a
  buffer the record still holds. RFC-0089 recorded that the rule which would
  refuse it was written in `movecheck` as a COMMENT rather than as a check, and
  `std/slots` says so in its own doc. It is a check now — rule 2 at the third
  place a value can leave. Five declared releases take their fields instead
  (`consume self.vals`), which is what those lines always meant. `consume self`
  did not parse until this milestone: `self` lexes as its own keyword, and the
  take's contextual test looked for an identifier.
- **A store the place desugar hid.** RFC-0082 M2 hoists a place assignment's
  index and value into temps, and every `[`-named binding was exempt from rule 2
  — an exemption written for the round-trip ELEMENT temp, which reads a place and
  writes it straight back. `ps[]val` is not that; it is the statement's
  right-hand side. So `ps[1].xs = ps[0].xs` was an unchecked store of a
  projection. `is_place_temp` tells the two apart, and the refusal moved to the
  hoist so the message names `ps[0].xs`.
- **A map lookup is a projection of its map, and nothing said so.** `m[k]`
  reaches `movecheck` as `at(m, k)`, a call, so the binder in
  `match ps[k] { Some(v) => v, .. }` was an OWNER: `std/http`'s `httpHeader`
  handed out a String the map still held and `httpHeaders` stored it into a
  second map. `element_path` is the reading that walks it — the same widening M1
  gave `store` and `returned_borrow`, arriving at the pattern binder late because
  the `Map` row is what made it observable. Three corpus sites, three fixtures.

**And one the compiler refused to emit.** `let t1 = Tagged { tag: "ab", .. }`
records the DECLARED type, parameters and all, so `llt` answered `void` for a
`Param` and the walk emitted `load { void, { i64, i64 } }`. The slot knows the
instance — its alloca was built from it — so the drop reads the slot's type. It
went unseen while only sums reached here, because a generic sum's payload
travels as a word.

**RFC-0093's hole is a leak, on purpose.** `consume d.title` takes one field and
leaves the record behind. The release walk is the type and the type does not know
about the hole, so a binding with one is left **unreclaimed** — the untaken
places leak too. RFC-0093 M2 is the milestone that carries the hole set through
to the walk and releases the rest. A documented leak is a task; a double free is
a bug in a language that promises memory safety.

**The price, measured.**

- **`domdemo.wasm` does not move at all: 28,445 bytes, before and after** — the
  gate said "less than the String header's 1,664" and the answer is zero.
  `fib.wasm` is 1,522 either way. Both are records-of-`Html`, and `Html` refers
  to itself, so the guard keeps the walk out. The two that do grow are
  `mapdemo.wasm` **36,881 → 37,442** (+561, +1.5%) and `vlog.wasm`
  **46,252 → 48,609** (+2,357, +5.1%).
- **`membench`, native, 22 rows, minimum of ~300 samples.** Every row is within
  ±4% of its baseline except `keys() of a 1000-key map`, which goes
  **1.256 ms → 1.405 ms (+11.8%)** — the map releasing its 1,000 keys, which is
  the row that should move and the only one that did. (The medians on that run
  are noisy — several read 3x their own minimum — so the minimum is the honest
  floor.)
- **The keystroke budget is unmoved**: `lspbench` reads 9.7 ms on `vlog.vyrn`
  and 17.0 ms on `std/graphql.vyrn`, against the 97 ms budget.
- **`vyrn why --memory` over the corpus: 2,274 bindings not reclaimed of 3,724.**
  It went UP from M1's 2,127, and the reason is the hole: a record whose field a
  `consume` took stops being reclaimed, and RFC-0093 M1 landed 45 takes.

**`keysLoop` stays leaking, and M3 is not what closes it.** `for k in m.keys()`
walks a temporary, and a loop over a temporary releases nothing — the body may
take an element. The map releasing its own keys does not reach the snapshot. It
is still Phase 10a's `if let` row, applied to `for` and per element.

**The must-use link is a fourth thing, not this one.** M0 called it "U4 plus a
recursion in `Owned::linear_kind`". U4 closed in M2 and M3 did not touch
`linear_kind` — a container's obligation is still read off the container's own
type key. That recursion is M4, which M3 unblocks and does not do.

### M4 — the obligation recurses

`linear_kind` answers for a container what it answers for its element. Two lines
and one example: `examples/mustuse.vyrn` gains the pool.

**Gate.** The pool program prints `end orders` before `done` on all three
engines, and `examples/mustuse_abandoned.vyrn` gains the case where the pool is
abandoned, under `EXPECTED_CHECK_FAILURE`.

**M4 as landed.** The recursion is in, both examples have their pool, and
RFC-0086 M3's storage hole is closed. Parity is `124 checked, 11 skipped, 0
failed`, the workspace is `1,514 passed, 0 failed` over 52 binaries, the memory
suite holds its 15 rows with `keysLoop` the only leak, `genwasm` is `11 passed`,
and the instrument still reads `stores: 0`, `elem-store: 0`, `elem-return: 0`,
`returns: 0` over 210 files.

**The design got the recursion right and the price wrong.** The recursion itself
is what this section said it was: `linear_kind` asks its element, over `Array`,
`ArrayN`, `SmallArray`, `Map`, `Option` and `Result`, with `release_kind`'s own
`self_referring` guard in front of it and no second guard written. A type
PARAMETER answers `None` for free, because `resolve` leaves a `Param` alone and
answers `Unit` for an undeclared `Named` — so `map`, `filter`, `fold` and
`std/slots` never see the question. A **record field** was left alone: this RFC
says *container*, `impl MustUse for Order` is one line where an inferred
obligation would be a rule nobody wrote, and the corpus stores no must-use type
in a record. The corpus check found **no new refusal at all** — the ten files
that fail `vyrn check` are the ten that already did, all listed under
`EXPECTED_CHECK_FAILURE`.

**The price was two lines and it was three changes.** Each was found by the
gate, not by review, and each was a defect the recursion made visible rather
than one it introduced.

1. **`pool.push(t)` laundered the obligation.** The must-use scan calls any
   mention of the name a disposal, which was exact for a `Stream` — a stream has
   no field, no length and no indexing, so every mention IS a hand-on. A
   container has all three. Worse, the parser turns `pool.push(t)` into
   `pool = push(pool, t)`, so the very statement M4 exists to catch read as
   "handed on by name" and the obligation evaporated. **This RFC's own
   motivating program — `pool.push(begin("orders"))` with no discharge — still
   compiled after the recursion landed.** The fix is one arm: a write back INTO
   the binding is not a disposal, because the binding holds a value again when
   the statement ends.
2. **A nested declared `release` was never called, in any of the three
   engines.** M2 and M3 gave `Array`, `Map`, `SmallArray`, the record and the
   user enum an element walk, and both compiling backends' walks opened with
   *"a type that declares its own release keeps it"* and then returned without
   calling it. That is right at the top of a drop, where `emit_drop` and
   `emit_rel` have their own arms for it, and wrong for every place underneath
   one. So `impl Owned for Txn` never ran for a `Txn` inside anything, and the
   interpreter agreed by having no walk at all. The three engines were
   consistently wrong, which is why nothing observed it: parity compares them to
   each other. M4 is where it becomes intolerable — the compiler would demand a
   discharge and then not perform it.
3. **The interpreter needed a walk it did not have.** The two compiling backends
   walk a release by TYPE; the interpreter reclaims through the host and runs
   only what is observable. A declared `release` is ordinary Vyrn and prints, so
   it is observable, and the interpreter now walks the VALUE for one — the value
   carries what the walk needs, because `coerce` stamps a record with the name it
   crossed its boundary as. Order is the other two engines' order and is
   load-bearing: an array's elements go in index order, and a record's fields go
   in DECLARED order, which is not the order a `HashMap` yields. An unnamed
   record has no declaration to read that order out of, so its fields are left
   alone rather than released in an order the other two would not use. The walk
   is gated on the program declaring any `impl Owned` at all, so a program
   without one pays nothing.

**The diagnostic names both types, and that cost a field.** `Linear::Declared`
carries the type key that declared the row, because since M4 that is not always
the type asked about. Without it the note read *"`Array<Txn>` declares `impl
MustUse`"*, which names a row no program wrote. It reads:

```text
`pool` is a `Array<Txn>` and is never disposed
  note: `Txn` declares `impl MustUse` and a `Array<Txn>` holds one, so the
  container must be handed on by name — passed to a call, forwarded by
  returning it, or released with `drop pool`, which releases each element —
  on every path
```

**The price, measured.**

- **No wasm grows.** `domdemo.wasm` is 28,445 bytes, `fib.wasm` 1,522,
  `mapdemo.wasm` 37,442 and `vlog.wasm` 48,609 — every one byte-identical to M3.
  None of the four holds a declared release inside a container, and the walk
  emits a call only where one is.
- **`vyrn why --memory` over `examples/` and `std/`: 1,958 bindings not
  reclaimed of 4,336, against 1,958 of 4,335 before.** The one binding is the
  `pool` this milestone added, and it is reclaimed. M4 changes what a release
  DOES, not which bindings get one, so the census is the right number to be
  unmoved.

**What M4 does not close, and one of them is new.**

- **A read through the obliged name still launders it.** `pool.length`,
  `pool[i]` and `for t in pool` are all mentions, and the scan calls a mention a
  disposal. The Assign arm fixes the one spelling that is unambiguously not a
  hand-on; the rest needs a notion of POSITION the pass cannot have, because
  after the method-call desugar `pool.push(t)` and `finish(t)` are the same
  `Call` shape and only the CAPABILITY tells them apart — and the capability
  table has no row for a builtin. This is the same answer §16 gets: the mechanism
  exists one pass over, and reaching it is its own milestone.
- **A record field carries no obligation.** Deliberate, above.
- **A nested GENERIC declared release** reaches the walk through the ordinary
  call route in the textual backend and the direct one, so `impl<T> Owned for
  Slots<T>` inside a container is emitted rather than skipped. It is untested by
  the corpus, which stores no `Slots` in an `Array`.
- Everything under "What it does not close" stands unchanged.

---

## Rejected

- **Track the lend instead of refusing it.** A lend that travels through a store
  and through a container is an alias analysis. RFC-0089 exists because the
  previous alias analysis leaked when unsure and its failure mode was silent.
  A cleverer guesser is still a guesser. Refusing is smaller, and its failure
  mode is a diagnostic with two named fixes.
- **Widen RFC-0075's storage ban to every `MustUse` type.** Priced and refused in
  PR #100, and this RFC agrees. The ban is about representation. Widening it
  forbids `Array<Txn>`, which is the program symptom 2 is trying to make work.
- **Give `Array<T>` a declaration site.** The census's own framing, and it is not
  needed: built-in rows are seeded, not declared. A declaration is what a
  container with its own liveness rule needs, and `std/slots` is that container.
- **Refuse `m.keys()` rather than make it copy.** It would be the cheaper
  compiler change and the worse language. The interpreter already copies, so
  refusing would make the oracle the outlier.
- **An `Option` of a borrow, so a projection can be handed out safely.** A place
  that may not exist is a much larger feature, and RFC-0091 M5 already recorded
  the same conclusion from the `Slots` skip.

---

## The recommendation

**Build it, gated on M0.**

The reasons, in order of weight.

1. **The compiler already contains the right answer and applies it
   inconsistently.** Two spellings of one program get two verdicts. That is a
   defect whatever else is decided, and fixing it is three lines.
2. **It is the last thing standing between rule 4 and being true.** RFC-0089's
   status line says rule 4 is half landed. Every other rule in that RFC is
   enforced. Half a rule is the state the whole memory arc was run to leave
   behind.
3. **The reason Phase 4b gave for not refusing has expired**, and RFC-0087's open
   tail says the next job is to price the replacement. This is that pricing, with
   a design attached to it.
4. **U4 is genuinely in scope**, and the only thing in the way is three builtins,
   one of which is already a live divergence between the interpreter and the two
   compiling backends.

**The gate that changes the answer is M0's count.** If a linked measurement finds
more than 300 sites, this is a bigger migration than RFC-0089 M2 was, and the
right response is to ship M1's rule for the **return** path only — where the
count is small, the bug is a use-after-free rather than a leak, and symptom 1
closes on its own — and leave the store path, U4 and the obligation open with the
number written down.

That partial outcome is a real one. It closes one symptom of three and it does
not pretend to close the others.
