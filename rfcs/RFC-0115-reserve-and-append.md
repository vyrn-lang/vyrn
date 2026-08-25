# RFC-0115 — `reserve`, `append` and `copyFrom`

- **Status:** Implemented — `xs.reserve(n)`, `xs.append(ys)` and
  `dst.copyFrom(src)` on growable arrays, in all four execution paths
  (interpreter, native, textual wasm, direct wasm), with the write-back
  statement form `push` already has.
- **Evidence:** the benchmarks census work item — "a 60-byte line is one
  allocation rather than five doublings and sixty checked pushes" — and
  `std/html`'s `appendBytes`, which hand-wrote the per-byte loop.

## The surface

```vyrn
let mut out: Array<UInt8> = []
out.reserve(610)        // room for 610 more elements, one allocation
out.append(line)        // every element of `line`, in order, one copy
```

Both rebuild the way `push` does: the receiver is `read`, the result carries
the (possibly reallocated) buffer, and a statement-position call writes back
through the receiver place — `out.append(line)` is `out = @append(out, line)`
for any place `push` accepts.

`reserve(n)` makes room for `n` MORE elements past the current length, in one
`realloc`, and is a no-op when the capacity already suffices. Capacity is not
observable — the interpreter's `Rc<Vec>` passes the value through unchanged,
and parity compares outputs, not allocators.

`append(ys)` copies `ys`'s elements — `ys` is read, not consumed — growing at
most once, to `max(need, cap * 2)`. A self-append is defined and duplicates
the array: the lowering reads the source out of the grown buffer when the two
data pointers were one.

## The refusal

`append` is a byte copy of the source's elements in the compiled backends, so
an element type that owns heap is refused at the check:

> `append` copies its source's elements by bytes, and `String` owns heap —
> push each element with `.copy()` in a loop instead

Copying an owning element by bytes would give two arrays one buffer, which is
the double free RFC-0089's rules exist to make unrepresentable.
`examples/appendowned.vyrn` pins the refusal; `examples/appendreserve.vyrn`
is the corpus witness, self-append included. `SmallArray` and fixed arrays
are out: one's capacity is its type, the other has none to grow.

## `copyFrom`

`dst.copyFrom(src)` overwrites the receiver's elements with the source's,
reusing the buffer — one copy, growing only when the source is longer, and a
self-copy moves zero bytes. It is the third fact `push` cannot compose:
every store keeps the length, and `append` only grows. The same heapless
rule applies for the same reason, plus one of its own: an owning element
that is overwritten would never be released. `examples/copyfromowned.vyrn`
pins that refusal.

`fannkuch`'s per-permutation refill — a checked-store loop over the working
buffer — is one `copyFrom` now, which is the benchmarks census's
"copy into an array that already exists" item.

## What it replaced

`std/html`'s `appendBytes` — a `for`-loop of per-byte `push` — is one
`append` now. The remaining census items this serves (`fasta`'s and
`reverse-complement`'s line builders) can take the same two calls.
