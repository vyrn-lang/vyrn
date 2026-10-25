# std/tile

Integer matrix tiles: `mmaInt8` and `mmaUInt8`, the CPU oracle a GPU tile
must equal bit for bit.

Each adds a 16 by 32 A tile of 8-bit integers times a 32 by 16 B tile to a
16 by 16 `Int32` accumulator. Every tile is row-major, and the sums wrap as
`Int32` arithmetic does, never saturating, so integer addition modulo 2^32
leaves no order to differ in.

## mmaInt8

```vyrn
fn mmaInt8(a: Array<Int8, 512>, b: Array<Int8, 512>, acc: Array<Int32, 256>) -> Array<Int32, 256>
```

`acc` plus `a` times `b` for `Int8` tiles: A is 16 by 32, B 32 by 16 and
`acc` 16 by 16, row-major. The sums wrap.

## mmaUInt8

```vyrn
fn mmaUInt8(a: Array<UInt8, 512>, b: Array<UInt8, 512>, acc: Array<Int32, 256>) -> Array<Int32, 256>
```

`acc` plus `a` times `b` for `UInt8` tiles, shaped as for `mmaInt8`. The
sums wrap.
