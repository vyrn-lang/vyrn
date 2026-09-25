#### Every rule of the checker's two walks, witnessed by one program (2026-09-25, `m7-typed`)
RFC-0125, milestone M7.
Decision: count before moving. `Checker::stmt` and `Checker::expr` state 60 refusal sites today, and one program witnesses all 60. The lead's brief (`m7-typed`).
Went: nothing. Stayed: all 60, because moving one needs the decision below.
Lines: `checker_census.rs` 772 to 792. `tests/checker-rules.vyrn` is new: 65 functions, one refused statement in each of 60, one body with two refused statements from two groups, and a typed-looking caller of a refused generic function. Refusals: 0 lost / 0 gained. Manifest: untouched. The corpus gains one refused root, this program.
Licence:
- `tests/pins/checker-rules.tsv`: what `vyrn check` prints over the program, 64 diagnostics. The checker stops at a statement's first error and goes on to the next statement, so each sentence prints once. A throwaway build tagged each message with the `cerr!` site that made it (`[site N]`), and each of the 60 sites answered exactly one line.
- the same build over the 492 corpus roots: 2 of the 60 sites are reached, the assign to an immutable binding (`examples/a6_reassign`) and the `drop` of a type parameter (`examples/dropparam`).
- `cargo nextest run --release -p vyrn-cli`: 670 passed. `letswalk`, `lowered_dump`, `columns` and `kernel` `--ignored` passed.
Findings:
- track-er counted 63 sites and 3 `Recorded` writes on 2026-09-11. The 3 writes are the judgment's record, not refusals. Which 3 sites left since then was not traced.
- typed.rs states none of the 60. The kernel states `break` and `continue` outside a loop in other words ("a `break` outside a loop").
- the sites by what they read: a store needs `mut` (3), `break`/`continue` (2), `drop` (4), a literal against its slot (8), a missing-`length` hint (1), an unknown name (5), a failed inference (6), and a check on computed types (31: mismatches, conditions, operands, fields, keys, containers, a missing field, a `where` record field).
- `vyrn check` builds the core only when the checker gave no diagnostic (`vyrn-frontend` `lib.rs`, before `movecheck::refusals`). So a rule stated over the core disappears from a file that has any other checker error, and it uncovers kernel refusals that the checker's error hid. Two witnesses show it: an immutable assign beside `if 1` prints 2 lines today; an immutable assign beside a use after `consume` prints 1.
- `testsweep --ignored` fails on main 513bae71: `refusals.rs` literal #937 is accepted without the kernel and refused with it (`kids` handed to a `consume` parameter). This track does not touch it.
- the checker states every refused statement of a body, not only the first (`Checker::block`). So under decision A a body with a checker refusal and a moved rule's refusal loses the second: `twoPasses` in the census program prints lines 299 and 300 today, and would print 300 alone.
Left: every move, blocked by stage 2 of decision A (#474): the core built for each body the checker typed.

#### The core is built for every body the checker typed (2026-09-25, `m7-typed`)
RFC-0125, milestone M7, decision A stage 2 (#474).
Decision: a program the checker refused in function bodies alone still has the core built for the rest, and the kernel's list is still read only when no pass refused. The lead's staging.
Went: nothing. Stayed: every rule; this stage moves none.
Lines: `vyrn-frontend` `lib.rs` 263 to 351 (+88, `lower_typed`), `checker.rs` 14,962 to 14,988 (+26, the refused set). Nothing is deleted: this is the pipeline the rules leave the checker through. Refusals: 0 lost / 0 gained. Manifest: untouched.
Shape: `check_accum_with_json_types` answers the functions with a refused statement, or `None` where a refusal stands outside a function body. `lower_typed` moves the refused functions out of the program by value, and every function that calls or names one, to a fixpoint. It builds nothing when a test, a bench, module state or an impl method names one, and nothing for a generator's own program. It runs `own::analyze`, drops the kernel's refusals, and moves the functions back in the source's order. No line of `vyrn-lower` changed, so it composes with `m7-slotrel`'s fixpoint in `augment`: a refused body is never in the program the lowering sees.
Licence:
- `vyrn check` over 496 roots on af4c7316, the census commit's binary against this one: byte-identical stderr and exit codes, 419 accepted, 77 refused. 18 refused roots take the new build, and the same 18 under a debug build print the same bytes.
- `checker-rules` pin unchanged. `nextest -p vyrn-cli` 670 passed; `lowered_dump`, `kernel`, `wasmhash` (check), `letswalk`, `columns`, `refusals` `--ignored` passed; `vyrn-frontend` 1,084 passed; `vyrn-lsp` 100 passed.
- pins: `checker-census` shared machinery 2,371 to 2,397 lines; the anchor of `check_accum_with_json_types` follows its new signature.
- time, `vyrn check`, best of interleaved runs: `site/export.vyrn`, accepted, 1,529 to 1,582 ms (8 rounds; medians 2,279 and 2,254, inside the band). The same file with one refused function added: 679 to 1,572 ms (6 rounds), because the core is now built for it; that is the accepted program's cost. The census program: 182 to 313 ms.
Findings:
- the editor pays the same: `load_warned` reaches `check_and_synthesize`, so a keystroke in a file with a checker refusal now builds the core.
- a typed caller of a refused generic function is moved out with it, the lead's fallback, not an invented instance. `callsRefused` in the census program is the witness: once the store rule is the judgment's, its line goes.
Left: the rules, groups 1 to 4 first.
