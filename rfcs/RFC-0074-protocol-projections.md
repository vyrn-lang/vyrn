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
