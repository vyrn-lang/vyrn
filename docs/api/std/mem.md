# std/mem

std/mem — the raw-memory primitives the runtime module stands on
(RFC-0125 §2.4, PLAN-0125-runtime §2.1 and §2.3).

Every function here is a declaration. The emitter never reads a body: it
maps each call to one wasm instruction (`load8` is `i32.load8_u`, `copy` is
`memory.copy`, `grow` is `memory.grow`). The bodies below exist so the
module parses and checks like any other; an engine that reaches one — the
interpreter, which has no linear memory — stops with the message.

The audience of this module is `std/runtime` and nothing else. The loader
declares that audience itself (PLAN-0125-runtime §3.2), so no `vyrn.json`
can widen it: a user import of `std/mem` is refused with a diagnostic, and
the safe surface over these instructions is what `std/runtime` exports.

An address is an `Int32` because the route is wasm32 (RFC-0125 §2.8). The
runtime module is the only reader of addresses, so there is no `Ptr<T>`.

## load8

```vyrn
fn load8(addr: Int32) -> UInt8
```

`i32.load8_u`: the byte at `addr`.

## load16

```vyrn
fn load16(addr: Int32) -> UInt16
```

`i32.load16_u`.

## load32

```vyrn
fn load32(addr: Int32) -> UInt32
```

`i32.load`.

## load64

```vyrn
fn load64(addr: Int32) -> UInt64
```

`i64.load`.

## loadF32

```vyrn
fn loadF32(addr: Int32) -> Float32
```

`f32.load`.

## loadF64

```vyrn
fn loadF64(addr: Int32) -> Float64
```

`f64.load`.

## store8

```vyrn
fn store8(addr: Int32, v: UInt8) -> Unit
```

`i32.store8`.

## store16

```vyrn
fn store16(addr: Int32, v: UInt16) -> Unit
```

`i32.store16`.

## store32

```vyrn
fn store32(addr: Int32, v: UInt32) -> Unit
```

`i32.store`.

## store64

```vyrn
fn store64(addr: Int32, v: UInt64) -> Unit
```

`i64.store`.

## storeF32

```vyrn
fn storeF32(addr: Int32, v: Float32) -> Unit
```

`f32.store`.

## storeF64

```vyrn
fn storeF64(addr: Int32, v: Float64) -> Unit
```

`f64.store`.

## copy

```vyrn
fn copy(dst: Int32, src: Int32, n: Int32) -> Unit
```

`memory.copy`: `n` bytes from `src` to `dst`. The ranges may overlap.

## fill

```vyrn
fn fill(dst: Int32, byte: UInt8, n: Int32) -> Unit
```

`memory.fill`: `n` bytes of `byte` at `dst`.

## memorySize

```vyrn
fn memorySize() -> Int32
```

`memory.size`: the memory's size in 64 KiB pages.

## grow

```vyrn
fn grow(delta: Int32) -> Int32
```

`memory.grow`: add `delta` pages. The old size in pages, or -1 when the
engine refuses.

## heapBase

```vyrn
fn heapBase() -> Int32
```

The `HEAP_BASE` global: where the data segment ends and the heap begins.

## trap

```vyrn
fn trap(msg: Int32, len: Int32) -> Unit
```

The trap primitive (PLAN-0125-runtime §2.3): `len` bytes at `msg` to
descriptor 2, then `proc_exit(1)`. It does not return.
