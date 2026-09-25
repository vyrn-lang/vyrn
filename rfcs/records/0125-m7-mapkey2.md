#### A map keyed by a record or an enum is read from the rows (2026-09-25, `m7-mapkey2`)
RFC-0125, milestone M7.
Decision: the lead's slice, reported by `m7-copy`. A map's key is read the way a call reads a layout argument: `core_val` pushes its address, and `pack_key` packs it. `core_layout_name` states that test once, and three readers use it: `core_args_readable`, a `Store` into a `Key` place, and a `Read` of one.
Census, from the instrument (`instr-census.patch`, rebased onto `ab947e5f`, run twice) over `mapkey`: 112 bodies, 1 refused, `main`. Its clauses: name-no-place `@borrow = Read(Key(..))` twice (`m[Point{..}]`, `suits[Hearts]`), `Store/Key` three times, `Let/Read` and `Switch` twice each. The five `@tally` rows no longer held it. Every clause was one cause: a `Key` place took only a scalar key (`core_val_readable`).
Prediction, sent before the change: all 9 clauses clear, `main` taken (+1), 1 manifest row, 0 run apart. Result: as predicted; the instrument showed 112 of 112 taken.
Went: the scalar-only key screen. Stayed: nothing.
Lines: `direct.rs` 22,045 to 22,056. `memory.rs` 2,506 to 2,508: the shape's leak test, and a separate commit that writes two older expected outputs as one escaped line. Refusals: 0 lost / 0 gained. Manifest: 1 row, `mapkey`.
Licence, at the tip on main `4f8a57e6`:
- `coredrive --ignored` with the manifest check: taken 21,095 of 21,172 at the tip. Main was not rerun here. On `ab947e5f` the same commit took 21,082 to 21,083. 1,666 carried end to end. 1 program byte-identical, 167 run the same, 0 run apart. The new shape `a-map-keyed-by-a-record-or-an-enum-the-rows-carry` is at 0 break, 0 continue.
- The moved row, from `wasm2wat`: `mapkey` `main` takes the core's walk. Its frame went from 192 to 128 bytes. The core builds each `Point` key literal in one slot at offset 32 where the arm took a slot per literal, and adds the two `drop`s its made layouts have. A store over an `Int64` value computes no address for the old value, because the row releases nothing. The `memory.copy` and `memory.fill` counts are unchanged (51, 20).
- `kernel`, `effects`, `typed`, `coretables`, `wasmhash` `--ignored`: 6 passed. `kernel` 27,061 / 0 / 0.
- `residue --ignored`: engine 173 clean / 0 leaking, route 173 / 0, 0 failed.
- `vyrn check` over the corpus, N:/wt-core's binary at `4f8a57e6` against the tip's: 524 shared roots equal; the new shape gained, accepted.
- `a_map_keyed_by_a_record_or_an_enum_is_freed_on_both_walks`: the shape under `VYRN_LEAK_CHECK=1` prints 622327 on both walks, exit 0, empty stderr.
- `VYRN_LEAK_CHECK=1 vyrn run mapkey.vyrn`, main's binary and the tip's, both walks: the same bytes, exit 0, no audit line.
- pins by `VYRN_PIN=write`: `emitter-census` the mapping 12,325 to 12,336; `emitter-reads` both, for two questions 10,087 to 10,098.
Time: about 130 minutes: 30 work, 75 gates, 25 on the findings below and two rebases.
Findings:
- A `Map<Point, String>` traps on main on both walks. The map's release and its deep copy walk the key column as Strings whenever the key is not `Int64`, so a packed key is freed as a pointer. The shape keeps `Int64` values for that reason.
- The checker refuses `s[h] = v` on a `Map<Suit, Int64>` ("`s` is keyed by Suit, but the key here is enum { Clubs | Hearts }"), even with `let h: Suit`. `tally` and the lookup accept the key. The shape uses `tally`.
Left: the two findings above, which have no track.
