# A borrow's root consumed after the borrow's extent keeps the alias (2026-09-26, `m7-spec2`)
RFC-0125, milestone M7.
Decision: the lead's (site 5 of the J/L/M leftovers, on the deletion draft). The shape, mine: `Fn_::core_alias` judges a chain root handed to `consume` within the borrow's extent, as it judged `modify`, and not anywhere in the body.
Went: refusals.rs `a_take_of_the_place_a_read_binding_reads_is_refused`, whose fixed program (`let xs = b.items.copy()` then `eat(b)`) had no core path. Stayed: a consume inside the extent that the checker accepts, `print(xs[0] + eat(b))`, which the emitter still refuses ("no lowering for a statement of `main` the core did not state"), because the extent is judged per statement and not per row.
Lines: `direct.rs` 17825 to 17820. Refusals: 0 lost / 0 gained. Manifest: not measured; `wasmhash` stops at `pagesdemo.vyrn` (`json$dad3ff2835e8dbe76`, group K) on the tip.
Licence, base `m7-delete-draft` `c4bafda4` against the tip:
- CLI suite, base 657 passed / 25 failed, tip 658 / 24 with `VYRN_PIN=write`: the one difference is the refusals test above. `emitter-census` and `emitter-reads` moved by 5 lines each.
- The unfixed program is refused on the same line with the same sentence, by `check` and `run`: "take.vyrn:6:0: `b` is written here while `xs` still reads out of it".
- The shape `a-borrow-of-a-field-whose-root-is-consumed-after-the-borrow-ends` (a copy, a borrow read before the consume, both in a loop) prints 3112 under `VYRN_LEAK_CHECK=1`; the base refuses it. A borrow read in a `while` and the root consumed after the loop prints 13.
- `kernel`, `effects`, `typed`, `coretables` green. `coredrive` stops at the shape `a move inside a loop that writes the moved name after it`, which sorts after this one.
- On main `2b08a1cf` (commit d197a2f2): CLI 682/682, `kernel` and `wasmhash` green with `VYRN_WASM_MANIFEST=write`, no row moved.
Time: about 70 minutes: 30 locating, 10 the change, 30 gates on two bases.
Findings:
- On the draft, `print(xs[0] + eat(b))` is an accepted program the emitter refuses. The kernel or the checker refusing it, or a per-row extent, is a decision nobody has made.
Left: the per-row extent above, blocked by that decision.
