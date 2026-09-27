# std/bench

std/bench: the runtime the `vyrn bench` transform links against.
Only the synthesized bench `main` imports it, so its clock use never reaches
a `run` or `build`.

## minOf

```vyrn
fn minOf(xs: Array<Int64>) -> Int64
```

The minimum of a non-empty sample array.

## mean

```vyrn
fn mean(xs: Array<Int64>) -> Int64
```

The truncating integer mean of a non-empty sample array.

## median

```vyrn
fn median(xs: Array<Int64>) -> Int64
```

The median (upper-middle element) of a non-empty sample array.

## formatDuration

```vyrn
fn formatDuration(ns: Int64) -> String
```

A nanosecond duration in human units: `ns` below one microsecond, then
microseconds, `ms` or `s` with two fractional digits.

## padRight

```vyrn
fn padRight(s: String, width: Int64) -> String
```

Right-pads `s` with spaces to `width` bytes; never truncates.

## BenchResult

```vyrn
type BenchResult = { name: String, minNs: Int64, medianNs: Int64, meanNs: Int64, samples: Int64, iters: Int64 }
```

One bench's per-iteration statistics in nanoseconds, and the
sample and iteration counts. Compare regressions on `minNs`, the least
noisy.

## benchMeasure

```vyrn
fn benchMeasure(name: String, body: fn()) -> BenchResult
```

Times one bench body and returns its statistics:

  1. warm up for about 50 ms, discarding results;
  2. double the per-sample iteration count until one sample takes 1 ms;
  3. collect samples until at least 31 and 500 ms, capped at 2 s;
  4. compute min, median and mean per-iteration time.

The body keeps its work alive with `blackBox`, so the compiler cannot delete
it between iterations.

## benchOne

```vyrn
fn benchOne(name: String, width: Int64, body: fn()) -> Unit
```

Times one bench body and prints its report line. `width` is the label column
width, computed from every bench name.

## benchJson

```vyrn
fn benchJson(results: Array<BenchResult>, backend: String, opt: String) -> String
```

The `--json` bench report: compact, benches in
declaration order, every number an integer:
`{ backend, opt, benches: [ { name, minNs, medianNs, meanNs, samples, iters } ] }`.
