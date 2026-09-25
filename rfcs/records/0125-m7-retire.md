#### A `drop` the reader wrote is its statement's row, and the `Stmt::Drop` arm is retired (2026-09-25, `m7-retire`)
RFC-0125, milestone M7.
Decision: the lead's. Retire the AST arms with the fewest owners, one commit each: `Stmt::Break` (0 owners on the core walk), then `Stmt::Drop` (1).
Count, with `VYRN_FORM_TALLY` over the CLI suite and the ignored suites on `origin/m7-shard` `c3561c12`, core-walk lines only: `Stmt::Break` 0, `Stmt::Drop` 4 in one owner, `closures2.vyrn` `main` (`drop local`), which stays on the arm by its lambdas.
- `Stmt::Drop`: a source `drop` lowered to `St::Drop(n, Site::None, line, None)`. It named no node, so `rows_by_statement` had no run for it. With a node, `core_head` still found no head, because it skipped every trailing `St::Drop` as a temporary's release.
- The rule: a `drop` the reader wrote carries its statement's node, and a release with a source line is its statement's own row. Line 0 marks a placed release, the convention `kernel.rs` and `typed.rs` already read. The plan fold skips a source drop, so it does not enter `discarded`.
Went: the `Stmt::Drop` arm (6 lines), now the retired stub it shares with `Stmt::Continue`. `FORMS` flags it `false`.
Stayed: `Stmt::Break`'s arm. With its flag `false` and the arm kept, the no-core-walk licence walk still reaches it twice: `until` in the shape "a for left early over the elements it never reached". There the `for` is the ForIn arm's and the `break` must release the unreached elements through that arm's counter locals, which no row names.
Lines: `direct.rs` 22,318 to 22,318; `core.rs` 9,981 to 9,986; `coredrive.rs` 602 to 604. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on `origin/m7-shard` `6db4e9f5` (the code of `f2087e5b`):
- coredrive, unsharded, with `VYRN_WASM_MANIFEST=check`: taken 21,141 of 21,172 on the base and at the tip. Carried end to end 1,676. 167 run the same, 0 run apart. The retired-arm assertion passes. The carrying-forms pin adds `Stmt::Drop`.
- `VYRN_FORM_TALLY` at the tip over coredrive and the CLI suite: `Stmt::Drop` 0 on both walks. With only the flag `false` and the arm kept, coredrive read 0 as well.
- `wasmhash` check green. `vyrn check` over the corpus, base against tip: 533 roots, `diff -r` empty.
- kernel 27,061 / 0 / 0. residue: engine 173 clean / 0 leaking, route 173 / 0.
- `VYRN_LEAK_CHECK=1 vyrn run` of the 28 examples with a `drop` statement, base against tip: stdout, stderr and exit code equal, no audit line.
- pins: `emitter-reads` both 109 to 110; `forms` `Stmt::Drop` 14 to 15.
Time: 150 minutes: 50 work, 80 gates, 20 measuring the two walks.
Findings:
- a form retires only when both walks read zero. `FORMS`' flag also hands the no-core-walk licence walk to the rows, so the core walk's zero is not enough: `Stmt::Break` read 0 there and 12,462 on the licence walk before the flag, then 2 after.
Left: `Stmt::Break`, blocked by a `for` the licence walk emits from the ForIn arm around a `break` that releases unreached elements. It goes after `Stmt::ForIn`, or with a licence walk that states the loop from the rows.
