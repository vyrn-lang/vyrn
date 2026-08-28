# std/hints.vyrn

Lines: 367. Exports: 5 (`noPolicy`, `policyOf`, `levelOf`, `hint`, `waived`), plus one exported type, `Policy` at `std/hints.vyrn:75`. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A checking library uses `std/hints` to govern its rules. The library parses its configuration once through `policyOf` into a `Policy` of per-code level words, and then wraps each candidate report in `hint`, which drops the report when the project turned the code off or when the author wrote a `vyrn-ignore <code>` marker on the reported line or the line above it. What survives is a `//@diag` line in RFC-0099 form. `std/vyx-hints` is built exactly on this surface (`std/vyx-hints.vyrn:52`).

## Findings

### 2. Algorithm complexity — HIGH

What: every waiver check re-walks the whole source text from byte 0, twice per emitted hint.

Where: `std/hints.vyrn:169` and `std/hints.vyrn:173` (`waived` calls `lineText` for the reported line and the line above); the proving loop is the newline walk at `std/hints.vyrn:227-236`, which starts at `i = 0` no matter which line is wanted. A generator that emits H hints into an N-byte source pays O(H·N).

Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/hints/b.vyrn` from `N:\lang`: `waived` at line 2 of a 1024-line (about 42 KB) source min 10.52 µs, at line 1023 min 25.76 µs; emitting 64 hints at early lines min 142.50 µs, at late lines min 1.07 ms — same hint count, only the line positions differ, so the 7.5× gap is the repeated walks.

Cost if unfixed: `std/vyx-hints.vyrn:300-488` calls `hint` once per rule hit with the full template text, so a template with many hits near its end pays the product of hit count and file size; the measured 1.07 ms for 64 hits in 42 KB grows linearly with both factors.

Smallest fix: make the walk return the text of line L and line L−1 in one pass, or cache a line-offset table per source file for the life of a generator run. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: each non-waived hint allocates several short-lived strings and byte arrays before producing its one output line.

Where: the marker build `"vyrn-ignore \{code}"` at `std/hints.vyrn:185`; the `bytes(marker)` array at `std/hints.vyrn:191`; a substring copy per `lineText` result at `std/hints.vyrn:230` and `std/hints.vyrn:238`; the level-word copy inside `levelOf` at `std/hints.vyrn:128`; and the interpolated message at `std/hints.vyrn:163`.

Evidence: allocation counts NOT MEASURED. End-to-end cost of the path is measured: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/hints/b.vyrn` reports 25.76 µs min for one `waived` call deep in a 42 KB source, and 1.07 ms min for 64 `hint` calls, both dominated by the walk-and-copy sequence above.

Cost if unfixed: `std/vyx-hints.vyrn:300-488` runs this sequence once per rule hit, so a busy template allocates a dozen or more throwaway objects per reported diagnostic.

Smallest fix: match the marker against the code bytes in place without building the concatenated string, and have `lineText` return start and end offsets instead of a copied substring. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
