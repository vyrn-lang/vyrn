# A byte class in std/regex is one ByteSet, and no byte-array table holds classes (2026-09-25, `std-regex-byteset`)
RFC-0125, no milestone: a std change that the check-elision research measured. The lead's brief placed the record here.
Decision: a class is a `ByteSet` record of four `Int64` words, and `Builder` and `Regex` hold one per instruction in `cls`, parallel to `op`, `a` and `b`; the user approved it. `ByteSet` stays private, as the old `Regex`'s internal fields were; the checker accepts it inside the exported `Regex`.
Went: the class table (`sets`, `nsets`) and its eight functions `emptySet`, `bitMask`, `setAdd`, `classAdd`, `setHas`, `newClass`, `literalClass`, `dotClass`; `Regex.first` is a `ByteSet` value, not a 32-byte array. Came: `ByteSet`, `noBytes`, `addByte`, `hasByte`. Stayed: nothing.
Lines: `std/regex.vyrn` 830 to 765 (+72 / -137); `docs/api/std/regex.md` 102 to 97.  Refusals: 0 lost / 0 gained.  Manifest: 1 row, `regexredux.vyrn`, below.
Licence:
- `VYRN_WASM_MANIFEST=check`: 1 of 174 rows differs, `regexredux.vyrn`, `3bdbf3d4` to `2b3a34fd`; re-pinned by `write`. `vyrn build --target wasm` against the old and the new std reproduces both hashes.
- `wasm2wat` of that row, built with `VYRN_WASM_NAMES=1`: 53 to 48 functions (the eight above gone, `addByte`, `hasByte`, `noBytes` added), 27,502 to 26,393 bytes. `emit` takes a fifth parameter and pushes a fourth array. `firstBytes` ORs four words in place of a 256-step loop. `bracketClass` and `parseAtom` build a `ByteSet` in place of a table row; the other parse functions pass `noBytes()`. `matchAt`, `countMatches` and `replaceAll` call `hasByte`. `compile` stores `first` inline. `Builder` lost a field and `Regex` holds `first` inline, so frame constants move by 8 or 16 bytes, and `addThread`, `compiled`, `freshMark`, `patch`, `peek`, `main`, `readAll` and `utf8Width` differ in frame offsets, local order and call indices alone.
- `residue --ignored`: engine 173 clean / 0 leaking, route 173 clean / 0 leaking, 0 failed.
- `kernel --ignored`: 27,066 to 27,061 instances accepted (5 fewer functions), 0 refused, 0 unlowered.
- `coredrive --ignored`: the core's walk took 20,906 of 21,177 bodies before and 20,901 of 21,172 after; 271 untaken both times.
- `scripts/check-corpus.sh`, old std against new, one binary: 491 roots, 415 accepted, 76 refused, `diff -r` empty.
- `vyrn doc --std --verify`: 41 files up to date with the regenerated page.
Behaviour:
- `vyrn test std/regex.vyrn`: 10 passed, old and new std, core walk and `VYRN_NO_CORE_WALK=1`.
- regexredux: output equals `rfcs/bench-0104/regexredux-1000.expected` under `vyrn run` and native `vyrn build`, each on both walks.
- differential: 81 patterns x 26 haystacks, old std against new, 2,481 lines equal on the same four routes. The set is the report's 54 x 20, regexredux's nine patterns, ranges and negations over bytes 128 to 255, and haystacks with 2- and 3-byte UTF-8.
Research measurements (2026-09-24, at `72ed434a`, by the check-elision analyzer; not gates):
- static checks proved: 1,659 of 2,358 to 1,658 of 2,351; 0 lost, and the one fewer is a deleted proved site.
- regexredux executed index checks: 3,052,324 to 2,706,353; proved 88.6% to 99.97%, with no analyzer rule added. Integer divide and remainder checks: 346,062 and 346,044 to 219 and 201.
- native time, 1M-base input, interleaved best of 7: 0.910 and 0.900 s to 0.849 and 0.873 s, noise band about 2.5%. Not re-measured on this main.
Time: 55 minutes: 15 work, 40 gates, 0 rebases, 0 waiting.
Findings: none.
Left: nothing.
