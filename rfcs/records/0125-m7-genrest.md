#### The generator host's `main` reads `argv` as a place (2026-09-25, `m7-genrest`)
RFC-0125, milestone M7.
Decision: count first. The gen-host bodies `compile_gen_host` sends to the AST arm fall under four clauses and the decoders; the table is `C:/wtcoretmp2/tally-gen.md`. This track took the largest refused clause of its set: the host `main` that `vyrn-genwasm::wrapper_program` synthesizes read `args()[i]`, an `@at` of a call, which the core does not state. It binds `args()` once and reads `argv[i]`, a desugar to a form the core states. The lead's split; the desugar is mine.
Went: the `Let/Call/Builtin/@at` clause for the host `main`, 50 refused walks and 242 arm occurrences.
Stayed, each with its owner: the decoders `__vyrnGenDec_*` (m7-atrhs); the `__vyrnGenReflect` hook in `__vyrnGenLex`, `__vyrnGenModuleInterface` and `__vyrnGenContractOf_*`, 56 walks and 168 occurrences, the hook row m7-stream builds; `Fn_::release`'s `Rel::Call`, which parks the receiver under `@rel` and calls a declared release through `Fn_::call`, 393 occurrences in bodies the core walk takes whole (`parse*`, `vyx*`, `rpc*` and the 21 others), and 30 in code the emitter makes itself.
Lines: `vyrn-genwasm` `lib.rs` 2,096 to 2,107. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, on `origin/m7-retire` at `9bcedc1b`:
- the census (`instr-census.patch`, gated on `VYRN_CENSUS` in the process) over `vyrn check` of the 15 roots whose generators reach the arm, a fresh `VYRN_GEN_CACHE_DIR` per root: refused walks 357 to 307, taken 12,769 to 12,819; arm occurrences 1,391 to 1,149; the host `main` 242 to 0. Predicted -50 walks and -242 occurrences; got both.
- `vyrn emit-gen` of the 15 roots, the base's binary against the head's, fresh caches: stdout, stderr and exit code byte-identical, all 15 exit 0.
- `check-corpus.sh`, the base's binary against the head's over this tree: 539 roots byte-identical, 460 accepted, 79 refused.
- `nextest -p vyrn-cli`: 680 passed. `vyrn-genwasm` tests: 3 passed. `kernel`, `effects`, `typed`, `coretables`, `wasmhash` (check) `--ignored`: green; kernel 27,061 accepted, 0 refused, 0 unlowered. `coredrive` (check): taken 21,161 of 21,172, 0 run apart; it emits `examples/` and never a generator host, so it cannot move here.
Time: 70 minutes: 30 census and tally, 10 work, 30 gates.
Findings:
- `VYRN_NO_GEN_CACHE=1` compiled no generator on a warm run; a fresh `VYRN_GEN_CACHE_DIR` per root is what forces every host through `compile_gen_host`. An earlier tally over a warm cache counted 674 occurrences where a cold one counts 1,391.
- the `@rel` path is the arm's call path reached from a body the core walk took: the form tally counts it against the taken body's owner. It moves no body and is the one emitter path the deletion must replace.
Left: `Rel::Call`, blocked by nothing but a slice of its own (a direct call of the declared release, with its argument's ABI); the hook bodies, blocked by m7-stream's `__vyrnGenReflect` row.
