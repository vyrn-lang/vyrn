# RFC-0072 — Audience and Derived RPC: Deleting the Contract File

- **Status:** Draft
- **Depends on:** RFC-0071 (module contracts — `Api` is the contract this RFC
  attaches), RFC-0019 / `std/rpc` (the generators this RFC re-points),
  RFC-0027 (`import * as ns`), RFC-0069 (universal pages, the payload protocol)
- **Evidence (user):** "why we have contract at all? shouldn't it be defined in
  `api` routes?", "but it won't become intuitive because contract imports and
  uses server", "it isn't intuitive what files code would be included",
  "I don't like exposing language or framework names", "why we not creating
  server/client dirs? something what Nuxt does"

---

## The problem

`examples/bin/contract.vyrn` is a file whose entire content is forwarding:

```vyrn
import * as store from "./store"
export fn listPastes() -> PasteList { return store.listPastes() }
export fn getPaste(req: IdReq) -> PasteResult { return store.getPaste(req) }
export fn createPaste(req: CreateReq) -> PasteResult { return store.createPaste(req) }
```

It exists because the generators (`rpcServer`, `rpcClient`, `openapi`) need one
module to reflect over. It has three costs:

1. **It straddles the boundary.** The file that is supposed to *be* the
   interface imports and calls the implementation. Nothing about the tree tells
   you which side of the wire anything is on.
2. **It is a manual index.** Every procedure is written twice; forgetting the
   second write is silent.
3. **It answers nothing about bundling.** Which files reach the browser is
   determined by transitive imports through generated modules — inspectable only
   by building.

This RFC deletes the file and replaces both jobs — *what is the API* and *what
ships where* — with the directory tree.

## Audience: who runs it

Every module has exactly one **audience**, determined by the nearest audience
segment in its path:

```
server/     server-only    never in the client bundle
client/     client-only    never in the server binary
app/        universal      SSR and client bundle
shared/     universal      no UI, usable by anything
```

Declared, not hardcoded:

```json
{
  "audience": {
    "server":    ["server"],
    "client":    ["client"],
    "universal": ["app", "shared"]
  }
}
```

**Nearest wins**, so the rule is recursive and both common layouts work under
one checker:

```
# audience-outer (the default; matches Nuxt's vocabulary)
server/api/pastes.vyrn
app/routes/index.vyx
shared/wire/paste.vyrn

# feature-outer (for larger apps)
src/pastes/server/api/pastes.vyrn
src/pastes/app/routes/index.vyx
src/users/server/api/users.vyrn
```

A module with no audience segment on its path is **universal** — the
conservative default, since a universal module is legal to import from anywhere.

### Enforcement

The checker rejects an import that widens audience:

```
error: `app/routes/index.vyx` is universal and cannot import
       `server/store.vyrn`, which is server-only
  --> app/routes/index.vyx:4:1
   |
 4 | import * as store from "../../server/store"
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   = audience `server` is declared by vyrn.json:audience.server
   = call it through `client("./server/api")` instead
```

Legal edges: `server → shared`, `client → shared`, `app → shared`,
`server → app` (SSR renders pages). Illegal: anything → `server` from a
non-server module, anything → `client` from a non-client module.

This is the improvement over the prior art. Nuxt's split is a bundler
convention, so a server import reaching a component is a build-time surprise at
best and a leaked secret at worst. Leptos's split is driven by Cargo feature
flags, so a misconfigured feature breaks it. Here it is a checker rule with a
diagnostic, decided before anything is bundled.

`vyrn why <file>` prints the audience, the segment that decided it, and every
import chain that reaches the file — so "what is bundled where" is a command,
not a convention you have to trust.

## Role: what it is

Audience is the outer dimension; **role** is the inner one, and roles carry
contracts (RFC-0071):

```json
{
  "roles": {
    "api":     "std/rpc:Api",
    "routes":  "std/ui:Page",
    "widgets": "std/vyx:Component"
  }
}
```

`server/api/pastes.vyrn` reads as what it is: server-only code, of the procedure
kind. Previously `api/` meant both at once, which is why the layout did not
scale to feature modules.

## Derived RPC paths

Every export of a module under an `api` role is a procedure (contract `Api`).
Its wire path derives from the module path and the export name:

```
server/api/pastes.vyrn :: list      →  POST /_/pastes/list
server/api/pastes.vyrn :: byId      →  POST /_/pastes/byId
server/api/orders/refund.vyrn :: run →  POST /_/orders/refund/run
```

Configurable, with `{module}` the api-relative module path and `{name}` the
export:

```json
{ "rpc": { "prefix": "/_", "path": "{module}/{name}" } }
```

No file declares this. No name is special. The rule is total, which is exactly
why it needs no magic names — the objection to `get`/`post`/`load` was that
special names hide behind ordinary syntax, and a total rule has no special names
to hide.

### Overrides, nearest wins

```vyrn
// server/api/module.vyrn — directory scope
export fn rpc() -> Rpc { return Rpc { prefix: "/internal", path: "{name}" } }
```

```vyrn
// server/api/pastes.vyrn — declaration scope, always wins
export fn listAt() -> Pin { return at(list, "/pastes/recent") }   // pinned; already published
```

`vyrn routes` prints the resolved table with a `source` column reading
`convention` or `override`, so drift is visible in review rather than
discovered in production.

**Collisions are errors.** Two procedures deriving the same path — reachable
through overrides or a `{name}`-only template — fail the build naming both
declarations. Last-wins is never silent.

## The generators, re-pointed

`std/rpc` gains directory-level entry points. The single-module forms remain for
libraries that legitimately have one module.

```vyrn
// server root — mounts the whole derived surface
import { rpc } from "std/rpc"
let rpcRoutes = rpc("./server/api")
```

```vyrn
// any universal or client module — the generated client
import * as api from client("./server/api")
…
api.pastes.list()
api.orders.refund.run(req)
```

`client(dir)` walks the directory, checks each module against `Api`, and emits
one namespace per module mirroring the tree. The emitted stubs are ordinary
Vyrn code, typechecked like anything else — which is the whole point. The
failure modes catalogued in the DX survey (router-size-proportional type-check
latency, non-local depth errors, silent degradation to `any`) all follow from
*inferring* a client type from a runtime router value. Here the client is
generated from a checked declaration and then checked, so adding the fortieth
procedure costs the checker one more ordinary module.

`client()` is server-blind by construction: it reads `Api` module *interfaces*
via `moduleInterface`, never their bodies, so a procedure body cannot reach a
client bundle even through the generator. The audience rule and the generator
enforce the same boundary from two directions.

### In-process versus over-wire

`client()` resolves per audience, with no call-site difference:

- In a **server** or SSR context, calls dispatch **in-process** — a direct call,
  no serialization, no HTTP. This is what makes a page's `data` query free
  during SSR.
- In the **client bundle**, calls dispatch over the wire.

This replaces `rpcInProcess` / `rpcClient` as separate generators with one
generator and an audience-determined backend. The same-named-stubs behaviour
from RFC-0022 is preserved.

## The wire

The `?__vyrn=data` marker is removed. A page's data payload is fetched from
**the same URL with `Accept: application/json`** — content negotiation, which is
the technique's real name, understood by every HTTP client in every language,
and leaking no framework identity:

```
GET /p/abc123    Accept: text/html          → the SSR'd page
GET /p/abc123    Accept: application/json   → { "props": …, "title": …, "params": … }
```

The query marker existed only because `Request` carries no headers (RFC-0069's
recorded deviation). This RFC adds `headers: Map<String, String>` to `Request`
and a `Response.vary` field, so the payload response can set `Vary: Accept` and
be cached correctly by intermediaries.

`vyrn-nav` sends `Accept: application/json` instead of rewriting the URL. The
payload body is unchanged, so RFC-0069/0070's `renderPage` / `resolvePage` and
the lazy fill path are untouched.

## Migration

**Delete `contract.vyrn`.** Its procedures move to `server/api/pastes.vyrn`,
losing the forwarding layer — the bodies call `store` directly, since `store`
is now a sibling under `server/`.

**Move the tree.** For `examples/bin`:

| before | after |
|---|---|
| `contract.vyrn` | *deleted* |
| `store.vyrn`, `persist.vyrn`, `util.vyrn` | `server/` |
| *(new)* | `server/api/pastes.vyrn` |
| `wire.vyrn` | `shared/wire/paste.vyrn` |
| `routes/**` | `app/routes/**` |
| `widgets/**` | `app/widgets/**` |
| `client.vyrn` | `client/boot.vyrn` |
| `server.vyrn` | unchanged (composition root) |

`shelf`, `fullstack`, `rpc`, and `rpcsplit` migrate the same way; each is a
mechanical move plus deleting its contract file.

**`vyrn migrate --audience`** performs the move and rewrites import paths, so
the diff is reviewable rather than hand-typed.

## Compatibility

- Applications without an `audience` key in `vyrn.json` are entirely
  unaffected: no segments declared means every module is universal and no import
  is rejected. Adoption is opt-in per project.
- `rpcServer` / `rpcClient` / `rpcInProcess` over a single module keep working;
  they are re-expressed on top of the directory forms.
- The derived path shape (`/_/{module}/{name}`) differs from today's
  `/rpc/{name}`. Existing deployments pin the old shape with
  `{ "rpc": { "prefix": "/rpc", "path": "{name}" } }`, which reproduces current
  URLs exactly — verified by the byte-identical wire pins in the rpc test suite.

## Milestones

- **M1 — audience.** `vyrn.json:audience`, path→audience resolution in the
  loader, the import-widening check with its diagnostic, `vyrn why`. No
  generator changes.
- **M2 — roles + `Api`.** `vyrn.json:roles`, attachment of RFC-0071 contracts by
  role, serializability checking on procedure signatures.
- **M3 — derived paths.** `rpc(dir)` and `client(dir)`; override forms
  (`module.vyrn`-scope and `at()`); collision errors; `vyrn routes`.
- **M4 — the wire.** `Request.headers`, `Response.vary`, content negotiation in
  the page router, `vyrn-nav` sending `Accept`; `?__vyrn=data` deleted.
- **M5 — migration.** `vyrn migrate --audience`; move all five fullstack
  examples; delete every `contract.vyrn`.

## Acceptance

- `examples/bin` has no `contract.vyrn` and no hand-written client stubs.
- A page importing `server/store.vyrn` is a checker error naming both files and
  citing the `vyrn.json` key.
- `vyrn routes` lists every derived and explicit path with its source.
- `vyrn why app/routes/index.vyx` prints audience `universal` and its import
  chains.
- `curl -H 'Accept: application/json' /p/<id>` returns the payload; the same URL
  with `Accept: text/html` returns SSR HTML byte-identical to today's.
- No response anywhere contains the string `vyrn`.
- Three-way parity (interp == native == wasm) green across all migrated
  examples; the rpc wire pins reproduce byte-identically under the compatibility
  `rpc` config.
