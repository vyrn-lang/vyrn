# RFC-0038 — Contract Exports: Connect Wire Compat, OpenAPI, and GraphQL SDL

- **Status:** Implemented — `std/connect`, `std/openapi`, `std/graphql` (three
  pure generator libraries; zero compiler changes, `std/rpc` byte-identical). See
  "Implementation notes (as landed)" at the end.
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

---

## Implementation notes (as landed)

Three std files, each self-contained with a prefixed vocabulary
(`connect*`/`oa*`/`gql*`), built exactly as `std/rpc` is: `gen fn`s over
`moduleInterface` reflection, emitting Vyrn source. **The acceptance criterion
held: zero compiler changes, and `std/rpc.vyrn` is byte-identical.** The one
compiler diff in the landing branch is the unrelated `fmt.rs` chip.

### The thesis result — RPC-as-library held (with reflection-ergonomics caveats)

All three protocols we did **not** design were realized as pure generator
libraries. The founding question ("is there space for gRPC, GraphQL and so on?
— yes, because RPC is a library over general primitives") survives its first
test against foreign protocols. It held not because reflection was rich enough
to bake everything, but because the two things reflection *doesn't* expose were
both reachable **without touching the compiler** — via the same runtime-emission
and source-parsing moves `std/rpc` already relies on. The precise gaps, recorded
as the RFC asked (a primary deliverable, not something hacked around):

1. **`moduleInterface` exposes no procedure/param/module `///` docs.** `FnInfo`
   is `{name, params, ret, retSchema}` and `ParamInfo` is `{name, spelling,
   schema}` — neither carries a doc, and there is no module-level doc field.
   Only **type** docs are reflected (`TypeInfo.schema.doc`, via `schemaOf`).
   Consequence: OpenAPI `info`/operation `summary` and GraphQL operation
   descriptions cannot come from procedure or module `///` docs. OpenAPI `info`
   is derived from the contract's module **base name** + a fixed `version`;
   GraphQL/OpenAPI **type** descriptions DO come through (from `TypeInfo.schema.doc`).
   *If ever closed:* add `doc: Option<String>` to `FnInfo` (and a module doc to
   `ModuleInterface`). It would stand alone and is a small, general improvement —
   but it is a compiler change, so it was **not** made here (the whole point was
   to need none).

2. **The reflected `Schema` is shallow — a scalar's bounds, never a recursive
   shape.** `Schema` is `{name, base, doc, min, max, multipleOf, minLength,
   maxLength, pattern}`; a record's fields and an enum's variants are not in it.
   So a fully-baked OpenAPI (the `css()` ideal the draft named) is **not**
   possible from reflection alone. Realization: OpenAPI bakes the document
   **envelope** (`openapi`/`info`/`paths`/the `$ref`s/the 422 shape) as a
   constant, but the **schema bodies** in `components/schemas` are emitted as
   runtime `jsonSchema(T)` calls — the exact `rpcSchema()` precedent `std/rpc`
   uses for `GET /rpc/$schema`. Each `jsonSchema()` is itself a compile-time
   constant string, so the whole document is still deterministic and byte-stable
   (verified: generate twice, byte-equal). Each component is wrapped with an
   injected `$id` so its self-contained `#/$defs/..` back-references resolve
   inside that component (JSON Schema 2020-12 `$id` base-URI scoping); path
   entries `$ref` the components by `#/components/schemas/<T>`. Component keys are
   sorted; paths follow procedure declaration order.

3. **GraphQL parses `TypeInfo.source`.** For the same shallow-`Schema` reason,
   the SDL generator recovers record fields / enum variants / alias bases by
   parsing the canonical `TypeInfo.source` text (a small `gql*` byte scanner,
   the `std/tw` JSON-reader pattern). The SDL is then fully **baked** (no runtime
   calls). This is a workaround for the same gap as (2): reflection hands a
   generator a type's *source text* and *shallow schema*, but no structured
   field/variant list.

Net: the gaps are **reflection ergonomics**, not expressive walls. Every one was
worked around inside the library layer with no compiler change, which is exactly
the claim under test.

### `std/connect`

- Generator is **`connectServer(contract)`**, emitting the router
  **`connectHandle(req: Request) -> Option<Response>`** — deliberately different
  names. The draft named both `connectHandle`, which is **unrealizable**: a
  `gen fn` and the module it generates are BOTH linked into the program, and the
  flat namespace forbids two top-level `connectHandle`s. This mirrors the
  `std/rpc` precedent exactly (`rpcServer` emits `rpcHandle`); the mounted router
  keeps the RFC name (`connectHandle`, beside `rpcHandle`). `connectClient` is
  unaffected (its stubs are procedure-named).
- Service name = the contract specifier's base name (`"./contract"` → `contract`),
  so paths are `POST /<base>.<Proc>`. Wire semantics as locked: 200 =
  `toJson(result)`; validation failure = **HTTP 400** `{"code":"invalid_argument",
  "message":"request validation failed","details":[<Issues lossless>]}` (details
  built with `toJson` over `Array<Issue>`, so key/path/message are preserved);
  unknown procedure under the prefix = **HTTP 404** `{"code":"unimplemented"}`
  (Connect's canonical status for that code); a `Result` `Err` = 200 with the
  ordinary encoding (RFC-0024). A `Unit` return = 200 `{}`.
- Verified end to end over `vyrn serve` (real HTTP): success 200, validation
  failure 400 `invalid_argument` + lossless `details`, unknown proc 404
  `unimplemented`. The SAME contract simultaneously answered `/rpc/*` (via
  `rpcHandle`) and `/<base>.<Proc>` (via `connectHandle`) — the point proven.

### `std/openapi`

Emits `openapiJson() -> String`, an OpenAPI 3.1 document describing the
**`POST /rpc/*`** surface (chosen because the draft calls for the `422` Issues
shape, which is `std/rpc`'s validation status; Connect's is `400`). Determinism
and well-formedness are covered by an integration test that parses the output
with the compiler's own JSON parser (no new dependency) and asserts the 3.1
required keys, sorted `components/schemas`, resolvable `$ref`s, a validated
scalar's bound, a `Result` `oneOf`, a `Map` `additionalProperties`, and the
`$id` scoping — then generates twice for byte-equality.

### `std/graphql`

Emits `sdlText() -> String`. Mapping decisions where the draft left latitude
(documented in the file header, "dumb on purpose"):

- a record → a **`type`/`input` pair** (`Book` + `BookInput`); a non-`Option`
  field is non-null (`!`), `Array<T>` → `[<T>!]!`, `Option<T>` drops the outer
  `!`. Input-position references to record types get the `Input` suffix.
- honest scalars: `Int64`/sized ints → `Int`, `Float64`/`Float32` → `Float`,
  `String`, `Bool` → `Boolean`.
- a **validated scalar** → a named custom `scalar` whose description carries the
  `///` doc AND the base + constraint (`"""A user id (positive). — Int64 where
  value >= 1"""`), so it is documented "as its base + a `@constraint`-style doc
  comment" while keeping its name in the type graph.
- `Map<String, V>` → a documented `scalar JSON`; a **named** map alias becomes
  its own documented `scalar` (keeping the name).
- a **payload enum** (any variant carries data — including `Result<A, B>`) → a
  "tagged" object `type` with one **nullable** field per variant (nullary →
  `Boolean`, single payload → that type, multi-payload → `JSON`); a **nullary-only**
  enum → a real GraphQL `enum`.
- procedures → `Query` (`get*`/`list*`) else `Mutation`; a 1-param procedure
  takes `(input: <Req>Input)`; a `Unit` return → `Boolean`; an empty `Query`
  gets a `_placeholder` field (GraphQL needs a non-empty query root); an empty
  `Mutation` is omitted.

Well-formedness is checked by a dependency-free grammar sanity test (balanced
brackets + every block opener is a `type|input|enum Name {` header + `scalar`
lines are two tokens), plus a determinism byte-equality check.

### shelf

`examples/shelf/server.vyrn` mounts `connectHandle` beside `rpcHandle` and serves
`/openapi.json` (`application/json`) and `/schema.graphql` (`application/graphql`).
All prior shelf flows (home SSR, book routes, `/theme.css`, `/rpc/*`, the RFC-0037
middleware guard) remain green. Test/parity: 880 workspace + 11 LSP green, full
three-way parity green, `vyrn fmt --check` clean over `examples/` + `std/`.
