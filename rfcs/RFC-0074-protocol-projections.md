# RFC-0074 — Protocol Projections: Full Fidelity, No Erasure

- **Status:** Draft
- **Depends on:** RFC-0072 (audience, roles, derived RPC surface — projections
  are the opt-in layer above it), RFC-0073 (symbol maps — projections write
  their policy into `derived`), RFC-0038 (`std/connect`, `std/openapi`,
  `std/graphql` — the reflection generators this RFC extends),
  RFC-0021 (`gen fn`), RFC-0037 (stored closures — the callbacks below)
- **Evidence (user):** "then how it would work with graphql, rest, grpc?
  SSE/WebSocket?", "this doesn't make sense tho? what about protocol specific
  features?", "what about GraphQL autogen?", "then we losing automatic
  resolving from paths and simplicity?"

---

## The rejected design

The obvious move — one service description, projected mechanically onto every
transport — was proposed and correctly rejected: it erases exactly what makes
each protocol worth choosing. GraphQL without field selection, REST without
cache validators, gRPC without deadlines, and WebSocket without a lifecycle are
all just RPC wearing a costume.

This RFC takes the opposite position. The **procedure** is shared and
transport-free. Each **projection** is a separate file speaking its protocol's
own vocabulary at full fidelity. Nothing is reduced to a common denominator,
because there is no common denominator layer.

## Two surfaces, not one

**The RPC surface is derived and needs no file** (RFC-0072). It is what pages
and clients call. Zero configuration, no naming rules, complete coverage.

**A protocol projection is opt-in and hand-written**, because it encodes
decisions no reflection can make. `GET /pastes/{id}`, an ETag, and a `201
Created` carrying a Location header are API *design*; deriving them would
require inventing the magic naming conventions this arc exists to remove.

So simplicity is the default and explicitness is available. You never write a
projection until you publish a public API.

## Placement

A projection colocates with its procedures by stem, and derives its base path
from that stem:

```
server/api/pastes.vyrn            procedures
server/api/pastes.http.vyrn       REST projection      base /pastes
server/api/pastes.graphql.vyrn    GraphQL projection
server/api/events.vyrn            stream procedures
server/api/events.http.vyrn       SSE + WS projection  base /events
```

```json
{ "projections": { "http": ".http", "graphql": ".graphql" } }
```

The suffix marks the file's kind at a glance, keeps it beside what it projects,
and derives the base path so renaming the resource moves its URLs.

## `std/http`

```vyrn
/// server/api/pastes.http.vyrn — base path `/pastes`, from the stem.
/// Only sub-paths are written here.
import { Route, get, post } from "std/http"
import { list, byId, create } from "./pastes"

export fn routes() -> Array<Route> {
    return [
        get("/",     list).cacheFor(60).etag(),
        get("/{id}", byId).cacheFor(3600).etag().notFoundWhen(|e| e == "no such paste"),
        post("/",    create).createdAt(|p| "/pastes/\{p.id}"),
    ]
}
```

The critical property: **the chain is value-level, not type-level.** `Route` is
`Route` after every call. Nothing accumulates in the type, so the failure modes
catalogued in the DX survey — router-size-proportional check latency, a global
instantiation budget producing non-local errors, composition that breaks around
the fifteenth chained call — have no mechanism here. Adding the fortieth route
costs the checker one more nominal value.

`get`/`post`/`put`/`patch`/`delete` are generic over the procedure's signature
and erase it into a uniform `Route` holding a `fn(Request) -> Response`. Path
placeholders are checked against the procedure's input type via RFC-0073's
symbol map: `{id}` requires an `id` field, and a typo is an error listing the
available fields.

Vocabulary is HTTP's, not a neutral abstraction: `cacheFor`, `etag`,
`lastModified`, `vary`, `status`, `createdAt`, `notFoundWhen`, `accepts`,
`produces`. Each writes into the route's `derived` metadata so `vyrn routes` and
hover show the policy.

### Streaming projections

SSE and WebSocket are different protocols and are spelled differently. Options
meaningless to one are absent from the other rather than ignored:

```vyrn
import { sse, ws, Route } from "std/http"
import { tail } from "./events"

export fn routes() -> Array<Route> {
    return [
        sse("/",       tail).retryAfter(3000).resumable(),
        ws("/socket",  tail).heartbeat(30).closeCode(1001),
    ]
}
```

- `sse` — `retryAfter` (reconnect hint), `resumable` (Last-Event-ID replay,
  which requires the procedure's stream to accept a cursor), `keepAlive`.
- `ws` — `heartbeat`, `closeCode`, `subprotocol`, `maxFrame`.

Both consume a `Stream<T>` procedure (RFC-0075), which supplies the cleanup
guarantee and the normalized cancellation these adapters need.

## `std/graphql` — generated, then overridden

Reflection produces the entire schema. `schemaOf(T)` already yields names,
bases, docs, and constraints, so `where` clauses become non-null and constrained
scalars without restating anything:

```vyrn
import { graphql } from "std/graphql"
import { create } from "./pastes"
import { Paste } from "../../shared/wire/paste"
import * as store from "../store"

export fn sdl() -> Schema {
    return graphql("./pastes")
        .mutations([create])
        .lazy(|p: Paste| p.body, |p| store.loadBody(p.id))
}
```

Two things reflection cannot know, both declared in real symbols rather than
strings:

- **Mutation-ness.** Vyrn does not track effects, so it is declared. A list of
  procedure references, not a naming convention, and not a guess.
- **Lazy fields.** Autogen resolves whole objects — correct, but eager. Override
  the one expensive field with a selector closure naming it in ordinary code, so
  `p.body` completes, hovers, and renames. Every other field keeps its generated
  resolver.

This is the corrected shape of the earlier claim that field selection forces a
hand-written schema. It does not: you generate the schema and override per
field. `std/openapi` follows the same rule and already works this way.

## `std/grpc` and others

Out of scope for this RFC, but the shape is fixed by it: a `.grpc.vyrn`
projection with gRPC's own vocabulary (deadlines, metadata, streaming
cardinality, status codes), consuming the same procedures. The point of the
placement and naming rules is that a third-party projection is a library, not a
compiler feature.

## Composition root

```vyrn
import { serve, mount } from "std/http"
import { rpc } from "std/rpc"
import { pagesThemed } from "std/ui"
import { route } from pagesThemed("./app/routes", "./theme.json")
import * as pastesHttp from "./server/api/pastes.http"
import * as eventsHttp from "./server/api/events.http"
import { sdl } from "./server/api/pastes.graphql"

fn handle(req: Request) -> Response {
    return mount(req, [
        rpc("./server/api"),          // the derived surface
        pastesHttp.routes(),
        eventsHttp.routes(),
        sdl().endpoint("/graphql"),
        route,                        // pages last: they own everything else
    ])
}
```

`mount` resolves in order, first match wins, and reports overlaps between
mounted groups as a startup error rather than shadowing silently.

## Milestones

- **M1 — `std/http` core.** `Route`, the method constructors, `mount`,
  placeholder checking against input types, stem-derived base paths, `derived`
  metadata emission.
- **M2 — cache and validators.** `cacheFor`, `etag`, `lastModified`, `vary`,
  `status`, `createdAt`, `notFoundWhen`, conditional-request handling
  (`If-None-Match` → 304).
- **M3 — streaming projections.** `sse` and `ws` over RFC-0075 streams. **Split:
  M3a is `sse` (shipped — see "As landed — M3a"), M3b is `ws`** — see below.
  RFC-0075 M4 named the same work and has given it up; what stays there is the
  conformance contract these adapters must meet, and M3a closed its last open
  row.

### M3a — `sse`, and the disconnect signal

**The signal is the write.** `#6204`/`#6343`/`#6842` burned eight months and
five PRs guessing which host event means "the client is gone" — `res.close` on
one deployment, `req.close` on another, none on a third, no `socket` at all on
two more. Vyrn does not guess: **the server learns the client is gone by trying
to write to it and failing.** That is the one mechanism every host implements
identically, because it is the socket rather than the framework, and it is what
makes the conformance row testable rather than deployment-specific.

So the loop is: produce one event, write it, and if the write fails, `close` the
stream. RFC-0075's "release runs within 100 ms" becomes the stronger and simpler
"release runs before the next event would be produced".

**A streaming response is a second shape, not a flag on the first.**
`ServeResponse` is a buffered `body: String` and must stay one — a `Vary`
header and a 304 are about a response that exists all at once. The handler
answers either that or a stream handle, and the server pumps the handle by
calling back into the interpreter for each element. Nothing about the buffered
path changes.

**`sse` does not return a `Route`.** This RFC says options meaningless to one
transport are *absent* from the other rather than ignored, and `Route` carries
`Policy` — `cacheFor`, `etag`, `lastModified`, the conditional-request
machinery — every one of which is meaningless on an event stream. So `sse`
returns its own record with its own protocol (`retryAfter`, `keepAlive`,
`resumable`), and `mount` accepts both. That separation is only spellable
because RFC-0084 M1 made a record a legal protocol target; before it, both would
have had to be the same record and the options would have had to be ignored.

**`resumable` is the cursor.** RFC-0075 M2b made the seed the resume token, so
`Last-Event-ID` is the seed handed to `unfold` and nothing new is needed to
support replay — the id written beside each event is the cursor that produced
it.

### M3b — `ws`

A second adapter, and the real test of whether the signal generalises: **it must
pass the conformance file `sse` passes, unchanged.** M3a wrote that file below
`std/http` on purpose, calling `serveStream`/`fromStep` directly, so there is
nothing to rewrite. If it needs rewriting, the signal did not generalise and
that is the finding.

**Scope: server-push.** RFC 6455 is bidirectional and this milestone is not. The
`ws` in this RFC's own example consumes a `Stream<T>` and its four options are
all server-side, so a client→server message has no shape here — it would want a
handler per message, which is a different design and not one this RFC spells.
Say so rather than half-building it.

**Who owns the bytes: Vyrn owns what the user chooses, the host owns what the
protocol fixes.** SSE's `data:`, `id:` and `retry:` are a design surface — which
event name, which id — so `event(id, name, data)` is Vyrn and the host writes
what it is handed. A WebSocket frame's opcode, length and mask are not a surface;
there is no choice in them. So the host frames, and Vyrn yields the payload. That
rule is worth stating because the two adapters look inconsistent without it.

**The handshake needs SHA-1**, and it should be ordinary Vyrn in `std/hash`
beside `fnv1a` — pure integer work, three-way parity for free, and pinnable
against RFC 3174's own vectors. Its doc comment must say what it is for:
RFC 6455 mandates it as a **handshake nonce transform**, not as a security
primitive, and a `sha1` sitting in a standard library invites exactly the misuse
that sentence prevents. `base64` is already in `std/codecs`.

**Predict `heartbeat` to fail for `keepAlive`'s reason, then check.** M3a refused
`keepAlive` because a pull producer says `Some` or `None` and never "nothing
yet", so there is no idle moment to fill — and the pump blocks in the producer,
so a timer has nowhere to fire from. A ping is host-generated rather than
producer-generated, which is the one difference worth testing before assuming
the answer carries over. If it does carry over, refuse it the same way and for
the same stated reason.

`closeCode` (the code in the close frame when the stream ends), `subprotocol`
(echoed in the handshake response) and `maxFrame` (splitting a large payload)
have no such problem.
- **M4 — schema overrides.** `graphql(...).mutations(...).lazy(...)`; the same
  override surface for `std/openapi`.

## As landed — M1

`std/http.vyrn`: `Route`, five method constructors, `surface`, `mount`, and a
`gen fn http(module)` that emits a placeholder-checked constructor per procedure.
`examples/rest.vyrn` is the three-engine evidence; `examples/bin` and
`examples/fullstack` both carry a projection now. The chain stayed value-level:
`Route` is a five-field record, `GET` takes one and returns one, and nothing in
M1 or M2's vocabulary can put a type parameter on it.

**RFC-0073 was not needed, and this is the milestone's one real finding.** The
text says placeholders are checked "via RFC-0073's symbol map", which has no
implementation. It turns out the check needs only the procedure's input type, and
the language could already express it: the generator reads the input record's
fields off `TypeInfo.source` with `lex()` and emits

```vyrn
export type PathById = String where value =~ "([^{}]|\{id\})*"
```

as the parameter type of `byId`. A literal argument is const-folded against that
predicate by the existing `prove_coercion`, so `byId("/{ID}")` is a checker error
quoting the pattern and the legal placeholders. `std/ui`, `std/tw` and `std/i18n`
already validate literals this way; nothing was added to the compiler. RFC-0073
is about *renaming* across the generated boundary, which is a different need.

**The cost of that check is one spelling change.** `GET(byId("/{id}"))`, not
`get("/{id}", byId)`. The pattern must sit in the procedure's own parameter slot
to be checked against that procedure's fields, and a uniform `get(pattern, proc)`
cannot be: `get` would have to be generic over the signature, and a generic
cannot hand a type parameter to `fromJson`, whose first argument must be a
declared type name. The verbs are UPPERCASE because `get` and `set` are reserved
(the `cell`/`Ref` builtins dispatch before user functions); given that, the wire
spelling beat a coined synonym.

**`mount` reports a shadow as a startup trap**, naming both routes and both
groups. It proves the containments it can — an earlier prefix covering a later
pattern, an earlier pattern whose placeholders swallow a later one — and stays
quiet otherwise, because a false startup trap is worse than a missed shadow.
Order *within* a group is the author's own and is not policed. A route that never
got a method traps the same way rather than defaulting to GET.

**The two surfaces are byte-identical because there is one codec, not two that
agree.** A projection decodes with the same `fromJson` and encodes with the same
`toJson` the derived surface uses, and it imports `validateContract` from
`std/rpc` rather than restating what a procedure is. `examples/rest.vyrn` asserts
status, content type and body equality between `GET /users/7` and
`POST /_/users/byId` on all three engines.

Three things are smaller than the text implies, stated so they are not mistaken
for coverage:

- **`vyrn routes` does not show explicit routes.** The table has exactly one
  producer — the generator that mounts the surface, emitting `//@route` — and a
  projection's patterns are written in a hand-written file the generator never
  reads. It cannot read one either: generation-time I/O is scoped to the
  generator's constant path arguments, and `http("./pastes")` admits `pastes` and
  `pastes.vyrn`, not `pastes.http.vyrn`. Showing them means either a second path
  argument at every call site or `vyrn routes` learning to run the mounted
  router, and neither is worth it before M2 gives the table a policy column.
- **`derived` carries the route line, not a policy line**, since M1 has no
  policy. Its one reader today is the shadow diagnostic, which names the
  procedure rather than only the path. M2's combinators append to it.
- **A path placeholder binds a string or an integer.** The generator resolves the
  field to its base type through one alias hop, so `/users/{id}` over an `Int64`
  arrives as `7`; a `Float64` field or a two-hop alias arrives as text and
  decodes as a 422 naming that field. `parse` is integer-only and no dogfood has
  either.

**A native codegen bug fell out of `surface`**, and half of it is fixed. A
lambda whose body CALLS a captured `fn` value — `|req, ps| run(req)` — left the
callee out of the lifted lambda's capture list, because the capture walk looked
only at a call's arguments. Both compiling backends then emitted a call to
`@vyrn_run`, a symbol no module defines; the interpreter, resolving through its
environment, ran the same program. It was a build failure on exactly the two
engines a `vyrn run` test never reaches. Capturing a `let`-bound fn value is
fixed in the shared walker (`examples/fnvalstore.vyrn` covers it on all three
engines). Capturing a fn-typed *parameter* is not: that binding has no slot — it
lives in `fn_bindings` as a target symbol plus capture values — and materializing
it as a capture is RFC-0037 work, not this milestone's. `Route` therefore carries
two `fn` fields, `run` and `whole`, with `prefix` selecting which `mount` calls
and the other left at a named never-answers stub. It costs a word per route and
no closure at all.

The manifest's `projections` key is not implemented: `std/http` knows its own
suffix. What the suffix does do is keep a projection out of the derived surface —
`rpc(dir)` skips a dotted stem now, so `pastes.http.vyrn` colocates with
`pastes.vyrn` without becoming a fourth procedure whose `routes()` returns
`Array<Route>` onto the wire.

## As landed — M2

Seven combinators in `std/http`, each `Route -> Route`: `cacheFor`, `etag`,
`lastModified`, `vary`, `status`, `createdAt`, `notFoundWhen`. `Route` gained
seven plain fields and no type parameter, so the property M1 protected survived
the milestone that was most likely to break it — the fortieth route with the
fortieth policy is still one nominal value to the checker.

**The policy is applied in `mount`**, which is the only place holding the route,
the request and the response at once. That is a design consequence, not a
convenience: it means no combinator captures anything, and the single stored
closure in a policy (`notFoundWhen`'s) is a capture-free predicate over a
`String`. It lowers on all three engines, which M1's codegen finding made worth
checking before designing rather than after.

**`Response` gained a `headers: Map<String, String>` field**, because it had
exactly one header channel — `vary` — and M2 needs four more. A field per header
does not scale; `vary` kept its own because RFC-0072 M4 already shipped it and one
negotiation channel with one reader beats two. Sixty construction sites across
std, examples and tests were swept, the same cost RFC-0072 M4 paid.

**The ETag is `FNV-1a-64(contentType + "\n" + body)` in hex, quoted.** It hashes
the representation — the bytes AND the media type that says how to read them,
since two `Vary`-selected variants of one URL must not share a validator — and it
hashes content and nothing else. No clock, no counter, no process identity: two
processes serving the same bytes emit the same tag, so a client's `If-None-Match`
still matches after a restart or across a load-balanced pair. A per-process seed
would have made the whole feature silently inert (always a 200, never a wrong
answer, never a failing test), which is why a test asserts the tag is identical in
a second process. At 64 bits a collision serves a 304 for content the client does
not have; it is the same hash that content-addresses this repo's pastes.

**`Cache-Control` is bare `max-age=N`, neither `public` nor `private`.** `public`
would tell a shared cache to store the one response it is otherwise forbidden to
store — the one for a request carrying `Authorization` (RFC 9111 §3.5) — which is
the cache-poisoning shape. `private` would make `cacheFor` useless for the public
API a projection exists to publish. The spec's default already refuses the
credentialed case, so the correct move was to add neither word.

**A 304 carries the validators and `Cache-Control`, no body, and no
`Content-Type`** — RFC 9110 §15.4.5 lists what a 304 sends and a media type is not
among them, so the host now omits the field entirely when the type is empty rather
than writing `Content-Type:` with nothing after it. `If-None-Match` takes
precedence over `If-Modified-Since` rather than being tried alongside it
(§13.1.3); getting that backwards would 304 a client whose validator we just said
does not match. `If-Modified-Since` is compared for exact equality with what we
would have stamped, not parsed as a date: a missed 304 costs bytes, a wrong one
costs correctness.

Four things are smaller or differently shaped than the RFC's text:

- **The chain reads outside-in: `etag(cacheFor(GET(byId("/{id}")), 3600))`.** The
  RFC's `.cacheFor(3600).etag()` needs a method call on a user value, which needs
  a protocol impl, and `impl P for Route` is refused outright — protocols
  implement for `Int64`/`Bool`/`String` or an enum, because records erase at
  runtime. This is the second spelling cost in the series (M1 paid the first) and,
  like the first, it is a spelling cost and not a structural one.
  **No longer true:** RFC-0084 removed the refusal (M1) and the native backend's
  variable-receiver rule (M2), and the seven combinators are now the methods of
  `Policy` — `GET(byId("/{id}")).cacheFor(3600).etag()`. Which is the whole of
  that RFC's argument: the cost was one engine's, not the design's.
- **`createdAt` takes a template, not a closure.** `createdAt(|p| "/pastes/\{p.id}")`
  takes the procedure's OUTPUT type, and a `Route` that could carry that closure
  would be `Route<T>` — precisely the type-level chain this RFC exists to refuse.
  `createdAt(POST(create("/")), "/pastes/{id}")` fills `{id}` from the created
  object's own field at runtime, unwrapping the `Ok` of a `Result`-returning
  procedure, and leaves an unknown `{name}` verbatim where it is loud.
  `notFoundWhen`'s closure survives because its argument is a `String`.
- **`lastModified` names a field.** Same erasure, same answer: `lastModified(r,
  "created")` reads a top-level epoch-millis field off the response the codec just
  wrote and formats the IMF-fixdate of RFC 9110 §5.6.7. A field that is absent,
  non-numeric or negative writes no header rather than trapping — a validator is
  an optimization and losing one must not lose the response.
- **`vyrn routes` still cannot see an explicit route, and M2 did not change
  that.** M1 said the table has one producer, the generator that emits `//@route`
  while mounting the derived surface, and this milestone confirms the diagnosis
  rather than fixing it: `http("./pastes")` runs before the hand-written
  projection exists as data, so the generator knows the base path and the
  procedure names but not the method, the sub-path or the policy — it could emit
  `? /pastes/? byId`, which is worse than nothing. What `derived` DOES carry now
  is the policy line M1 promised, appended by each combinator (`GET /notes/{id}
  byId max-age=60 etag`), and its reader is still the shadow diagnostic. Showing
  the table needs `vyrn routes` to run the mounted router, which is a separate
  change and not this milestone's.

`examples/rest.vyrn` is the three-engine evidence: interp, native and wasm print
the same tag, the same 304 and the same `Location`. `examples/bin` declares the
policy over real data — ten seconds on a listing that changes whenever anyone
posts, an hour on an immutable paste, `created` as its `Last-Modified`, and
`Err("no paste with id …")` as the one absence the resource has. `del` in the
fullstack demo now reports absence and refusal with different words, because a
projection reads them as two different answers.

One test was written and removed. A `vyrn serve` harness in `tests/http.rs` could
not read a response from the child it spawned on this host, while the identical
pattern in `tests/serve.rs` and `tests/rpc.rs` can — so the wire claims (the
header map on the socket, a 304 with nothing after the header block) are pinned in
`tests/serve.rs`, where the harness works, and the policy claims are pinned
in-process in `tests/http.rs`. The end-to-end round trip was verified by hand
against `vyrn serve examples/bin`: a `GET /pastes/<id>` answering 200 with
`Cache-Control`, `ETag` and `Last-Modified`, the same request with
`If-None-Match` answering 304 with an empty body and no `Content-Type`, and an
unknown id answering 404.

## As landed — M3b

`ws` is in `std/http` beside `sse`, `sha1` is in `std/hash`, and the milestone's
own question — does M3a's disconnect signal belong to one transport or to the
design — is answered by a diff: **`tests/serve.rs` has no deletions.** M3a's four
stream tests are untouched and the socket tests sit beside them, which is what
"the conformance file passes unchanged" was written to mean. The signal
generalised.

**`heartbeat` is refused, and the reason is better than the one predicted.**
This document guessed it would fail for `keepAlive`'s reason with one difference
worth checking — a ping is HOST-generated where a keep-alive comment would have
been producer-generated. That difference is real and it does not save the option:
between frames the host is not idle, it is blocked inside the producer waiting
for the next payload, so there is no moment for a timer to fire in. The sharper
half is what the check turned up on the way: **what a heartbeat is FOR is
detecting a peer that went away without saying so, and the host already learns
that by writing to it and failing.** So the option is not merely unimplementable
here; it is redundant with the signal this whole arc is built on, and it costs no
ping.

**SHA-1 mixes in masked `UInt64`, not `UInt32`.** Every word is masked to 32 bits
explicitly rather than left to a narrower type's overflow rule, so the three
engines agree without any of them being asked to round-trip an overflow the same
way. `examples/sha1.vyrn` is RFC 3174's own vectors as a parity citizen. The doc
comment says what the function is for — RFC 6455 §4.2.2's handshake nonce
transform, not a security primitive — because a `sha1` in a standard library
invites exactly the misuse that sentence prevents.

**One thing the design did not have: base64 could not take bytes.**
`base64EncodeV` took a `String`, and a twenty-byte digest is not text — it can
hold a NUL and need not be valid UTF-8, so it cannot make the trip through a
`String` first. `std/codecs` grew `base64EncodeBytes` and the string form now
calls it.

**Server-push, said out loud.** RFC 6455 is bidirectional and this is not; a
client→server message would want a handler per inbound message, which is a
different design. The host still parses inbound frames — enough to answer a
client-initiated close with a close frame, and to refuse an unmasked client frame
with 1002 per §5.1 — because ignoring the bytes a peer sends is not the same as
not supporting inbound messages.

## As landed — M3a

`sse` is in `std/http`, `serveStream` is the one builtin the milestone added, and
`examples/bin` has a live tail: `GET /pastes/live` with a `Last-Event-ID` streams
the pastes that client has not seen, one `id:`/`event:`/`data:` frame each, and
ends when it catches up. RFC-0075's last open conformance row is closed by
`tests/serve.rs`. Nothing about the buffered path changed — `ServeResponse` is
still a `body: String`, and the same `examples/bin` answers `GET /pastes` with a
`max-age`, an `ETag` and a 304 exactly as it did before.

**The signal is the write, and that turned out to cost less than the design
feared.** The pump is one loop in the host: pull a frame, write it, flush, and the
first time any of those fails, `Close` — which runs RFC-0075's release path. There
is no host-event subscription, no abort signal threaded through the interpreter,
and no per-deployment special case, because a failing `write_all` is the socket
rather than the framework. TCP supplies the backpressure for free — a slow client
fills the send buffer and the pump blocks in `write_all` — so the "pull-based, the
producer runs only when the consumer asks" property RFC-0075 argues for is the
socket's property here rather than a scheduler's.

**The discriminator is the open producer, not a field of the response.** `handle`
answers a `Response` in both cases; what makes one of them a stream is that a
producer is still parked in the interpreter behind it. So `ServeResponse` needed
no flag, no status convention and no content-type sniffing, and the second shape
lives where the RFC said it should — at the host boundary, as
`ServeAnswer::Buffered` beside `ServeAnswer::Live`. The host's question grew from
one to three (`Handle`, `Next`, `Close`), which is the only change to the serve API
and the only one the buffered path notices at all: it takes the same request and
returns the same struct.

The `Response` a live route hands back is a **header block plus a prologue**. Its
`body` is written once, after the header block and before the first frame, which
is exactly where SSE's `retry:` line belongs and is the only thing in a stream
that is not a frame. There is no `Content-Length` — the body ends when the
connection does — so `write_response_vary` could not have written it, which is
itself the argument for the second shape.

**The first frame is pulled before the header block goes out, and that is what
makes a completed stream terminate.** oRPC's documented caveat is that its
completion signal is non-standard, so plain `EventSource` clients reconnect
forever. The standard answer is `204 No Content`, the one status the WHATWG
algorithm reads as "stop, do not reconnect" — and a status has to be chosen before
any byte is written. So the pump asks the producer for one element first: `Some`
opens a 200 and streams, `None` answers 204 and the client stops. A feed that
drains therefore costs a reconnecting client exactly one more request, and
`examples/bin` demonstrates both halves against a real store.

**`keepAlive` is absent, and it is the milestone's one refusal.** The RFC lists
`retryAfter`, `keepAlive` and `resumable` as `sse`'s vocabulary. Two of them are
here. A pull producer answers `Some(v)` or `None`; it has no way to say "nothing
yet", and RFC-0075 states outright that this language adds no concurrency, so a
producer that blocked waiting for the next event would block the single-threaded
server rather than idling politely. There is no idle connection for a keep-alive
comment to hold open — which means `keepAlive` is not deferred work, it is an
option with nothing under it, and shipping it as a field that writes a `derived`
line and does nothing else would be exactly the meaningless-option-quietly-ignored
this RFC refuses. What it changes about its neighbour is worth stating plainly:
**`retryAfter` is the poll interval, not a failure hint.** A feed catches up, ends,
and the client comes back with its cursor.

**`resumable` needed nothing, as promised.** `Last-Event-ID` is parsed to an
`Int64` and handed to the producer as its seed; the id written beside each frame
is the cursor that produced it. In `examples/bin` the cursor is the storage
position and the store is append-only, so `id: 5` is the index the next connection
resumes at, and replay is `fromStep(since, tailStep)` with a different number in
it. Without `.resumable()` the seed is always `0`.

Four things are shaped differently from the design, and one of them is a finding
about the checker rather than about HTTP.

- **A `fn` type mentioning a stream was rejected, and should not have been.**
  `std/http`'s `Feed` is `fn(Request, Map<String, String>, Int64) -> Stream<String>`,
  and `contains_stream` — the walk behind RFC-0075 M1's "nothing may store a
  stream" — descended into `Type::Fn`, so every declaration mentioning `Feed`
  failed. It is wrong for a reason M1 already wrote down: a `fn` type's parameters
  and its return ARE the two positions M1 declares legal, so `fn(..) -> Stream<T>`
  stores no stream — it describes a call that produces one, and the caller owes it
  the moment it exists. The arm answers `false` now. Descending was safe only while
  nothing in the corpus had such a type, which is the same shape as M2's "M1 left
  `Stream` out of every generic walk" lesson: a new type variant's walks are wrong
  in whichever direction the milestone that added it had no use for.
- **`mount` grew a third parameter rather than a union.** "`mount` accepts both"
  has no other spelling: Vyrn has no sum over two record types, and wrapping the
  existing groups in an enum would have changed every call site anyway. So it is
  `mount(req, groups, live)`, five call sites in the repo, and `[]` where an app
  has no streams. Streams resolve BEFORE the buffered groups, because a stream's
  path is exact and `/pastes/{id}` is precisely the pattern that would swallow one;
  a stream that shadows a buffered route is the startup trap M1's check already
  had, worded for the stream.
- **`sse` takes its pattern directly, and this is not a weaker `GET(byId("/{id}"))`.**
  M1's placeholder check works by putting the pattern in the *procedure's own*
  parameter slot, so the generated `String where value =~ …` can be built from that
  procedure's input record. A feed takes the request rather than a decoded record,
  so there are no fields to check placeholders against — the rule is the same and
  there is nothing for it to do. `sse("/", tail)` is the RFC's own spelling, which
  it gets to keep for the reason M1 had to give it up.
- **The element is an encoded frame, `Stream<String>`, not `Stream<Event>`.** The
  natural design is a record stream that `std/http` maps into frames, and it is
  unbuildable today for a reason that belongs to RFC-0075: `map` and `filter` are
  eager walks that build a buffer, so mapping an endless feed would materialise it
  — `#6156` arriving through the library written to prevent it. The producer
  encodes as it goes with `event(id, name, data)`, which owns the whole of SSE's
  syntax including the rule that a newline in the payload becomes a second `data:`
  line rather than ending the event. When the combinators become lazy,
  `Stream<Event>` is a one-line change on this side.

**A compiled build traps.** `serveStream` reaches the host's accept loop and a
native or wasm binary has none, so both backends emit the same runtime trap from
one shared constant. It is a runtime trap rather than a compile error because
`mount` reaches that arm whether or not a program mounts a live route, and
refusing at compile time would make every REST projection unbuildable to serve a
feature it does not use — `examples/rest.vyrn` builds and runs on all three
engines with the trap emitted and unreached. The consequence for evidence is
stated rather than hidden: **there can be no three-way parity example for `sse`**,
since the interpreter serves it and the other two engines are required to trap,
and an example whose engines are supposed to differ is not a parity example. The
evidence is `tests/serve.rs` (the wire and the disconnect), `tests/http.rs` (the
values and the mount order), and `examples/bin` (the live tail against a real
store). Parity is 105 checked, 7 skipped, 0 failed — unchanged.

**The disconnect pin, and what it actually asserts.** RFC-0075's row is "client
disconnects mid-stream → producer release runs", and this RFC promised the
stronger "release runs before the next event would be produced". The test opens
`/live` against an endless producer, reads two frames, drops the socket, and then
asserts two different things, because "production stopped" and "the release ran"
are two claims and only one of them is the row:

- `/steps` reports how many times the step function ran. It is read once after the
  drop — that request blocks until the pump notices, so its answer is already
  final — and again 300 ms later, and the two must be equal. That is the row's own
  wording: no further element was produced.
- `/probe` reads the stream's cursor cell through a `Ref` the step parked in module
  state. `close` releases that cell and bumps its generation, so the read is the
  canonical `reference used after release` trap and the route answers 500. A stuck
  pump would also stop producing; only a release can invalidate the cell.

A third test opens and abandons 200 streams and then asks the server for
something, which is `#6156` at transport scale: the cursor cells come from a slab
of 65536, so a release that did not run would eventually trap rather than merely
grow. The pin is written below `std/http` on purpose — it calls `serveStream` and
`fromStep` directly — so it is about the mechanism rather than about the
projection that spells it, which is what `ws` will have to pass in M3b.

Two smaller things, so they are not mistaken for coverage. **A stream monopolises
the sequential server while it drains** — `serve_one` handles one connection at a
time, which is RFC-0013's host-owns-the-loop arrangement and not new, but it is
newly visible: a feed that ended is the thing that lets the next request in, and
`--workers` is the answer for an app that needs more. And the dogfood calls
`fromStep` where it wanted `std/stream`'s `unfold`, because **`std/arrays` and
`std/stream` both export `map` and `filter`** and top-level names are unique across
a linked program, so no program can import both. That is a std naming collision
this milestone found rather than caused; `unfold` is a three-line wrapper over the
same call, so the dogfood loses the better name and nothing else.

## Acceptance

- `examples/bin` serves both the derived RPC surface and a public REST API, with
  the REST projection under 15 lines.
- A GraphQL query selecting only `id` does not run the `body` resolver — proven
  by a resolver-call counter in the test suite.
- `{ID}` in a route pattern is a compile error listing the input type's fields.
- Adding 100 routes changes checker time linearly and by a measurable-but-small
  constant — pinned as a benchmark under RFC-0063's CI benchmark job.
- `If-None-Match` against an `etag()` route returns 304 with an empty body.
- `vyrn routes` shows cache and validator policy for every explicit route.
- Three-way parity green; REST and RPC responses byte-identical across
  interp/native/wasm.
