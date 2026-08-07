# RFC-0090 — One Model: Values, and Nothing Else

- **Status:** **Implemented.** M1 landed in Phases 8a and 8b, M3 in 8c and 8d,
  M4 in 8e; M2 is RFC-0089 M2 and landed there. A delta on RFC-0089 — accepted
  with it. It removed RFC-0089's rule 5 and replaced it, and `Ref`, `cell`,
  `get`, `set` and `release` no longer exist in any engine. Read the four
  "as landed" sections at the end: each corrects something this RFC claimed.
- **Depends on:** RFC-0089 (rules 1–4 stand), RFC-0079 (failure is a value),
  RFC-0082 (containers are Vyrn), RFC-0078 (the runtime is Vyrn)
- **Premise:** RFC-0089 asked "can the model be better than inference?" This
  asks "can it be better than a hybrid?"

---

## The residue RFC-0089 leaves

RFC-0089 rules 1–4 make everything compile-time — except rule 5, which keeps
`Ref<T>` as "the one runtime mechanism": a hidden global slab of 65536 cells, a
generation compare on every access, and one trap (`error: reference used after
release`) that arrives with no location (U5), at run time, in the one corner of
the language a user understands least.

The session's own exploration concluded: compile-time liveness *in general*
requires regions in types (written or inferred) or giving up first-class
aliasing. RFC-0089 chose to keep the runtime island. This RFC chooses the other
branch — and observes that Vyrn already lives on it.

## The observation

What does the corpus actually use `Ref<T>` for? §5.3 measured it: `genref`,
`freelist`, `linkedlist`, `tree` — **identity inside a collection of nodes**.
`freelist.vyrn` builds a linked list *by storing indices in a slab and running
generation counters over it*. The example reimplements Path B on top of Path B.

That is the tell. The need is not "a pointer with a check." The need is
**stable identity for a value that lives in a container you own**. And a handle
into an owned container is:

- a plain value — copies freely, no conventions, no move errors (it owns no heap)
- compile-time memory-safe — the *storage* is the container's, owned by rules 1–4
- dynamically *live* or not — which is a *domain* fact, not a memory fact

## The rule

> **Delete Path B. Identity is a handle into a container you own. A dead handle
> is a value-level miss, not a memory error.**

`cell` / `get` / `set` / `release`, the slab, the generation runtime in three
engines, `ReleaseRef`, `fresh_refs`, §5.3's elision pass — all removed. In their
place, one std container written in Vyrn over `Array` (RFC-0082's thesis, and
its M2 port unblocks because RFC-0089's `read`/`modify` conventions remove the
"any call escapes its receiver" wall that stopped it):

```vyrn
let mut people: Slots<Person> = [:slots:]        // or just Slots.new()
let h = people.insert(Person { name: "ada" })    // h: Handle<Person> — a plain value
people[h].name = "lovelace"                       // traps on a dead handle, like a[i] on OOB
match people.get(h) { Some(p) => ..., None => ... }  // liveness as a value (RFC-0079)
people.remove(h)                                  // the slot's generation bumps
drop people                                       // ALL nodes reclaimed — compile-time
```

Two spellings, both already canonical in the language: `people[h]` joins the
bounds-trap family (`a[i]` out of range), and `.get(h)` returns `Option<T>` so
staleness can be *handled* — which the old `Ref` could never offer: its only
behavior was a trap.

## What this buys over RFC-0089 alone

| | RFC-0089 | with RFC-0090 |
|---|---|---|
| memory safety | compile-time except `Ref` | **compile-time, no exception** |
| use-after-free class | a runtime trap | **does not exist** — a dead handle is `None` |
| U5 (trap has no location) | still open for `Ref` | gone — nothing to locate |
| the 65536 global slab | kept, hidden, process-wide | gone; capacity is your container's, reclaimed on drop |
| generation checks (P5's 94%) | in the language runtime, unmeasured | in std Vyrn source — visible, benchmarkable, optimizable, skippable (`[h]` vs `.get`) |
| the runtime surface | slab code in three engines | **zero** — every engine's memory runtime is: malloc, free, memcpy |
| data-race freedom | mostly | **total**: values move or are read-borrowed; all mutation is exclusive; there is no shared mutable anything to race on |
| cyclic structures | via `Ref` | via handles — the same graphs `freelist`/`tree` already build, minus the hidden runtime |

And one law becomes statable, and testable, on the whole language:

> **No hidden allocation, no hidden copy, no hidden check.** Heap is copied only
> at `.copy()`, shared only through a container you can name, and checked only
> where the source shows an index or a handle.

That is stronger than Rust offers (no lifetime annotations), stronger than Vale
(no generation runtime), and honest about the one dynamic residue: liveness of
an identity is data, and data is checked where you read it — same as every
`Option` the language already returns.

## What it costs

- **Staleness stops being enforced.** Old `Ref` *trapped* a stale access; a
  handle miss can be *ignored* via `.get` → `None` handling. This is the trade
  RFC-0079 already made for every other failure: a value you can handle beats a
  trap you cannot. `[h]` keeps the trapping spelling for "this must be alive."
- **A handle does not free its target on scope exit.** Liveness is manual
  (`remove`) or wholesale (`drop people`). That is also true of today's `Ref`
  (release is explicit or block-exit) — but the census habit of "block-exit
  auto-release" for cells goes away with the cells.
- **Migration:** `genref`, `freelist`, `linkedlist`, `tree`, `autorelease`,
  `slottable`, the stream cursor internals (RFC-0075 M2c keeps its own slab
  logic inside std, now in Vyrn). The stream case is the hardest and lands last.
- **Two theses get audited by construction:** RFC-0078 (runtime is Vyrn) and
  RFC-0082 (containers are Vyrn) both claimed this direction; this RFC is the
  first change that *requires* them to be true.

## Milestones

- **M0** — unchanged: the instrument (RFC-0088 M1 / RFC-0089 M0).
- **M1** — `std/slots` in Vyrn over `Array`, alongside `Ref` — **LANDED
  (Phase 8a, completed in Phase 8b).** See "M1 as landed" at the end of this
  RFC: the benchmark says 2.02x, and reclamation was three refusals away.
  Phase 8b closed all three, and a `Slots<String>` is now flat across turns.
- **M2** — RFC-0089 M2 (conventions) lands; the `Slots` port stops being
  blocked by escape-on-call.
- **M3** — streams re-host their cursor on `std/slots` — **LANDED (Phase 8c),
  and the cost it reported is mostly recovered (Phase 8d).** See "M3 as landed"
  and "M3's cost, measured again" at the end, and RFC-0075 "As landed — M3".
  The compiler carries no slab logic for streams any more. 8c measured 2.5x per
  element and blamed the check; the check was not the reason and the check is
  still there. 8d halved the two element rows by making a trap cost one cold
  call instead of three inline ones.
- **M4** — `cell`/`get`/`set`/`release` and the slab are deleted from all three
  engines — **LANDED (Phase 8e).** See "M4 as landed" at the end. The language's
  memory runtime is malloc, free and memcpy, checked rather than claimed.

## Measured predictions

Everything below ran on 2026-08-05, native, same machine, release compiler. The
simulated columns are today's compiler running the code shape the new model
would produce — not estimates.

| claim | today | under the new model (simulated) | factor |
|---|---|---|---|
| RFC-0090: a Vyrn-source slab replaces the built-in (2M insert/set/get/get/remove) | `cell`/`set`/`get`/`release`: **0.428 s** | handles over two `Array<Int64>`: **0.230 s** | **1.86× faster** — the built-in boxes every payload through `malloc`/`free`; arrays stay flat |
| RFC-0089 rule 4: §14's fallible style (2M × `Option<String>`, 36-byte payload) | **88.6 MB** peak, linear in calls | **4.1 MB** peak, flat | **21× less memory** at this N; unbounded factor as N grows |
| RFC-0089 M1: String header (200k `byteLength` reads of a 64 KB string) | **0.551 s** — each read is a `strlen` | **0.102 s** — length is a word | **5.4×** whole-program; the read itself ~2.2 µs → ~0 |
| RFC-0089 (from RFC-0087 P1): module-state accumulator, n = 160k | **4.92 s, 12.2 GB** | **0.095 s**, one buffer | **52× faster, ~3 000× less memory** |

**The error surface, counted.** The whole corpus — 33,728 lines of Vyrn across
`examples/` and `std/` — contains:

- **21** returns of a parameter from a heap-returning function (the rule 3
  errors; each fix is `consume` or `.copy()`),
- **5** bare aliases `let y = x` (candidate rule 1 errors, before filtering for
  heap types),
- 214 bare-variable returns in heap-returning functions total, of which the
  other ~193 return a local owner — a legal move, no change.

So the migration the "move errors" objection worries about is **~26 sites in
34k lines — 0.08%**. RFC-0004 §2's error-surface fear, priced: for every site
that gains a compile error, the same corpus has been paying §14's 21× memory
and P1's 52× time silently.

**What M4 deletes, counted:** the LLVM cell prelude (158 lines of IR), the
direct backend's `cell_runtime` (212 lines), the interpreter's slab (~200
lines), and the inference half of `own.rs` (most of 1,479 lines — the
`Owned`-table lookup stays). Three engine implementations of one hidden
runtime, replaced by one std module in Vyrn that measured *faster*.

One honest minus from the same runs: the owned §14 shape pays ~20% wall clock
over the leak at small N (0.30 s vs 0.25 s) — a `free` per iteration is not
gratis. It buys the flat line; P1 shows the leak losing the time race too once
the working set outgrows cache.

## Downsides not yet stated

Recorded here so acceptance is informed, not sold.

- **Cross-container handle confusion.** A `Handle<Person>` from container A,
  used on container B of the same element type, type-checks — and the
  generation compare can pass by coincidence. The old global slab could not
  confuse containers because there was only one. Mitigation: a container
  carries an identity word, a handle carries a copy, `get` compares both — one
  extra compare, still in std source. Type-level branding is the zero-cost
  version and needs cheap newtypes.
- **Staleness bugs go quiet.** `Ref` trapped loudly; `.get` returns a `None` a
  program may silently swallow. `[h]` keeps the loud spelling, but the choice
  is now the programmer's, and the lazy choice is the quiet one.
- **Linear becomes affine.** Today a `Stream` *must* be consumed — dropping it
  silently is an error. Under ownership, scope exit releases it implicitly.
  That deletes the "forgot to consume" diagnostic in exchange for RAII. For
  streams this is arguably an improvement; for a resource where *forgetting is
  the bug* (a transaction, a reply obligation), an opt-in `must-use` row on the
  `Owned` table keeps RFC-0075's guarantee available per type.
- **Conventions are API design burden.** An author must choose
  `read`/`modify`/`consume` per parameter the way a Rust author chooses
  `&`/`&mut`/`T`. The default (`read`) covers most signatures, and the checker
  can say "this body stores `x`; take `consume x`" — but the burden exists and
  lands on library authors.
- **Two `modify` borrows of one variable must be refused** at the call site
  (`f(modify a, modify a)`). Syntactic, cheap, but it is a rule users will
  meet. Two handles into the same container are fine — handles are values, not
  borrows, which is exactly why the handle model composes where borrows would
  fight.
- **The String triple changes the extern ABI.** A `(ptr, len, cap)` String
  crosses to JS differently than a NUL-terminated pointer; `wasi-min.js` and
  the shim convert at the boundary. Churn, priced once, and the boundary
  already converts in both directions today.
- **Iteration needs a rule.** `for x in xs` over an `Array<String>`: copying
  every element violates the no-hidden-copy law; moving destroys the array.
  The rule must be: the loop variable is a `read` borrow (a `modify` form can
  come later). This falls out of the conventions but has to be said, and it
  changes today's copy semantics for heap elements.

## UX — what the new model makes possible

The model fits in one sentence, so the tooling can too:

- **Every move error is a menu, not a wall.** The diagnostic names its fixes:
  `consume` the parameter, `.copy()` the value, or restructure — with spans on
  both the move and the later use. The diagnostics infrastructure (RFC-0006,
  RFC-0009) already carries structured, multi-error output; movecheck's
  messages join it.
- **`vyrn fix`.** The migration is 26 mechanical sites; the checker knows the
  fix for each. A `--fix` mode applies them. The same mode serves every future
  user migrating code into the model.
- **The LSP shows ownership instead of explaining it.** Inlay hint at a move
  site; hover on a binding says "owned here, released line N" or "moved line
  N"; a semantic-token modifier marks the *last use* of every owning binding.
  This is U1's printer with the model made simple enough to print.
- **Subject-first was already convention-shaped.** `sq.push(v)` is
  `modify self` + `read v`. `t.join()` is `consume self`. The surface the
  language migrated to over ten RFCs already *reads* as conventions — the
  checker starts enforcing what the style already says. Most signatures need
  zero annotation because `read` is the default and receivers infer from
  mutation.
- **Progressive disclosure holds.** A program of scalars, records of scalars
  and `Option<Int64>` never sees a move, a convention or a `copy`. The model
  appears exactly at the first owning aggregate — which is also the first
  place today's model starts silently leaking.

## The floor

Can *this* be better? The remaining dynamic act is one integer compare where a
program reads a handle it chose to keep. Removing it requires proving liveness
of arbitrary identities at compile time — which is regions-in-types or linear
handles, both priced and rejected as annotation surface. So this is the floor
for a language that refuses lifetime annotations: **memory is fully static;
identity is data.** Anything past it buys no safety — only the deletion of a
compare the source can already see.

---

## M1 as landed

Shipped in Phase 8a. `std/slots` is `Slots<T>` and `Handle<T>` over `Array`,
with `insert`, `remove`, `fetch`, `alive`, `count`, `capacity` and `handles`,
plus `impl Index` for `s[h]` and `s[h] = v`, `impl Iterate` for `for x in s`,
and `impl Copy`. `genref`, `freelist`, `linkedlist`, `tree`, `autorelease` and
`slottable` all run on it, on three engines, and `cell`/`get`/`set`/`release`
are untouched beside it.

The reader is `get` now. M1 spelled it `fetch` because Path B held the name;
M4 deleted Path B and a follow-through took the name back.

### The gate, measured

`vyrn bench examples/membench.vyrn`, native, release build:

| row | median |
|---|---|
| `cell` insert/set/get/get/remove, 1000 times | **18.29 µs** |
| `std/slots` insert/set/get/get/remove, 1000 times | **9.06 µs** |
| handles hand-inlined over two `Array<Int64>`, 1000 times | 0.64 µs |

**2.02x in favour of the replacement**, against the 1.86x this RFC predicted.

The prediction was made on the third row's shape, and that row's own comment
already disowns it: the handle is made and used in one block, so the optimizer
proves the four generation checks true and deletes them, while `cell` cannot
fold checks that happen inside opaque runtime calls. The second row is the
library as a program imports it — real calls, a projection inlined per access,
an identity compare per access, and two arrays more than the hand-written
version carries. It still wins by more than the estimate, for the reason this
RFC gave: `cell` boxes every payload through `malloc` and returns it through
`free`, and two flat arrays do neither.

So the gate is clear on speed. It is not clear on reclamation — see below.

### What this RFC got wrong

**"`freelist.vyrn` builds a linked list by storing indices in a slab and running
generation counters over it."** It does not, and never did. `freelist` used
`cell`/`get`/`drop` directly. The file that reimplemented Path B on top of Path
B is `slottable.vyrn`, which this RFC lists separately as a migration target.
The observation the RFC rests on is true — the corpus was already writing the
container by hand — but it is one file's evidence, not four.

**`.get(h)` cannot be spelled.** `get` is reserved by Path B (RFC-0004), and so
are `cell`, `set` and `release`. A protocol method named `get` does not help:
`x.get(h)` resolves to the builtin before any dispatch and reports "`get` takes
1 argument, got 2". `slottable.vyrn` recorded this collision two phases ago as a
naming problem; it is a naming problem that survives into the module meant to
replace the thing reserving the name. The reading half is `fetch` until M4.

**The API cannot be subject-first.** RFC-0091 M1-as-landed records that an impl
method's receiver is bare `self` and IS `read`. `insert` and `remove` mutate, so
neither can be a method on any protocol, and `Slots` is free functions
(`insert(s, v)`) rather than `s.insert(v)`. Both spellings in this RFC's own
example — `people.insert(..)` and `people.remove(h)` — are unavailable. `s[h]`,
`s[h] = v` and `for x in s` DO work, because those three are what a `place`
member reaches.

**`drop people` did not reclaim anything, and Phase 8b is why it does now.**
Phase 8a found three refusals between a slab and its buffers, all of them the
memory model's rather than the container's. Phase 8b closed all three.

1. `own::fate` refused a declared `release` for a `mut` binding, and a slab is
   always `mut` because `insert` takes `modify self`. The reason written at the
   refusal was an engine disagreement: the interpreter released what the `let`
   captured and the two compiling backends release the slot's final value — the
   same program for a `free`, two programs for a `release` that can print. The
   interpreter reads the slot now, so all three release the same value.
2. A generic `impl Owned` flattened its `release` to a generic function, and the
   drop site emitted that name with no type arguments on it — a symbol nothing
   defines. The row is recorded like any other now, keyed by the type
   CONSTRUCTOR, and each drop site solves the arguments from the binding's own
   type and asks for that instance. It is the route a written call already took,
   and the route the direct backend already took for a declared release.
3. Census U4 is open **for a container that declares what it owns**. A built-in
   `Array<T>` still releases no element and should not: an array cannot say
   whether it owns its elements or views somebody else's, and `m.keys()` is the
   view that would be freed twice. `Slots` can say. Its release walks every slot
   from 0 to `vals.length` and gives the payload back — every slot holds exactly
   one payload the slab owns, because `insert` takes `consume T` and the only
   other writer is a store, which releases what it replaced.

`drop v` where `v: T` also had to become legal. A generic body is checked once
and lowered once per instantiation, so the instance decides: a `free` in a
`Slots<String>`, no instruction at all in a `Slots<Int64>`. That is this plan's
own rule — conventions checked per monomorphized instance — reaching `drop`.

Measured on the Node and `wasi-min.js` harness the memory suite uses, 500 calls
against 2000: **720,896 -> 2,424,832 bytes before, 131,072 -> 131,072 after.**
Flat, at the two pages the module starts with. **M4 may now delete Path B on
this ground** — the replacement reclaims what `cell`'s slab reclaimed.

**`elementLeak` (U4) still does not flip, and that is the correct answer.** It
is a bare `Array<String>`, and the array is the container that cannot say. The
suite gained a `slotsContainer` row instead: the same heap element in a
container that declares its ownership, steady. Twelve rows now — nine steady,
three leaking. `optionString` and `lambdaLoop` are blocked elsewhere and did not
move.

**Releasing the container is faster than leaking it.** A thousand short-lived
slabs of sixteen elements each cost 348 µs with the release and 481 µs without
it, same machine, same session (`std/slots build and release` in
`examples/membench.vyrn`). Handing the buffers back lets the allocator hand the
same blocks out again; leaking makes it ask the system for more. The gate row is
unchanged at 2.0x.

### What the migrated corpus proves now

- **`slottable`** was the container; it now imports one. Every question the
  hand-written table answered is asked of the library and answered the same way,
  byte for byte. It gained a test the hand-written version could not pass: two
  containers both issue slot 0 at generation 1, and a handle from one is dead on
  the other.
- **`freelist`** proved reuse by NOT trapping — Path B's slab was fixed at
  65,536, so exhaustion was reachable and reaching it was the failure. A `Slots`
  grows, so "it finished" proves nothing. It now PRINTS the table size after
  100,000 inserts: five slots, the size of one list. A number held on three
  engines, where the old file had an absent trap.
- **`autorelease`** measured the ownership INFERENCE: a million cells through
  65,536 slots, and the proof was again a trap that did not happen. Nothing
  about a handle is inferred, so the file now prints the high-water mark: one
  slot. The count came down to 100,000 because the interpreter runs Vyrn where
  it used to run four builtins, and a million turns took 75 seconds against 0.8.
- **`genref`, `linkedlist`, `tree`** are the same programs with the storage
  named. `Handle<Node>` is fixed size, so a record still refers to its own type;
  an `Option` still ends a list; the aliasing tour still aliases. `genref` gained
  one printed line — `alive(nums, alias)` after the removal — because the old
  file claimed every copy of a released `Ref` was stale and printed nothing to
  show it.
- **`examples/slots.vyrn`** is new: a heap payload, a write through a handle
  into a field, iteration, a container copy, and the trap. It ends in the trap
  on purpose, so the corpus holds all three engines to one wording and one exit
  code.

### What a real container found missing

`Slots` is the first generic container in the corpus, and Phase 7's dogfood
(`Window`, `Slice`, `Ring`) was concrete. Seven gaps, each fixed here:

1. **The checker read a projection's return type literally at two of three
   sites.** `c[k] = v` and `for x in c` took `place atSet` and `place nth` at
   their word, so a projection declared `-> T` answered `T`. `place at` already
   solved the impl head; the other two do now.
2. **A store through a user container demanded an `Int64` key.** `Index`
   declares `type Key` and `place atSet` names it, and nothing read it.
3. **A record literal inferred its parameters from its field values alone.** An
   empty array literal for a field declared `Array<T>` says nothing, and a
   parameter no field mentions says nothing at all — which is exactly a handle's
   branding parameter.
4. **A generic call inferred its type arguments from its arguments alone**, in
   the checker and in BOTH compiling backends, so `newSlots()` had nothing to
   read.
5. **The textual backend passed a `modify` argument to a generic function by
   value while the definition took a pointer.** A native segfault in three lines
   of Vyrn. No corpus function was ever both generic and `modify`.
6. **A `place` body's string literals never reached the textual backend's
   pool.** A projection is inlined rather than flattened into
   `program.functions`, so the walk that pools literals never saw one — and
   `panic("..")` inside `place at` is the whole point of a trapping index.
7. **The direct backend could not name a user container's element type in a
   branch**, so `match o { Some(h) => s[h].value .. }` refused to lower.

Two more were recorded rather than fixed. The first — a generic `impl Owned`
with no monomorphized release — is closed by Phase 8b, above. The second stands:
`insert(s, i)` on a `Slots<Int64>` is refused because `consume T` is
conservative for a `T` movecheck cannot resolve, so the caller writes
`let v = i` first.

**RFC-0091's stated motivation for `Iterate` is not achievable as it landed.**
It says "`Slots` skips dead slots by implementing it". A `place nth` cannot
skip: it maps a dense position to a place, it has no cursor to advance and no
branch to yield from, and the rules a projection obeys forbid both. `Slots`
skips by keeping a dense array of live slots and a map back from a slot to its
place in it, so the skipping happens before the projection runs. That is the
standard slot-map layout, it is what makes `remove` O(1), and it costs the two
arrays the benchmark above already pays for.

## M3 as landed — the stream cursor

The hardest migration on this RFC's own list, and it landed as the list
predicted: a stream's cursor is a slot in a `Slots<CursorCell>` that lives in
`std/stream`, and the slab logic is Vyrn anyone can read. RFC-0075's
"As landed — M3, the cursor re-host" is the full account; four things belong
here because they are this RFC's claims rather than that one's.

**A Vyrn slab is not faster everywhere.** M1's gate row says `std/slots` beats
`cell` by 2.02× on insert/set/get/get/remove. The stream cursor is the shape
where it loses: about 2.5× slower per element, measured in `examples/membench`'s
three new rows. Three reasons, and only the first is about `Slots`:

1. **Path B's generation check was elidable and a `Slots` read is not.** RFC-0004
   §5.3 proves a cursor is never aliased and drops the check; nothing proves the
   equivalent about an `Array` index behind a handle.
2. `cells` is module state behind a global, where the slab was a fixed array at a
   known address.
3. A stream's registered step is an adapter — it turns the two cursor words into
   a `Cursor` and handles the closing call — so every element costs one more
   dispatched call.

Only (1) is a property of the model. (2) and (3) are the price of the cursor
being std's rather than the compiler's, and (3) would go if a step took the raw
words, at the cost of every producer in every program spelling them.

**Reason (1) is wrong, and Phase 8d measured why.** Read the paragraph below it
instead. §5.3 elided 3 of 48 sites and says in writing which 45 it did not:
"`genref`, `freelist`, `linkedlist`, `tree` **and the three stream examples**".
A stream's `get(c)` took `c` as a parameter, and `fresh_refs_in` only ever sees a
`let c = cell(..)` in the same block. So a stream under Path B paid its
generation check at every access, exactly as it does now.

## M3's cost, measured again — Phase 8d

The two element rows recovered by about half, and none of it was elision.

| row | before 8c | 8c | 8d | 8d, guard removed |
|---|---|---|---|---|
| unfold + take, 1000 elements | 2.60 µs | 6.54 µs | **3.32 µs** | 2.97 µs |
| map over unfold + take, 1000 | 4.02 µs | 9.20 µs | **4.62 µs** | 4.10 µs |
| open and close, 1000 cycles | 18.61 µs | 25.86 µs | **25.31 µs** | 24.73 µs |

**What cost the time was the trap tail, not the check.** Every trap and every
`panic` emitted three calls INLINE at its site: `@__vyrn_stderr`, an `fputs` or a
variadic `fprintf`, and `exit`. LLVM's inliner reads cost before it reads
probability, and three calls is about what a small function is allowed to cost in
total. So `place at`'s one guard — a branch no program takes — made `cursorGet`,
`cursorSet`, `srcOf` and `takeCursor` too expensive to inline into the step. The
emitted assembly says so: four surviving calls per element before, none after.

The trap tail is now one `noreturn cold` call to a shared function. **14,935 trap
sites across the 121-example corpus**, three calls each and one now. Nothing
about what is printed moved, which is why parity is byte-identical.

**The check is elidable in principle and there is no customer.** Three
measurements, in the order that settles it:

1. A guard reading no memory at all (`h.slot < 0 || h.gen < 0`) cost the same as
   the real four-condition guard: 6.33 µs against 6.37. So the cost was never the
   `gens` load, and 8c's cheaper `cursorGet` was right to measure no difference.
2. After 8d, removing the guard entirely buys 8% and 11% on the two element rows.
   That is the whole remaining prize.
3. On `slotsChurn` — the ONE corpus shape a §5.3-style proof could reach, where
   `insert` and `s[h]` sit in one block over an unaliased local — removing the
   guard changes nothing at all: 9.29 µs against 9.34. LLVM already folds it.

So a frontend elision pass would delete the checks that are already free and
would not reach the checks that cost. It does not reach `cursorGet` for a reason
no pass can fix: `Cursor` is an exported record, any program can spell one, and
the handle is built inside the accessor from a parameter. The guard there is
doing real work, which is §5.3's own conclusion about the 94%.

**What remains after 8d, for M4 to price.** The two element rows sit 1.28x and
1.15x over Path B. A guard-free `Slots` sits at 1.14x and 1.02x, so most of the
residue is reasons (2) and (3) above and not the check. The open-and-close row is
still 1.36x, and neither the check nor inlining touches it: it is a `Slots`
insert and remove against a fixed slab, plus the `malloc`/`free` of the step's
capture block that Path B never did. That is the honest price of the re-host.

**The wasm output did not move by one byte.** A wasm build already ran at a size
level that outlines cold tails, so it had made this change for itself; native at
`-O2` had not. Native binaries grow about 0.3%, which is the extra inlining.

**M4 may now delete Path B.** Nothing in the compiler reaches the cell slab for a
stream. What still does is listed in the PR body for this phase, and it is
exactly what the M4 bullet already named plus the two corpus sites that write
`cell`/`get`/`set`/`release` on purpose (`examples/copy.vyrn`, and the
`cellChurn` baseline row in `examples/membench.vyrn` — the row M1's 2.02× is
measured against, which retires with the thing it measures).

**"The slab logic inside std, now in Vyrn" was right about the slab and wrong
about the release.** This RFC's migration line assumed the cursor could move and
nothing else would. It could not: a release is type-erased in the runtime, and a
slab in Vyrn cannot be named from there. So the release had to become a CALL into
the stream's own step, which made the compiler's one `__vyrn_stream_close` into
one function per element type. That is more code, not less — and it bought the
thing the walk could not have: a wrapper's release is ordinary Vyrn, so
`movecheck` proves the chain releases exactly once instead of a hand-written loop
promising to.

**A stream is where the handle model's ergonomics show.** M1's `Handle` carries a
container identity word, and a stream carries two words, so the identity is
recovered from `cells.owner` rather than stored. That works because there is
exactly one cursor container in a program. It is the first place the model needed
a container it could assume, and it is worth noticing before anything else copies
the trick.

---

## M4 as landed — Path B is deleted

**1,714 lines of code removed, 186 added, across 24 files** (the RFC edits are
separate: +279 / -72). Path B is gone from every engine: no `cell`, `get`, `set`
or `release`, no `Type::Ref`, no `Val::Ref`, no `DropKind::ReleaseRef`, no
`Rel::Cell`, no `fresh_refs`, no §5.3 elision pass.
The LLVM `CELL_RUNTIME` prelude, `direct.rs`'s `cell_runtime` and its four
`CELLS` constants, and the interpreter's `CellSlot`/`cells`/`free` and four
`cell_*` methods are all deleted. RFC-0004 gained §5.4, which records the
reversal and where the evidence for it is.

### The runtime, as a checked fact

This RFC's thesis was "the language's runtime memory surface after this: malloc,
free, memcpy". It was checked against the emitted output rather than asserted, and
**the check found one more primitive and one surviving mechanism. Both are named
here, because a slogan that needs a footnote is better written with the footnote
in it.**

The **native/LLVM** engine's emitted IR declares exactly these memory primitives:
`@__vyrn_malloc`, `@__vyrn_realloc`, `@free`, `@llvm.memcpy`, and `@strcpy` /
`@strcat`. So the honest list is **malloc, realloc, free and memcpy**. `realloc`
is not an oversight and not a fifth mechanism: it is how an `Array` and a `String`
grow in place, and RFC-0089 M1a's header is what makes an in-place grow legal.
`strcpy`/`strcat` copy; they allocate nothing.

Two wrappers sit above `malloc`/`free` and neither is a mechanism either:
`@__vyrn_str_free` reads a String's header and returns on `cap == 0` (a
data-segment literal), and `@__vyrn_stream_box`/`_unbox` is one `malloc` plus one
`free` with a magic word between them (M3, above).

**`region` survives, and it should.** It is RFC-0004 §4's Path A arena and this
RFC never proposed removing it: a chain of `malloc`ed blocks, freed together at
the block's end, over a thread-local stack of 64 chain heads. Two lifetimes, one
allocator. What went is the second ALLOCATOR, not the second lifetime.

The **direct wasm** engine has a bump allocator over `memory.grow` and a
`memory.copy`. It has no `realloc`: a grow is a `malloc` plus a `memory.copy`. Its
`free` is a no-op, which is a property of a bump allocator rather than of the
memory model.

The **interpreter** holds Rust values and reclaims them the way Rust does.

**Nothing in any engine now allocates from a fixed table, checks a generation
counter, or recycles a slot** — except `std/slots.vyrn`, which is ordinary Vyrn
that a reader can open, and which is the point. The one remaining static table in
the emitted output is `@__vyrn_utf8d`, the UTF-8 validator's DFA, and that is
data.

### Binary size

The slab cost every module that never used it. Two measurements, before and
after, same compiler build:

| artefact | before | after | delta |
|---|---|---|---|
| `fib.wasm` (direct backend) | 1,590 B | 1,490 B | **-100 B** |
| hello-world `.wasm` | 1,461 B | 1,361 B | **-100 B** |
| `fib.ll` (LLVM IR) | 138,508 B | 135,957 B | **-2,551 B** |
| hello-world `.ll` | 138,032 B | 135,481 B | **-2,551 B** |
| `fib.exe` (native, linked) | 181,248 B | 181,248 B | 0 |
| hello-world `.exe` | 181,760 B | 181,248 B | -512 B |

**The direct backend's 100 bytes are the whole story there.** `fib` allocates
nothing and never named a cell, and it carried four cell functions anyway. The
1 MiB slab does NOT appear: it was one lazy `malloc` at first use, so a module
that never allocated one never paid for it in bytes — the comment in `direct.rs`
said so and was right.

**The LLVM prelude's four `[65536 x i64]` arrays do not show in the linked
binary, and that is not a null result.** They were `zeroinitializer` globals, so
they lived in `.bss` and cost file bytes nowhere; the linker then dropped them as
unreferenced. What they cost was 2,551 bytes of IR in every `.ll` this compiler
emits, plus the compile time to parse and discard it. The one native binary that
moved dropped 512 bytes, which is one section-alignment step.

### The two corpus sites

- **`examples/copy.vyrn`** — `handles()` pinned that a `Ref<T>` copies as the two
  words it is. It now pins the same statement about a `Handle<T>` from
  `std/slots`: a handle copies as the plain value it is, a store through the copy
  is visible through the original, and the printed output is byte-identical.
  `examples/slots.vyrn` already pinned the other half — that a whole `Slots`
  copies as a SECOND container with a fresh identity.
- **`examples/membench.vyrn`** — `cellChurn` is retired, and **its numbers are
  not.** 18.29 µs against `std/slots`' 9.06 µs, 2.02x, is in "M1 as landed" above
  and is now also quoted in the doc comment on `slabChurn`, which is where a
  reader of the benchmark file will look for it. RFC-0004 §5.4 carries it a third
  time, as the evidence for the reversal.

### The primitive census, 94 to 90

`vyrn-frontend/tests/primitives.rs` holds one row per builtin the interpreter
implements in Rust, with the category and the reason. Four rows left: `cell`,
`get`, `set` and `release`, all `Memory`, all "the slot table".

**94 to 90 is the largest single drop the census has recorded.** The only other
row that ever left was `afree`, alone, and it left for having no callers. These
four left because the mechanism did.

Nothing replaced them. `std/slots` is a library a program imports, not a route
the loader installs, so this is not a builtin moving into Vyrn — it is four
builtins ceasing to exist and a library appearing beside them. **The test named
the change before anything else did**, which is the second time it has done that.

### What the record got wrong

- **The line count.** This RFC estimated 158 (LLVM prelude) + 212 (`cell_runtime`)
  + ~200 (interpreter) + `own.rs`'s elision pass — about 570 to 770. The real
  figure is **1,714 deleted lines of code**, more than double. The estimate
  counted the three runtimes and missed everything around them: the `Type::Ref`
  arms across fourteen files, the emission sites in both backends, and above all
  the TESTS. Twenty-two unit tests in the checker and the interpreter existed to
  hold the mechanism up, plus one parity pin and four census rows. A deletion is
  wider than the thing being deleted, every time.

  The four biggest files, deleted lines: `direct.rs` 389, `own.rs` 386,
  `lib.rs` 283, `interp.rs` 280.

- **`Leak::Mutable` was Path B's, not `mut`'s.** `own.rs` refused a declared
  release for a `mut` binding, and Phase 8b narrowed that refusal to cells alone —
  because `fresh_refs_in` read the same `droppable` set and a re-pointable binding
  would have elided a check that could fail. Deleting Path B deleted the last
  reason, so the refusal, the `Leak` variant and the `mutable` parameter of
  `own::fate` all went with it. Nothing in `own.rs` asks whether a binding is `mut`
  any more.

- **`Gone::Aliased` looked dead and is not.** It fires when a whole place did not
  move, which needs a type with a release kind that rule 1 leaves alone. `Ref<T>`
  was the built-in case and the only one, so the reason "aliased by another
  binding" stopped appearing in `vyrn why --memory` over the whole corpus. It is
  still reachable, through `impl Owned for T` on a type that holds no heap of its
  own — census U4's shape. The memory suite's `why` fixture now carries exactly
  that, which is a better test than the cell was: it exercises the live path
  instead of the built-in one.

- **`movecheck::sinks` was three entries and is one.** It lists the builtins that
  STORE an argument somewhere it outlives the call, and two of the three were
  `("set", 1)` and `("cell", 0)`. What is left is `push`. The `release(c)` arm in
  the same file — `drop` spelled as a call — went with them.

- **Four names came back to the user.** `cell`, `get`, `set` and `release` left
  `RESERVED`, so `fn get(..)` compiles now. `std/stream` renamed its cursor
  accessors to `cursorGet`/`cursorSet` in Phase 8c *because* Path B held the short
  names (see RFC-0075 "As landed — M3"). They are free again. Renaming back is an
  API break for no gain, so it was not done — but the reason those functions have
  the names they have no longer exists, and a future reader should know that.

