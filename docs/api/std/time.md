# std/time

std/time — wall-clock time at the host boundary (RFC-0043).

Time is a host INPUT, not part of Vyrn's deterministic core: you must import
`now()`, and calling it is a host effect (forbidden in `gen`/comptime, like
I/O). Everything ELSE here — the UTC calendar breakdown and the formatters —
is pure Vyrn computed from the epoch milliseconds, so it is an ordinary
three-way parity citizen (interp == native == wasm) usable anywhere,
including generators. Parity holds for a clock example because the harness
FIXES the clock (`VYRN_FIXED_TIME`): every backend reads the same injected
millis.

v1 is UTC-only (no timezone database, no DST, no locale); a `+HH:MM` offset
is the escape hatch a later revision can add.

## Instant

```vyrn
type Instant = Int64 (validated)
```

Milliseconds since the Unix epoch (1970-01-01T00:00:00Z), UTC. A distinct
validated type so a timestamp can't be silently mixed with a plain count;
`toMillis`/`fromMillis` bridge to raw `Int64`.

## Civil

```vyrn
type Civil = { year: Int64, month: Int64, day: Int64 }
```

A UTC calendar date, the pure breakdown of an `Instant`'s day component.

## now

```vyrn
fn now() -> Instant
```

The current wall-clock instant (UTC epoch millis). HOST EFFECT: reads the
clock, so it is forbidden in generators/comptime and cannot cross a `spawn`
boundary (exactly like `print`/file I/O).

## monotonic

```vyrn
fn monotonic() -> Int64
```

A monotonic nanosecond reading for measuring elapsed time. HOST EFFECT. Use
this for durations (not `now()`, which can jump when the wall clock is set).

## toMillis

```vyrn
fn toMillis(i: Instant) -> Int64
```

The raw epoch milliseconds of an instant.

## fromMillis

```vyrn
fn fromMillis(n: Int64) -> Instant
```

An instant from raw epoch milliseconds (validated non-negative at the boundary).

## civil

```vyrn
fn civil(i: Instant) -> Civil
```

The UTC calendar date (year/month/day) of an instant.

## year

```vyrn
fn year(i: Instant) -> Int64
```

The UTC year of an instant (e.g. 2023).

## month

```vyrn
fn month(i: Instant) -> Int64
```

The UTC month of an instant (1..12).

## day

```vyrn
fn day(i: Instant) -> Int64
```

The UTC day-of-month of an instant (1..31).

## hour

```vyrn
fn hour(i: Instant) -> Int64
```

The UTC hour of an instant (0..23).

## minute

```vyrn
fn minute(i: Instant) -> Int64
```

The UTC minute of an instant (0..59).

## second

```vyrn
fn second(i: Instant) -> Int64
```

The UTC second of an instant (0..59).

## format

```vyrn
fn format(i: Instant) -> String
```

Human-readable UTC timestamp `YYYY-MM-DD HH:MM:SS` (a parity citizen: pure).

## formatIso

```vyrn
fn formatIso(i: Instant) -> String
```

ISO-8601 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` (a parity citizen: pure).
