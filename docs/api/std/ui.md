# std/ui

std/ui — the pages generator (RFC-0026 M3), file-based routing as a library
on RFC-0021 generator imports. One `gen fn`, `pages(dir)`, scans a directory
of ORDINARY `.vyrn` page modules at compile time (sandboxed, deterministic,
cached) and synthesizes a router module. The compiler knows nothing about
routing — everything below is plain, comptime-pure Vyrn over `listDir`,
`moduleInterface` (RFC-0021), `std/html` (RFC-0026 M1), the `Request`/
`Response` server surface (RFC-0016), `fromJson` (RFC-0018), and the
regex-validated string types (RFC-0020).

  import { pages } from "std/ui"
  import { route } from pages("./pages")
  fn handle(req: Request) -> Response { return route(req) }

Directory conventions (v1):
  pages/index.vyrn        → GET /
  pages/items/index.vyrn  → GET /items
  pages/items/[id].vyrn   → GET /items/:id     (single-segment dynamic param)

A page module EXPORTS:
  - `fn page(p: Params) -> Html`  — or `fn page() -> Html` for a page with no
    dynamic segments and no loader.
  - `type Params = { id: Int64, … }` when it has `[bracket]` segments — the
    field NAMES must match the segments exactly (checked via `moduleInterface`
    at generation time). v1 supports `Int64` params only.
  - optionally `fn data() -> ParamQuery<Params, Validation<Data>>` (the `data`
    member of the `Page` contract below, RFC-0071) and then
    `fn page(p: Params, d: Data) -> Html`. On `Invalid` the synthesized router
    renders an error page (422) built with `std/html`, listing the issues.

The synthesized module EXPORTS `route(req: Request) -> Response`: it matches
the path, parses+validates each dynamic segment against its declared type (an
`Int64` segment that is not an integer 404s, never reaching user code), runs
`load`, renders the page through `document(…)`, and returns the `Response`. An
unknown path is a 404 page. It also emits `type RoutePath` — a regex-validated
string of the whole route language — plus a `href<Route>(…)` helper per dynamic
route and a `<route>Path()` helper per static route (typed URLs).

Generation failures — a Params/segment mismatch, an unsupported param type, or
a route collision — fail the load with a diagnostic naming the offending file
(the std/rpc identifier-carrying convention: the offense rides a bare
top-level identifier so parsing fails immediately, attributed to the generator
call site).

Inspect the synthesized module with:  vyrn emit-gen <file>

## PageError

```vyrn
type PageError = { status: Int64, message: String }
```

A page-load failure: an HTTP status and a human message. The router renders
the nearest `error.vyx` (or a built-in error body) at `status`.

## pageError

```vyrn
fn pageError(status: Int64, message: String) -> PageError
```

A `PageError` with an explicit status.

## notFound

```vyrn
fn notFound(message: String) -> PageError
```

A 404 `PageError`.

## badRequest

```vyrn
fn badRequest(message: String) -> PageError
```

A 400 `PageError`.

## PageData

```vyrn
type PageData = Loading | Ready(T)
```

The data a lazy page's view is rendered over: `Loading` before the data has
arrived (client nav only), `Ready(T)` once it has (always, server-side).

## Meta

```vyrn
type Meta = { name: String, content: String }
```

One `<meta name=… content=…>` a page contributes to the document head.

## Head

```vyrn
type Head = { title: Option<String>, stylesheets: Array<String>, modules: Array<String>, scripts: Array<String>, meta: Array<Meta> }
```

What a page contributes to the document head.

Exactly what the `head { … }` block could express — a title, stylesheet
links, module and classic script includes — plus `meta`, which the block
could not. Build one from `noHead()` and the `with*` combinators:

    export fn head() -> Head {
        return withModule(noHead(), "/app.js")
    }

Elements are emitted stylesheets-then-modules-then-scripts-then-meta, each
group in the order it was added.

## noHead

```vyrn
fn noHead() -> Head
```

The empty head — no title, no includes. The default for a page that declares
no `head`, and the base every `with*` combinator builds on.

## withTitle

```vyrn
fn withTitle(h: Head, title: String) -> Head
```

`h` with the document title set. The page's title wins over its layouts'.

## withStylesheet

```vyrn
fn withStylesheet(h: Head, href: String) -> Head
```

`h` with a `<link rel="stylesheet" href=…>` appended.

## withModule

```vyrn
fn withModule(h: Head, src: String) -> Head
```

`h` with a `<script type="module" src=…>` appended.

## withScript

```vyrn
fn withScript(h: Head, src: String) -> Head
```

`h` with a classic `<script src=…>` appended.

## withMeta

```vyrn
fn withMeta(h: Head, name: String, content: String) -> Head
```

`h` with a `<meta name=… content=…>` appended.

## headHtml

```vyrn
fn headHtml(h: Head) -> Array<Html>
```

A head's elements, in the document order `document(title, head, body)` emits.
Byte-identical to what the `head { … }` block compiled to, element for
element — which is what keeps a migrated page's SSR bytes unchanged.

## headTitleOf

```vyrn
fn headTitleOf(h: Head) -> String
```

A head's document title, or "" when it declares none — the spelling
`uiFirst` composes through the layout chain.

## Query

```vyrn
type Query = { run: fn() -> T }
```

A page's data: the call that produces it, deferred until the router asks.

    export fn data() -> Query<Array<Paste>> {
        return query(|| listPastes().pastes)
    }

The page BLOCKS on this — the document is not sent until the data lands. For
render-then-fill, return a [`Lazy`] instead.

## Lazy

```vyrn
type Lazy = { run: fn() -> T }
```

A page's data when the page renders LAZILY (RFC-0070): the shell paints
instantly on a client soft nav and the data region fills in when the payload
lands.

    export fn data() -> Lazy<Array<Paste>> {
        return lazy(query(|| listPastes().pastes))
    }

This is a DISTINCT TYPE rather than a flag on `Query`, and that is the whole
point (RFC-0071 M2b). Laziness decides the *view's* type (`PageData<T>` versus
`T`), so the generator has to know it at generation time; a runtime `lazy`
field meant reading the `lazy(…)` call out of `data`'s body — a source scan,
which is the practice this RFC exists to end. As a type it is reflection.

## ParamQuery

```vyrn
type ParamQuery = { run: fn(P) -> T }
```

A page's data when it depends on the route parameters: the deferred call
receives the page's own `Params`.

    export fn data() -> ParamQuery<Params, Result<Paste, PageError>> {
        return paramQuery(|p: Params| fetch(p.id))
    }

`Params` is declared per page, in the page's own module, which is exactly why
`std/ui` cannot name it — so it is an open member type parameter, resolved at
the use site (RFC-0071 M2b).

## ParamLazy

```vyrn
type ParamLazy = { run: fn(P) -> T }
```

[`ParamQuery`] rendered lazily — the params-taking counterpart of [`Lazy`].

## query

```vyrn
fn query<T>(run: fn() -> T) -> Query<T>
```

A query over `run`, resolved before the page renders.

## lazy

```vyrn
fn lazy<T>(q: Query<T>) -> Lazy<T>
```

`q` marked lazy: the page renders its shell and a skeleton first, then fills
the data region in. The server always has the data, so SSR is unaffected.

## paramQuery

```vyrn
fn paramQuery<P, T>(run: fn(P) -> T) -> ParamQuery<P, T>
```

A query whose deferred call receives the page's route parameters.

## paramLazy

```vyrn
fn paramLazy<P, T>(q: ParamQuery<P, T>) -> ParamLazy<P, T>
```

`q` marked lazy — the params-taking counterpart of [`lazy`].

## runQuery

```vyrn
fn runQuery<T>(q: Query<T>) -> T
```

Run a query, producing the page's data.

## runLazy

```vyrn
fn runLazy<T>(q: Lazy<T>) -> T
```

Run a lazy query. Identical to [`runQuery`] — laziness is about how the
document is delivered, never about whether the data is fetched.

## runParamQuery

```vyrn
fn runParamQuery<P, T>(q: ParamQuery<P, T>, p: P) -> T
```

Run a params-taking query over this request's parameters.

## runParamLazy

```vyrn
fn runParamLazy<P, T>(q: ParamLazy<P, T>, p: P) -> T
```

Run a lazy params-taking query.

## noQuery

```vyrn
fn noQuery() -> Query<Unit>
```

The absent query — the default for a page that declares no `data`, and the
value a generator substitutes when it finds none. A page with no data has
nothing to produce, which is what `Query<Unit>` says.

## uiNoView

```vyrn
fn uiNoView() -> Html
```

The absent view — the default that makes `page` an optional member. A page
exports `page` or `respond`, never both and never neither, so neither default
is ever substituted; they exist so that a page writing one is not reported
for omitting the other.

## uiNoRespond

```vyrn
fn uiNoRespond() -> Response
```

The absent raw response. See [`uiNoView`].

## uiDataQuery

```vyrn
fn uiDataQuery() -> Int64
```

The `data` member's alternative-signature indices, as `matchedMember` reports
them — named once here so the generator never spells a bare 2 (RFC-0071 M2b).
Declaration order in `contract Page` is the contract; these four functions are
the only thing that has to move if it changes.

## uiDataLazy

```vyrn
fn uiDataLazy() -> Int64
```

`fn data() -> Lazy<T>` — no params, render-then-fill.

## uiDataParamQuery

```vyrn
fn uiDataParamQuery() -> Int64
```

`fn data() -> ParamQuery<P, T>` — params, blocking.

## uiDataParamLazy

```vyrn
fn uiDataParamLazy() -> Int64
```

`fn data() -> ParamLazy<P, T>` — params, render-then-fill.

## uiDataMarker

```vyrn
fn uiDataMarker() -> String
```

The data-request marker (RFC-0069 §2). The server `Request` exposes no headers,
so the marker rides the query string; `uiRouteSegments` already strips the query
from routing, so a marked request routes to the same page.

## uiIsDataRequest

```vyrn
fn uiIsDataRequest(path: String) -> Bool
```

Whether `path`'s query carries the data marker.

## uiPayload

```vyrn
fn uiPayload(page: String, title: String, props: String, params: String) -> String
```

Assemble a page data payload. `props`/`params` are already JSON (a `toJson`
result, or the literal `null`); `page`/`title` are JSON-encoded here.

## uiErrorPayload

```vyrn
fn uiErrorPayload(status: Int64, props: String) -> String
```

The `@error` data payload — the page the client renders on a load miss. Carries
a `title` so vyrn-nav v3 sets `document.title` on a client-rendered error.

## uiDataResponse

```vyrn
fn uiDataResponse(body: String) -> Response
```

Wrap a payload body in a `200 application/json` `Response`. A marked request is a
DATA fetch: it always answers 200 (the payload's `page`/`status` describe what to
render), while the UNMARKED document channel still returns a real 404 etc.

## uiDataMiss

```vyrn
fn uiDataMiss() -> Response
```

The data response for a true miss (no route matched): the themed error page's
payload at 404.

## uiErrorResponseOf

```vyrn
fn uiErrorResponseOf(e: PageError) -> Response
```

The `@error` data response for a load failure. Takes the `PageError` as a TYPED
parameter so `toJson`/field access see its concrete type — a `PageError` bound
directly from a `Result` match arm (`Err(e)`) otherwise loses its type for
`toJson` (RFC-0069 §2), the same reason the loaded data goes through the page's
own `encodeProps`.

## pages

```vyrn
fn pages(dir: String) -> String
```

`pages(dir)` — scan `dir` for page modules (`.vyrn` and, RFC-0039 §4, `.vyx`)
and synthesize the router module.

## pagesThemed

```vyrn
fn pagesThemed(dir: String, theme: String) -> String
```

`pagesThemed(dir, theme)` (RFC-0036/0039 §4) — like `pages`, but every `.vyx`
page in `dir` compiles its template classes against `theme` (a static class is
proven `⊆ Tw` at compile time, a dynamic one coerces at runtime). `.vyrn` pages
are unaffected. `theme` resolves relative to the importing module, like `dir`.

## pagesClient

```vyrn
fn pagesClient(dir: String) -> String
```

`pagesClient(dir)` — synthesize the CLIENT page bundle from `dir` (RFC-0069 §1).

## pagesClientThemed

```vyrn
fn pagesClientThemed(dir: String, theme: String) -> String
```

`pagesClientThemed(dir, theme)` — the themed client bundle (RFC-0069 §1); the
server side uses `pagesThemed`, this its client counterpart.
