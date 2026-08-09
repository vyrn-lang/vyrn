# Census — The Region

- **Status:** measurement only. No engine code changed. The recommendation at
  the end closes two census rows; it does not propose a milestone.
- **Measured at:** `3d013ef`.
- **Machine:** Windows 11, `clang` 22.1.0, release CLI, native target.
- **Question:** RFC-0087 §5 says regions are hand-placed and "the model's best
  tool is rarely reachable". RFC-0087 U6 says "the arena excludes the case it is
  for". Both rows point at RFC-0004 Q3. Both are OPEN and undesigned. This census
  decides whether they deserve a design.
- **Read first:** RFC-0004 §4 and Q3, RFC-0087 §5 (row 11) and U6, RFC-0089
  ("`own.rs` is deleted, not extended"), RFC-0092 M2/M3, RFC-0093 M2, RFC-0095 M1.
- **Evidence:** the corpus harness
  `cargo test -p vyrn-frontend --lib rfc0089_move_surface_over_the_corpus --
  --ignored --nocapture`; `vyrn why --memory` over all 216 corpus `.vyrn` files;
  three native programs built and timed; three `bench` rows.

---

## The answer, first

**Refuse.** An inferred region has **no customer in this corpus**, and the arena
it would place is **slower and larger** than the reclamation the language already
does.

Three numbers carry the whole census.

| measurement | result |
|---|---|
| corpus bindings an inferred region could reclaim | **0 of 3758** |
| peak memory, 40M concatenations, region around the loop vs no region | **4,393,564 KiB vs 4,296 KiB — 1023x worse** |
| wall time, same program | **3.73 s vs 0.85 s — 4.4x worse** |

The rest of this file is how those numbers were taken, and two defects the
taking of them found.

---

## 1. What the corpus writes

`region` appears as a statement in **three** places in 216 corpus files:

- `examples/region.vyrn:17` and `examples/region.vyrn:29` — the example that
  teaches the feature;
- `examples/controlflow.vyrn:94` — a `continue` under a region, which exists to
  pin that the arena is freed on the `continue` edge.

No `std/` module writes `region`. No dogfood application writes `region` —
not `examples/vlog`, not `examples/bin`, not `examples/shelf`. Every other
occurrence of the word in the corpus is prose in a doc comment.

Nine years of RFCs after RFC-0004 shipped the feature, the corpus writes it
**only where the corpus is teaching it**. That is the first datum, and RFC-0087
§5 already predicted it: hand placement means nobody places it.

---

## 2. U6, quoted and explained

RFC-0087 U6 reads, in full:

> `region { }` is a block. `push` cannot draw from it, because arena memory has
> no `realloc`. So the loop that builds many values with one lifetime — the case
> RFC-0004 §4 names as the arena's reason to exist — is the case the arena cannot
> serve.
>
> What is left is String concatenation inside a lexical block. RFC-0004 Q3 asked
> for inferred, invisible regions and nothing was designed.

**What the exclusion is, in the code.** `Gen::heap_alloc`
(`compiler/vyrn-codegen/src/lib.rs:2250`) routes an allocation to the arena when
`region_depth > 0`, and to `malloc` otherwise. A growable buffer never reaches it.
`Gen::array_n_to_heap` (`compiler/vyrn-codegen/src/lib.rs:2513`) says why in its
own words:

> Always plain `malloc`, never the region arena: `push` grows this buffer with
> `realloc` and cleanup uses `free`, both undefined on an arena interior pointer.

The checker states the same fact from the other side. `Checker::contains_heap`
(`compiler/vyrn-frontend/src/checker.rs:3386`) carries the comment "Array buffers
are always malloc'd (never in the region arena), so only their *contents* can
dangle."

**So the arena's reach is exactly one thing: a `String` buffer allocated
lexically inside the block.** Everything else in the language that owns heap — an
`Array`, a `SmallArray`, a `Map`, a `Stream`, a spawn frame — is `malloc`'d
whether a region is open or not.

`examples/region.vyrn` demonstrates the exclusion inside itself. `vyrn why
--memory` on that file reports:

```
    line 20    greeting         NOT reclaimed — it is inside a `region` — the arena owns it
    line 30    a                reclaimed at block exit — freeing the array buffer
```

Two bindings, both inside a `region`. The `String` belongs to the arena. The
`Array` belongs to the ownership walk, which frees it whether the region is there
or not. The example's own second block already says so in a comment: "Growable
arrays built inside a region use the ordinary heap".

---

## 3. What the arena actually is

The measurement in section 5 is only readable against the runtime, so read the
runtime first. `REGION_RUNTIME` (`compiler/vyrn-codegen/src/lib.rs:48`) is 60
lines of LLVM IR:

- `__vyrn_region_alloc(n)` calls `__vyrn_malloc(n + 8)` and writes the previous
  head into the first 8 bytes. It returns `raw + 8`.
- `__vyrn_region_exit()` walks that chain and calls `free` on every link.
- `__vyrn_region_enter()` traps above 64 nested scopes. The stack is 64 pointers,
  `thread_local`.

**The arena is a chain of `malloc`s, not a bump allocator.** It does not free in
O(1) at block exit. It frees **once per allocation**, exactly as many times as the
ownership walk would, and it pays **8 extra bytes and one extra store** per
allocation to build the chain.

That is the mechanism in one backend. The other two have none:

- **The interpreter** (`compiler/vyrn-frontend/src/interp.rs:3534`) keeps a
  counter and reproduces the depth-64 trap. It reclaims nothing region-specific.
- **The direct wasm backend** (`compiler/vyrn-codegen/src/direct.rs:2463`) keeps a
  counter and reproduces the trap. Its own comment says "It reclaims nothing", and
  a `ponytail:` note records that a sound version needs a separate bump arena that
  does not exist yet.

So `region` is one backend's mechanism and two backends' counter.

---

## 4. Measurement 1 — how many bindings could an inferred region capture?

A candidate is a binding that (a) owns heap, (b) does not leave its block, and
(c) is not already reclaimed.

### The corpus harness

`rfc0089_move_surface_over_the_corpus`, 216 files, 38,693 lines:

```
bindings: 3758 — 1477 reclaimed/moved/dropped/static, 2281 not reclaimed
   2048  the type owns no heap
    131  it names somebody else's value
     83  the type has no release rule
     10  escaped into a call
      8  aliased by another binding
      1  inside a `region`
```

`vyrn why --memory` over the same 216 files, deduplicated across importers,
agrees: 79 with no release rule, 17 aliased, 7 escaped. The two views differ
because the harness reads each file unlinked and the CLI reads each file linked;
the CLI is the one that can name types, so the classification below uses it.

### The classification

| bucket | count | is it a candidate? | why |
|---|---:|---|---|
| the type owns no heap | 2048 | **no** | fails (a). A scalar has nothing to reclaim |
| it names somebody else's value | 131 | **no** | fails (a). Rule 2 says the owner is elsewhere |
| the type has no release rule | 79 | **the hunting ground** | see below |
| escaped into a call | 7 | **no** | fails (b) by definition |
| aliased by another binding | 17 | **no** | fails (b). Every one is generator-emitted `Array<JsonField>` or a `String` a return carries out |
| inside a `region` | 1 | **no** | fails (c). The arena already holds it |

### The hunting ground, by type

The 79 bindings with no release rule, grouped by their type:

| type | count | can an arena reclaim it? |
|---|---:|---|
| `Html` | 12 | no — `El(String, Array<Attr>, Array<Html>)` |
| `Task<Int64>` / `Task<String>` | 11 | no — the frame is `__vyrn_malloc`'d at the spawn site (`lib.rs:10429`), never routed |
| `Stream<T>` / `Stream<Int64>` / `Stream<U>` | 10 | no — a `Stream` is three words over a `malloc`'d buffer |
| `VyxComp` | 9 | no — six `Array<..>` fields and a `VyxNode` |
| `Json` / `json$Json` | 11 | no — `JArr(Array<Json>)`, `JObj(Array<JsonField>)` |
| `GqlOut` / `GqlVal` / `GqlSet` / `GqlArgs` / `GqlQuery` | 12 | no — every one reaches `Array<..>` or `Json` |
| `VyxOne` / `VyxParse` / `VyxBody` / `VyxGroup` / `VyxNode` / `VyxNodeR` / `VyxRegistry` | 13 | no — every one reaches `Array<VyxNode>` |
| `RpcClientTypes` | 1 | no — `Array<Symbol>`, and `Symbol` reaches `Json` |

**Every one of the 79 reaches an allocation the arena refuses.** The candidate
count is **0**.

**All but `Html`, `Json` and what reaches them are reclaimed now** (RFC-0096, and
the paragraph at the end of this section). That changes nothing about the region
question: the reclamation is a declared `release`, and an arena was refused for
being slower than the walk it would replace, not for the walk's coverage.

### Two facts inside that table

**21 of the 79 are not leaks at all.** `release_kind`
(`compiler/vyrn-frontend/src/own.rs:370`) answers `None` for `Stream<T>` and
`Task<T>` on purpose, because both are reclaimed by their own lowering — a stream
by `close`, a task by `join` or `drop` since RFC-0095 M1. `examples/concurrency.vyrn`
joins all four of its tasks and drops the fifth. `vyrn why --memory` reports all
five as "NOT reclaimed". See defect 2.

**All 58 of the rest are self-referring types.** `release_kind`'s record arm
(`own.rs:444`) refuses a type that reaches itself, because a structural release
walk of one has no bottom — the same guard `copy` carries since Phase 4b. `Html`,
`Json` and `VyxNode` are that shape, and every other name in the table reaches one
of the three. The declared answer already exists: `release_kind` reads `impl Owned
for T` **before** the self-referring guard (`own.rs:296`), so a hand-written
release closes all 58 the way `std/json`'s `copyJson` closes the `copy` side. It is
a declaration, and it is not a region.

**The declaration was written, and the number is two.** RFC-0096 closed this row.
By the time it ran the harness read 63 rather than 79 (RFC-0094 and RFC-0095 M1
had landed), and **all 63 were self-referring** — 56 in `std/vyx.vyrn`, 7 in
`std/graphql.vyrn`. This section expected one `impl Owned` per leaking type and
measured wrong: the guard's question was "does this type reach itself", and the
question a release wants is "is there a declaration ON the cycle", because the
walk emits a CALL at a declared type instead of expanding. So `impl Owned for
VyxNode` and `impl Owned for GqlSel` — two declarations, on the two types the
cycles run through — gave every type above them its structural row back. The
harness row **falls 63 to 0** and the "not reclaimed" total **2267 to 2207**. The
sentence held; the count did not.

---

## 5. Measurement 2 and 3 — what would it save, and what does a region buy?

The candidate count is 0, so the saving is **0 bytes**. What is left to measure is
whether a region is worth placing where one **is** legal today. It is not.

### 5a. RFC-0004 §4's own program, re-run

RFC-0004 §4 justified the arena with a 40-million-iteration concatenation loop.
Three shapes of that program, built native, median of 3 runs, peak working set:

| shape | peak | wall |
|---|---:|---:|
| no region — the type-driven walk frees each temporary | **4,296 KiB** | **0.85 s** |
| one region per iteration | 4,208 KiB | 1.07 s |
| one region around the loop | **4,393,564 KiB** | **3.73 s** |

The region placed where a person places it — around the loop, which is the shape
`examples/region.vyrn` itself is written in — is **1023x the memory and 4.4x the
time**. The chain holds every one of the 40 million buffers until the closing
brace. That is 112 bytes per iteration, linear, and unbounded in the loop count.

The region placed where an inference would place it — the innermost block that
contains the value's whole life — costs 26% more time and reclaims what the walk
already reclaimed. It buys nothing and charges for the counter and the link word.

RFC-0004's measurement was true when it was taken. It was taken against a compiler
that leaked without a region. **The walk now gives that flatness with no region and
no annotation**, which is what RFC-0089 through RFC-0095 were for.

### 5b. The same comparison as timing rows

Three `bench` rows over `concatFresh`'s exact shape, 1000 fresh buffers, median of
3 runs:

| row | median |
|---|---:|
| the walk frees each | **20.44 µs** |
| one region per iteration | 20.28 µs |
| one region around the loop | **25.29 µs** (+24%) |

An arena that frees once per allocation cannot beat a walk that frees once per
allocation. It can only lose by the size of the chain it also maintains.

### 5c. Is any candidate in a hot path?

Yes, and it changes nothing. Nine of the 58 self-referring leaks sit inside loops
in `std/vyx`, `std/graphql` and `std/rpc` — the generators, which RFC-0076 runs as
wasm in the LSP on every keystroke. Those leaks are real and they are worth
closing. **A region cannot close one of them**, because every one of them is a
tree built out of `Array` buffers. `impl Owned` can close all nine.

---

## 6. Measurement 4 — U6's own case

U6's exclusion is one routing decision: a growable buffer never asks the arena.
Removing it needs three things the language does not have.

1. **An arena that supports `realloc`.** Today's arena cannot, because a link
   word sits 8 bytes behind every user pointer and `realloc` would move it. The
   sound shape is a separate bump arena with a per-region mark, which
   `direct.rs:2455` already names as the missing piece in this backend and which
   exists in neither.
2. **A second implementation in the direct wasm backend**, which today has a
   counter and no allocator.
3. **A wider escape guard.** `region_store_guard`
   (`compiler/vyrn-frontend/src/checker.rs:3420`) examines stores into named
   bindings. It does not examine returns. `REGION_RUNTIME`'s own comment records
   the consequence: a `return` out of a region emits `__vyrn_region_pop`, which
   pops the frame **without freeing it**, because `return a + b` is not covered by
   anything. Popping leaks. The comment calls it "the trade RFC-0004's escape
   analysis has to be written before it can be improved on."

**And fixing U6 makes measurement 5a worse, not better.** The exclusion is the
only reason the arena's deferral is bounded today. An arena that also swallowed
every `Array` in a block would hold more, for longer, on the same 1023x curve.
Widening the arena's reach widens its memory regression.

A targeted U6 fix therefore does not beat inference. **Both lose to what already
ships.**

---

## 7. Measurement 5 — what would inference cost?

**Region inference is the escape analysis this repo deleted.** RFC-0089 states the
deletion in its own words:

> **`own.rs` is deleted, not extended.** The escape walker, the safe-read list,
> the `owned_fns` fixpoint, `transfers`, `expr_type` — all of it exists to *infer*
> what rules 2 and 3 now *declare*. What replaces it is a checker: signatures in,
> diagnostics out, no fixpoint, no under-approximation, no leak-when-unsure.

`own.rs`'s header records what survived and why:

> Until Phase 4c this file inferred both. It carried a list of expression forms
> that "transfer", a list of built-in calls that produce, a list of argument
> positions that only read, and a fixpoint over which functions return an owned
> value. Every one of those was a guess made in parallel with a rule the compiler
> was separately enforcing, and where the guess was unsure it leaked. The lists
> are gone.

An inferred region needs, for each block, a proof that **no allocation made inside
it outlives it** — through every store, every call argument, every return, every
capture. That is the escape walker, restored, and made sound on the two paths the
current guard admits it does not cover (a return, and a callee that retains). The
price is the analysis RFC-0089 removed 1,714 lines to be rid of, plus a second
allocator in the direct backend, and the corpus it would serve is 0 bindings.

There is one more cost, and it is already being paid. `region` is the **only**
construct in the language that switches the type-driven walk off. `own.rs:1137`
reads:

```rust
if kind == DropKind::FreeStr && self.region_depth > 0 {
```

A `String` inside a region is recorded as `Leak::Region` and the walk skips it.
`own.rs`'s header lists this as one of only two conditions the file still decides
for itself. Every region in a program is a hole cut in the model's one rule, and
the hole exists to hand the value to a mechanism that is measurably worse.

---

## 8. Two defects the census found

### Defect 1 — a copied `Array` inside a region corrupts the native heap

`Gen::copy_buf` (`compiler/vyrn-codegen/src/lib.rs:2709`) routes through
`heap_alloc`. `Gen::array_n_to_heap` (`lib.rs:2513`) does not, and its comment
says why nothing may. So `xs.copy()` **inside** a region draws its buffer
from the arena. The walk then frees it at block exit — `own.rs:1137` suppresses
only `DropKind::FreeStr`, never `FreeArr` — and `__vyrn_region_exit` frees the
same block a second time.

```vyrn
fn main() -> Int64 {
    let mut xs: Array<Int64> = []
    let mut i = 0
    while i < 8 { xs.push(i); i = i + 1 }
    let mut n = 0
    region {
        let a = xs.copy()
        n = a[3]
    }
    print(n)
    return 0
}
```

| engine | result |
|---|---|
| interpreter | prints `3` |
| wasm (`wasmtime`) | prints `3` |
| **native** | **exits `0xC0000374` — `STATUS_HEAP_CORRUPTION`** |

No `push` is needed; the double free alone does it. Adding a `push` reproduces the
same crash through `realloc` on an arena interior pointer. This is a live
three-way parity break and a memory-safety defect. It is not fixed here — this
census changes no engine code — and it is one more count against the mechanism:
the arena's routing is a second owner for allocations the walk already owns, and
the two disagree.

**Fixed.** The diagnosis above names one of the two sites, and `copy` is not the
mechanism. `Gen::copy_buf` is the first site: it calls `__vyrn_malloc` directly
now, which is the rule `Gen::array_n_to_heap` states, because `push` grows a
copied buffer with `realloc` and the walk hands it back with `free`. The second
site has no `copy` in it. `Gen::str_alloc` routes through `Gen::heap_alloc` too,
so **every** `String` built inside a region comes from the arena — including a
`String` the walk reaches one level down. `own.rs` suppresses by the binding's
own `DropKind`, so it sees `let s = a + b` and it does not see the `String` under
an `Array<String>`, in a record field, or under a `Map` key. All three corrupted
the native heap at `259330b` with no `copy` written. The fix gives the release
side the key the allocation side already uses: `Gen::deep_release` frees no
`String` while `region_depth > 0`, exactly as `Gen::heap_alloc` draws from the
arena while `region_depth > 0`. The two sides now partition the same way at every
depth. `own.rs` does not change — its rule was right about what it can see, and
the asymmetry was between codegen's two halves. One shape leaks where it freed
before: a `String` allocated outside a region and moved into a container declared
inside one. The arena does not own that buffer and the walk no longer hands it
back. A leak is not a corruption, and `region_store_guard` refuses the reverse
shape (a store out of the region) at compile time. `examples/region.vyrn` carries
both halves now — an `Array` copy and a `String` copy, then a `String` reached
through a container and through a record field — so the corpus gates the shape on
all three engines.

### Defect 2 — `vyrn why --memory` reports 21 discharged bindings as leaks

`release_kind` answers `None` for `Stream<T>` and `Task<T>` deliberately, because
each is reclaimed by its own lowering. `own.rs` mints `Fate::Leaked(NoRelease {
owns_heap: true })` from that `None`, so the report says "nothing releases the type
`Task<Int64>` yet" about a task that `examples/concurrency.vyrn` joins on the next
line.

RFC-0087 U1 exists because the model needed a window. The window is wrong about
27% of what it flags. The fix is small — `linear_kind` already answers `Some` for
both types, and the printer can ask it — but it is a change to the reporter, and
this census does not make one.

**Closed.** `own::fate` asks `linear_kind` where `release_kind` answered `None`,
and a must-use binding gets `Fate::Discharged` instead of a leak. The sentence is
categorical, and it may be: a value of a linear type that is not discharged on
every path is a compile error (RFC-0075 M1 for a stream, RFC-0095 M1 for a task),
so a program that reaches this analysis has been proved to discharge every one.
A `drop` and a move still answer for themselves, because each names a line a
reader can go to, and a line beats a category. Over the corpus the "not
reclaimed" total falls **2292 to 2267** — 25 bindings, against the 21 measured
here: the extra four are `Stream` bindings that had no declared type at all until
RFC-0094's return-type fold gave the stream producers one, and they were counted
under "the type owns no heap" rather than under "no release rule". By bucket,
"the type has no release rule" falls **85 to 63** and "the type owns no heap"
falls **2055 to 2052**. `examples/concurrency.vyrn` reads 7 bindings not
reclaimed before and 2 after: its four tasks are discharged and `unwanted` reads
"reclaimed by `drop` at line 49". **Nothing gained a free** — `Fate::Discharged`
mints no `droppable` row, so both backends emit what they emitted, and the
memory suite is 16 rows and 16 steady.

**The 63 that were left are 0 (RFC-0096).** Two `impl Owned` rows — on `VyxNode`
and on `GqlSel` — closed the row this census called the hunting ground. The
memory suite is 17 rows and 17 steady. What was left is `Json` and `Html`: 31
bindings by the linked reading, invisible to the unlinked harness, and the same
declaration one module over.

**The linked reading is 0 too (RFC-0096 M2).** Re-derived at `12255e4` the
number is **33** rather than 31 — `Html` had risen from 10 to 12 over the two
commits between the measurements, and every other family matched. Two more
`impl Owned` rows — on `Json` and on `Html` — closed all 33, and the memory
suite is 18 rows and 18 steady. Over the same files the unlinked harness reads
3778 bindings and **2204** not reclaimed, down from 2207, with "the type has no
release rule" still 0; the two parity examples M2 adds take it to 3783 and 2208,
because the harness parses each file alone and cannot name an imported `Json`.

---

## 9. Recommendation

**Close RFC-0087 §5 and U6 as refused by measurement.** Do not design region
inference. Do not design a targeted U6 fix. Point both rows at this file.

The reasoning, in four lines:

1. An inferred region has **0 candidates** in 3758 corpus bindings. Every binding
   the walk does not reclaim reaches an `Array`, a `Stream` or a spawn frame, and
   the arena refuses all three.
2. Where a region **is** legal, it is **1023x the memory and 4.4x the time** of
   the reclamation that already ships, on RFC-0004 §4's own program.
3. Fixing U6 widens the arena's reach and therefore widens that regression.
4. Inference means rebuilding the escape walker RFC-0089 deleted, to serve zero
   bindings.

The 58 real leaks are a **declaration** away, not a region away: `impl Owned for
T` on the self-referring types (`Json`, `Html`, `VyxNode` and the families that
reach them). That belongs to RFC-0092's open tail, and this census hands it the
list.

### What this does not recommend

**Do not delete `region { }` in this change.** Deleting it is a separate,
defensible RFC — it would remove `REGION_RUNTIME`, the depth-64 trap in three
engines, `region_store_guard`, `Leak::Region`, `own.rs`'s only self-decided
`String` exception, and defect 1 with them. It is a deletion of a shipped surface
and it deserves its own measurement and its own migration of two examples. This
census measures inference, and inference is refused.

### The gate, if anyone reopens this

A design for inferred regions must show, before it is written:

- **at least 20 corpus bindings** that an arena reclaims and the type-driven walk
  does not, after `impl Owned` lands on the self-referring types; and
- a shape where the arena's deferral **beats** per-binding release on peak memory,
  measured in `examples/membench.vyrn`, not argued.

Neither exists at `3d013ef`.

---

## Appendix — how to re-run every number

```
# section 4, the corpus counts
cargo test -p vyrn-frontend --lib rfc0089_move_surface_over_the_corpus -- --ignored --nocapture

# section 4, the type breakdown (per file, then group the
# "nothing releases the type X yet" lines)
vyrn why --memory <each of the 216 corpus .vyrn files>

# section 1, the corpus uses
rg '^\s*region \{' examples std

# sections 5a and 8, the native programs: build with `vyrn build -o`, run,
# read PeakWorkingSet64. The three shapes are the loop with no region,
# with one region per iteration, and with one region around the loop.

# section 5b, the timing rows: three `bench` blocks over `concatFresh`'s
# shape, run with `vyrn bench`.
```
