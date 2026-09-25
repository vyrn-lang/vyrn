#### A SmallArray literal is a row the emitter makes (2026-09-25, `m7-smallarray`)
RFC-0125, milestone M7.
Decision: the core's `Make(Array)` row at a `SmallArray<T, N>` type is built by `Fn_::sa_into`: the inline header of `sa_head`, then the parts written into the inline buffer by `fixed_elems`. The arm's `sa_from_fixed` writes its header through the same `sa_head`, so the inline state is stated once. The lead's brief (the census clause `name-no-place:SmallArray:Make`, 4 bodies).
Went: `name-no-place:SmallArray:Make` in branchtypes `main`, generics `main`, copy `smallArrays` and smallarray `main`. Stayed: copy `smallArrays` and smallarray `main`, because their `@push`, `@pop`, `@swapRemove` and `@toArray` rows on a SmallArray receiver and the element read `row = grid[1]` have no core reader (`name-no-place:SmallArray:Call`, `SmallArray:Read`).
Lines: `direct.rs` 45 added, 13 removed (the 12 header lines of `sa_from_fixed` moved into `sa_head`; `sa_into` and the make arm are new). Refusals: 0 lost / 0 gained. Manifest: 4 rows, below.
Licence:
- the census instrument on 6ad6dfc2 (#491), warm generator cache: 117 refused walks, 111 bodies after the two that went. The census before the slice, on c2f59d6f, was 119 walks and 113 bodies.
- `coredrive --ignored` with the manifest check: taken 21,053 to 21,055 of 21,172 (branchtypes and generics `main`); 0 run apart; the two new shapes at 0 break, 0 continue.
- the 4 moved rows, `VYRN_WASM_NAMES=1`, `wasm2wat` against 6ad6dfc2's binary. One function moves in each and no other function differs. branchtypes `main`: `memory.copy` 20 to 13, frame 656 to 304 bytes. generics `main`: 3 to 1, frame 432 to 336. copy `smallArrays`: 12 to 10, frame 224 to 160. smallarray `main`: 31 to 19, frame 1088 to 560. The last two take the literal's `let` per statement.
- the 4 programs under `VYRN_LEAK_CHECK=1` with 6ad6dfc2's binary and the tip's: the same stdout, stderr and exit code, no leak line.
- the shapes `a-smallarray-literal-the-core-makes` (28) and `a-smallarray-literal-of-owned-elements-the-core-makes` (22): the same value with main's binary and the tip's, with the core walk on and off, under `VYRN_LEAK_CHECK=1`. Under `VYRN_FORM_TALLY`, main's binary gives the literal's `let` to the arm and the tip gives nothing to the arm.
- `residue --ignored`: engine 173 clean, route 173 clean, 0 leaking. `kernel` 27,061 / 0 / 0.
- `cargo nextest run --release -p vyrn-cli`: 672 passed. `emitter-census`, `emitter-reads` and `surface` re-pinned.
Time: about 90 minutes: census port and runs 30, the row 15, `wasm2wat`, leak and tally runs 10, gates 35.
Findings:
- The instrument counts walks from every module a run compiles. With a cold generator cache, the generator builds add walks: one run counted 231 refused and 24,279 taken instead of 117 and 21,055. Run it twice after a rebuild and read the second log.
Left: SmallArray `@push`, `@pop`, `@swapRemove`, `@toArray` and element reads as core rows (2 bodies), blocked by a track that owns them.
