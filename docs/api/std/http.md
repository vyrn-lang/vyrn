# std/http

## Handler

```vyrn
type Handler = fn(Request, Map<String, String>) -> Option<Response>
```

What a mounted route runs: the request, plus the placeholder bindings the
pattern captured. `None` declines — a whole mounted subsystem (the derived RPC
surface, a page router) answers for some paths under its prefix and passes on
the rest, and `mount` continues to the next group when it does.

## Surface

```vyrn
type Surface = fn(Request) -> Option<Response>
```

A whole mounted subsystem: it takes the request and nothing else, because a
prefix binds no placeholders. `rpcHandle` and a page router already have this
shape (the RFC-0016 `handle` convention).

## Route

```vyrn
type Route = { method: String, pattern: String, prefix: Bool, run: Handler, whole: Surface, derived: String }
```

One route: an HTTP method, a full path pattern (base + sub-path), what runs,
and the `derived` policy line the diagnostics quote. `prefix` marks a whole
subsystem mounted under `pattern` rather than a single path, and selects
which of the two callables `mount` invokes.

`method` is `""` until a method constructor sets it — a route that reaches
`mount` without one is a startup error, not a silent GET.

Two fn fields rather than one plus an adapting closure, and the reason is a
backend limit worth naming: a lambda that CALLS a captured `fn`-typed
PARAMETER (`|req, ps| run(req)`) lowers to a call on a symbol no module
defines, so it runs under the interpreter and fails to build natively. Each
route uses one field and leaves the other at a named never-answers stub,
which costs a word and no closure at all.

## httpRoute

```vyrn
fn httpRoute(pattern: String, run: Handler, derived: String) -> Route
```

Build a route with no method yet. What a generated per-procedure constructor
calls; `pattern` is already base-qualified.

## GET

```vyrn
fn GET(r: Route) -> Route
```

`GET` this route. The four below are the same call with another verb; the
vocabulary is HTTP's, so `PUT` and `PATCH` are separate words rather than one
`method("PUT")` taking a string nobody can complete or spell-check.

UPPERCASE, against the house style, because `get` and `set` are RESERVED
(the `cell`/`Ref` builtins dispatch before user functions, so a `fn get`
would be silently unreachable and the checker rejects it outright). Given
that `get` is unavailable, the wire spelling is the better fallback than a
coined synonym: `GET` is what the method IS, and it keeps the five verbs
spelled one way.

## POST

```vyrn
fn POST(r: Route) -> Route
```

## PUT

```vyrn
fn PUT(r: Route) -> Route
```

## PATCH

```vyrn
fn PATCH(r: Route) -> Route
```

## DELETE

```vyrn
fn DELETE(r: Route) -> Route
```

## surface

```vyrn
fn surface(prefix: String, run: Surface) -> Route
```

A whole subsystem mounted under `prefix`: the derived RPC surface, a page
router. It answers with `Some` or declines with `None` — the same
`handle`-convention shape those already have (RFC-0016) — so mounting one
costs no adapter at the composition root.

## mount

```vyrn
fn mount(req: Request, groups: Array<Array<Route>>) -> Option<Response>
```

Resolve `req` against the mounted groups, in order, first match wins.

Overlaps BETWEEN groups are a startup error, not a silent shadow: a route in a
later group that an earlier group already answers for is unreachable, and an
unreachable route is the kind of thing nobody notices until production. Order
*within* a group is the author's own, and left alone.

The check runs on every call rather than once, because there is no init hook
to hang it on and a router is a handful of routes: the scan is O(routes²) over
a number that is 5 in `examples/bin` and would have to reach the thousands
before it showed up next to a JSON decode. `vyrn serve` runs `main` at
startup, so an app that touches `handle` there sees the trap at startup, which
is where it belongs.

## httpInput

```vyrn
fn httpInput(ps: Map<String, String>, body: String, numeric: Array<String>) -> String
```

The JSON a generated adapter hands `fromJson`: the placeholder bindings as
object fields, with the request body's own fields after them.

A captured segment is text, but the field it binds may not be: `/users/{id}`
over an `Int64` id must produce `{"id":7}`, not `{"id":"7"}`, or every numeric
REST route in existence decodes as a 422. The generator resolves each
placeholder's field to its base type and passes the numeric ones in
`numeric`; a value that does not parse as an integer stays a string, so the
decoder reports a typed issue naming the field rather than this function
inventing a number. A `Float64` path field is text here too — `parse` is
integer-only, and no dogfood has one.

## http

```vyrn
fn http(module: String) -> String
```

`http(module)` — the REST projection of one procedure module: a
placeholder-checked constructor per procedure, mounted under the base path its
stem derives.

Inspect the generated module with:  vyrn emit-gen <file>
