#### A stored fn's dispatcher pushes its operands from their places (2026-09-26, `m7-leftovers`)
RFC-0125, milestone M7.
Decision: the lead's (the leftovers of the wide tally: the owner-less arm code, the sortWith closure body, four test bodies). The shape, mine: `lower_dispatcher` pushes each parameter and capture from its place through `Fn_::emit_call_with`, the operand form that `Fn_::emit_call` delegates to. `Fn_::push_place` states once how a place's value is read.
Went: the `@a{i}` and `@c{i}` scope entries and their synthetic `Var` arguments. Stayed: the owner-less `Str("")` and `Bool(false)`, and the five test bodies, for the reasons below.
Lines: `direct.rs` 22,591 to 22,614, against `92d552e5`. More than it deletes: the operand form of `emit_call` and `push_place`, each stated once. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on `92d552e5`:
- cause, from an `ARM` debug print at `Fn_::expr`'s count (not committed) over `vyrn check` of every example with a cold generator cache: the owner-less arm expressions were `Var @a0`/`@a1` (the dispatcher), then `Str("")` 8 and `Bool(false)` 1. After the change the same run prints no `@a` or `@c`.
- `wasmhash` with `VYRN_WASM_MANIFEST=check`: pass, so every example is byte-identical.
- CLI suite 680/680. `vyrn-genwasm` tests pass. Pins by `VYRN_PIN=write`: `emitter-census`; `emitter-reads` (`lower_fnval_copy` moves from "the source, and the core has no row" to "neither"); `forms` (`Expr::Var` 123 to 121).
Time: about 110 minutes: 50 causes of all five items, 20 build, 40 gates.
Findings:
- `Str("")` and `Bool(false)` are module-state initializers (`lower_globals_init` through `store_into(&g.init)`), not emitter-made code: the whole "globals initializer" class of the wide tally.
- The sortWith body: the screen asks `lambda_sig` for an inline lambda target before anything has lifted it, so `core_sig` answers `None` and the call is not an aggregate call. On a later screen of the same site, after the AST arm lifted it, `core_ho` answers.
- site/app/hl, site/app/bench and site/export test bodies, and the generator host `main`s: `@at` of a call result (`pieces()[0]`, `radarRings()[i]`, `args()[0]`). The core states a Builtin `@at` that no row names, and the element's name has no place.
Left: the globals initializer, blocked by `lower_globals_init` not walking the module-state body. sortWith, blocked by lifting an inline lambda target in the core walk. `@at` of a temporary, blocked by the ownership decision for an element read out of a temporary before the temporary is released.
