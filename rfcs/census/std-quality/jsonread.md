# std/jsonread.vyrn

Lines: 704. Exports: 1. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

The one export is `parseJson(src: String) -> Result<Json, String>` (`std/jsonread.vyrn:513`). The module has no other export kinds. Lines 524-704 are tests and test helpers.

## What this module is for

A caller feeds a UTF-8 JSON document to `parseJson` and gets a `std/json` `Json` tree or a `line N, col M: <reason>` error. The reader is strict: commas required, trailing commas rejected, duplicate object keys rejected by name, the full escape set including surrogate pairs decoded, numbers validated but kept as raw text in `JNum`, field order preserved, nesting capped at 128 levels so deep input is an `Err` instead of a trap.

## Findings

### 8. Allocation frequency — MEDIUM

What: every string and number token allocates a fresh growable byte array, pushes one byte per loop iteration, then `stringFromBytes` copies the whole thing again into the final `String`.
Where: `std/jsonread.vyrn:165` and `std/jsonread.vyrn:244` (fresh arrays), `std/jsonread.vyrn:173` and `std/jsonread.vyrn:289` (second copy). Object keys add a third copy through `key.copy()` at `std/jsonread.vyrn:430` on top of the fields entry at `std/jsonread.vyrn:486`, and each key is hashed twice — once in `ksFind` (`std/jsonread.vyrn:403`) and again in `ksAdd`/`ksPlace` (`std/jsonread.vyrn:450`, `std/jsonread.vyrn:419`). No token reserves its final length even though the span sits contiguous in `p.src`.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/jsonread/b.vyrn` from N:\lang printed min 1.11 ms for build+parse of an array of 4000 plain 22-byte strings against min 28.10 µs for building it, so parsing alone costs about 1082 µs, roughly 270 ns per short string. The escape-heavy variant costs about 666 µs for 4000 strings and the number path about 747 µs for 4000 tokens.
Cost if unfixed: `std/http.vyrn:1118` and `std/http.vyrn:1518` push every request and response body through this path, and `std/graphql.vyrn:1594` parses query bodies with it.
Smallest fix: scan to the closing quote or last digit first, decode into one buffer sized to the span, skip the second copy when no escapes need widening. RECOMMENDATION, NOT A DECISION.

### 22. Vectorization — LOW

What: no scan step processes more than one byte per iteration; whitespace skipping, string scanning, and number scanning all loop byte-at-a-time with a bounds check and a branch per byte.
Where: `std/jsonread.vyrn:97-106` (skipWs), `std/jsonread.vyrn:166-237` (parseString), `std/jsonread.vyrn:256-288` (number digit loops).
Evidence: the same bench run gives about 1082 µs for roughly 92 KB of plain string input, about 85 MB/s on the hottest path; there is no word-at-a-time ASCII scan anywhere in the file.
Cost if unfixed: `examples/langbench.vyrn:235` already benches document decoding, so the gap is visible to anyone profiling the language against peers.
Smallest fix: copy ASCII runs between quotes and backslashes in bulk before falling back to the per-byte escape path. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — LOW

What: parsing holds the full source twice at once — `newParser` copies the whole document via `bytes(s)` while the tree under construction retains every decoded string and raw number text.
Where: `std/jsonread.vyrn:55-56`.
Evidence: NOT MEASURED. The claim is structural: the copy at `std/jsonread.vyrn:55` lives until `parseJson` returns while the output tree grows beside it.
Cost if unfixed: `std/icons.vyrn:207` parses whole icon-collection JSON files, the largest documents any in-repo caller feeds the reader.
Smallest fix: index the caller's string directly instead of copying it into `Parser.src`. RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — LOW

What: the test helper `nest` builds documents by repeated whole-string concatenation, O(n²) bytes copied across the two loops.
Where: `std/jsonread.vyrn:667-681`; the proving loops run n times each with `out = out + open` at `std/jsonread.vyrn:671` and `out = out + close` at `std/jsonread.vyrn:677`, concatenating a string that grows to O(n) bytes.
Evidence: loop bounds above prove O(n²) in n; the wall-clock cost itself is NOT MEASURED. Test-only: callers pass n up to 4000 (`std/jsonread.vyrn:688`, `std/jsonread.vyrn:692`).
Cost if unfixed: none outside this file's own tests.
Smallest fix: build with an `Array<String>` plus `joinWith` from `std/strings`. RECOMMENDATION, NOT A DECISION.

The hot parser itself shows no complexity problem: duplicate-key detection is amortized O(1) through the per-object `KeySet` hash table (`std/jsonread.vyrn:388-455`), and depth is bounded at 128 with an `Err` past it (`std/jsonread.vyrn:318`).

## No finding

No finding: 1, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 30.
