# RFC-0073 — Generator Symbol Maps: Rename Across the Boundary

- **Status:** M1 landed (reflection origins, `std/symbolmap`, `client()`/`rpc()`
  maps, `vyrn emit-gen --maps`); M2–M4 draft
- **Depends on:** RFC-0033 (`//@origin` directives), RFC-0048 (vyx origins),
  RFC-0053 (generated error mapping), RFC-0050 (LSP references),
  RFC-0071 (contracts — members are the symbols this RFC maps),
  RFC-0072 (derived paths — the metadata this RFC surfaces on hover)
- **Evidence (user):** "full autocomplete and language integration", "does
  variables/function like `head` would be suggested by LSP?"

---

## The problem

`//@origin` maps generated *lines* back to source lines, which is enough for
diagnostics: an error inside emitted code reports at the author's cursor. It is
not enough for anything symbol-shaped.

Today, `api.pastes.list()` in a page is a call into a generated module. The LSP
can resolve it (cross-file hover and go-to-def over generated modules already
ship), but:

- **Rename does not cross the boundary.** Renaming `list` in
  `server/api/pastes.vyrn` does not touch `api.pastes.list()` call sites,
  because references stop at the generated module.
- **Derived facts are invisible.** RFC-0072 derives a wire path for every
  procedure. Nothing shows it in the editor, so the convention is something you
  remember rather than something you see.
- **Route params are stringly-typed.** A page reaches URL parameters through a
  string lookup, so a typo is a runtime miss, not a compile error.

A convention you cannot see is magic. This RFC makes the derived layer
inspectable and refactorable.

## The model

Generators emit, alongside code, a **symbol map**: for each exported symbol of
the generated module, the source declaration it stands for, plus any derived
metadata.

```
generated symbol            origin declaration                    derived
──────────────────────────  ────────────────────────────────────  ─────────────────────
api.pastes.list             server/api/pastes.vyrn:8:11 (list)    POST /_/pastes/list
api.pastes.byId             server/api/pastes.vyrn:9:11 (byId)    POST /_/pastes/byId
api.pastes.PasteList        shared/wire/paste.vyrn:12:13          —
Params.id                   app/routes/p/[id].vyx  (route seg)    —
```

This is `//@origin` promoted from line granularity to symbol granularity, and
extended with a derived-facts slot. The emission point is the same: generators
already know both halves at the moment they emit.

### Representation

A sibling `.map.json` per generated module, in the generator cache next to the
emitted source, keyed by the same content hash so it invalidates together:

```json
{
  "module": "client(./server/api)/pastes",
  "symbols": [
    { "name": "list",
      "origin": { "file": "server/api/pastes.vyrn", "line": 8, "col": 11, "name": "list" },
      "derived": { "kind": "rpc", "method": "POST", "path": "/_/pastes/list", "source": "convention" } },
    { "name": "byId",
      "origin": { "file": "server/api/pastes.vyrn", "line": 9, "col": 11, "name": "byId" },
      "derived": { "kind": "rpc", "method": "POST", "path": "/_/pastes/byId", "source": "convention" } }
  ]
}
```

JSON, not a bespoke format, so `vyrn routes`, the LSP, and any third-party tool
read the same file. `derived` is open — `std/http` writes cache and ETag policy
into it (RFC-0074), `std/ui` writes the route pattern.

`std/symbolmap` provides the emit helper so every generator produces the same
shape:

```vyrn
import { symbol, emitMap } from "std/symbolmap"

gen fn client(dir: String) -> Module {
    …
    emitMap([
        symbol("list", originOf(iface, "list"), rpcDerived(method, path, source)),
        …
    ])
}
```

## Typed route parameters

`std/ui` gains a generated `Params` record per dynamic route, derived from the
filename, with its fields mapped back to the path segments that produced them:

```
app/routes/p/[id].vyx        →  Params { id: String }
app/routes/[org]/[repo].vyx  →  Params { org: String, repo: String }
```

```vyx
export fn data() -> Query<Paste> { return query(|p| api.pastes.byId(IdReq { id: p.id })) }
```

`p.` completes to exactly the declared segments. Renaming the file to
`[pasteId].vyx` makes `p.id` a type error at that character, and the symbol map
gives the diagnostic a source: *"`Params` has no field `id`; this route declares
`pasteId` (app/routes/p/[pasteId].vyx)"*.

This replaces the string lookup form entirely. In a REST projection the same
check runs against the procedure's input type: `get("/{id}", byId)` requires
`IdReq` to have an `id` field, and `{ID}` is an error listing the available
fields.

> **This section describes work that happened without it.** `Params` is
> *declared* and checked against the segments (`std/ui.vyrn:1035`), not
> generated — a filename carries a segment's name and not its type, and the
> corpus uses both `Int64` and `String`, so the declaration is the only place
> the type is stated. REST placeholders are checked by a generated
> `String where value =~ …` refinement type, which `std/http`'s header notes
> "needs neither RFC-0073's symbol map nor a compiler rule". The string-lookup
> form no longer exists. See "M2 — read against what shipped".

## LSP capabilities

**Rename.** `textDocument/rename` on a procedure declaration collects
references through the symbol map: every generated symbol whose `origin` is the
renamed declaration is itself renamed, and its own references follow. The edit
spans source files only — generated modules are regenerated, never edited.

The payoff is the one the DX survey says no TypeScript framework can offer.
Because the client is generated from a checked declaration and then typechecked
as ordinary code, a missed call site is a **build error**, not a silent `any`.
Rename is therefore a convenience on top of a guarantee, rather than the only
thing standing between you and a runtime failure.

**Hover.** Hovering a procedure declaration shows its derived wire facts:

```
fn list() -> PasteList

POST /_/pastes/list · derived from server/api/pastes.vyrn
GET  /pastes        · explicit, server/api/pastes.http.vyrn:8 (cache 60s, etag)
```

Hovering a generated symbol at a call site shows the origin declaration's own
doc comment, not the stub's.

**CodeLens.** Above each `api` export, its derived path, click-to-open — the
same lens machinery as RFC-0064's dev entry and RFC-0055's bench lenses.

**Go-to-def.** Already works; the symbol map makes it land on the *declaration*
rather than the generated stub, which is what a reader wants.

## `vyrn routes`

The symbol map is the source of truth, so the command is a formatter over
existing data rather than a second implementation:

```
POST  /_/pastes/list       server/api/pastes.vyrn::list        convention
POST  /_/pastes/byId       server/api/pastes.vyrn::byId        convention
POST  /_/pastes/create     server/api/pastes.vyrn::create      convention
GET   /pastes              server/api/pastes.http.vyrn:8       explicit   cache=60 etag
GET   /pastes/{id}         server/api/pastes.http.vyrn:9       explicit   cache=3600 etag
POST  /pastes              server/api/pastes.http.vyrn:10      explicit   201
GET   /p/{id}              app/routes/p/[id].vyx               convention
GET   /                    app/routes/index.vyx                convention
```

`--json` emits the merged map for external tooling.

## Cache interaction

Generator output is content-addressed and cached (RFC-0021), keyed including the
generator name. Symbol maps join the cached artifact set under the same key: a
cache hit restores code and map together, so the LSP never sees a map that
disagrees with the code beside it. A map without its code, or the reverse, is a
cache-integrity error that forces regeneration rather than a silent skip.

## What this does not do

- It does not make generated modules editable. They remain build artifacts;
  rename rewrites sources and regenerates.
- It does not cover *values* — only symbols. A derived string embedded in a
  procedure body is not tracked.
- It does not extend to remote pinned modules, which have no local source to
  rename into. Hover and go-to-def over those keep today's behaviour.

## Milestones

- **M1 — format + emit.** `std/symbolmap`, the `.map.json` shape, cache
  integration. `client()` and `rpc()` emit maps. **Two prerequisites this
  document does not name, and one thing it should not build; see below.**

### M1 — what it actually needs

**Prerequisite: reflection has no origins.** `originOf(iface, "list")` above
cannot be written, because `FnInfo` is
`{ name, params, ret, retSchema, retUncodable, mutates }` — no file, no line, no
column. The whole RFC is "promote `//@origin` from line granularity to symbol
granularity", and the symbol half is not in the reflection the generators read.

That is an addition rather than a problem: `retUncodable` arrived with RFC-0071
M3 and `mutates` with RFC-0074 M4a, both by the same route. `FnInfo.origin`
is the third, and `TypeInfo` needs one too — the sketch above maps
`api.pastes.PasteList` back to `shared/wire/paste.vyrn:12:13`.

**The thing not to build: a second generator output.** The sketch writes
`gen fn client(dir: String) -> Module` and `emitMap([...])`. There is no
`Module`; a `gen fn` returns `String`, the emitted source, and adding a second
artifact means a new generator protocol, a new cache entry to keep in step, and
a new way for the two to disagree.

**The map is the module.** Emit it as an ordinary exported function —
`symbolMap() -> String`, returning the JSON — and every one of those problems
disappears: the cache already keys the module by content hash, so a map that
lives *inside* the module cannot go stale relative to it, and "cache
integration" stops being a milestone item. The LSP already runs generators as
compiled wasm (RFC-0076), so reading it costs a call.

`std/symbolmap` then provides the **builder**, not an emitter: `symbol(..)` and
`mapJson(..)` produce the string every generator bakes in, so the shape is
shared for the reason the sketch wanted — one library, one format.

The sibling `.map.json` still exists, and is written by the CLI on request
rather than by the generator. That keeps the RFC's actual requirement — a
third-party tool reads JSON, not Vyrn — without making a generator responsible
for a file it cannot invalidate.

### M1 — as landed

`FnInfo` and `TypeInfo` carry an `Origin`, `std/symbolmap` builds the document,
`client()` and `rpc()` bake a `symbolMap()` into what they emit, and
`vyrn emit-gen --maps` prints it as JSON. Six places where the implementation is
not what this document said, and why.

**The origin cost a lexer pass, not a span rewrite.** The AST carries a `line`
per declaration and no column at all — `symbols.rs` says why: threading spans
through every node construction site is high churn for something two consumers
want, so the LSP recovers a declaration's name column from the lexer's per-token
`(line, col)` instead. Reflection now does the same, once per module rather than
once per lookup: `Origins` lexes each module the reflected link read and indexes
the first identifier of each `(line, name)`. The sources were already in hand —
`gen_module_interface_lit` records every module the link touched so a closure
type's defining file joins the generator's cache inputs (RFC-0031) — so the index
is built from the same reads, and a module reflected is a module indexed by
construction. Re-lexing is also what keeps a comment or a string containing the
name from being mistaken for it, which a substring search would not.

**`Origin` carries a `name`, and it is not a restatement.** The record is
`{ file, line, col, name }`, where `name` is the DECLARATION's name — routinely
not the generated symbol's. `client()` exports `pastesCreate` and `rpc()`
dispatches to `rpcHandlePastesCreate`; both stand for `create` in
`server/api/pastes.vyrn`. That is the sketch's own `"name": "list"` field, and it
is what lets a consumer holding one origin render `pastes.vyrn:28:15 (create)`
without also holding the `FnInfo` it came from.

**The map-inside-the-module decision held, and it removed work rather than
adding it.** No new generator protocol, no second cache entry, no atomicity rule
to enforce: the map is an export of the module, so a cache hit that restores the
code restores the map, and the "cache integration" milestone item and the
"partial artifact set forces regeneration" acceptance line are both moot. The
generator side is one call — `symbolMapFn(module, symbols)` appended to the
emitted source — and the JSON is baked as a string literal through an RFC-0054
code quote, so the compiler's own escaping does the second layer rather than a
hand-rolled escaper free to disagree with the lexer.

**The CLI surface is `vyrn emit-gen --maps`, and it writes no file.** The
smallest thing that satisfies the requirement — a third-party tool reads JSON,
not Vyrn — is a flag on the command that already runs every generator and already
banners its output. It prints one compact document per line, banners on stderr,
so `> api.map.json` produces the sibling file without this command inventing a
NAME for it. That is the part worth not guessing: a name would have to be a slug
of a generator CALL (`client("../server/api")`), and `vyrn routes --json` in M4
will decide how the maps are addressed with the merged table in hand. Reading is
a parse, not a run: the map is a string literal the generator baked in.

**`rpc()`'s mapped symbols are its internal handlers, which the sketch's "each
exported symbol" does not cover.** The router exports exactly one function,
`rpcHandle`, so mapping only exports would map nothing on the server side. Each
`rpcHandlePastesCreate` stands for exactly one declaration, and both maps name
that declaration at the same file, line and column — which is the property the
cross-boundary rename in M4 needs, and is under test. `client()` maps its
procedure stubs AND its re-emitted types: a re-emitted `type` has lost its file
in the generated source, so the map is the only place that still says
`PasteList` came from `shared/wire/paste.vyrn`. That is the sketch's third row,
and the reason `TypeInfo` needed an origin as well as `FnInfo`.

**It found a comptime-purity bug that had nothing to do with symbol maps.**
`std/symbolmap` reaches `std/json`'s `emit`, whose body is
`JArr(items) => emitArr(items)` — and the purity analysis collected `let` and
`for` binders as locals but never a match arm's pattern binding. So `items` read
as a reference to module state, and in the one example that happens to declare
`let mut items` (`examples/rpcsplit`) every generator reaching `emit` became
impure. Naming a binder after a global in a module it cannot see is a
coincidence, not an effect. The fix is scoped rather than flat — an arm's binders
shadow inside that arm and not in its siblings — and `if let` was missing the
same thing.

- **M2 — typed `Params`.** ~~Generated per-route `Params` records with mapped
  fields; placeholder checking in REST projections; the string-lookup form
  removed.~~ **Struck.** All three are already done, moot, or impossible — see
  below.

### M2 — read against what shipped

**Field names are already checked against the segments.** `std/ui.vyrn:1035`
does it at generation time through `moduleInterface`, and the module's own
header has said so since it was written: *"the field NAMES must match the
segments exactly."* Rename `[id].vyx` to `[pasteId].vyx` and the declaration
stops matching — which is precisely the behaviour this milestone was written to
add.

**Placeholder checking in REST projections shipped without this RFC**, and
`std/http`'s header says why in as many words: a procedure's generated parameter
type is a `String where value =~ …` admitting exactly the placeholders its input
record has fields for, so `byId("/{ID}")` is a checker error at the call site and
*"needs neither RFC-0073's symbol map nor a compiler rule."* That is a better
mechanism than the one proposed here — a refinement type the checker already
understands, rather than a generated table something has to consult.

**The string-lookup form is already gone.** There is nothing left in `std/` or
the corpus to remove.

**And the one item that is not done cannot be.** "A generated `Params` record
derived from the filename" would have to invent the field *types*, and a
filename does not carry them: `[id]` says a segment is named `id` and nothing
about whether it is an `Int64` or a `String`. The corpus uses both. So the
declaration is not redundancy to be generated away — **it is the only place the
type is stated**, and the check against the filename is what keeps it honest.

The milestone was written before any of this existed and reads as though the
declaration were the problem. It is the answer.

**What genuinely survives** is the diagnostic's *source*: today the mismatch is
reported by the generator, and M1's symbol map could let it name the file and
line that declares the conflicting segment. That is a note on M3 (LSP read),
not a milestone of its own.
- **M3 — LSP read.** Hover with derived facts, go-to-def onto declarations,
  CodeLens with paths.
- **M4 — rename.** Cross-boundary rename with regeneration; `vyrn routes` and
  `--json` over the merged map.

## Acceptance

- Renaming `list` → `recent` in `server/api/pastes.vyrn` updates every
  `api.pastes.list()` call site across `app/` and `client/` in one edit.
- Deleting a procedure that is still called is a build error naming the call
  sites — never a silent `any`, never a runtime 404.
- Hover on a procedure shows its derived path and whether it is convention or
  override.
- Renaming `[id].vyx` to `[pasteId].vyx` produces a type error at `p.id` naming
  the new segment.
- `vyrn routes --json` and the LSP agree, because both read the same maps.
- Generator cache hits restore code and map atomically; a partial artifact set
  forces regeneration.
