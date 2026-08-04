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

The EXECUTOR (RFC-0085 M1/M2) lives at the bottom of this file:
`graphqlServer` emits a `graphqlHandle(req) -> Option<Response>` answering
`POST /graphql` over the SAME `iface.functions` walk the SDL reads, so a field
the schema declares and a field the executor dispatches are one list. It bakes
the SDL's TYPE GRAPH beside the resolver table, out of the same `gqlMembers`
reading of each declaration the `type` definitions above are emitted from —
so an object type's fields are one set, not two that agree.

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
type GqlSel = { key: String, name: String, args: Array<JsonField>, subs: Array<GqlSel> }
```

One selected field: the RESPONSE KEY the answer arrives under (the alias when
one was written, else the field name), the field it names, its arguments as
the value tree `fromJson` reads, and whatever it selects in turn.

The key is stored rather than the alias, because every consumer wants the key
and nothing wants to ask whether one was written.

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
fn gqlProject(v: Json, sels: Array<GqlSel>, ty: String, schema: fn(String, String) -> String) -> Result<Json, String>
```

Project `sels` out of `v`, whose named GraphQL type is `ty`.

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
A list is walked transparently: `[Book]` and `Book` ask `schema` the same
question, which is why only the NAMED type is carried.

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

## GqlArg

```vyrn
type GqlArg = { json: String, err: String }
```

One argument's value as the JSON text `fromJson` reads, or why it is not
there (`err != ""`).

## gqlArgOf

```vyrn
fn gqlArgOf(field: String, args: Array<JsonField>, want: String) -> GqlArg
```

The `want` argument of `field`, emitted as the JSON text a procedure's input
record is decoded from.

**The same decode path the RPC surface uses.** `std/rpc`'s generated handler
runs a POSTed body through `fromJson(<ReqType>, body)`; the arm this feeds
runs `fromJson(<ReqType>, ..)` over the text this returns. An argument and a
request body reach the same record by one route and are validated by one rule,
so a `Title` too long is refused identically over both wires — there is no
second decoder here to be lenient where the first is strict.

The SDL spells a procedure's single argument `input` (`gqlRootField`), so an
argument by any other name is one the field does not declare.

## gqlNoArgs

```vyrn
fn gqlNoArgs(field: String, args: Array<JsonField>) -> String
```

"" when `field` takes no arguments and got none, else the GraphQL error
naming the first one it does not declare.

## gqlArgError

```vyrn
fn gqlArgError(field: String, issues: Array<Issue>) -> String
```

The GraphQL error for an argument the input record refuses: the accumulated
`Issue`s `fromJson` produced (RFC-0009) — the same ones a 422 carries over
RPC, since it is the same decode.

## gqlAnswer

```vyrn
fn gqlAnswer(body: String, resolve: fn(String, String, Array<JsonField>) -> Result<Json, String>, schema: fn(String, String) -> String) -> Response
```

Answer one GraphQL request body against the two tables the generator bakes
from the contract's reflection: `resolve` (`(root, field, args) -> the encoded
value, or why not`) and `schema`, the type graph the projection is checked
against.

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
