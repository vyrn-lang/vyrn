# RFC-0073 — Generator Symbol Maps: Rename Across the Boundary

- **Status:** Draft
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
export let data = query(|p| api.pastes.byId(IdReq { id: p.id }))
```

`p.` completes to exactly the declared segments. Renaming the file to
`[pasteId].vyx` makes `p.id` a type error at that character, and the symbol map
gives the diagnostic a source: *"`Params` has no field `id`; this route declares
`pasteId` (app/routes/p/[pasteId].vyx)"*.

This replaces the string lookup form entirely. In a REST projection the same
check runs against the procedure's input type: `get("/{id}", byId)` requires
`IdReq` to have an `id` field, and `{ID}` is an error listing the available
fields.

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
  integration. `client()` and `rpc()` emit maps.
- **M2 — typed `Params`.** Generated per-route `Params` records with mapped
  fields; placeholder checking in REST projections; the string-lookup form
  removed.
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
