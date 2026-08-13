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
    table above. An empty `Mutation` is omitted.
  - an OBJECT with no fields gets a `_placeholder: Boolean` field, wherever it
    comes from — an empty query root, or a zero-field record (`type Empty =
    {}`). GraphQL requires an object type to define at least one field, so
    `type Empty {}` is a syntax error that poisons every field referencing it.
  - a NAME the document would define twice (a contract type called `Query`,
    `Mutation` or `JSON`; a record `Foo` beside a type `FooInput`), or one
    beginning with the introspection-reserved `__`, is REPORTED (RFC-0099)
    and not repaired: these names are the wire surface, and renaming one is
    the author's decision.
  - `///` docs on TYPES become SDL descriptions. Procedure/param/module docs
    are NOT in `moduleInterface` reflection, so operation descriptions are
    absent (gap recorded in RFC-0038).

The EXECUTOR (RFC-0085 M1–M3) lives at the bottom of this file:
`graphqlServer` emits a `graphqlHandle(req) -> Option<Response>` answering
`POST /graphql` over the SAME `iface.functions` walk the SDL reads, so a field
the schema declares and a field the executor dispatches are one list. It bakes
the SDL's TYPE GRAPH beside the resolver table, out of the same `gqlMembers`
reading of each declaration the `type` definitions above are emitted from —
so an object type's fields are one set, not two that agree. The graph carries
each field's full type REFERENCE, `!` markers included, because those are what
say how far a `null` climbs when something faults under them (M3).

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

## GqlErr

```vyrn
type GqlErr = { message: String, path: Array<Json> }
```

One GraphQL error: its message and the RESPONSE PATH it happened at — the
response keys and list indices from the root down to the position that
faulted (RFC-0085 M3), which is how a client finds it inside a partial `data`.

A path element is a NAME or an INDEX in ONE array, and the value tree says
that directly: a `JStr` and a `JNum` in a single `JArr`. Nothing had to be
invented for the mixed array and no index is stringified to fit.

## GqlOut

```vyrn
type GqlOut = { value: Json, errs: Array<GqlErr>, failed: Bool }
```

A completed field position: its value, every error found at or below it, and
whether the `null` is still CLIMBING (`failed`) — which is what a NON-NULL
position does with a fault, because `null` may not sit there.

## gqlProject

```vyrn
fn gqlProject(v: Json, sels: Array<GqlSel>, tref: String, path: Array<Json>, schema: fn(String, String) -> String) -> GqlOut
```

Complete one field position: project `sels` out of `v` at response `path`,
against `tref` — the field's GraphQL type REFERENCE, list and non-null
wrappers included.

**Field selection is a projection over a value the procedure ALREADY
returned, not a fetch plan.** `v` arrives here fully computed — every field of
it, selected or not, was produced by the procedure and encoded before this
function saw it. So at a leaf this OMITS; it does not avoid computing. Nothing
below is lazy and a reader must not read it as if it were.

That distinction is the one RFC-0074 M4b's `.lazy(field, resolver)` later has
to break: a lazy field is the single case where omission must ALSO mean the
work was never done. Stated here because if it is only stated then, `.lazy(..)`
is an optimisation nobody can observe and there is nothing to point at.

**Null-bubbling (RFC-0085 M3).** A fault does not fail the request: it becomes
a `null` at the position it happened, and the SDL's own `!` markers say how far
that `null` has to climb. This function is the ONE place a climb stops — when
the position it is passing through may hold a `null`. Everything below just
reports `failed` upward, so the rule is read in one place rather than
re-decided at each shape.

An empty selection set means "this field is a leaf" — take the value whole.

## gqlQueryText

```vyrn
fn gqlQueryText(body: String) -> String
```

The query text of a request body: the `query` member of a GraphQL-over-HTTP
JSON envelope, or — when the body is not such an envelope — the body itself,
which is what an `application/graphql` request sends. A GraphQL document is
not valid JSON, so the two cannot be confused.

## gqlErrorBody

```vyrn
fn gqlErrorBody(message: String) -> String
```

`{"errors":[{"message":"…"}]}` — a REQUEST fault, and the only reply with no
`data` entry at all.

A GraphQL client reads the `errors` array, not the status line, so the reply
stays a 200 `application/json`. This shape is for the faults that land BEFORE
execution — an unparseable document, a second operation — where no field was
ever reached, so there is no position to attribute and no partial `data` to
carry. Everything reached during execution goes through `gqlExecBody`.

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

**Every root field the operation selects** (RFC-0085 M3). Each is resolved and
completed on its own, so one of them faulting leaves the others in `data` and
puts its own message in `errors` at its own path. `data` goes `null` only when
a fault climbs past a NON-NULL root field — which this generator's own SDL
never declares, since a root field is nullable by construction precisely so a
sibling's answer survives.

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
