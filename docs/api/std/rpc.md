# std/rpc

std/rpc — Typed RPC as a library (RFC-0019), built entirely on RFC-0021
generator imports. Three `gen fn`s over an ordinary contract module:
`rpcServer`, `rpcClient`, and `rpcInProcess`. The compiler knows nothing
about RPC — everything below is generated Vyrn source over `moduleInterface`
reflection, `toJson`/`fromJson` (RFC-0018), `jsonSchema` (RFC-0003), `extern`
(RFC-0012), and the `handle` convention (RFC-0016). No keyword, no builtin,
no compilation roles.

A procedure is any EXPORTED function of the contract module with 0 or 1
codable parameters and a codable (or `Unit`) return. Non-conforming exports
fail generation with a load diagnostic pointing at the generator call.

Inspect any generated module with:  vyrn emit-gen <file>

## rpcServer

```vyrn
fn rpcServer(contract: String) -> String
```

`rpcServer(contract)` — emit a module that imports the contract's procedures
and exposes `rpcHandle(req: Request) -> Option<Response>` plugging into the
RFC-0016 `handle` convention. The mount surface is exactly the module you
point it at: explicit, no transitive surprises.

## rpcClient

```vyrn
fn rpcClient(contract: String) -> String
```

`rpcClient(contract)` — emit the client surface: the contract's TYPE
declarations verbatim (the client build never links server bodies, so there
is nothing to strip), one same-named stub per procedure over a single shared
`vyrnRpcCall` extern, a per-procedure pending-callback map, and one
`export extern fn` completion dispatcher per procedure. The caller passes the
callback inline: `createPaste(req, |res| match res { .. })` (RFC-0040 §2).

## rpcInProcess

```vyrn
fn rpcInProcess(contract: String) -> String
```

`rpcInProcess(contract)` — the deterministic test / SSR flavor. Emits a
module that imports the real procedures (each under a `__real` alias) + their
types from the contract and exposes a **same-named** `<proc>(req, cb)` stub per
procedure that runs the real logic and calls `cb(Valid(..))` synchronously
(RFC-0040 §2). Callers use the exact procedure names the wire client exposes.

Same-named stubs are possible because of import aliasing (RFC-0022): the
generated module imports `getUser as getUser__real` and its stub takes the
real name, forwarding to the alias. (Before RFC-0022 the dispatchers had to
be named `call<Proc>`, since a stub could not share a name with the real
function in the flat namespace — that deviation is now gone.)
