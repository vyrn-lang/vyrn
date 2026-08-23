# std/json.vyrn

Lines: 406. Exports: 6. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller builds a `Json` tree (`JNull`, `JBool`, `JNum`, `JStr`, `JArr`, `JObj`) and serializes it with `emit` (compact) or `emitPretty` (indented), compares two trees with `jsonEq`, and deep-copies trees with `copyJson`, `copyJsonArray` and `copyJsonFields`. `JNum` carries raw validated number text, so `emit` output is byte-stable through a parse and object field order is whatever the tree stores. Generators are the main consumers: `std/tw`, `std/i18n`, `std/openapi`, `std/graphql`, `std/http`, `std/vyx`, `std/symbolmap`, and `site/app/search.vyrn` all call `emit` or `emitPretty`. Besides the 6 `export fn`, the module exports 2 types (`Json`, `JsonField`) and 2 impls (`Copy`, `Owned`). The module imports nothing.

## Findings

### 2. Algorithm complexity — MEDIUM

What: `emitPretty` cost grows at least with the cube of the nesting depth, because `spaces` builds each indentation pad by appending one space at a time to a growing string, once per array/object node per level.
Where: `std/json.vyrn:306-314` (`spaces`: k appends, each copying the growing accumulator), pads taken per level at `std/json.vyrn:318-319` and `std/json.vyrn:334-335`.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/json/b.vyrn` and `.../b2.vyrn`, left-nested `[[[…]]]` chains at indent 2: depth 24 min 13.28 µs, depth 48 min 74.72 µs (5.6x), depth 96 min 592.85 µs (7.9x), depth 192 min 7.42 ms (12.5x per doubling, above the 8x a pure cube predicts). Same 192-deep tree, compact `emit`: min 39.30 µs — the pretty pass costs 189x the compact pass on identical input. Flat arrays scale linearly (compact numArr: 512 → 33.31 µs, 2048 → 136.68 µs, 8192 → 545.60 µs; ratios 4.1x and 4.0x), so the superlinear term is depth-driven, not size-driven.
Cost if unfixed: `std/openapi.vyrn:300` runs `emitPretty(doc, 2)` on every generated OpenAPI document, and `site/app/search.vyrn:181` feeds the writer from the site exporter; today their documents are shallow, so they pay little, but any generator emitting deeply nested schemas pays cubically.
Smallest fix: build each pad once by repeated doubling (or a shared pad per depth memoized across nodes) instead of one-space concatenation. RECOMMENDATION, NOT A DECISION.

### 3. Side effects — LOW

What: `emit` and `emitPretty` trap the whole process when a `JNum` holds text that fails `numberOk`; serialization is not total.
Where: `std/json.vyrn:246-250` (`emitNumber` panics; message built at `std/json.vyrn:248`), reached from `std/json.vyrn:298` and `std/json.vyrn:355`.
Evidence: `compiler/target/release/vyrn run C:/Users/demko/AppData/Local/Temp/claude/ox-a2/json/panic.vyrn` prints `error: json: \`0x1f\` is not a usable number … (std/json.vyrn:248)` and exits 1. The placement is deliberate (`std/json.vyrn:239-245` argues for the single funnel), and `JNum`'s constructor is a public raw `String` (`std/json.vyrn:35`).
Cost if unfixed: every producer of hand-built trees — `std/graphql.vyrn:1650`, `std/http.vyrn:1473`, `std/vyx.vyrn:2258` — turns one bad literal into a crashed reply path instead of an error value.
Smallest fix: none available without changing the exported signature to return `Result`; the current design accepts the trap on purpose. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: every control character below the short-form set makes `emitString` allocate twice per character through `hex2`: the 16-byte digit table rebuilt from a string literal, then a `stringFromBytes` → `bytes` round trip of the 2-digit result.
Where: `std/json.vyrn:131-140` (`hex2` body), call chain at `std/json.vyrn:176-179` inside the per-byte escape loop.
Evidence: NOT MEASURED (isolated cost of `hex2` not benchmarked; control characters did not appear in the measured workloads).
Cost if unfixed: callers escaping binary-ish payloads pay extra allocations per control byte; no hot in-repo caller does this today.
Smallest fix: hoist the digit table to a module-level constant and index it directly into `out` without the intermediate `String`. RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — LOW

What: the hard worst case of `emit` is a trap, and the measured ceiling drifted from the documented one: a value nested 499 levels stops the process, while the header comment says "roughly 450".
Where: `std/json.vyrn:283-293` (the documented limit), recursion at `std/json.vyrn:294-303` and `std/json.vyrn:254-279` (about two frames per level).
Evidence: `compiler/target/release/vyrn run` on a probe that left-nests n arrays and emits: depth 498 prints normally, depth 499 prints `error: call depth exceeds 1000` and exits 1. Parsed input cannot reach the ceiling (`std/json.vyrn:287-289`: `std/jsonread` refuses past 128 levels), so only deliberately built trees hit it, as the comment states.
Cost if unfixed: a program that builds a 500-deep tree on purpose dies at serialize time with no error channel; `examples/jsondepth.vyrn:29` exercises exactly this path.
Smallest fix: correct the comment to the measured boundary (499 traps, 498 survives) or raise the frame budget. RECOMMENDATION, NOT A DECISION.

### 16. Adaptive behaviour — LOW

What: `jsonEq` serializes both trees in full no matter where they first differ, so its best case equals its worst case.
Where: `std/json.vyrn:380-382` (`return emit(a) == emit(b)`).
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/json/b.vyrn` and `.../b2.vyrn` on 2048-element arrays: equal trees min 293.17 µs, trees differing at element 0 min 281.65 µs, differing at element 2047 min 290.37 µs; one `emit` of the same tree alone min 132.55 µs. Position of the first difference changes nothing; the cost is always two full serializations.
Cost if unfixed: `std/jsonread.vyrn:25` imports `jsonEq` and its tests compare parsed trees (`std/jsonread.vyrn:547-576`, `std/jsonread.vyrn:612-625`); test suites pay double serialization on every comparison, and any future runtime caller comparing large trees pays it too.
Smallest fix: a direct structural walk that returns false on the first mismatching kind or field. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 17, 18, 19, 20, 21, 22, 23, 24, 26, 27, 28, 29, 30.
