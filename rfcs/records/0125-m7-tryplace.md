#### A switch reads a scrutinee an enclosing list made (2026-09-25, `m7-tryplace`)
RFC-0125, milestone M7.
Decision: the lead's (tryplace `main`, the census's three `stmt:Switch`). The shape, mine: `bound` in `core_readable` holds every name with a place on the path, the enclosing arms' binders and the names an enclosing list made. It replaces the `ss[..i]` scan in the `St::Switch` arm, so one rule states both.
Went: the same-list scan. Stayed: nothing of this clause; no other refused body met it alone.
Lines: `direct.rs` 22,318 to 22,328, `coredrive.rs` 560 to 562, against `d62c4f6e`. Refusals: 0 lost / 0 gained. Manifest: 1 row (`tryplace`).
Licence, at the tip on `d62c4f6e`:
- cause, from a debug print in the screen's switch arm (not committed): the three refused switches were the `tryField` ones on `doc` and `plain`. The optional projection nests the rest of the body in an arm, and the second switch on `doc` read "placed=false" because its `let` sat in an enclosing list. The emitter's `core_switch` finds the slot through the walk state, so only the screen had the gap.
- prediction sent before the build: taken +1, 1 row, `Stmt::IfLet` back on the forms list. Taken and the row hit. The forms prediction missed: the per-statement count leaves out a body taken whole, so `Stmt::IfLet` stayed off and `Stmt::Break` left too.
- fast `coredrive` with the manifest written: taken 21,139 to 21,140 of 21,172; 1,676 distinct; 0 run apart. The forms table reads 0 on the arm and 0 on the rows for both `Stmt::Break` and `Stmt::IfLet`.
- `wasm2wat` of tryplace, main against the tip: `main` alone moves. The frame goes from 352 to 240 bytes and `memory.copy` from 26 to 9. Locals go from 92 to 126, and the wat from 1,324 to 1,240 lines. The output is the same (`vyrn run`, both binaries).
- shape `a-switch-on-a-name-an-enclosing-list-made`: two `tryField` switches on one made name. It prints 12, and the two walks run the same.
- `vyrn check` corpus diff against N:/wt-core (`ffb24fcc`): the new shape and #506's changes (`excl_alias` column, a new shape) only; 79 refused on both sides.
- `kernel` 27,061 / 0 / 0. `residue --ignored`: engine 173 clean, route 173 clean. CLI suite 678/678; pins by `VYRN_PIN=write`: `emitter-census`, `emitter-reads`.
Time: about 70 minutes: 20 cause, 10 build, 40 gates and wat.
Findings:
- Main `d62c4f6e` fails coredrive on its own: `Stmt::IfLet` was 11 on the arm and 0 on the rows, so the pinned forms list failed. This slice drops `Stmt::IfLet` and `Stmt::Break` from the list, since both arms now emit nothing over the corpus.
Left: nothing on this clause. Retiring the `Stmt::Break` and `Stmt::IfLet` arms is its own slice: they emit nothing over the corpus, and `PIN`'s `jchain` row and the shapes decide whether the arms can go.
