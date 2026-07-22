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
