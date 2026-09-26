#### A `let` of a heap element of a temporary is a copy (2026-09-26, `m7-stream`)
RFC-0125, milestone M7.
Decision: the lead's. `let a = pieces()[0]` copies a heap element out of the temporary, and the temporary is released whole at the statement's end. A scalar element needs no copy. The rule is #512's and #521's, stated in `Builder::copies`.
Went: the leak of every `let` bound to a heap element of a call result (`pieces()[0]`, `radarRings()[i]`, `split(doc, "<body")[0]`, `args()[0]`). The core stated a Builtin `@at` that no row named, and the name got no place. The copy now drains its operand as a call does, so the placer keys the temporary's drop to its producer; the AST walk frees that argument temporary after `copy_stack` or `copy_at`.
Stayed: the same element in a take position that is no `let`: `return words()[1]`, `xs.push(words()[0])`, `R { s: words()[0] }` and `s = words()[0]`. Each leaks 2 or 3 blocks on both walks, on main and at the tip. `bag()[0]` of a user container is "the core cannot state a projection whose yield is not a place", and `pieces()[1].lines` is the kernel's "released whole although a `consume` took `.[].lines`", both on main too.
Lines: `core.rs` 10,534 to 10,546, `direct.rs` 22,632 to 22,637. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence:
- `VYRN_LEAK_CHECK=1`, walk on and off, main's binary against the tip's. The three shapes, run with a `main` that prints: `a-heap-element-of-a-call-result-bound-by-let` 222108 on both, 21 blocks leaked on main and none at the tip; `...-at-a-computed-index` 555, 42 blocks to none; `an-argument-bound-by-let-off-args` with one argument 3, 2 blocks to none. A probe of nested, loop, `continue`, `Map` and `for`-body forms also runs clean at the tip.
- `VYRN_FORM_TALLY` over `vyrn test` of `site/app/hl.vyrn`, `site/app/bench.vyrn` and `site/export.vyrn`: `__vyrn_body_5` (7 arm forms over the two app files) and export's `__vyrn_body_9` (12) are gone, and no other row moved. hl 6 passed, bench 23 passed, export 34 passed and 1 failed on both binaries (the release-tag test, which reads the worktree's tags).
- `emit-gen` of 235 roots (`examples/`, `site/`, `site/app/`), each with its own cold `VYRN_GEN_CACHE_DIR`: byte-identical, 29 with generated output, `gentablefail` refused on both.
- `vyrn check` over 542 roots, main's binary against the tip's: equal, 463 accepted.
- `coredrive --ignored`, alone, unsharded, manifest check: taken 21,163 of 21,172, on 95d9a531 and at the tip; 352 s after the rebase. The three shapes read 0 break, 0 continue.
- `kernel` 27,061 / 0 / 0. `effects` 29,619 judged. `residue --ignored` passes. CLI nextest 681 run, 678 passed and the 3 pin tests passed once re-pinned; frontend, lower and codegen 1,111 passed.
- pins by `VYRN_PIN=write`: `emitter-census` the mapping 12,852 to 12,857 lines; `emitter-reads` both 10,504 to 10,509; `forms` `Expr::Call` 90 to 91, all 893 to 894, on 95d9a531.
Time: 120 minutes: 55 tracing and work, 65 gates.
Findings:
- The `.copy()` spelling was clean on both walks only because the call drained its argument; the `let` drained nothing, so the receiver's producer drop was never placed.
Left: the take positions above, blocked by a fact the AST walk reads at a return, a push argument, a literal part and a store, as `Facts::copies` is at a `let`.
