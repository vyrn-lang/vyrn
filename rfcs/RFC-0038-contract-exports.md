# RFC-0038 — Contract Exports: Connect Wire Compat, OpenAPI, and GraphQL SDL

- **Status:** Draft (design locked)
- **Depends on:** RFC-0019 (`std/rpc` — the library thesis under test),
  RFC-0021 (`moduleInterface`), RFC-0031 (the reachable type closure —
  exports must describe imported wire types too), RFC-0003 (`jsonSchema` —
  the schema source of truth), RFC-0037 (stored closures — noted for the
  deferred GraphQL executor)
- **Evidence:** the user's founding question for this arc was "is there
  space for gRPC, GraphQL and so on?" and the answer given was "yes,
  because RPC is a library over general primitives." That claim has never
  been tested against a protocol we didn't design. This RFC tests it
  three ways with zero compiler changes — the acceptance criterion is
  that `std/rpc` needs no modification and the compiler needs none.

---

## Scope honesty first

- **Connect wire compatibility** means the [Connect protocol's unary
  JSON flavor: `POST /<Service>/<Procedure>`, `content-type:
  application/json`, spec-shaped `{"code": "...", "message": "..."}`
  errors] — chosen because it is gRPC-family semantics carried over
  plain HTTP/1 + JSON, which the `serve` runtime already speaks. True
  gRPC (HTTP/2 framing, protobuf) is explicitly out: no protobuf codec
  exists and `serve` has no HTTP/2 — that is a runtime project, not a
  generator, and pretending otherwise would be the crutch this project
  keeps refusing.
- **OpenAPI and GraphQL SDL are documents, not runtimes.** They export
  the contract's shape for the *rest of the world's* tooling (codegen,
  Postman/Insomnia, GraphQL clients' introspection-adjacent workflows).
  A GraphQL *executor* is deferred — newly feasible now that resolvers
  can be stored function values (RFC-0037), but it should be demanded by
  a real consumer, not built speculatively.

## The three generators (each its own small std file, prefixed internals)

1. **`std/connect` — `connectHandle(contract)`** (and
   `connectClient(contract)` for symmetric in-Vyrn callers): a synthesized
   module whose `connectHandle(req: Request) -> Option<Response>` mounts
   beside `rpcHandle`:
   - Routes `POST /<contractName>.<Proc>` (Connect's service/method path
     shape; the service name derives from the contract module name).
   - Request body decodes with the SAME `fromJson` + validation as
     `std/rpc`; success = 200 JSON; validation failure = HTTP 400 with
     Connect's error JSON, `code: "invalid_argument"`, and the Issues
     mapped into `details` (lossless — key/path/message preserved);
     unknown procedure = `code: "unimplemented"` per spec; `Err` returns
     of `Result` procedures = 200 with the ordinary encoded value (the
     RFC-0024 decision — domain errors are values, transport errors are
     Connect errors).
   - The point being proven: the SAME contract serves `/rpc/*` and
     Connect paths simultaneously — two protocols, one module, no
     compiler.
2. **`std/openapi` — `openapi(contract)`**: exports
   `openapiJson() -> String` — an OpenAPI 3.1 document baked as a
   deterministic constant (the `css()` precedent): one `paths` entry per
   procedure (request body schema from `ParamInfo.schema`, response from
   `retSchema`, plus the 422 Issues shape), `components/schemas` from the
   RFC-0031 type closure so imported wire types appear, `info` from the
   contract's `///` docs where present. Byte-stable ordering (procedure
   declaration order; schema keys sorted) — pinned by golden.
3. **`std/graphql` — `sdl(contract)`**: exports `sdlText() -> String` —
   a GraphQL SDL document: wire records → `type`/`input` pairs (GraphQL
   separates them; both derive from one Vyrn record), procedures →
   `Query`/`Mutation` fields (read-only-named procedures — `get*`/
   `list*` — become Query, the rest Mutation; the split rule is
   documented and dumb on purpose), scalars mapped honestly
   (`Int64 → Int`, `Float64 → Float`, `String`, `Bool`; validated types
   as their base + a `@constraint`-style doc comment; `Map<String, V>` →
   a documented JSON scalar — SDL has no map type and inventing one
   would lie), payload enums → `union`/tagged conventions documented in
   the file header. `///` docs → SDL descriptions.

## Guardrails (locked)

- **Zero compiler changes; `std/rpc` unmodified.** Any needed
  information must already flow from `moduleInterface`/`jsonSchema` — if
  it doesn't, that gap report is a primary deliverable of this RFC, not
  something to hack around.
- All three outputs deterministic and golden-pinned (`vyrn emit-gen` +
  document-content goldens); documents validate against their specs'
  basic well-formedness (OpenAPI: parseable JSON with required keys;
  SDL: parseable by graphql-js grammar rules — a fixture check in the
  test, not a new dependency).
- Composition hygiene: `connect`/`oa`/`gql` internal prefixes (the
  std-generator convention).

## Consumers / proof

- **shelf** serves `/openapi.json` and `/schema.graphql` from the two
  document generators, and mounts `connectHandle` beside `rpcHandle` —
  then a Connect-shaped round-trip is verified end to end (curl-style
  requests against `vyrn dev`: success, validation failure with
  `invalid_argument` + details, unknown procedure → `unimplemented`).
- The OpenAPI document is loaded into a validator fixture (well-formed
  3.1); the SDL parses under standard grammar.
- `examples/rpcsplit` (the in-process parity citizen) gains nothing —
  these are serve-surface features; parity coverage comes from the
  generators' own emit-gen goldens.

## Out of scope

True gRPC (HTTP/2 + protobuf — a runtime project), a GraphQL executor
(deferred; resolvers-as-stored-closures makes it feasible when demanded),
streaming of any kind, Connect's GET/query-param flavor and compression,
protobuf `.proto` emission (belongs with a protobuf codec, if ever),
OpenAPI callbacks/webhooks/auth schemes.
