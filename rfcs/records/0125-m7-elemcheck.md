# A store into a validated element or map value states its constructor (2026-09-26, `m7-spec2`)
RFC-0125, milestone M7.
Decision: the lead's (the class from m7-atrhs's census, `vyrn-places/interp-store-validated:8`). The shape, mine: `Builder::index_set` asks `Builder::checked`, as `Stmt::Assign` does, in place of `Builder::proven_val`.
Went: an element or map-value store whose crossing into a validated type the checker did not prove. The core passed the raw value, the emitter's store screen refused it, and the body stayed on the AST walk (`t.xs[k] = k + 18` into `Array<Age>`). Stayed: an owned value read from a place or lent, which `checked` leaves to the reader, as it does for a store to a name.
Lines: `core.rs` 10538 to 10543. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, base `origin/main` `06e42e2c` against the tip, unsharded:
- prediction, sent before the gates: no corpus count moves (the corpus has one such store, `mapdemo`'s `counts["hits"] = 7`, which the checker proves), 0 manifest rows, two new shapes.
- `coredrive --ignored` with the manifest check: every count equal; the two shapes join, 0 break and 0 continue each.
- `a-store-into-a-validated-element-and-a-validated-map-value` prints 4322 on both walks. `a-store-into-a-validated-element-out-of-its-range` exits 1 with "error: validation failed for `Age`" on both walks and under main's binary. Main's binary tallies each shape's `vyrnTestMain` on the arm (`VYRN_FORM_TALLY`); the tip's tallies nothing.
- The `places.rs` program prints 25 on both walks under `VYRN_LEAK_CHECK=1`, with an empty tally; a map-value store of a failing value traps with the same sentence on both walks.
- `VYRN_LEAK_CHECK=1 vyrn run` of `mapdemo` and the six `validate*` examples, core walk and `VYRN_NO_CORE_WALK=1`, main's binary against the tip's: stdout, stderr and exit code equal, no audit line.
- `kernel` 27,061 / 0 / 0. `effects` 0 unattributed, 0 differ. `typed` 237,084, 0 unjudged. `coretables`, `wasmhash` green with `VYRN_WASM_MANIFEST=write`: no row moved.
- CLI suite 681/681 with `VYRN_PIN=write`: no pin moved.
- `vyrn check` over the corpus roots, main's binary against the tip's: `diff -r` differs only by the two new shapes, both accepted; 545 roots to 547, 462 accepted to 464, 83 refused.
Time: about 60 minutes: 15 reading, 10 the change and its shapes, 35 gates.
Findings:
- Proven crossings come out as before: `proven_val` bound the same `checked_temp` where the checker proved one.
Left: nothing in this class.
