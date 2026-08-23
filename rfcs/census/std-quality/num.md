# std/num.vyrn

Lines: 884. Exports: 5 (`parseFloat64`, `parseFloat32`, `parseInt64`, `parseUInt64`, `f64Str`). The types `Dec` and `Scanned` are file-private; the module imports nothing. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller uses it to turn decimal text into numbers and one number back into text. `parseFloat64` and `parseFloat32` round correctly through exact digit-array arithmetic instead of floating point; `parseInt64` and `parseUInt64` refuse overflow where the builtin `parse` wraps; `f64Str` prints the fixed six decimal places that `@str` and `print` lower to on both compiled backends. Every engine runs the same Vyrn code over two primitives, `floatBits` and `floatFromBits`.

## Findings

### 2. Algorithm complexity — HIGH

What: `parseFloat64` scales its digit array by repeated whole-array passes, so cost grows linearly in the decimal exponent times the digit count.
Where: `std/num.vyrn:312-327` (scaling loops), with each pass an O(digits) long division at `std/num.vyrn:95-100`. The comment at `std/num.vyrn:307-310` states each 32-bit pass moves the exponent by nine to ten, so `"1e300"` needs about 31 coarse passes plus up to about 35 single-bit passes, each rebuilding an array of up to 800 digits.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/num/b.vyrn` from N:\lang measured `parseFloat64 "1e300"` min 202.60 µs against `parseFloat64 "12345.678"` min 5.02 µs and `parseInt64` (no scaling) min 29 ns.
Cost if unfixed: `std/jsondec.vyrn:36` imports all four parsers and `dFloat64` at `std/jsondec.vyrn:351` decodes every JSON number through them, so any document containing a large-exponent literal pays hundreds of microseconds per value.
Smallest fix: scale by precomputed table powers over base-10^k limbs so one pass consumes many exponent units, the chunking `f64Str` already applies at `std/num.vyrn:579-607`. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: every scaling pass allocates two or three short-lived digit arrays for a result that is eight bytes.
Where: `std/num.vyrn:92` builds `out`, `std/num.vyrn:116` builds `o2`, and `tidy` at `std/num.vyrn:71-77` copies again into a third array; the `"1e300"` path runs the loops at `std/num.vyrn:312-315` about 31 times plus the single-bit loops at `std/num.vyrn:320-327`, which proves at least roughly 130 fresh arrays per such parse.
Evidence: same bench command as above; `parseFloat64 "900-digit input"` min 69.80 µs and `parseFloat64 "1e300"` min 202.60 µs against `parseInt64 "19 digits"` min 34 ns, which does comparable per-digit work with no intermediate arrays. An exact allocation count is NOT MEASURED.
Cost if unfixed: the JSON decode path in `std/jsondec.vyrn:36` pays this allocation churn for every non-trivial numeric literal in a document.
Smallest fix: reuse one scratch buffer across passes inside `toFloat`, since no pass needs the previous digits after its loop ends. RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — MEDIUM

What: worst-case inputs sit two orders of magnitude above average ones for both directions of conversion.
Where: the exponent guards at `std/num.vyrn:297-301` allow any exponent within ±400 to enter full scaling, and `f64Str` reaches `reps` = 1074 for a smallest subnormal per its own doc at `std/num.vyrn:509-513`.
Evidence: same bench command as above. Parse: `"12345.678"` min 5.02 µs, `"5e-324"` min 88.45 µs, `"1e300"` min 202.60 µs — a 40× spread. Format: `f64Str(0.1)` min 464 ns, `f64Str(smallest subnormal)` min 96.63 µs, `f64Str(1e300)` min 198.12 µs — a 420× spread.
Cost if unfixed: `@str` and `print` on a float route here on both compiled backends per `std/num.vyrn:516-519`, so printing an extreme-magnitude computed float costs hundreds of microseconds wherever it happens.
Smallest fix: none smaller than fixing the scaling complexity in finding 2; the spread is that complexity seen from the input side. RECOMMENDATION, NOT A DECISION.

### 21. Footprint size — MEDIUM

What: any program that prints or stringifies one float links the entire 884-line module.
Where: the loader gate is described at `rfcs/RFC-0081-float-formatting-in-vyrn.md:217-218`: a mention of `@str` or `print` injects `std/num`.
Evidence: `compiler/target/release/vyrn build <tiny> -o <out> --target wasm` on two scratch files that differ only in one `print(0.1)` emitted 1413 bytes versus 7680 bytes — 6267 extra wasm bytes, 5.4×, for one printed float. The per-function share of those bytes is NOT MEASURED.
Cost if unfixed: every example in `examples/` and most site programs print floats and carry the parser plus formatter they never call.
Smallest fix: none available without backend work, since the injection exists so all three engines agree byte for byte; record it as accepted cost. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 22, 23, 24, 25, 26, 27, 28, 29, 30.
