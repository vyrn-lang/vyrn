# RFC-0067 — Soft Navigation v2: No More Full Reloads

- **Status:** Locked design
- **Depends on:** RFC-0026 (vyrn-dom differ, TEA islands), RFC-0039/0041
  (SSR pages, layouts, head ownership), RFC-0013 (host-owns-the-loop,
  `takeNav`), RFC-0019 (`vyrn dev` serving the runtimes)
- **Evidence (user):** "It reloads page each time?" — yes: the wasm
  client patches in place, but every NAVIGATION (post-create redirect,
  header links) is a full document load — assets refetched, wasm
  rebooted. This is the "data-only navigation v2" item deferred in the
  RFC-0026 arc, now demanded by use.

---

## Design

`vyrn-nav.js` v2 — the served runtime becomes a real soft navigator:

- **Intercepts:** same-origin left-clicks on `<a>` (no modifier keys, no
  `target`, no `download`, http(s) only, hash-only changes stay native),
  the client's `takeNav` targets (the post-create redirect), and
  `popstate`.
- **On nav:** `fetch` the target URL (ordinary GET — the server is
  UNCHANGED; it keeps rendering full pages), parse with `DOMParser`,
  then:
  1. swap `document.title` and the page-owned `head{}` elements
     (RFC-0041's head-ownership markers identify them);
  2. replace the layout's content region (`<main>`, falling back to
     `<body>` if a page has none) with the fetched one;
  3. re-mount the TEA islands inside the new content against the
     EXISTING wasm instance (the current boot path minus
     instantiation — widgets re-request their view and patch);
  4. `pushState` on forward nav; scroll to top on push, restored
     position on popstate.
- **The wasm instance, stylesheets, and runtimes are never refetched or
  rebooted.** That is the entire point; the network tab after a soft nav
  shows exactly one document fetch.
- **Hard-nav fallback** (assign `location`) on: cross-origin, fetch
  failure or non-2xx, a response that isn't HTML, or any exception
  during the swap — a broken soft nav must degrade to the reload that
  works today, never to a broken page.
- Client state across navs: module state in the wasm instance survives
  (that's a feature — drafts persist across a nav and back); per-page
  islands are re-mounted from the new DOM.

## Verification

1. In-browser against the bin app (the Browser pane, dev server):
   create a paste → lands on `/p/<id>` with NO `client.wasm` /
   runtime / stylesheet refetch (assert via the network request log);
   header links soft-nav; back/forward restore pages and scroll; a
   cross-origin link and a fetch-failure hard-nav.
2. The self-check pages (`/`, `/about`, `/p/*`) all soft-nav cleanly in
   both directions; `error.vyx` renders via soft nav on a 404 target…
   NO — a non-2xx hard-navs by design; pin that.
3. `node --check` on the runtime; the served-runtimes plumbing already
   has CLI tests (extend the served-bytes pin if one exists).
4. Full suite + LSP + parity green (the runtime is a static asset —
   parity cannot move, verify anyway); 0 new clippy warnings; no LSP
   redeploy (state unchanged hash).

## Out of scope

Server-side partials/JSON page payloads (v3 — today's full-HTML fetch
is already a massive win and needs no server change), prefetch on
hover, view transitions API, scroll-position persistence per history
entry beyond the browser default, and progress indicators.
