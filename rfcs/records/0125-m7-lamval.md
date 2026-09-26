#### A lambda literal a call takes as a value is typed, bound and freed (2026-09-26, `m7-lamval`)
RFC-0125, milestone M7.
Decision: the lead's. The track takes the lambda-as-value class of the finish-line probe (`uiPgData__from0`'s `lazy(query(() -> recent().pastes))`) and the capture leak found on the way. Four commits, each with its own witness.
- a, `fix(frontend)`: `check_fn_arg`'s lambda arm recorded the function it hands the callee but not the literal's node, as the bare-name arm does. The core typed the closure `Unit`, and `core_lambda` refused it. The node is recorded as the solved signature.
- b, `fix(codegen)`: the prologue keyed a `consume` parameter's release by its `Param` node, and `instance_shell` and `ho_shell` build copies. So every `consume` parameter of a generic or higher-order instance was never released, on both walks. The key is the declaration's parameter of the same name ([`Cx::declared_param`]). A stored value's one capture at a `consume fn` parameter keeps `consume`.
- c, `feat(lower)`: a literal at a `consume fn` parameter gave the call no targets, so the emitter named no instance. It is `Target::Value`, the stored-value path from m7-fnval2.
- d, `fix(lower)`: `Builder::lambda` named the closure temporary as owning nothing. A literal no target names is a value, and it owns its snapshot.
Went: `uiPgData__from0` from the arm. The probe over `vyrn routes server.vyrn` in examples/bin, fresh generator cache, on c: 0 rows for it. It had 12 in C:/wtsatmp/probe-all.tsv on `aa63e0e1`.
Stayed: the middleware lambda in bin's module state (server.vyrn:35, `return None`), because its frame types `Option<Response>` and the emitter resolves only the structural record, so `core_makes` finds no layout for `None`. That is a naming question in a module-state initializer, not this class.
Lines: `checker.rs` 14,345 to 14,347. `direct.rs` 22,776 to 22,801. `core.rs` 10,567 to 10,567. `memory.rs` 2,525 to 2,547. Refusals: 0 lost / 0 gained. Manifest: 2 rows in d.
Licence, on `05fa618d`:
- Leak witnesses under `VYRN_LEAK_CHECK=1`, core and AST walk, before and after:
  - shape `a-consume-parameter-of-an-instance`: 3 blocks and 40 bytes never freed on each walk, to none (b).
  - shape `a-capturing-lambda-handed-to-a-call-as-a-value`: 1 block and 8 bytes on each walk, to none (d).
- `emit-lowered` over the corpus, a: 23 lines in 11 files moved, each a `lambda` row gaining its type.
- The moved rows of d, from `emit-wat` with c's and d's binaries: langbench `main` gains one release of the closure temporary of `callN(x -> x * 3 + 1, 4)`, which adds the fn-value release function (functions 132 to 134). rest's `routes` gains one, after `notFoundWhen(why -> ..)`, whose callee copies the value. `rest` under the leak check prints the same with both binaries.
- `vyrn check` over the corpus, before and after each commit: 551 to 553 roots, `diff -r` empty each time.
- CLI nextest 683/683. `kernel`, `effects`, `typed`, `coretables`, `wasmhash`: green; `kernel` 27,061 / 0 / 0. `residue`: engine 173 / 0, route 173 / 0, after b, c and d.
- `coredrive` unsharded, alone, at the tip: taken 21,171 of 21,172, the same after b; 0 run apart.
- pins by `VYRN_PIN=write`: `checker-census` typing 2,986 to 2,988 (a); `emitter-census` mapping 12,962 to 12,971 and shared machinery 2,767 to 2,783 (b); `emitter-reads` neither 7,375 to 7,391, the core's rows 3,314 to 3,311, both 10,720 to 10,732 (b); `forms` `Expr::Lambda` 37 to 36 (c).
Port onto the deletion branch `origin/m7-delete` (`5fac9c8c`), core walk only, same four commits; the shapes run one walk (`_is_freed`). There, against the branch's own code:
- CLI nextest: `derived` `routes_shows_the_hand_written_projection_beside_the_derived_surface` and the two leak shapes went from failing to passing; 5 `benching` failures stay, blocked by m7-small's `blackBox` row. `residue`: 29 failed to 10, 0 leaking on both engines. The corpus diff is empty (560 roots). The same 2 manifest rows moved in d, to the same hashes.
- `emit-wat` of examples/bin/server.vyrn succeeds, so the middleware lambda's `return None` is taken there (m7-stream's `core_returns_as_is`).
Time: about 200 minutes: 70 work and tracing, 110 gates, 20 waiting on the plan.
Findings:
- b's defect predates M7 and hid because the corpus hands no owned value to a `consume` parameter of an instance, so no manifest row moved when it was fixed.
- langbench `__vyrn_bench_body_7` is taken as far as the lambda goes: an alias parameter types the literal through `stored_fn_lambda`. It waits on m7-small's `blackBox` row alone.
Left: nothing on the deletion branch; on main, the middleware lambda above.
