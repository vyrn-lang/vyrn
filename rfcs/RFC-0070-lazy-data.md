# RFC-0070 — Lazy Data: Render Instantly, Fill In When It Arrives

- **Status:** Locked design
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
