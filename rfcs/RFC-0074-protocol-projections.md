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
- **M3 — streaming projections.** `sse` and `ws` over RFC-0075 streams.
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
