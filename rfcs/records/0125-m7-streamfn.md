# A bound parameter a lambda captures is made where it is read (2026-09-25, `m7-streamfn`)
RFC-0125, milestone M7.
Decision: the lead's (stream `map` and `filter`, `m7-boxstream`'s Left). The shape, mine: no new row. `specialize` binds the captured parameter once, before the row that reads it, to `Ctor::Closure` of its target, the row a stored function name already is (`m7-fnval`).
Went: `specialize`'s refusal of a bound parameter read as a value, for one read. Stayed: a bound parameter read twice, which would be made twice; `capturefn`'s `let g = n -> f(n) + 1`, on the arm beside its loop.
Lines: `core.rs` 9,864 to 9,909 (`make_before_read`, 35: the row goes in at the depth of the read). `coredrive.rs` 560 to 561. Refusals: 0 lost / 0 gained. Manifest: 8 rows.
Licence, base `origin/m7-stream` `f7d481d9` against the tip:
- prediction before the build: +14, the specializations of `map`, `filter` and `unfold` over named targets in `membench`, `streamlazy`, `streamops` and `streamunfold` (2, 5, 2, 5). Taken: +17. The three more are the same shape outside `std/stream`: `capturefn`'s `applyAll` and the two `defer`s, which store their `fn` parameter into a record.
- fast `coredrive` with the manifest written: taken 21,139 to 21,156 of 21,172; carried end to end 1,676 to 1,676; 1 byte-identical, 167 run the same, 0 run apart.
- the base failed `coredrive`'s forms pin: it listed `Stmt::IfLet`, which `m7-stream` had taken off the core's column (0 rows there). The tip lists what is true: `Stmt::While` joins (`applyAll`'s loop), `Stmt::IfLet` goes.
- `wasm2wat` with `VYRN_WASM_NAMES=1`, base binary against the tip's, the 8 moved modules: `map` and `filter` in each stream module build the target's closure value in the frame (tag, empty payload) and copy it into the step's capture box, where the arm built it in the box: +2 lines each, frame 160 to 144. The copy retains the captured value, so the three stream modules and `membench` gain the closure's copy helpers: 3 functions and 1 type, +152 to +226 bytes. `unfold`, `defer`, `paramQuery` and the rest move only by the shifted type and function indices. `capturefn` 7,112 to 7,102 bytes.
- speed, `vyrn bench membench.vyrn` stream rows, base and tip interleaved, 3 rounds, best of 3: unfold + take 69.06 to 67.23 us, map over unfold 94.45 to 95.36 us, open and close 65.94 to 66.63 us. Within the rounds' spread of about 5%.
- `kernel` 27,061 / 0 / 0. `effects` 0 unattributed, 0 differ. `typed` 237,084 judged, 0 unjudged. `coretables` and the lowering pin green.
- `residue --ignored`: engine 173 clean, route 173 clean.
- `check-corpus.sh`, base binary against the tip's: `diff -r` empty; 453 accepted, 79 refused.
- CLI suite 678/678 with `VYRN_PIN=write`; no census pin moved.
Time: about 110 minutes: 35 reading, 15 the change, 60 gates, wat and bench.
Findings:
- a captured name is copied into the capture box with a retain, where the arm builds a fresh value in place; a made value read once could be built in the box.
- `m7-stream`'s tip fails `coredrive --ignored` on the forms pin, while #499's CI is green.
Left: building a once-read captured value in its capture box, blocked by the emitter's capture path taking a name as a copy; a parameter captured twice; a lambda target captured (made with its captures as parts, which `core_closure` does not read).
