# std/slots.vyrn

Lines: 278. Exports: 8. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

The 8 counted exports are the top-level `export fn` rows: `newSlots` (84), `insert` (97), `alive` (117), `get` (130), `remove` (141), `count` (159), `capacity` (165), `handles` (171). The module also exports two types (`Handle<T>` at 56, `Slots<T>` at 64) and four impl blocks (`Index` at 184, `Iterate` at 209, `Owned` at 247, `Copy` at 267); these are not counted in the 8.

## What this module is for

A generational slab over `Array`. A caller stores values with `insert`, receives a three-word `Handle<T>` (slot, generation, owner identity), and later reads through `get` (returns `Option`, dead handle is a value) or `s[h]` (traps on a dead handle). `remove` bumps the generation and returns the slot to a free list in O(1). A module-global counter hands each container a unique owner word, so a handle from one container reads as dead in another. `std/stream.vyrn:32` builds its producer cursors on this module, and eight files under `examples/` use it.

## Findings

### 8. Allocation frequency — MEDIUM

What: `get` copies the whole element out on every call, so each read of a heap payload allocates a fresh buffer.

Where: `std/slots.vyrn:134`.

Evidence: bench file `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/slots/b.vyrn`, command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/slots/b.vyrn` from `N:\lang`. Bench "get string 64B x10000" ran 10000 `get` calls on a `Slots<String>` of 64-byte strings: min 424.50 µs, which is about 42 ns per call including the alive check and the fresh string allocation. The same loop spelled through the place form, `p.s[p.h].byteLength` (bench "index string 64B x10000"), folded to min 860 ns total, because the place spelling copies nothing and the reads are loop-invariant.

Cost if unfixed: every caller that polls liveness through `get` in a loop pays one allocation per poll; `examples/genref.vyrn` and `examples/slottable.vyrn` read through `get` today, and the doc comment at `std/slots.vyrn:128` already tells readers to prefer `s[h]` for this reason.

Smallest fix: none needed in code — the cheaper spelling exists; document the 40x gap on `get`. RECOMMENDATION, NOT A DECISION.

What: `handles` materialises a fresh N-element `Array<Handle<T>>` on every call.

Where: `std/slots.vyrn:172-176`.

Evidence: same command. Bench "handles x100 incl build" ran 100 `handles` calls on a 100000-element slab: min 50.67 ms minus bench "build slots 100000" min 1.04 ms gives about 496 µs per call — roughly half the cost of building the whole slab, one 2.4 MB buffer per call.

Cost if unfixed: `examples/slots.vyrn:64` calls `handles(people)` just to take `.length`; any caller doing that on a large slab allocates the full buffer to read a number that `count` already returns in O(1) (`std/slots.vyrn:159-161`).

Smallest fix: point readers at `count`; add no `len` alias until a second real need appears. RECOMMENDATION, NOT A DECISION.

### 20. Thread safety — MEDIUM

What: the owner-identity counter is one unsynchronized module-global, incremented on every container creation and every `copy`.

Where: `std/slots.vyrn:75-80` (the `let mut issued = 0` state and `takeIdentity`), called from `std/slots.vyrn:91` and `std/slots.vyrn:275`.

Evidence: structural, plus the language fact that tasks exist: `examples/branchtypes.vyrn:76-79` spawns `Task<Int64>` values with `spawn`, and `examples/concurrency.vyrn:16` shares a record with a task through a `share Config` parameter. Whether two tasks can construct `Slots` containers concurrently and race `issued = issued + 1`: NOT MEASURED. If they can, two containers can receive one identity, and the foreign-handle guarantee at `std/slots.vyrn:26-32` fails silently.

Cost if unfixed: the first concurrent program that creates slabs from two tasks gets colliding owner words, and stale handles pass `alive` (`std/slots.vyrn:117-122`) across containers.

Smallest fix: measure whether task state is shared; if it is, move identity issue behind whatever atomic or lock primitive the language offers, or forbid `newSlots` in task bodies. RECOMMENDATION, NOT A DECISION.

### 3. Side effects (target zero) — LOW

What: constructing or copying a container mutates module state, so `newSlots` and `copy` are not pure functions.

Where: `std/slots.vyrn:77-80`.

Evidence: the assignment `issued = issued + 1` runs once per `newSlots` (`std/slots.vyrn:91`) and once per `Copy::copy` (`std/slots.vyrn:275`). No bench applies; the effect is one integer increment, NOT MEASURED in isolation because it is unmeasurably small.

Cost if unfixed: none in performance terms today; the cost is that container creation has hidden order-dependent behaviour, which is what the identity word at `std/slots.vyrn:73-74` intends.

Smallest fix: accept it — the side effect is the mechanism, and it is documented. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — LOW

What: a removed element's payload stays in `vals` until the slot is reused or the container drops, so peak memory holds dead payloads.

Where: `std/slots.vyrn:34-39` states the policy; `remove` at `std/slots.vyrn:141-156` bumps the generation and pushes to `free` without clearing `vals`.

Evidence: structural — no statement in `remove` writes `s.vals[h.slot]`. Magnitude: NOT MEASURED.

Cost if unfixed: a workload that inserts many large strings and removes most of them keeps every dead string resident until reuse or drop; `examples/freelist.vyrn` churns nodes through exactly this path.

Smallest fix: none available without an uninitialized-place story in the language; the doc comment already names the two release points. RECOMMENDATION, NOT A DECISION.

### 1. Cache locality — LOW

What: `for x in s` reads every element through two arrays, `dense[i]` then `vals[dense[i]]`, and pays about 17% more per element than walking one flat array even when no removal happened.

Where: `std/slots.vyrn:213-215` (`place nth` yields `self.vals[self.dense[i]]`).

Evidence: same bench command. Min-of-samples, build subtracted within the same run: "walk array 100000 x50" 2.98 ms − "build array 100000" 133 µs gives about 57 µs per walk of 100000 elements (0.57 ns/element); "walk slots 100000 packed x50" 4.37 ms − "build slots 100000" 1.04 ms gives about 67 µs per walk (0.67 ns/element). After removing half the slots ("walk slots 50000 scattered x50" 3.68 ms − "build+halve slots 100000" 2.22 ms), the per-element rate was about 0.58 ns — no additional scatter penalty at this working set (100000 × 8 bytes fits L2 on the test machine). Larger working sets: NOT MEASURED.

Cost if unfixed: hot iteration loops over large slabs pay the extra indirection per element per pass; current in-repo walkers are demo-scale (`examples/slots.vyrn:64`, `examples/freelist.vyrn`), so nobody pays a visible amount today.

Smallest fix: nothing to change in this module — the indirection is what makes `remove` O(1) and `for` skip-free; callers needing raw speed should hold a plain `Array`. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 2, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
