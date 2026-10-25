# std/narrow

Narrow number formats for storage: `Float16`, `BFloat16`, the two FP8
formats, the OCP MX blocks, the NVFP4 block and packed Int4. A program
widens a value to `Float32`, computes, and narrows the result; the narrow
types have no arithmetic of their own.

Every conversion is integer code over the `Float32` bit view, so every
engine gives the same bits. `toFloat32` is exact. A narrowing rounds half to
even and gives infinity past the largest finite value where the format has
one, else NaN; its `Sat` form gives the largest finite value. A NaN narrows
to the format's NaN code with the sign cleared. `BFloat16` is `Float32`
rounded to its upper 16 bits, subnormals kept.

## Narrow

```vyrn
protocol Narrow { fn toFloat32(self) -> Float32 }
```

A value that widens to `Float32` exactly.

## Float16

```vyrn
type Float16 = { f16: UInt16 }
```

IEEE-754 binary16: a sign, 5 exponent bits with bias 15 and 10 mantissa
bits. Its largest finite value is 65504 and its NaN code 0x7E00.

## toFloat16

```vyrn
fn toFloat16(x: Float32) -> Float16
```

The `Float16` nearest `x`: infinity where `x` rounds past 65504.

## toFloat16Sat

```vyrn
fn toFloat16Sat(x: Float32) -> Float16
```

The `Float16` nearest `x`: 65504 where `x` rounds past it.

## BFloat16

```vyrn
type BFloat16 = { bf16: UInt16 }
```

The upper 16 bits of a `Float32`: a sign, 8 exponent bits with bias 127 and
7 mantissa bits. Its NaN code is 0x7FC0.

## toBFloat16

```vyrn
fn toBFloat16(x: Float32) -> BFloat16
```

The `BFloat16` nearest `x`: infinity where `x` rounds past the largest
finite value.

## toBFloat16Sat

```vyrn
fn toBFloat16Sat(x: Float32) -> BFloat16
```

The `BFloat16` nearest `x`: the largest finite value where `x` rounds past
it.

## Float8E4M3

```vyrn
type Float8E4M3 = { e4m3: UInt8 }
```

OCP FP8 E4M3: a sign, 4 exponent bits with bias 7 and 3 mantissa bits. It
has no infinity; its largest finite value is 448 and its NaN code 0x7F.

## toFloat8E4M3

```vyrn
fn toFloat8E4M3(x: Float32) -> Float8E4M3
```

The `Float8E4M3` nearest `x`: NaN where `x` rounds past 448, an infinity
included.

## toFloat8E4M3Sat

```vyrn
fn toFloat8E4M3Sat(x: Float32) -> Float8E4M3
```

The `Float8E4M3` nearest `x`: 448 where `x` rounds past it.

## Float8E5M2

```vyrn
type Float8E5M2 = { e5m2: UInt8 }
```

OCP FP8 E5M2: a sign, 5 exponent bits with bias 15 and 2 mantissa bits. Its
largest finite value is 57344 and its NaN code 0x7E.

## toFloat8E5M2

```vyrn
fn toFloat8E5M2(x: Float32) -> Float8E5M2
```

The `Float8E5M2` nearest `x`: infinity where `x` rounds past 57344.

## toFloat8E5M2Sat

```vyrn
fn toFloat8E5M2Sat(x: Float32) -> Float8E5M2
```

The `Float8E5M2` nearest `x`: 57344 where `x` rounds past it.

## Block

```vyrn
protocol Block { fn valueAt(self, Int64) -> Float32 }
```

A block of elements that share one scale.

### `fn valueAt(self, Int64) -> Float32`

The value of element `i`, or NaN in a block whose scale is NaN.

## MxFp8E4M3Block

```vyrn
type MxFp8E4M3Block = { e8m0: UInt8, e4m3: Array<UInt8, 32> }
```

MXFP8 with E4M3 elements.

## toMxFp8E4M3

```vyrn
fn toMxFp8E4M3(xs: Array<Float32, 32>) -> MxFp8E4M3Block
```

The MXFP8 E4M3 block of `xs`.

## MxFp8E5M2Block

```vyrn
type MxFp8E5M2Block = { e8m0: UInt8, e5m2: Array<UInt8, 32> }
```

MXFP8 with E5M2 elements.

## toMxFp8E5M2

```vyrn
fn toMxFp8E5M2(xs: Array<Float32, 32>) -> MxFp8E5M2Block
```

The MXFP8 E5M2 block of `xs`.

## MxFp6E2M3Block

```vyrn
type MxFp6E2M3Block = { e8m0: UInt8, e2m3: Array<UInt8, 24> }
```

MXFP6 with E2M3 elements, which have no infinity or NaN and reach 7.5.

## toMxFp6E2M3

```vyrn
fn toMxFp6E2M3(xs: Array<Float32, 32>) -> MxFp6E2M3Block
```

The MXFP6 E2M3 block of `xs`.

## MxFp6E3M2Block

```vyrn
type MxFp6E3M2Block = { e8m0: UInt8, e3m2: Array<UInt8, 24> }
```

MXFP6 with E3M2 elements, which have no infinity or NaN and reach 28.

## toMxFp6E3M2

```vyrn
fn toMxFp6E3M2(xs: Array<Float32, 32>) -> MxFp6E3M2Block
```

The MXFP6 E3M2 block of `xs`.

## MxFp4Block

```vyrn
type MxFp4Block = { e8m0: UInt8, e2m1: Array<UInt8, 16> }
```

MXFP4: E2M1 elements, which have no infinity or NaN and reach 6, two to a
byte with element `2i` in the low nibble of byte `i`.

## toMxFp4

```vyrn
fn toMxFp4(xs: Array<Float32, 32>) -> MxFp4Block
```

The MXFP4 block of `xs`.

## MxInt8Block

```vyrn
type MxInt8Block = { e8m0: UInt8, int8: Array<Int8, 32> }
```

MXINT8: two's complement elements with an implicit scale of 2^-6. A
narrowing keeps them within -127 to 127.

## toMxInt8

```vyrn
fn toMxInt8(xs: Array<Float32, 32>) -> MxInt8Block
```

The MXINT8 block of `xs`.

## NvFp4Block

```vyrn
type NvFp4Block = { ue4m3: UInt8, e2m1: Array<UInt8, 8> }
```

NVFP4: 16 E2M1 elements, packed as in `MxFp4Block`, and a UE4M3 scale, an
E4M3 code without a sign. A scale code of 127 is NaN; bit 7 is ignored.

## toNvFp4

```vyrn
fn toNvFp4(xs: Array<Float32, 16>) -> NvFp4Block
```

The NVFP4 block of `xs`. The scale is the largest magnitude divided by 6,
rounded to UE4M3 and saturated; each element is its value divided by the
scale, rounded to E2M1 and saturated. An infinity counts as the largest
finite `Float32`, and a NaN sets the scale to 127 and every element to 0.

## packInt4

```vyrn
fn packInt4(xs: Array<Int8>) -> Array<UInt8>
```

`xs` as Int4 values, two to a byte with element `2i` in the low nibble of
byte `i`. Each value wraps to its low 4 bits, and an odd length leaves the
last high nibble 0.

## int4At

```vyrn
fn int4At(packed: Array<UInt8>, i: Int64) -> Int8
```

Element `i` of Int4 values that `packInt4` packed, from -8 to 7.
