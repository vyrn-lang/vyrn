# A borrow's root consumed after the borrow's extent keeps the alias (2026-09-26, `m7-spec2`)
RFC-0125, milestone M7.
Decision: the lead's (site 5 of the J/L/M leftovers, on the deletion draft; then (a), the extent per row). The shape, mine: `Fn_::core_alias` judges a chain root handed to `consume` within the borrow's extent, as it judged `modify`, and not anywhere in the body; the extent it judges ends at the last row that reads the name or a borrow read out of it.
Went: refusals.rs `a_take_of_the_place_a_read_binding_reads_is_refused`, whose fixed program (`let xs = b.items.copy()` then `eat(b)`) had no core path. Went too, in the second commit: `print(xs[0] + eat(b))`, which the checker accepts and the emitter refused, because `extent_ends` holds the borrow while the temporary copied out of it lives. A scalar that owns no heap copied out of the borrow holds nothing of it. Stayed: nothing in this class; `print(xs[0] + eat(b).toString())` over `Array<String>` is the checker's refusal ("`b` is written here while `xs[0]` still reads out of it").
Lines: `direct.rs` 17825 to 17820, then 17820 to 17844. Refusals: 0 lost / 0 gained. Manifest: not measured; `wasmhash` stops at `pagesdemo.vyrn` (`json$dad3ff2835e8dbe76`, group K) on the tip.
Licence, base `m7-delete-draft` `c4bafda4` against the tip:
- CLI suite, base 657 passed / 25 failed, tip 658 / 24 with `VYRN_PIN=write`: the one difference is the refusals test above. `emitter-census` and `emitter-reads` moved by 5 lines each.
- The unfixed program is refused on the same line with the same sentence, by `check` and `run`: "take.vyrn:6:0: `b` is written here while `xs` still reads out of it".
- The shape `a-borrow-of-a-field-whose-root-is-consumed-after-the-borrow-ends` (a copy, a borrow read before the consume, both in a loop, and `ds[0] + eat(d)`) prints 43112 under `VYRN_LEAK_CHECK=1`; the base refuses it. Alone, `print(xs[0] + eat(b))` prints 4, `r.k + eat(b)` and `r.s.byteLength + eat(b)` over a record field print 4 and 6, a borrow read in a `while` with the root consumed after the loop prints 13; none reports a leak.
- Second commit: CLI suite 658 / 24, the same 24; `emitter-census` and `emitter-reads` moved by 24 lines, and the reads row "both" by one read (313 to 314); `kernel` 27,061 / 0 / 0, `effects` 0 differ, `typed` 0 unjudged, `coretables` green.
- `kernel`, `effects`, `typed`, `coretables` green. `coredrive` stops at the shape `a move inside a loop that writes the moved name after it`, which sorts after this one.
- On main `2b08a1cf` (commit d197a2f2): CLI 682/682, `kernel` and `wasmhash` green with `VYRN_WASM_MANIFEST=write`, no row moved.
Time: about 100 minutes: 30 locating, 25 the two changes, 45 gates.
Findings:
- `extent_ends` extends a name's extent by every read rooted at it, a copied scalar too, so the kernel's extent is longer than what the alias needs. The alias trims its own; the kernel's is unchanged.
Left: nothing.
