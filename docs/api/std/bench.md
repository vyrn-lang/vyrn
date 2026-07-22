# std/bench

std/bench — the benchmarking harness helpers (RFC-0055).

This module is the runtime the `vyrn bench` transform links against: the
per-bench timing loop (`benchOne`) plus the pure statistics and formatting it
prints. It is never part of a `run`/`build`/`emit-ir` compile — only the
synthesized bench `main` imports it — so its host-effectful clock use is fine.

The *pure* helpers (`minOf`/`median`/`mean`/`formatDuration`/`padRight`) do no
I/O and touch no clock, so they are ordinary Vyrn computed from their inputs:
unit-testable in Vyrn itself (see the `test` blocks below, run with
`vyrn test std/bench.vyrn`).

## minOf

```vyrn
fn minOf(xs: Array<Int64>) -> Int64
```

The minimum of a non-empty sample array (per-iteration nanoseconds).

## mean

```vyrn
fn mean(xs: Array<Int64>) -> Int64
```

The arithmetic mean (integer, truncating) of a non-empty sample array.

## median

```vyrn
fn median(xs: Array<Int64>) -> Int64
```

The median (upper-middle element) of a non-empty sample array.

## formatDuration

```vyrn
fn formatDuration(ns: Int64) -> String
```

A nanosecond duration in human units: `ns` below 1 µs, then `µs`/`ms`/`s`
with two fractional digits. Pure — used for min/median/mean rendering.

## padRight

```vyrn
fn padRight(s: String, width: Int64) -> String
```

Right-pad `s` with spaces to `width` (measured in bytes — bench names are the
only variable-width field, and the column widths are computed the same way).

## BenchResult

```vyrn
type BenchResult = { name: String, minNs: Int64, medianNs: Int64, meanNs: Int64, samples: Int64, iters: Int64 }
```

One bench's measured statistics (RFC-0063): per-iteration nanosecond
aggregates plus the sample × iteration counts the loop settled on. The stable
statistic for regression comparison is `minNs` — the least noise-perturbed.

## benchMeasure

```vyrn
fn benchMeasure(name: String, body: fn()) -> BenchResult
```

Time one bench body and RETURN its statistics (RFC-0055 §Harness, factored out
by RFC-0063 so both the human report and the `--json` record share one loop).
The divan-simplified loop:

  1. warm up ~50 ms of iterations (discarded);
  2. auto-scale the per-sample iteration count until one sample takes ≥ 1 ms;
  3. collect samples until ≥ 31 of them AND ≥ 500 ms elapsed (cap: 2 s);
  4. compute min / median / mean per-iteration time.

`body` is called through a monomorphized function value (RFC-0023), and the
benched work is kept alive by `blackBox` inside the body, so the compiler
cannot delete it between iterations.

## benchOne

```vyrn
fn benchOne(name: String, width: Int64, body: fn()) -> Unit
```

Time one bench body and print its aligned human report line (RFC-0055). `width`
is the label column width the caller pre-computed from every bench name so the
columns align. Delegates the timing to `benchMeasure`.

## benchJson

```vyrn
fn benchJson(results: Array<BenchResult>, backend: String, opt: String) -> String
```

The machine-readable bench report (RFC-0063 §1) — a `std/json` tree emitted
compact to stdout. `backend`/`opt` are metadata (always `native`/`O2` today);
`results` are in stable declaration order. The schema is
`{ backend, opt, benches: [ { name, minNs, medianNs, meanNs, samples, iters } ] }`
with every number an integer, so it round-trips through `parseJson`.
