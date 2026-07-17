# RFC-0034 — Soft Navigation: SPA Feel over MPA Truth

- **Status:** Implemented (see "As landed" below)
- **Depends on:** RFC-0026 (M2 `vyrn-dom.js` — the differ this extends;
  M3 pages — the SSR routes being navigated), RFC-0031 (the shelf
  architecture this must not disturb)
- **Evidence:** RFC-0026 locked v1 as MPA and named SPA takeover the M5+
  follow-up; shelf now has real multi-page flows where every `<a>` click
  is a full reload — flash, lost scroll, dropped client state. Nuxt's
  feel is the remaining visible gap.

---

## The model (and why not isomorphic pages yet)

True client-side page rendering means compiling page modules into the
client build — which transitively links server-only modules (shelf's
store with its state init; anything doing I/O). That is a real design
round (module splitting or dead-state elimination), not a footnote. v1
therefore takes the Turbolinks/htmx lineage: **navigation stays a server
render; only the page transition goes soft.** The server stays the truth;
the client stops flashing.

## Mechanics (all in `web/vyrn-dom.js` + a sibling `vyrn-nav.js`; zero
language surface, zero generator changes)

1. **Intercept:** same-origin, left-click, no-modifier `<a href>` clicks
   (and form GET submits are v2 — out of scope). Opt-out per link:
   `data-nav="hard"`. External links, downloads, and anchors-only
   (`#hash`) behave natively.
2. **Fetch:** `fetch(href, {headers: {"x-vyrn-nav": "soft"}})` — the
   header is informational v1 (servers need no cooperation; a future
   server MAY use it to skip the shell). Non-200 or non-HTML responses
   fall back to a hard navigation — soft nav must never make an error
   page unreachable.
3. **Morph:** parse the fetched document, then **DOM↔DOM morph** the
   live `<body>` against the new one — a second differ beside the
   existing tree↔DOM one, same keyed rules (`data-key` moves nodes,
   positional otherwise), attribute patching, script/style handling
   (new `<head>` stylesheets appended, `<title>` swapped; inline scripts
   in the new body are NOT re-executed — declarative pages don't carry
   them). Focus and form state survive morphing where identity holds
   (the keyed-reorder discipline, now cross-page).
4. **Client islands:** if the incoming page contains the wasm mount node
   (`#app` convention), the runtime re-boots the client app after the
   morph exactly as a hard load would (fresh state — same semantics as
   MPA, minus the flash). If the node morphs in place across two pages
   that both mount it, still re-boot: page identity changed. Same-page
   morphs (see prefetch revalidation) never re-boot.
5. **History & scroll:** `pushState` on soft nav, `popstate` handled by
   the same fetch+morph path, scroll restored per history entry
   (top on new entries, saved position on back/forward).
6. **Prefetch:** `data-nav="prefetch"` links fetch-and-cache on
   hover/focus (small LRU, staleTime borrowed from `vyrn-query.js`
   conventions); click then morphs from cache and revalidates.
7. **Events:** `vyrnNav` dispatches `vyrn:nav-start` / `vyrn:nav-end` /
   `vyrn:nav-error` DOM events (progress indicators hook these).

## Guardrails (locked)

- **Progressive enhancement, strictly:** with `vyrn-nav.js` absent or JS
  failing, every link works as today — nothing in `std/ui`'s emitted
  HTML may depend on soft nav. (`data-nav` attributes are inert hints.)
- **No language/generator changes.** `std/ui` gains at most the optional
  `data-nav` attribute pass-through it already supports via ordinary
  attrs. If implementation finds itself wanting generator cooperation,
  that is v2 (data-only navigation) arriving early — stop and report.
- **Fallback bias:** any ambiguity (morph failure, mid-flight second
  click, fetch timeout) resolves as a hard navigation. Soft nav is an
  optimization, never a correctness layer.

## v2 sketch (recorded, not built): data-only navigation

The pages generator additionally emits, per SPA-opted page, a client
render module (page view fns only — requires the view/load module-split
convention) and the server answers `x-vyrn-nav: data` with the loader's
`Validation<Data>` JSON; the client renders locally through the existing
tree differ and the query cache carries staleness. This is the isomorphic
step, gated on the server-only-imports design; this RFC's interception,
history, and event surface are built to carry it unchanged.

## Consumers / proof

- **shelf:** nav links get soft navigation (about ⇄ home ⇄ book detail),
  one `data-nav="prefetch"` on the book list, a tiny nav-progress
  indicator riding the events. Browser-verify: no flash (DOM node
  identity preserved across nav where keyed), back/forward + scroll
  restoration, 404/422 pages still reached, client island re-boots on
  pages that mount it, everything still works with `vyrn-nav.js` deleted
  (progressive enhancement proof).
- `web/` demo page exercising morph corners (keyed list page ⇄ detail,
  head stylesheet swap, title change).

## Out of scope

Data-only/isomorphic navigation (v2 above), form submits, view
transitions API styling, streaming/partial morphs, offline caching,
generator cooperation of any kind.

## As landed

Host-runtime only — zero compiler/generator/language/`std` changes. Suite
unchanged: 847 workspace + 11 LSP tests, three-way parity green.

- **The morph lives in `web/vyrn-nav.js`, not `vyrn-dom.js`.** `vyrn-dom.js`
  is the *tree↔DOM* differ (vnode JSON → DOM); the soft-nav morph is a
  *DOM↔DOM* differ (parsed `<body>` → live `<body>`) with no vnodes. They
  share a discipline (keyed identity via `data-key`, positional otherwise,
  attribute patching) but not a data model, so keeping the morph in
  `vyrn-nav.js` lets it stay a self-contained, import-nothing sibling and
  keeps `vyrn-dom.js` focused. `morphChildren`/`morphKeyed`/`morphNode`
  mirror `patchChildren`/`patchKeyed`/`patchNode`.
- **Focus / form preservation is emergent, not bolted on.** Where identity
  holds the morph *reuses* the DOM node (never `replaceChild`), so
  `document.activeElement` and any typed-in `.value` (a live property
  `setAttribute` never touches) survive for free. For a non-focused form
  control the live value is synced to the incoming server value; the focused
  field is left alone. A focused node that must be replaced is re-focused by
  `id` if a same-`id` node survives. Verified: a typed value in a keyed
  header input survives list⇄detail navigation.
- **Stylesheets are additive; `<title>` is swapped.** New `<head>`
  stylesheets (by `href`, and `<style>` by text) are appended, never
  removed — a returning page keeps sheets a prior page added (no FOUC, no
  reflow churn). Shelf keeps its `<link>`s in `<body>` (inside `#root`), so
  the body morph reuses them in place; the `web/` demo exercises the
  `<head>` append path.
- **Refinement to interception rule 2 (non-200):** an HTML response is
  MORPHED regardless of status, so a 404/422 error *page* is reached softly
  (flash-free) — which is what the guardrail "soft nav must never make an
  error page unreachable" actually wants. Hard fallback fires on a
  **non-HTML** response, a fetch failure/timeout/abort, a mid-flight second
  click, or a morph that throws. Verified: `/books/999` (422) and
  `/books/abc` (404) morph softly; `/theme.css` (non-HTML) and a
  fetch-failure both hard-navigate for real.
- **Island protocol.** Because morphed-in `<script>`s are not re-executed,
  the client boot registers via `window.vyrnNav.registerIsland(sel, boot)`;
  vyrn-nav owns the lifecycle — boot on first appearance, tear down +
  re-boot on every real navigation, leave alone on same-page revalidation
  morphs. Shelf's `app.js` registers `#app` and returns a `destroy()` that
  also removes its injected button (no accumulation across re-boots); it
  falls back to booting inline when `window.vyrnNav` is absent
  (progressive-enhancement proof: deleting the include hard-navigates every
  link and the island still boots). `syncScripts` additionally appends any
  new `<script type=module src>` (idempotent — ES modules evaluate once) so
  a boot reached mid-session still loads.
- **Scroll corner actually hit.** Per-entry scroll is stamped
  *synchronously* into the leaving entry at `pushState` time (a late
  throttled frame must not write it into the new entry). On `popstate` the
  saved offset is re-applied on a short bounded schedule (0/60/160/320/520
  ms) because an async island re-boot briefly clears `#app` and collapses
  page height right after the morph, which would otherwise clamp the restore
  to 0. Verified: scroll 300 on the list, into a detail, back → 299
  restored.
- **Progress indicator.** Shipped as a built-in top bar in `vyrn-nav.js`
  that rides the `vyrn:nav-start/end/error` events (the RFC frames progress
  as event-driven), marked `data-vyrn-nav-ui` and appended to
  `<html>` so no morph touches it; opt out with
  `window.__vyrnNavConfig = { progress: false }` and hook the events
  yourself. Shelf gets it for free — its pages stay untouched beyond the one
  `<script>` include and the prefetch `data-nav` on the book-list detail
  links.
- **Consumers:** `web/navdemo.html` + `web/navdemo-detail.html` +
  `web/navdemo.js` (keyed list ⇄ detail, `<title>`/stylesheet swap, prefetch,
  reload sentinel); shelf's `view.vyrn` shell includes the runtime, the SSR
  and `.vyx` book rows gained a prefetch `/books/:id` detail link, and
  `public/app.js` became an island.
