# std/stream

std/stream — the `Stream<T>` combinators (RFC-0075 M2), written in Vyrn
itself, exactly as std/arrays is. Being ordinary Vyrn they get
interpreter == native == wasm parity for free, and — the part that matters
here — they inherit M1's linearity instead of restating it. Every function
below takes a `Stream` parameter, which carries the obligation into the
callee, discharges it, and hands back a stream the caller now owes. An
abandoned `map(...)` result therefore fails to compile for precisely the
reason an abandoned `fromArray(...)` does; there is no combinator-shaped hole
to plug because there is no combinator-shaped rule.

**Five, and two refused.** M2b made `Stream<T>` a producer rather than a
buffer, so `unfold` is here now: it is the reason the representation
changed. `merge`-by-arrival and `channel` are still absent, and not for want
of a representation — both need to know which of several sources has a value
READY, and this RFC states outright that it adds no concurrency while
RFC-0013 leaves the loop to the host. There is nothing to ask.

**Lazy since M2c.** `map`, `filter` and `take` do not drain their source into
a buffer. Each is a wrapper: a producer whose cursor holds the stream it
wraps, whose step reads that source one element at a time (`pullAt`), and whose
close releases the source on the way past — one release per stream, down the
chain. So `map(unfold(..), f)` is a feed rather than a hang, which is the
first thing anyone does with a feed.

**The slab is this module's, since RFC-0090 M3.** A cursor used to be a
`Ref<Int64>` out of a slab the compiler carried, with a fourth parallel array
holding "the stream behind this cursor". Both are gone. A cursor is a slot in
the `Slots<CursorCell>` below, so the slab logic is Vyrn anyone can read, it
grows instead of stopping at 65 536, and a cursor that outlived its stream
traps as a dead handle rather than as a released cell.

## Cursor

```vyrn
type Cursor = { slot: Int64, gen: Int64 }
```

A producer's cursor: which slot of this module's slab, and the generation
that was live when the stream took it. Two plain words, so a stream carries
one exactly where it used to carry a `Ref`.

## cursorGet

```vyrn
fn cursorGet(c: Cursor) -> Int64
```

Read the cursor. Traps if the stream that owns it has been closed.

## cursorSet

```vyrn
fn cursorSet(c: Cursor, v: Int64) -> Unit
```

Write the cursor.

## unfold

```vyrn
fn unfold<T>(seed: Int64, step: fn(Cursor) -> Option<T>) -> Stream<T>
```

A stream from a step function and a starting cursor (RFC-0075 M2b).

`step` is handed the stream's own cursor once per element: read it with
`cursorGet`, write the next cursor with `cursorSet`, and answer `Some(v)` to
yield or `None` to end. Nothing runs until a consumer asks, so a step that
never answers `None` is an endless feed rather than a hang —
`take(unfold(..), n)` over one allocates n.

The cursor is the resume token RFC-0074's `.resumable()` wants: hand a
reconnecting client's `Last-Event-ID` in as `seed` and replay is the ordinary
path. It is an `Int64` because the step's SIGNATURE is what a stream
dispatches through, and a seed type in that signature would be a seed type in
`Stream<T>` — which is exactly what M2 could not hide. Producer state that is
not one integer goes in a second cursor the step closes over.

## map

```vyrn
fn map<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U>
```

Apply `f` to every element. `s` is consumed; the result is a new obligation.

**Lazy** (M2c): `f` runs once per element the consumer actually asks for, so
`map(unfold(..), f)` is a feed rather than a hang and `take(map(feed, f), n)`
asks the feed n times. Nothing is buffered on the way through — a mapped
buffer stream allocates less than the eager version did, not more.

The three wrappers below share a shape and not a function. Each puts its
source in a box, keeps the address in its own cursor slot, reads the source
with `pullAt`, and on the closing call gives the slot back, takes the source out
of the box and CLOSES it — an ordinary `close` in ordinary Vyrn, which is why
`movecheck` checks the chain's releases the way it checks every other one.

**They repeat because sharing costs, and NOT because it fails to compile.**
M2c wrote three copies for the second reason. One `wrap(s, step)` takes a
`fn` parameter that carries captures, its own step is a lambda that captures
it, and the LLVM backend lost the inner captures at that second level:
`take`'s `n` read 0 on native and correct on the interpreter. RFC-0087
Phase 10b rebuilt that machinery — a `fn` value owns its capture block and
`@__vyrn_fnval_copy` is derived over RFC-0037's defunctionalized enum — and a
follow-through built the shared `wrap` to find out whether the divergence
went with it.

It did. The shared form compiled and the three engines agreed byte for byte
over `take(unfold(..), n)`, `take(feed(..), 0)`, `take(map(unfold(..), f), n)`,
`take(filter(unfold(..), p), n)`, `take(filter(map(unfold(..), f), p), 2)` in
a loop of 30 open-and-close cycles, `take(map(filter(unfold(..), p), f), 3)`,
and a three-layer chain drained whole. `take`'s counter was read, not merely
non-zero. **The divergence is dead; do not cite it here again.**

The shared form was dropped on its measurements, which are the reason to keep
reading three copies. It adds one indirect call per element per wrapper
layer: `map` over `unfold` under `take` went 4.54 µs -> 5.64 µs (+24%). That
row was 4.02 µs before Phase 8c, 9.20 µs after it, and 4.54 µs once Phase 8d
won it back — a whole phase of work to give a quarter of away. `take` over
`unfold` alone went
3.27 µs -> 2.91 µs, and open-and-close did not move. It also saved no lines —
each caller had to bind its step to a NAMED, typed local, because a lambda
literal carries no return annotation and `wrap`'s result parameter has
nothing else to solve from. Worse on speed, no shorter, and one more rule to
learn. See RFC-0075 "As landed — M3".

## filter

```vyrn
fn filter<T>(s: Stream<T>, pred: fn(T) -> Bool) -> Stream<T>
```

Keep only the elements for which `pred` holds.

**Lazy** (M2c), and the harder half of it: one element out is any number of
elements in, so the loop below is the shape "one `next` in, one `next` out"
does not have. A predicate admitting one in k therefore asks its source
about kn times to yield n, and over a source with no end it asks forever if
nothing ever passes — which is the honest behaviour of a filter, not a bug
laziness introduced.

## take

```vyrn
fn take<T>(s: Stream<T>, n: Int64) -> Stream<T>
```

The first `n` elements (fewer if the stream is shorter; none if `n <= 0`).

**Lazy** (M2c), and the count moved because of it: this asks its source
exactly n times, not the n + 1 M2b measured. The extra one was the element
the eager version read before its `break` fired — a wrapper never reads it,
because the counter is checked before the source is asked at all. `take` is
no longer the one combinator that escapes an endless feed; it is the one
that ENDS one, which is a different job and the reason it still exists.

The count lives in the wrapper's own cursor — `cursorGet`/`cursorSet` here
are the ordinary cursor operations, on the slot this call took.

## merge

```vyrn
fn merge<T>(a: Stream<T>, b: Stream<T>) -> Stream<T>
```

Interleave two streams one element at a time, draining whichever outlasts
the other. Both inputs are consumed.

This is merge on *sequences*, which is the only merge this RFC can mean —
it states outright that it does not add concurrency, so "whichever source
has a value ready" is not a question the language can ask. Turn-taking is
what a pull-based merge would do anyway when both sides are ready, so the
observable sequence is the same one a lazy implementation would produce for
any stream that terminates.

**Still eager, and therefore still a hang on an endless side** — the one
combinator M2c did not make lazy, for a stated reason rather than an
oversight: a wrapper owns ONE source, because a cursor holds one box. Two
sources want a second address in that cell and a step that remembers whose
turn it is, which is a second wrapper shape rather than a second use of this
one. Wrap the endless side in `take` first; a merge that stops on its own
needs both sides to.
