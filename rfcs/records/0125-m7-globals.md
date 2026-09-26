#### The module-state initializer is walked from the core's body (2026-09-26, `m7-globals`)
RFC-0125, milestone M7.
Decision: the lead's brief (the largest class of the wide tally). The core builds the module-state body (`build_module_state`, filed under the empty name), and no emitter read it: `lower_globals_init` stored each initializer through `store_into(&g.init)`. It now walks that body where `core_walkable` takes it and no global's type validates. `core_enter` states once, for `lower_body` and the initializer, how a walk holds a body's rows.
Went: the initializer's arm in every compile with module state.
Stayed: a global whose type validates. The module-state body states no check, so such a global stays on the arm. No corpus program has one.
Lines: `direct.rs` 22,650 to 22,672. Refusals: 0 lost / 0 gained. Manifest: 21 rows.
Licence, at the tip on `95d9a531`:
- `VYRN_FORM_TALLY`, core walk on, class "(the globals initializer)", in lines / occurrences. CLI suite on `aa63e0e1`: 443 / 1,328 (`Expr::Str` 551, `Expr::Var` 529, `Expr::Int` 196, `Expr::Bool` 41, `Expr::Binary` 11) in 205 compiles. This commit alone on `aa63e0e1`: 74 / 522, all `Expr::Var`, the fn-value dispatcher's `@aN` reads, which #535 removed. Tip: 0 / 0. `vyrn check` of the 539 roots, a fresh `VYRN_GEN_CACHE_DIR` per root: 59 / 71 on `aa63e0e1`, no arm line at all on the tip over `aa63e0e1`. kernel, effects, typed, coretables, wasmhash and unsharded coredrive: 0.
- Predicted: every initializer form leaves the arm, and only the dispatcher's `Expr::Var` stays until its own fix. Got both.
- the 21 moved rows: `closures2`, `contractquery`, `domdemo`, `fieldmut`, `fnvalstore`, `genericpayload`, `graphql`, `i18ndemo`, `jsonplace`, `membench`, `namedplace`, `placeorder`, `refutablelet`, `regionarena`, `rpcsplit`, `statemod`, `streamlazy`, `streamops`, `streamunfold`, `tryplace`, `validate_store`; the same rows and hashes on `aa63e0e1` and `95d9a531`. From `wasm2wat`, only the initializer differs. `fieldmut`: the frame falls from 64 to 32 bytes, because the second literal is built in the first one's slot after that is copied to its global. `statemod`: each scalar initializer is computed into a local and then stored (5 `i64` locals).
- `vyrn emit-gen` of the 51 roots that import through a generator, `VYRN_NO_GEN_CACHE=1` and a fresh `VYRN_GEN_CACHE_DIR` each, `aa63e0e1`'s binary against the tip's over it: every output and exit code equal (33 non-empty, `gentablefail` exits 1 on both).
- corpus diff against `aa63e0e1` built in this worktree: 539 roots, 460 accepted, 79 refused on both; `diff -r` differs only in `shelf/client/boot`'s gen-cache warning, and that root is equal under a fresh cache.
- unsharded `coredrive` with the manifest check on `95d9a531`: pass, 21,163 of 21,172. kernel 27,061 / 0 / 0; effects 9,939 pure; residue passes. CLI suite 681/681.
- pins by `VYRN_PIN=write`: `lower_globals_init` from `Twice` to `Both` in `emitter_census.rs`; `emitter-reads` (the section moves to "both, for two questions", 13 to 14 sections), `emitter-census` (shared machinery 2,751 to 2,767).
Time: 190 minutes: 15 recovery and cache repair, 35 tallies, 20 work, 90 gates, 30 the rebase onto `95d9a531` and dropping the dispatcher commit, which duplicated #535.
Findings:
- `N:/wt-core`'s release binary (2026-09-25 20:31) was not `aa63e0e1`'s: its corpus output differed from a fresh build of main in `checker-rules` and `excl_alias`.
- the core walk's literal construction leaves `local.get; drop; drop` pairs (32 in `fieldmut` on `aa63e0e1`); the initializer adds 2.
Left: nothing of this class.
