# RFC-0019 — Typed Procedures: RPC, the Fullstack Build, and Transports

- **Status:** Core implemented (procedures, HTTP mounts, in-process dispatch,
  role-based fullstack build); `velac dev` + the browser runtimes
  (`vela-rpc.js`/`vela-query.js`) and the `examples/fullstack/` demo pending.
- **Depends on:** RFC-0018 (JSON codec), RFC-0016 (`velac serve`), RFC-0012/13
  (extern + host-owns-the-loop), RFC-0010 (modules/manifest)

> **Motivation.** The long-range pitch — same language on server and client,
> validated types as the wire contract — becomes a *product* here: define a
> procedure once, call it from the browser with full type safety, get
> accumulated validation errors on bad input for free. What oRPC/tRPC do with
> type inference across a codebase boundary, Vela does by **sharing the
> module**: the contract is code, not a schema file.

---

## Procedures

```vela
// api.vela — imported by BOTH builds
export type GetUserReq = { id: Int64 }
export type User = { name: Username, age: Age }

rpc fn getUser(req: GetUserReq) -> User {
    ...                                    // ordinary body; may import server logic
}
```

- **`rpc fn`** — a contextual modifier (the `extern` precedent). Body
  required. Otherwise an ordinary function: callable, testable, analyzable.
- Zero or one parameter; parameter and return types must be **codable**
  (RFC-0018; checker enforces, naming the offender). Return may be `Unit`.
- Procedure names are the wire names; they share the top-level namespace.
- **`rpc` implies `export`.** A procedure is inherently public — the client
  must import its name for the typed `rpc()` call — so it is always
  module-importable. Writing `export rpc fn` is a redundant-export error
  (`redundant `export`: `rpc fn` procedures are always exported`) — one way to
  spell it, the canonical-style ethos.

## Transports (v1) and the seams (locked now, cheap forever)

The contract layer is `(procedure name, request bytes) → (status, response
bytes)`. Three transports ship; everything else is an adapter later:

1. **In-process** — calling the function. This is SSR's transport and the
   test story's: zero serialization, always available on server-role builds.
2. **HTTP** — `velac serve` mounts `POST /rpc/<name>`: decode via
   `fromJson` → `Invalid` ⇒ **422** with `{"issues":[...]}` (the `Issue`
   array, toJson'd) → call → `toJson` ⇒ 200 `application/json`. A `Unit`
   return ⇒ **204** (no body). Unknown procedure ⇒ 404. `Content-Type` other
   than `application/json` ⇒ 415 — **the codec seam**: negotiated by content
   type, JSON is simply the only v1 codec. A trap in a procedure ⇒ logged 500
   (server survives, RFC-0016).

   **The mount surface is root-visible procedures only** (a security rule).
   `velac serve` (and later `dev`) mounts exactly the procedures visible in
   the SERVER ROOT module — declared in the root file, or imported BY NAME
   into it. The root's import list IS the route table: explicit and greppable.
   A procedure reachable only through a transitively-imported third-party
   module is NOT an endpoint (a 404, indistinguishable from a name that does
   not exist, so a dependency cannot add routes or be probed for hidden ones).
   `GET /rpc/$schema` lists exactly this mounted surface. In-process `rpc()`
   dispatch still works for ANY procedure in the linked program (it is just a
   call) — only the WIRE surface is root-scoped.
3. **Browser** — the `rpc()` builtin below, via `web/vela-rpc.js` (fetch).

**Contract endpoint:** `GET /rpc/$schema` returns
`{"procedures":[{"name","request":<jsonSchema>,"response":<jsonSchema>}]}` —
a protocol-neutral registry that any emitter (OpenAPI, `.proto`, SDL) can
feed on later.

## The client call: `rpc()` + the `onRpc` convention

```vela
// client.vela
import { getUser, GetUserReq, User } from "./api"

fn refresh(id: Int64) {
    let reqId = rpc(getUser, GetUserReq { id: id })   // typed: req checked against getUser
}

export extern fn onRpc(id: Int64, status: Int64, body: String) {
    if status == 200 {
        match fromJson(User, body) { Valid(u) => ..., Invalid(is) => ... }
    }
}
```

- **`rpc(procedure, req) -> Int64`** — the procedure is named as an
  *identifier* (no function values needed); the checker verifies it is an
  `rpc fn` and that `req` matches its parameter. Returns a request id.
- **Server role / interp / native:** `rpc()` dispatches **in-process,
  synchronously** — encode, call, decode, invoke `onRpc` before returning.
  Deterministic; client logic is testable against real procedures with zero
  mocks; parity holds three-way.
- **Client role (browser wasm):** routed through `vela-rpc.js` — a real
  fetch; `onRpc` fires on completion (host owns the loop). CPS-by-convention
  is the accepted v1 cost of the async decision (RFC-0016).

## The fullstack build

```
vela.json   { "name": "app", "server": "server.vela", "client": "client.vela" }
velac build --server   -> native server binary
velac build --client   -> browser wasm (client ROLE)
velac dev              -> everything on :8080
```

- **Role-based lowering, one shared contract module.** Both roles type-check
  `rpc fn` bodies (identical diagnostics — the contract cannot rot silently).
  Server role lowers them fully. **Client role** lowers each `rpc fn` as a
  remote stub: a *direct* call is a checker error ("call `getUser` through
  `rpc()` on the client"); only signatures ship. Dead server code in the
  client artifact is pruned by wasm-ld's linker GC (documented caveat:
  secrets belong in files/env — which do not exist client-side — not in
  literals; v1 relies on GC, not a guarantee).
- `run`/`test`/parity always use **server role** — bodies present, `rpc()`
  in-process, fully deterministic.
- **`velac dev`**: builds the client wasm, then serves — `/rpc/*`
  (procedures), `public/` + the client module (static), everything else →
  `handle()` if defined. Interp server; rebuild-on-change is v2.

## The query layer (the "colada")

`web/vela-query.js` — a zero-dependency host runtime (sibling of
`wasi-min.js`/`vela-rpc.js`) owning cache **policy**: keys =
`(procedure, requestJson)`, in-flight dedupe, `staleTime`, refetch,
`invalidate(key)`. Vela owns the **truth**: decoded, validated results land
in module state through exported handlers. The split is deliberate (host
owns timing, Vela owns state — RFC-0013), and honest: a fully Vela-native
cache wants **function values/closures and library-owned state
(`export let`)** — this direction is precisely what should motivate those
RFCs, and real usage of v1 is the evidence-gathering.

## Protocol roadmap (space reserved, nothing built)

- **gRPC via the Connect protocol** — unary gRPC semantics over plain
  HTTP/1.1 POST (JSON or protobuf bodies): ecosystem interop without
  hand-rolling HTTP/2. Protobuf requires **stable field numbers** — records
  crossing a protobuf boundary will need an explicit numbering annotation
  (design reserved, decided when built). `.proto` emit/import follows the
  jsonSchema round-trip precedent.
- **GraphQL — the boundary stated:** SDL emission from Vela types is cheap
  (the reflection exists); a shallow adapter exposing procedures as
  `Query`/`Mutation` fields is plausible; nested field resolvers need
  function values and an executor — out, deliberately, and somewhat contrary
  to Vela's closed-contract bet.
- **JSON-RPC 2.0** — a trivial envelope adapter (which also makes a Vela
  server MCP-servable). **SSE** for server-streaming before WebSockets;
  both gated on the streaming/async revisit triggers. **Queues/postMessage**
  — the contract layer already fits.

## Deliverables & demo

`examples/fullstack/` — `vela.json` (server/client roots), `api.vela`
(types + procedures with validated fields), `server.vela`, `client.vela`,
`public/index.html`. `velac dev` runs it; browser-verified: a typed round
trip, a 422 whose `Issue`s render in the page, and the query cache
deduping/invalidating visibly. Integration tests drive `/rpc/*` (200, 422
with exact Issue JSON, 404, 415) and the client role's checker errors;
`velac test` covers procedures in-process.

## Out of scope

Streaming procedures, non-JSON codecs (seam only), auth/middleware, HMR,
per-procedure config, GraphQL execution, HTTP/2.
