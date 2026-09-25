#### `value(x)` and `@list([..])` are rows the emitter reads (2026-09-25, `m7-small`)
RFC-0125, milestone M7.
Decision: the lead's. A `value(x)` row is a variant of `Value` built like any constructor row. `@list([..])` of a literal is built straight into its heap buffer, not in the frame followed by a copy. An earlier patch built it in the frame and copied it, which measured slower: tagged `main` frame 64 to 96 bytes, 2 more `memory.copy`, two new release functions.
Went: tagged, templates and show `main`.
- `core_ctor_name` answers the variant of a `value(x)` row from `value_scalar`, which `value_variant` shares.
- `core_lists` pairs each literal with the `@list` row that takes it, and `core_made_ty` makes the literal at `Array<T>`, so its parts land in the heap buffer (`PartIn::Buffer`). The `@list` name takes the literal's place, or, where it is a part of a record literal (templates' `Template`), the literal is made at the record's field.
- In the lowering, `@list` takes its literal by `consume`, because a read left the literal to release the elements the array holds.
- `value(p)` of a type with `impl Show` states the `show` call as a row, as `print` and `@str` do, and the box takes the String it returns.
Lines: `direct.rs` 22,340 to 22,446, `core.rs` 10,039 to 10,051. The added lines are new rows the emitter reads: the two readers, `core_lists`, `core_made_ty` and `value_scalar`. No arm retires with them. `core_lists` collects a body's lets on every call; bodies are small, and `coredrive`'s time did not move outside its noise.
Refusals: 0 lost / 0 gained. Manifest: 3 rows.
Licence:
- `coredrive` unsharded at the tip, on #517 (bf04fe21): taken 21,161 of 21,172, up from 21,158; 302 s. Every program runs the same.
- `wasm2wat`, each commit against its parent's binary. Only `$main` differs, with no new function.
  - tagged: 235 to 229 wat lines, frame 64 to 48 bytes, `memory.copy` 1 unchanged (the `userName` copy).
  - templates: 385 to 383 lines, frame 96 to 48, `memory.copy` 5 to 3.
  - show: 234 to 251 lines, frame 80 to 64, `memory.copy` 1 to 0.
- `VYRN_LEAK_CHECK=1` on tagged, templates and show with the core walk on and off: the same output and no free-audit line.
  - The #512 shapes are taken by the core and run clean.
  - A probe of `sql"\{p} and \{p.name} and \{p}"` and `template"x\{p}y\{k}"` over a `Show` record, in a loop, runs clean. Main double-frees on it.
  - A probe that renders `IntVal` by `k.toString()` in a `match` arm leaks 16 bytes per render, as on main (#518).
- `kernel` 27,061 / 0 / 0. `residue --ignored` after each commit: engine 173 clean, route 173 clean, 0 leaking.
- the frontend, lower and codegen unit suites: 1,124 passed. CLI nextest: 680 passed.
- `vyrn check` over 537 corpus roots with the parent's binary and the tip's: byte-identical, 458 accepted.
- `emitter-census`, `emitter-reads` and `surface` re-pinned in the commit that moved each.
Time: about 150 minutes: the rows 70, gates 80.
