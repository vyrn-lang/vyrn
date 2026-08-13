# std/http

std/http — the REST projection (RFC-0074 M1), a library over the same
procedures the derived RPC surface reflects on. The compiler knows nothing
about routes: everything below is either an ordinary record moved around at
runtime or generated Vyrn source over `moduleInterface` reflection, exactly
as `std/rpc`, `std/openapi` and `std/graphql` are.

A projection is opt-in and hand-written, because `GET /pastes/{id}` is API
*design* — no reflection can derive it, and deriving it would need the magic
naming conventions RFC-0072 exists to remove. You write one when you publish
a public API, and not before:

```vyrn
// server/api/pastes.http.vyrn — base path `/pastes`, from the stem.
import { http, Route, GET, POST } from "std/http"
import { recent, byId, create } from http("./pastes")

export fn routes() -> Array<Route> {
    return [GET(recent("/")), GET(byId("/{id}")), POST(create("/"))]
}
```

**The chain is value-level, not type-level.** `Route` is `Route` after every
call — `GET(..)` takes a `Route` and hands back a `Route`, and so does every
method of `Policy` below. Nothing accumulates in the type, so the
fortieth route costs the checker one more nominal value rather than a wider
instantiation. That is the property this design exists to keep.

The shape is `GET(byId("/{id}"))` and not the RFC's `get("/{id}", byId)`, for
one reason: the pattern has to sit in the *procedure's own* parameter slot for
its placeholders to be checked at compile time. `byId`'s generated parameter
type is a `String where value =~ …` admitting exactly the placeholders `IdReq`
has fields for, so `byId("/{ID}")` is a checker error at the call site and
needs neither RFC-0073's symbol map nor a compiler rule. A uniform
`GET(pattern, proc)` cannot do that: it would have to be generic over the
signature, and a generic cannot name a type to `fromJson` — the first argument
there must be a declared type name, which is precisely what a type parameter
is not.

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

Two fn fields rather than one plus an adapting closure. M1 chose this because
`|req, ps| run(req)` — a lambda CALLING a captured `fn`-typed parameter — did
not lower in either compiled backend; that hole is closed and the shape stays
anyway, now as a choice rather than a workaround. An adapter would allocate a
capture block per mounted subsystem and turn a field read into a dispatch on
every request; two fields cost a word and no closure at all, and each route
leaves the one it does not use at a named never-answers stub.

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

UPPERCASE, against the house style. `get` was RESERVED when this was
written — the `cell`/`Ref` builtins dispatched before user functions — and
RFC-0090 M4 deleted those builtins, so the name is free now (`std/slots`
took it). The spelling stays: `GET` is what the method IS, and the five
verbs are one vocabulary spelled one way. `get` alone in lowercase would
read as this module's reader rather than as a route's method.

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
fn surface(prefix: String, run: consume Surface) -> Route
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

### `fn cacheFor(self, Int64) -> Route`

`Cache-Control: max-age=N` on a successful answer.

Deliberately NOT `public` and not `private`. `public` would tell a shared
cache to store a response it is otherwise forbidden to store — the one for
a request that carried `Authorization` (RFC 9111 §3.5) — which is the
cache-poisoning shape, one user's copy served to the next. `private` would
make `cacheFor` useless for the public API this projection exists to
publish. Bare `max-age` gets both: a CDN may cache the anonymous GET, and
the spec's own default already refuses the credentialed one.

### `fn etag(self) -> Route`

A strong `ETag`, and `If-None-Match` answered with 304.

The validator is `FNV-1a-64(contentType + "\n" + body)` in hex. It hashes
the REPRESENTATION — the bytes plus the media type that says how to read
them — because two `Vary`-selected variants of one URL are different
representations and must not share a validator. It hashes CONTENT and
nothing else: no clock, no counter, no process identity, so two processes
serving the same bytes emit the same tag and a client's `If-None-Match`
still matches after a restart or across a load-balanced pair. A
per-process seed here would make the whole feature silently inert.

64 bits: a collision serves a 304 for content the client does not have. At
that width it takes ~5 billion distinct representations of one URL for a
50% chance, and the same hash already assigns this repo's paste ids.

### `fn lastModified(self, String) -> Route`

`Last-Modified` from an epoch-millis field.

The field is named rather than selected by a closure for the reason
`IsMissing` records: `Route` has erased the procedure's output type by the
time a combinator sees it. A name that no longer exists writes no header
rather than trapping — a validator is an optimization, and losing one must
not lose the response. `If-Modified-Since` is answered with 304.

### `fn vary(self, String) -> Route`

The `Vary` header for this route.

Writes the `Response.vary` field RFC-0072 M4 already ships — one
negotiation channel with one reader, not a second one hidden in the header
map.

### `fn status(self, Int64) -> Route`

The status a SUCCESSFUL answer carries.

202 for an accepted job, 201 without a Location. An error keeps its own: a
422 from `fromJson` is the codec's answer and not this route's to relabel.

### `fn createdAt(self, String) -> Route`

`201 Created` with a `Location` built from the response.

`"/pastes/{id}"` takes `{id}` from the created object's `id` field. A
template and not the RFC's `|p| "/pastes/\{p.id}"`, and this is the honest
version of that line: the closure it shows takes the procedure's OUTPUT
type, and a `Route` that could carry one would be `Route<T>` — the
type-level chain this RFC exists to refuse. The template is checked at
runtime against the fields actually present; an unknown `{name}` is left
verbatim in the URL, where it is loud.

### `fn notFoundWhen(self, IsMissing) -> Route`

Which `Err` payloads are an absence.

`.notFoundWhen(|why| why == "no such paste")` turns that one error into a
404 and leaves every other `Err` a 200 carrying the error the procedure
returned. The lambda captures nothing at all, which is the cheapest thing
it can be — no capture block, no dispatch.

## Feed

```vyrn
type Feed = fn(Request, Map<String, String>, Int64) -> Stream<String>
```

What a mounted stream route runs: the request, the placeholder bindings, and
the CURSOR — the seed the producer resumes from, which is a reconnecting
client's `Last-Event-ID` when the route is `resumable` and `0` otherwise.

It hands back a `Stream<String>` of already-encoded frames (build them with
`event` below). The element is a frame rather than a record for a reason that
is about `std/stream` rather than about taste: `map` and `filter` are eager
walks over a buffer, so mapping a record stream into a frame stream would
materialise the feed — which is `#6156`, the incident RFC-0075 quotes,
arriving through the library written to prevent it. The producer encodes as
it goes, and stays lazy.

## Live

```vyrn
type Live = { pattern: String, feed: Feed, retry: Int64, resume: Bool, derived: String }
```

A mounted event stream — and NOT a `Route`, which is the whole point.

`Route` carries `Policy`, and every one of its seven — `cacheFor`, `etag`,
`lastModified`, `vary`, `status`, `createdAt`, `notFoundWhen` — is about a
response that exists all at once. A validator for bytes that have not been
produced yet is not a thing, and a 304 for a feed is not either. This RFC's
rule is that an option meaningless to a transport is ABSENT from it rather
than ignored, so a stream is a different record with a different protocol, and
`mount` takes both. That separation is only spellable since RFC-0084 M1 made a
record a legal protocol target; before it, both would have had to be one
record with the options quietly doing nothing.

## sse

```vyrn
fn sse(pattern: String, feed: consume Feed) -> Live
```

`sse(pattern, feed)` — mount `feed` as an event stream at `pattern`.

The pattern is written here rather than in the procedure's own parameter slot,
which is where M1 put it and why: that trick exists to check `{id}` against a
procedure's INPUT RECORD, and a feed takes the request rather than a decoded
record — there are no fields to check the placeholders against. So `sse` is
the RFC's own `sse("/", tail)` spelling, and it is not a weaker version of
`GET(byId("/{id}"))`; it is the same rule with nothing to check.

## Wire

```vyrn
protocol Wire { fn retryAfter(self, Int64) -> Live; fn resumable(self) -> Live }
```

A stream's policy, and it has nothing in common with `Policy`'s.

`keepAlive` is NOT here, and its absence is the same rule as `Policy`'s
absence: a pull producer that has nothing to say ends rather than idling, so
there is no idle connection for a keep-alive comment to hold open. The RFC
lists it beside the other two; there is nothing under it to build.

### `fn retryAfter(self, Int64) -> Live`

The reconnect hint: `retry: N` before the first frame.

A pull producer answers `Some` or `None`; it has no way to say "nothing
yet". So a feed that catches up ENDS, the connection closes, and the
client comes back `ms` later — with its `Last-Event-ID` if the route is
`resumable`. That makes `retryAfter` the poll interval rather than a
failure hint, and it is the honest shape for a language with no
concurrency: the alternative is a producer blocking the single-threaded
server while it waits for something to happen.

### `fn resumable(self) -> Live`

`Last-Event-ID` becomes the producer's seed.

RFC-0075 M2b made a stream's cursor its resume token, so replay is not a
feature: the header's value is handed to the feed as its seed and the
ordinary code path produces exactly what the client has not seen. Write
that cursor as each frame's `id`, which is what `event` takes it for.
Without `resumable` the seed is always `0` and a reconnecting client gets
the feed from the top.

## event

```vyrn
fn event(id: String, name: String, data: String) -> String
```

One SSE frame: `id:`, `event:`, the `data:` lines, and the blank line that
ends it. An empty `id` or `name` writes no field rather than an empty one.

**A line terminator is CR, LF or CRLF** — all three, per the EventSource
grammar, not `\n` alone. So every one of them is a field terminator, and the
single rule here is that **no argument can end a line the caller did not ask
for**:

  - `data` KEEPS every byte, because the format has a way to carry a line
    break: each break becomes a second `data:` line, which the client rejoins
    with `\n`. CR, LF and CRLF all split alike, so a payload with Windows line
    endings arrives whole instead of truncating at the first CR.
  - `id` and `name` have no such way — the field is one line by construction —
    so a CR or an LF in them is REMOVED rather than written. Left in, a newline
    would close the field and the rest of the argument would be read as further
    SSE fields: an `id` of `1\nevent: x\ndata: y` writes a whole second event.
    Stripping is what keeps an untrusted id from injecting one. This is
    `std/html`'s answer to an unsafe name — refuse the bytes rather than
    pretend to escape them — and the value is otherwise preserved.

The spec's third `id` hazard, a NUL (a client discards an id carrying one), is
unreachable here rather than handled: a Vyrn `String` is NUL-terminated, and
every way to make one — a literal, `stringFromBytes`, `readFile`, `readLine` —
refuses an embedded NUL before this function could see it.

## Socket

```vyrn
type Socket = { pattern: String, feed: Feed, closing: Int64, subproto: String, fragment: Int64, derived: String }
```

A mounted WebSocket, and — like `Live` — not a `Route`.

**Server-push only.** RFC 6455 is bidirectional; this is not. A `Socket`
consumes a `Stream<T>` and all four of the RFC's options are server-side, so a
client→server message has no shape here: it would want a handler per inbound
message, which is a different design and not one RFC-0074 spells. The host
still PARSES inbound frames — enough to honour a client-initiated close and
§5.1's rule that a client's frames are masked — because ignoring the bytes a
peer sends is not the same as not supporting inbound messages.

`heartbeat` is absent, and it is this milestone's refusal — the same one
`keepAlive` got from `Wire`, checked rather than assumed. A ping is
HOST-generated where a keep-alive comment would have been producer-generated,
which looked like the difference that would save it, and it is not: between
frames the host is not idle, it is blocked inside the producer waiting for the
next payload, so there is no moment for a timer to fire in. What a heartbeat
is FOR is detecting a peer that has gone away without saying so, and the host
already learns that by writing to it and failing — which is the signal this
adapter shares with `sse`, and it costs no ping.

## ws

```vyrn
fn ws(pattern: String, feed: consume Feed) -> Socket
```

`ws(pattern, feed)` — mount `feed` as a WebSocket at `pattern`.

The feed's element is the message PAYLOAD, not a frame, and that asymmetry
with `sse` is the rule rather than an inconsistency: **Vyrn owns what the user
chooses and the host owns what the protocol fixes.** SSE's `data:`, `id:` and
`retry:` are a design surface — which event name, which id — so `event(..)` is
Vyrn and the host writes what it is handed. A frame's opcode, length and mask
are not a surface; there is no choice in any of them. So the host frames.

## Frames

```vyrn
protocol Frames { fn closeCode(self, Int64) -> Socket; fn subprotocol(self, String) -> Socket; fn maxFrame(self, Int64) -> Socket }
```

A socket's policy. Three options, all server-side, all with something under
them — `heartbeat` is the fourth and is refused; see [`Socket`].

## mount

```vyrn
fn mount(req: Request, groups: Array<Array<Route>>, live: Array<Live>, sockets: Array<Socket>) -> Option<Response>
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

The fourth parameter is `ws`'s, and it is a fourth parameter for the same
reason `live` was a third: Vyrn has no sum over two record types, and a
`Socket` is deliberately not a `Live` — `retryAfter`/`resumable` mean nothing
to a WebSocket and `closeCode`/`subprotocol`/`maxFrame` mean nothing to an
event stream. An app with neither writes `[], []` and pays a word.

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
