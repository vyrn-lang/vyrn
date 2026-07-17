# RFC-0031 — `moduleInterface`: The Reachable Type Closure

- **Status:** Draft (design locked)
- **Depends on:** RFC-0021 (`moduleInterface` — the reflection primitive
  this completes), RFC-0019 (`std/rpc` — the consumer whose generated
  clients need it), RFC-0029 (module state everywhere — which made the
  contract/store coupling visible)
- **Evidence (shelf dogfood + RFC-0029 as-landed):** the contract must
  currently BE the store. `rpcClient` re-emits the contract's type
  declarations verbatim and `moduleInterface` reflects only a module's
  OWN declarations, so every wire type must be declared in the contract;
  a separate store module owning `Array<Book>` needs `Book` from the
  contract while the contract needs the store's accessors — an import
  cycle. The workaround ("contract = store") is defensible but forced;
  reflection failing to see through imports is the actual limitation.

---

## The change

`moduleInterface(path)` returns, in `types`, the **reachable type
closure** of the module's exported surface: every named type declaration
transitively referenced by the exported functions' parameter and return
spellings AND by those types' own definitions (record fields, enum
payloads, alias targets, validated-type bases, generic arguments) —
**regardless of which module declares it**. Each entry keeps its defining
declaration's source text, exactly as own-module types do today.

```vyrn
// types.vyrn
export type Book = { id: Int64, title: Title, tags: Array<String>, rating: Rating }

// store.vyrn
import { Book } from "./types"
let mut books: Array<Book> = seed()
export fn allBooks() -> Array<Book> { return books }

// contract.vyrn — thin, stateless, and honest again
import { Book } from "./types"
import * as store from "./store"
export fn listBooks(req: ListReq) -> Array<Book> { return store.allBooks() }
```

`moduleInterface("./contract")` now includes `Book`, `Title`, `Rating`,
`ListReq` — so `rpcClient`'s verbatim re-emission works unchanged, and
the cycle dissolves: types flow down from a leaf module, state lives in
the store, the contract is the boundary layer it was always meant to be.

## Semantics (locked)

- **Closure root:** the exported functions' signatures (the procedure
  surface). Types exported by the module but unreachable from any
  exported signature are still included **if declared in the module
  itself** (today's behavior, kept), but imported-and-unreferenced types
  are NOT dragged in — the closure is about what the interface needs.
- **Dedup and order:** one entry per declaration; deterministic order —
  own declarations first (source order, today's order preserved), then
  foreign closure entries in linker order of their modules, source order
  within a module. (Deterministic order matters: generator output is
  content-addressed.)
- **Name collisions are honest errors:** if the closure would contain two
  DISTINCT declarations with the same name (two `Book`s from different
  modules), `moduleInterface` fails generation with a load diagnostic
  naming both declaring modules. No silent renaming — a wire format with
  two `Book`s has no honest JSON spelling.
- **Source fidelity:** each `TypeInfo.source` is the defining module's
  declaration text (unchanged rule). Re-emission by generators therefore
  reproduces validated-type predicates, `///` docs, and finite domains
  byte-exactly, wherever the type was declared.
- **`ParamInfo`/`retSchema` schemas:** unchanged — schemas were already
  computed against the linked program and never had the visibility bug.
- **Protocols/impls:** out of scope — wire types are records, enums,
  aliases, and validated types; protocol machinery is not reflected.

## What must NOT change

`moduleInterface` of a module with no foreign references is byte-for-byte
today's result (order rule above preserves it), so every existing
generator cache key and emit-gen golden for self-contained contracts
stays valid. The gen cache key already folds in resolved input roots
(the dogfood P0 fix); closure entries come from the same linked source
set, so caching stays sound — but VERIFY: if a foreign type's defining
FILE is not already among the generator's recorded inputs, its content
must join the cache key (a closure type edited in types.vyrn must miss
the cache).

## Consumers (the proof)

- `std/rpc`: no generator changes expected — `rpcClient`/`rpcServer`
  iterate `iface.types` and re-emit; the closure just appears. Verify
  `rpcInProcess` likewise.
- **shelf refactor as evidence:** split `contract.vyrn` into
  `types.vyrn` + `store.vyrn` (owns `books`) + a thin `contract.vyrn`,
  per the example above; browser-verify the app end to end; the
  RFC-0029 as-landed "contract must BE the store" note gets a
  "superseded by RFC-0031" pointer.
- `std/ui` pages: `Params`/`Data` may now be imported types too — test
  one page doing so.

## Out of scope

Explicit re-export surface (`export { X } from "./m"` — add only if a
facade use case demands it; the closure makes it unnecessary for wire
types), reflecting functions of imported modules, protocol reflection,
cross-package (remote) closure policy changes (remote modules already
vendor their sources; the closure follows the same linked set).
