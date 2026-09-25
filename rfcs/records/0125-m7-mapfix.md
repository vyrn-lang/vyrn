#### A packed map key is released, copied and stored as what it is (2026-09-25, `m7-mapfix`)
RFC-0125, milestone M7.
Decision: the lead's, for #508 and #509, one commit each. Both defects came up in `m7-mapkey2`.
#508: the deep release and the deep copy of a `Map` whose values own heap treated every key that is not `Int64` as a String. They walked the key column at stride 4. A record or fieldless-enum key is packed bytes, so `Map<Point, String>` trapped at its release ("memory fault at wasm address 0xfffffff1"), and a copy duplicated 4 of each 16 key bytes. Both arms ask `map_key` now. They walk keys only for `MapKey::Str`, at `MapKey::stride()`.
#509: the index store compared the key with the declared key type as written. The lookup, a literal entry, `has` and `remove` compared both at their base. `Checker::key_fits` states the comparison once, and all five sites ask it.
Went: the `ik` flag in both map arms; four spellings of the key comparison. Stayed: each site's own refusal sentence.
Lines: `direct.rs` 22,318 to 22,320. `checker.rs` 14,709 to 14,731, of which 18 are the unit test. `memory.rs` 2,508 to 2,515. Refusals: 0 lost / 0 gained in the corpus; the named-enum index store is accepted now. Manifest: untouched.
Licence, at the tip on main `d62c4f6e`:
- #508, from `wasm2wat` of the witness, built by this tree without and with the change: four functions change, the copy and the release of `Map<Point, String>` and of `Map<Suit, String>`. Each release loses its per-key String release loop (`call 28`). Each copy loses its per-key String duplicate loop (`call 33`), and its key byte count goes from `n * 4` to `n * 16` (`Point`) and `n * 8` (`Suit`). No corpus program has a packed key with a heap value, so no manifest row moved.
- #508, the shape `a-map-of-a-packed-key-and-a-string-value` in `a_map_of_a_packed_key_and_a_string_value_is_freed_on_both_walks`: under `VYRN_LEAK_CHECK=1` it prints 22321 on both walks, exit 0, empty stderr. It stores over a key, copies, removes, and builds an enum-keyed literal and copies it. Main's binary traps on it.
- #509, `an_index_store_takes_the_key_type_a_lookup_takes`: `s[h] = 1` with `h: Suit` and `s[Clubs] = 2` are accepted. `s[3] = 1` is refused with "`s` is keyed by Suit, but the key here is Int64". Main's binary refuses all three stores of the witness and names the Int64 one with that same sentence.
- `vyrn check` over the corpus, with this tree's binary without and with both changes: 534 roots, 455 accepted, 79 refused, `diff -r` empty.
- `kernel`, `effects`, `typed`, `coretables`, `wasmhash` `--ignored` with the manifest check: 6 passed. `kernel` 27,061 / 0 / 0.
- `coredrive --ignored` with the manifest check, on a local merge of this branch with `origin/m7-shard` `6db4e9f5`, which carries #513's forms pin: passed. Taken 21,141 of 21,172, 1,676 carried end to end, 1 byte-identical, 167 run the same, 0 run apart. The new shape is at 0 break, 0 continue. On main `d62c4f6e` alone it fails at `coredrive.rs:482` (the carried forms lack `Stmt::IfLet`) with and without this track's code. The CLI suite (680/680) and the other ignored suites also passed on the merge.
- `residue --ignored`: engine 173 clean / 0 leaking, route 173 / 0, 0 failed.
- pins by `VYRN_PIN=write`: #508 moves `emitter-census` (the mapping 12,572 to 12,573, a decision 1,148 to 1,149), `emitter-reads` (neither 6,709 to 6,711) and `surface` (`Type::Int` 130 to 128). #509 moves `checker-census` (the typing judgment 3,223 to 3,230, tests 4,424 to 4,439).
Time: about 130 minutes: 30 work, 75 gates, 25 on a rebase that dropped the pins and on main's `coredrive` failure.
Findings:
- Main `d62c4f6e` fails `coredrive`: no `if let` in the corpus is carried by the core's rows any more. #513 moves the forms pin.
Left: nothing on this track.
