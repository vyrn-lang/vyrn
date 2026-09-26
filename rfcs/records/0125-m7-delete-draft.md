# The AST walk is deleted in one piece (2026-09-26, `m7-copy`)
RFC-0125, milestone M7.
Decision: the lead's, to draft the one-piece deletion before the tally read 0, then, at the user's word, to land it and fix every remaining site on top of it (`m7-delete`, #545).
Went: the arms of `Fn_::stmt` and `Fn_::expr_inner`, `Fn_::expr`, `expr_as`, `store_into`, `agg_into`, `elem_field_store`, `lambda_value`, about 90 helpers only they reached, `Parts::Ast`, `ArmRef` and `BodyRef`, `FORMS` and its tally, `VYRN_NO_CORE_WALK`, `Fn_::reflected`, coredrive's second walk, the `lowered` gate's core-off mode and its answer floors, the memory suite's second walk, the shapes' `// pin:` lines, and, once nothing read them, `Facts::copies`, `Facts::unreached`, `Body::unreached`, `NameInfo::copied` and `Fn_::walks`.
Stayed: `Fn_::block` and `Fn_::core_took`, the per-statement path; coredrive takes every body whole, but the path is what refuses a statement by name.
Lines: direct.rs 22,793 to 17,901, core.rs 10,588 to 10,556, against main `6232168e`. `git diff 6232168e...4fc646bf`: 174 files, 1,245 insertions, 6,527 deletions. The forms census's wasm column 192 to 44 on the draft.
Refusals: 0 lost, 0 gained. Manifest: 2 rows, langbench and rest, each one more release of a closure temporary (m7-lamval); no other row moved.
Licence, on the integrated branch (`cefecb26` to the tip):
- CLI suite 684/684 at `39ad1de9`.
- benching, universal_pages, derived: 17/17 at `cefecb26`.
- universal_pages leaves no server running (0 leaky).
- residue with a fresh gen cache: pass, 130 s.
- coredrive unsharded with the manifest check: 36,058 of 36,058 bodies taken, 154 s.
- 24 other ignored tests pass with the manifest check, and no row moved.
- corpus diff against main `6232168e`: 554 shared roots byte-identical; 6 new roots are new shapes, all accepted.
- route: 2/2 on `5fac9c8c`; the final tip's chunks are m7-spec2's.
- fmt and LSP fmt clean; build with 0 warnings.
The measure: a probe that stubs each refused body and logs it (`VYRN_DELETE_PROBE`, never committed) over the CLI suite, every ignored suite, `emit-wat` of every example root, `vyrn test` of the 92 test-block files and `vyrn check` of every root, each with a fresh generator cache. Refusals: 1,512 on `aa63e0e1`, 1,021 on `95d9a531`, 161 on `2ae7b2ab`, 207 at 155 sites on `d1ac9b23` with every refusal logged. The probe was not run after the ports landed; the gate table above is the evidence that no site is left.
Time: about 6 hours over the day: draft 110 minutes, probes and rebases 180, integration and prose 70.
Findings:
- the globals initializer had no core body: `std/json.vyrn`'s `prettyOut` alone reached it in 496 compiles, and every program with module state was refused (#538).
- `lower_dispatcher` emitted its arguments through the AST walk in 513 compiles; the tally filed them under "(the globals initializer)", because `top_level` has no owner (#535).
- `elem_field_store` emitted the `a[i].f = v` window from the AST before `core_took`, and no tally counted it.
- the tally counted only eight expression forms, so a call, a record or a `match` the arm emitted was invisible unless a literal or a variable sat under it.
- bench bodies reached the arm in every bench file, and no tally named them (`blackBox`, m7-small).
- `universal_pages` waited 600 s on a server that had died at once; m7-stream made its harness fail on the child's exit.
Left: nothing on this track.
