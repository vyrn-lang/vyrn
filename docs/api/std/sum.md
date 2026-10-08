# std/sum

std/sum: the exact sum of an `Array<Float64>`, written in Vyrn.

## sum

```vyrn
fn sum(xs: Array<Float64>) -> Float64
```

The true sum of `xs`, rounded once to the nearest `Float64` with ties to
even.

The result does not depend on the order of `xs`, on cancellation, or on an
intermediate sum that would overflow: the largest finite value twice and
its negation sum to the largest finite value. An infinity or a NaN follows
IEEE: `+inf + -inf` is NaN, and a NaN anywhere makes the sum NaN. An exact
zero is `+0.0`, and an empty array sums to `0.0`. A loop that adds with `+`
keeps sequential rounding.

An array of 512 values or more goes into slots by sign and exponent, with
no rounding. Four tables of 4096 slots take every fourth value in turn, so
a run of equal exponents does not wait on one slot. A slot moves into a
2304-bit accumulator before it can overflow. Shorter arrays go straight to
the accumulator.
