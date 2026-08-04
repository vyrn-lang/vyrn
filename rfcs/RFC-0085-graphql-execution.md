# RFC-0085 — Answering a GraphQL Query

- **Status:** M1 shipped. M2–M4 designed.
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
- **M2 — arguments, aliases, and the type graph they bring with them.** Nesting
  landed in M1 (see its "as landed" — once the projector walks an array, walking
  an object is three lines). What is left is field arguments decoded into the
  procedure's input record, via the same `fromJson` path the RPC surface uses,
  and aliases.

  **The reason those belong together with M1's two open holes** is that an
  argument cannot be decoded without knowing the input record's *type*. M1's
  projector has only the value, which is why a selected member the value lacks
  answers `null` rather than an error, and why a selection on a scalar is
  unrefused: `toJson` omits a `None` `Option`, so absence-in-value and
  absence-from-schema are indistinguishable when all you hold is JSON.
  Decoding an argument forces the type graph into the executor — and once it is
  there, both holes close for free. Closing them in M3 instead would mean
  carrying the type graph in for arguments and then not using it for a
  milestone.
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

---

## M1 — as landed

`graphqlServer(contract)` sits beside `sdl(contract)` in `std/graphql` and emits
`graphqlHandle(req: Request) -> Option<Response>`, answering `POST /graphql`.
`examples/shelf` mounts it next to `rpcHandle` and `connectHandle` — one api
directory, three protocols — and `examples/graphql.vyrn` drives the same
procedures as an ordinary three-way parity citizen.

### The projection, and what it cost

`toJson` encodes a whole record and there is no partial encoder, so the executor
goes through the value tree: `toJson(browse())` → `parseJson` → project → `emit`.
The cost is one encode, one parse and one re-encode per request, and it is paid
in `std/json`'s own reader and writer rather than in a second JSON writer written
for GraphQL. That was the alternative and it was refused: a projection-aware
encoder would be a second thing that has to agree with `toJson` about how a
validated scalar, a `Map`, and a payload enum reach the wire, which is the
divergence this file's whole shape exists to prevent. The tree is the cheap
answer because it already exists.

The projection is recursive over the value, and **that is a deviation from M1's
scope**: the milestone said "no nesting past the first level", and nesting is not
separable from the projection here. `browse` returns `BookList = { books:
Array<Book> }`, so the acceptance query in the section above —
`{ browse { title } }` — does not typecheck against shelf's own SDL; the titles
live at `browse.books[].title`. The projector has to walk an array to answer even
one level, and once it walks an array, walking an object costs three lines while
a depth limit costs more than that and buys nothing. So nesting works and M2 keeps
arguments and aliases, which are genuinely separate: both are refused BY NAME
where they are written, so a query this milestone cannot answer is never answered
as if it meant something else.

### One walk

`gqlRootOf(f)` reads `FnInfo.mutates` (RFC-0074 M4a) and returns the operation
type. The SDL's Query/Mutation split and the resolver table's root check are the
same call, so a field cannot be declared under one root and answered under the
other. Both generators iterate `iface.functions` and nothing else: there is no
second list of what the procedures are. A procedure that takes an argument is
IN the table and refuses by name rather than being omitted from it, because a
declared field silently missing from the executor is exactly the divergence being
avoided.

`graphqlServer` also calls `std/rpc`'s `validateContract` — the same `Api` +
serializability rule `std/rpc` and `std/http` apply — because it emits a `toJson`
call per procedure and that is what the rule governs. `sdl` needs no such check;
it only reads type spellings.

### Recorded, not fixed

**Nested field validation needs the type graph.** The resolver table is the
schema for root fields, so `{ shelved }` answers
``Cannot query field `shelved` on type `Query`.`` At depth the projector has only
the value, and `toJson` OMITS an `Option` field that is `None` — so absence in
the value and absence from the schema are indistinguishable there. A selected
member the value lacks answers `null` (a client indexes the reply by the names it
wrote) rather than being reported. Carrying the type graph into nested selection
is what M2/M3 cost; this is the honest statement of the gap rather than a check
that would be wrong on every `None`.

**A selection on a scalar is not refused.** GraphQL says "field must not have a
selection since type String has no subfields"; saying that also needs the type
graph. The scalar answers itself for now. M3.

**Errors are a single message.** No `path`, no `locations`, no partial `data`
beside a populated `errors` — the reply is either `{"data": …}` or
`{"errors":[{"message": …}]}`, always 200 `application/json`, because a client
reads the array and not the status line. One root field per request is enforced
for the same reason: two of them raise partial data the moment one resolves and
the other does not, and that is the whole of M3.

**Introspection is absent.** `__schema` and `__type` are ordinary undeclared
fields and answer as such. The schema is served at `/schema.graphql`.

**The endpoint path is `/graphql`, hardcoded.** RFC-0074's `.endpoint("/graphql")`
is a projection-builder concern and arrives with M4.

### The decision, in the code

`gqlProject`'s doc comment states it where a reader would otherwise assume
laziness: the value arrives fully computed, so at a leaf the executor OMITS
rather than avoids computing, and `.lazy(field, resolver)` is the one case that
later has to mean the work was never done. `examples/graphql.vyrn` shows both
sides of it in adjacent lines — `{ browse { books { title } } }` drops four
fields per book that the store computed and `toJson` encoded, and
`{ browse { books } }` keeps them.
