# std/stream.vyrn

Lines: 279. Exports: 7 (`cursorGet`, `cursorSet`, `unfold`, `map`, `filter`, `take`, `merge`) plus one exported type, `Cursor` (std/stream.vyrn:37). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller builds a producer with `unfold(seed, step)` or the builtin `fromArray`, then shapes it with the lazy combinators `map`, `filter` and `take`, or interleaves two finite streams with `merge`. Every function takes a `Stream` parameter and returns one, so the module is also how the linear ownership rule reaches combinator chains: an abandoned result fails to compile. The cursor each step reads and writes lives in a slab this module owns (`Slots<CursorCell>`, std/stream.vyrn:45). In-repo callers are the SSE and WebSocket tails in examples/bin/server/api/pastes.http.vyrn:60,86 (both `stream.unfold`) and the bench rows in examples/membench.vyrn:444,454.

All performance numbers below come from one run of
`compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/stream/b.vyrn` from `N:\lang`. Scratch file: `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/stream/b.vyrn`.

## Findings

### 2. Algorithm complexity — MEDIUM

What: `merge` drains both inputs into arrays before it emits the first element, so its time and its first output are both O(|a| + |b|), and an endless side never yields anything.
Where: `std/stream.vyrn:259-277` — the two full-drain loops at lines 260-266 precede any output push.
Evidence: `bench "merge feeds 1000+1000"` min 3.98 µs for a 2000-element result; the hang on an endless side is structural, not probabilistic, because line 260's loop has no end condition short of source exhaustion. The module states this itself at std/stream.vyrn:251-257.
Cost if unfixed: today only examples/streamops.vyrn:90 calls `merge`, over two finite three-and-five-element feeds, so no in-repo caller pays; the cost lands on the first caller who reaches for merge over a live feed.
Smallest fix: none that fits the one-box-per-cursor representation the module documents at std/stream.vyrn:253-256; a lazy two-source merge needs a second cursor cell word. RECOMMENDATION, NOT A DECISION.

### 3. Side effects — LOW

What: every cursor read and write mutates one module-global slab shared by all streams in the program.
Where: `std/stream.vyrn:45` declares `let mut cells: Slots<CursorCell>`; lines 67 and 72 write it through `handleOf`.
Evidence: structural. Each `cursorSet` in a user step (for example examples/bin/server/api/pastes.http.vyrn:48) is a store into this global; the per-element cost of that round trip is measured under axis 10. NOT MEASURED as an isolation defect.
Cost if unfixed: two streams stepping inside one host callback sequence share one container, so a trapped dead handle takes down unrelated producers' state visibility.
Smallest fix: keep the global but scope handles per stream, which the generation check at std/slots.vyrn:186 already does. No change recommended. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — MEDIUM

What: `merge` holds both drained inputs plus the output array live at once, so peak retention is |a| + |b| + |a| + |b| elements.
Where: `std/stream.vyrn:259,263,267` allocate `xs`, `ys` and `out`; all three are live until line 278.
Evidence: structural from those three declarations; timing corroboration `bench "merge feeds 1000+1000"` min 3.98 µs against `bench "take unfold 10000"` min 97.06 µs shows the eager path itself is cheap, which makes the memory, not the time, the binding cost.
Cost if unfixed: same caller story as axis 2 — only examples/streamops.vyrn:90 today, at eight elements.
Smallest fix: interleave by pulling both sources through a second boxed address instead of draining them. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: building any wrapper allocates at least twice — one box for the wrapped source and one new slab cell — plus a capture block for the registered step lambda.
Where: `std/stream.vyrn:161-162` (map), `187-188` (filter), `222-223` (take) each call `newCursor` and `boxStream` and define a capturing lambda; `unfold` at 103 and 108 does the insert and the lambda without the box.
Evidence: `bench "open close 1000"` min 30.95 µs for 1000 unfold-plus-close cycles = about 31 ns per open-close pair, against `bench "array loop 1000"` min 614 ns for 1000 pushes plus a walk. Allocation counts per op NOT MEASURED directly.
Cost if unfixed: examples/membench.vyrn:466 opens and closes one stream per loop iteration and pays the 31 ns each time.
Smallest fix: nothing obvious below the representation; the costs are the design. RECOMMENDATION, NOT A DECISION.

### 10. Control flow predictability — MEDIUM

What: each element crosses at least one indirect call (the step fn value) and one wrapper dispatch through `pullAt`, so the hot loop is a chain of indirect branches instead of a straight walk.
Where: `std/stream.vyrn:114` (step dispatch in `unfold`), `169` (`pullAt` in map's step), `235` (in take's step).
Evidence: `bench "take unfold 1000"` min 9.71 µs ≈ 9.7 ns per element, against `bench "array loop 1000"` min 614 ns ≈ 0.61 ns per element — about 16× for the same sum. Adding one wrapper layer costs `bench "take map unfold 1000"` min 13.01 µs (+34%). Scaling stays linear: `bench "take unfold 10000"` min 97.06 µs, 10.0× the 1000 row.
Cost if unfixed: examples/bin/server/api/pastes.http.vyrn:60 pulls every SSE frame through `stream.unfold`, paying the round trip per frame; at frame sizes seen in that server the absolute cost is small.
Smallest fix: none available in Vyrn; the calls are the lazy design the module chose deliberately (std/stream.vyrn:149-159 records rejecting the shared `wrap` on these same grounds). RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — MEDIUM

What: `filter` asks its source once per element scanned, so admitting one element out of k costs k·n asks for n outputs, and over an endless source where nothing passes it never returns.
Where: `std/stream.vyrn:195-204` — the `while true` loop re-pulls until the predicate passes or the source ends; the module documents both facts at std/stream.vyrn:180-185.
Evidence: `bench "filter passall 1000"` min 13.42 µs (1000 asks, 1000 yields) against `bench "filter 1-in-10 over 10000"` min 130.21 µs (10000 asks, 1000 yields) — 9.7× the time for the same yield count, matching the 10× ask count.
Cost if unfixed: any caller filtering a dense event feed pays k times the take-only rate; no in-repo caller filters a stream outside tests today.
Smallest fix: none; this is the honest contract of a pull filter and the module says so. RECOMMENDATION, NOT A DECISION.

### 20. Thread safety — LOW

What: the shared slab has no synchronization; correctness rests entirely on the host owning the single loop.
Where: `std/stream.vyrn:45` (the unsynchronized `let mut cells`), with the stated concurrency refusal at std/stream.vyrn:14-16.
Evidence: structural; there is no lock primitive in the file. NOT MEASURED under threads.
Cost if unfixed: a host that steps two streams concurrently races on slot indices and generations.
Smallest fix: document the single-threaded requirement at the export site rather than only in RFC prose. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 4, 5, 6, 9, 11, 12, 13, 14, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
