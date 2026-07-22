# std/connect

std/connect — Connect wire compatibility as a library (RFC-0038), built
entirely on RFC-0021 generator imports. Two `gen fn`s over an ordinary
contract module — `connectServer` (emits the `connectHandle` router) and
`connectClient` (in-Vyrn caller) — that speak the Connect protocol's
**unary JSON** flavor:

  POST /<Service>.<Procedure>
  content-type: application/json
  errors: {"code": "...", "message": "...", "details": [...]}

The compiler knows nothing about Connect: everything below is generated Vyrn
source over `moduleInterface` reflection, `fromJson`/`toJson` (RFC-0018),
`extern` (RFC-0012), and the `handle` convention (RFC-0016) — the SAME
machinery `std/rpc` uses, mirrored (not modified). The point being proven
(RFC-0038): one contract serves `/rpc/*` and Connect paths simultaneously —
two protocols, one module, zero compiler changes.

Scope honesty: this is Connect's unary-JSON-over-HTTP/1 flavor only. True
gRPC (HTTP/2 framing + protobuf) is out — no protobuf codec exists and the
`serve` runtime has no HTTP/2. Streaming, the GET/query flavor, and
compression are also out (RFC-0038 "Out of scope").

Wire semantics (locked):
  - success => HTTP 200, body = `toJson(result)`;
  - a `fromJson` validation failure => HTTP 400, Connect error JSON with
    `code: "invalid_argument"` and the Issues carried LOSSLESS in `details`
    (each Issue's key/path/message preserved);
  - an unknown procedure under the service prefix => `{"code":"unimplemented"}`
    (HTTP 404, Connect's canonical mapping for that code);
  - an `Err` return of a `Result` procedure => HTTP 200 with the ordinary
    encoded value (RFC-0024: domain errors are values, not transport errors).

Inspect any generated module with:  vyrn emit-gen <file>

## connectServer

```vyrn
fn connectServer(contract: String) -> String
```

`connectServer(contract)` — emit a module that imports the contract's
procedures and exposes `connectHandle(req: Request) -> Option<Response>`,
mountable beside `rpcHandle` on the same `serve` root.

The generator is `connectServer` (not `connectHandle`) so it never shares a
top-level name with the router it emits — a `gen fn` and its generated module
are BOTH linked into the program, and the flat namespace forbids the clash.
This mirrors the `std/rpc` precedent exactly (`rpcServer` emits `rpcHandle`);
the RFC-0038 draft named the generator `connectHandle`, which is unrealizable
for that reason (recorded in the RFC's as-landed notes).

## connectClient

```vyrn
fn connectClient(contract: String) -> String
```

`connectClient(contract)` — emit the symmetric in-Vyrn client surface: the
contract's TYPE declarations verbatim, one same-named stub per procedure over
a single shared `vyrnConnectCall` extern that POSTs to the Connect path, and
one `export extern fn` completion dispatcher per procedure. The user writes
only `fn onGetBook(id: Int64, res: Validation<T>)`. Mirrors `rpcClient`'s
structure, speaking Connect paths/errors.
