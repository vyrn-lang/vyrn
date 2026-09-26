#### A declared release shares the generic solve and the place read (2026-09-26, `m7-small`)
RFC-0125, milestone M7.
Decision: the lead's. Port m7-where's fixes to #533 (`8e59486c` on `m7-relcall`) as one commit on main.
Went: `Fn_::release_call`'s own copy of the generic solve, now `Fn_::generic_sig`, shared with `Fn_::call_inner`; its own copy of the place read, now `Fn_::push_place`, shared with the `Expr::Var` arm.
Fixed: a release inside a monomorphized instance solves with `self.cx.sub(ty)`, so the instance's parameters are concrete. The value coerces to the release's parameter type. A release whose signature is not exactly one non-modify parameter is refused as unsupported; `Fn_::call` checked both before #533.
Stayed: the expected result is not passed to the solve (`None`), as #533 had it. m7-where passed `self.expect.last()`, which at a release is the enclosing expression's expectation and unrelated to the release's result.
Lines: `direct.rs` 22,632 to 22,631. The two helpers remove three copies; the coerce and the signature check add back what #533 dropped. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, on main `aa63e0e1`, main's binary against the head's:
- `vyrn emit-gen` of the 115 example and site roots that import a generator, a fresh `VYRN_GEN_CACHE_DIR` per root: stdout, stderr and exit code byte-identical; 30 roots print modules, 114 exit 0, `gentablefail` exits 1 as its fixture says.
- `vyrn check` over the corpus: 539 roots byte-identical, 460 accepted, 79 refused.
- `VYRN_WASM_MANIFEST=check`: `wasmhash` green; `coredrive` green in both shards.
- CLI suite: 681 passed. kernel 27,061 / 0 / 0; effects 9,939 pure; typed and coretables green.
- pins: `emitter-census` the mapping 12,844 to 12,829, one block per builtin name 4,607 to 4,621; `emitter-reads` neither 7,057 to 7,061, both 10,486 to 10,481.
Time: 45 minutes: 10 work, 35 gates.
Findings:
- the lead's 15-root list was not in `tally-gen.md`; the licence used the 115 roots whose `emit-gen` finds a generator import, a superset.
- `vyrn` outside its tree needs `VYRN_STD`; without it `check` refuses 380 of 539 roots and `emit-gen` refuses every generator root, identically on both binaries, so an unset `VYRN_STD` gives a false green.
Left: nothing of this slice.
