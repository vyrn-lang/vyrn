# std/graphql

std/graphql -- the contract as a GraphQL SDL document, built on
generator imports. `sdl(contract)` returns a module exporting
`sdlText() -> String`, baked at generation time from the reflected type
sources and procedure signatures.

  import { sdl } from "std/graphql"
  import { sdlText } from sdl("./contract")

Mapping rules:
  - a record becomes a `type` and an `input` twin (`Book`, `BookInput`); a
    non-`Option` field is non-null (`!`).
  - `Int64` and the sized ints => `Int`, `Float64`/`Float32` => `Float`,
    `String` => `String`, `Bool` => `Boolean`.
  - a validated scalar (`BookId = Int64 where value >= 0`) becomes a named
    `scalar` whose description carries its base and constraint.
  - `Map<String, V>` becomes `scalar JSON`; a named map alias its own `scalar`.
  - a payload enum (including `Result<A, B>`) becomes an object with one
    nullable field per variant (nullary => `Boolean`, one payload => its type,
    more => `JSON`); a nullary-only enum is a GraphQL `enum`.
  - a `mut fn` is a `Mutation` field, any other procedure a `Query` field
    (`FnInfo.mutates`). A procedure with a parameter takes
    `(input: <Req>Input)`. An empty `Mutation` is omitted.
  - an object with no fields gets `_placeholder: Boolean`, since GraphQL
    requires one field; the executor answers it too.
  - a name the document would define twice (`Query`, `Mutation`, `JSON`, or
    `Foo` beside `FooInput`), or one starting with `__`, is reported, not
    renamed.
  - `///` docs on types become descriptions. Reflection carries no procedure
    docs, so operations have none.

`graphqlServer` emits `graphqlHandle(req) -> Option<Response>` answering
`POST /graphql`. It walks the same `iface.functions` and bakes its
type graph from the same `gqlMembers`, so schema and executor agree by
construction. The graph keeps each field's `!` markers, which decide how far
a `null` propagates.

## gqlRootType

```vyrn
fn gqlRootType(root: String) -> String
```

## sdl

```vyrn
fn sdl(contract: String) -> String
```

Emits a module exporting `sdlText() -> String`, the contract's SDL as a constant.

## GqlSel

```vyrn
type GqlSel = { key: String, name: String, args: Array<JsonField>, subs: Array<GqlSel> }
```

One selected field: the response key (the alias, else the field name), the
field it names, its arguments as a JSON tree, and its subselections.

## GqlQuery

```vyrn
type GqlQuery = { root: String, sels: Array<GqlSel>, err: String }
```

A parsed operation: its root (`"query"` or `"mutation"`), its fields, and `err != ""` when it did not parse.

## gqlParseQuery

```vyrn
fn gqlParseQuery(src: String) -> GqlQuery
```

One executable operation: an optional `query` or `mutation` keyword and name,
then a selection set. A second operation is refused.

## GqlErr

```vyrn
type GqlErr = { message: String, path: Array<Json> }
```

One GraphQL error: its message and the response path to the fault, response
keys and list indices as `JStr` and `JNum` in one array.

## GqlOut

```vyrn
type GqlOut = { value: Json, errs: Array<GqlErr>, failed: Bool }
```

A completed position: its value, every error at or below it, and whether its
`null` is still climbing (`failed`), as a fault under a non-null position does.

## gqlProject

```vyrn
fn gqlProject(v: Json, sels: Array<GqlSel>, tref: String, path: Array<Json>, schema: fn(String, String) -> String) -> GqlOut
```

Completes one field position: projects `sels` out of `v` at `path`, against
`tref`, the field's type reference with its wrappers. An empty `sels` takes
the value whole.

This is a projection over a value the procedure already computed: a leaf is
omitted, not avoided. A lazy field (`.lazy`) would be the one case
where omission must also skip the work.

A fault becomes a `null` at its position and climbs through non-null (`!`)
positions. This is the one place a climb stops: at a nullable
position.

## gqlQueryText

```vyrn
fn gqlQueryText(body: String) -> String
```

The query text of a request body: the `query` member of a JSON envelope, or
the body itself (`application/graphql`). A GraphQL document is not JSON, so the
two cannot be confused.

## gqlErrorBody

```vyrn
fn gqlErrorBody(message: String) -> String
```

`{"errors":[{"message":"..."}]}`: a fault before execution (an unparseable
document, a second operation, a validation fault), with no `data`. A 200,
because a GraphQL client reads `errors`, not the status line.

## GqlArg

```vyrn
type GqlArg = { json: String, err: String }
```

One argument as the JSON text `fromJson` reads, or `err != ""`.

## gqlArgOf

```vyrn
fn gqlArgOf(field: String, args: Array<JsonField>, want: String) -> GqlArg
```

The `want` argument of `field` as JSON text, decoded by the same
`fromJson<ReqType>` that `std/rpc` runs on a body, so both wires validate
alike. Any other argument name is undeclared: the SDL names it `input`.

## gqlNoArgs

```vyrn
fn gqlNoArgs(field: String, args: Array<JsonField>) -> String
```

"" when `args` is empty, else the error naming the first undeclared argument.

## gqlArgError

```vyrn
fn gqlArgError(field: String, issues: Array<Issue>) -> String
```

The error for an argument the input record refuses: `fromJson`'s issues, as a 422 carries.

## gqlAnswer

```vyrn
fn gqlAnswer(body: String, resolve: fn(String, String, Array<JsonField>) -> Result<Json, String>, schema: fn(String, String) -> String) -> Response
```

Answers one GraphQL request body with the two baked tables: `resolve`
(root, field, args -> the encoded value or why not) and `schema`, the type
graph. The whole tree is validated before any procedure runs, so a typo'd
subfield of a `mut fn` never performs the mutation. Each root field then
completes on its own: a fault puts its message in `errors` at its path and
leaves the siblings in `data`. Argument decode faults surface per field.

## graphqlServer

```vyrn
fn graphqlServer(contract: String) -> String
```

Emits a module exposing `graphqlHandle(req: Request) -> Option<Response>`,
answering `POST /graphql` for the contract `sdl(contract)` documents. It mounts
beside `rpcHandle` and `connectHandle`. The generator and its module link into
one flat namespace, so the generator is not named `graphqlHandle`.
