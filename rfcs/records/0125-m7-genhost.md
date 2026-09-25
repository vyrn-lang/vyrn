#### A generator's atom transfer is a host row, and the generated decoders are the core's (2026-09-25, `m7-retire`)
RFC-0125, milestone M7.
Decision: the lead's. The generated decoders are the largest block left on the AST arm: `__vyrnGenDec_Arr_Str` (Assign 66, While 66), `__vyrnGenDec_Opt_Str` (If 81), `__vyrnGenDec_Opt_Int` (If 63). Find the clause, predict, build.
Count, with the screen instrument over the `typed` suite on main `daabd078`. That suite, like `effects`, compiles the corpus's generators; the CLI's builds serve them from the cache. 22 generator bodies were refused, each by one clause family and nothing else. 17 decoders `__vyrnGenDec_*` met `Let/Call/Reserved/__vyrnGenNextInt` or `__vyrnGenNextStr`. 5 entries (`__vyrnGenLex`, `__vyrnGenModuleInterface`, `__vyrnGenContractOf_Api`, `_Component`, `_Page`) met `Do/Call/Reserved/__vyrnGenReflect`. Prediction: +22. Measured: +22, all taken.
Rule: RFC-0076 M3b's three transfer primitives are `Spec::Host` rows, as `raw`, `rawAt`, `render` and the code imports are. `Fn_::host` emits them for the arm and for the rows. `core_rhs_ty` answers a host row at the type the checker gave the site, which a `Do` asks.
Went: the arm's own `match` over the three names in `Fn_::gen_builtin`, and its unused handle to the host.
Lines: `direct.rs` 22,340 to 22,339; `core.rs` 10,044 to 10,049. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on main `daabd078`:
- coredrive does not cover these bodies. It counts program modules and drops a generator's own compile, so it reads 21,158 of 21,172 before and after; it passes, and its retired-arm assertion is green.
- `vyrn emit-gen` of the 25 examples that import through a generator, main's binary against the tip's, `VYRN_NO_GEN_CACHE=1` and a fresh `VYRN_GEN_CACHE_DIR` each. The tip's emitter compiled 14 modules cold, and all 25 outputs and exit codes are byte-equal.
- `genwasm --ignored` (determinism), the `vyrn-genwasm` crate tests and the `vyrn-lsp` suite (100 passed) pass.
- `vyrn check` over the corpus, main against the tip: 537 roots, `diff -r` empty. kernel 27,061 / 0 / 0. effects 9,939 pure before and after. residue: engine 173 clean / 0 leaking, route 173 / 0.
- pins: `emitter-census` the mapping 12,572 to 12,577 and one block per builtin name 4,588 to 4,582; `emitter-reads` the core's rows 3,184 to 3,178 and both 10,374 to 10,379.
Time: 70 minutes: 25 work, 35 gates, 10 finding where generator modules compile.
Findings:
- no gate counts a generator body's walk. coredrive drops them on purpose, and the form tally sees them only through `typed` and `effects`, which compile generators as a side effect.
Left: no gate of its own for generator bodies, the lead's decision. Once the wide tally reads 0, the AST walk is deleted in one piece (#525), and no arm is left to regress to. Until then the wide tally with the core walk on, run over the `typed` and `effects` suites, is the measure for these bodies.
