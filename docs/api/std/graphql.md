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
  - PROCEDURES become `Query`/`Mutation` fields: a `get*`/`list*` procedure is
    a Query, everything else a Mutation (the split is dumb by design). A
    1-parameter procedure takes `(input: <Req>Input)`; the return maps by the
    table above. An empty `Query` gets a `_placeholder` field (GraphQL needs a
    non-empty query root); an empty `Mutation` is omitted.
  - `///` docs on TYPES become SDL descriptions. Procedure/param/module docs
    are NOT in `moduleInterface` reflection, so operation descriptions are
    absent (gap recorded in RFC-0038).

Scope: a DOCUMENT, not an executor. A GraphQL executor is deferred (RFC-0038).

Inspect the synthesized module with:  vyrn emit-gen <file>

## sdl

```vyrn
fn sdl(contract: String) -> String
```

`sdl(contract)` — emit a module exporting `sdlText() -> String`, a GraphQL SDL
document for the contract. The document is baked as a deterministic constant.
