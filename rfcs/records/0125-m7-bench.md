#### `blackBox` is a core row, so every bench body is the core's (2026-09-26, `m7-small`)
RFC-0125, milestone M7.
Decision: the lead's. The bench bodies are the probe's `__vyrn_bench_body_*` class, about 110 compiles, and m7-where's `__vyrn_body_0..2` (the `bench --check` doors). `VYRN_GAP_TALLY` over the 17 bench examples on main `06e42e2c`: 77 door bodies gap on `Call:Builtin:blackBox` alone and langbench's `__vyrn_body_7` on `Lambda` and then `blackBox`; its lifted lambda is whole. m7-mapleak found the same over the suite's temporary roots. Built on the deletion branch `m7-delete` at `5fac9c8c`, where no arm is left to fall back to.
Rule: `blackBox` is `Spec::Barrier`, a new kind. No kind stated "one operand, handed back as it is": `Spec::OwnType` is `@copy`, which builds a copy. `Fn_::core_call` writes the store and load. A `let` of a layout from it binds the operand's own address and takes over the operand's slot to the end of its extent, as a rename does. The result type is the checker's at the site. The lead asked for the aggregate `let` as a rename of the operand (`Val::Name`, placed by `core_renames`). The builder cannot state that: whether a type is a layout is the emitter's `Repr`, which the core does not know. A rename would also drop the barrier on the address, which the arm had; nothing measured says it is unneeded.
Fixed: `Builder::hands_back_a_borrow` counts a string literal as lent. The core stated `blackBox("k")` as an owned result and dropped it, so the native audit read `double or foreign free` and every later door in the instance trapped. No body reached this rule before, because each one gapped.
Went: the 78 door bodies and every native bench body. Stayed: nothing of this class. Langbench `__vyrn_body_7` still tallies a `Lambda` gap and emits whole, as m7-mapleak predicted from `core_lambda`.
Lines: `direct.rs` 17,877 to 17,938; `core.rs` 10,588 to 10,594. More than it deletes: the arm's barrier went with the arm, so the core's is new here. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, `m7-int` at `ee805bd5` against it with this commit, then again on `m7-delete`:
- `vyrn bench --check` of the 17 bench examples: 0 of 17 exit 0 to 17 of 17.
- the CLI suite: 10 failed to 5. The 5 `benching` tests pass; `emitter_census` (2), `places`, `derived` and `lowered` fail before and after.
- `VYRN_LEAK_CHECK=1 vyrn bench --json` of the 17: no `free audit` line, exit 0.
- the bench table against main `2b08a1cf`, interleaved, best of three: `hash to 1000` 3,042 to 3,026 ns, `push 1000` 1,264 to 1,253 ns, so the barrier holds.
- on `m7-delete`: `benching` 20 of 20 with the ignored tests; the CLI suite fails `emitter_census` before the re-pin and `derived`, which failed on `m7-int` without this commit too.
- pins: `forms` `Expr::Str` 21 to 22; `emitter-census` the mapping 10,063 to 10,124 lines and 733 to 747 wasm; `emitter-reads` both 6,044 to 6,105, rows 315 to 316.
- on main `2b08a1cf`, with the arm, the same row passed the gate list: kernel 27,061 / 0 / 0, residue 173 clean on both engines, 554 corpus roots byte-identical, the manifest unmoved.
Time: 215 minutes: 40 cause, 50 work and two defects, 60 gates on main, 65 port and gates on the deletion branches.
Findings:
- no gate runs a bench body under the leak audit. The literal's double free passed every suite; only `VYRN_LEAK_CHECK=1 vyrn bench --json` showed it.
- the probe patch no longer applies to main; the form tally was the measure there.
Left: the `Lambda` gap tag on langbench `__vyrn_body_7` (a literal passed to a parameter typed by an alias of `fn`); m7-mapleak re-probes it. A gate that runs the bench bodies under the audit, after the deletion.
