# RFC-0016 — The Server: `vyrn serve` & the Async Decision

- **Status:** Implemented
- **Depends on:** RFC-0013 (module state + the host-owns-the-loop model),
  RFC-0014 (I/O), RFC-0003/RFC-0010 (validated types + jsonSchema as the
  wire contract — the long-range pitch this RFC starts cashing in)

> **Motivation.** The long-range goal is same-language server (native SSR) +
> client (wasm). The client half exists: a Vyrn module runs in a page, calls
> JS, and is called back. This RFC ships the server half's v1 — and settles
> the question it forces: does Vyrn need `async`/`await`? **No** (for now,
> deliberately) — see "The async decision" below.

---

## The model: the server is another host that owns the loop

RFC-0013 decided the browser shape: the host owns the loop; Vyrn owns state
and logic. A server is the *same shape* with a different host:

| phase | browser (RFC-0013) | server (this RFC) |
|---|---|---|
| module state init | `_start` | process start |
| `main` runs once | setup | setup |
| the host's loop | `setInterval` → exported handlers | accept loop → `handle(req)` |
| state between events | module `let mut` | module `let mut` |

```vyrn
let mut hits = 0

fn main() -> Int64 {
    print("server ready")          // runs once, before the first request
    return 0
}

fn handle(req: Request) -> Response {
    hits = hits + 1
    if req.path == "/health" {
        return Response { status: 200, contentType: "text/plain", body: "ok" }
    }
    return Response {
        status: 200,
        contentType: "text/html",
        body: "<h1>hello #\{hits.toString()} via \{req.method}</h1>",
    }
}
```

```
vyrn serve examples/server.vyrn --port 8080
```

## Surface

- **Injected record types** (parser-injected like `Schema`/`Issue`, hidden
  from user symbol indexes):

  ```vyrn
  type Request  = { method: String, path: String, body: String }
  type Response = { status: Int64, contentType: String, body: String }
  ```

  `path` includes the query string as sent (`/users?id=3`); `body` is the
  raw request body (`""` when absent). Finer accessors (headers, parsed
  query) are additive later.

- **`fn handle(req: Request) -> Response`** — an ordinary Vyrn function. No
  new declaration form, no attributes: `vyrn serve` requires the root file
  to define it (with exactly this signature) and errors otherwise. Being
  ordinary means it is testable (`test` blocks call it directly), checkable,
  and a parity citizen when exercised from `main`.

- **`main` is optional in a served file** — if present it runs once at
  startup (the setup hook, mirroring `_start`); a nonzero return aborts the
  serve. A file defining `handle` needs no `main` (extends the no-`main`
  rule: exports OR tests OR handle).

## `vyrn serve [file] [--port N]`

A hand-rolled HTTP/1.1 host inside `vyrn` (Rust `std::net` only — the
no-crates ethos), running `handle` under the **interpreter** (the reference
semantics; a compiled-server story is future work):

- Sequential accept loop, **one request at a time** (v1 is deliberately
  single-threaded — see the async decision). Module state is therefore
  race-free by construction, no locking, fully deterministic per request
  sequence.
- Parses the request line, headers, and a `Content-Length` body. Responds
  with `Content-Length` and `Connection: close` (no keep-alive/chunked in
  v1 — curl and every browser handle this fine). Malformed requests get a
  400 without reaching Vyrn; a *trap* inside `handle` (validation failure,
  OOB, division by zero) is caught, logged to the server's stderr with the
  canonical trap wording, and answered with a 500 — one bad request must
  not kill the server.
- Default port 8080; `--port N` overrides. Prints one line at startup
  (`serving <file> on http://localhost:<port>`) and one access line per
  request to stderr (`<method> <path> -> <status>`).
- Manifest-aware like every other command.

## The async decision (the point of this RFC)

**Vyrn does not add `async`/`await` now.** Reasons, in order:

1. **Function suspension is the highest-risk feature the invariant could
   face.** A CPS/state-machine transform must behave identically across
   three backends, touches ownership/drops/regions at every suspension
   point, and wasm cannot switch stacks (JSPI is not shippable). Months of
   surgery to buy latency overlap the current use cases don't need.
2. **The host-owns-the-loop model already covers the real use cases.**
   Browser: JS performs the async work and calls back into an exported
   handler (RFC-0012/0013 — shipping today). Server: concurrency-across-
   requests is the host's job, not the language's; `handle` stays a plain
   synchronous function.
3. **Determinism is the product.** Vyrn's concurrency (`spawn`) is
   deterministic by construction; nondeterministic completion ordering is
   exactly what an async runtime imports. When concurrency does come to the
   server it will be **worker threads calling `handle` in parallel**, gated
   on the isolation analysis (a `handle` that touches module state forces
   the sequential loop; a pure one may fan out) — same output, better
   wall-clock, no new language surface.

**Revisit triggers** (any one reopens the question): wasm stack-switching
ships broadly; outbound network calls from handlers (an HTTP *client*)
arrive and blocking-per-handler starts to hurt; or the threaded-`spawn`
runtime lands and wants an I/O-overlap story. Until then, "async Vyrn" is
spelled: start the work in the host, come back through a handler.

## Verification & parity

- `examples/server.vyrn` defines `handle` + a `main` that calls `handle`
  directly with constructed `Request`s and prints the responses — a normal
  three-way parity citizen (the serve host is CLI runtime, not language
  semantics).
- Integration tests in vyrn-cli: spawn `vyrn serve` on an ephemeral port,
  issue real HTTP requests (std `TcpStream`), assert status/body bytes,
  including: module state persisting across two requests, the 500-on-trap
  path (server survives), 400 on garbage, and `main` running first.
- Live verification: serve the example and load it in the browser pane —
  Vyrn-rendered HTML over real HTTP.

## Out of scope

Headers in/out beyond `contentType`, cookies, routing sugar, TLS, HTTP/2,
keep-alive, chunked encoding, worker threads (the concurrency follow-up
above), outbound HTTP client, WebSockets, a compiled/native serve runtime,
hot reload. Each is additive on this v1 without changing the model.
