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
  - optionally `fn load(p: Params) -> Validation<Data>` and then
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
type Query = { run: fn() -> T, lazy: Bool }
```

A page's data: the call that produces it, deferred until the router asks, and
whether the page renders LAZILY (RFC-0070 — the shell paints instantly on a
client soft nav and the data region fills in when the payload lands).

    export fn data() -> Query<Array<Paste>> {
        return lazy(query(|| listPastes().pastes))
    }

## query

```vyrn
fn query<T>(run: fn() -> T) -> Query<T>
```

A query over `run`, resolved before the page renders.

## lazy

```vyrn
fn lazy<T>(q: Query<T>) -> Query<T>
```

`q` marked lazy: the page renders its shell and a skeleton first, then fills
the data region in. The server always has the data, so SSR is unaffected.

## runQuery

```vyrn
fn runQuery<T>(q: Query<T>) -> T
```

Run a query, producing the page's data.

## noQuery

```vyrn
fn noQuery() -> Query<Unit>
```

The absent query — the default for a page that declares no `data`, and the
value a generator substitutes when it finds none. A page with no data has
nothing to produce, which is what `Query<Unit>` says.

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
