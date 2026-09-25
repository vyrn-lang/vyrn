#### A declared release is a call to its signature (2026-09-25, `m7-relcall`)
RFC-0125, milestone M7.
Decision: the lead's (class 3 of the wide tally: the one expression shape the core hands to the arm inside otherwise-taken std functions). The shape, found here: it is no expression the program wrote. `Fn_::emit_rel`'s `Rel::Call` parked a value whose type declares `impl Owned` under the reserved name `@rel`, then called the release through `Fn_::call` with a synthetic `Var("@rel")`. `Fn_::release_call` calls the signature, or the instance where the impl is generic.
Went: the `@rel` scope entry and the synthetic call. Stayed: nothing of this shape.
Lines: `direct.rs` 22,482 to 22,522, against `a57c8cf7`. More than it deletes: the generic instance lookup and the three places a value can cross from, which `Fn_::call` did for the synthetic argument. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on `a57c8cf7`:
- cause, from an `ARM` debug print at `Fn_::expr`'s count (not committed) over `examples/graphql.vyrn`: every arm `Expr::Var` in the class-3 owners was `@rel`. The releases were `Owned__json$Json__release` 70 times and `Owned__GqlSel__release` 2 times.
- `wasmhash` with `VYRN_WASM_MANIFEST=check`: pass, so every example's module is byte-identical.
- `VYRN_FORM_TALLY` over the CLI suite, unsharded coredrive and the other ignored suites, core walk on: parse*, mapJson, rpcApplyConfig, rpcConfig, walkVon, policyOf, twParseTheme, vyx*, http*, gql* and json$w*/json$d* leave the arm rows. 102 owners remain, and `main` is in 101 compiles.
- unsharded `coredrive` with the manifest check: 21,163 of 21,172, pass. The screen is untouched.
- `kernel`, `effects`, `typed`, `coretables`, `residue`: pass; kernel 27,061 / 0 / 0; residue 173 clean on both engines. CLI suite 680/680.
- pins by `VYRN_PIN=write`: `emitter-census`, `emitter-reads` (`emit_releases` moves from "the source, and the core has no row" to "neither"), `forms` (`Expr::Var` 123 to 122).
Time: about 75 minutes: 25 cause, 15 build, 35 gates and tally.
Findings:
- An arm count can be the emitter building AST for itself. The tally cannot tell that from a program's own expression, so an owner that reads only `Expr::Var` is worth a debug print before a track is given it.
Left: the globals initializer, the `main` bodies and the std test bodies, blocked by no track yet in this record.
