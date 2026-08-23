# std/bench.vyrn

Lines: 268. Exports: 9 (8 `export fn` — `minOf`, `mean`, `median`, `formatDuration`, `padRight`, `benchMeasure`, `benchOne`, `benchJson` — plus 1 `export type BenchResult`). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

The `vyrn bench` transform links every benched program against this module. `benchMeasure` runs the sampling loop (warmup, iteration auto-scale, sample collection) over a `std/time.monotonic` clock; `minOf`/`median`/`mean` reduce the per-iteration nanosecond samples; `benchOne` prints the aligned human line and `benchJson` emits the RFC-0063 machine report. The pure helpers are unit-tested in Vyrn at `std/bench.vyrn:229-268`. The synthesized harness `main` is the only importer (`rfcs/RFC-0055-benchmarking.md:193-203`); the module never enters a `run`/`build` compile.

## Findings

### 2. Algorithm complexity — LOW

What: `median` sorts through a private insertion sort, O(n²) in sample count, where the sampling cap allows up to ~2000 samples (2000 ms cap / 1 ms minimum sample).
Where: `std/bench.vyrn:39-61` (`sortedCopy`), called from `median` at `std/bench.vyrn:65`; loop bounds that set n at `std/bench.vyrn:154` and `std/bench.vyrn:178`.
Evidence: bench on verbatim copies of `sortedCopy`/`median`/`minOf` over reverse-sorted inputs (worst case for insertion sort), scratch `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/bench/b.vyrn`, command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/bench/b.vyrn` from N:\lang: n=31 min 464 ns; n=250 min 13.63 µs (29× time for 8× n); n=2000 min 777.00 µs (1671× time for 65× n). The linear `minOf` on the same 2000-element array took 4.65 µs — the sort costs ~167× the scan it sits beside.
Cost if unfixed: one off-quadratic pass per benched block, paid by every `vyrn bench` invocation including CI (`rfcs/RFC-0063-ci-benchmarks.md:101`); worst case ~0.8 ms against a ≥500 ms run, so today's cost is real but small.
Smallest fix: replace `sortedCopy` with a standard-library sort or document the bounded-n tradeoff in its doc comment. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: `padRight` allocates one intermediate String per padding byte by repeated concatenation instead of building the result once.
Where: `std/bench.vyrn:105-108`.
Evidence: same scratch and command as finding 2: `padRight("bench \"hash to 1000\"", 40)` (~20 appends) min 89 ns; width 320 (~300 appends) min 328 ns — cost grows with append count but subquadratically in this range, so capacity reuse likely absorbs part of it. NOT MEASURED: allocator call counts.
Cost if unfixed: negligible — the only caller is `benchOne` at `std/bench.vyrn:199`, once per bench.
Smallest fix: preallocate to `width` bytes and copy once, or leave as-is given the call frequency. RECOMMENDATION, NOT A DECISION.

### 18. Precision loss — LOW

What: two truncating integer divisions lose sub-nanosecond resolution in reported statistics: `mean` rounds toward zero (measured `mean([10, 11])` prints 10), and each sample is `dt / iters`, which reads 0 whenever a sample takes less than `iters` nanoseconds.
Where: `std/bench.vyrn:34` (`sum / xs.length`); `std/bench.vyrn:173` (`samples.push(dt / iters)`).
Evidence: `compiler/target/release/vyrn run C:/Users/demko/AppData/Local/Temp/claude/ox-a2/bench/b3.vyrn` printed `10` for mean([10, 11]). For the sample division: auto-scale stops at `dt >= 1000000` (`std/bench.vyrn:154`), so a body near-zero-cost reaches huge `iters` before dt crosses 1 ms — measured output of `vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/bench/b2.vyrn`: an empty body settled at 4194304 iterations and reported `min 0 ns median 0 ns mean 0 ns`.
Cost if unfixed: `--compare` verdicts in RFC-0063 compare these integers across runs (`rfcs/RFC-0063-ci-benchmarks.md:89-107`); sub-ns bodies report all-zero statistics instead of a duration.
Cost note: integer-only rendering is deliberate for byte-identical backends (`std/bench.vyrn:78`).
Smallest fix: scale samples to picoseconds (multiply before dividing) or keep more significant digits in `BenchResult`. RECOMMENDATION, NOT A DECISION.

### 26. Syscall frequency — LOW

What: the warmup loop reads the monotonic clock once per `body()` call, so for fast bodies most warmup work is clock reads.
Where: `std/bench.vyrn:139`.
Evidence: same b2.vyrn command as above: one `monotonic()` read through the bench harness min 1.83 µs; an empty body min 0 ns. Any body slower than ~1.83 µs per call (the documented example bodies run ~1.35–2.01 µs, `examples/benching.vyrn:12-13`) makes the clock read the dominant term of each warmup iteration. Warmup results are discarded, so the bias lands on wall time only, not on the recorded samples.
Cost if unfixed: every bench pays up to ~50 ms of mostly clock reads during warmup; no effect on reported numbers.
Smallest fix: call `body()` in fixed batches (for example 1024 calls) between clock checks inside the warmup loop. RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: each bench burns a fixed ~50 ms warmup plus every doubling pass of the auto-scale phase, including one full ≥1 ms timed pass whose measurement is used only for the scaling decision and then dropped, before the first recorded sample.
Where: `std/bench.vyrn:137-141` (warmup bound), `std/bench.vyrn:146-159` (doubling re-runs and discarded final timing).
Evidence: loop bounds prove ≥50 ms plus ~2× the final iteration count executed pre-sampling per bench; exact split between compile, warmup, and sampling NOT MEASURED. Observed wall time: the six-bench b.vyrn run took 4.69 s total and the two-bench no-op b2.vyrn run 2.35 s (both include clang compilation of the harness).
Cost if unfixed: suites with many benches pay ~150 ms+ of non-sampling time each; CI pays it on every run (`rfcs/RFC-0063-ci-benchmarks.md:161-164`). No effect on reported numbers.
Smallest fix: reuse the final auto-scale pass as the first sample instead of discarding it. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19, 20, 21, 22, 23, 24, 25, 27, 29, 30.
