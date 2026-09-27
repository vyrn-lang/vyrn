# std/ui

std/ui: file-based routing as a generator. `pages(dir)` scans a
directory of page modules at generation time and returns a router module.

  import { pages } from "std/ui"
  import { route } from pages("./pages")
  fn handle(req: Request) -> Response { return route(req) }

  pages/index.vyrn        -> GET /
  pages/items/index.vyrn  -> GET /items
  pages/items/[id].vyrn   -> GET /items/:id

A page module exports what the `Page` contract below allows. A page with
`[bracket]` segments exports `type Params`, whose field names must equal the
segments; a segment that does not parse as its type is a 404 before user
code runs. The router module exports `route(req: Request) -> Response`,
`type RoutePath` (the regex of every route), and a typed URL helper per
route. A Params mismatch, an unsupported param type or a route collision
fails the load with a report at the page module.

## PageError

```vyrn
type PageError = { status: Int64, message: String }
```

A page-load failure. The router renders the nearest `error.vyx`, or a
built-in error body, at `status`.

## pageError

```vyrn
fn pageError(status: Int64, message: String) -> PageError
```

## notFound

```vyrn
fn notFound(message: String) -> PageError
```

## badRequest

```vyrn
fn badRequest(message: String) -> PageError
```

## PageData

```vyrn
type PageData = Loading | Ready(T)
```

A lazy page's view data. `Loading` exists only on a client soft
nav; the server always renders `Ready`. Defined once here, because each
constructor may register only once.

## Meta

```vyrn
type Meta = { name: String, content: String }
```

## Head

```vyrn
type Head = { title: Option<String>, stylesheets: Array<String>, modules: Array<String>, scripts: Array<String>, meta: Array<Meta> }
```

What a page contributes to the document head, built from `noHead()` and
the `with*` combinators:

    export fn head() -> Head {
        return withModule(noHead(), "/app.js")
    }

## noHead

```vyrn
fn noHead() -> Head
```

## withTitle

```vyrn
fn withTitle(h: Head, title: String) -> Head
```

The page's title wins over its layouts'.

## withStylesheet

```vyrn
fn withStylesheet(h: Head, href: String) -> Head
```

## withModule

```vyrn
fn withModule(h: Head, src: String) -> Head
```

## withScript

```vyrn
fn withScript(h: Head, src: String) -> Head
```

## withMeta

```vyrn
fn withMeta(h: Head, name: String, content: String) -> Head
```

## headHtml

```vyrn
fn headHtml(h: Head) -> Array<Html>
```

Stylesheets, then modules, then scripts, then meta, each in the order added.

## headTitleOf

```vyrn
fn headTitleOf(h: Head) -> String
```

`""` when the head declares no title.

## Query

```vyrn
type Query = { run: fn() -> T }
```

A page's data, produced by a deferred call. The page blocks until the data
lands; return a [`Lazy`] to render first and fill in later.

    export fn data() -> Query<Array<Paste>> {
        return query(|| listPastes().pastes)
    }

## Lazy

```vyrn
type Lazy = { run: fn() -> T }
```

A page's data when the page renders lazily: on a client soft nav
the shell paints first and the data region fills in. A distinct type, not a
flag, because laziness decides the view's type (`PageData<T>`), and the
generator reads types, not function bodies.

## ParamQuery

```vyrn
type ParamQuery = { run: fn(P) -> T }
```

A page's data when it depends on the route parameters. `P` is the page's own
`Params`, which `std/ui` cannot name.

    export fn data() -> ParamQuery<Params, Result<Paste, PageError>> {
        return paramQuery(|p: Params| fetch(p.id))
    }

## ParamLazy

```vyrn
type ParamLazy = { run: fn(P) -> T }
```

## query

```vyrn
fn query<T>(run: consume fn() -> T) -> Query<T>
```

## lazy

```vyrn
fn lazy<T>(q: consume Query<T>) -> Lazy<T>
```

The server always has the data, so SSR is unaffected.

## paramQuery

```vyrn
fn paramQuery<P, T>(run: consume fn(P) -> T) -> ParamQuery<P, T>
```

## paramLazy

```vyrn
fn paramLazy<P, T>(q: consume ParamQuery<P, T>) -> ParamLazy<P, T>
```

## runQuery

```vyrn
fn runQuery<T>(q: Query<T>) -> T
```

## runLazy

```vyrn
fn runLazy<T>(q: Lazy<T>) -> T
```

Laziness changes how the document is delivered, not whether data is fetched.

## runParamQuery

```vyrn
fn runParamQuery<P, T>(q: ParamQuery<P, T>, p: P) -> T
```

## runParamLazy

```vyrn
fn runParamLazy<P, T>(q: ParamLazy<P, T>, p: P) -> T
```

## noQuery

```vyrn
fn noQuery() -> Query<Unit>
```

The default `data` for a page that declares none.

## uiNoView

```vyrn
fn uiNoView() -> Html
```

The default that makes `page` optional. A page exports `page` or `respond`,
so this is never substituted.

## uiNoRespond

```vyrn
fn uiNoRespond() -> Response
```

The default that makes `respond` optional. See [`uiNoView`].

## uiDataQuery

```vyrn
fn uiDataQuery() -> Int64
```

The `data` member's shape indices as `matchedMember` reports them, in the
declaration order of `contract Page`. `fn data() -> Query<T>`.

## uiDataLazy

```vyrn
fn uiDataLazy() -> Int64
```

## uiDataParamQuery

```vyrn
fn uiDataParamQuery() -> Int64
```

## uiDataParamLazy

```vyrn
fn uiDataParamLazy() -> Int64
```

## uiWantsData

```vyrn
fn uiWantsData(req: Request) -> Bool
```

JSON wins only when the client names `application/json` and not
`text/html`. A browser navigation, `*/*` and a missing `Accept` get HTML.
No q-value ranking: no client of this router separates the two by weight.

## uiPayload

```vyrn
fn uiPayload(page: String, title: String, props: String, params: String) -> String
```

`props` and `params` are already JSON; `page` and `title` are encoded here.

## uiErrorPayload

```vyrn
fn uiErrorPayload(status: Int64, props: String) -> String
```

The `@error` payload a client renders on a load miss.

## uiDataResponse

```vyrn
fn uiDataResponse(body: String) -> Response
```

A data fetch always answers 200; the payload says what to render. `vary:
"Accept"` keeps a shared cache from serving a page's JSON to a browser.

## uiDataMiss

```vyrn
fn uiDataMiss() -> Response
```

The data response when no route matched.

## uiErrorResponseOf

```vyrn
fn uiErrorResponseOf(e: PageError) -> Response
```

`e` is a typed parameter so `toJson` sees `PageError`: bound from an
`Err(e)` arm it loses its type for `toJson`.

## pages

```vyrn
fn pages(dir: String) -> String
```

Scans `dir` for `.vyrn` and `.vyx` pages and returns the router module.

## pagesThemed

```vyrn
fn pagesThemed(dir: String, theme: String) -> String
```

[`pages`], with every `.vyx` page's classes checked against `theme`.
`theme` resolves relative to the importing module.

## pagesClient

```vyrn
fn pagesClient(dir: String) -> String
```

The client page bundle for `dir`.

## pagesClientThemed

```vyrn
fn pagesClientThemed(dir: String, theme: String) -> String
```

[`pagesClient`] with a theme; the server side is [`pagesThemed`].
