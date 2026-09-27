# std/stream

std/stream: the `Stream<T>` combinators, in Vyrn.

Every combinator takes a `Stream` parameter, so it inherits the linearity
rule: an abandoned `map(...)` result fails to compile for the same reason an
abandoned `fromArray(...)` does. `map`, `filter` and `take` are lazy wrappers:
a wrapper's cursor holds its source, its step pulls one element at a time, and
its close releases the source, one release per stream down the chain. A cursor
is a slot in this module's `Slots<CursorCell>`, so a cursor that outlives its
stream traps as a dead handle. There is no merge-by-arrival and no `channel`:
both need to know which source is ready, and streams have no concurrency.

## Cursor

```vyrn
type Cursor = { slot: Int64, gen: Int64 }
```

A producer's cursor: a slot of this module's slab and the generation that was
live when the stream took it.

## cursorGet

```vyrn
fn cursorGet(c: Cursor) -> Int64
```

Traps if the stream that owns the cursor is closed.

## cursorSet

```vyrn
fn cursorSet(c: Cursor, v: Int64) -> Unit
```

## unfold

```vyrn
fn unfold<T>(seed: Int64, step: fn(Cursor) -> Option<T>) -> Stream<T>
```

A stream from a step function and a starting cursor.

`step` gets the stream's own cursor once per element: read it with
`cursorGet`, write the next one with `cursorSet`, and answer `Some(v)` to
yield or `None` to end. Nothing runs until a consumer asks, so a step that
never answers `None` is an endless feed. The cursor is an `Int64` because a
seed type in the step's signature would be a type parameter of `Stream<T>`;
other producer state goes in a second cursor the step closes over.

## map

```vyrn
fn map<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U>
```

Applies `f` to every element the consumer asks for. `s` is consumed; the
result is a new obligation.

The three wrappers repeat one shape by hand. A shared `wrap(s, step)` compiles
and agrees on every engine, but it adds one indirect call per element per
layer (`map` over `unfold` under `take`: +24%) and saves no lines.

## filter

```vyrn
fn filter<T>(s: Stream<T>, pred: fn(T) -> Bool) -> Stream<T>
```

Keeps only the elements for which `pred` holds.

A predicate that admits one in k asks its source about kn times to yield n;
over an endless source where nothing passes, it asks forever.

## take

```vyrn
fn take<T>(s: Stream<T>, n: Int64) -> Stream<T>
```

The first `n` elements (fewer if the stream is shorter; none if `n <= 0`).
Asks its source exactly n times: the count is checked before each pull.

## merge

```vyrn
fn merge<T>(a: Stream<T>, b: Stream<T>) -> Stream<T>
```

Interleaves two streams one element at a time, draining whichever outlasts
the other. Both inputs are consumed.

Eager, so an endless side hangs: a wrapper owns one source because a cursor
holds one box. Wrap an endless side in `take` first.
