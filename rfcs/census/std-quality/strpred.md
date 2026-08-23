# std/strpred.vyrn

Lines: 313. Exports: 9. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

Other export kinds beside `export fn`: one `export type`, `SliceError` (`std/strpred.vyrn:239`).

## What this module is for

Callers import the four string predicates that used to be builtins — `startsWith`, `endsWith`, `contains`, `slice` — plus the scanning core (`findPlain`, `findSkipping`, `skipTable`, `worthPreparing`) and `byteLengthV`. Everything works on UTF-8 bytes through the byte view (`s[i]`, `bytes(s)`), so all three backends run the same Vyrn source. `slice` returns `Result<String, SliceError>` and names the byte offset that failed. Callers include 21 `std/` modules and most of `site/app/`.

## Findings

### 8. Allocation frequency — MEDIUM

What: every call to `startsWith`, `endsWith` or `contains` allocates two to three whole-string byte copies, because they read lengths through `byteLengthV`, which is `bytes(s).length`.
Where: `std/strpred.vyrn:64-66` (the allocating body), used at `std/strpred.vyrn:71,72` (`startsWith`), `87,91` (`endsWith`), and `204,205,215` (`contains`). The module itself states the field form does not allocate: `std/strpred.vyrn:270-276`.
Evidence: command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/strpred/b.vyrn` from N:\lang. `byteLengthV x100000 (allocates)` min 2.59 ms → about 26 ns per call for a 32-byte string; `s.byteLength x100000 (strlen)` min 0 ns, folded at compile time. On a vyx-shaped call (47-byte haystack, needle `class`, 20000 iterations): exported `contains tag-ask x20000 (exported)` min 1.45 ms → 72.5 ns per call; a line-for-line copy of the same scan written on `.byteLength` ran min 89.65 µs → 4.5 ns per call. The exported path is about 16 times slower per call on this input.
Cost if unfixed: `std/vyx.vyrn:146` imports `contains`, `startsWith` and `endsWith`; the module's own comment (`std/strpred.vyrn:211-214`) says vyx asks `contains` millions of times while compiling templates. The in-file counter-measurement (`std/strpred.vyrn:52-58`: generator app 933 ms -> 951 ms) bounds the end-to-end cost near 2 per cent, which is why this is MEDIUM, not HIGH.
Smallest fix: replace `byteLengthV(s)` with `s.byteLength` at the nine use sites above and keep `byteLengthV` only as the public function form. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: `contains` rebuilds the 256-entry skip table with 256 `push` calls on every call whose haystack reaches the 512-byte threshold.
Where: `std/strpred.vyrn:125-130` (the push loop), entered from `std/strpred.vyrn:208-210`.
Evidence: same bench file and command. `skipTable alone x10000` min 6.62 ms → about 0.66 µs per table build. `contains big-hay x2000 (builds table per call)` min 8.76 ms → 4.4 µs per call over a 4096-byte haystack with needle `find`; the same search reusing one prepared table (`findSkipping`) ran min 5.60 ms → 2.8 µs per call. The per-call table build is roughly a third of the call.
Cost if unfixed: any caller that asks `contains` repeatedly about the same long needle pays the build each time; `std/graphql.vyrn:86` and `std/rpc.vyrn:47` import both `contains` and the scanning core, so the prepared path exists and is one import away.
Smallest fix: none inside `contains` without changing its signature — the fix is caller-side, preparing one table with `skipTable` and calling `findSkipping`, which the exports already support. RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — LOW

What: `findSkipping` has an O(n·m) worst case. With only the bad-character rule, a window can match m−1 bytes, fail on the last, and advance by one.
Where: `std/strpred.vyrn:190-199`; the step of one comes from `std/strpred.vyrn:198` reading `skip['a'] = nl - 1 - j` with the needle's last `a` at j = m−2 (`std/strpred.vyrn:132-134`).
Evidence: same bench file and command, needle `b` + 15×`a`, 32768-byte haystacks. Worst case (`32768a`): min 236.25 µs minus the 68.59 µs `setup: repeatByte 32768` → about 168 µs for 32753 windows × up to 16 comparisons ≈ 524k comparisons, proving O(n·m). Best case (`32768z`, one comparison then a full-needle jump): min 73.81 µs − 68.59 µs → about 5 µs, close to O(n/m). A 30-fold gap between best and worst on identical sizes.
Cost if unfixed: `std/strings.vyrn:31` re-exports `findPlain`, `findSkipping` and `skipTable`, so any `indexOf`-shaped caller over attacker-chosen text can hit the quadratic shape; no in-repo caller feeds adversarial data today.
Smallest fix: none recommended — the naive fallback `findPlain` is also O(n·m), and the module documents the plain scan as what the old builtin did (`std/strpred.vyrn:105`). Record the bound; changing the algorithm is an owner decision. RECOMMENDATION, NOT A DECISION.

### 22. Vectorization — LOW

What: `slice` copies its range one byte per loop iteration with array `push` and then re-walks the bytes in `stringFromBytes`, where the deleted builtin was one `memcpy`.
Where: `std/strpred.vyrn:303-309`.
Evidence: same bench file and command. `slice 64B token from 64KB src x4096` min 1.12 ms minus `setup: repeatByte 65536` min 131.01 µs → about 989 µs for 4096 slices × 64 bytes = 256 KiB copied, about 240 ns per token or 3.8 ns per byte. The runtime's whole-string bulk copy, `bytes() bulk copy 64KB x4096`, min 80.07 ms → 19.5 µs per 64 KiB, about 0.30 ns per byte. The per-byte loop is roughly 13 times slower per byte than the bulk copy.
Cost if unfixed: `std/scan.vyrn:17` imports `slice` and calls it once per token (`std/strpred.vyrn:272-274` states this), so a scanner over a large source pays about 240 ns per token here.
Smallest fix: none available in pure Vyrn today — the doc records the same trade and declines it because `stringFromBytes` is the only String construction from bytes (`std/strpred.vyrn:279-288`). RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 30.
