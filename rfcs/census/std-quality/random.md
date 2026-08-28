# std/random.vyrn

Lines: 128. Exports: 4. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller seeds a pure SplitMix64 generator with `seededRng` and threads it through `nextInt` or `nextInRange` draws; each call returns the drawn value plus the advanced `Rng`, so any run replays byte-identically on all three backends. `randomSeed()` is the single host-seeded escape hatch for unpredictable starts. Besides the 4 `export fn`, the module exports two types (`Rng`, `std/random.vyrn:25` and `Draw`, `std/random.vyrn:29`) and declares one module-private extern (`hostRandomSeed`, `std/random.vyrn:35`). In-repo callers today: `std/storage.vyrn:30` imports `randomSeed` for temp-file names, and `examples/clock.vyrn:24` imports the whole surface.

## Findings

### 8. Allocation frequency — LOW

What: every `nextInRange` call constructs three records — the internal `Draw` and `Rng` inside `nextInt` plus its own result `Draw` — roughly 2.5x the per-draw cost of `nextInt` alone.

Where: `std/random.vyrn:69` and `std/random.vyrn:92`.

Evidence: from `N:\lang`, `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/random/b2.vyrn` printed `bench "nextInt x 4096 accumulated" min 3.13 µs` (4096 draws = 0.76 ns/draw), and `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/random/b.vyrn` printed `bench "nextInRange 1..6 x 4096" min 7.65 µs` (1.87 ns/draw) and `bench "nextInRange full span x 4096" min 5.38 µs` (1.31 ns/draw). Measurement note: a first bench that used only the threaded final state was constant-folded to `min 1 ns`; the accumulated-xor form above is the one that actually runs the draws.

Cost if unfixed: `examples/clock.vyrn:24` pays about 1 extra nanosecond per ranged draw; no in-repo caller draws in a hot loop, so the absolute cost today is negligible.

Smallest fix: have `nextInRange` mix and reduce inline instead of delegating to `nextInt`, building one `Draw` instead of two. RECOMMENDATION, NOT A DECISION.

### 17. Numerical stability — LOW

What: unsigned modulo reduction biases low outcomes by up to `(2^64 mod span)` extra counts out of `2^64` draws.

Where: `std/random.vyrn:91` (reduction), documented as accepted at `std/random.vyrn:73-74`.

Evidence: the mechanism is provable from the code — line 91 computes `UInt64(d.value) % span`, and `2^64` is never a multiple of an arbitrary `span`, so residues below `2^64 mod span` hit one extra output class. Concretely, for the dice range `[1, 6]`: `2^64 mod 6 = 4`, so 4 of the 6 faces carry a relative excess near `2^-62`. The statistical size of the bias is NOT MEASURED; detecting it needs far more samples than a bench facility provides, and the module comment already accepts it for v1.

Cost if unfixed: sampling or shuffle code built on `nextInRange` carries an unmeasurable-in-practice skew; `examples/clock.vyrn:24` is the only in-repo consumer and does not care.

Smallest fix: rejection-sample over `UInt64.max - (UInt64.max % span)` before reducing, or document the bias bound in the public doc comment. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
