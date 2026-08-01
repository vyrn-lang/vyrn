# RFC-0075 — `Stream<T>`: Cleanup as an Obligation, Not a Convention

- **Status:** **M1 shipped; M2 shipped in part** — `Stream<T>` is a linear
  resource, an abandoned stream does not compile
  (`examples/stream_abandoned.vyrn`), and release runs on normal end, `break` and
  early `return`. `std/stream` ships `map`, `filter`, `take` and `merge`;
  `unfold` and `channel` are **refused for now** — they need a producer the eager
  representation cannot hold, see "As landed — M2". M3–M4 unstarted; **M4
  additionally depends on RFC-0074, which has no implementation at all.** One
  claim in this document is now known false — see "As landed — M1" on the trap
  row.
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

| test | requirement |
|---|---|
| client disconnects mid-stream | producer release runs within 100 ms |
| consumer `break`s | release runs before the loop's next statement |
| consumer traps | release runs during unwind — **unachievable as written; see "As landed"** |
| producer raises | release runs; the error surfaces to the consumer |
| stream is dropped unconsumed | compile error, not a runtime condition |
| 10 000 open-then-abandon cycles | steady-state memory within 5% of baseline |

The last row is `#6156` as a regression test. The suite is a public part of
`std/stream`, so a third-party adapter proves itself with the same file the
built-in adapters run.

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
- **M3 — cancellation + conformance.** The normalized signal; the conformance
  suite; native and wasm adapters passing it.
- **M4 — transports.** `sse` and `ws` projections; resumability via seed;
  the raw-`EventSource` completion test; a live tail in `examples/bin`.

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
unifiers, `collect_params`, `walk_type`, `contains_heap`, and the loader's two
type rewrites. The one worth naming is the codegen unifier: the LLVM emitter
silently substituted `Unit` where the direct backend refused the call outright,
which is the two backends specializing *different* functions for one call site —
exactly the failure `solve_type_args` was centralised to prevent. Six one-token
arms and two real ones; the lesson is that a new `Type` variant needs the walks
swept even when the milestone that adds it has no use for them.

One diagnostic changed. A generic producer returns `Stream<U>`, and quoting that
at `let m = map(feed(), double)` names a type parameter the program never wrote;
the pass has no types, so it now says plain `Stream`, which is what it already
said for `fromArray`.

## Acceptance

- A stream acquired and abandoned is a **compile error**; the `#6193` program
  shape does not build.
- Client disconnect runs producer release within 100 ms on every adapter,
  proven by the conformance suite rather than by a per-host special case.
- 10 000 open-then-abandon cycles hold memory within 5% of baseline — `#6156` as
  a regression test.
- `break` inside `for … in` over a stream leaks nothing, verified by the existing
  `RUNTIME_FREES` accounting.
- A raw `EventSource` does not reconnect after a normally-completed stream.
- Three-way parity green: identical event sequences and identical trap wording
  across interp, native, and wasm.
