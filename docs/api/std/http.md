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

## IsMissing

```vyrn
type IsMissing = fn(String) -> Bool
```

`notFoundWhen`'s question, asked of the `Err` payload a procedure returned:
"is THIS the absence this resource is named after?" A `String` and not the
procedure's own error type, because `Route` erases the signature — and that
erasure is the property the whole design exists to keep.

## Route

```vyrn
type Route = { method: String, pattern: String, prefix: Bool, run: Handler, whole: Surface, derived: String, maxAge: Int64, validator: Bool, modified: String, varyOn: String, ok: Int64, location: String, missing: IsMissing }
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

The M2 policy fields below are flat on `Route` rather than nested in a
`Policy` record, for one reason: nesting would put a `fn` value one level
deeper inside a record that is copied on every combinator, and M1 already paid
for finding out which `fn`-in-a-record shapes lower on all three engines. Flat
costs one `httpCopy` listing thirteen fields; nested would have cost the same
listing plus a shape nobody has proven.

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

## Policy

```vyrn
protocol Policy { fn cacheFor(self, Int64) -> Route; fn etag(self) -> Route; fn lastModified(self, String) -> Route; fn vary(self, String) -> Route; fn status(self, Int64) -> Route; fn createdAt(self, String) -> Route; fn notFoundWhen(self, IsMissing) -> Route }
```

The route policy: seven methods, each `Route -> Route`, so the chain stays
value-level — the fortieth route with the fortieth policy is still one
nominal value to the checker, and `Route` gains no type parameter.

They shipped as PREFIX functions (`etag(cacheFor(GET(byId("/{id}")), 3600))`)
because a method call on a user value needs a protocol impl and `impl P for
Route` was refused outright: a record carried no runtime name for the
interpreter to dispatch on. RFC-0084 M1 removed the refusal and M2 lifted the
native backend's variable-receiver rule, which is what a chain actually needs
— every receiver after the first is a call. So this is RFC-0074's own designed
spelling, three milestones late:

```vyrn
GET(byId("/{id}")).cacheFor(3600).etag().notFoundWhen(|why| why == "no such user")
```

There are no free-function versions left. Two spellings of one combinator
would be two things to keep in step, and the checker would not have them
anyway: a protocol method and a top-level function cannot share a name.

The seven reasons live here, in the protocol's own doc, and not on the method
signatures below: `MethodSig` carries no doc field, so a `///` inside a
protocol body is dropped by the parser and reaches neither `vyrn doc` nor
hover. That is the same reflection gap RFC-0038 recorded for `FnInfo`.

### `cacheFor(seconds)` — `Cache-Control: max-age=N` on a successful answer

Deliberately NOT `public` and not `private`. `public` would tell a shared
cache to store a response it is otherwise forbidden to store — the one for a
request that carried `Authorization` (RFC 9111 §3.5) — which is the
cache-poisoning shape, one user's copy served to the next. `private` would
make `cacheFor` useless for the public API this projection exists to publish.
Bare `max-age` gets both: a CDN may cache the anonymous GET, and the spec's
own default already refuses the credentialed one.

### `etag()` — a strong `ETag`, and `If-None-Match` answered with 304

The validator is `FNV-1a-64(contentType + "\n" + body)` in hex. It hashes the
REPRESENTATION — the bytes plus the media type that says how to read them —
because two `Vary`-selected variants of one URL are different representations
and must not share a validator. It hashes CONTENT and nothing else: no clock,
no counter, no process identity, so two processes serving the same bytes emit
the same tag and a client's `If-None-Match` still matches after a restart or
across a load-balanced pair. A per-process seed here would make the whole
feature silently inert.

64 bits: a collision serves a 304 for content the client does not have. At
that width it takes ~5 billion distinct representations of one URL for a 50%
chance, and the same hash already assigns this repo's paste ids.

### `lastModified(field)` — `Last-Modified` from an epoch-millis field

The field is named rather than selected by a closure for the reason
`IsMissing` records: `Route` has erased the procedure's output type by the
time a combinator sees it. A name that no longer exists writes no header
rather than trapping — a validator is an optimization, and losing one must not
lose the response. `If-Modified-Since` is answered with 304.

### `vary(headers)` — the `Vary` header for this route

Writes the `Response.vary` field RFC-0072 M4 already ships — one negotiation
channel with one reader, not a second one hidden in the header map.

### `status(code)` — the status a SUCCESSFUL answer carries

202 for an accepted job, 201 without a Location. An error keeps its own: a 422
from `fromJson` is the codec's answer and not this route's to relabel.

### `createdAt(template)` — `201 Created` with a `Location` from the response

`"/pastes/{id}"` takes `{id}` from the created object's `id` field. A template
and not the RFC's `|p| "/pastes/\{p.id}"`, and this is the honest version of
that line: the closure it shows takes the procedure's OUTPUT type, and a
`Route` that could carry one would be `Route<T>` — the type-level chain this
RFC exists to refuse. The template is checked at runtime against the fields
actually present; an unknown `{name}` is left verbatim in the URL, where it is
loud.

### `notFoundWhen(isMissing)` — which `Err` payloads are an absence

`.notFoundWhen(|why| why == "no such paste")` turns that one error into a 404
and leaves every other `Err` a 200 carrying the error the procedure returned.
The lambda captures nothing, which is what makes it lowerable: M1's native
codegen bug was a lambda CALLING a captured `fn`-typed parameter, and a
predicate over a `String` has neither.

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
