# RFC-0085 — Answering a GraphQL Query

- **Status:** M1, M2, M3 shipped. M4 designed.
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

  **What an error is here is narrower than GraphQL's general case, and saying so
  is most of the design.** Three things could produce one, and only the first
  does:

  - **A request fault** — an undeclared field, a selection on a scalar, an
    argument that fails `fromJson`. M1 and M2 already answer these; M3's job is
    to attribute them to a *path* and to let a sibling root field succeed
    beside them.
  - **A resolver's `Err`** — and it stays in `data`. `Result<A, B>` already
    projects as a tagged object with nullable `Ok`/`Err` (RFC-0038), because an
    `Err` is a **modelled outcome, not a fault**: the schema declared it, the
    client can select it, and moving it to `errors` would hide a value the type
    says exists. RFC-0074 made the same call for HTTP — a `200` carrying
    `{"Err": …}` unless `notFoundWhen` says that particular payload means
    absence. The GraphQL twin of `notFoundWhen` is a projection decision and
    belongs with M4's override surface, **not here**. M3 must not invent an error
    policy.
  - **A trap** — Vyrn has no unwinding, so a trap ends the process and
    `vyrn serve` answers `500`. It is a dead request, not a GraphQL error, and
    nothing in this milestone can catch one.

  So the partial-data case M3 exists to build is real and reachable with the
  error kinds M1 and M2 already produce: `{ browse { title }, nope }` answers
  `browse` and reports `nope` at its path. And null-bubbling has a concrete
  trigger — a fault inside a **non-null** subtree, where the SDL's own `!`
  markers say how far the `null` must climb.
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

---

## M2 — as landed

`{ byId(input: { id: 2 }) { Ok { title } } }` answers with that book's title, and
`{ recent: browse { .. } }` answers under `recent`. Both of M1's recorded holes
are closed: a selected member the schema does not declare is
``Cannot query field `nope` on type `Book`.`` and a selection on a scalar is
``Field `title` must not have a selection since type `Title` has no subfields.``
`examples/graphql.vyrn` asserts all four as its own tests, and `examples/shelf`
answers them over the wire.

### The argument, and where the type actually was

Decoding reused the RPC path completely: the arm the generator emits for a
procedure that takes an argument is `fromJson(<ReqType>, ..)`, which is the same
call `std/rpc`'s handler makes on a POSTed body, over the same record. So
`BookId = Int64 where value >= 0` refuses `{ id: -1 }` here without this file
knowing that the rule exists, and the accumulated `Issue`s reaching the GraphQL
error are the ones a 422 would have carried.

**This document's stated reason for pairing the holes with the arguments was
wrong about the mechanism.** It said an argument cannot be decoded without
knowing the input record's type, so decoding one "forces the type graph into the
executor". The premise is true and the conclusion does not follow: the type is
known where the arm is EMITTED — `ParamInfo.spelling`, which the SDL already
reads to write `(input: BookIdInput)` — so it is spelled into the generated
source and nothing carries a type at run time. Arguments cost no type graph at
all.

The two holes still cost one, and it is a separate table. The pairing was right
for a reason this document did not give: the table has to list exactly the object
types the SDL declares and exactly their fields, or a query the document accepts
gets `Cannot query field` from the executor — which is the divergence the shared
walk exists to prevent, arriving through a second walk instead of a second
declaration. So the SDL emitter was refactored first: `gqlMembers(decl, iface)`
is now the ONE reading of a declaration's right-hand side, answering "does this
map to a GraphQL object, and with which members" for a record, a payload enum's
tagged form, and a `Result<A, B>` alias alike. `gqlRecord`, the tagged type and
the `Result` type are all emitted from it, and so is the baked table. The SDL is
byte-identical across that refactor and the file is shorter than it was.

### One lookup, two questions

The table is one generated function:

- `gqlSchema(t, field)` is the NAMED GraphQL type of `t`'s `field`, `""` when `t`
  declares no such field.
- `gqlSchema(t, "")` is `t` itself when `t` is an object type, `""` when it is a
  leaf.

Two questions rather than two tables, because a selection asks both of them at
once and a leaf is the ABSENCE of an arm — which is exactly the answer "has no
subfields". `Query` and `Mutation` are arms like any other, split by the same
`gqlRootOf` verdict that puts a field under one root in the document, so the
projector gets a root field's type from the same place the SDL got its spelling.
Only the NAMED type is carried: the projector walks a list transparently, so
`[Book]` and `Book` ask the table the same question.

That is what separates the two observations M1 could not tell apart. A member
the schema DECLARES and the value lacks is still `null` — it is a `None`
`Option`, and that is what `null` is for. A member the schema does not declare is
an error naming it. `toJson` still omits both; the schema is what says which one
happened.

### Arguments are a JSON tree, read by std/json where it matters

GraphQL's argument syntax is not JSON — an input-object key is unquoted, an enum
value is a bare word — but what an argument MEANS is a JSON value, which is the
form `fromJson` already decodes. So the executor reads the structure (list,
input object, bare word) and deliberately does not read the two forms that have a
lexical definition worth getting exactly right: a string literal and a number
token are handed to `std/json`'s own reader over their source span. A second
escape decoder and a second number grammar are precisely the kind of near-copy
that is lenient where the original is strict, and this is the same refusal M1
made about writing a projection-aware encoder.

### Refused by name, still

The milestone's habit is kept for everything it does not do. A variable (`$id`)
and a variable definition list are refused naming them, because an argument here
is a literal and nothing substitutes the envelope's `variables` — silently
ignoring `($id: Int)` would leave every later `$id` reading as a bare enum value.
An argument written at depth is `Unknown argument`, since the SDL declares
arguments on root fields only.

### Recorded, not fixed

**A missing selection on an object type is not refused.** GraphQL requires one:
`{ browse { books } }` should be "Field `books` of type `[Book]` must have a
selection of subfields". It is legal here and answers each book whole, which is
M1's own demonstration that a leaf is taken entire and that the executor OMITS
rather than avoids computing. Refusing it would delete the clearest evidence for
the decision this RFC rests on, and the check belongs with the rest of the
validation pass rather than smuggled in beside these two.

**Introspection is still absent**, and the error model is still one message. M3
is not closer for the type graph being here: `gqlAnswerOne` would loop over root
fields in two lines, and the two lines are not the milestone — `path`
attribution, partial `data` beside a populated `errors`, and the `null`-bubbling
rule are, and none of them got cheaper.

110 examples, three engines.

---

## M3 — as landed

`{ browse { books { title } } nope }` answers `browse` in `data`, reports `nope`
in `errors` at `["nope"]`, and neither hides the other. Every error carries the
path it happened at; a fault under a non-null field makes the `null` climb until
it reaches a position that may hold one. `examples/graphql.vyrn` asserts each of
those and `examples/shelf` answers them over the wire.

### The type graph did not carry nullability, and it cost one call to fix

M2 baked `gqlNamed(m.ref)` into `gqlSchema` — the NAMED type, with the list and
non-null wrappers stripped off — so the `!` markers were being generated,
published in the SDL, and then discarded at precisely the point the projector
needed them. They are the whole of the bubbling rule: `Array<Book>` maps to
`[Book!]!` and that string is the answer to "how far does this `null` climb".

The fix is one `gqlNamed` call deleted from the emitter and three added at the
places that want a name rather than a reference. So M2's honest note — that M3
"is not closer for the type graph being here" — was half wrong: the graph carried
the wrong half of what it already knew, and the milestone's hardest rule was one
edit away from having its input. What M2 got right is that the two lines looping
`gqlAnswerOne` really were not the milestone.

### A path is a mixed array, and `std/json` already had one

GraphQL's `path` is field names and list indices in one array, which is a shape a
JSON tree either has or has to fake. `std/json`'s does have it: `JArr` holds
`Array<Json>`, so `["browse", "books", 0, "title"]` is `JStr`s and a `JNum` in the
same list and `emit` writes the index unquoted with no special case. `GqlErr` is
`{ message: String, path: Array<Json> }` and nothing converts an index to a
string to make it fit.

### The rule, and the one place it is decided

Every fault becomes a `null` at the position it happened. `gqlProject` completes
one position: it strips the outer `!`, walks the shape below, and then — and only
then — asks whether the position it is standing on may hold a `null`. If it may,
the climb stops there; if it may not, `failed` goes up and the parent asks the
same question. A list is a position whose elements are positions, so a faulting
`Book!` takes `[Book!]!` with it. Nothing below re-decides the rule: `gqlPick` and
`gqlProjectEach` only report `failed` upward, so there is one line in the file
where a `null` is allowed to settle.

**Every element is still walked after one faults.** A fault that repeats down a
list is reported at each index rather than once, which is what makes the index in
the path load-bearing rather than decorative.

**A position with no declared type is nullable.** An undeclared field has no
type reference, so nothing says a `null` may not sit at it — and that is exactly
why `{ browse { .. } nope }` leaves `browse`'s answer in `data`. Had the missing
type been read as non-null, the headline case of this milestone would have nulled
the whole reply, which is the wrong answer arrived at from the right rule.

### Where the climb stops, and the shape shelf cannot produce

A root field is nullable by construction — `gqlRootField` strips the `!` in the
document and `gqlRootMembers` strips it in the graph — so a climb that starts
under `browse` ends at `browse`: `{"data": {"browse": null}}` with the errors
beside it. That is the right default and it is the reason a sibling survives at
all; a non-null root field would discard an answer that was already computed.

It also means **`examples/shelf` cannot produce `"data": null`**. Every climb has
a nullable position waiting for it at the root. Rather than assert a case the
corpus cannot reach, `examples/graphql.vyrn` declares a stand-in type graph whose
root field is `Book!` and drives the same `gqlAnswer` the generated handler calls,
which is the only shape where the `null` runs out of positions. Shelf produces
the other two on its own: `Title!` inside `[Book!]!` climbs past three positions
to stop at `browse`, and `BookResult`'s `Ok` — nullable, because a tagged union
has exactly one arm non-null at a time — stops a climb one level below the root.

### What did not become an error

The taxonomy above held with nothing added to it. A resolver's `Err` is still a
value in `data`; a trap is still a dead request. One candidate the executor could
now see was refused: GraphQL's "Cannot return null for non-nullable field" — a
value that is absent where the schema says non-null. `toJson` omits a `None`
`Option`, so this executor sees that shape routinely and reporting it would be an
error policy invented in M3 rather than a request fault attributed by it.
Bubbling is driven by faults only, and an incidentally-absent value stays the
`null` M2 already made it.

### The suites nothing was running

`std/graphql.vyrn` and `examples/graphql.vyrn` both carried `test` blocks that no
Rust test executed: the parity harness runs an example's `main` on three engines
and never its tests, and the per-std-module `vyrn test` runners other modules have
had never been written for this one. So M1's and M2's assertions were green
because someone ran them by hand. `exports.rs` now runs both suites and asserts a
floor on how many blocks ran, since a suite that stops being discovered otherwise
passes.

23 blocks in `std/graphql.vyrn` and 11 in `examples/graphql.vyrn`, now run; 110
examples, three engines.
