# RFC-0019 — Typed RPC as a Library (on Generator Imports)

- **Status:** Implemented — `std/rpc.vyrn` (three generators), `web/vyrn-rpc.js`
  + `web/vyrn-query.js` (the host runtimes), `vyrn dev`, and the
  `examples/fullstack/` demo. Covered by `examples/rpc.vyrn` (three-way parity)
  and `vyrn-cli/tests/rpc.rs`. Supersedes the withdrawn `rpc fn` keyword design
  (implemented, then **reverted** in 7032894 — a language-level crutch; see the
  revision note).
- **Depends on:** RFC-0018 (JSON codec), RFC-0021 (generator imports **+
  `moduleInterface` reflection** — added there for this), RFC-0016 (`serve`
  and its `handle` convention), RFC-0012/13 (extern + host-owns-the-loop)

> **Revision note.** The first design added `rpc fn`, an `rpc()` builtin,
> and client/server compilation roles. User review held that this bakes a
> domain into the language — the i18n mistake one layer deeper. It was
> right: the *only* thing the compiler must provide that a library cannot
> is **reading a module's typed interface**, and that is reflection (the
> `schemaOf` family generalized), not RPC knowledge. With that one general
> primitive, everything below is generated Vyrn source. No keyword. No
> builtin. No compilation roles. The language knows nothing about RPC.

---

## The contract is an ordinary module

```vyrn
// api.vyrn — ordinary Vyrn, zero annotations
export type GetUserReq = { id: Int64 }
export type User = { name: String, age: Age }

export fn getUser(req: GetUserReq) -> User {
    return dbFind(req.id)
}
```

Procedures are simply **the exported functions of the module you point a
generator at**. The generator enforces RFC-0018 codability of every
exported signature at generation time — a non-codable export is a
generation error naming the offender (keep helpers unexported or in
another module).

## The library: three generators over one contract

The three generators are **ordinary `gen fn`s in an ordinary module** —
shipped as `std/rpc` (written in Vyrn, in-repo), equally usable from a
fork or `github:` pin. They have zero capabilities a user-written
generator lacks; "official" means only "we wrote them". Anyone can write
a sibling (mocks, another transport, another codec) with the same
`moduleInterface` + source-emission tools.

```vyrn
// server.vyrn
import { rpcServer } from "std/rpc"       // the generators themselves — just a library
import { rpcHandle } from rpcServer("./api")
import { getUser } from "./api"           // direct calls stay fine on the server

fn handle(req: Request) -> Response {
    return match rpcHandle(req) {          // POST /rpc/<name> + GET /rpc/$schema
        Some(r) => r,
        None => pages(req),                // your own routes/fallback
    }
}
```

```vyrn
// client.vyrn
import { rpcClient } from "std/rpc"
import { getUser, GetUserReq, User } from rpcClient("./api")

fn load(id: Int64) {
    getUser(GetUserReq { id: id })         // the STUB — fires the wire call
}

fn onGetUser(id: Int64, res: Validation<User>) {
    match res { Valid(u) => ..., Invalid(issues) => ... }
}
```

```vyrn
// client tests / SSR: swap ONE import — same names, in-process dispatch
import { rpcInProcess } from "std/rpc"
import { getUser, GetUserReq, User } from rpcInProcess("./api")
```

- **`rpcServer(path)`** emits `rpcHandle(req: Request) -> Option<Response>`:
  matches `POST /rpc/<name>`; decodes via `fromJson` (`Invalid` ⇒ 422 with
  `{"issues":[...]}`); calls the real function (imported normally — this
  is the server, bodies exist); encodes ⇒ 200. `GET /rpc/$schema` ⇒ the
  procedure registry, built with `jsonSchema()` calls *in generated code*.
  Plugs into the existing `handle` convention — `serve` is untouched. The
  mount surface is exactly the module you pointed the generator at:
  explicit, no transitive surprises.
- **`rpcClient(path)`** emits: verbatim re-declarations of the contract's
  *types* (via `moduleInterface` source text — the client build never
  links server bodies, so there is nothing to strip: no roles, no
  linker-GC-secrets caveat); one **stub per procedure** (same
  name/parameter, returns `Unit` — fires the transport) over a tiny
  extern; and one **per-procedure dispatcher** (`export extern fn`) that
  decodes the completion with the right type and calls your plain
  `fn onGetUser(id: Int64, res: Validation<User>)`. A missing handler is
  an ordinary "unknown function" error inside the generated module,
  banner pointing at the generator call — `vyrn emit-gen` shows the
  exact expected signature. Result unification (locked): 200 ⇒ `Valid` /
  decode-failure `Invalid`; 422 ⇒ `Invalid` carrying the **server's own
  issues**; transport/HTTP failure ⇒
  `Invalid([Issue{key: "rpc.transport", path: "", message: <locked
  canonical wording>}])`. One handler shape covers everything.
- **`rpcInProcess(path)`** emits same-named stubs that call the real
  functions directly and invoke `onGetUser` synchronously before
  returning — the deterministic test/SSR flavor. Choosing it is one
  visible import line, not a compiler mode.

`web/vyrn-rpc.js` routes fetch completions by procedure name (it made the
request, so it knows the owner — **no shared client state needed
anywhere**); `web/vyrn-query.js` owns cache policy (dedupe, staleTime,
invalidate) as before.

## The fullstack build (what remains of it)

**No compilation roles.** `vyrn.json`'s `server`/`client` keys survive
purely as `vyrn dev` convenience (which root to serve, which to build to
wasm) — zero semantics. `vyrn build client.vyrn --target wasm` is a plain
wasm build; client and server differ only in *what they import*.

## What the language provides (all of it general)

`toJson`/`fromJson` (RFC-0018) · generator imports + `moduleInterface`
(RFC-0021) · `extern` (RFC-0012) · the `handle` convention (RFC-0016).
The protocol roadmap — Connect/gRPC, `.proto`/SDL emitters, JSON-RPC/MCP,
SSE — is now *more generators and more host adapters*; none of it will
ever touch the compiler.

## Out of scope

Streaming, auth/middleware (a generator-parameter story later), HMR,
GraphQL execution, HTTP/2 — plus everything the revision deleted (roles,
keyword, builtin), permanently.

## Implementation notes & decisions (as landed)

- **`std/rpc.vyrn`** is ~330 lines of pure Vyrn: three exported `gen fn`s
  (`rpcServer`/`rpcClient`/`rpcInProcess`) plus comptime-pure string-building
  helpers. Derived identifiers (`onGetUser`, `vyrnRpcDoneGetUser`) capitalize the
  procedure name via `bytes`/`stringFromBytes` (no dedicated case primitive
  exists). The dispatcher-naming convention — `vyrnRpcDone` + the procedure name
  with its first letter uppercased — is shared verbatim by the JS runtimes.
- **Generation-time contract enforcement.** Each generator validates the
  contract first: every exported procedure must take 0 or 1 parameters and, for
  1-param procedures, the parameter must be an exported named (codable) type
  (`fromJson` needs a declared type name). A violation is surfaced as a bare
  top-level identifier in the synthesized module — Vyrn has no message-carrying
  trap primitive, so this yields a single parse diagnostic at the generator call
  site (its banner) naming the offender, unmasked by any later import/link error.
  Deep codability is additionally enforced by the compiler when the emitted
  `toJson`/`fromJson`/`jsonSchema` calls are checked against the generated module.
- **Zero-param / Unit locks.** A zero-param procedure is `POST` with an
  empty/ignored body; a `Unit` return encodes as `204` with an empty body and, on
  the client, unifies to `Validation<Bool>` (`Valid(true)` on any 2xx).
- **Client result unification (locked).** 200 ⇒ `fromJson` of the body; 422 ⇒ the
  server's own issues, decoded from `{"issues":[...]}` via an injected
  `RpcIssues = { issues: Array<Issue> }` shape; any other/transport status ⇒ a
  single `Issue{key:"rpc.transport", path:"", message:"procedure \`X\` failed
  with status <n>"}`, with **status 0 = "procedure \`X\` is unreachable"** (a
  network failure, reported by the JS transport).
- **In-process request ids = 0 (locked).** Module state is root-only and a
  generated module is never the root, so an in-process generator cannot allocate
  ids without smuggling in a global. v1 lock: **in-process dispatch does not
  allocate request ids** — every `on<Proc>` fires with `id = 0`. Real wire ids
  come from the JS runtime (a per-transport counter). Validated types also cannot
  be constructed invalid in-process, so the in-process flavor only ever produces
  `Valid` — the `Invalid` paths are a wire concern.
- **In-process naming deviation (honest).** Vyrn links every module into one flat
  namespace, so a stub cannot share a name with the real function it calls. The
  in-process dispatchers are therefore named `call<Proc>` (not the wire client's
  `<proc>`), and the contract TYPES are imported from the contract directly. A
  faithful same-named in-process flavor would need a general import-aliasing
  primitive; deferred.
- **`vyrn dev` static precedence (locked).** A GET whose path names an existing
  static asset (the built `/client.wasm`, the runtimes under `/vyrn-runtime/*`,
  or a file under the public dir with `/` → `index.html`) is served from disk;
  everything else — every POST, and every non-file GET, so all of `/rpc/*` —
  goes to the server root's `handle`. Static-first for exact file matches, then
  `handle`.
- **Frontend fixes the generators flushed out** (all general — no RPC knowledge
  entered the compiler): a generated module's relative/bare imports resolve
  against the real importer encoded in its banner key; a generated module may
  call names owned by its importer (the dispatcher→`onGetUser` callback); and the
  generator cache key now includes the generator name (one module, several
  `gen fn`s, same args no longer collide). See `compiler/vyrn-frontend/src/loader.rs`.
