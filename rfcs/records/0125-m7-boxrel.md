# A made function value with captures is released after the lambda that copies it (2026-09-26, `m7-spec2`)
RFC-0125, milestone M7.
Decision: the lead's (the m7-capture record's Left). The shape, mine: `specialize` marks the made value as released and puts its release after the lambda that captures it; the emitter reads a function value's captures into its box as it reads a lambda's.
Went: `applyAll`'s instance for `viaOnward`'s lambda, which the m7-capture record left to the AST walk because its made target owned a capture box that nothing released. Stayed: a made value read by anything but a lambda's capture, which `make_before_read` refuses; a parameter read twice.
Lines: `core.rs` 10538 to 10554, `direct.rs` 22650 to 22667. Refusals: 0 lost / 0 gained. Manifest: 1 row (`capturefn`, the instance's body from the core walk).
Licence, base `origin/main` `95d9a531` against the tip, unsharded:
- `coredrive --ignored` with the manifest check: taken 21,163 to 21,164 of 21,172; `Op::Closure` class 31 to 31; carried end to end 1,679 to 1,679; arm `Stmt::Let` 5 to 4, `Stmt::Return` 0 to 0 (rows 9 to 8), `Stmt::While` rows 1 to 0, `Expr::Var` 23 to 21; 1 byte-identical, 167 run the same, 0 run apart. The forms pin loses `Stmt::While`.
- The new shape `a-lambda-target-with-a-capture-captured-by-a-lambda` runs 63 on both walks. With the `core.rs` hunk reverted, `VYRN_LEAK_CHECK=1 vyrn run` of the shape exits 135, "1 block(s), 8 bytes, never freed", and of `capturefn` "16 bytes"; the AST walk is clean for both.
- `VYRN_LEAK_CHECK=1 vyrn run` of `a2_capture`, `a2_capture_escape`, `capturefn`, `closures2`, `fnvalarg`, `fnvalstore`, `lambdas`, `streamlazy`, `streamops`, core walk and `VYRN_NO_CORE_WALK=1`, main's binary against the tip's: stdout, stderr and exit code equal, no audit line.
- `kernel` 27,061 / 0 / 0. `effects` 0 unattributed, 0 differ. `typed` 237,084, 0 unjudged. `coretables`, `wasmhash` green. `residue` green.
- Restacked by the lead on `05fa618d` (#539 to #541): unsharded `coredrive` with the manifest check takes 21,171 to 21,172 of 21,172, and the forms pin reads empty (`Stmt::Let` and `Stmt::Return` leave with `Stmt::While`); `capturefn` moves to the same hash as on `95d9a531`.
- CLI suite 681/681 with `VYRN_PIN=write`: `emitter-census` and `emitter-reads` moved by 17 lines each.
- `vyrn check` over the corpus roots, main's binary against the tip's: `diff -r` differs only by the new shape, accepted; 545 roots to 546, 462 accepted to 463, 83 refused.
Time: about 190 minutes in this session: 10 the rebases, 20 the shape and its witness, 160 gates, three times, as main moved from `aa63e0e1` to `69f88d13` and `95d9a531`.
Findings:
- `took the core's walk` moved by 1 although the instance already had a core body in part: the `while` was the last statement the arm emitted.
Left: a made function value read twice, or read by a call rather than a capture, blocked by a release placement `make_before_read` does not state.
