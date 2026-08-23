# std/hash.vyrn

Lines: 173. Exports: 4 (`fnv1a`, `fnv1aStr`, `sha1`, `sha1Hex`; all top-level exports are functions, no exported types or constants). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

Non-cryptographic byte hashing. `fnv1a` and `fnv1aStr` give a 64-bit FNV-1a digest for hash tables and content-addressed ids; `sha1` and `sha1Hex` exist for exactly one purpose, the RFC 6455 WebSocket handshake accept key (`std/hash.vyrn:36-55`). Callers today: `std/http.vyrn:707` (handshake), `std/http.vyrn:1007` (`httpEtag` hashes each response body with `fnv1aStr`), `std/jsonread.vyrn:403,419,450` (key-set probing), `examples/vlog.vyrn:370`, `examples/bin/server/util.vyrn:51`.

## Findings

### 8. Allocation frequency — MEDIUM

What: `sha1` constructs one fresh 80-element `Array<UInt64>` per 64-byte block and grows it by 80 pushes, and `fnv1aStr` builds a full byte copy of its input string on every call.
Where: `std/hash.vyrn:86` (schedule array allocated inside the per-block loop that starts at line 84), `std/hash.vyrn:31` (`bytes(s)` copy per call).
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/hash/b.vyrn` from `N:\lang` gave min values over the same 65536-byte ASCII input: fill-only 57.18 µs, `fnv1a` 118.31 µs, `sha1` 633.30 µs — sha1 costs 576 µs per call against fnv1a's 61 µs, a 9.4x gap per byte on identical input. A 32-byte `sha1` call takes 764 ns min, so fixed per-call setup dominates small inputs. The schedule-array loop runs ⌈n/64⌉ times with 80 pushes each (lines 93-95), proving O(n/64) array constructions per call.
Cost if unfixed: `std/http.vyrn:1007` pays it on every cacheable response — `httpEtag` copies the whole body through `bytes(s)` inside `fnv1aStr` before hashing.
Smallest fix: hoist the message-schedule array out of the block loop into one preallocated 80-word buffer reused across blocks, and add an `fnv1aBytes(s)` path that walks the string without materializing `bytes(s)`. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — LOW

What: `sha1` duplicates the entire message before hashing, so peak live memory reaches roughly twice the input size plus up to 71 pad bytes.
Where: `std/hash.vyrn:65-68` (byte-for-byte copy of `data` into `msg`) and `std/hash.vyrn:69-81` (padding appended to the copy).
Evidence: the copy loop at lines 66-68 runs once per input byte, so the extra buffer is O(n); absolute allocation peak NOT MEASURED. Timing side of the same copy shows up in the bench gap above (9.4x slower per byte than the zero-copy `fnv1a`).
Cost if unfixed: any caller hashing large payloads doubles resident memory for the duration of the digest; current callers pass only short WebSocket keys (`std/http.vyrn:707`), so nobody pays meaningfully today.
Smallest fix: hash blocks straight out of `data` and handle only the final padded block in a scratch buffer. RECOMMENDATION, NOT A DECISION.

### 22. Vectorization — LOW

What: no SIMD or SWAR anywhere in the module; both hashes run as scalar serial loops.
Where: `std/hash.vyrn:22-24` (FNV-1a recurrence: each step multiplies the previous `h`, so a serial data-dependency chain spans the whole input) and `std/hash.vyrn:104-125` (SHA-1 rounds chain `a..e` serially).
Evidence: measured throughput from the bench command above is 65536 bytes / 61.13 µs ≈ 1.07 GB/s (0.93 ns/byte) for `fnv1a`; SIMD or SWAR forms of FNV would need a different digest definition.
Cost if unfixed: callers hashing bulk data get ~1 GB/s instead of several; `examples/vlog.vyrn:370` fingerprints a whole joined corpus through this loop.
Smallest fix: none available without changing the published digest values; document the serial dependency instead. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 30.
