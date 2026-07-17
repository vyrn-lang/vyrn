# RFC-0031 — `moduleInterface`: The Reachable Type Closure

- **Status:** Implemented — see the as-landed notes at the end
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

## Implementation notes & decisions (as landed)

- **Where the closure lives:** `moduleInterface` now LINKS the reflected
  module (`loader::load` through the generation resolver) instead of parsing
  the single file, and `schema_reflect::module_interface_lit` computes the
  closure over the linked program: roots are the reflected module's own
  exported functions' param/return spellings (`module == None` — RFC-0010
  attribution distinguishes own from foreign decls), edges walk every type
  position (record fields, enum payloads, alias/validated bases, generic
  args, `Option`/`Result`/`Array`/`Map`/`Fn`/`Omit`/`Pick`/`Merge`/
  `Partial`/`ArrayN`/`Ref`/`Task`). Own decls are always included (today's
  behavior kept); foreign decls only when reached. The locked ORDER falls
  out of the linked program's decl layout (own decls first in source order,
  foreign after in linker order), so a self-contained module's interface is
  byte-identical to before — verified against the untouched rpc/pages
  emit-gen assertions.
- **Collisions:** two same-named distinct decls across linked modules were
  already a `load` error naming both files; reflection linking the module
  surfaces exactly that diagnostic at the generator import site, so the
  closure can never contain two `Book`s. (Tested.)
- **Cache soundness (the "verify" clause):** the reflection link runs
  through a `RecordingResolver` proxy; every module file the link reads
  joins the generator's recorded inputs, so a closure type edited in
  `types.vyrn` misses the cache while an unrelated edit still hits.
  (Tested: miss + fresh output vs. hit.)
- **Scoping note:** reflection reads the reflected module's import closure —
  exactly the file set an ordinary `import` of that module would read at
  load time. The generator's path-argument scoping still gates the ROOT of
  the reflection; the transitive module reads are loader-mediated, not
  filesystem-ambient.
- **`TypeInfo.module` (new field):** each closure entry carries the import
  specifier of its DECLARING module (own decls carry the generator's own
  argument spelling), computed by `loader::import_specifier` relative to the
  real importing file — `std/` and remote keys keep their specifier form.
  Generators that IMPORT contract types (rather than re-emit) need it:
  `rpcServer`/`rpcInProcess` now group their type imports by declaring
  module (`typeImportBlock`), and `std/ui`'s router imports a FOREIGN
  `Params` under a `uiParams<idx>` alias from its declaring module (a
  namespace reaches a module's own exports only). `rpcClient` re-emits
  `iface.types` verbatim and needed no change. Self-contained contracts
  emit byte-identical import lines.
- **A latent alias-machinery bug surfaced and fixed:** a co-naming rename
  (RFC-0022) rewrote method-sugar call names unconditionally, corrupting
  `ns.member(..)` (RFC-0027) into `ns.renamed` when a like-named decl was
  renamed — hit by the thin contract delegating `store.getItem(..)` while
  `rpcInProcess`'s generated module co-named `getItem`. The plain-name
  rewrites now skip a call whose receiver is a namespace binding (pass 5
  owns those references).
- **Proof:** `examples/shelf` split into `wire.vyrn` + `store.vyrn` + thin
  `contract.vyrn` (`import * as store`), browser-verified end to end;
  `examples/rpcsplit.vyrn` is the in-process/parity citizen of the same
  shape; a pages fixture imports `Params`/`Data` from a shared module.
