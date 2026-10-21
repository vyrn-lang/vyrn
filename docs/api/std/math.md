# std/math

Integer helpers (min, max, abs, clamp) and `Float64` functions (pi,
rounding, sine, cosine, exp, log, pow, tan, atan2), written in Vyrn. The
rounding functions are exact, and exp, log, pow, tan and atan2 state
their error in ulps against the exact result.

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

## trunc

```vyrn
fn trunc(x: Float64) -> Float64
```

`x` rounded toward zero to a whole number. Exact. A zero keeps its sign,
and an infinity or a NaN is returned as it is.

## floor

```vyrn
fn floor(x: Float64) -> Float64
```

The greatest whole number at or below `x`. Exact, with `trunc`'s zeros,
infinities and NaN.

## ceil

```vyrn
fn ceil(x: Float64) -> Float64
```

The least whole number at or above `x`. Exact; `ceil(-0.5)` is `-0.0`.

## round

```vyrn
fn round(x: Float64) -> Float64
```

`x` rounded to the nearest whole number, a half away from zero:
`round(2.5)` is `3.0` and `round(-0.5)` is `-1.0`. Exact.

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

## exp

```vyrn
fn exp(x: Float64) -> Float64
```

e raised to `x`. Within 1 ulp. NaN gives NaN, and `x` above 709.78
gives infinity; below -745.13, zero.

## log

```vyrn
fn log(x: Float64) -> Float64
```

The natural logarithm of `x`. Within 1 ulp. Zero gives -infinity, a
negative `x` or NaN gives NaN, and infinity gives infinity.

## pow

```vyrn
fn pow(x: Float64, y: Float64) -> Float64
```

`x` raised to `y`. Within 1 ulp, and exact where the result is a whole
number a `Float64` holds. `pow(x, 0.0)` and `pow(1.0, y)` are 1 even for
NaN; a negative `x` to a fractional `y` is NaN; otherwise the C99 rules
for zeros and infinities hold.

## tan

```vyrn
fn tan(x: Float64) -> Float64
```

Tangent of `x` radians. Within 1 ulp at every finite `x`; an infinity or
NaN gives NaN.

## atan2

```vyrn
fn atan2(y: Float64, x: Float64) -> Float64
```

The angle of the point (`x`, `y`) from the positive x axis, in radians
within [-pi, pi]. Within 2 ulps. The signs of zeros and infinities pick
the quadrant as C99 states: `atan2(0.0, -1.0)` is pi and
`atan2(-0.0, -1.0)` is -pi. NaN in either gives NaN.
