#### A map removal modifies its receiver (2026-09-25, `m7-mapleak`)
RFC-0125, milestone M7.
Decision: the lead's. `@remove` gets a prelude row with a `modify` receiver, as `@pop` and `@swapRemove` have. The checker's map arm still types the call.
Went: nothing. Stayed: the checker's hand-written `@remove` arm, because deleting it moves its refusal sentences.
Cause: `@remove` had no row, so `core::call` read its receiver at `read`. `removal_at` states a move-out window only through a `modify` receiver. So on main `31a3e69a`, `h.m.remove(k)` on a record field stopped with "internal error: the core cannot state a move-out window whose removal does not modify the temp alone, so `main` is not judged", under both walks. The leak `m7-mapcall` reported on `c2f59d6f` came before the reach decision (#487, #489) made a gap an internal error. At that point the placer left the unbuilt body as the plan had it.
Lines: `prelude.rs` 1,224 to 1,244. `direct.rs` 21,895 to 21,895 (`map_set`'s doc moved back above `map_set`). `memory.rs` 2,455 to 2,483. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence, at the tip on main `31a3e69a`:
- `a_removal_from_a_map_field_releases_the_entry`: under `VYRN_LEAK_CHECK=1`, both walks print `true` and `1`, exit 0, and print no audit line. Main's binary exits 1 with the internal error.
- `coredrive`, `kernel`, `effects`, `typed`, `coretables`, `wasmhash` `--ignored` with `VYRN_WASM_MANIFEST=check`: 7 passed. The emitter took the core's walk for 21,053 of 21,172 bodies. Main was not rerun; the manifest check holds every module to main's hash. 1,656 bodies carried end to end. Of 168 programs, 1 emits the same module either way and 167 run the same. `kernel` 27,061 accepted / 0 refused / 0 unlowered.
- `residue --ignored`: engine 173 clean / 0 leaking, route 173 / 0, 0 failed.
- `vyrn check` over the corpus with main's binary and the tip's: 507 roots, 430 accepted, 77 refused, `diff -r` empty.
- `VYRN_LEAK_CHECK=1 vyrn run` of `intkeys`, `mapdemo`, `mapkey` and `slots` (a `Slots` remove, which traps by design), on both walks: main's binary and the tip's print the same bytes and exit codes, with no audit line.
- pin by `VYRN_PIN=write`: `surface` `Type::Param` 42 to 45, `Type::Map` 64 to 65, all 1,439 to 1,443.
Time: about 45 minutes: 15 work, 30 gates.
Findings:
- No corpus program removes from a map in a field, so the fix moves no body and no manifest row.
Left: `fieldmut`'s map literal with layout values, which `core_made` excludes. It has no track yet.
