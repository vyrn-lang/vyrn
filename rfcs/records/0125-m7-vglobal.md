# A crossing into a validated global is its constructor (2026-09-26, `m7-spec2`)
RFC-0125, milestone M7.
Decision: the lead's (CI on #545, `semantics::validated_global_traps_at_runtime_on_bad_store`). The shape, mine: the rule #543 stated for an element, stated for module state at a store and at its initializer.
Went: `a = n` into a global of a validated type, which the core stored raw and the emitter refused ("no lowering for a statement of `setAge`"), and the emitter's blanket refusal of every module-state body with a validated global ("a module-state initializer the core did not state"). `Stmt::Assign` asks `Builder::checked` against module state's declared type as against a binding's, and `build_module_state` states each initializer's crossing into its declared type as the constructor.
Stayed: a crossing `checked` leaves to the reader (an owned value read from a place, or lent); the store screen refuses it.
Lines: `core.rs` 10556 to 10564, `direct.rs` 17901 to 17896. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, base `origin/m7-delete` `0cb6cfa4` against the tip:
- `vyrn-frontend` `semantics`: base 187 / 6 failed, tip 188 / 5; `validated_global_traps_at_runtime_on_bad_store` passes ("validation failed for `Age`"). The other five fail alike on both (`lmap` twice, `schema_of_*` three times).
- CLI suite 684/684 with `VYRN_PIN=write`: `emitter-census` and `emitter-reads` moved by 5 lines.
- `kernel`, `typed`, `effects`, `wasmhash` green with `VYRN_WASM_MANIFEST=check`.
Time: about 35 minutes: 5 reading, 10 the two changes, 20 gates.
Findings:
- The module-state append arm of `Stmt::Assign` matched whatever `check` said, so a growing validated String global would have skipped its constructor; it now requires no check, as a binding's arm does.
Left: nothing.
