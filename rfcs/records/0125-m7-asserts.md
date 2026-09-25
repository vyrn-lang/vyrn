#### `assert` and `assertEq` are rows the emitter reads (2026-09-25, `m7-asserts`)
RFC-0125, milestone M7.
Decision: the lead's. The AST walk goes in one piece, and the measure widens to every body any gate compiles with the core walk on, so take the bodies that still reach the `if let` arm. The shape, mine: one row kind, `Spec::Asserts`, for both builtins. `Fn_::asserts` takes an operand closure, and the arm and the row share it.
Went: the arm's own `assert`/`assertEq` emission. Stayed: nothing of this clause.
Lines: `direct.rs` 22,376 to 22,429, `core.rs` 10,154 to 10,160, against `fa7e9081`. More than it deletes: the two operand closures and the shared function's signature. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on `fa7e9081`:
- the two bodies behind the IfLet stub. memory `process(n: consume Node)` is taken with the core on, with an empty arm tally; it reached the stub only in `payload_run`'s second leg, `VYRN_NO_CORE_WALK=1`. The std/regex.vyrn:720 test body was refused on `stmt:Do/Call/Builtin/assertEq`, so its `if let` went to the arm.
- screen instrument over `vyrn test` of the 42 std modules: 384 refused bodies to 42, predicted 43. The one I counted as staying (`assertEq` as a `let` value) is taken too. The 42 left: `__vyrnGenNextInt` 29, `__vyrnGenNextStr` 21, `__vyrnGenReflect` 9 (ui, vyx, vyx-hints, von), and one `sortWith`/closure body (arrays). Every module's tests pass.
- unsharded `coredrive` with the manifest check: 21,160 of 21,172, pass. Test blocks are not in its corpus.
- `kernel`, `effects`, `typed`, `coretables`, `wasmhash` (check), `residue`: pass. kernel 27,061 / 0 / 0; residue 173 clean on both engines.
- CLI suite 680/680. Pins by `VYRN_PIN=write`: `emitter-census`, `emitter-reads`, `surface` (`Type::Int` 128 to 129).
- `VYRN_FORM_TALLY` over the CLI suite, coredrive and the other ignored suites, core walk on: no `Stmt::IfLet` occurrence. The next bodies are below.
Time: about 90 minutes: 30 cause, 15 build, 45 gates and tally.
Findings:
- The wide tally with the core on reads 14,019 arm occurrences in 249 owners. The largest classes: the globals initializer (501 compiles; `Expr::Var`, `Expr::Str`, `Expr::Int`), `main` bodies (232 compiles), one or two `Expr::Var` inside otherwise-taken std functions (`parseArray`, `parseObject`, `parseValue`, `mapJson`, `http*`, `gql*`, `json$w*`), and the generated decoders `__vyrnGenDec_*` (`Stmt::Return`, `Stmt::Let`).
Left: the globals initializer, the one-`Expr::Var` class and the generated decoders, blocked by no track yet.
