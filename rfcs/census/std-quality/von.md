# std/von.vyrn

Lines: 1513. Exports: 7. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

std/von reads and writes VON, Vyrn Object Notation: a `.von` document is one `import type { T } from "…"` header followed by one literal value in Vyrn's own record-literal grammar. A caller parses a config file at generation time with `parseVon`, inspects the `VonDoc` tree, and writes canonical, `fmt`-shaped text back with `toVon`; `jsonToVon` migrates a JSON tree to VON text; `copyVonArray`/`copyVonFields`/`copyVonEntries` back the hand-written deep copy behind `impl Copy for Von`. Besides the 7 exported functions the module exports 6 types (`VonField`, `VonEntry`, `Von`, `VonImport`, `VonDoc`, `VonTok`). `parseVon` is a `gen fn` because the compiler's `lex()` builtin is generation-only, so every document failure is a build error positioned `line N, col M:` in the `.von` source.

## Findings

### 2. Algorithm complexity — LOW

What: duplicate detection rescans the accumulated list once per element, and verbatim-number extraction rescans the whole source once per number token.
Where: `std/von.vyrn:364-371` (`fieldLine`, called per field at `:422`), `:374-381` (`entryLine`, called per entry at `:497`), and `lineStartOffset` at `:269-285`, reached for every number token through `rawNumber` at `:307-308`.
Evidence: loop bounds — `fieldLine` walks every stored field for each newly read field, so an n-field record costs n(n−1)/2 String comparisons; `lineStartOffset` restarts its scan at byte 0 (`let mut i = 0`, line 274) for each number token, so k numbers in an L-byte document cost O(k·L) byte steps. Timing MEASURED after the fact — see `rfcs/census/comptime-parsing-quadratic.md`. Both loops are real and both are now indexed, and together they were under a fifth of the cost: the rest was a coercion check in the interpreter, outside this module. The reader is a `gen fn`, so its cost is BUILD time and `vyrn bench` could never have measured it; the bench failure was a separate defect (`rfcs/census/blocked-bench-name-collision.md`) and is fixed.
Cost if unfixed: `examples/lib/gen_von.vyrn:19` (`vonModule`) pays both scans for every `.von` file on every build that loads one; at config-file sizes no in-repo caller hurts today.
Smallest fix: check duplicates against a single growing index keyed by name instead of a linear scan, and carry the running line-start offset into the number rule. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — LOW

What: one parse keeps four whole-document representations live at the same time.
Where: `std/von.vyrn:711` (the full token array from `vonLex`), `:716-722` (every token copied again into an owned `VonTok` row with its own kind and text strings), `:727` and `:732` (`bytes(src)` materialised twice), plus the result tree.
Evidence: structural — peak is about tokens × (row + two strings) + two source-byte arrays + the value tree, all simultaneous; NOT MEASURED.
Cost if unfixed: generation-time builds of documents near the size where token rows dominate pay roughly twice the necessary footprint; same caller as above.
Smallest fix: build `bytes(src)` once and pass it to both consumers, and let the walk consume the token array instead of holding it beside the finished tree. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: every token probe allocates a fresh String copy, and a punctuation or keyword test performs up to two such probes.
Where: `std/von.vyrn:169`, `:177`, `:186`, `:195` (`.copy()` on every `kindAt`/`textAt`/`kindAhead`/`textAhead` call), paired inside `atPunct`/`atKeyword` at `:239-246`, which `readValue` fires several times per token (`:556-591`); `hex2` also rebuilds its digit table per call (`:793`) and `indent` re-concatenates padding per block (`:781-789`).
Evidence: NOT MEASURED for the reader — `compiler/target/release/vyrn.exe bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/von/bemit.vyrn` fails with ``error: field `toks` missing during coercion`` (exit 1) for any scratch file importing std/von, while the identical writer code in a copy with only the reader functions stubbed benches natively, so the blocker sits in this module's reader section. The writer path itself is linear under native benching: emit of a flat array measured min 32.34 µs at 250 elements, 129.35 µs at 1000, 509.05 µs at 4000 (4× elements → 4.0× time, 16× → 15.7×).
Cost if unfixed: every `.von` load through `examples/lib/gen_von.vyrn:19` heap-allocates one to two strings per token probe across the whole document.
Smallest fix: compare tokens by row index against interned constants so probes return a number rather than a copied String. RECOMMENDATION, NOT A DECISION.

### 21. Footprint size — LOW

What: five readers carry an unreachable `return Err(...)` after a `while true` loop whose body always exits by return.
Where: `std/von.vyrn:439`, `:460`, `:480`, `:514`, `:641`.
Evidence: each loop body ends every path in `return Ok(...)` or `return Err(...)` (for example `:410-412` and `:434-436` in `readRecord`), so control cannot reach the statement after the loop.
Cost if unfixed: five dead statements a reader must step past; no runtime cost.
Smallest fix: delete the five unreachable returns. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23, 24, 25, 26, 27, 28, 29, 30.
