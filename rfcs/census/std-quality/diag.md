# std/diag.vyrn

Lines: 110. Exports: 2. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

Other export kinds: `export type Severity = | Warning | Error` (`std/diag.vyrn:38`).

## What this module is for

A generator (a `gen fn`) returns Vyrn source text, and a line of that text can carry a diagnostic. `report` formats a severity, a file anchor with a 1-based line and column, and a message into one `//@diag` directive line; `reportHere` does the same without a position. The loader lifts the directive out and shows it like any compiler diagnostic. Every checking library in std builds on this: `std/i18n`, `std/rpc`, `std/tw`, `std/ui`, `std/vyx`, `std/vyx-hints`, `std/icons`, `std/graphql`, `std/connect`, and `std/hints` all import it, as does `examples/lib/gen_table.vyrn:18`.

## Findings

### 8. Allocation frequency — MEDIUM

What: every report runs `replace` over the message twice, and each pass rebuilds the whole message into a fresh String even when the message holds no newline and no carriage return — the common case.
Where: `std/diag.vyrn:70`.
Evidence: `replace` always builds a new `out` by concatenation (`std/strings.vyrn:296`, `std/strings.vyrn:311`); it has no pattern-absent fast path that returns the input. Measured with `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/diag/b.vyrn` run from `N:\lang`: `report short message` min 992 ns median 1.83 µs mean 1.76 µs; `reportHere short message` min 550 ns median 912 ns mean 920 ns. The 442 ns gap between the two floors is the anchor construction plus the two replace passes on a clean 34-byte message.
Cost if unfixed: `std/i18n` emits one report per drift issue in a loop (`std/i18n.vyrn:906-909`), so a locale pair with many issues pays this per issue; the other nine std generators pay it on every failure path.
Smallest fix: scan the message bytes once and return it untouched when neither `\n` nor `\r` appears. RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — LOW

What: `oneLine` scans the full message twice, once per pattern, where one fused byte pass suffices.
Where: `std/diag.vyrn:70` calls `replace` twice; each call runs the scan loop at `std/strings.vyrn:301` over the whole message.
Evidence: measured scaling is linear with a doubled constant. Same bench command: `report 1k message` min 7.07 µs, `report 8k message` min 48.52 µs — about 5.9 ns per extra message byte across both passes, against a 992 ns floor for a 34-byte message.
Cost if unfixed: callers composing long messages, chiefly `std/i18n` drift messages built from entry keys and paths, pay twice the necessary scan work per report.
Smallest fix: one loop over the message bytes that copies through, mapping `\n` and `\r` to a space. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
