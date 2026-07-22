# RFC-0067 — Soft Navigation v2: No More Full Reloads

- **Status:** Implemented (see "As landed" at the end)
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

## As landed (2026-07-23)

Host-runtime only — **zero** compiler / generator / language / `std` /
server changes. The change is three served JS files: `web/vyrn-nav.js`
(rewritten to v2), `web/vyrn-dom.js` (one new method), and bin's
`public/app.js` (the island shape). `vyrn dev` serves these straight from
`web/`, so a `vyrn.exe` rebuild picks them up with no embed step.

**The v2 model (replacing RFC-0034's body morph).** A soft nav now:
fetches the destination (plain GET, `x-vyrn-nav: soft` header, server
untouched); on a **2xx HTML** response swaps `document.title` + the
page-owned `<head>` tags, **replaces the `<main>` element** (fallback:
the `<body>` children) with the fetched one, and re-mounts islands. The
DOM↔DOM keyed morph of v1 is gone — the shell (`header`/`nav`, the
persistent `<head>` assets, the delegated document-level click listener,
and the wasm instance) all sit **outside** `<main>`, so replacing the
content region leaves them untouched by construction. After a soft nav
the network log shows exactly one document fetch.

**Island re-mount against a surviving wasm instance (the point).** v1
re-booted every island per nav, which for a wasm island refetched
`/client.wasm`, re-instantiated, and dropped its module state. v2's
island registry boots an island **once** (`reg.boot(el)` → an instance);
on every later nav where the mount reappears it calls
`instance.mount(el)` — the same instance re-attaches its view to the new
node. A nav to a page *without* the mount leaves the instance alive and
unmounted, so its module state persists until the mount returns. Legacy
islands (an instance with a `destroy()` but no `mount()` — e.g. shelf's)
fall back to v1 tear-down-and-reboot, so nothing that worked breaks.
`vyrn-dom.js` gained `app.remount(newEl)`: it tears down the old mount's
DOM/effect/subscription/delegated-event state and rebuilds from the full
`vyrnView()`. It rebuilds from the *full view* (not the `vyrnPatch()` op
stream) on purpose: the fresh mount node is empty, but wasm's retained
`lastTree` still equals the current view (a navigation never changes
module state), so a full paint restores the DOM↔`lastTree` invariant the
op applier relies on, without needing to reset wasm-side state JS cannot
reach. Result: bin's create-form **draft survives** navigating away and
back — the "feature" the RFC calls out.

**404 hard-navs, by design.** A non-2xx response (incl. a `/p/<unknown>`
404 → the themed `error.vyx`), a non-HTML body, a cross-origin target, a
fetch failure/timeout, a second click mid-flight, or **any exception
thrown mid-swap** all degrade to `location.assign` — the reload that
works today, never a half-swapped page. (This *changes* RFC-0034, which
morphed 404 HTML in place; RFC-0067 §Verification pins the hard-nav.)

**Head ownership — deviation, documented.** §2.1 assumes RFC-0041 emits
per-tag head-ownership *markers*. It does not: `std/html`'s `document()`
concatenates the layout head and the page head into one `<head>` with no
attribute distinguishing them, and adding one is a server change (out of
scope — the SSR bytes stay identical). So v2 identifies the never-refetch
assets by **kind** instead: `<link rel=stylesheet>`, `<style>`, and
`<script src>` are kept in place and only *added* when genuinely new
(never removed → no stylesheet/runtime/wasm refetch, no FOUC); every
other `<head>` element (page `<meta>`, canonical/icon links; the
`<title>` via `document.title`) is treated as page-owned and swapped.
For the pages runtime this lands exactly where markers would have — the
layout-owned stylesheets + the `vyrn-nav` module are the kept assets, the
dynamic paste `<title>` is the swapped page tag — with no server touch.
If real markers ever arrive, `isKeptAsset`/`isPageOwnedHead` are the two
functions to swap for a marker check.

**Prefetch dropped (v2 simplification).** RFC-0034's hover/focus prefetch
+ background-revalidation morph is removed — it is out of scope here and
was built on the morph model v2 replaces. `vyrnNav.prefetch(url)` remains
as a no-op and `data-nav="prefetch"` is an inert (still soft-navigable)
hint, so no consumer throws. `data-nav="hard"` opt-out, the
`vyrn:nav-start/end/error` events, and the built-in progress bar are
unchanged.

**Verification.** `node --check` clean on all served runtimes. Full
workspace suite **1044 passed / 10 ignored**; `vyrn-lsp` (excluded)
**43 passed / 1 ignored**; three-way parity **6 passed** (the runtime is
a static asset, so parity cannot and did not move — verified anyway). 0
new clippy warnings (no Rust touched; the 52 baseline warnings are
pre-existing). No `.vyrn` touched → no `fmt` needed. **Runtime-only, so no
LSP redeploy:** `editor/vscode/server/vyrn-lsp.exe` stays
`57569c62bbec95ca7cdcb43f093a001af4836db969d0ef5a55a013f25049a116`
(verified). `vyrn dev` on bin was smoke-tested via `curl`: `/` and
`/about` 200, `/p/<id>` 200 with a dynamic `<title>` and no `#app`,
`/p/<unknown>` 404, `/vyrn-runtime/vyrn-nav.js` + `/client.wasm` served.

**Browser click-path to verify post-merge** (open DevTools → Network,
filter to non-document requests; `cd examples/bin && vyrn dev`):

1. **Create → redirect, zero refetch.** Load `/`, type a paste, submit.
   The page soft-navs to `/p/<id>`. Assert the Network log shows exactly
   **one** document request (`/p/<id>`) and **no** refetch of
   `client.wasm`, `vyrn-nav.js`, `vyrn-dom.js`, `vyrn-rpc.js`, `theme.css`,
   or `style.css`. The URL bar and `<title>` update.
2. **Header links.** Click **About** then **New paste**: both soft-nav
   (one document fetch each, no asset refetch); the header stays put (no
   flash), `<title>` swaps, `<main>` content swaps.
3. **Draft survives (the feature).** On `/`, type into the create form
   (don't submit), click **About**, then **New paste**. The draft text is
   **still there** — the wasm instance and its module state survived; only
   the view re-mounted.
4. **Back / forward.** After 1–2, press Back and Forward. Each restores
   the page and scroll position via soft nav (no full reload); the create
   island re-mounts on the home entry.
5. **404 hard-navs.** Navigate to `/p/deadbeef` (e.g. edit the URL, or a
   stale link). It returns 404 → a **full reload** to the themed error
   page (the document *does* refetch here — that is the design), not a
   soft swap.
6. **Cross-origin / opt-out.** An external `<a>` (or `target=_blank`,
   `download`, `data-nav="hard"`, `mailto:`) is a native browser
   navigation, never intercepted.
7. **Progressive enhancement.** Remove the `vyrn-nav` module include from
   `routes/layout.vyx` head: every link hard-navigates and the `#app`
   island still boots inline (app.js's fallback path). Restore it.
