# std/rpc

std/rpc -- typed RPC as a library, built on generator imports.
A procedure is an exported function of a procedure module with at
most one parameter and both ends serializable (the `Api` contract plus
`rpcWhyNotSerializable`); anything else fails the load at the generator call.

- Directory forms: `rpc(dir)` mounts every module under an api
  directory at a path derived from the module path and the export name;
  `client(dir)` and `clientInProcess(dir)` emit the calling side.
- Single-module forms: `rpcServer`, `rpcClient` and `rpcInProcess`.

Inspect a generated module with `vyrn emit-gen <file>`, the derived table
with `vyrn routes <file>`.

## validateContract

```vyrn
fn validateContract(iface: ModuleInterface) -> String
```

The first objection to `iface` as a procedure module, or "": the `Api`
contract, then serializability. A `gen fn`, because `contractOf` has no
runtime lowering. Exported for `std/http`'s REST projection, which publishes
the same procedures under the same rule.

## rpcServer

```vyrn
fn rpcServer(contract: String) -> String
```

Emits a module that imports the contract's procedures and exposes
`rpcHandle(req: Request) -> Option<Response>`.

## rpcClient

```vyrn
fn rpcClient(contract: String) -> String
```

Emits the client: the contract's type declarations, one stub per procedure
over one `vyrnRpcCall` extern, and one completion per procedure. The caller
passes the callback inline: `createPaste(req, |res| match res { .. })`.

## rpcInProcess

```vyrn
fn rpcInProcess(contract: String) -> String
```

Emits the deterministic test and SSR flavor: a stub per procedure under the
same name the wire client exposes, running the real procedure synchronously.

## rpc

```vyrn
fn rpc(dir: String) -> String
```

Mounts every procedure module under `dir` at its derived path. Two procedures
deriving one path fail the build naming both.

## client

```vyrn
fn client(dir: String) -> String
```

The calling side of an api directory. It reads interfaces, re-emits types and
never imports an api module, so no procedure body reaches a client bundle.

## clientInProcess

```vyrn
fn clientInProcess(dir: String) -> String
```

`client(dir)` with direct dispatch. A separate generator, because a generator
cannot see its importer's audience; the stubs share names, so a composition
root swaps one import line.
