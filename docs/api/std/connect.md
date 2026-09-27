# std/connect

std/connect -- Connect wire compatibility as a library, built on
generator imports. `connectServer` emits the `connectHandle` router
and `connectClient` the in-Vyrn caller, for Connect's unary JSON flavor over
HTTP/1 only: no gRPC, streaming, GET flavor or compression.

  POST /<Service>.<Procedure>
  content-type: application/json
  errors: {"code": "...", "message": "...", "details": [...]}

Wire semantics:
  - success: 200, body `toJson(result)`;
  - a `fromJson` failure: 400, `code: "invalid_argument"`, every Issue in `details`;
  - an unknown procedure under the service prefix: 501 `unimplemented`;
  - a known procedure with a non-POST method: 405, as `std/rpc` does;
  - an `Err` from a `Result` procedure: 200 with the encoded value.

## connectServer

```vyrn
fn connectServer(contract: String) -> String
```

Emits a module exposing `connectHandle(req: Request) -> Option<Response>`.
The generator and its module link into one flat namespace, so the generator
cannot itself be named `connectHandle` (as `rpcServer` emits `rpcHandle`).

## connectClient

```vyrn
fn connectClient(contract: String) -> String
```

Emits the client: the contract's type declarations, one stub per procedure
over one `vyrnConnectCall` extern, and one completion per procedure. The user
writes only `fn onGetBook(id: Int64, res: Validation<T>)`.
