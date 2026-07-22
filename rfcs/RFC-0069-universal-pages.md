# RFC-0069 — Universal Pages: Nuxt-Mode Navigation

- **Status:** Locked design
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
