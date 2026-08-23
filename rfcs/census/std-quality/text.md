# std/text.vyrn

Lines: 333. Exports: 6. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

`std/text` holds the Vyrn implementations of four retired builtins: `decodeUtf8` and `chars` decode a byte buffer or a `String` into Unicode scalar values, `charCountV` counts scalars with a continuation-byte scan, and `lineAtV`/`colAtV` map a byte offset to a 1-based line and column. A caller imports `chars` directly (RFC-0094 M2) or reaches `charCountV`, `lineAtV`, `colAtV` through the interpreter's route table; each Vyrn body stays in the file as the live oracle the builtin is proved against (`tests/text.rs`, `examples/textbytes.vyrn:28`). The module also exports `showCps`, a pinning helper that prints codepoints as decimal text.

## Findings

### 2. Algorithm complexity — MEDIUM

What: `lineAtV` scans from byte 0 to `off` and `colAtV` walks back to the previous LF, so both are linear in the distance, where the memoized builtins are O(1).
Where: `std/text.vyrn:186` (forward scan, loop at :193-198), `std/text.vyrn:211` (backward walk, loop at :217-220); the header states the trade at :16-21.
Evidence: command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/text/c.vyrn`, 1 MB buffer with an LF every 64 bytes, 64 calls per sample: `lineAtV x64 off 1000` min 781.80 µs vs `lineAtV x64 off 999000` min 10.02 ms — one full 1 MB forward scan costs about 147 µs. On a buffer whose second half has no LF: `colAtV x64 just after lf` min 1.06 ms vs `colAtV x64 long line` min 14.73 ms — one half-megabyte backward walk costs about 217 µs.
Cost if unfixed: today only `examples/textbytes.vyrn:54` and this module's tests (:314-332) call the Vyrn versions; production callers use the memoized builtins through `std/vyx.vyrn:226` and `std/vyx.vyrn:233`, which exist because this exact loop shape cost `std/vyx` 122 ms of a 291 ms page compile (`std/text.vyrn:19`). Any future caller of `lineAtV` inside a per-node diagnostic path pays the scan again.
Smallest fix: none exists in Vyrn — a generator may not keep module state (`std/text.vyrn:16-18`), so hot callers must keep routing to the builtins; document that rule on the two exports. RECOMMENDATION, NOT A DECISION.

### 7. Peak memory use — LOW

What: `chars(s)` holds the byte copy from `bytes(s)` and the result `Array<Int64>` alive at the same time, up to 9 bytes of transient allocation per input byte, when some callers only need the count.
Where: `std/text.vyrn:145` (the `bytes(s)` copy), `std/text.vyrn:68` and :130 (the result array grown by `push`).
Evidence: peak bytes NOT MEASURED; the cost shows in time. Command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/text/b.vyrn`, same 100 KB ASCII string built by `stringFromBytes` in both bodies: `charCountV 100000 ascii` min 249.17 µs / median 268.25 µs vs `chars 100000 ascii` min 351.20 µs / median 454.22 µs — the decoding route is 1.4x to 1.7x slower on top of its allocations.
Cost if unfixed: `examples/encoding.vyrn:18-19` pays it today with `chars(s).length`, building an `Array<Int64>` the caller throws away; the module itself names this anti-pattern at `std/text.vyrn:154-156`.
Smallest fix: point count-only callers at `charCountV` in `examples/encoding.vyrn`. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: `showCps` builds one intermediate `String` per element by repeated concatenation, n strings for n codepoints.
Where: `std/text.vyrn:230-236` (`out = out + "," + c.toString()`).
Evidence: command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/text/b.vyrn`: `showCps 2000` min 92.16 µs vs `showCps 8000` min 368.45 µs — 4x data costs 4x time, so growth is linear, but the constant is about 46 ns per codepoint at n=8000.
Cost if unfixed: callers are test-only today — `examples/textbytes.vyrn:35`, :42, :46 and this module's tests — so the cost lands on parity runs, not production paths.
Smallest fix: none needed while callers stay test-only; if `showCps` ever serves non-test output, accumulate parts and join once. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
