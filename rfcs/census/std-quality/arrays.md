# std/arrays.vyrn

Lines: 94. Exports: 7. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

Higher-order array helpers written in Vyrn itself: `map`, `filter`, `fold`, `any`, `all`, `includes`, and `sortBy`. A caller imports them to transform, test, or stably sort an `Array` with a lambda or stored function value. In-repo users are `std/i18n` (`includes` for locale key drift checks, `std/i18n.vyrn:43`, called at `std/i18n.vyrn:1107` and `std/i18n.vyrn:1120`), `std/icons` (`includes` against reserved and taken names, `std/icons.vyrn:404` and `std/icons.vyrn:413`), and the examples `examples/knucleotide.vyrn:124` and `examples/closures2.vyrn:119` (`sortBy`).

## Findings

### 2. Algorithm complexity — MEDIUM

What: `sortBy` is an insertion sort, so average and worst case time is O(n^2) in the array length `n`, and the key extractor runs twice per comparison instead of once per element.
Where: `std/arrays.vyrn:73`; the proving loops are the nested `while` at `std/arrays.vyrn:78-92`, and the double key call is `key(out[j - 1]) > key(out[j])` at `std/arrays.vyrn:82`, inside the inner loop.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/arrays/b.vyrn` printed `bench "sortBy random 100" min 1.48 µs`, `"sortBy random 200" min 4.76 µs` (3.2x), `"sortBy random 400" min 19.37 µs` (4.1x) — time grows with the square of `n`. The key-call count is proven by the loop bounds: each inner iteration at line 82 makes 2 calls, and the inner loop runs once per inversion pair, which reaches n(n-1)/2 on adversarial input.
Cost if unfixed: any caller sorting more than a few hundred elements pays quadratically; today's named callers sort small tables (`es` in `examples/knucleotide.vyrn:124` holds one entry per distinct fragment, `unsorted` in `examples/closures2.vyrn:119` holds 3), and `site/app/bench.vyrn:1214` already lists the missing comparator for `sortBy` as a known gap.
Smallest fix: replace the insertion sort with a merge sort or pattern-defeating quicksort behind the unchanged signature, computing each key once into a parallel buffer. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: `map`, `filter`, and `sortBy` each build their result from an empty array with one `push` per element, so every call reallocates the output about log2(n) times during growth.
Where: `std/arrays.vyrn:8-10` (`map`), `std/arrays.vyrn:17-20` (`filter`), `std/arrays.vyrn:74-77` (`sortBy`).
Evidence: no capacity reservation or bulk append exists anywhere in the standard library surface — a search of `std` and `docs/api` for `reserve|withCapacity|capacity` matched no Array method — so growth-by-doubling is the only path. Measured: `vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/arrays/b.vyrn` printed `bench "map 10000" min 30.98 µs`, about 3.1 ns per element including the doubling copies. The separate cost of the reallocations themselves: NOT MEASURED. `site/app/bench.vyrn:1217` already measures the shape of this cost for strings ("five doublings and sixty checked pushes").
Cost if unfixed: `std/i18n` builds and rescans key arrays on every locale compile (`std/i18n.vyrn:1107`), and every hot-loop `map`/`filter` user pays the doublings; the absolute cost today is small.
Smallest fix: add a `reserve` or bulk-append primitive to Array and use it in these three functions. RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — LOW

What: `sortBy` runtime spans a 34x range between its linear best case and its quadratic worst case at the same input size.
Where: `std/arrays.vyrn:78-92`; the early exit `j = 0` at `std/arrays.vyrn:88` gives the best case, full swap chains give the worst.
Evidence: same bench command printed `"sortBy sorted 400" min 814 ns` versus `"sortBy reverse 400" min 27.54 µs` — 34x — and `"sortBy random 400" min 19.37 µs`, so ordinary unsorted input sits near the worst case, not the best.
Cost if unfixed: a caller feeding descending or near-descending data pays the full quadratic path; `examples/knucleotide.vyrn:124` sorts by `0 - e.count`, and count-ordered input is exactly the shape a re-sort of mostly ordered data takes.
Smallest fix: the merge sort from finding 2 removes the input-order cliff entirely. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
