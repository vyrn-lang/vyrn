# std/num

std/num: text -> number, correctly rounded, and `Float64` -> `%f` text.

A library, not a builtin: the direct wasm backend has no `strtod`, so one
Vyrn implementation serves every engine. The only primitives are the
IEEE-754 bit views `floatBits` and `floatFromBits`.

The conversion is exact. The decimal stays a digit array, scaled by powers
of two into `[1/2, 1)`; repeated doubling yields the mantissa bits, and the
leftover fraction rounds half to even. A subnormal asks for fewer bits and
rounds the same way; overflow is an exponent field past its maximum. At
most 800 significant digits are kept; a dropped nonzero digit sets a sticky
flag that turns an exact tie into a round-up.

## parseFloat64

```vyrn
fn parseFloat64(s: String) -> Option<Float64>
```

Decimal text as a correctly rounded `Float64`, or `None` if the text is not
a number. The whole string must match. Too large is `inf` and too small is a
signed zero, as in every IEEE-754 text conversion.

## parseFloat32

```vyrn
fn parseFloat32(s: String) -> Option<Float32>
```

Decimal text as a `Float32`, rounded once: decimal -> `Float64` ->
`Float32` rounds twice and is wrong near a `Float32` tie.

## parseInt64

```vyrn
fn parseInt64(s: String) -> Option<Int64>
```

Decimal text as an exact `Int64`, or `None` if it is not an integer or does
not fit. `"9223372036854775808"` is `None`; the `parse` builtin wraps it.

## parseUInt64

```vyrn
fn parseUInt64(s: String) -> Option<UInt64>
```

Decimal text as an exact `UInt64`, or `None`. No sign is accepted, not
even `+`.

## f64Str

```vyrn
fn f64Str(x: Float64) -> String
```

A `Float64` as exactly `%f`'s six decimal places. `@str` and `print` on a
float lower to this on both compiled backends; the interpreter's `{:.6}` is
the oracle `tests/numbers.rs` compares it against.

The value is `M * 2^E` with `M < 2^53`, so `x * 10^6` is the exact integer
`M * 10^6 * 2^E` when `E >= 0`, and `M * 10^6 * 5^k / 10^k` with `k = -E`
otherwise, whose last `k` digits are the fraction to round away. Limbs are
base 10^6, so the `* 10^6` is a zero limb. Each pass multiplies by the
largest chunk of the power that fits an `Int64`, so the cost follows the
size of the answer, not the exponent.

Every answer is a fresh allocation, the non-finite words included: the
ownership analysis frees an `@str` result, so a `.rodata` literal must never
reach it.
