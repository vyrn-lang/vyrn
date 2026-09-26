#### A higher-order call's capture aliases die with the call (2026-09-26, `m7-mapleak`)
RFC-0125, milestone M7. Issue #526.
Decision: the lead's. The track owns #526, the flaky double free the m7-fnval2 record found in `twice(scale(2), 5)`.
Cause: `ReleasePlan` keys its rows by AST node address, and a clone reaches its original's rows through the alias map. `resolve_fn_arg` clones a `fn`-typed argument that is no name (a field, an element, a call's result) into its capture source and registers the clone's addresses as scoped aliases. `ho_call` took its watermark after `resolve_fn_arg` ran, so `alias_unwind` removed only its own pairs and left the capture source's behind. The source vector dies when `ho_call` returns. A later node the allocator placed at one of its addresses then resolved through the stale alias to the row of the caller's argument `scale(2)`, and emitted that row's release. In `twice`'s instance the release landed on the `fn` parameter between the two `apply` calls, so `g` was freed and then read and freed again.
Why it varied: whether a later node lands on a dead clone's address depends on where the heap places each allocation in that process. One binary compiling the same file 40 times, AST walk, `VYRN_LEAK_CHECK=1`, gave 3 different modules: 34 correct, 6 with the extra `call` to the release in `twice`. I did not find what makes the placement vary between processes; the Windows heap's randomized block choice is a guess. The m7-fnval2 record's reading that the bytes were stable and the variance was at run time was wrong.
Fix: `ho_call` takes the watermark before any argument is resolved, so the unwind removes every alias the call registered. `direct.rs` 22,710 to 22,715.
Went: `twice` back into the shape `a-stored-function-value-passed-to-a-higher-order-function` as the witness. Stayed: nothing.
Lines: `direct.rs` 22,710 to 22,715; the shape 6 to 8. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on `aa63e0e1`:
- Without the fix, this tree's binary: the shape under `VYRN_LEAK_CHECK=1`, 40 runs per walk: core 40 clean, AST 36 clean and 4 "free audit: double or foreign free". `emit-wat`, 40 compiles per walk: core 1 module, AST 3.
- With the fix: 40 of 40 clean runs on each walk, output 0, 4 and 321353. `emit-wat`, 40 compiles per walk: 1 module each; the AST one is the correct module above.
- `a_stored_function_value_passed_on_is_freed_on_both_walks` in the CLI suite: 682/682.
- `coredrive --ignored`, unsharded, alone, with the manifest check: passed, taken 21,166 of 21,172, 0 run apart. `wasmhash` check: passed.
- pins by `VYRN_PIN=write`: `emitter-census` the mapping 12,922 to 12,927; `emitter-reads` both 10,563 to 10,568.
- On `95d9a531`, after one more rebase: the watermark still comes first in `ho_call`, ahead of `resolve_fn_arg`. 40 of 40 clean runs on each walk. `direct.rs` 22,728 to 22,733. `emitter-census` the mapping 12,930 to 12,935; `emitter-reads` both 10,581 to 10,586.
Time: about 40 minutes: 15 work, 25 measurement.
Findings:
- `target_call` and `ho_call` both clone argument expressions for the plan. A clone that outlives its alias scope, or a scope that starts after a clone's pairs, emits a release for a node the program never wrote. Keying the plan by address makes such a defect depend on the allocator.
Left: nothing on #526.
