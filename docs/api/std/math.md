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

## clamp

```vyrn
fn clamp(x: Int64, lo: Int64, hi: Int64) -> Int64
```

Clamp `x` into the inclusive range [lo, hi].
