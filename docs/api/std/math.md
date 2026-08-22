# std/math

std/math — numeric helpers, written in Vyrn itself. Being ordinary Vyrn,
everything here gets interpreter == native == wasm parity for free.

## min

```vyrn
fn min(a: Int64, b: Int64) -> Int64
```

## max

```vyrn
fn max(a: Int64, b: Int64) -> Int64
```

## abs

```vyrn
fn abs(x: Int64) -> Int64
```

Absolute value.

`Int64.min` has no positive opposite — the language wraps, so `0 - x` would
hand the minimum straight back and `abs` would return NEGATIVE. There is no
exact answer, so it saturates to `Int64.max`, the nearest representable
magnitude, and the doc says so rather than leaving a trap.

## clamp

```vyrn
fn clamp(x: Int64, lo: Int64, hi: Int64) -> Int64
```

Clamp `x` into the inclusive range [lo, hi].

## pi

```vyrn
fn pi() -> Float64
```

Pi, to the last bit a `Float64` holds.

## floorF

```vyrn
fn floorF(x: Float64) -> Float64
```

The greatest whole number at or below `x`, as a `Float64`.
`Int64(x)` truncates towards zero, so a negative non-integer needs one step
down. Values past the `Int64` range are already whole and pass through.

## sin

```vyrn
fn sin(x: Float64) -> Float64
```

Sine of `x` radians.

Two steps, both plain `Float64` arithmetic. First reduce `x` to `r` in
[-pi/2, pi/2] using `sin(x) = sin(pi - x)` and a period of 2pi. Then evaluate
the odd Taylor polynomial through `x^13` in Horner form. On the reduced range
its error stays under 1e-13 — far below the 8-bit luminance the hero field
quantises to.

Every step is a single rounded operation, and RFC-0083 compiles with
`-ffp-contract=off`, so the interpreter, the native binary and wasm return the
same bits (`examples/herofield.vyrn` prints them and the parity harness
compares all three).

## cos

```vyrn
fn cos(x: Float64) -> Float64
```

Cosine of `x` radians, as the sine a quarter turn ahead.
