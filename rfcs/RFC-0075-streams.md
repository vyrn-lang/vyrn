# RFC-0075 — `Stream<T>`: Cleanup as an Obligation, Not a Convention

- **Status:** **M1, M2, M2b and M2c shipped** — `Stream<T>` is a linear
  resource, an abandoned stream does not compile
  (`examples/stream_abandoned.vyrn`), release runs on normal end, `break` and
  early `return`, and since M2b a stream is a PRODUCER rather than a buffer.
  Since M2c the *combinators* are too: `take(map(unfold(..), f), n)` over a feed
  with no end asks the source exactly n times
  (`examples/streamunfold.vyrn`, `examples/streamlazy.vyrn`). `std/stream` ships
  `unfold`, `map`, `filter`, `take` and `merge`; `merge` is the one that stays
  eager, for a structural reason — a cursor cell has one stream behind it — and
  `channel` and arrival-order `merge` stay **refused**, not for want of a
  representation. See "As landed — M2" and "As landed — M2c". **M3's conformance table is four-sixths true already**: one row
  is struck (it asks for behaviour during an unwind Vyrn does not have), one
  needed nothing (a fallible producer yields `Stream<Result<T, E>>`, so the
  error is an element), and the last — client disconnect — **is closed by
  RFC-0074 M3a**, whose `sse` learns the client is gone by writing to it and
  failing. **M4 is given up to RFC-0074 M3**, which owns `Route` and spells the
  adapters as projections; what stays here is the contract they must meet.
- **Depends on:** RFC-0074 (`sse` / `ws` projections — the transports that
  consume streams), RFC-0072 (audience, derived RPC), RFC-0037 (stored closures
  / defunctionalization — the producer state below), RFC-0060 (`break` /
  `continue` with divergence-aware movecheck — the drop machinery this reuses)
- **Evidence (external):** the streaming failure modes verified in the DX survey
  that opened this arc. They are quoted in full below because they are the whole
  justification for the design.

---

## The evidence

Two maintainer-confirmed incidents in tRPC, both in code that typechecked and
followed the documented idiom.

**Cleanup that never ran.** `trpc#6193`: the documented pattern
`while (!opts.signal!.aborted)` kept looping with `aborted === false` after the
client tab closed — *"next.js reporting that the SSE request finished, but trpc
keeps running this loop"*. A second reporter: *"our `return` method on the
AsyncIterable is not being called after connection closes. This is causing a
memory leak and our servers to OOM every couple of hours."* The maintainer
placed fault in the framework, not user code.

**Retention inside the adapter.** `trpc#6156`: v11 heap grew from ~195 MB to
646 MB and stayed elevated while a WebSocket was open, against v10 stable at
~217 MB, on a generator yielding a 100 KB string every 100 ms. Fixed inside the
framework by nulling references and resolving a dangling `Promise.race` —
nothing application code could see or prevent.

**Disconnect detection that varies by host.** Which event fires when a client
vanishes differed per deployment: *"local dev triggers `res.close`, fly triggers
`req.close`"*; StackBlitz *"triggers none"*; CodeSandbox emits
`req.socket 'end'`, then `res 'close'`, then `req.socket 'close'`; AWS and
Netlify supply no `socket` at all. The fix history oscillated for eight months
across five PRs and ended with `#6842` **reverting** the proxy fix and deleting
the regression test labelled for `#6193`, trading proxy correctness for a
different leak.

The pattern: correct cleanup could not be written once and relied upon, and no
amount of typechecking surfaced any of it. This is the failure class Vyrn's
ownership model already solves for memory, applied to a resource it does not yet
cover.

## The model

`Stream<T>` is a **linear resource**. It is produced by a procedure, consumed
exactly once, and its disposal is checked by the same movecheck that already
governs owned values and early exits.

```vyrn
export fn tail(req: TailReq) -> Stream<Paste>
```

A stream that is neither consumed to completion nor explicitly closed does not
compile:

```
error: `events` is a `Stream<Paste>` and is never disposed
  --> server/api/feed.vyrn:14:9
   |
14 |     let events = tail(req)
   |         ^^^^^^ acquired here, not consumed and not closed
   = a stream must be consumed with `for … in`, forwarded by returning it,
     or released with `close(events)`
```

The obligation is discharged by consuming it (`for … in` runs the producer's
release path on normal end, `break`, and early `return` alike — RFC-0060 already
made drop correct across divergent exits), by returning it (ownership moves to
the caller), or by `close()`.

This makes the `#6193` program non-buildable rather than non-terminating.

## Producing a stream

This is the one genuinely open design question in the set, and it should be
settled with an implementation spike rather than asserted.

**Option A — combinators only (no new syntax).** `std/stream` provides
`unfold`, `fromArray`, `map`, `filter`, `take`, `merge`, and `channel`. A
producer is a step function over explicit state:

```vyrn
export fn tail(req: TailReq) -> Stream<Paste> {
    return unfold(req.since, |cursor| match store.after(cursor) {
        Some(p) => Next(p, p.created),
        None    => Done,
    })
}
```

*For:* zero frontend work; rides RFC-0037's defunctionalized closures, so it
carries no function pointers and no new IR. *Against:* state that is trivial in
a coroutine becomes an explicit accumulator, and multi-stage producers read
poorly.

**Option B — `stream fn` with `yield`.** A coroutine form the compiler lowers
into Option A's state machine:

```vyrn
export stream fn tail(req: TailReq) -> Paste {
    let mut cursor = req.since
    while true {
        for p in store.after(cursor) {
            cursor = p.created
            yield p
        }
        sleep(1000)
    }
}
```

*For:* by far the better authoring experience, and it matches what every
comparable system converged on. *Against:* a real frontend feature — resumable
stack frames, movecheck across suspension points, and three backends
(interpreter, native, wasm) that must agree byte-for-byte on it. That is a
large, risky change to the invariant the whole project rests on.

**Recommendation:** ship Option A first and prove the *semantics* — linearity,
cancellation, conformance — against real SSE and WS traffic. Take Option B only
if authoring pain in the migrated examples justifies it, as a separate RFC, with
Option A remaining the lowering target so the runtime contract never changes.
This ordering keeps the risky piece optional and keeps parity provable at every
step.

## Consuming a stream

```vyrn
for p in tail(TailReq { since: 0 }) {
    render(p)
    if p.lang == "stop" { break }        // release runs; no leak, no ceremony
}
```

`break`, early `return`, and a trap all run the producer's release path. There
is no `finally` to write and therefore none to forget — the pathology behind
both tRPC incidents.

Over the wire, a client stream is the same shape:

```vyrn
for p in api.events.tail(req) { … }
```

which is the property worth borrowing from oRPC: a streaming call is an ordinary
call whose result is iterated. No separate subscription API, no observable type,
no second mental model.

## Cancellation, normalized

The failure in `#6204`/`#6343`/`#6842` was that *disconnect detection was left
to the host*. Here it is owned by one abstraction with a mandatory conformance
suite.

`Stream<T>` producers observe cancellation through a single normalized signal.
Every host adapter — the native server, the wasm/WASI server, SSE, WebSocket,
and any third-party adapter — must pass a shared suite:

| test | requirement | state after M2b |
|---|---|---|
| client disconnects mid-stream | producer release runs within 100 ms | **holds, and stronger** — RFC-0074 M3a |
| consumer `break`s | release runs before the loop's next statement | **holds** — M1's pin, re-counted in M2b |
| consumer traps | release runs during unwind | **struck; see below** |
| producer raises | release runs; the error surfaces to the consumer | **holds, and needed nothing** |
| stream is dropped unconsumed | compile error, not a runtime condition | **holds** — `examples/stream_abandoned.vyrn` |
| 10 000 open-then-abandon cycles | steady-state memory within 5% of baseline | **holds, and stronger** — M2b |

The last row is `#6156` as a regression test. The suite is a public part of
`std/stream`, so a third-party adapter proves itself with the same file the
built-in adapters run.

**The disconnect row is closed, in the stronger wording, and there was never a
100 ms in it.** RFC-0074 M3a's `sse` learns the client is gone by writing to it
and failing, so the release is the statement after the failed write rather than a
deadline — "release runs before the next event would be produced". The pin
(`tests/serve.rs`) drops a socket mid-feed and asserts two things, because
"production stopped" and "the release ran" are different claims: the producer's
step count does not move again, and a `Ref` into the stream's own cursor cell
traps with `reference used after release`, which only a `close` can cause. A third
test opens and abandons 200 streams over the wire — this table's last row at
transport scale. All three run below `std/http`, against `serveStream` and
`fromStep` directly, so `ws` (RFC-0074 M3b) passes the same file rather than a
version of it written for the second adapter.

**"Consumer traps" is struck rather than deferred.** Vyrn has no unwinding —
every trap on every engine is `fputs(stderr); exit(1); unreachable`, checked in
the emitted IR of all three. A row demanding behaviour during an unwind that
does not exist is not a test an adapter can pass or fail, so it is not a
conformance row; it is a request for a different language. What replaces it is
the true statement: a trap ends the process, and the process ending closes the
sockets and file descriptors a producer was holding. The resource class that
survives a process exit — a row in someone else's database, a lock in a
different service — is not reachable by any release path this RFC could
specify, so it belongs to the program rather than to the stream.

**"Producer raises" needs no error channel, and adding one would be the
mistake.** M2b's step is `fn(Ref<Int64>) -> Option<T>` with no way to report
failure, which reads at first like a gap in the signature. It is not: a producer
that can fail yields `Stream<Result<T, E>>`, so the error **is an element**. The
consumer matches it in the ordinary `for … in`, the release path is the same one
every other element takes, and "the error surfaces to the consumer" is satisfied
by the type rather than by a mechanism. Widening the step to
`Result<Option<T>, E>` would add a second failure channel that says the same
thing, force every dispatcher and both backends through a wider return, and give
`map`/`filter`/`take` two error stories to compose instead of one.

## Resumability

`sse(...).resumable()` (RFC-0074) requires the producer to accept a cursor,
which Option A makes explicit: the `unfold` seed *is* the resume token. On
reconnect the adapter passes the client's `Last-Event-ID` as that seed, so
replay is the ordinary code path rather than a special one.

oRPC's documented caveat is worth avoiding: its completion signal is
non-standard, so plain `EventSource` clients do not recognize handler-return as
completion and auto-reconnect forever. This RFC's SSE adapter terminates with a
standards-recognized close, and the conformance suite includes a raw
`EventSource` client asserting that it does not reconnect after normal
completion.

## Backpressure

A slow consumer must not let an eager producer accumulate unboundedly — the
mechanism behind `#6156`'s heap growth. `Stream<T>` is **pull-based**: the
producer's step function runs only when the consumer asks. `channel()` (the
push-shaped constructor, for producers driven by external events) takes a
mandatory bounded capacity and an explicit overflow policy — `Block`,
`DropOldest`, or `Fail` — with no default, so the decision is made rather than
inherited.

## What this does not do

- It does not add concurrency. A stream is a sequence, not a task; nothing here
  changes the single-threaded execution model or the RFC-0013 host-owns-the-loop
  arrangement.
- It does not make streams storable in module state. A stream's lifetime is a
  scope, which is what makes the obligation checkable.
- It does not cover bidirectional WebSocket messaging. `ws` here projects a
  server-to-client stream; client-to-server messaging is a separate design.

## Milestones

- **M1 — the type.** `Stream<T>`, linearity in movecheck with its diagnostic,
  `for … in` consumption with release on every exit path, `close()`. **Shipped**
  — see "As landed" below; the trap claim in this document is wrong and M3
  depends on it.
- **M2 — `std/stream`.** `unfold`, `fromArray`, `map`, `filter`, `take`,
  `merge`, `channel` with mandatory bounded capacity and overflow policy.
  **Shipped in part** — four of the seven; `unfold` and `channel` are the
  representation change M1 did not make, and `merge` shipped as sequence
  interleave. See "As landed — M2".
- **M2b — the pull representation.** `Stream<T>` stops being a buffer.
  **Shipped** — the four deliverables M2's price list itemised, plus `unfold`.
  See "As landed — M2b".
- **M2c — the combinators are not lazy.** M2b made the *representation* pull,
  and stopped there. `map` and `filter` still drain their source into a buffer,
  so `map(unfold(..), f)` over an endless feed does not return — and `map` over
  a feed is the first thing anyone writes. **Shipped** for `map`, `filter` and
  `take`; `merge` is still eager, for a stated reason. See "As landed — M2c".
- **M3 — cancellation + conformance.** The normalized signal and the conformance
  suite. **Shipped, and not here**: four of the six rows held after M2b, one is
  struck, and the last came with the transport rather than before it — RFC-0074
  M3a. The normalized signal turned out to be the failing write, which is the one
  mechanism every host implements identically because it is the socket.
- **M4 — transports.** **Given up to RFC-0074 M3**, which spells `sse` and `ws`
  as projections and owns `Route`. They were always the same adapters and the
  same evidence, and two RFCs claiming one deliverable is how it gets built
  twice or not at all. What stays here is the *contract* the adapters must meet:
  the conformance table above, resumability via the cursor (M2b made the seed
  the resume token), and the `#6156` cycle count.

### M2c — the combinators are not lazy

**What M2b actually bought, stated exactly.** A `Stream<T>` is a producer, and
`take(unfold(..), n)` asks n + 1 times and stops. That is the representation.
It is not the library: `map` and `filter` are still `for x in s { out.push(..) }
… fromArray(out)`, which drains the source. So the milestone that exists to make
an unbounded feed expressible leaves the most obvious thing to do with one —
`map` it into wire frames — a silent hang, and RFC-0074 M3a hit exactly that
(its `sse` element is an encoded frame rather than a mapped record because of
it).

`take` escapes because it `break`s. `merge` documents its hang. `map` and
`filter` did not, and now do.

**Why this is a milestone and not a patch.** A lazy `map` is a stream whose step
calls another stream's step. The source is a **linear resource**, so the wrapper
must own it, and RFC-0037 captures by value — so the source has to live
somewhere the step can reach without a second owner existing. That is the same
class of question M2b answered for the seed, and it deserves the same treatment:
its own evidence, its own pins, and a stated answer for what `close` on a
wrapped stream releases.

`filter` is the harder half and worth naming separately: a lazy `filter` may ask
its source any number of times to answer once, so "one `next` in, one `next`
out" stops being the shape, and the conformance row about a release running
before the next element would be produced needs re-reading against it.

**The shape, and the one open question.** A wrapper needs its source (six words)
and its function (two) and has six to put them in, so it cannot hold them
inline. But it does not have to: M2b's producer variant already reaches its
state indirectly — `cur`/`gen` are a handle into the cursor slab. **A wrapper's
state goes in a slab slot the same way**, and the wrapper is then an ordinary
producer whose synthesised step reads the slot, asks the source, and applies the
function. `take` becomes lazy by the same move with a counter beside the source,
which also retires the "`take` escapes because it `break`s" special case.

The open question is that slot's lifetime, and it is the milestone's real
content: **`close` on a wrapper must release the source, exactly once, on every
path M1 counts.** A chain of three combinators over one producer is four streams
and one release each, and the number is countable in the IR the way M2b's were.
Get that wrong in the direction of releasing twice and it is a double free; in
the other direction it is `#6156` with extra steps.

The alternative is to box the wrapper's state, which trades the slab's fixed
65 536 for a malloc per combinator application. It is worth pricing rather than
assuming — but note that the slab's ceiling is what makes a missed release
*trap* instead of merely growing, which is why M2b's cycle row came out stronger
than this RFC asked for. A malloc would give that up.

**What must not happen** is a lazy `map` that quietly keeps the eager one for
buffer sources. Two implementations of one combinator, chosen by a runtime tag,
is the unreferenced multiplicity this project has repeatedly grown a divergence
from. One `map`, whatever it costs a buffer source.

**The pins.** `take(map(unfold(endless), f), n)` returns, and asks the source
exactly n + 1 times — the same relation M2b pinned, now through a wrapper.
`filter` over an endless source with a predicate that admits one in k asks
roughly kn. Both are numbers rather than shapes, for the reason M2b's was: the
eager version passes every assertion about the values.

*(Both numbers came out tighter than this predicted — n, and 3n − 2 for one in
three. See "As landed — M2c"; the +1 was an artefact of the eager `take`.)*

### M2b — the pull representation

**What forced it.** RFC-0074 M3 spells `sse("/", tail).retryAfter(3000)` where
`tail` yields a live feed. A `Stream<T>` is `Array<T>`'s three words, so that
call materialises the entire feed before the first byte reaches the client —
which is `#6156`, the incident this RFC quotes, arriving through the library
written to prevent it. The eager representation was the right M1 decision and it
is now the thing standing between this RFC and its own transports milestone.

**The deliverables are the price list**, already itemised above and repeated
here as work: `Stream<T>` becomes a tagged union across the interpreter, the
LLVM emitter and the direct wasm backend; `fromArray` stops emitting nothing and
`from_array_emits_no_instruction` is **deleted rather than adjusted**; `close`
becomes variant-aware and M1's four release-path pins are re-counted in the IR;
`for … in` over a stream stops sharing `direct.rs`'s indexed-array arm and
becomes a `next`-until-`Done` loop. The mechanism is RFC-0037's: defunctionalise
the producer into a closed enum with one variant per `unfold` site and make
`next` an `@dispatch` match, which is how the seed type `S` gets hidden without
an existential.

**The pin is a measurement, not a shape.** An unbounded producer consumed by
`take(n)` must allocate O(n), not O(feed) — and the eager version passes any
assertion about the *values* while failing that one. Something that cannot
terminate under the old representation is the only honest evidence, so the
example must be a producer with no end rather than a large finite one.

**The alternative, and why not.** `sse` could take a procedure the runtime calls
once per event, and then no representation changes at all. It is refused because
each adapter would hand-roll its own cleanup and its own disconnect handling —
the two things `Stream<T>` exists to supply once. A second transport would write
them a second time, differently, which is the shape of every bug M1's linearity
was built to make unspellable.

## As landed — M1

M1 shipped as specified: `Stream<T>`, the linearity, `for … in` consumption with
release on every exit path, and `close()`. Four things the text above got wrong
or left unsaid, in the order they cost time.

**A trap does not run the release, and cannot.** The text claims "`break`, early
`return`, and a trap all run the producer's release path", and the conformance
table has a row for "consumer traps → release runs during unwind". **Vyrn has no
unwinding.** Every trap on every engine is `fputs(stderr); exit(1); unreachable`
— checked in the emitted IR, and it is the same in the interpreter and the direct
wasm backend. So no drop, no `region_exit` and no stream release has ever run on
a trap in this language, and none does now. For M1 the consequence is nil: the
release is a `free`, and the process exiting reclaims the buffer either way. For
M3 it is not nil — a producer holding a socket would not close it — and closing
that row means *adding unwinding*, which is a language change with a
three-engine parity cost, not an adapter detail. The row should be rewritten as
"the host reclaims on abnormal exit" or the RFC should own the unwinding
question explicitly. **Do not plan M3 assuming the release runs on a trap.**

**`Stream<T>` is a new `Type` variant whose lowering is `Array<T>`'s exactly.**
The open question was type-versus-library-type-with-an-attribute. It has to be a
type, because both alternatives launder the obligation: an attribute on
`Array<T>` leaves `at`/`push`/`.length` applicable to a stream, and a library
alias has to name an `Array` base, so `let a: Array<T> = s` erases it. But that
is entirely a *checker* argument — the runtime has nothing new in it. `Stream<T>`
is `{ ptr, i64, i64 }`, `Val::Array`, and the same indexed walk, on all three
engines; `fromArray` lowers to no instruction at all (pinned by emitting it
beside the `Array` version and comparing the bodies byte for byte). RFC-0083's
`F32x4` was the mirror image: a new representation with no ownership. This is a
new *rule* with no new representation.

**The obligation needed a second analysis, not an extension of the first.**
"Extend movecheck" was the right instinct and the wrong mechanism. Movecheck's
existing `Consumed` map is a **may**-analysis — a value consumed on either branch
of an `if` is consumed afterward, which is what makes use-after-consume sound.
Disposal is a **must**-analysis, and the two want opposite merges at every
branch. Folding them would have made one of the two wrong everywhere. So M1 adds
a second walk over the same bodies (`movecheck::streams`), sharing the file and
the diagnostic channel and nothing else. It is ~250 lines and it is exact rather
than conservative, for one reason worth recording: **a stream has no read
operations at all**, so every mention of a stream binding is a move, and a
syntactic walk needs no notion of position. The stronger claim was affordable
because the type surface was kept small.

Two rules fell out of implementation that the text does not mention, and both are
load-bearing:

- **A stream may not be stored.** The text says this about module state; it is
  true of any composite. `type R = { s: Stream<Int64> }` was a one-line erasure
  of the entire obligation, so a stream is now legal exactly at the root of a
  binding, a parameter, or a return type — rejected in a record field, an enum
  payload, an `Array`, an `Option`, a `Ref`, a `Map` value, and module state.
- **A `Stream` parameter carries the obligation into the callee.** Otherwise
  `fn sink(s: Stream<Int64>) -> Int64 { return 0 }` is the same one-line hole:
  the caller discharges by moving, and nobody else has to do anything.

**Disposing twice is the other half, and it is the worse bug.** The text is only
about the leak. `close` frees the buffer, so a second `close` — or a `close`
after a `for … in` — is a double free, which the may-analysis does not catch
because `close` is not a `consume` parameter. Both directions are checked now,
and where two branches disagree about whether a stream was disposed, the
*acquisition* is reported rather than waiting for a later statement to make the
merge look clean: whatever follows, one of those two paths is wrong.

Two limits, stated so they are not mistaken for coverage. Bindings are keyed by
**name**, exactly as the `Consumed` map above them is, so an inner `let s = 1`
shadowing an outer stream `s` reads as a disposal of the outer one; fixing that
means giving both analyses a scope-id key at once. And the `close` on the direct
wasm backend reclaims nothing, because that backend's `malloc` is a bump pointer
that never frees — pre-existing, already noted at its `region_exit`, and
unobservable, since a released stream cannot be named again on any engine.

## As landed — M2

Four of the seven combinators shipped: `map`, `filter`, `take`, `merge`, in
`std/stream.vyrn`, with `examples/streamops.vyrn` as their three-engine parity
evidence. `unfold` and `channel` did not, and the reason is the first thing
below because it is the milestone's real content.

**The eager representation survives, and it is the boundary.** M1 chose
`Stream<T>` = `Array<T>`'s three words. Everything shipped here is a walk over a
buffer that already exists, producing another one, so all four are ordinary Vyrn
generics — no builtin, no census row, no IR. `unfold` is not that. Its state is
a *seed plus a step*, and the seed's type `S` appears nowhere in `Stream<T>`:
the consumer is monomorphized at `Stream<Int64>` with no `S` in scope, so it
cannot call the step. Vyrn has no existential and no dynamic box to hide `S` in.
The one mechanism that *could* hide it is the one RFC-0037 already uses for
closures — defunctionalize the producer into a closed enum with one variant per
`unfold` site, and make `next(s)` an `@dispatch` match — and that is a different
representation, not an addition to this one. Its price, itemised:

- `Stream<T>` stops being `{ ptr, i64, i64 }` (`llt_of`) and becomes a tagged
  union, on the interpreter, the LLVM emitter and the direct wasm backend.
- `fromArray` stops being free. It is currently pinned to emit *nothing*, by
  compiling it beside the `Array` version and comparing the bodies byte for
  byte (`from_array_emits_no_instruction`); it would become a variant
  construction and that pin would be deleted, not adjusted.
- `close` stops being a `free` and becomes variant-aware, which re-opens the
  four release-path pins M1 counted in the IR.
- `for … in` over a stream stops being the indexed array walk it shares with
  `Array<T>` today (`direct.rs`'s `Type::Array(inner) | Type::Stream(inner)`
  arm) and becomes a `next`-until-`Done` loop.

That is a language change with a three-engine parity cost, and it should be its
own milestone with its own evidence rather than a line item inside "add the
combinators". What must NOT happen is the version that fits in the current
representation: running the step function to exhaustion at construction time.
That compiles, passes parity on any finite seed, and turns "stream an unbounded
feed" into "materialize an unbounded feed" — which is `#6156`, the incident this
RFC quotes, reintroduced by the library that was supposed to prevent it.

`channel` is refused for the same reason plus one more: it is push-shaped, so
something must be able to hand it a value while a consumer is not asking, and
this RFC states outright that it adds no concurrency and RFC-0013 leaves the
loop to the host. There is no producer to push. The mandatory capacity and the
`Block`/`DropOldest`/`Fail` policy are the right design and they stay unbuilt —
inventing a policy for a queue nothing can fill would be a decision made to have
something to ship.

**`merge` shipped as sequence interleave, which is the only merge this RFC can
mean.** Turn by turn, then the longer side drains. Arrival-order merge needs a
notion of which source is *ready*, and a sequence has no such notion — the "does
not add concurrency" line above rules it out at the design level, not the
implementation level. For any stream that terminates, turn-taking is also what a
pull-based merge would produce, so the observable sequence is not a compromise.
What is genuinely lost is merging an endless source with a finite one, and that
is the `unfold` gap again rather than a second one.

**The obligation composes; it was checked rather than assumed.** The worry worth
having about combinators is that `map(s, f)` becomes a laundry — takes the
obligation in and hands none back. It cannot, and the reason is that no rule
about combinators exists: `std/stream` is ordinary Vyrn, so each function has a
`Stream` parameter (M1: the parameter carries the obligation into the callee),
discharges it with `for … in`, and returns a `Stream` (M1: the caller owes the
result). Both halves are pinned in `movecheck`
(`a_combinator_neither_swallows_the_obligation_nor_launders_it`), and
`examples/stream_combinator_abandoned.vyrn` is the corpus version — an abandoned
`map(...)` result, an abandoned chain, and a chain consumed then `close`d, all
producing M1's own diagnostics. The release paths were counted in the IR rather
than reasoned about: a chain releases once per stream in the function that owns
it, on the normal, `break` and early-`return` paths alike.

**M1 left `Stream` out of every generic type walk, and M2 is what found it.**
M1 never put a type parameter *inside* a stream — `fromArray` was always called
at a concrete element type — so `Stream` was missing from `substitute`, both
unifiers, `collect_params`, `walk_type`, `contains_heap`, `fn_sigs_match`, the
loader's three type walks, the parser's member-parameter marking, and the schema
reflector's name collection. The one worth naming is the codegen unifier: the
LLVM emitter silently substituted `Unit` where the direct backend refused the
call outright, which is the two backends specializing *different* functions for
one call site — exactly the failure `solve_type_args` was centralised to prevent.
Three arms that had to be written and eight that were one token added to an
existing or-pattern; the lesson is that a new `Type` variant needs the walks
swept even when the milestone that adds it has no use for them.

One diagnostic changed. A generic producer returns `Stream<U>`, and quoting that
at `let m = map(feed(), double)` names a type parameter the program never wrote;
the pass has no types, so it now says plain `Stream`, which is what it already
said for `fromArray`.

## As landed — M2b

The four deliverables landed as priced, `unfold` joined `std/stream`, and
`examples/streamunfold.vyrn` is the three-engine evidence. What the design got
wrong is one thing, and it is the central one.

**The seed type is not hidden by defunctionalizing the producer. It is hidden by
not having one.** This document says the mechanism is RFC-0037's — "one variant
per `unfold` site", `next` an `@dispatch` match — and that is right about the
machine and wrong about who builds it. RFC-0037 already synthesizes one closed
enum per *signature*, and a step function IS a stored fn value, so the site table
existed before this milestone started and no new one was written. What did not
exist was a way to make that signature independent of `S`, and that is the whole
problem: a stream dispatches through its step's signature, so an `S` in the
signature is an `S` in `Stream<T>` by another route. The RFC's spelling —
`unfold(seed, |s| Next(v, s'))` — needs a `Step<T, S>` whose layout depends on
`S`, which is the same wall one level down.

What actually erased it: **the cursor is a `Ref<Int64>` and the step answers
`Option<T>`.** A `Ref<S>` is two words for every `S`, and pinning it at `Int64`
makes `fn(Ref<Int64>) -> Option<T>` a function of the element type alone —
which is exactly the property the dispatcher needs. It also costs no new type:
`Option` and `Ref` are builtins every walk, every layout and every engine
already handles, so the "sweep every generic walk" lesson M2 recorded was not
paid a second time. The producer is spelled

```vyrn
fn tail(c: Ref<Int64>) -> Option<Paste> {
    match store.after(get(c)) {
        Some(p) => { set(c, p.created) return Some(p) }
        None => return None
    }
}
… unfold(req.since, tail)
```

which threads the cursor exactly as `Next(p, p.created)` would, and keeps the
resumability property intact: the seed IS the resume token. The limit is real
and stated — producer state that is not one integer goes in a second cell the
step closes over, since RFC-0037 captures are by value and a `Ref` handle copies
fine. `unfold` is a three-line generic in `std/stream`; the builtin under it is
`fromStep`, and the split is load-bearing rather than cosmetic: the builtin
chain has no access to `check_fn_arg`, so a builtin taking a `fn` argument
cannot type a lambda literal. The wrapper gets that for free from the ordinary
RFC-0023 higher-order path.

**`Stream<T>` is six words, overlaid, not a union of the widest variant.**
`{ ptr data, i64 len, i64 tag, i64 pay, i64 cur, i64 gen }`; a negative `tag`
means a buffer and `cur` is its read cursor, a non-negative one means a producer
and `tag`/`pay` ARE the fn value while `cur`/`gen` ARE the cursor cell. Two
things about that ordering are deliberate. The pairs are adjacent and 8-aligned,
so `&s + 16` and `&s + 32` are a `{ i64, i64 }` fn value and a `{ i64, i64 }`
`Ref` with nothing to reassemble — both backends load them whole. And the
alternative, boxing the producer's state, would have added a malloc to every
`unfold` and an indirection to every `next` to save two words on a value that
lives in a frame slot.

**`close` did not need a branch at the call site. It needed a runtime
function.** The branch itself is trivial; the problem is where drops are emitted.
`emit_drop` runs mid-block, immediately before an early `ret`, and M1's pin says
the release is *in the block the `ret` terminates* — emitting a branch there
would have split that block and made the pin's own claim untestable. So the
release is one call to `@__vyrn_stream_close`, which branches inside itself. Two
consequences worth recording: `RUNTIME_FREES` went 2 → 4 (the helper holds one
`free` per variant), and every stream release site is now countable by name
rather than by counting `free`s, which is strictly the better pin.

**The re-counted release-path pins, against M1's and M2's numbers.** M1 counted
`free`s; the count is now `@__vyrn_stream_close` calls, and the number of `free`s
at the call site is zero — a stream is no longer reclaimable as an array,
because which of the two producers it holds is not knowable there.

| pin | M1/M2 counted | M2b counts |
|---|---|---|
| `for … in`, normal exit | 1 free, in `fend` | 1 close, in `fend` |
| `for … in`, `break` | 1 free | 1 close, and `break` still branches to it |
| `for … in`, early `return` | 2 frees | 2 closes |
| `close(s)` | 1 free | 1 close |
| combinator chain | 2 frees | 2 closes |
| chain + `break` / early `return` | 2 / 3 frees | 2 / 3 closes |

The numbers did not move. Only what a release *is* moved, which is the outcome
worth having: the release paths were the part M1 got right.

**`from_array_emits_no_instruction` is deleted rather than adjusted**, as this
document asked. `fromArray` emits six `insertvalue`s now. Its claim — that
`Stream<T>` and `Array<T>` lower identically — is simply not true any more, and
a weakened version of it ("emits few instructions") would have asserted
something nobody chose.

**The measurement, which is the milestone.** `examples/streamunfold.vyrn`'s
`naturals` never answers `None`. `cost(n)` takes `n` from it and reports how
many times the step ran: **6 for n = 5, 1001 for n = 1000, 20001 for
n = 20000** — n + 1 every time, the extra being the element `take` reads before
its `break` fires, and the allocation is `take`'s output array, so it is n too.
**(M2c retired the extra ask along with the eager `take`: the numbers in that
file are n now. The relation below is M2b's, recorded as it stood.)**
Under M1's representation there is no number here; there is a program that does
not terminate. The same file runs `cycles(10000)` — RFC-0075's own acceptance
row — and that row turns out to be *stronger* than this document claims:
cursor cells come from a slab of 65536 on all three engines, so a `close` that
failed to release would not merely hold memory above baseline, it would trap on
the 65537th acquisition. The interpreter's own test runs 100 000 of them.

Three smaller things, stated so they are not mistaken for coverage.

- **A dead dispatcher is emitted for any program containing a stream**, even one
  that only ever calls `fromArray`: the loop reserves the step signature's
  dispatcher unconditionally, and with no registered variants its body is the
  defensive trap alone. Ten lines, unreachable, and removing it would mean
  deciding at loop-emission time whether any `fromStep` exists anywhere in the
  program — a whole-program question asked from inside one function body.
- **`merge` with an endless side is now a hang** rather than something the
  language cannot spell. M2 recorded "nothing in the language can build an
  endless source yet" as what merge-by-interleave loses; something can now, and
  the answer is `take` on the endless side first. A merge that stops on its own
  needs both sides to.
- **The direct wasm backend still reclaims nothing for a buffer stream**, which
  is M1's note unchanged — its `malloc` is a bump pointer. The cursor cell is
  the exception and the reason `close` grew a real body there: the slab is not
  the bump heap, and it is finite.

`channel` is unchanged by any of this. It is push-shaped, and the representation
was never what stood in its way — this RFC adds no concurrency and RFC-0013
leaves the loop to the host, so there is still no producer to push.

## As landed — M2c

`map`, `filter` and `take` are lazy on all three engines, `examples/streamlazy.vyrn`
is the evidence, and `merge` is not. Two builtins arrived, `fromWrap` and `pull`,
and the census went 84 → 86 — the second row is the interesting one, and the
first thing below.

**A wrapper needed no words at all.** This document says a wrapper "needs its
source (six words) and its function (two) and has six to put them in", and then
solves that with a slab slot. The premise is wrong: a wrapper does not need a
representation of its own, because M2b's producer already has the two things it
wants. `fromWrap(src, step)` allocates the SAME cursor cell `fromStep` does and
parks the step in the SAME two header words; the only difference is what the
cell holds. So a wrapper IS an ordinary producer — `for … in` needed no arm for
it, the dispatcher no new key, `Stream<T>` no new tag, and the six words are
still the six words M2b laid out.

What the cell holds is `{ i64 cursor, Stream src }` in one allocation, plus a
pointer to that second half in a fourth parallel array beside the slab's
generations, pointers and free list. Three consequences, all of them load-bearing:

- **`take`'s counter is the cursor.** This document imagined "a counter beside
  the source". It is the cell's first word — the one `fromStep` puts a seed in —
  so `get(c)`/`set(c, i + 1)` inside `take`'s step are the ORDINARY cell
  operations, unchanged and unaware. Nothing was added for it.
- **`pull` needs no new state either.** It is the wrapper's step reading its own
  cursor cell's other half.
- **The array doubles as the flag.** Null means an ordinary cell, so `pull` on
  one traps (`error: no stream behind this cursor`) rather than reading past an
  8-byte box. `pull` is a builtin and a program can spell it; the trap is
  pinned in all three engines.

**Slotted, not boxed, and the price list came out one-sided.** Boxing was to be
priced as "a malloc per combinator application" against the slab's fixed 65 536.
The comparison is not that: a cursor cell's payload is ALREADY a malloc on the
native backend, so putting the source in it costs the same one allocation a
box would — 56 bytes instead of 8. Slotting is therefore the same price plus the
ceiling, and the ceiling is what makes a missed release trap rather than grow.
There was nothing to trade.

**`close` became a walk, and the release counts moved — one of them.** A wrapper
holds its source where no function can name it, so releasing the outermost
stream has to reach the rest. `@__vyrn_stream_close` is a loop over the chain
(the wasm backend runs the same loop, spelled with `br_if`; the interpreter's is
`loop { release; take the source }`), one iteration per stream, and it stops at a
buffer or at a cell with nothing behind it.

| pin | M1/M2 counted | M2b counts | M2c counts |
|---|---|---|---|
| `for … in`, normal exit | 1 free, in `fend` | 1 close, in `fend` | unchanged |
| `for … in`, `break` | 1 free | 1 close | unchanged |
| `for … in`, early `return` | 2 frees | 2 closes | unchanged |
| `close(s)` | 1 free | 1 close | unchanged |
| combinator chain | 2 frees | 2 closes | **1 close, +1 walked** |
| chain + `break` / early `return` | 2 / 3 frees | 2 / 3 closes | **1 / 2, +1 walked each** |

Only the chain rows moved, and they moved for a reason worth stating exactly:
under the eager version the intermediate stream was a local inside `map`, which
released it with its own `for … in` — so the second release was a second CALL
SITE. A lazy `map` has no loop and releases nothing; the second release is the
same release, executed one iteration further down the walk. The site count is
now a count of streams a function can NAME, and the release count is still one
per stream. Both halves are pinned:
`a_lazy_combinator_releases_at_one_site_and_walks_the_rest` asserts the site
count and that `map`'s own body contains no release at all, and
`examples/streamlazy.vyrn` runs 30 000 cycles of a three-deep chain — 90 000
cursor cells out of a slab of 65 536, so a walk that stopped one stream early
would trap, and one that went round twice would hand a slot back twice and
hand two live streams the same cursor.

**The step counts, and the one this document got wrong.** `take(map(feed, f), n)`
asks the feed **exactly n times**, not the n + 1 pinned here. The extra one was
never laziness's: it was the element the eager `take` read before its `break`
fired, and a wrapper checks its counter before it asks at all. So M2b's
`examples/streamunfold.vyrn` numbers changed with this milestone — 6 → 5,
1001 → 1000, 20001 → 20000 — and the file says so. `filter` admitting one in
three asks **3n - 2** to yield n, which is exact rather than "roughly kn"
because the producer is exact.

**A lazy `map` is CHEAPER for a buffer source, not dearer.** The constraint here
was to accept whatever one implementation cost a buffer stream. It costs it
nothing: the eager version built an output array of the same length, and the
wrapper builds nothing. The one real risk was internal — a shared "ask a stream
for one element" emitter that answered an `Option<T>` would have boxed every
element wider than a word, putting an allocation (and a leak, since sum payload
boxes are not reclaimed) inside `for r in fromArray(records)`. So the shared
emitter answers a staged element and a boolean, and `pull` builds the `Option`
its own signature owes. One implementation, and the loop pays nothing for it.

**A lambda cannot capture an RFC-0023 `fn` parameter.** Found here, because a
lazy combinator is exactly the shape that wants to: `map`'s step captures `f`.
A `fn`-typed parameter is not a value in scope — it is a specialization the
caller chose — so the LLVM backend emitted a call to an undefined `@vyrn_f` and
the failure landed at the linker. The one-line fix is in `std/stream` rather
than in the backend: `let g: fn(T) -> U = f` re-materializes it as an RFC-0037
stored value, which a lambda captures the ordinary way. It is recorded as a
compiler hole rather than closed, because closing it means teaching the lambda
lifter to forward another instance's capture arguments — a change in the
higher-order machinery, not in this milestone.

**`merge` is still eager, and the reason is structural rather than schedule.** A
wrapper owns ONE source, because a cursor cell has one stream behind it.
Interleaving wants two sources and a step that remembers whose turn it is, which
is a second wrapper shape and a second thing for `close` to walk — not a second
use of this one. So `merge` with an endless side is still a hang, `std/stream`
says so in the place it used to say `map` was, and the answer is still `take` on
the endless side first.

**RFC-0074 M3a is unblocked, and the proof is at the transport.** Its `sse`
element is an encoded frame partly because mapping a feed hung. `serve.rs` now
serves `map(unfold(0, nums), frame)`: the frames arrive, the client vanishes, the
feed stops, and `/probe` traps on the INNER producer's cursor — a cell the host
never touched, released because the walk reached it.

## Acceptance

- A stream acquired and abandoned is a **compile error**; the `#6193` program
  shape does not build.
- Client disconnect runs producer release within 100 ms on every adapter,
  proven by the conformance suite rather than by a per-host special case. **Met,
  and by a better rule than a deadline** (RFC-0074 M3a): the release is the
  statement after the write that failed, so it runs before the next event would
  be produced.
- 10 000 open-then-abandon cycles hold memory within 5% of baseline — `#6156` as
  a regression test.
- `break` inside `for … in` over a stream leaks nothing, verified by the existing
  `RUNTIME_FREES` accounting.
- A raw `EventSource` does not reconnect after a normally-completed stream.
  **Met by construction** (RFC-0074 M3a): the adapter pulls the first element
  before it writes a status line, so a producer with nothing left answers `204 No
  Content` — the one status the WHATWG algorithm reads as "stop". A drained feed
  therefore costs the client one more request rather than an endless loop; the
  pin is `a_producer_with_nothing_to_say_answers_204_rather_than_an_empty_stream`
  and the browser half is M3b's to run.
- Three-way parity green: identical event sequences and identical trap wording
  across interp, native, and wasm.
