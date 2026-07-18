# RFC-0040 — App Ergonomics: Generator Identity, RPC Callbacks, `.vyx` Order

- **Status:** Draft (design locked)
- **Depends on:** RFC-0021/0031 (generator identity + cache keys), RFC-0037
  (stored closures — the RPC-callback unlock), RFC-0039 (`.vyx` v2 — the
  script-section order rule lands there), RFC-0019 (`std/rpc` — the
  `on<Proc>` convention being retired)
- **Evidence (user review of examples/bin):** imports appear AFTER
  `props {}` in `.vyx` scripts (backwards by Vue convention and Vyrn's own
  imports-first module rule); `labels.vyrn` is seventeen one-line wrapper
  functions existing only because two spellings of the same generator arg
  (`"./strings"` vs `"../strings"`) create two colliding stateful modules;
  `client.vyrn` carries mandatory NO-OP `on<Proc>` handlers for procedures
  it never calls; `server.vyrn` lazily installs its middleware from inside
  `handle` behind a length check; and the examples read like annotated
  tutorials rather than app code.

---

## 1. Synthesized-module identity keys on RESOLVED inputs (loader)

A generator import's module identity currently keys on the raw argument
text, so `i18n("./strings")` (root) and `i18n("../strings")` (a widget)
synthesize TWO modules whose module state collides — the RFC-0039
"one thin wrapper module" pattern exists only to dodge this.

**Change:** the loader keys synthesized-module identity by (generator
name, generator sources, **resolved** path arguments — the same resolved
roots the sandbox and the cache key already compute — plus non-path args
verbatim). Two imports whose path args resolve identically ARE the same
module: one instance, shared state, no collision. The gen CACHE key
gains nothing new (it already folds resolved roots); this aligns
identity with it.

- Consequence in bin: widgets and root import `i18n("<their-relative
  spelling>/strings")` directly; **`labels.vyrn` is deleted** (its one
  real function, the explicit-locale plural demo, moves to the about
  page's script). Shelf's equivalent threading simplifies the same way.
- Re-exports (`export { X } from …`) stay deferred: with identity fixed,
  the facade module that demanded them is gone.

## 2. `std/rpc` client v2: callbacks as stored values (retiring `on<Proc>`)

RFC-0023 kept the `on<Proc>` convention "until closures can be stored."
RFC-0037 delivered storage; the convention retires:

```vyrn
import { rpcClient } from "std/rpc"
import * as api from rpcClient("./contract")

api.createPaste(req, |res| match res {
    Valid(r) => afterCreate(r),
    Invalid(iss) => setIssues(renderIssues(iss)),
})
```

- Each generated stub takes `(req, cb: fn(Validation<Ret>))`; the
  generated module holds `let mut pending: Map<String, fn(Validation<…>)>`
  per procedure (fn-typed Map values — RFC-0037), keyed by call id; the
  extern completion path invokes and removes the callback. No handler
  needs to exist for a procedure the client never calls — bin's no-op
  `onListPastes`/`onGetPaste` are deleted.
- `rpcInProcess` mirrors the same signature (calls the callback
  synchronously). The wire, server, `connectClient`, and error semantics
  are UNTOUCHED — this is client-surface only.
- Migration: bin + shelf clients move to callbacks; the `on<Proc>`
  emission is REMOVED (one convention, not two — pre-1.0, the corpus is
  ours). vyrn-query.js/vyrn-rpc.js host glue is re-verified against the
  new dispatcher shape (adjust the JS if the completion plumbing shifts;
  behavior identical from the page's view).

## 3. `.vyx` script order: imports first (std/vyx)

Imports must precede `props {}` / `params {}` in a `.vyx` script section
— matching Vue's `<script setup>` and Vyrn's module rule. A `props`
block before the last import is a named generation diagnostic
(`VYX_IMPORTS_FIRST`, naming the file/line). All repo `.vyx` files
migrate; the emitted module is unchanged (this is source order, not
semantics).

## 4. Middleware init honestly (bin/server + a verification)

`let mut middleware: Array<Middleware> = [ |req| … ]` — the chain
installs at module-state init, deleting the lazy `installMiddleware()` +
length-check from `handle`. VERIFY a lambda literal is legal in a
module-state initializer (nothing runs at init but construction; the
RFC-0029 init restrictions govern CALLS, not values) — if the checker
rejects it, that is a reportable gap in RFC-0029/0037 composition, not
something to paper over (report before working around).

## 5. Example style pass (bin, then shelf where egregious)

Examples are apps, not tutorials: comments state constraints the code
can't show, nothing else. RFC citations, mechanism narration, and
teaching asides move to the NOTES files or get deleted. `main()` smokes
stay (they are the serve-app test surface) but lose their narration.

## Out of scope

Re-exports (deferred again, see §1), clock/random and std/storage (the
next design round), `v-model`, any server/wire change, `on<Proc>`-style
compat shims (clean break).
