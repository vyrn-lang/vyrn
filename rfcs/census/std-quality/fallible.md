# std/fallible.vyrn

Lines: 29. Exports: 0. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

The one top-level declaration is `export protocol Fallible` (`std/fallible.vyrn:25`), not an `export fn`; it carries an associated type `Output` and two method signatures, `isSuccess` and `success`. The module has no function bodies and no runtime code of its own.

## What this module is for

A caller implements `Fallible` for a sum type that is neither `Option` nor `Result`, so that the `?` operator works on values of that type (RFC-0080 M3). The operator resolves through the protocol: `isSuccess` says which side of the sum the value is on, and `success` reads the success payload. Propagation copies the whole failing sum to the caller unchanged. In-repo users are the guide demo (`site/guide/fallible.vyrn:12`) and the example program (`examples/fallible.vyrn:25`); no module in `std/` implements or imports it.

## Findings

### 10. Control flow predictability — LOW

What: `x?` on a `Fallible` type costs two protocol-method calls per propagation where `?` on `Option`/`Result` is an inline tag test, and the measured gap is about 5x.
Where: `std/fallible.vyrn:25`.
Evidence: bench file `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/fallible/b.vyrn`, run from `N:\lang` with `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/fallible/b.vyrn`. Two loops of 100000 propagations over the same data-dependent recurrence, one through `Step?` (a user type implementing `Fallible`, two variants with an `Int64` payload) and one through `Result<Int64, Int64>?`: "propagate fallible 100k" min 100.80 µs median 137.67 µs; "propagate result 100k" min 20.02 µs median 31.60 µs. A second run gave min 102.35 µs versus 20.96 µs. This matches the prediction recorded at `rfcs/RFC-0080-associated-types-and-generic-impls.md:359-360`: through the protocol the lowering becomes two calls instead of a tag test and an `extractvalue`.
Cost if unfixed: every user of `?` on a custom sum type pays roughly five times the nominal propagation cost; today the paying callers in this repository are the demos at `examples/fallible.vyrn:69` and `site/guide/fallible.vyrn`, which are not hot paths.
Smallest fix: have the backends inline the two monomorphized protocol methods at each `?` site so the Fallible path collapses to the same tag test the nominal path emits. `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
