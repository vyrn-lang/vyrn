# std/strings.vyrn

Lines: 434. Exports: 19. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

Callers import byte-oriented text helpers written in Vyrn itself: search (`indexOf`, `lastIndexOf`), splitting (`split`, `lines`, `splitWhitespace`), trimming and padding (`trim`, `padStart`, `padEnd`), ASCII case mapping (`toLower`, `toUpper`), rebuilding (`joinWith`, `repeat`, `replace`), hex formatting (`toHex`), did-you-mean distance (`editDistance`), and `fromBytesOr`, the one shared home for a known-valid UTF-8 decode. All offsets are byte offsets; all cuts land on codepoint boundaries or fail loudly. 101 callsites outside this module call `joinWith`; `std/http.vyrn`, `std/cli.vyrn`, `std/tw.vyrn`, and most of `site/app/` import from here.

## Findings

### 8. Allocation frequency — MEDIUM

What: `split` and `replace` copy the 256-entry skip table once per match inside their match loops, so every match pays an array allocation plus a 2 KiB copy.

Where: `std/strings.vyrn:170` (`skip.copy()` inside the `while i + sl <= s.byteLength` loop of `split`) and `std/strings.vyrn:302` (the same pattern in `replace`). `findSkipping` only reads the table (`std/strpred.vyrn:198` indexes it, never writes), so the copy protects nothing.

Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/strings/b.vyrn` run from `N:\lang`. Two copies of the `split` loop over a 16000-byte haystack (`"xy, "` repeated 4000 times, so 4000 matches of the 2-byte separator `", "`): with `skip.copy()` per match, median 3.33 ms (min 2.26 ms); passing `skip` directly, median 429 µs (min 216 µs). The copy costs about 8x on this shape. The `replace` bench over the same haystack matches: median 3.42 ms.

Cost if unfixed: every caller that splits or replaces a document-sized string with a separator of two or more bytes pays an allocation per match today — `site/app/apidoc.vyrn:268` splits escaped doc pages on `` "``" `` during API documentation rendering, and `site/app/pagemd.vyrn:265` runs `replace(quotes, "&amp;", "&")` over page-sized markdown.

Smallest fix: hoist one `.copy()` out of the loop, or pass `skip` directly since `findSkipping` does not mutate it. `RECOMMENDATION, NOT A DECISION`.

### 15. Best/worst/average case — LOW

What: `indexOf` degrades to a step-1 scan that still pays a table lookup per position when the mismatching byte sits at the needle's end, because then the skip entry is 1.

Where: `std/strings.vyrn:116-120` dispatches to `findSkipping` with a table from `skipTable`; the table gives skip 1 whenever the needle's second-to-last byte equals its last (`std/strpred.vyrn:127-135` fills entries for positions `0..nl-2` only).

Evidence: same bench command. On a 32768-byte all-`a` haystack with the absent 64-byte needle `"b" + "a"` repeated: `indexOf` median 1.63 ms, while `lastIndexOf`'s unprepared backward scan over the identical input ran median 114 µs — about 14x faster without any table.

Cost if unfixed: `contains` and `indexOf` are hot in template compilation — `std/vyx` asks about short strings millions of times while compiling templates (`std/strpred.vyrn:174-177`) — and needles like `"//"` or `"ee"` hit skip 1 on prose-like input.

Smallest fix: fall back to `findPlain` when the computed skip would be 1, in `std/strpred`'s dispatcher. `RECOMMENDATION, NOT A DECISION`.

### 7. Peak memory use — LOW

What: `editDistance` allocates the full `(n+1)*(m+1)` Int64 matrix, but its inner loop reads only rows `r-2`, `r-1`, and `r`, so three rows suffice.

Where: `std/strings.vyrn:389-394` pushes `(n + 1) * w` zeros one by one; the reads at `std/strings.vyrn:413`, `418`, and `423` never reach past row `r-2`. Peak memory is therefore `8*(n+1)*(m+1)` bytes where `O(m)` rows would do.

Evidence: allocation size proven by the loop bound at `std/strings.vyrn:391` (`while k < (n + 1) * w`). Runtime measured with the same bench command: a 48x48 distance takes median 12.25 µs, so time is not the problem today.

Cost if unfixed: `std/icons.vyrn:645` and `std/contract.vyrn:281` compute distances over identifier-length keys, where the matrix is a few KiB; a future caller measuring long strings would pay quadratically more memory than needed.

Smallest fix: keep three row buffers of length `w` and index modulo 3. `RECOMMENDATION, NOT A DECISION`.

### 28. Initialization overhead — LOW

What: `toHex` builds the 16-byte digit lookup array by calling `bytes("0123456789abcdef")` on every call.

Where: `std/strings.vyrn:350`.

Evidence: same bench command: 1024 `toHex` calls take median 350 µs, about 342 ns per call including the whole function; the digit-array construction is pure overhead because a `String` already supports byte indexing (`std/strings.vyrn:192` uses `s[i] == 10`).

Cost if unfixed: `std/http.vyrn:1007` calls `toHex` to compute an ETag on every response body, one wasted allocation per response.

Smallest fix: drop `db` and index the string literal directly. `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 29, 30.
