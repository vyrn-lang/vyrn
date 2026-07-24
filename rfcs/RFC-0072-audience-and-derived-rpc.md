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

The move is a `git mv` plus import-path rewrites, done by hand across five
examples. No migration command: Vyrn is pre-1.0 with no users, so a tool to
mechanize a one-time move of code we own would cost more than the move.

## Compatibility

- Applications without an `audience` key in `vyrn.json` are entirely
  unaffected: no segments declared means every module is universal and no import
  is rejected. Adoption is opt-in per project.
- `rpcServer` / `rpcClient` / `rpcInProcess` over a single module keep working;
  they are re-expressed on top of the directory forms.
- The derived path shape (`/_/{module}/{name}`) differs from today's
  `/rpc/{name}`. The `rpc` config exists so a project can choose its own URLs,
  not as a compatibility shim — nothing is deployed against the old shape. The
  repo's wire pins are updated to the new one in the same change.

## Milestones

- **M1 — audience.** `vyrn.json:audience`, path→audience resolution in the
  loader, the import-widening check with its diagnostic, `vyrn why`. No
  generator changes. **Landed** — see below.
- **M2 — roles + `Api`.** `vyrn.json:roles`, attachment of RFC-0071 contracts by
  role, serializability checking on procedure signatures. **Landed** — see below.
- **M3 — derived paths.** `rpc(dir)` and `client(dir)`; override forms
  (`module.vyrn`-scope and `at()`); collision errors; `vyrn routes`. **Landed** —
  see below.
- **M4 — the wire.** `Request.headers`, `Response.vary`, content negotiation in
  the page router, `vyrn-nav` sending `Accept`; `?__vyrn=data` deleted.
- **M5 — the move.** Move all five fullstack examples; delete every
  `contract.vyrn`.

## M1 — as landed

Five places where the implementation is not what this document said, and why.

**A composition root needed a rule, and the manifest already had one.** This
document's migration table leaves `server.vyrn` at the project root, "unchanged
(composition root)" — and a root at the project root has no audience segment, so
it is universal, so it may not import `server/`. The one module whose entire job
is to name both sides would have been the first thing the rule rejected. Adding a
segment for it would move a file the document says not to move; blessing the
*name* `server.vyrn` would be exactly the hardcoding the `audience` key exists to
avoid. So an entry point takes the audience of the `vyrn.json` key that NAMES it:
`"server": "server.vyrn"` is server-only, `"client": "client.vyrn"` is
client-only, `"main"` is server-only (a `main` runs on the machine it was built
for). Those keys are already in every fullstack example's manifest, so nothing
new is declared and `client.vyrn` reaching into `server/` is still an error —
which is the leak worth catching.

**A file named `server.vyrn` is not otherwise a server module.** Audience is read
from DIRECTORY components only. Reading the filename too would have made every
existing example's entry point server-only overnight, without anyone opting in,
and "the nearest segment on its path" is a statement about directories in every
example this document gives.

**A generated module's audience comes from the generator's INPUT, not its call
site.** A `.vyx` page compiles to a module whose key is a banner ending at the
root that mounted it, so borrowing the calling module's audience would have given
every page the audience of `server.vyrn` and quietly exempted the entire page
tree — the exact case the rule exists for. The banner also carries the
generator's own argument, so a generator with one input file (`vyxPage("./app/
routes/index.vyx")`) lends its module that file's audience, and a generator
pointed at a DIRECTORY (`pages("./app/routes")`) does not, because router glue
has no single origin and inherits its caller.

**The advice line is a note, not a second error line.** `Diagnostic` carries one
message and one optional note; the sketch above renders as two `=` lines because
that is how rustc prints, and Vyrn's printer has one. Both facts the sketch
carries — the `vyrn.json` key that declared the audience, and what to do instead
— are in the note, together with what decided the IMPORTER's audience, which the
sketch omits and which is the other half of the question.

**`vyrn why <file>` reads the tree, not a build.** Import chains come from a
lex-and-parse scan of every `.vyrn`/`.vyx` under the project, resolved through
the loader's own `resolve_spec` — no load, no generators, no link. The file whose
audience you are asking about is quite often the one that does not compile, and a
`why` that needed a successful build would be unavailable exactly when it is
wanted. Generator imports contribute edges too (`pages("./routes")` reaches every
page under it), because a chain that stopped at the generator call would omit the
only edge anyone is asking about.

## M2 — as landed

Five places where the implementation is not what this document said, and why.
This milestone carries RFC-0071's deferred M3.

**A role scope may be a RUN of segments, and that is how the two axes compose.**
This document shows `"roles": { "api": "std/rpc:Api" }` — one segment — while the
layout it proposes puts `api/` under both `server/` and, in principle, anywhere
else. A one-segment scope cannot tell those apart, so whichever role matched
first would silently govern both. `RoleScope::Segment` now holds a run
(`"server/api"`), matched consecutively, and role scopes are scored by the index
of their LAST matched component — the same "nearest wins" rule audience uses,
deliberately, because two axes read off one path have to agree about what "more
specific" means. The one-segment form is the degenerate case and is unchanged.

**`Api` states the surface; the generator states the rule.** The open rule
constrains the return type only, and a procedure's return is a member type
parameter, so `contract Api { fn *(..) -> R }` is the most the contract grammar
can say — every export of an `api` module is a procedure. Serializability is a
property of the TYPES, not of the signature shape: "at most one parameter, both
ends nameable by the module's own reflection" is unspellable in a contract and
stays in `std/rpc`, where it now lives as one named predicate
(`rpcIsSerializable`) that every message cites. What changed is that it is a rule
with a name instead of an inline scan, and that it applies to the RETURN, which
nothing checked before — a non-serializable return used to reach the wire as a
`null` schema and a client-side decode failure at run time.

**`validateContract` had to become a `gen fn`, and three-way parity is what said
so.** `contractOf` is compile-time reflection with no runtime lowering by design
(RFC-0071 M1), and `std/rpc`'s plain `fn` is linked into every binary that
imports the module. The interpreter was unaffected, so it was invisible until the
native leg of `examples/rpc.vyrn` refused to build. Same shape as `std/ui`'s
`uiContractErrs`, same fix, and every caller was already a `gen fn`.

**`Page` names `page` and `respond`, which closes the item deferred three
times.** RFC-0071's M2b, M2c and M4 each recorded that a `.vyrn` page's own
surface carries the router's entry point, which `Page` did not name, so the
CLOSED rule could not be applied to `.vyrn` pages — leaving typo detection, the
entire point of declaring the contract, working on `.vyx` pages only. The
alternative was a role-aware surface filter, which is a scanner with better
manners: it would hide `page` from the contract while the router went on
requiring it, so the declaration would still not be the truth. Naming them is the
truth — both at four shapes, following `head`'s own house rule that a member
takes what the view takes, and both optional, because WHICH of the two a page
must export is the router's rule (`uiInspectPage` still owns it) while what a page
MAY export is the contract's. `fn hedd()` in a `.vyrn` page is now an error naming
`head`.

**One test changed meaning, correctly.** `page_type_error_remaps_to_the_page_module`
proved RFC-0033 origin remapping using a page whose `page()` returned `Int64` —
which the contract now rejects at the declaration, before any glue is generated.
It uses a mismatch between `data`'s type and `page`'s parameter instead: a member's
type parameters are open, so the contract genuinely cannot object, which makes it
the honest example of an error that must survive into generated code to be caught
at all.

## M3 — as landed

Seven places where the implementation is not what this document said, and why.

**The overrides are `rpc.json`, not `module.vyrn` and `at()`.** Both forms this
document proposes require a generator to *evaluate* a module — `fn rpc() -> Rpc`
returns a record, `at(list, "/pastes/recent")` names a function value — and a
generator can only REFLECT over a module through `moduleInterface`, which yields
names and types. The only way to reach a call expression from a generator is to
read the module's source and scan it for `at(`, which is precisely the technique
RFC-0071 spent four milestones deleting, and it would have been reintroduced in
the same repository in the same season. So the overrides are data a generator can
actually read: an `rpc.json` beside the api directory, carrying the same
`prefix`/`path` keys plus a `pin` object mapping `{module}/{name}` to an explicit
path. Declaration-scope pinning is therefore directory-scope pinning of one
declaration, which is a real loss of locality and the honest price of not owning
a language change in this milestone.

**The project-wide `rpc` key cannot live in `vyrn.json`.** RFC-0021 confines a
generator's `readFile` to the constant path arguments it was handed, and walking
up from `server/api` to the project root is exactly the escape that sandbox
exists to refuse — the loader says so by name. The keys are unchanged and the
`{"rpc": {…}}` wrapper is accepted verbatim; the file is one the generator is
allowed to open. Teaching the loader to hand generators a project config is a
language change, and a worthwhile one, but not this milestone's.

**Procedure modules are reached through a NAMESPACE, not an import alias.** Two
api modules both exporting `get` is not a mistake — it is the normal shape of a
directory-derived API, and the whole reason `{module}` is in the template. But
Vyrn's namespace is flat, so `import { get as pastesGet__real }` makes
`pastes/get` and `orders/get` a "defined in both" error before the generator can
say anything about either. RFC-0027's `import * as` is the one construct that
keeps a module's exports out of the flat namespace, so each module binds as
`rpcM0`, `rpcM1`, and handlers dispatch through it. Types stay named imports: a
type reached across modules is one declaration and collides with nothing.

**The client's procedures are flat qualified names, not nested namespaces.** This
document writes `api.orders.refund.run`, which is a namespace inside a namespace;
`import * as` binds exactly one level. The generated stub is `ordersRefundRun`,
the module tree read left to right, so every procedure stays distinct in the flat
namespace and the tree is still visible in the name. `api.pastes.list()` is a
language feature away, not a generator away.

**`client()` and `clientInProcess()` are two generators, not one dispatching on
audience.** A generator receives its arguments and nothing else — not the
audience of the module that imported it — so `client()` cannot choose its own
backend without the loader passing audience into generation. The stubs are
same-named across both, exactly as `rpcClient` and `rpcInProcess` already were,
so a composition root swaps one import line. Everything this document says about
in-process dispatch being free during SSR holds; what it costs is the import
line, not the call site.

**`vyrn routes` reads directives; it does not recompute.** The generator that
mounts the surface emits one `//@route METHOD PATH PROCEDURE SOURCE` comment per
route, and the command reads them back out of `emit-gen`'s output. Recomputing
the table in Rust would have created a second implementation of the derivation
rule that could disagree with the router actually serving traffic — the exact
failure mode RFC-0071 M4 avoided by making the LSP a pure adapter over
`vyrn_frontend::contracts`. One producer, one table.

**`list` is a reserved name, so this document's own example cannot be written.**
`server/api/pastes.vyrn :: list` is the illustration used throughout; `list` is
reserved (the removed `list([..])` array form), so the tests spell it `recent`.
Nothing about the derivation changes — it is worth recording only because the
example reads as though it were runnable.

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
  examples; the rpc wire pins are updated to the derived shape and pinned there.
