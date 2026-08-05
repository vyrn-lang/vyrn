# RFC-0090 — One Model: Values, and Nothing Else

- **Status:** Proposed. A delta on RFC-0089 — accept both or neither. It removes
  RFC-0089's rule 5 and replaces it.
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
- **M1** — `std/slots` in Vyrn over `Array`, alongside `Ref` (both exist;
  corpus migrates example by example; benchmarks compare them under M0's
  harness).
- **M2** — RFC-0089 M2 (conventions) lands; the `Slots` port stops being
  blocked by escape-on-call.
- **M3** — streams re-host their cursor on `std/slots`.
- **M4** — `cell`/`get`/`set`/`release` and the slab are deleted from all three
  engines. The language's memory runtime is malloc, free, memcpy.

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
