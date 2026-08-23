# std/scan.vyrn

Lines: 394. Exports: 14 `export fn`. The module also exports one record type, `Scanner` (`std/scan.vyrn:20`). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

`std/scan` (RFC-0054) is one comment- and string-aware cursor over foreign text — CSS, SDL, GraphQL-ish input, generator templates. A caller builds a `Scanner` with one of the three constructors, then walks it with `advance`/`skipWs`, and cuts tokens with `ident`, `quotedString`, `until`, `untilStr`, and `balanced`, which never end a scan at a delimiter that hides inside a quoted string or a comment. All offsets are byte offsets (`std/scan.vyrn:7`). Callers today: `std/tw` imports the cursor for its CSS value gate (`std/tw.vyrn:54`), `std/graphql` builds a `#`-comment scanner on it and parses every incoming query with it (`std/graphql.vyrn:79`, `std/graphql.vyrn:954-958`, `std/graphql.vyrn:1811`), and `examples/scan.vyrn` exercises it.

## Findings

### 2. Algorithm complexity — LOW

What: `untilStr` is O(n·m): it steps one lexical unit per iteration and re-runs a byte-by-byte compare of the full stop string at every step.

Where: `std/scan.vyrn:297-306` calls `looksAt(sc, stop)` at line 300 once per unit; `looksAt` compares up to m bytes byte-by-byte (`std/scan.vyrn:120-133`).

Evidence: bench over 100 KB where the stop's first byte matches at every position — `untilStr` with stop length 1–2 costs min 1.60 ms / median 1.88 ms, stop length 8 costs min 1.68 ms / median 1.98 ms; command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/scan/m.vyrn`. The m-term is real but small (~5% at m=8) because per-byte overhead dominates (finding 24). Worst case for every walk function is bounded by n; unterminated strings and comments consume to end of input by design (`std/scan.vyrn:160-161`, `std/scan.vyrn:256-257`).

Cost if unfixed: `std/graphql.vyrn:1811` pays the O(n·m) walk on every GraphQL request body; long queries pay more per multi-byte delimiter.

Smallest fix: test only `stop[0]` before calling `looksAt`, or route the search through `std/strpred`'s prepared skip-table search (`std/strpred.vyrn:178-183`) — RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: every token-producing call allocates a fresh `String` through `slice`, and every constructor copies the whole source string into the cursor.

Where: nine `slice` result allocations at `std/scan.vyrn:224`, `std/scan.vyrn:249`, `std/scan.vyrn:257`, `std/scan.vyrn:288`, `std/scan.vyrn:292`, `std/scan.vyrn:301`, `std/scan.vyrn:305`, `std/scan.vyrn:335`, `std/scan.vyrn:344`; source copies at `std/scan.vyrn:42`, `std/scan.vyrn:59`, `std/scan.vyrn:84` (plus marker copies at `std/scan.vyrn:88-90`). RFC-0092 documents the copy as deliberate ownership (`rfcs/RFC-0092-a-projection-is-a-borrow.md:661-663`).

Evidence: tokenizing 10,000 identifiers from 100 KB costs min 742.50 µs (~74 ns per identifier including one `String` allocation each); scanning 10,000 quoted strings costs min 773.90 µs; copying the 100 KB source into a scanner costs min 14.49 µs. Command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/scan/b.vyrn`.

Cost if unfixed: one heap allocation per token per parsed GraphQL query in production (`std/graphql.vyrn:1263`, driven from `std/graphql.vyrn:1811`), and two live copies of each scanned document in `std/tw` and `std/graphql`.

Smallest fix: none inside this module's API contract, which returns owned `String`s by design; a borrow-based cut would be a language-level decision under RFC-0092 — RECOMMENDATION, NOT A DECISION.

### 24. Branch predictability — MEDIUM

What: `until` re-tests quote, block-comment, and line-comment configuration for every single byte, even when no comment or quote appears anywhere in the input.

Where: `until` calls `skipUnit` per byte (`std/scan.vyrn:290`); `skipUnit` runs the three-way chain `isQuoteByte` → `skipBlockComment` (a `looksAt(blockOpen)` call) → `looksAt(lineComment)` at `std/scan.vyrn:263-278`.

Evidence: `until` over 100 KB with no comments and no quotes costs min 1.47 ms (~15 ns per byte); the tight whitespace loop `skipWs` over 100 KB of spaces costs min 261.17 µs (~2.6 ns per byte) — about 5.6x slower per byte for the same walk distance. Command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/scan/b.vyrn`.

Cost if unfixed: `std/tw` scans every CSS leaf value of every styled class through this path at generation time (`std/tw.vyrn:54`), and `std/graphql` scans every request body through `skipWs`-heavy parsing (`std/graphql.vyrn:997`, `std/graphql.vyrn:1263`).

Smallest fix: hoist the disabled-marker cases out of the loop — when both markers are empty and quotes are off, run a plain byte-compare loop — RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: constructing a scanner copies the entire source plus all three marker strings before any scanning starts.

Where: `src.copy()` at `std/scan.vyrn:42`, `std/scan.vyrn:59`, `std/scan.vyrn:84`; marker `.copy()` calls at `std/scan.vyrn:88-90`; the copy cost repeats across the three near-identical constructors (`std/scan.vyrn:40-53`, `std/scan.vyrn:57-70`, `std/scan.vyrn:74-95`).

Evidence: `newScanner` over a 100 KB source costs min 14.49 µs per construction (command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/scan/b.vyrn`); peak memory effect of holding source twice is structural, not RSS-measured — NOT MEASURED.

Cost if unfixed: callers that build many short-lived scanners over large documents pay one full copy each; today's callers scan compile-time documents and per-request queries, so the paid cost stays small.

Smallest fix: have `newScanner` and `cssScanner` delegate to `scanner()` so the duplication shrinks to one literal, and let `scanner()` take a consumed `String` to drop the copy — RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 25, 26, 27, 29, 30.
