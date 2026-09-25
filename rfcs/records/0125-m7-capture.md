# A lambda literal handed to a fn parameter is the call's target (2026-09-25, `m7-capture`)
RFC-0125, milestone M7.
Decision: the lead's (the capture bodies still refused, `m7-streamfn`'s Left). The shape, mine: `targets_of` names a literal as `Target::Lambda`, and the row forwards its captures as a pass-through's are.
Went: the closure value typed `Unit` at a `fn` parameter, for a literal at a non-`consume` position. Stayed: `applyAll`'s instance for `viaOnward`'s lambda, whose made target owns its capture box and nothing releases it (a first build that made it leaked 16 bytes); a literal at a `consume` position, which the kernel judges as a value.
Lines: `core.rs` 10039 to 10067, `direct.rs` 22340 to 22397 (`Fn_::core_lift_targets`). Refusals: 0 lost / 0 gained. Manifest: 2 rows (`capturefn`, `lambdas`).
Licence, base `origin/m7-valuecopy` `bf04fe21` against the tip, unsharded:
- prediction: the 5 statements' bodies (`shiftAll`, `zipApply`, `viaOnward`, `lambdas` `main` and `sum`). `took the core's walk` stays 21,158: each body already had a core body, and the count moved where the arm's forms did.
- `coredrive --ignored` with the manifest check: taken 21,158 to 21,158 of 21,172; `Op::Closure` class 36 to 31, `nothing` 21,063 to 21,068; carried end to end 1,676 to 1,679; arm `Stmt::Let` 19 to 16, `Stmt::Return` 4 to 0, `Stmt::If` 4 to 3, `Stmt::Expr` 17 to 15; 1 byte-identical, 167 run the same, 0 run apart. The forms pin gains `Stmt::If`.
- `VYRN_LEAK_CHECK=1` runs of `capturefn`, `lambdas`, `fnvalarg`, `fnvalstore`, `closures2`, `streamops`, `streamlazy`, core walk against `VYRN_NO_CORE_WALK=1`: same output and exit code.
- `kernel` 27,061 / 0 / 0. `effects` 0 unattributed, 0 differ. `typed` 237,084, 0 unjudged. `coretables`, `lowered_dump` green. `residue`: engine 173 clean, route 173 clean.
- CLI suite 680/680 with `VYRN_PIN=write`: `emitter-census`, `emitter-reads`, `forms`, `surface` moved. The first build lost the refusal "a closure at a consume fn parameter captures a borrow"; the `consume` screen restored it.
- `vyrn check` over the corpus roots, base binary against the tip's: `diff -r` empty; 458 accepted, 79 refused.
Time: about 150 minutes: 40 reading, 30 the change and two wrong turns, 80 gates.
Findings:
- `took the core's walk` counts a body with any core statement, so a slice that empties a partly taken body moves only the arm's forms and the gap classes.
Left: a lambda target with captures bound as a value (`applyAll` for `viaOnward`), blocked by a release for the made value's capture box; a parameter read twice.
