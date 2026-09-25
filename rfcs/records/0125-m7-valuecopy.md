#### `value(s)` of a String boxes a copy the box owns (2026-09-25, `m7-small`)
RFC-0125, milestone M7. Fixes #512.
Decision: the lead's. `value(x)` of a String read out of a place boxes a copy, because a `Value` owns its payload. Making `value` consume its operand would refuse `print(s)` after a tagged template, which is a language change, so that option was rejected.
Went: `value` as a lending call, from `prelude::lends`, the core's `lends_name` and the arm's `lends`. The box held the caller's String buffer, and the release of whatever held the box freed it under its owner. Stayed: the arm's own copy, because the arm reads the AST and not the rows. `prelude::boxes_a_copy` states which operand is copied, and both walks ask it. `is_place_read` moved from `core.rs` to `vyrn_frontend::project` so that the predicate has one home.
Lines: `direct.rs` 22,111 to 22,131, `core.rs` 9,820 to 9,833, `prelude.rs` -2, `project.rs` +12. The fix adds lines, because the copy is a row of its own in the core, and the arm has to state the copy too.
Refusals: 0 lost / 0 gained. Manifest: 2 rows, below.
Licence:
- the repro from #512 and `sql"x \{s} y \{s}"` then `print(s)`, as the shapes `a-string-boxed-by-value-while-its-owner-lives` (abcd, 14) and `a-string-interpolated-by-a-tagged-template-while-its-owner-lives` (abcd, 324). On main, both walks exit 134 with "double or foreign free". On the tip, both walks print the values and give no free-audit line under `VYRN_LEAK_CHECK=1`.
- a wider probe of holes on a record field, an array element, a temporary, an Int64 and a Bool, in a loop, plus `let v = value(p.name)` matched: no double free on the tip. Main gives a double free. The probe also leaks 16 bytes per `IntVal` rendered by `k.toString()` inside a `match` arm operand of `+`. Main leaks the same, so this is a separate defect.
- `kernel` 27,061 / 0 / 0. `effects` 29,619 functions judged. `residue --ignored`: engine 173 clean, route 173 clean, 0 leaking.
- `coredrive` as two shards, on the parent and on the tip: 10,043 + 11,061 = 21,104 of 21,172 both times.
- `vyrn check` over the 531 corpus roots with the parent's binary and the tip's: byte-identical, 452 accepted.
- the 2 moved rows, `wasm2wat` against the parent's binary: only `$main` differs. In tagged, `userName` is copied (`strNew` and one `memory.copy`) before it is boxed. templates gets the same copy for its String hole. `show` is unchanged: a type that renders by `show` boxes its render with no copy. `VYRN_LEAK_CHECK=1` on tagged, templates and show gives the same output with both binaries.
- `forms`, `frontend-census`, `emitter-census` and `emitter-reads` re-pinned. The frontend, lower and codegen unit suites pass (1,122).
Time: about 110 minutes: tracing 30, the fix and probes 30, gates 50.
Findings:
- The dispatch census in `primitives.rs` reads `name == "StrVal"` as a builtin dispatch. The arm names the variant `variant` for that reason.
- `core.rs` on main carried a stray doc line, "A field read or an element read", above `over_a_literal`. It went with `is_place_read`.
