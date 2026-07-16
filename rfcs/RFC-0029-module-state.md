# RFC-0029 — Module State Everywhere: Lifting the Root-Only Rule

- **Status:** Draft (design locked)
- **Depends on:** RFC-0013 (module state — the root-only rule this lifts;
  everything else there stands), RFC-0021 (the synthesized-module state
  carve-out this generalizes and deletes), RFC-0025 (the `--workers` gate
  and spawn isolation — the analyses that make this safe)
- **Evidence (the shelf dogfood, NOTES-dogfood-shelf P1):** RPC procedure
  bodies live in the contract module and page loaders live in page
  modules — none may touch state, so shelf hand-wrote its entire server
  dispatch, shipped dead placeholder contract bodies, and pre-generated
  i18n (forfeiting the locale carve-out). The stack's own architecture
  routes logic through non-root modules; the root-only rule fights the
  module system. Meanwhile RFC-0021's carve-out means SYNTHESIZED modules
  already own state (i18n's `currentLocale`) — generated code gets a deal
  users don't, exactly the asymmetry this project keeps killing.

---

## The change

Top-level `let` / `let mut` is legal in **any** module. The RFC-0021
synthesized-module carve-out is deleted — it becomes the general rule.

```vyrn
// store.vyrn — a store module: server-side Pinia, nothing special
let mut books: Array<Book> = seedBooks()

export fn allBooks() -> Array<Book> { return books }
export fn addBook(b: Book) { books.push(b) }
```

## Semantics (locked)

- **One instance per process, per module.** Module identity is the
  loader's resolved identity (path / generator cache key) — diamond
  imports share the one instance, exactly as they share the module.
- **State is module-private, always.** `export let` is rejected with a
  named diagnostic ("module state is not exportable — export accessor
  functions"). Cross-module access goes through the owning module's
  exported functions, full stop. Every piece of state keeps exactly one
  named owner and a visible API — the discipline root-only bought, kept
  without the location restriction.
- **Initialization order:** all modules' state initializes before `main`
  (or the host's first exported call), in **linker order** — post-order
  over the import graph (dependencies first), top-to-bottom within a
  module. An initializer may therefore call imported functions and observe
  imported modules' state, which is already initialized; it can name
  nothing else (you can't reference what you don't import). Imports are
  acyclic, so the order is total and deterministic. When only the root has
  state this is byte-identical to today's behavior.
- **Lifetime:** unchanged from RFC-0013 — state lives for the process;
  reclamation at exit follows the existing root-state rules.
- **Isolation analyses: no changes needed, verify and pin.** Spawn safety
  and the `serve --workers` gate key on `touches_globals` reachability,
  which is already module-agnostic; a handler chain reaching ANY module's
  state gates workers, with the refusal naming the owning module in the
  chain it already prints. The event-loop/export rules (RFC-0013) are
  unchanged: exports may mutate any state they can reach through imports.
- **Backends:** after linking, module state is already program-level
  globals in every backend; per-worker interpreter copies re-init ALL
  globals (they do today). The expectation is that this RFC deletes a
  checker restriction and adds ordering guarantees — if implementation
  finds a backend that genuinely special-cases root state, that is a
  finding to report, not to patch around silently.

## What this unlocks (the follow-up wave, same milestone)

- **Stateful contracts:** `rpcServer` procedure bodies may hold/reach
  state — shelf's hand-written dispatch is deleted; the contract's bodies
  become the real implementations backed by a store module.
- **Stateful page loaders:** `load(p)` reads the store directly.
- **i18n composes everywhere:** shelf's pre-generated `strings_gen` is
  deleted in favor of `i18n("./locales")` imported where needed —
  including from `.vyx` script sections (the components module is just a
  module) — and `setLocale` returns.
- The old `export let` request is answered permanently: no — accessors.

## Out of scope

Multiple instances / scoped state (one per process stays), lazy or
on-demand init, cross-module init cycles (imports are acyclic), any
change to spawn semantics or the workers gate beyond verification,
thread-shared mutable state (workers still require state-free `handle`).
