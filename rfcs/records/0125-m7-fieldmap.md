#### A map literal of layout values is built from the rows (2026-09-25, `m7-fieldmap`)
RFC-0125, milestone M7.
Decision: the lead's. A map's layout value is a name with a slot of its own. `map_set` moves its bytes into the entry from the address `core_val` pushes, as it does for the arm's `Expr::Var`, and the map takes the value with its inner buffers.
Went: two screens. `core_made` refused a layout part of a `Ctor::Map`. `core_built` refused a layout part that is a temporary with no binding. Deleting the first alone moved no body: every map literal's value is such a temporary. `core_part_at` still answers `None` for a map parent, so a map's value is never built at an offset.
Prediction, sent before the change: `fieldmut` `main` and its test body taken, 0 run apart. Result: `main` taken (+1). The test body is not in the count.
Lines: `direct.rs` 22,084 to 22,085. `memory.rs` 2,483 to 2,506: a shared `shape_runs_clean` replaced the slotrel test's body. Refusals: 0 lost / 0 gained. Manifest: 1 row, `fieldmut`.
Licence, at the tip on main `ab947e5f`:
- `coredrive --ignored` with the manifest check: taken 21,082 to 21,083 of 21,172. Before the rebase, on `6dbf79e8`, it was 21,076 to 21,077; an instrument over `fieldmut` showed `main` as the one body that moved. 1,664 carried end to end. 1 program byte-identical, 167 run the same, 0 run apart. The new shape `a-map-literal-of-layout-values-the-rows-carry` is at 0 break, 0 continue.
- The moved row, from `wasm2wat`: `fieldmut` `main` takes the core's walk whole. Its frame went from 352 to 256 bytes. The arm built each nested array literal in a slot and copied its 24-byte header into the parent. The core builds it at the part's offset. The module has 12 `memory.copy` instead of 19, and 4,693 wat lines instead of 4,740.
- `kernel`, `effects`, `typed`, `coretables`, `wasmhash` `--ignored`: 6 passed. `kernel` 27,061 / 0 / 0.
- `residue --ignored`: engine 173 clean / 0 leaking, route 173 / 0, 0 failed.
- `vyrn check` over the corpus, with the tip's binary with and without the change: 518 roots, 441 accepted, 77 refused, `diff -r` empty.
- `a_map_literal_of_layout_values_is_freed_on_both_walks`: the shape under `VYRN_LEAK_CHECK=1` prints 720323 on both walks, exit 0, empty stderr. It holds a repeated key `["k": [1], "k": [2, 3]]` (the checker accepts it; the hit releases the first value), a map of a record with a String field, and a nested array value.
- `VYRN_LEAK_CHECK=1 vyrn run fieldmut.vyrn`: both walks print the same bytes, exit 0, no audit line.
- pins by `VYRN_PIN=write`: `emitter-census` the mapping 12,364 to 12,365; `emitter-reads` both, for two questions 10,166 to 10,167.
Time: about 110 minutes: 25 work, 70 gates (one coredrive killed at 600 s under load), 15 rebase and re-gate.
Findings:
- N:/wt-core's tree was ahead of its binary during this track (517 roots, 236 refused), so the corpus base came from this tree with the change reverted.
Left: nothing on this slice.
