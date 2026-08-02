# std/stream

std/stream — the `Stream<T>` combinators (RFC-0075 M2), written in Vyrn
itself, exactly as std/arrays is. Being ordinary Vyrn they get
interpreter == native == wasm parity for free, they add no builtin and no
census row, and — the part that matters here — they inherit M1's linearity
instead of restating it. Every function below takes a `Stream` parameter,
which carries the obligation into the callee, discharges it with `for … in`,
and hands back a stream the caller now owes. An abandoned `map(...)` result
therefore fails to compile for precisely the reason an abandoned
`fromArray(...)` does; there is no combinator-shaped hole to plug because
there is no combinator-shaped rule.

**Five, and two refused.** M2b made `Stream<T>` a producer rather than a
buffer, so `unfold` is here now: it is the reason the representation
changed. `merge`-by-arrival and `channel` are still absent, and not for want
of a representation — both need to know which of several sources has a value
READY, and this RFC states outright that it adds no concurrency while
RFC-0013 leaves the loop to the host. There is nothing to ask.

## unfold

```vyrn
fn unfold<T>(seed: Int64, step: fn(Ref<Int64>) -> Option<T>) -> Stream<T>
```

A stream from a step function and a starting cursor (RFC-0075 M2b).

`step` is handed the stream's own cursor cell once per element: read it with
`get`, write the next cursor with `set`, and answer `Some(v)` to yield or
`None` to end. Nothing runs until a consumer asks, so a step that never
answers `None` is an endless feed rather than a hang — `take(unfold(..), n)`
over one allocates n.

The cursor is the resume token RFC-0074's `.resumable()` wants: hand a
reconnecting client's `Last-Event-ID` in as `seed` and replay is the ordinary
path. It is an `Int64` because the step's SIGNATURE is what a stream
dispatches through, and a seed type in that signature would be a seed type
in `Stream<T>` — which is exactly what M2 could not hide. Producer state that
is not one integer goes in a second cell the step closes over.

## map

```vyrn
fn map<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U>
```

Apply `f` to every element. `s` is consumed; the result is a new obligation.

## filter

```vyrn
fn filter<T>(s: Stream<T>, pred: fn(T) -> Bool) -> Stream<T>
```

Keep only the elements for which `pred` holds.

## take

```vyrn
fn take<T>(s: Stream<T>, n: Int64) -> Stream<T>
```

The first `n` elements (fewer if the stream is shorter; none if `n <= 0`).

The `break` is the point: it leaves the loop early, and M1's release still
runs on that path — one release, never two. Over M1's eager representation
the early exit bought nothing but the pin, since the buffer was already
built. Over M2b's it is the whole reason `take` exists: `take(unfold(..), n)`
asks the producer n + 1 times and stops, which is what makes an endless feed
a program rather than a hang.

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
any stream that terminates. What is lost is merging an endless source with a
finite one — and since M2b an endless source EXISTS, so `merge(unfold(..), b)`
is now a hang rather than a thing the language cannot spell. Wrap the endless
side in `take` first; a merge that stops on its own needs both sides to.
