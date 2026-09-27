# std/time

std/time: wall-clock time at the host boundary.

`now()` and `monotonic()` are host effects, so generators and comptime code
cannot call them; the parity harness fixes the clock with `VYRN_FIXED_TIME`.
The calendar breakdown and the formatters are pure. UTC only: no timezone
database, no DST, no locale.

## Instant

```vyrn
type Instant = Int64 (validated)
```

Milliseconds since the Unix epoch, UTC. A distinct type, so a timestamp does
not mix with a plain count.

## Civil

```vyrn
type Civil = { year: Int64, month: Int64, day: Int64 }
```

A UTC calendar date, the pure breakdown of an `Instant`'s day component.

## now

```vyrn
fn now() -> Instant
```

The current wall-clock instant.

## monotonic

```vyrn
fn monotonic() -> Int64
```

Monotonic nanoseconds, for durations: `now()` jumps when the wall clock is
set.

## toMillis

```vyrn
fn toMillis(i: Instant) -> Int64
```

## fromMillis

```vyrn
fn fromMillis(n: Int64) -> Instant
```

An instant from raw epoch milliseconds, validated non-negative.

## civil

```vyrn
fn civil(i: Instant) -> Civil
```

The UTC calendar date (year/month/day) of an instant.

## year

```vyrn
fn year(i: Instant) -> Int64
```

The UTC year of an instant.

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

UTC timestamp `YYYY-MM-DD HH:MM:SS`.

## formatIso

```vyrn
fn formatIso(i: Instant) -> String
```

ISO-8601 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ`.
