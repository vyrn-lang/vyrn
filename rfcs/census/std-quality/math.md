# std/math.vyrn

Lines: 124. Exports: 8 (`min`, `max`, `abs`, `clamp`, `pi`, `floorF`, `sin`, `cos`; all top-level `export fn`, no other export kinds). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

Integer helpers (`min`, `max`, `abs`, `clamp`), the `pi()` constant, a branchy float `floorF`, and a software sine: range reduction by `2*pi` plus an odd Taylor polynomial in Horner form through `x^13`. Callers import it for ring layouts and wave fields — `examples/herofield.vyrn:58-60` calls `sin` three times per cell, `site/app/corpus.vyrn:321-326` and `site/app/bench.vyrn:581-582` call `sin`/`cos` per plotted point. The module has no `sqrt` and no `log`; callers that need them reimplement them locally through `F64x2.sqrt` (`examples/nbody.vyrn:39`, `examples/spectralnorm.vyrn:24`) or their own series (`site/app/bench.vyrn:512`).

## Findings

### 18. Precision loss — MEDIUM

What: the argument reduction at line 74 subtracts two nearly equal numbers, so the absolute error of `sin(x)` grows linearly with `|x|`, and past 2^53 the result is arbitrary.

Where: `std/math.vyrn:74`.

Evidence: scratch probe `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/math/a.vyrn`, command `compiler/target/release/vyrn run C:/Users/demko/AppData/Local/Temp/claude/ox-a2/math/a.vyrn` from N:\lang. It prints `(vyrn sin - Python math.sin) * 1e10` on identical doubles: error 5.6e-13 at |x| = 6283.7, 3.9e-10 at |x| = 6283185.8, 6.9e-8 at |x| = 628318531.2, and `sin(100000000000000000.0)` prints `0.000000` where libm returns -0.4645301048353727 — absolute error 0.46 on a function bounded by 1. The growth matches eps * |x| / (2 pi) for one full-precision subtraction; the doc comment's "error stays under 1e-13" (line 64) holds only near the origin and states no range limit.

Cost if unfixed: any caller passing arguments above a few million radians gets silently wrong digits with no signal; today's callers keep arguments small (`examples/herofield.vyrn:58-60` uses |x| under ~20), so nobody pays yet.

Smallest fix: state the working range in the doc comment, or switch to a Cody–Waite reduction with a split high/low 2/pi so accuracy stops depending on |x|. `RECOMMENDATION, NOT A DECISION`.

### 2. Algorithm complexity — LOW

What: `cos` re-runs the whole reduction-plus-polynomial pipeline of `sin` on a shifted argument, so a caller needing both pays two full evaluations where one shared reduction could serve both.

Where: `std/math.vyrn:92`.

Evidence: bench file `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/math/b.vyrn`, command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/math/b.vyrn` from N:\lang: `bench "sin x4096"` min 72.56 µs (17.7 ns per call) and `bench "cos x4096"` min 71.68 µs (17.5 ns per call), against `min` at 4.86 µs per 4096 calls (1.19 ns). Both functions are O(1); the cost is constant-factor duplicated work. `examples/herofield.vyrn:67` calls `sin` and `cos` together per lattice cell, and both site ring renderers do the same.

Cost if unfixed: `examples/herofield.vyrn` pays roughly double the transcendental cost per cell on every frame; at terminal scale this measured ~35 ns per cell, so the practical loss is small.

Smallest fix: export a combined entry point that reduces once and returns both values, keeping `cos` as the single-value wrapper. `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
