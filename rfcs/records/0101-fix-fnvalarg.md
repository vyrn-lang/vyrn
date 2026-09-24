#### An address key is sound only while its node lives (2026-09-24, `fix-fnvalarg`, #444)
Decision: keep RFC-0101's identity rule, a node is its address, and state its condition: an address key is sound only while its node lives. A node the backend makes or copies and then walks is kept alive by the `Cx` for the whole compile. The lead's rule; the list is mine.
The defect: `vyrn run` emitted a module that wasmtime refused (`function[34]`, `call 33`, "expected i32, found i64") and a route executable that double-freed, on Linux runners only and not on every run. The module the route leg of the same job wrote validated and was byte-identical to a local build. The cause was not reproduced: 400 local builds, a no-reuse allocator, a LIFO allocator, a random-reuse allocator that scribbled freed blocks, and 180 runs of a musl Linux build all gave one hash.
Went: `Module::fill`'s arity `debug_assert`, replaced by a check of the whole signature in every build that names the function and both signatures. Stayed: every address key but the two below, because each names a node that lives for the compile.
The 65 address sites in lowering and codegen, by the lifetime of the node each names:

| class | sites | where |
|---|---|---|
| a node of the program, or of an expansion `project` leaks | 59 | `direct.rs` 32 (`Fn_::stmt` 17, `expr`, `expr_inner`, `peek`, `block`, `lower_body`'s parameter, `core_run`, `annotations`, `annotates_a_check`, `Cx::lambda`); `core.rs` 20; `lib.rs` 7 |
| guarded: restored when its guard drops | 1 | `core.rs` `key_of`, the program's address under `decide` |
| temporary: a literal `Cx::lambdas` misses | 1 | `direct.rs` `lift_lambda`'s `Key::Lambda`, now the address of a copy the `Cx` keeps |
| not an address: the name `core_addr_of` matched the search | 4 | `direct.rs` `core_stmts`, `core_call` twice, `core_addr_of` |

Two temporaries outside the 65: `if let`'s synthesized binder `let`, which `Fn_::stmt` keys a release and an accumulator on, is kept the same way; `schemaOf`'s reflected literal only reads tables whose keys are live nodes, so a miss is its answer, and it stays as it is.
Lines: `wasm.rs` +45 / -30 (+15, of which 11 are the unit test), `direct.rs` +50 / -26 (+24). `reproducible.rs` +40. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence: the numbers are in the gate table of the branch's report.
Findings:
- a release build wrote any body into any reservation of the same index; only a debug build compared even the parameter count.
- the witness, `reproducible`'s `a_program_compiled_after_another_in_one_process_is_the_same_bytes`, passed before the fix as well. It guards the rule and does not reproduce #444.
Left: the Linux-only defect itself, blocked by a copy of the module the engine refused, which the dump in the next commit keeps.
