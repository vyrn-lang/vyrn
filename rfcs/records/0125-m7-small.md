#### A SmallArray's calls and a borrow out of a join are rows the emitter reads (2026-09-25, `m7-small`)
RFC-0125, milestone M7.
Decision: from the lead's brief, one commit per census clause: `name-no-place:Array:Val`, then `name-no-place:SmallArray:Call` together with the element reads.
Went: `name-no-place:Array:Val` in jchain `main` and jsonplace `main`, because `core_renames` gives a borrow the place of the join it is bound out of. `name-no-place:SmallArray:Call`, `Enum:Call` and `Array:Call` in copy `smallArrays` and smallarray `main`: `@push`, `@pop`, `@swapRemove` and `@toArray` on a SmallArray receiver. The element reads needed no change. Stayed: tryplace `main`, because of three `stmt:Switch`.
Lines: `direct.rs` 22,084 to 22,083 to 22,094. `sa_method` became `sa_open`, `sa_to_array`, `sa_pop` and `sa_swap_remove`, entered from `arr_rebuild`, `pop_at` and `swap_remove_at`. So the arm and the rows share one entry, and the arm's `@pop`/`@swapRemove` peek dispatch went. The added lines are the three new signatures. `core.rs` +5 (the `@toArray` `Builds` row). Refusals: 0 lost / 0 gained. Manifest: 4 rows, below.
Licence:
- `coredrive` with the manifest check, on 4f8a57e6 as two shards: taken 21,096 after the first commit and 21,098 after the second, of 21,172. Every program runs the same. On ab947e5f the same commits took 21,084 and 21,086 against main's 21,082.
- `wasm2wat`, `VYRN_WASM_NAMES=1`, each commit against its parent's binary. Only `$main` or `$smallArrays` differs. jchain: `memory.copy` 38 to 27 in the module, frame 320 to 112 bytes. jsonplace: 43 to 31, frame 336 to 80. copy: 10 to 7, frame 160 unchanged. smallarray: 18 to 12, frame 528 to 192.
- `VYRN_LEAK_CHECK=1 vyrn run` of jchain, jsonplace, tryplace, copy and smallarray with each parent's binary and each commit's: the same stdout, stderr and exit code, and no leak line.
- the shapes `a-borrowed-layout-bound-by-a-refutable-let` (36) and `a-smallarray-shrunk-and-copied-out-by-core-rows` (231): the same value from both binaries with the core walk on and off, under `VYRN_LEAK_CHECK=1`. The core takes `vyrnTestMain`.
- `residue --ignored` after each commit: engine 173 clean, route 173 clean, 0 leaking. `kernel` 27,061 / 0 / 0.
- `emitter-census`, `emitter-reads` and `surface` re-pinned in the commit that moved each.
Time: about 240 minutes: work 60, gates 110, two rebases 30, item 3 and the singles 40.
Findings:
- 0dcc2ef1 was queue item 2. On #495 it moved one body; on #500 it moves two, because #498 gave jchain's `@at` its row.
- `coredrive` took 235 s to 557 s while four other tracks ran theirs. One run with the wasmhash suite hit the 600 s timeout.
Left:
- `value(x)` and `list([..])` rows (the queue's item 3; a patch is set aside). This moves tagged and templates `main`, but the code gets worse. tagged `main`: `memory.copy` 0 to 2, frame 64 to 96, and two new release functions. The row makes the fixed literal in the frame and copies it to the heap. Blocked until `list([..])` of a literal is built straight into its heap buffer.
- show `main`: `value(p)` of a type with `impl Show` boxes through `show`, which the row does not state.
- fieldmut `main` (`Make(Map)`), reflection `main` (`schemaOf`, which the emitter expands from the AST) and shadowing `shadowedByLambdaParam` (`applyTo`, `no-core:has-body`). Each needs a row that no track has.
