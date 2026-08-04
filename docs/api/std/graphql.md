# std/graphql

std/graphql — the contract as a GraphQL SDL document (RFC-0038), a library
entirely on RFC-0021 generator imports. One `gen fn`, `sdl(contract)`,
reflects the contract with `moduleInterface` and RETURNS a synthesized module
exporting `sdlText() -> String`. The compiler knows nothing about GraphQL:
everything below is comptime-pure Vyrn string building over the reflected
type SOURCES and procedure signatures — the SDL is BAKED as a deterministic
constant at generation time (no runtime calls).

  import { sdl } from "std/graphql"
  import { sdlText } from sdl("./contract")
  // sdlText() -> String : a deterministic GraphQL SDL document

Mapping rules (documented and DUMB on purpose — RFC-0038):
  - a wire RECORD becomes a `type`/`input` PAIR (`Book` and `BookInput`);
    GraphQL separates output objects from input objects, and both derive from
    one Vyrn record. A record field's type maps by the scalar table below; a
    non-`Option` field is non-null (`!`).
  - SCALARS map honestly: `Int64` and the sized ints => `Int`, `Float64`/
    `Float32` => `Float`, `String` => `String`, `Bool` => `Boolean`.
  - a VALIDATED scalar (e.g. `BookId = Int64 where value >= 0`) becomes a
    named custom `scalar` whose description documents its base and constraint
    (the `@constraint`-style doc comment). Fields keep the name, so the type
    graph stays legible and the constraint is documented once.
  - `Map<String, V>` has no SDL type, so it maps to a documented custom
    `scalar JSON` (a JSON object on the wire); a NAMED map alias becomes its
    own documented `scalar`.
  - a PAYLOAD enum (any variant carries data — including `Result<A, B>` and
    `Circle(Int64)`) maps to a "tagged" object `type` with one NULLABLE field
    per variant (nullary => `Boolean`, single payload => that type, multi
    payload => `JSON`): exactly one field is non-null at a time. A nullary-only
    enum maps to a real GraphQL `enum`.
  - PROCEDURES become `Query`/`Mutation` fields: a `mut fn` is a Mutation,
    everything else a Query. The split is the author's DECLARATION, read off
    `FnInfo.mutates` (RFC-0074 M4a) — nothing here guesses from a name, so
    renaming a procedure cannot silently move it between roots. A
    1-parameter procedure takes `(input: <Req>Input)`; the return maps by the
    table above. An empty `Query` gets a `_placeholder` field (GraphQL needs a
    non-empty query root); an empty `Mutation` is omitted.
  - `///` docs on TYPES become SDL descriptions. Procedure/param/module docs
    are NOT in `moduleInterface` reflection, so operation descriptions are
    absent (gap recorded in RFC-0038).

The EXECUTOR (RFC-0085 M1) lives at the bottom of this file: `graphqlServer`
emits a `graphqlHandle(req) -> Option<Response>` answering `POST /graphql`
over the SAME `iface.functions` walk the SDL reads, so a field the schema
declares and a field the executor dispatches are one list.

Inspect the synthesized module with:  vyrn emit-gen <file>

## gqlRootType

```vyrn
fn gqlRootType(root: String) -> String
```

The GraphQL root TYPE name for an operation type (`query` => `Query`).

## sdl

```vyrn
fn sdl(contract: String) -> String
```

`sdl(contract)` — emit a module exporting `sdlText() -> String`, a GraphQL SDL
document for the contract. The document is baked as a deterministic constant.

## GqlSel

```vyrn
type GqlSel = { name: String, subs: Array<GqlSel> }
```

One selected field: its name and whatever it selects in turn.

No alias and no arguments — both are RFC-0085 M2, and the parser REFUSES them
by name rather than dropping them, so a query that means something this
milestone cannot do is never answered as if it meant something else.

## GqlQuery

```vyrn
type GqlQuery = { root: String, sels: Array<GqlSel>, err: String }
```

A parsed operation: its root (`"query"` / `"mutation"`), its selected fields,
and the reason it would not parse (`err != ""`).

## gqlParseQuery

```vyrn
fn gqlParseQuery(src: String) -> GqlQuery
```

Parse one executable operation: an optional `query`/`mutation` keyword and
operation name, then a selection set. Anything past it is a second operation,
which needs `operationName` to choose between — RFC-0085 M2.

## gqlProject

```vyrn
fn gqlProject(v: Json, sels: Array<GqlSel>) -> Json
```

Project `sels` out of `v`.

**Field selection is a projection over a value the procedure ALREADY
returned, not a fetch plan.** `v` arrives here fully computed — every field of
it, selected or not, was produced by the procedure and encoded before this
function saw it. So at a leaf this OMITS; it does not avoid computing. Nothing
below is lazy and a reader must not read it as if it were.

That distinction is the one RFC-0074 M4b's `.lazy(field, resolver)` later has
to break: a lazy field is the single case where omission must ALSO mean the
work was never done. Stated here because if it is only stated then, `.lazy(..)`
is an optimisation nobody can observe and there is nothing to point at.

An empty selection set means "this field is a leaf" — take the value whole.

## gqlQueryText

```vyrn
fn gqlQueryText(body: String) -> String
```

The query text of a request body: the `query` member of a GraphQL-over-HTTP
JSON envelope, or — when the body is not such an envelope — the body itself,
which is what an `application/graphql` request sends. A GraphQL document is
not valid JSON, so the two cannot be confused.

## gqlDataBody

```vyrn
fn gqlDataBody(field: String, value: Json) -> String
```

`{"data":{"<field>":…}}` — the answer.

## gqlErrorBody

```vyrn
fn gqlErrorBody(message: String) -> String
```

`{"errors":[{"message":"…"}]}` — an error a GraphQL client can read.

A GraphQL client reads the `errors` array, not the status line, so the reply
stays a 200 `application/json`. Path attribution and partial `data` beside a
populated `errors` are RFC-0085 M3.

## gqlAnswer

```vyrn
fn gqlAnswer(body: String, resolve: fn(String, String) -> Result<Json, String>) -> Response
```

Answer one GraphQL request body against `resolve`, the generated resolver
table (`(root, field) -> the encoded value, or why not`).

One root field per request: a second one immediately raises partial `data`
beside a populated `errors` when only one of them resolves, which is the whole
of RFC-0085 M3 and is not worth half-building here.

## graphqlServer

```vyrn
fn graphqlServer(contract: String) -> String
```

`graphqlServer(contract)` — emit a module exposing
`graphqlHandle(req: Request) -> Option<Response>`, answering `POST /graphql`
against the same contract `sdl(contract)` documents (RFC-0085 M1).

It mounts beside `rpcHandle` and `connectHandle` on one `serve` root: a
GraphQL request is an ordinary route over the same procedures and the same
`toJson`, not a second wire. The generator is `graphqlServer` (not
`graphqlHandle`) for the reason `std/rpc` and `std/connect` already record — a
`gen fn` and the module it emits are both linked, and the flat namespace
forbids the clash.
