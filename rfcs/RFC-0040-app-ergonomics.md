# RFC-0040 — App Ergonomics: Generator Identity, RPC Callbacks, `.vyx` Order

- **Status:** Implemented (§1–§5; the recorded downstream wall is now RESOLVED — see as-landed notes)
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

---

## As-landed notes

- **§1 / §2 (prior commits `6a7761e`, `caccec9`):** generator-import identity
  keyed on resolved inputs; std/rpc client callbacks; `on<Proc>` retired.
- **§3 (`a074da1`) — `.vyx` imports-first:** a `props`/`params` block that
  precedes an import in a script section is `VYX_IMPORTS_FIRST` (file/line). The
  check runs on the ORIGINAL source in `vyxCompileComponent` (props) and
  `vyxBuildPageModule` / `vyxPageShape` (params); the page synthesizer now trails
  its synthetic `props` block AFTER the page's imports so it satisfies the rule.
  Emitted modules are byte-identical (proven via `emit-gen` diff on `vyxdemo` —
  source order only). All repo `.vyx` migrated (CreateForm, PasteView, ShelfApp,
  Listing, Row). Tests: 6 in-language + 2 integration.
- **§4 (`6c78c64`) — labels.vyrn deleted, middleware honest:**
  `examples/bin/labels.vyrn` (17 wrappers) is gone; each widget/page imports
  `i18n("<its-spelling>/strings")` directly (§1 shares the one instance), and the
  explicit-locale plural demo moved to the about page as a local `countIn` helper.
  **VERDICT — a lambda literal in a module-state initializer is LEGAL:** RFC-0029's
  init restrictions govern CALLS, not construction, and the checker accepts it
  (verified interp + server smoke — logging fires per-request). `bin/server.vyrn`
  now installs its chain at init (`let mut middleware: Array<Middleware> = [ |req|
  … ]`), deleting the lazy `installMiddleware()` + length-check. No RFC-0029/0037
  composition gap. Shelf's `Labels` record was left as legitimate root-owned state
  (its i18n imports are all `"./strings"` at the root, leaf widgets take resolved
  scalar props — it never had the two-spelling collision labels.vyrn dodged).
- **§5 (`6a9b8cd`) — style pass:** RFC citations, mechanism narration, and
  teaching asides stripped from bin (thorough) and shelf (egregious first);
  shelf's server middleware also moved to the same direct init as bin (deleting
  its lazy-install wart). `server.vyrn` 89→72, `client.vyrn` 154→142 (bin).

### Verification & downstream wall

- **SSR verified (via `vyrn serve`, no wasm client needed):** bin — home list
  `2 pastes` (en), `/about` en+uk CLDR plurals (`1 вставка / 2 вставки / 5 вставок`
  — the RELOCATED demo, and proof the §1 shared instance flips uk then restores en
  for the home render), `/p/<id>` HTML-escaped, `/raw/<id>` byte-exact
  `text/plain`, 404s, **restart survival** (2 pastes reload from disk). shelf —
  home (3 books), `/about` en+uk plurals, `/books/1` loader, `/admin` guard **403**
  (middleware direct-init), 404/422, openapi+graphql. Full suite 900 + full parity
  green at every commit; fullstack `rpc.rs` passes.
- **WALL (pre-existing, NOT this RFC's §3–§5) — RESOLVED (follow-up task_9c712034).** The §2 callback clients could not build to
  wasm/native: `vyrn dev` failed with `error: unbound cb` for bin, shelf, AND
  fullstack (untouched by §3–§5, so it predated this work and was never covered —
  the parity harness and `rpc.rs` only exercise interp/serve). Root cause was an
  RFC-0023 × RFC-0037 codegen composition gap in `vyrn-codegen`: a monomorphized
  `fn`-value PARAMETER lives in `fn_bindings` (a known defunctionalization target +
  its capture SSA values) with NO local slot, so using it as a VALUE — `pending[k]
  = cb`, `let g = cb`, `return cb`, a record field, an array push — fell through
  `gen_expr`'s `Expr::Var` arm to `unbound`. (It was signature-agnostic: `fn(Int64)`
  failed too the moment it was STORED rather than immediately called; scalar
  "worked" only where the value was consumed inline.) **Fix:** the `Expr::Var` arm
  now materializes the same `{ i64, i64 }` aggregate a lambda/named source builds
  via a new `construct_fnval_binding` — box the binding's captures by value (empty ⇒
  payload 0), `register_fnval` the (signature, target, captures), return the
  aggregate — so STORING a fn-param never diverges from CALLING it, for any payload
  (scalar or non-scalar, no record special-case). The RFC-0023 IR invariant holds
  (every emitted `call` still names an `@symbol`; zero wasm table entries). Proof:
  a new three-way parity citizen `examples/fnvalstore.vyrn` (scalar / record /
  `Validation<Record>` / `Result<Record,String>` payloads stored via a fn-param
  into a Map, an Array, and a record field; a record-capturing closure; drop of a
  map of closures) — the exact path the harness was blind to — plus a
  `stored_fn_param_compiles_for_any_payload` codegen regression test; bin, shelf, and
  fullstack clients now build to wasm; and BROWSER-VERIFIED islands over `vyrn dev`:
  bin create → soft-nav to `/p/<id>` (`createPaste -> 200`), shelf add/rate/delete
  (two-step confirm)/tag-filter/locale-toggle (`addBook`/`rateBook`/`deleteBook ->
  200`). 900 workspace tests (unchanged; the regression test is `#[ignore]`,
  needs clang) + full three-way parity green (now including `fnvalstore.vyrn`).
