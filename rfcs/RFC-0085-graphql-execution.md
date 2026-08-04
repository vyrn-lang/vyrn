# RFC-0085 — Answering a GraphQL Query

- **Status:** Draft. M1 designed.
- **Depends on:** RFC-0038 (`std/graphql` — the SDL this executes against),
  RFC-0074 (protocol projections — M4b is the consumer), RFC-0021 (`gen fn`),
  RFC-0031 (`moduleInterface` reachable type closure), RFC-0059 (`std/json`)
- **Evidence (in the tree):** `examples/shelf` serves
  `GET /schema.graphql` and cannot answer a query against it.

---

## The gap

`std/graphql`'s `sdl(contract)` emits `sdlText() -> String`: a deterministic
GraphQL schema document, generated from reflection, byte-stable, correct. Shelf
publishes it.

**Nothing can call it.** A GraphQL client reads that schema, writes
`{ browse { title } }`, POSTs it, and gets a 404 — the schema advertises
procedures that no GraphQL client can reach. The document is a description of an
API that does not exist at that endpoint.

That is also why RFC-0074 M4b — `graphql(...).lazy(|p| p.body, ...)`, a per-field
resolver override — is not buildable: **there are no resolvers to override.**
M4b was correctly deferred to "its own RFC". This is it.

## What this is not

**Not a GraphQL server framework.** No subscriptions, no persisted queries, no
dataloader, no schema stitching. The test of scope is RFC-0074's own rule:
a projection speaks its protocol's vocabulary at full fidelity, and *field
selection* is the thing GraphQL is chosen for. Everything else here exists to
make field selection answerable.

**Not a second wire.** A GraphQL request is an ordinary `Route` — `POST /graphql`
— and the procedures it dispatches to are the same ones `std/rpc` and
`std/openapi` project. Reflection stays the single source; this RFC adds an
interpreter for one request body, not a parallel surface.

## The shape

Three pieces, and only the middle one is new.

**A parser** for the query language's executable subset — selection sets, field
arguments, aliases, nested selections. GraphQL's grammar is small and this
project has `std/scan` and RFC-0054 code quotes for exactly this kind of work.

**A resolver table**, generated beside the SDL by the same `gen fn` that already
walks the contract. Reflection knows every procedure, its input record and its
return type; a resolver is the procedure plus the field name that reaches it.
`FnInfo.mutates` (RFC-0074 M4a) already says which root a field belongs under.

**An executor** that walks a selection set against the table, calls each root
field's procedure, and projects the requested fields out of the result. That
projection is the whole point: `{ browse { title } }` must not serialise an
author or an id.

## The decision this rests on

**Field selection is a projection over a value the procedure already returned,
not a fetch plan.** A Vyrn procedure returns a whole record; GraphQL asks for
part of it. So the executor's job at a leaf is to *omit*, not to *avoid
computing* — and that is a real semantic difference worth stating, because it is
what makes M4b's `.lazy(..)` meaningful later: a lazy field is the one case where
omission must also mean the work was never done.

If that distinction is not made now, `.lazy(..)` becomes an optimisation nobody
can observe, and the RFC-0074 example that motivates it stops making sense.

## Milestones

- **M1 — a query is answered.** Parse a selection set, dispatch a root field,
  project the requested fields, answer `{"data": …}`. One root field, no
  arguments, no aliases, no nesting past the first level. Errors as GraphQL's
  `{"errors": [...]}` shape rather than an HTTP status, because that is what a
  client reads.
- **M2 — arguments and nesting.** Field arguments decoded into the procedure's
  input record (the same `fromJson` path the RPC surface uses), aliases, nested
  selection into record-valued fields.
- **M3 — the error model.** Partial data with a populated `errors` array, path
  attribution, and the `null`-bubbling rule for non-null fields. This is the part
  every GraphQL implementation gets wrong first; it deserves its own milestone
  and its own evidence.
- **M4 — RFC-0074 M4b.** `.mutations(..)` is already answered by `mut fn`;
  `.lazy(field, resolver)` becomes buildable, and this is where "omit" versus
  "never computed" earns its distinction.

## Acceptance

- `examples/shelf` answers `POST /graphql` with `{ browse { title } }` and gets
  titles and nothing else.
- The same procedure answers over RPC, OpenAPI and GraphQL with no third
  declaration of anything.
- A field the schema does not declare is a GraphQL error naming it, not a trap
  and not a 500.
