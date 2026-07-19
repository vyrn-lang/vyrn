# RFC-0041 — Layouts, Error Pages, and Head Ownership (the Nuxt shape)

- **Status:** Implemented (see "As landed" at the end)
- **Depends on:** RFC-0039 (`.vyx` pages + `pagesThemed`), RFC-0036
  (themed class checking), RFC-0031 (interface closure), RFC-0027
  (`import * as` — the i18n access polish)
- **Evidence (user review of examples/bin):** the templates read like
  Vue, but every page hand-rolls the whole shell — `index.vyx` and
  `about.vyx` each repeat `<div id="root">`, both stylesheet `<link>`s,
  the nav-runtime `<script>`, and the `<header>`/nav; `[id].vyrn`'s 404
  is a hand-built `el(...)` hyperscript chain because `respond()` has no
  `.vyx` form; and i18n arrives as a per-key import list. A Nuxt app
  writes the shell once (a layout), throws a 404 (an error page), and
  never puts `<script>`/`<link>` in a page body. This closes that gap.

---

## 1. Layouts: the shell, written once

A `layout.vyx` at a routes root (and nestable per subdir — nearest
ancestor wins) is a `.vyx` component with a `<slot/>`; the router wraps
every page body in the nearest layout before `document()`:

```html
<!-- routes/layout.vyx -->
<script>
import { i18n } from "std/i18n"
import * as t from i18n("./strings")
</script>
<template>
<div id="root">
    <header class="flex items-center gap-3 md:gap-6">
        <h1>{{ t.appTitle() }}</h1>
        <nav>
            <a href="/" class="mr-2 hover:text-brand-600">{{ t.navHome() }}</a>
            <a href="/about" class="mr-2 hover:text-brand-600">{{ t.navAbout() }}</a>
        </nav>
    </header>
    <main><slot/></main>
</div>
</template>
```

```html
<!-- routes/index.vyx — now just the page body -->
<script>
import { listPastes } from "../contract"
import * as t from i18n("./strings")
</script>
<template>
<p class="sub">{{ t.tagline() }}</p>
<div id="app"></div>
<h2>{{ t.recentHeading() }}</h2>
<p v-if="tally() == 0" class="empty">{{ t.empty() }}</p>
<div v-else> … </div>
</template>
```

- **Slot binding:** `<slot/>` in the layout is where the page body
  renders (the existing `children: Array<Html>` mechanism from RFC-0026;
  the router passes the compiled page body as the layout's children).
  A layout with no `<slot/>` is a generation diagnostic.
- **Nesting:** `routes/blog/layout.vyx` wraps pages under `blog/`,
  itself wrapped by `routes/layout.vyx` — layouts compose outermost-last.
- **A page opts out** with `layout="none"` in its `<script>`
  (a full-document page, rare). Default is the nearest layout.
- Pages stop carrying the shell; `index.vyx`/`about.vyx` drop to their
  unique bodies (bin's index goes from ~30 shell lines to its content).

## 2. Head & assets belong to the shell, not the page

The stylesheet `<link>`s, the client-island `<script src="/app.js">`,
and the nav-runtime `<script>` move into the layout (or a dedicated
`<head>` the layout declares). Concretely:

- A `.vyx` layout/page `<script>` may declare `head { … }` — a small
  block emitting `<link>`/`<meta>`/`<title>`/`<script>` into the
  document head (the router threads it into `document(title, head,
  body)` which already takes a head array). `title` may be dynamic
  (`head { title: pageTitle() }`).
- The client-island mount (`<div id="app">` + its boot `<script>`) is a
  page concern (only the home page hydrates), so it stays in the page —
  but the boot `<script src="/app.js">` moves to a `head`-declared or
  convention-emitted include, not an inline `<script>` mid-body.
- Result: page/layout templates contain markup, not plumbing.

## 3. Error pages replace respond() hyperscript

A page's `load()` may fail, and the router renders a themed error page
instead of the page — the Nuxt `throw createError({ statusCode })` shape:

```html
<!-- routes/p/[id].vyx -->
<script>
import { getPaste } from "../../contract"
params { id: String }
fn load(id: String) -> Result<Paste, PageError> {
    return match getPaste(IdReq { id: id }) {
        Ok(p) => Ok(p),
        Err(why) => Err(notFound(why)),     // PageError { status, message }
    }
}
</script>
<template>
<article> … {{ data.title }} … </article>   <!-- data = the Ok payload -->
</template>
```

- `load -> Result<Data, PageError>` (or the existing
  `Validation<Data>` — a validation failure becomes a 422 error page):
  `Ok`/`Valid` renders the page with `data`; `Err`/`Invalid` renders
  `error.vyx` (nearest, like layouts) with the `PageError`/Issues, at the
  carried status. `PageError = { status: Int64, message: String }` in
  `std/ui`.
- `error.vyx` is an ordinary themed `.vyx` (`params`-free, gets the
  error as its prop) — so the 404/422 body is a *template*, not
  `el(...)`. bin's `missing()` hyperscript is deleted; `[id].vyrn`
  becomes `[id].vyx`.
- **`respond()` stays** for genuine non-HTML raw responses (`/raw/[id]`
  = `text/plain`) — that is a real escape hatch, not boilerplate, and
  keeps its `.vyrn` form (a `respond` page has no template).

## 4. i18n access polish (namespace + de-prefixed names)

`import * as t from i18n("./strings")` then `t.appTitle()` — the
generated module's functions are ALSO exported un-prefixed for namespace
use (the `t` prefix exists only to survive flat imports; under a
namespace it is redundant). Flat `import { tAppTitle }` still works
(unchanged). This collapses bin's per-key import lists to one line each.
`setLocale`/`locale`/`Locale`/`TransKey` keep their names.

## Migration & proof

- bin: add `routes/layout.vyx` (+ `error.vyx`), strip the shell from
  `index.vyx`/`about.vyx`, convert `p/[id].vyrn` → `p/[id].vyx` with a
  failing `load`, delete the `missing()`/`found()` hyperscript, move
  head/asset plumbing into the layout, switch i18n to namespace access.
  shelf gets the same layout treatment (its shell duplication too).
- Browser-verify both: pages render identically (same HTML modulo the
  now-single-sourced shell), 404/422 render the themed error page,
  `/raw` still byte-exact, soft-nav + islands still work, uk plurals.
- emit-gen goldens + std/ui tests for layout wrapping, nesting,
  error-page dispatch, `head` emission; the "no `<slot/>`" and
  layout-not-found diagnostics.

## Out of scope

Named layouts / per-page layout selection beyond `layout="none"`
(nearest-ancestor is the v1 rule), `<NuxtPage>`-style nested route
outlets, transition/animation on layout change, per-component scoped
`<style>`, streaming, and the **template editor completion** (attributes,
Tw classes, component props) — that is RFC-0042, the companion DX round.

## As landed (2026-07-19)

Zero compiler changes, as designed — everything below is `std/vyx` +
`std/ui` + `std/i18n` + the two dogfood apps.

**§1 layouts.** A `layout.vyx` is a `.vyx` component with a required
`<slot/>` (a slot-less layout is `VYX_LAYOUT_NO_SLOT` naming the dir);
`std/vyx` gains `vyxBuildLayoutModule` + `vyxLayout`/`vyxLayoutThemed`
(exporting `layout(children)` + `head()`/`headTitle()`). The scanner
(`uiScanAll`) collects `layout.vyx`/`error.vyx` as chrome, NOT routes;
each page resolves its layout chain by directory prefix (every ancestor,
composed outermost-last via nested `layout([…])` calls) and the router
wraps the page body before `document()`. `layout="none"` in a page
`<script>` opts out. `.vyrn` `page` pages are wrapped too; `respond`
pages are not (they own the whole `Response`).

**§2 head blocks.** A page/layout `<script>` may declare `head { title:
<expr>, stylesheet <expr>, module <expr>, script <expr> }`; it compiles
to `head() -> Array<Html>` + `headTitle() -> String`. Script includes use
the `module`/`script` KEYWORD forms (not literal `<script>…</script>`
tags) so the enclosing `<script>` section stays scannable — a `</script>`
inside the head block would otherwise truncate it. A page's head/headTitle
take the page signature (`p: Params, d: Data`) so a dynamic title can read
the loaded data (bin's `/p/[id]` title is the paste title). The router
threads `document(uiFirst(pageTitle, …layoutTitles…, routePattern),
layoutHeads ++ pageHead, layoutWrap(body))`. bin's stylesheet `<link>`s +
nav-runtime `<script>` moved into the layout's head; the `/app.js` boot
moved into the home page's head block (the `#app` mount div stays in the
page); the emitted tags are the same set, now single-sourced into `<head>`
(they were in `<body>` before).

**§3 error pages.** `PageError { status, message }` + `notFound`/
`pageError`/`badRequest` are exported from `std/ui`. `load` may return
`Result<Data, PageError>` (an `Err` renders the nearest `error.vyx` at the
carried status) OR the existing `Validation<Data>` (an `Invalid` folds
into a 422 `PageError` — issues joined into the message). `error.vyx` is a
themed `.vyx` with an injected `error: PageError` prop, rendered inside the
same layout. `respond()` stays for raw non-HTML. The `.vyx` loader keeps
the RFC-0039 `load(p: Params)` convention (not the flat `load(id: String)`
the design sketch showed); its data type is DERIVED from the loader return
(inner of `Validation<T>` / first arg of `Result<T, PageError>`), so a page
loads straight into a wire type (`Result<Paste, PageError>`) with no
`type Data` alias — Vyrn forbids record aliases anyway.

**§4 i18n namespace.** The generated i18n module now exports each key
un-prefixed (`appTitle` beside `tAppTitle`), delegating to the prefixed
dispatcher; `import * as t from i18n("…")` reads `t.appTitle()`. Flat
imports and `setLocale`/`locale`/`Locale`/`TransKey` are unchanged. bin's
layout + index + paste pages use namespace access (index's 9-key list
collapsed to one line); about/shelf keep flat imports where the mixed
`setLocale`/enum surface doesn't shorten.

**Byte-identity gate.** The head/error runtime helpers and the `PageError`
import are gated on "any page threads a head" / "any page uses an error
page", so a router with no layouts/loaders (e.g. `pagesdemo`) stays
byte-identical. `.vyrn` page + no layout keeps the old
`document(title, [], body)` form.

**Migration + proof.** bin: `view.vyrn` was already gone (RFC-0039);
`routes/layout.vyx` + `routes/error.vyx` added; `index.vyx`/`about.vyx`
dropped to their `<main>` bodies (the layout's `<slot/>` splices the
page's `<main>` after the header, so `div#root > header + main` is
structurally identical); `p/[id].vyrn → p/[id].vyx` with a failing
`Result` loader (the `missing()`/`found()` hyperscript deleted);
`widgets/PasteView.vyx` deleted. shelf: `view.vyrn`'s `shell()` removed
(its bodies now return `<main>` directly), `routes/layout.vyx` +
`routes/error.vyx` added; the `/books/999` + `/books/-5` `Validation`
failures now render the themed 422 error page. Both browser-verified via
`vyrn dev`: identical SSR shell single-sourced into `<head>`, themed
404/422 error pages wrapped in the layout, dynamic `<title>`, `/raw/[id]`
byte-exact `text/plain`, and the `#app` island boot chain (app.js →
client.wasm) served unchanged.
