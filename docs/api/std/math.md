# std/math

std/math: integer helpers (min, max, abs, clamp) and float ones (pi, floor,
sine, cosine), written in Vyrn.

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

Absolute value. `abs(Int64.min)` saturates to `Int64.max`: the language
wraps, so `0 - x` would return the minimum itself.

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

## sin

```vyrn
fn sin(x: Float64) -> Float64
```

Sine of `x` radians.

Reduces `x` to [-pi/2, pi/2] with `sin(x) = sin(pi - x)` and a period of 2pi,
then evaluates the odd Taylor polynomial through `x^13` in Horner form; the
error stays under 1e-13. Every step is one rounded operation and native code
compiles with `-ffp-contract=off`, so every engine returns the
same bits.

## cos

```vyrn
fn cos(x: Float64) -> Float64
```

Cosine of `x` radians, as the sine a quarter turn ahead.
