# RFC-0070 — Lazy Data: Render Instantly, Fill In When It Arrives

- **Status:** Implemented
- **Depends on:** RFC-0069 (universal pages, `load()`, `renderPage`,
  `resolvePage`, the payload protocol, vyrn-nav v3), RFC-0026 (vyrn-dom
  keyed differ — the skeleton→data transition is just a patch)
- **Evidence (user):** "shouldn't index page open instantly and only then
  request data, show spinner for data not fetched yet? But first full
  reload rendered by server." — correct. RFC-0069 navigates a data page
  by fetching, WAITING, then rendering the finished page (the old page
  lingers, the new one appears all-at-once). The Nuxt-lazy feel is:
  static structure instant, data region shows a loading state, data
  fills in.

---

## The model

RFC-0069's default is **blocking**: nav → fetch → render `Ready`. That is
Nuxt's default too and is correct for fast/local data (a ~1 ms localhost
fetch under a spinner would only FLICKER). This RFC adds an OPT-IN
**lazy** mode for genuinely slow data.

### Opt-in: `lazy load()`

A page marks its loader lazy:

```vyrn
lazy fn load() -> Array<Paste> { … }
```

A lazy page's view receives its data wrapped:

```vyrn
export type PageData<T> = Loading | Ready(T)
```

The index (the showcase) renders its static shell unconditionally and
matches only where the data is used:

```vyrn
fn page(data: PageData<Array<Paste>>) -> Html {
    // form + headings render always
    match data {
        Loading => spinnerRow(),           // just the list region
        Ready(pastes) => pasteRows(pastes),
    }
}
```

- A **non-lazy** page is unchanged: view is `page(data: T)`, nav blocks.
  Lazy is purely additive; no existing page's signature moves.
- `PageData<T>` is a builtin generic enum (like `Result`/`Validation`),
  usable in `.vyx` script and matchable in templates
  (`v-if`/`match`-in-expression).

## First load — always full SSR

The server runs `load()` for SSR whether or not it is lazy, so the FIRST
render is `Ready(data)` — a complete page, no skeleton, no hydration
flash. `Loading` is a **client-navigation-only** state: it never appears
in server output. (Verified: unmarked SSR bytes for a lazy page are
byte-identical to the same page rendered non-lazy — the wrapper is
erased server-side.)

## Client navigation (vyrn-nav)

`resolvePage(path)` (RFC-0069) already returns `{found, hasData, title}`;
it gains **`lazy`**. On a soft nav to a resolved page:

- **`lazy: true`** → render **immediately** via `renderPage` with a
  `Loading` payload (instant shell + the page's skeleton, ZERO wait),
  set the title, THEN fetch `?__vyrn=data`; on arrival re-render with
  `Ready(props)`. The vyrn-dom differ patches only the data region — the
  form/headings never repaint. The 150 ms-armed progress bar still rides
  the fetch.
- **`lazy: false` + `hasData`** → blocking, exactly as RFC-0069 (fetch,
  then render `Ready`).
- **dataless / unknown** → unchanged (zero-fetch render / HTML fallback).

`renderPage` accepts a payload whose `props` may be the sentinel
`{"__loading__":true}` → the page module constructs `Loading` for its
`PageData<T>` view; a real props object → `Ready(decode(props))`.

## A lazy load that fails

A lazy `load()` returning `Err`/`Invalid` resolves to the RFC-0069
`@error` payload → the themed error page, same as blocking. Inline
per-region error states are out of scope (v1 keeps whole-page errors).

## Showcase + scope

`examples/bin/routes/index.vyx` becomes lazy: the create form + headings
render instantly on nav, the recent-pastes list shows a one-row spinner
until its `Array<Paste>` arrives. Every other bin route stays blocking
(their data is trivial). This is the ONLY behavioral change to the app;
SSR output stays byte-identical.

## Verification

1. `vyrn test` on the client: a lazy page's `renderPage` with the
   `__loading__` sentinel produces the skeleton; with real props produces
   the data view; `resolvePage` reports `lazy:true` for the index,
   `false` elsewhere.
2. SSR byte-identical: the lazy index's unmarked HTML equals its
   pre-lazy bytes (the `Loading` arm never renders server-side because
   the server always has data) — pinned in `universal_pages.rs`.
3. Browser (user post-merge, click-path in as-landed): nav to the lazy
   index shows the form instantly + a spinning list, then the list fills
   — and the Network panel shows the data fetch happening AFTER the shell
   painted (not before). A hard reload of the index shows the full list
   immediately (no skeleton).
4. Full suite + universal_pages(--ignored) + LSP + parity green; fmt
   clean; 0 new clippy; node --check runtimes. If frontend (checker for
   `lazy` keyword + `PageData`) changes → rebuild + hash-verify LSP;
   else state the unchanged hash. Rebuild release CLI.

## Out of scope

Lazy-by-default (opt-in only — flicker on fast data), per-region inline
error states, streaming/partial payloads, `Suspense`-style nested
boundaries, prefetch-on-hover, and a stale-while-revalidate cache.

---

## As landed

Shipped **frontend-free** across two milestones, entirely in the generators
(`std/vyx`, `std/ui`), the runtimes (`web/vyrn-nav.js`, `web/vyrn-dom.js`), and
`examples/bin` — **no Rust frontend/compiler source changed** (only a new assertion
in the existing integration-test file), so the deployed LSP binary is untouched
(hash unchanged, below).

### Lazy-marker mechanism: scan-and-strip (frontend-free)

`lazy fn load()` is recognized and the `lazy` marker STRIPPED by `std/vyx`'s scanner
before the module is emitted — the Vyrn compiler never sees `lazy fn` (which is not
valid syntax). This mirrors exactly how `load()` itself is already detected and how
`layout="none"` is consumed. A single `vyxUnlazy(trimmedLine)` normalizer lets the
existing load-line scanners (`vyxScriptHasLoad`/`LoadHasParams`/`LoadRet`,
`vyxStripLoadFn`, `vyxLoadBody`) tolerate the prefix; `vyxScriptLoadIsLazy` records
laziness; `vyxAutoExportPageDecls` drops the `lazy ` before exporting the loader
(non-lazy pages stay byte-identical — the strip only fires on a `lazy`-marked line).
Laziness flows as page metadata: `VyxPageShape.loadLazy` → `UiPageInfo.loadLazy`. The
strip is sound (the marker only ever prefixes the one statement-leading `fn load`
line), so the fallback "compiler tolerates a marker" path was not needed.

### `PageData<T>` placement: defined ONCE in `std/ui`

`export type PageData<T> = | Loading | Ready(T)` lives in `std/ui` and is imported by
every generated lazy page module (server and client). It is **never** redefined
per-page-module — that would re-register the `Loading`/`Ready` constructors in the
one global constructor table (the RFC-0068 lesson) and collide. `std/ui` is already
in both the server router's and the client bundle's link graph, so the import adds no
new module. The generator prepends `import { PageData } from "std/ui"` to a lazy
page's module (generated modules collect imports order-independently).

### Constructor-name verdict: `Loading` / `Ready` are FREE

Verified against every injected prelude enum (`Value`=IntVal/StrVal/BoolVal,
`Validation`=Valid/Invalid, `LoadResult`=Missing/Corrupt/Loaded, `Result`=Ok/Err,
`Option`=Some/None, `Json`=JNull/JBool/JNum/JStr/JArr/JObj) and a repo-wide grep of
`std/` + `examples/` — no `Loading` or `Ready` constructor exists. No rename was
needed (unlike RFC-0068's `Invalid`→`Rejected`).

### What moved where

- **The view is over `PageData<T>`, the loader still returns `T`.** For a lazy page
  the synthesized `uiPageBody`'s `data` prop is `PageData<T>`; the `page(d: T)` entry
  keeps its **raw-`T`** signature (the router's `page(d)` call is UNCHANGED) and wraps
  `Ready(d)` internally. So SSR — which always has data — only ever renders the
  `Ready` arm; the `Loading` arm is a client-navigation-only state that never reaches
  server output. `page`'s head fns also take raw `T` (server always Ready), unchanged.
- **The client bundle's lazy channel.** A lazy page module also exports `pageLoading`
  (the skeleton view — `uiPageBody(Loading)`) and `clientRenderLoading`. The bundle's
  `renderPage` dispatch calls `clientRenderLoading` when the payload's `props` is the
  `{"__loading__":true}` sentinel (detected structurally via `uiClientIsLoading` — a
  `props` JObj with `__loading__`=true, so a real `Array<Paste>`/record props never
  false-positives), and `clientRender` (the `Ready` path) otherwise — that path is
  byte-identical to RFC-0069 (`page(uiD)` already wraps `Ready`). `resolvePage`'s
  descriptor gains `lazy` (`{found,hasData,lazy,page,title}`).
- **`vyrn-dom` static differ.** `makePageView(json)` builds a page tree retaining the
  vnode; `patchPageView(view, json)` diffs a new tree against it and patches only the
  changed nodes in place (keyed-list aware, no events/effects — pages are pure). The
  `#app` island mount is a static leaf: unchanged between skeleton and data, it is
  left exactly where it is, so the form never repaints and the wasm instance is never
  touched. `buildStatic` now records `v.dom` for this.
- **`vyrn-nav` render-then-fill.** `navigateLazy`: paint the skeleton INSTANTLY from a
  `{"__loading__":true}` payload (zero wait), then fetch `?__vyrn=data` and fill in
  `Ready(props)` by PATCHING the retained skeleton view. The 150 ms-armed progress
  bar still rides the fetch. A payload that returns a DIFFERENT page (a failed lazy
  load's `@error`) repaints `<main>` wholesale; a skeleton render that fails (a
  dynamic lazy page whose Params the host can't build) degrades to the blocking fill;
  any fetch/render failure hard-navs. Non-lazy data pages still block exactly as
  RFC-0069; dataless/unknown are unchanged.
- **Showcase.** `examples/bin/routes/index.vyx` is `lazy fn load() -> Array<Paste>`;
  its view is over `PageData<Array<Paste>>` via two local helpers (`isLoading`,
  `recentRows`) driving a `v-if`/`v-else-if`/`v-else` chain — the create form +
  headings render always, the list region shows a themed one-row spinner (new
  `bin.loading` i18n string, en+uk; `loading`/`spinner` safelisted, spinner CSS in
  `public/style.css`) until the `Array<Paste>` arrives. Every other bin route stays
  blocking.

### Deviation (with justification)

**Dynamic lazy pages degrade to blocking rather than instant-skeleton.** The locked
design allows `page(params, data: PageData<T>)`. The instant skeleton paint needs the
route `Params`, but before the data fetch the host has only the URL — deriving typed
`Params` from the path in JS would duplicate the server's parse. So `navigateLazy`
tries the skeleton with `params:null`; if `clientRenderLoading` can't build `Params`
it returns the fallback sentinel and the nav proceeds straight to the blocking fill
(the server payload carries the params the `Ready` render needs). A STATIC lazy loader
(the showcase, and the RFC's primary case) gets the full instant-skeleton treatment;
a dynamic one behaves like a blocking RFC-0069 page — correct, just not instant. This
is the minimal sound closure, not a redesign.

### SSR byte-identical proof

Captured the pre-lazy home from `f8c8719` before any change, then re-captured after:
the empty home (deterministic — no timestamps) is **byte-identical** (`diff` clean,
701 bytes), and the marked payload is unchanged
(`{"page":"/","title":"/","props":[],"params":null}`). The non-empty home renders the
same `Ready` structure (`<div><p class="count">…</p><ul class="pastelist">…</ul></div>`,
timestamp aside). Pinned durably in `universal_pages.rs`
(`unmarked_lazy_home_is_byte_identical_and_never_renders_the_skeleton`): the shell
prefix is byte-for-byte unchanged AND no `spinner`/`Loading recent pastes` ever
appears in SSR (the `Loading` arm never renders server-side).

### Verification

- `vyrn test examples/bin/client.vyrn`: **21 green** — the RFC-0069 pins plus three
  new ones: the `{"__loading__":true}` sentinel renders the skeleton (shell + spinner
  + localized loading row, no list), real props render the `Ready` list (no skeleton),
  and `resolvePage` reports `lazy:true` for `/`, `false` for `/about` and `/p/:id`.
- `universal_pages` (`--ignored` tier): the new byte-identical/no-skeleton pin plus
  the unchanged RFC-0069 seven, all green.
- Full workspace (`cargo test --workspace`) green; `vyrn-lsp` tests green; three-way
  parity green; `fmt --check` clean; 0 new clippy; `node --check` clean on all three
  runtimes.

### Browser click-path (post-merge proof — the in-repo pane is flaky)

1. Hard-load `/`. The full recent-pastes list renders immediately (SSR `Ready`) — NO
   skeleton, no spinner. The create island boots (`#app`) and registers the renderer +
   resolver.
2. Navigate away (click **About** → zero network, RFC-0069 M4) and back to **New
   paste** (`/`): the create form + headings paint INSTANTLY and the recent-pastes
   list shows the one spinning "Loading recent pastes…" row — THEN the Network panel
   shows the `/?__vyrn=data` fetch happening AFTER the shell painted (not before), and
   the list fills in. Only the list region repaints — the form (and any draft typed in
   it) never flickers (vyrn-dom patched just the data region; the `#app` island was
   never re-mounted).
3. Hard reload `/`: the full list is there immediately again — no skeleton (first load
   is always full SSR).

Expected: nav (2) shows the spinner FIRST, then one JSON payload fetch, then the list —
and the create form never repaints across the transition.

### State / hash

No Rust frontend, `vyrn-codegen`, or `vyrn-lsp` source changed — the only Rust edit is
the new assertion in the integration-test file, which does not enter the shipped LSP
binary. The deployed `editor/vscode/server/vyrn-lsp.exe` is therefore unchanged, its
state hash still
`57569c62bbec95ca7cdcb43f093a001af4836db969d0ef5a55a013f25049a116` (verified), so no
LSP redeploy. The release CLI was rebuilt.
