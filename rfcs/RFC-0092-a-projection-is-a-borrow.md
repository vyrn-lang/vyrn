# RFC-0092 — A Projection Is a Borrow

- **Status:** **M0 measured; M1 to M4 designed, not built.** The gate passes:
  92 sites over the linked corpus, against a 300 limit. See "M0 as measured". Supersedes nothing. Closes named gaps in RFC-0087 (§3,
  §14's remainder, U4), RFC-0089 rule 4 (the half its own status line says is
  missing) and RFC-0086 M3 (the recorded storage hole).
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
| `Type::Record`, `Type::Enum` | `None` (`own.rs:325`) | `Deep` | the rule |
| `Type::ArrayN` | `None` (`own.rs:325`) | `Deep` | the rule + `@list` |
| `Type::Array`, `Type::Map`, `Type::SmallArray` | buffer only (`own.rs:238-241`) | buffer + elements | the rule + all three constructors |
| `linear_kind` on a container | reads the container's own key | reads the element's too | the row above |

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
| `std/graphql`'s `gqlScanner(src)` | a record holding `bytes(src)` | `bytes` already allocates, so the record owns it — the lend is the ARGUMENT, and `src: consume String` states it |
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

### M2 — the three view constructors

`m.keys()`, `toArray(sa)` and `@list(fixed)` copy their elements where the
element type owns heap, and emit exactly what they emit today where it does not.

**Gate.** Three-way parity, and a benchmark row for `keys()` over a map of 1,000
sixteen-byte keys, before and after, in `examples/membench.vyrn`. The measurement
is kept whatever it says: this is a correctness repair, and the interpreter has
been paying it all along.

### M3 — the release rows

`Record`, `Enum` and `ArrayN` take `Deep`. `Array`, `Map` and `SmallArray` take a
per-element release. M3 decides the inline-loop against the per-type function by
measuring both on `domdemo.wasm` and `fib.wasm`.

**Gate.** `elementLeak` flips to steady — the row RFC-0087 has carried since
Phase 5, asserting its own leak so the day it stops is a failure and not a
silence. And the emitted size of `domdemo.wasm` grows by less than the String
header did (1,664 bytes), or the per-type function ships instead.

### M4 — the obligation recurses

`linear_kind` answers for a container what it answers for its element. Two lines
and one example: `examples/mustuse.vyrn` gains the pool.

**Gate.** The pool program prints `end orders` before `done` on all three
engines, and `examples/mustuse_abandoned.vyrn` gains the case where the pool is
abandoned, under `EXPECTED_CHECK_FAILURE`.

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
