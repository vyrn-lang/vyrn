# std/time.vyrn

Lines: 148. Exports: 13. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

Exports are 13 top-level `export fn` declarations. Two further exports are types, not functions: `Instant` (std/time.vyrn:18) and `Civil` (std/time.vyrn:21).

## What this module is for

A caller imports `now()` to read the wall clock as epoch millis and `monotonic()` to measure elapsed time. Both are host effects behind shim-implemented externs (std/time.vyrn:27,32). Everything else is pure Vyrn: `civil` breaks an instant into UTC year/month/day with Howard Hinnant's `civil_from_days`, and `format`/`formatIso` render `YYYY-MM-DD HH:MM:SS` strings. In-repo callers include `std/http.vyrn:1208-1209` (HTTP date headers), `examples/vlog.vyrn:32`, and the site routes `bin/app/routes/index.vyx:17`.

## Findings

### 2. Algorithm complexity — LOW

What: `year`, `month`, and `day` each run the full calendar division chain independently, so a caller wanting all three fields pays the work three times.
Where: `std/time.vyrn:81-93`.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/time/b.vyrn` printed `bench "civil once" min 7 ns median 7 ns mean 7 ns` versus `bench "year+month+day" min 21 ns median 22 ns mean 23 ns`. Three separate O(1) computations replace one; the proving bodies differ only in calling `civil(i)` once versus `year(i) + month(i) + day(i)`.
Cost if unfixed: `examples/clock.vyrn:30` calls `year(i)`, `month(i)`, and `day(i)` on one instant and pays 21 ns instead of 7 ns per breakdown.
Smallest fix: document `civil(i)` as the multi-field entry point so callers take fields from one `Civil` value. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: one `format` call builds nine short-lived heap strings through interpolation, roughly 100 times the cost of the pure arithmetic path.
Where: `std/time.vyrn:135-139` builds `pad4(c.year)`, five `pad2` results (defined at `std/time.vyrn:113-118` and `std/time.vyrn:121-132`), then concatenates `date`, `time`, and the final string.
Evidence: the same bench run printed `bench "hour+minute+second" min 1 ns median 1 ns mean 1 ns` and `bench "civil once" min 7 ns`, while `bench "format" min 675 ns median 710 ns mean 731 ns`. The gap between the arithmetic path (~8 ns) and `format` (~700 ns) is the string construction work. NOT MEASURED: the per-string allocator share specifically.
Cost if unfixed: `bin/app/routes/index.vyx:17` imports `format` and runs it on every paste page render; 0.7 microseconds per render is small next to page assembly, but `formatIso` at `std/time.vyrn:143-147` repeats the same pattern.
Smallest fix: build the timestamp into one preallocated buffer instead of nine interpolations, once the language exposes such an API. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
