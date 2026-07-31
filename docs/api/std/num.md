# std/num

std/num — text -> number, written in Vyrn (RFC-0078 M4a).

The language could already turn a number into text on every engine. It could
not turn text into a number at all beyond `parse`'s `String -> Option<Int64>`:
`Float64("1.5")` is a check error, and there is no expression that builds a
`Float64` from anything but another number. That is the wall RFC-0078 M3 hit,
and this module is the answer to it.

**Why a library and not a builtin.** A `parseFloat` builtin would need three
implementations, and the third one is the problem: the direct wasm backend has
no `strtod` to call, so it would have to grow a correctly-rounded
decimal-to-binary conversion in hand-emitted wasm, next to the ~300 lines
RFC-0077 M2h already wrote for the other direction. Written here it is written
once and every engine gets it, which is RFC-0078's whole thesis. The price is
two genuinely irreducible primitives — `floatBits` and `floatFromBits`, the
IEEE-754 bit views, one instruction each — and this file.

**Correctly rounded, and how.** The conversion is exact, not floating point:
the decimal is kept as a digit array and scaled by powers of two until it lies
in `[1/2, 1)`, at which point the mantissa bits fall out of repeated doubling
and the leftover fraction decides the rounding — greater than a half rounds up,
less rounds down, exactly a half goes to even. Nothing here computes with a
`Float64` except the very last step, which assembles one from its bits. That is
the same shape RFC-0077 M2h used to print `%f`'s six places exactly, run
backwards.

Subnormals and overflow are not special cases bolted on: the number of
mantissa bits to extract is `min(mantissa width, exponent - the smallest)`, so
a subnormal simply asks for fewer bits and rounds the same way, and a value too
large for the format falls out as an exponent field past its maximum.

**The digit cap.** At most 800 significant digits are kept; anything dropped
sets a sticky flag which turns an exact tie into a round-up, because a
truncated tail means the true value is strictly greater than what was kept.
Beyond 800 digits no further digit can change a rounding decision except at a
tie, and the flag covers that.

## parseFloat64

```vyrn
fn parseFloat64(s: String) -> Option<Float64>
```

Decimal text as a `Float64`, correctly rounded — `None` when the text is not a
number. Accepts an optional sign, digits with an optional fraction and an
optional `e` exponent; the whole string must be consumed.

A value too large for the format is `inf` rather than a refusal, and one too
small is zero with its sign, which is what every other IEEE-754 text
conversion does.

## parseFloat32

```vyrn
fn parseFloat32(s: String) -> Option<Float32>
```

The same, rounded to single precision directly rather than through a
`Float64` — decimal to `Float64` to `Float32` rounds twice and is wrong for
values that sit near a `Float32` tie.

## parseInt64

```vyrn
fn parseInt64(s: String) -> Option<Int64>
```

Decimal text as an exact `Int64` — `None` when the text is not an integer or
does not fit. Unlike the `parse` builtin, which wraps on overflow, this
refuses: `"9223372036854775808"` is `None` rather than `Int64.min`.

## parseUInt64

```vyrn
fn parseUInt64(s: String) -> Option<UInt64>
```

Decimal text as an exact `UInt64` — the case `parse` cannot serve at all,
since its `Option<Int64>` has no room for a value above `Int64.max`. No sign
is accepted, not even `+`.
