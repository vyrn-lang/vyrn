# std/storage.vyrn

Lines: 94. Exports: 1. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller saves a whole file without a corruption window: `writeAtomic(path, content)` streams the content to a sibling temp `<path>.tmp.<seed>` and renames the temp over `path`, so a crash mid-save leaves either the complete old file or the complete new one (`std/storage.vyrn:51`). The per-call seed from `std/random` keeps concurrent writers out of one shared temp (`std/storage.vyrn:40`). The typed helpers `save` / `load` / `loadOr` are global forms expanded at the call site, not module exports, so they add nothing to the export count (`std/storage.vyrn:11`).

## Findings

### 9. Disk and network IO — MEDIUM

What: every save does the write work twice over a plain write — a full temp-file write plus a rename — and a rename failure abandons its written temp forever because the module has no delete primitive.
Where: `std/storage.vyrn:58`.
Evidence: run from `N:\lang`: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/storage/b.vyrn`. Output: `bench "writeFile 1KiB" min 130.42 µs median 203.15 µs mean 218.73 µs`, `bench "writeAtomic 1KiB" min 537.15 µs median 794.40 µs mean 814.50 µs` — about 3.9x the baseline at 1 KiB, and the gap grows with content size because the payload is written twice. A successful save leaves no litter: after the bench run the scratch directory held only the two target files and zero `.tmp` siblings. The failure-path leak is structural: the match arm at `std/storage.vyrn:60` returns the rename error and nothing in the module deletes the temp afterwards.
Cost if unfixed: `examples/bin/server/persist.vyrn:42` rewrites the entire paste store through `writeAtomic` on every mutation, paying roughly 591 µs extra per 1 KiB save today and one leaked temp file per failed rename.
Smallest fix: expose a delete-or-cleanup primitive (or an `onRenameFailure` hook) so failed publishes stop accumulating `<path>.tmp.*` files; the double-write on success is the crash-consistency mechanism and should stay. `RECOMMENDATION, NOT A DECISION`.

### 15. Best/worst/average case — LOW

What: the worst case is unbounded disk growth — each failed rename leaves exactly one new uniquely named temp, and nothing reclaims it.
Where: `std/storage.vyrn:49`.
Evidence: the module's own test proves both arms: a missing destination directory fails the rename after a successful temp write (`std/storage.vyrn:92`), and the comment block at `std/storage.vyrn:87` records that this leak already reached a commit once. Accumulation rate over time is NOT MEASURED.
Cost if unfixed: a server that hits persistent rename failures (full disk, permission change) grows one stray file per attempt until an operator cleans up; `examples/bin/server/persist.vyrn:42` runs on that path on every save.
Smallest fix: same as finding 9 — one delete primitive closes the worst case. `RECOMMENDATION, NOT A DECISION`.

### 26. Syscall frequency — LOW

What: one logical save issues three host calls (`randomSeed`, `writeFile`, `renameFile`) where a plain save issues one.
Where: `std/storage.vyrn:52`.
Evidence: same bench command as finding 9. The overhead decomposition: `randomSeed` median 1.75 µs, seed-to-temp-name string work 1.93 µs (`bench "temp name build"`), so about 3 µs of the 591 µs delta is name minting and the rest is the second write plus rename syscalls.
Cost if unfixed: `examples/bin/server/persist.vyrn:42` triples its syscall count per save; at paste-server traffic this is invisible next to network costs.
Smallest fix: none — the extra syscalls are the atomicity mechanism itself. `RECOMMENDATION, NOT A DECISION`.

### 8. Allocation frequency — LOW

What: each call allocates the seed's decimal `String` and then the interpolated temp-name `String` before any IO starts.
Where: `std/storage.vyrn:53`.
Evidence: `bench "temp name build"` median 1.93 µs versus `bench "randomSeed"` median 1.55 µs, from the same bench command as finding 9; the name construction adds under half a microsecond per save.
Cost if unfixed: two short-lived allocations per save for every caller including `examples/bin/server/persist.vyrn:42`; unmeasurable against the 794 µs save.
Smallest fix: none worth making at this cost. `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 27, 28, 29, 30.
