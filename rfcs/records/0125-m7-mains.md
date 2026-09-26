# A body lifts its lambda targets once, before the screen (2026-09-26, `m7-atrhs`)
RFC-0125, milestone M7.
Decision: the lead's (G1 of the `main`-body class). The shape, mine: `Fn_::core_lift_targets` takes the body's rows and `lower_body` asks it once, before the whole-body screen. The screen asked `Cx::lambda_sig` of a literal the per-statement arm had not lifted yet, so a body that handed a lambda to a `fn` parameter went to the arm statement by statement. A literal lifted there takes its captures from the row's `Target::Lambda`, because no capture is in scope before the first statement.
Went: the per-statement lift in `Fn_::core_took`. Stayed: the arm's own lift in `Fn_::lift_lambda`'s other two callers, which lower a literal where it stands.
Lines: `direct.rs` 22,632 to 22,647. Refusals: 0 lost / 0 gained. Manifest: 1 row (`lambdas`).
Licence, base `origin/main` `aa63e0e1` against the tip, unsharded:
- `coredrive --ignored` with the manifest check: taken 21,163 to 21,168 of 21,172; arm forms unchanged (`Stmt::Let` 5, `Stmt::Expr` 12); the core's rows per statement `Stmt::Let` 45 to 38, `Stmt::Return` 9 to 4, `Stmt::If` 1 to 0, `Stmt::Expr` 30 to 24; 1 byte-identical, 167 run the same, 0 run apart. The forms pin loses `Stmt::If`.
- `lambdas` row: `wasm2wat` of both modules holds the same 43 functions, equal modulo index. The lifted lambdas are emitted before `main`'s later callees, so function and type indices renumber.
- the shape `a-body-taken-whole-lifts-a-lambda-that-captures-a-later-let`: 1008, and 1008 on wasmtime.
- `VYRN_LEAK_CHECK=1` runs of `capturefn`, `lambdas`, `fnvalarg`, `fnvalstore`, `closures2`, `streamops`, `streamlazy`, core walk against `VYRN_NO_CORE_WALK=1`: same output and exit code.
- CLI suite 681/681 with `VYRN_PIN=write`: `emitter-census` and `emitter-reads` moved. `kernel`, `effects`, `typed`, `coretables`, `wasmhash` `--ignored`: green.
- `check-corpus.sh`, base binary against the tip's: `diff -r` differs by the new shape only; 461 accepted, 79 refused.
Time: about 70 minutes: 10 rebase and reading, 5 the shape, 55 gates (coredrive three times: head, base, head).
Findings:
- the earlier session predicted +9 bodies over the whole gate list (the 8 lambda shapes, `lambdas` `main`, `sortWith`'s `__vyrn_body_0`); `coredrive` counts +5. This session did not trace which 5.
Left: G3, `a-move-inside-a-loop-that-writes-the-moved-name-first`, a slice of its own.

# A move inside a loop takes the moved name's place (2026-09-26, `m7-atrhs`)
RFC-0125, milestone M7.
Decision: the lead's (G3). `core_after` gave up where a loop held the `let`, because the loop runs its earlier rows again. They run only after the renamed name's extent ends: the `let` in the loop binds it anew each turn, and the kernel refuses a move in a loop of a name bound outside it. So `core_after` climbs through a loop as through a block.
Lines: `direct.rs` 22,647 to 22,648. Refusals: 0 lost / 0 gained. Manifest: 1 row (`vyxdemo`).
Licence, base `aa63e0e1` against the tip:
- prediction: the shape's body. Measured: it, and vyxdemo `listing` (`Listing.vyx`'s `v-for`). `VYRN_FORM_TALLY` of vyxdemo: `listing` 4 arm occurrences to 0 (`Stmt::ForIn` 2, `Expr::Var` 2).
- `vyxdemo` row, `VYRN_WASM_NAMES=1` and `wasm2wat`: 54 functions, only `listing` differs. Its frame falls from 192 to 112 bytes and `memory.copy` from 6 to 3: the rename drops the 24-byte copy of the built array, and the element is read at its address. The core's two `for` loops carry their bounds check, as every loop the core walk takes does.
- vyxdemo under `VYRN_LEAK_CHECK=1` on both binaries: the same 846 bytes, exit 0.
- the shapes `...-writes-the-moved-name-first` (now taken; 6) and the new `...-writes-the-moved-name-after-it` (a store after the move keeps the arm; 36): both binaries, core walk on and off, `VYRN_LEAK_CHECK=1`, no leak line.
- unsharded `coredrive` with the manifest check: 21,168 of 21,172, pass; it counts `listing` among the bodies taken before.
- CLI suite 681/681 with `VYRN_PIN=write`; `kernel`, `effects`, `typed`, `coretables`, `wasmhash` green; `residue` green. `check-corpus.sh` against the base: the two new shapes only.
Time: about 45 minutes: 10 reading, 5 the change, 30 gates.
Left: the 3 stored-value bodies (m7-mapleak) and the `@at` site test bodies (m7-stream).
