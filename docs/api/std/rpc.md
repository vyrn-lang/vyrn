# std/rpc

std/rpc — Typed RPC as a library (RFC-0019), built entirely on RFC-0021
generator imports. The compiler knows nothing about RPC — everything below is
generated Vyrn source over `moduleInterface` reflection, `toJson`/`fromJson`
(RFC-0018), `jsonSchema` (RFC-0003), `extern` (RFC-0012), and the `handle`
convention (RFC-0016). No keyword, no builtin, no compilation roles.

A procedure is any EXPORTED function of a procedure module with 0 or 1
serializable parameters and a serializable (or `Unit`) return — the `Api`
contract plus the serializability rule, both declared below. Non-conforming
exports fail generation with a load diagnostic pointing at the generator call.

Two layers of generators:

- **Directory forms** (RFC-0072): `rpc(dir)` mounts every module under an api
  directory at its DERIVED path; `client(dir)` / `clientInProcess(dir)` emit
  the calling side. Nothing declares a route — the path is a function of the
  module path and the export name.
- **Single-module forms** (RFC-0019): `rpcServer`, `rpcClient` and
  `rpcInProcess` over one module, for a library that legitimately has one.

Inspect any generated module with:  vyrn emit-gen <file>
Inspect the derived table with:     vyrn routes <file>

## validateContract

```vyrn
fn validateContract(iface: ModuleInterface) -> String
```

Reject any module the RPC layer cannot model. Returns "" when it is clean.

Two checks, in the order a reader would apply them. First the `Api` contract
(RFC-0071): every export is a procedure, which is what the OPEN rule states.
Then serializability, which the contract grammar cannot express because it is
a property of the types rather than of the signature shape: at most one
parameter, and both ends nameable by the module's own reflection.

A `gen fn`, because `contractOf` is compile-time reflection and has no
runtime lowering by design (RFC-0071 M1) — the same reason `std/ui`'s
`uiContractErrs` is one. Every caller is already a `gen fn`.

Exported because `std/http`'s REST projection (RFC-0074) publishes the SAME
procedures over the same codec, so it applies the same rule. Two copies of
"what is a procedure" would be two things to drift.

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

## rpc

```vyrn
fn rpc(dir: String) -> String
```

`rpc(dir)` — mount every procedure module under `dir` at its derived path
(RFC-0072 M3).

Nothing in the tree declares a route. The path is a function of the module's
api-relative path and the export's name, configurable per project through
`vyrn.json`'s `rpc` key and per directory through an `rpc.json` beside the
modules. Two procedures deriving one path fail the build naming both.

## client

```vyrn
fn client(dir: String) -> String
```

`client(dir)` — the calling side of a derived api directory (RFC-0072 M3).

SERVER-BLIND by construction: it reads each module's INTERFACE through
`moduleInterface`, re-emits the type declarations it finds, and never imports
an api module. There is therefore no path — not even an accidental one
through the generator — by which a procedure body reaches a client bundle.

The emitted stubs are ordinary Vyrn, typechecked like anything else, which is
the whole point: the DX failures catalogued for inference-based clients
(router-size-proportional check latency, non-local depth errors, silent
degradation to `any`) all follow from INFERRING a client type from a runtime
router value. Here the client is generated from a checked declaration and
then checked, so the fortieth procedure costs the checker one more ordinary
module.

## clientInProcess

```vyrn
fn clientInProcess(dir: String) -> String
```

`clientInProcess(dir)` — the same surface as `client(dir)`, dispatching
directly instead of over the wire.

DEVIATION (see "M3 — as landed"): this document has ONE `client()` whose
backend is chosen by the calling module's audience. A generator receives its
arguments and nothing else — not the audience of the module that imported it
— so the choice cannot be made inside `client()` without teaching the loader
to pass audience into generation, which is a language change this milestone
does not own. The two flavors are two generators, exactly as `rpcClient` and
`rpcInProcess` already were, and the stubs are same-named, so a composition
root swaps one import line.
