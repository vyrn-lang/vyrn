#### `schemaOf<T>()` lowers through the literal the checker typed (2026-09-25, `m7-schema`)
RFC-0125, milestone M7.
Decision: the memo the gap tail named for `schemaOf` (2026-09-23) is the compile memo `project::Memo`, the one a projection site's expansion lives in. The checker expands `schemaOf<T>()` once per call node while the memo is open (`project::schema`) and types the `Schema` literal it stands for; the row walk and the builder read it by the node (`project::schema_at`), so the core states the record the literal builds and the emitter reads it from the rows. The lead's aim; the memo is mine, on the projection precedent.
Went: the `schemaOf` gap in the builder (`Rhs::Call` of `schemaOf`, `Builtin`); reflection `main`'s record name with no place.
Stayed: the arm's own expansion in `Fn_::reflected`, which it builds from `types::schema_struct_lit` as the memo does, for a compile with no memo open (the LSP); it goes with the arm. A `schemaOf<T>()` inside a generic body, whose target is a parameter: the arm refuses it and the checker expands nothing.
Lines: `checker.rs` 14,731 to 14,749. `project.rs` 1,682 to 1,715. `core.rs` 9,981 to 9,986. `vyrn-lower` `lib.rs` 1,522 to 1,529. Refusals: 0 lost / 0 gained. Manifest: 1 row, `reflection.vyrn`.
Licence, on main at `bcf3eb01`:
- predicted +1, reflection `main` (the tail census: its only clauses were the `schemaOf` name and statement).
- `coredrive --ignored` with `VYRN_WASM_MANIFEST=check`, both on this machine: taken 21,141 to 21,142 of 21,172; whole 21,063 to 21,064; carried 1,676, unchanged; 1 byte-identical, 167 run the same, 0 run apart.
- the moved row, in `wasm2wat`: `main` moves to the core walk and builds the `Schema` literal from the rows, 9,622 to 9,605 bytes, 29 functions both; the other functions are unchanged. Interpreter, wasm and main's wasm print the same bytes; `VYRN_LEAK_CHECK=1`: exit 0, no leak line.
- `check-corpus.sh`, the base's binary against the head's over this tree: 535 roots byte-identical, 456 accepted, 79 refused. `site/guide/schema.vyrn` and `site/app/guide.vyrn` call `schemaOf` and print the same.
- `nextest -p vyrn-cli`: 680 passed. `kernel`, `effects`, `typed`, `coretables`, `wasmhash` (check) `--ignored`: green; kernel 27,061 accepted, 0 refused, 0 unlowered; typed 237,084 judged.
Time: 90 minutes: 25 work, 55 gates (three `coredrive` runs of 290 to 550 s), 10 two re-bases after `origin/main` moved under the worktree twice.
Findings:
- the first try typed the literal and still gapped: the builder reads a node's type from the row walk (`vyrn-lower` `Walk`), not from the checker's record, so an expansion needs a row-walk arm as a projection site has one.
- `origin/main` is shared by every worktree, so another track's fetch moves it mid-gate. My first base run measured a later main than `d62c4f6e` and failed on `Stmt::IfLet` in the `carrying` pin; on `bcf3eb01` it passes. Pin a base by hash, not by `origin/main`.
Left: nothing for `schemaOf`.
Census: `checker-census` the typing judgment 3,230 to 3,248; `frontend-census` `project.rs` own job 1,074 to 1,076, shared machinery 298 to 329; `forms` `Expr::Call` 88 to 89; `surface` `Type::Named` 74 to 75, `Type::App` 30 to 31.
