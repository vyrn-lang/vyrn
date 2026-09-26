#### A generator host is never audited, and the lowering knows it (2026-09-26, `m7-audit`)
RFC-0125, milestone M7, on the deletion branch.
Decision: the lead's. On `m7-delete` (`5fac9c8c`) the residue ratchet failed for 15 generator programs, engine and route: under `VYRN_LEAK_CHECK=1` the generation engine declined every generator. The emitter audited a build when `gen.is_none() && audit_build()`, and the lowering asked `audit_build()` alone. So a generator host's core stated `std/runtime`'s `auditBirth` calls in `runtime$malloc`, and `Fn_::core_sig` refused them (`audit_dropped`). Before the deletion the arm dropped them. `vyrn_frontend::loader::audit_build` now answers false for a generator host (`checker::gen_host`), so both readers get one answer from one place.
Went: the emitter's second condition, and `Fn_::audit_dropped` with its reader in `core_sig`. An unaudited build's core states no hook (`core.rs`'s `Stmt::Expr` arm), so nothing reached it.
Lines: `direct.rs` 17,877 to 17,864; `loader.rs` 4,905 to 4,910. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on `5fac9c8c`:
- `residue --ignored`: FAIL to pass. Before: clidemo, clifail, gendemo, gentable, graphql, i18ndemo, namespace, pagesdemo, rest, rpc, rpcsplit, shadowbuiltin, twdemo, vondemo and vyxdemo each failed on both engine and route.
- `VYRN_LEAK_CHECK=1 vyrn run gendemo.vyrn`, fresh gen cache: before, "the installed generation engine declined it" (`VYRN_GENWASM_TRACE`: "no lowering for a statement of `runtime$malloc` ... at line 357"); after, exit 0, the output of the unaudited run, no audit line.
- CLI suite: 6 failing before and after, the same six (five `benching`, one `derived`).
- `VYRN_WASM_MANIFEST=check` over `wasmhash` and `kernel`: pass.
- pins by `VYRN_PIN=write`: `emitter-census`, `emitter-reads`, `frontend-census`; the `emitter_census.rs` anchor `fn audit_dropped` became `fn is_extern`, the next item in that section.
Time: 45 minutes: 15 trace, 10 work, 20 gates.
