# std/jsondec.vyrn

Lines: 452. Exports: 24. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

`fromJson(T, s)` is generated per type by `vyrn-frontend`'s `jsondec.rs`, and the generated source calls this module for everything that is not type-directed: kind names, the RFC-0018 `Issue` vocabulary, path arithmetic, tree accessors, and the scalar decoders (`dStr`, `dBool`, `dInt64`, `dIntRange`, `dUIntMax`, `dFloat64`, `dFloat32`). Every decoder returns a zero-or-one-element array; empty means the issue is already recorded (RFC-0018 accumulation). Integers decode exactly through text, never through a `Float64`, and `Float32` parses directly instead of rounding twice (`std/jsondec.vyrn:349-368`). Outside the generated layer, `std/icons.vyrn:95` imports `fieldsOf`.

All benches below come from one run of
`compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/jsondec/b.vyrn`
from `N:\lang`; per-operation numbers are that run's min divided by the bench's iteration count.

## Findings

### 2. Algorithm complexity — MEDIUM

What: decoding a record does O(F²) key comparisons in its field count F, because each required field runs two linear scans (`hasField`, then `fieldAt`) over the F-entry snapshot.
Where: `std/jsondec.vyrn:104` (the scan loop) and `std/jsondec.vyrn:115-119` (the second scan). The generated body emits one `hasField` call plus one `fieldAt` call per required field — `compiler/vyrn-frontend/src/jsondec.rs:486-487` inside the per-field loop at `jsondec.rs:465`.
Evidence: bench "record shape n" reproduces the generated shape (snapshot once, then `hasField` + `fieldAt` + `isNull` per key). Min per bench: n=32 11.65 µs, n=64 26.10 µs, n=128 65.65 µs, n=256 194.22 µs. Eight times the fields costs 16.7 times the time, which is the quadratic prediction (16×); the proving loops are `while i < n` over all n keys at b.vyrn lines 20-36.
Cost if unfixed: every generated `fromJson` record decode pays it today; wide records such as the pinned corpus in `examples/jsondecbytes.vyrn` pay it on every decode.
Smallest fix: emit one `fieldAt`-style lookup per field that returns found/not-found plus the value, halving the constant; a keyed map would remove the quadratic term. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — HIGH

What: every tree accessor deep-copies the subtree it hands back, so a value nested d levels is copied about d times during one decode, and `fieldAt` copies an entire present value before the caller inspects it.
Where: `std/jsondec.vyrn:117` (`copyJson(f.value)`), `std/jsondec.vyrn:69` (`copyJsonFields`), `std/jsondec.vyrn:81` (`copyJsonArray`), `std/jsondec.vyrn:130` (`copyJson(items[i])`). The generated Option path calls `fieldAt` before the only thing it first asks, `isNull` — `compiler/vyrn-frontend/src/jsondec.rs:473-474`.
Evidence: same bench run. bench "fieldat copy of 4096-elem subtree": min 21.26 ms for 64 lookups, 332 µs per lookup-copy of a 4096-element `JArr`; bench "fieldat copy of scalar": min 131.71 µs for 64 lookups, 2.06 µs per lookup. A scalar decode also carries overhead beyond dispatch: bench "dStr carrier alloc x4096" min 170.80 µs, 41.7 ns per `dStr` call including the zero-or-one carrier array, against 0.51 ns for a plain match-and-read ("one-match dispatch x4096", min 2.09 µs).
Cost if unfixed: any decode of an array or record field copies its whole subtree at least twice (`fieldAt`, then `itemsOf`/`fieldsOf` again); `std/icons.vyrn:729` pays the snapshot copy for the whole icon document today, and nested targets like those pinned in `examples/jsondecbytes.vyrn` pay per level.
Smallest fix: let `fieldAt` return a borrow-shaped view or take a kind test (`isAbsent(fs, key)`) so the Option path never copies before checking, and drop the second copy in the generated member decode. RECOMMENDATION, NOT A DECISION.

### 10. Control flow predictability — LOW

What: scalar decoders classify a node by calling `kindName(v)` — which materializes a fresh heap `String` from a literal — and then compare that String, where one direct six-arm match answers without allocating; `tagOf`/`numText` then match a second time.
Where: `std/jsondec.vyrn:43-52` (`kindName`), `std/jsondec.vyrn:249-250` and `254` (`dStr`: match, compare, match again), repeated in `dBool` (:261-266), `dInt64` (:287-292), `dIntRange` (:309-314), `dUIntMax` (:334-339), `dFloat64` (:353-358), `dFloat32` (:372-376).
Evidence: bench "two-match dispatch x4096" (kindName + compare + tagOf) min 101.96 µs, 24.9 ns per classification; bench "one-match dispatch x4096" (single direct match) min 2.09 µs, 0.51 ns per classification — a 49× gap per decoded node.
Cost if unfixed: every scalar node in every document pays one String allocation plus comparison; large arrays of scalars pay it per element.
Smallest fix: match on the `Json` variant directly inside each scalar decoder and build the kind name only on the error path. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: path building allocates a new growing String per member and per array element during a decode.
Where: `std/jsondec.vyrn:214-216` (`indexPath`: `parent + "[" + i.toString() + "]"`), `std/jsondec.vyrn:206-211` (`fieldPath`). The generated array body emits one `indexPath` per element (`compiler/vyrn-frontend/src/jsondec.rs:516-521`).
Evidence: bench "indexPath 100 elements" min 10.61 µs, 106 ns per built path; bench "indexPath 400 elements" min 43.44 µs, 109 ns per built path — linear in element count as expected.
Cost if unfixed: an N-element array decode allocates N discarded Strings even when no issue is ever recorded; callers decoding large arrays pay it on every run.
Smallest fix: build the path lazily, only when an issue is actually pushed at that node. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 25, 26, 27, 28, 29, 30.
