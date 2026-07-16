# RFC-0019 — Typed RPC as a Library (on Generator Imports)

- **Status:** Draft, redesigned — approved. Supersedes the withdrawn
  `rpc fn` keyword design (implemented, then **reverted** in 7032894 — a
  language-level crutch; see the revision note).
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
