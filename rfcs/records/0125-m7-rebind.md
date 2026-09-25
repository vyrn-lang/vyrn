#### A `let mut` of a borrow that the body rebinds is a copy (2026-09-25, `m7-rebind`)
RFC-0125, milestone M7.
Decision (the lead's, option A of three): a `let mut` bound to a borrow that the body rebinds names a value of its own, and the core states the `let` as `@copy`. The existing store and exit releases then hold on every path, the loop's back edge included. A rebind never writes the caller's heap, so there is no aliasing hazard to refuse; the only defect was the missing release. A states one fact in one place, with no runtime ownership word per binding (C) and no new placement code (B). The cost is flat on all 6 corpus sites, because each is an accumulator or a loop that copies on its first turn anyway. #503's refusal stays for a write THROUGH the alias, because that changes what the caller sees.
Went: the leak of every value a rebound borrow held (#501, witness 3). `Builder::copies` states the rule, `rebound` lists the names a body stores into whole, and `Facts::copies` carries the `let` nodes to the AST walk, which duplicates the bound value in place (`copy_stack` or `copy_at`).
Stayed: a type that declares `impl Copy`, which keeps the borrow and the leak because the AST walk states no call for it; no corpus body has one. A rebuild's write-back (`xs.push(v)`) is no rebind: census row 26 still refuses it on a borrow.
Lines: `core.rs` 9,981 to 10,091. `direct.rs` 22,320 to 22,346. `forms` all 879 to 892: `rebound` is a new walk over 13 statement forms. Refusals: 0 lost / 0 gained. Manifest: untouched.
Trace: on 4f8a57e6, `let mut t = s; t = mk(1); t = mk(2)` of a `read` String parameter leaked 3 blocks, and `let mut c = r; c = R { name: mk(3), .. }` 1, on both walks. The core stated each store `(Nothing)`, because the name was a borrow, and no exit released the last value. A String accumulator leaked only its final buffer: its ownership word makes the first append copy.
Licence:
- `VYRN_LEAK_CHECK=1`, walk on and off, identical output, exit 0, no audit line: witness 3 (`abx 3 ab`), the two stores above (`2 2 ab`), a rebind once, in a loop, of a field read, and on both arms of an `if`. The shape `a-borrow-rebound-in-a-let-mut-is-a-copy` prints 253232 on both walks; 4f8a57e6's binary leaks 6 blocks on it. Witnesses 1 and 2 are refused since #503, and with `.copy()` they run clean on both walks.
- `coredrive --ignored`, alone: unsharded at bcf3eb01, taken 21,141 of 21,172; on 4f8a57e6 and d62c4f6e the count was the same with the rule switched off (21,094, 21,139); 1 byte-identical, 167 run the same, 0 run apart.
- `VYRN_WASM_MANIFEST=check` green, so no row moved: the 6 sites are std/ui's `pages*` and std/vyx's `vyxStripDeadHelpers`, which run in generators, and a generated instance's `main`.
- `kernel --ignored`: 27,061 / 0 / 0. `residue --ignored`: engine 173 clean / 0 leaking, route 173 clean / 0 leaking.
- `vyrn check` over 536 roots at bcf3eb01, the tip with the rule switched off against the tip: equal.
- pins by `VYRN_PIN=write`: `emitter-census` the mapping 12,573 to 12,599 lines and 847 to 850 rows; `emitter-reads` the core's rows 3,184 to 3,192 lines, both 10,353 to 10,371 lines; `forms` as above.
Time: 190 minutes: 35 census and decision, 40 work, 90 gates, 25 two rebases.
Findings:
- A first cut counted a rebuild's write-back as a rebind, turned census row 26's refusal into an implicit copy, and lost it. `rebound` skips the write-back.
- A body that both rebinds a field-read borrow and writes through it (`let mut f = x.d; f[0] = 1; f = g`) now writes the copy and not `x.d`. No corpus body does both.
Left: `impl Copy` types, blocked by a copy call the AST walk can state; no program needs it.
