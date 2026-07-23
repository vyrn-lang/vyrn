# RFC-0069 — Universal Pages: Nuxt-Mode Navigation

- **Status:** Implemented
- **Depends on:** RFC-0026 (`.vyx` → pure view fns over `std/html` — the
  fact that makes this possible), RFC-0039/0041 (pages, layouts, `load()`,
  error pages), RFC-0067 (soft nav v2 — the fallback layer and the
  navigator this upgrades), RFC-0068 (wire codec discipline), RFC-0019
  (contract stubs)
- **Evidence (user):** "But this isn't how Nuxt works." — correct. Nuxt
  never fetches HTML on navigation: pages live in the client bundle and
  navigation renders client-side, fetching only a JSON data payload. v2
  fetched full HTML because `.vyx` pages compiled into the server module
  ONLY — the client had no way to render `/about`. This RFC closes that.

The target model, locked:

- **First load**: full SSR, exactly as today (no-JS, curl, SEO see
  complete pages; byte-identical output — the server's HTML rendering
  does not change).
- **Navigation**: the client renders the next page ITSELF from its
  compiled view fn, fetching only `{ page, title, props }` as JSON.
  No HTML transfer, no shell re-render, no hydration circus (client
  rendering starts only at the first navigation — the SSR'd first page
  is never re-rendered).
- **Fallbacks, in order**: page not in the client bundle → v2 HTML swap;
  anything else odd → v2's hard nav. Each layer degrades to the one
  below; nothing gets worse than today.

---

## 0. The page-data discipline (prerequisite)

A client-rendered page must be PURE over its props. Data enters through
the RFC-0041 `load()` convention only — an inline contract call in a
page's script helpers (bin's `index.vyx` `recent()` calls `listPastes()`
mid-view today) cannot run client-side (stubs are async callbacks).

- `load()` is extended to STATIC routes: a page may declare a
  zero-parameter `fn load() -> T` (index's becomes
  `fn load() -> Array<Paste>`); dynamic routes keep `load(id)`.
  A page with no `load` has empty props.
- The client-bundle compile of a page that still calls a contract stub
  inline must fail with a NAMED, actionable diagnostic (the natural
  "unknown/server-only fn" failure is upgraded to say: "page helpers run
  client-side; move the data into `load()`").
- `examples/bin` migrates (`index.vyx` → `load()`), as the showcase.

## 1. The client page bundle

The pages generator gains a client counterpart (e.g. `pagesClient(dir,
theme)` in `std/ui`, sharing ALL of the existing `.vyx` compilation):

- Compiles the same `routes/` tree — pages + `error.vyx` — into the
  CLIENT module: one view fn per page (props-typed, exactly what
  `vyxPageThemed` already produces), plus a ROUTE TABLE mapping path
  patterns (`/`, `/about`, `/p/[id]`) to page ids.
- Exports a single host-facing entry the wasm exposes:
  `renderPage(payloadJson: String) -> String` — dispatch on `page`,
  decode `props` through the wire codec (the types are the page's own
  wire types), call the view fn, return the `std/html` tree as JSON for
  `vyrn-dom` to paint. Unknown page id → a distinguished reply so the
  host falls back to the v2 HTML swap.
- The LAYOUT is not in the bundle: the shell's DOM persists (nav swaps
  `<main>` only, as in v2). Page `head{}` `title:` travels in the
  payload; head asset entries (rare on pages) go through v2's additive
  head machinery.
- `examples/bin/client.vyrn` imports it alongside the existing island;
  `vyrn dev` needs no change (it already builds the client).

## 2. The payload protocol (server)

- A soft nav requests the SAME path with a data marker. Transport:
  prefer the `x-vyrn-nav: data` header IF the server `Request` already
  exposes headers; otherwise the query convention `?__vyrn=data`
  (implementer verifies which and documents — do NOT widen `Request`
  for this).
- The generated server `route()` answers a marked request with
  `application/json`:
  `{ "page": "p/[id]", "title": "<rendered title>", "props": <load result via wire codec> }`
  — running the page's `load()` exactly as it would for SSR. A miss
  renders the error page's payload:
  `{ "page": "@error", "status": 404, "props": { … } }`.
- An UNMARKED request is byte-identical to today's HTML (pinned).
- Payloads are typically a few hundred bytes — record real sizes for the
  bin routes in the as-landed notes.

## 3. The client router (vyrn-nav v3)

- Soft nav: fetch with the data marker. `application/json` response →
  set `document.title`, call the wasm's `renderPage`, paint the tree
  into `<main>` via `vyrn-dom`, re-sync islands (the existing registry —
  the create island keeps surviving with its draft). HTML response →
  the v2 swap path unchanged. Failure anywhere → hard nav.
- popstate/scroll/progress-bar behavior identical to v2 (incl. the
  150ms-armed bar).
- If the client bundle is absent (no wasm, no-JS page, or `renderPage`
  missing), the navigator behaves exactly as v2 — feature-detect once.

## Verification

1. In-browser (or user click-path if the pane is flaky): after first
   load of `/`, navigating `/` ⇄ `/about` ⇄ `/p/<id>` transfers ONLY
   JSON payloads (network log: no HTML document fetches, no assets);
   sizes recorded. Back/forward, draft-survival, 404 (`@error` payload
   renders the themed error page client-side), and the not-in-bundle →
   HTML fallback all exercised.
2. Server: marked vs unmarked responses pinned (unmarked HTML
   byte-identical to pre-RFC; marked JSON schema + a `load()` props
   round-trip through the wire codec). Rust integration tests drive
   both against the bin server's `handle`.
3. Client: `vyrn test` on the client pins `renderPage` dispatch (known
   page, unknown page, `@error`), props decode, and title threading.
4. The inline-contract-call diagnostic pinned (index.vyx pre-migration
   shape as the negative fixture).
5. emit-gen diffs reviewed (server module gains the payload branch;
   SSR HTML output byte-identical); full suite + LSP + three-way parity
   green; fmt clean; 0 new clippy warnings; LSP redeploy only if
   frontend changes (hash-verify either way); release CLI rebuilt.

## Out of scope

Hydrating the FIRST page (client rendering starts at navigation),
prefetching payloads, streaming/partial hydration, per-component code
splitting, scroll restoration beyond v2, and offline caching.

---

## As landed

Shipped across five commits (M0–M4). Everything is in the generators
(`std/ui`, `std/vyx`), the runtimes (`web/vyrn-nav.js`, `web/vyrn-dom.js`), and
`examples/bin` — **no Rust frontend/compiler source changed** (only a new Rust
*test* file), so the deployed LSP binary is untouched (hash unchanged, below).

**M4 (the Nuxt correctness fix)** closes the last gap: v3 as first shipped
fetched the JSON payload on EVERY soft nav, including pages with no `load()`
(`/about`'s payload was ~61 B of `props:null`). That is not the Nuxt model —
Nuxt navigates a dataless page with ZERO network; the page's script decides, and
declaring `load()` IS the declaration (exactly like `useAsyncData`). M4 makes
navigation script-driven with no app-authored flags: the client bundle exports
`resolvePage(path)` and the navigator resolves the target locally FIRST, so a
dataless route transfers **0 B on the wire**.

### Transport verdict: query marker (not a header)

The server `Request` record exposes only `method`/`path`/`body` — no headers
(verified in `interp.rs`'s `handle_request`, which builds the record from those
three fields). Per §2 that settles it: the data marker rides the query string as
**`?__vyrn=data`**, never a header. `Request` was NOT widened. The generated
`uiRouteSegments` already strips the query before matching, so a marked request
routes to the same page; `uiIsDataRequest` (a substring scan for `__vyrn=data`)
gates the data channel.

### What moved where

- **M0 — page-data discipline** (`std/vyx`, `std/ui`, `bin/routes/index.vyx`).
  `load()` extended two ways: a zero-parameter static `fn load() -> T` (no
  `params` block, `page(d)`), and a third loader kind **`plain`** — an unwrapped
  data type with no failure path, because §0 writes `fn load() -> Array<Paste>`
  literally. `VyxPageShape` gains `loadHasParams`; `UiPageInfo` gains
  `loadHasParams` + the `plain` `loadKind`; `uiEmitLoadDispatch` factors the
  server loader render (plain binds `d`, wrapped matches its arms). `index.vyx`
  migrated `recent()`/`pasteTally()` into `fn load() -> Array<Paste>`; the body
  is now pure over `data`. SSR stays byte-identical for `/`, `/about`, `/p/[id]`,
  and the 404 (diffed against a pre-change capture).
- **M1 — client bundle** (`std/vyx`, `std/ui`, `bin/client.vyrn`).
  `vyxPageClient(Themed)` compiles a page for the client: the SAME template
  compilation, with `load` and its now-dead imports stripped (a *conservative*
  prune — only names the stripped `load` alone used are dropped, from a
  comment-stripped corpus) and `head`/`headTitle` omitted. Each client page
  module exports `clientRender(propsJson, paramsJson) -> String`, decoding
  props/params through the wire codec *inside* the module (a generic data type is
  aliased, since `fromJson` needs a named type) and returning the `std/html` tree
  as JSON. `pagesClient`/`pagesClientThemed` synthesize the bundle:
  per-page client imports, a url-pattern route table, and
  `renderPage(payloadJson)` that dispatches on `page`, hands props/params to the
  page's `clientRender`, renders `@error` through the (pure, reused) themed error
  page, and returns `"__vyrn_fallback__"` on an unknown page or bad JSON.
  `bin/client.vyrn` imports it and exports `vyrnRenderPage`.
- **M2 — server payload** (`std/ui`, `std/vyx`). The generated `route()` gains a
  data channel, emitted **only when the routes tree has a client-renderable
  `.vyx` page** (a `.vyrn`-only router emits the pre-RFC module byte-identical).
  Per client page, `uiTryData<n>`/`uiRenderData<n>` mirror the HTML try/render
  (shared `uiEmitTryFn`), run `load()` exactly as SSR, and answer
  `200 application/json {page, title, props[, params]}`; a `Result` `Err` /
  `Validation` `Invalid` answers the `@error` payload. `route()` on a marked
  request: a client page → JSON; a valid non-client route → its real Response
  (non-JSON → the client hard-navs); a true miss → `uiDataMiss` (`@error`). The
  payload runtime helpers live in `std/ui` as ordinary exported fns.
- **M3 — vyrn-nav v3** (`web/vyrn-nav.js`, `web/vyrn-dom.js`, `bin/public/app.js`).
  A soft nav fetches the marker; `application/json` → set title, wasm
  `renderPage`, paint the tree into `<main>` via `vyrn-dom`'s new standalone
  `renderTree`, re-sync islands; `text/html` → the v2 swap unchanged; anything
  else / any failure / the fallback sentinel → hard nav. Feature-detected once
  via `setPageRenderer` (the island boot hands the wasm's `renderPage` over);
  until set, v3 IS v2. popstate/scroll/progress-bar identical to v2.
- **M4 — script-driven nav** (`std/ui`, `std/vyx`, `web/vyrn-nav.js`,
  `bin/client.vyrn`, `bin/public/app.js`). The client bundle gains a second host
  entry, `resolvePage(path) -> String` (`{found, hasData, page, title}` JSON),
  synthesized from the SAME load-detection M0/M1 already carry (`hasData` is just
  `UiPageInfo.hasLoad`; no hand-maintained table, no app flag). A per-page
  `uiClientMatch<n>` mirrors the server's `uiTryData` routing — segment count plus
  every literal segment; dynamic segments accept any non-empty value (a data page's
  marker fetch re-runs the server's exact Int-parse routing, and a static dataless
  page has no dynamic segment to disagree on, so no per-type parse is duplicated
  client-side). `vyrn-nav` gains `setPageResolver`; on a soft nav it resolves the
  target with NO network — a known dataless page renders immediately from
  `{page, props:null, params:null}` through the existing `renderPage` (zero fetch),
  a data page fetches its payload exactly as M3, an unmatched path falls through to
  the v2 HTML swap. An older bundle that hands over only the renderer keeps the M3
  fetch-every-nav behavior (feature-detected). **The title fix:** the client bundle
  now KEEPS `headTitle` for a dataless page (a loader page's title depends on data
  and still travels in the payload), so `resolvePage` reads the page's own compiled
  title expression — `""` when it declares no `title{}`, in which case the
  navigator leaves `document.title` at the layout default rather than overwriting it
  with the url-pattern. This corrects the `title:"/about"` placeholder the user saw:
  the server payload still carries `"/about"` (server unchanged), but the client no
  longer FETCHES `/about`, so that placeholder never reaches the tab.

### Deviations (with justification)

1. **The payload carries `params` for dynamic routes** — the locked triple is
   `{page, title, props}`, but a client-rendered dynamic page's view fn is
   `page(p: Params, d: Data)` and needs its URL params, which `props` (= the
   `load` result) does not include. So a dynamic route's payload adds
   `"params": <toJson(Params)>`; static/loader-only pages carry `"params":null`.
   `renderPage` keeps its single-argument signature (the params ride the same
   envelope), so this is the minimal sound closure of the locked shape rather
   than a redesign.
2. **Props/errors serialize through a typed indirection.** `toJson` on a value
   bound from a `Result`/`Validation` match arm (`Ok(d)`, `Err(e)`) cannot infer
   its type (a checker limitation — a *parameter* carries the type, which is why
   SSR's `page(p, d: Data)` works). So the loaded data goes through the page
   module's typed `encodeProps(d: Data)` and a load error through `std/ui`'s
   `uiErrorResponseOf(e: PageError)`. An implementation necessity, not a design
   change.
3. **The inline-contract-call diagnostic is a heuristic.** After the load strip +
   prune, a relative-app-module import whose name is CALLED in the client view
   trips `page_helpers_run_client_side__move_the_data_into_load__<name>`. It
   precisely catches the canonical mistake (`recent()` calling `listPastes()`
   mid-view); a genuinely pure cross-module helper called in a view would also
   flag, with the same actionable guidance (move it into `load()` or make it a
   local helper). No example in the repo trips it falsely.
4. **The renderer is available once the `#app` island boots.** `app.js` loads via
   `index.vyx`'s `head`, so a hard land directly on `/about` (no `#app`, no
   `app.js`) stays pure v2 until a nav reaches a page carrying the island; the
   common flow (land on `/`) gets data-channel nav immediately, and the wasm
   instance then survives every nav (RFC-0067).
5. **Nested error pages** use the root `error.vyx` (`errors[0]`) in the client
   bundle; a nested error tree would need the payload to name which error page
   (out of scope — bin has one).

### Payload sizes (bin, `application/json`) — after M4

| route | has `load()` | on the wire per nav | server still answers marked |
| --- | --- | --- | --- |
| `/` | yes | 1498 B payload (props = full paste array) | yes |
| `/about` | **no** | **0 B — the client never asks** | yes (~61 B, for direct callers) |
| `/p/<id>` | yes | ~1354 B (large) / ~200 B (small) | yes |
| `@error` (404) | — | ~100 B (rides a data page's marker fetch) | yes |

Before M4 every nav — including `/about` — fetched the JSON payload; after M4 a
DATALESS route (`/about`) transfers nothing on the wire (the client resolves it
locally and renders from null props). A DATA route still transfers only its JSON
payload — no HTML document, no wasm/CSS/JS refetch. The server payload protocol is
unchanged: `/about?__vyrn=data` still answers ~61 B for progressive-enhancement or
direct callers; the client simply stops ASKING.

### emit-gen summary

Reviewed the generated server module: `route()` gains, **only** when the tree
has a client-renderable `.vyx` page, the marker check plus a per-client-page
`uiTryData<n>`/`uiRenderData<n>` pair; the SSR `uiTry<n>`/`uiRender<n>` and
`document(...)` output are **unchanged** (unmarked HTML byte-identical, verified
by diff and by the Rust integration tests). A `.vyrn`-only router
(`examples/pagesdemo.vyrn`) emits the pre-RFC module byte-for-byte — confirmed
green by the existing `pages` integration suite. M4 adds to the CLIENT bundle only:
a per-page `uiClientMatch<n>`, a `resolvePage` export, and a kept `headTitle` on the
dataless page (reviewed in `vyrn emit-gen bin/client.vyrn` — `/about` resolves to
`hasData:false` + `headTitle()`, the two data pages to `hasData:true`); the server
module is untouched.

### Verification

- Rust integration tests (`compiler/vyrn-cli/tests/universal_pages.rs`, a shared
  bin server in a temp cwd with an empty seeded store): unmarked HTML unchanged,
  the exact static `/about` payload, the home-list payload, a `/p/[id]`
  props+params round-trip, the `@error` payload on a miss, and a non-client
  `/raw` route falling back to its real (non-JSON) response.
- `vyrn test` on `bin/client.vyrn` (18 green): `renderPage` dispatch for `/`,
  `/p/:id`, `/about`, and `@error`, unknown-page + malformed-JSON fallback, the
  create-island tests unchanged, and (M4) `resolvePage` marking `/about` as
  `hasData:false` with an empty title, `/` and `/p/:id` as `hasData:true`,
  query-stripped matching, an unknown-path miss, and a dataless page rendering from
  a null-props payload (the zero-fetch dispatch).
- The inline-contract-call diagnostic pinned against a pre-migration
  (`recent()`→`listPastes()`) fixture.
- `node --check` on all three runtimes; `vyrn dev` serves the v3 nav + vyrn-dom +
  `app.js` + `client.wasm`; the client wasm exports `vyrnRenderPage` and (M4)
  `vyrnResolvePage`; zero real `store` file-I/O is pulled into the client bundle
  (the boot trap the strip exists to prevent).
- Server unchanged (M4 is client-side): `cargo test -p vyrn-cli --test
  universal_pages -- --ignored` stays 7/7 green (unmarked HTML byte-identical, the
  marked `/about` payload still the ~61 B `title:"/about"` shape — the client just
  stops fetching it). One cold run (generation cache disabled, debug build) was
  ~46 s wall.

### Browser click-path (post-merge proof — the in-repo pane is flaky)

1. Hard-load `/`. The create island boots (`#app`) and registers BOTH the renderer
   and (M4) the resolver.
2. Click **About** → the Network log shows **NO request at all** (the key M4 pin:
   `resolvePage("/about")` returns `hasData:false`, so the client renders `/about`
   from null props with zero network — no `?__vyrn=data`, no HTML document, no
   `.wasm`/`.css`/`app.js` refetch); About renders instantly. Its `title{}` is empty,
   so `document.title` stays at the layout default (no `"/about"` in the tab). Back
   returns to `/`.
3. Click a paste title → ONE request `/p/<id>?__vyrn=data` (~200 B JSON, a data
   page); the paste view renders and `document.title` becomes the paste title.
4. Type a draft in the create form, navigate away (to About — zero network) and
   back → the draft persists (the wasm instance survived).
5. Visit an unknown `/p/<id>` → ONE `/p/<id>?__vyrn=data` fetch returns the `@error`
   payload, which renders the themed 404 client-side.
6. A `/raw/<id>` link → `resolvePage` misses (not in the client bundle), so the
   navigator takes the HTML channel and hard-navs (non-JSON), the raw text loads.

Expected: nav (2) shows ZERO network requests; navs (3)/(5) show only their JSON
payload — no HTML documents, no asset refetches — and the shell/header never flashes.

### State / hash

No Rust frontend, `vyrn-codegen`, or `vyrn-lsp` source changed — the only Rust
edit is the new integration-test file, which does not enter the shipped LSP
binary. The deployed `editor/vscode/server/vyrn-lsp.exe` is therefore unchanged,
its state hash still
`57569c62bbec95ca7cdcb43f093a001af4836db969d0ef5a55a013f25049a116` (verified),
so no LSP redeploy. The release CLI was rebuilt.
