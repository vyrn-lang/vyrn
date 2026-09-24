#### A render of a type the language does not render is a call to its impl (2026-09-24, `m7-render`)
RFC-0125, milestone M7.
Decision: the core states `print` and `@str` of one argument whose base is outside `types::renders` as a `Callee::Fn` to the impl `types::show_dispatch` names, the way `loader::routed_callee` states `toJson`; `show_dispatch` is the one home of that choice, and the checker, the core and the emitter's arm each ask it. The decision paragraph of PR #447; the lead stopped the track after this step, because m7-names' one rule covers steps 2 to 4 (record names, enum names, the `where` alias).
Went: the checker's copy of the dispatch (13 lines) and the arm's (`renders` then `show_impl`, 3 lines); `types::show_impl` became `show_dispatch`, which takes the written and the resolved type. Stayed: a record or enum name, which the screen still refuses, so the new row reaches no emitter yet.
Lines: `checker.rs` 14,975 to 14,962, `direct.rs` 21,272 to 21,269, `types.rs` 2,671 to 2,678, `core.rs` 8,781 to 8,815; one shape file, 6 lines. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence:
- `VYRN_WASM_MANIFEST=check` green: the row is built, and no body that holds one is taken by the core walk until a record name is admitted.
- `coredrive --ignored`: 168 programs, 21,146 bodies, taken 20,418 of 21,176 plus 74 judged, 1,647 carried end to end, 1 byte-identical, 167 run the same, 0 run apart; 61 shapes at 0 break, 0 continue. The counts equal the ones `0125-m7-pins.md` records for main (not rerun on main here).
- `vyrn check` over the corpus, main's binary (`a7a45dd7`) against the tip's: the 475 shared roots equal; 1 root gained, the shape, accepted (399 to 400 accepted, 76 refused).
- `examples/show.vyrn` under `VYRN_LEAK_CHECK=1` with both binaries: the same 9 lines, exit 0.
- `kernel`: 175 programs, 27,066 accepted, 0 refused, 0 unlowered.
- pins re-pinned with `VYRN_PIN=write`: `checker-census` the typing judgment 3,388 to 3,375; `emitter-census` one block per builtin name 4,424 to 4,421; `emitter-reads` both, for two questions 9,864 to 9,861; `declarations` impls 14 to 13 mentions, 243 to 242.
Findings:
- the step-2 work left in the tree at the power cut (a record name admitted through a `core_name` helper, a record-name shape, 37 manifest rows) was discarded, not committed; it is m7-names' rule to state.
Left: record names, enum names and the `where` alias, to m7-names2's one rule, which this row unblocks for a render.
