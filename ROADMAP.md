# Roadmap

Open work, one line each, with what blocks it. [docs/decisions.md](docs/decisions.md) lists the roads not taken.

## Compiler

- Bring the compiler toward 45k lines (about 179k with std; `checker.rs` is 11k). Blocked by the checker's typing rules that `vyrn-lower`'s typed judgment does not state.
- Move the map value paths (`map_set`, `map_tally`, `map_at`) from the emitter into `std/runtime`. Blocked by the call cost: needs a wasm inliner before emit, or a wasmtime that inlines.
- Make `push` one runtime call per element instead of two. Blocked by the same missing inliner.
- Delete the free audit. Blocked by a second oracle: the kernel corpus must catch what the audit catches.
- Remove `own.rs`'s install hooks. Blocked by crate order: `vyrn-lower` sits above `vyrn-frontend`.
- State the WASI host once (`wasmrun.rs` and `wasi_host.c`). Blocked by the native route: only a Cranelift `build` would remove the C host.
- Vectorize array read loops under `wasm2c`. Blocked by 32-bit wasm addresses: clang cannot prove contiguity. No fix designed.
- Build each generic once and instantiate it on the core. Blocked by a probe that prices it.
- Move the storage desugar out of the parser. Blocked by a design for the codec's call site, which takes `toJson` and `fromJson` with it.
- Give `toJson` a `Show` bound. Blocked by overlap: a user `impl Show` would collide with a seeded scalar impl.
- Gate native build output bytes and LSP latency in CI. Blocked by an owner.

## Memory

- Close the known leak shapes: a projection read into a local and handed on later, a return out of a `region`, a hole under an enum payload or inside an `impl Owned` release, a `for` over an owned container left by `return`. Blocked by alias analysis for the first; a kernel slice each for the rest.
- A must-use container read (`pool.length`, `pool[i]`) counts as a disposal. Blocked by position and capability data in the must-use scan.
- The wasm allocator never coalesces, splits or shrinks memory. Blocked by a measurement that shows it matters.
- Conditional places (a projection returning an `if` of places). Blocked by a container in the corpus that needs one.

## Language

- Field and variant docs in reflection (`FieldInfo.doc`). Blocked by a parser change: `ast::Field` has no doc. It would remove the source re-scans in `std/http`, `std/graphql` and `std/cli`.
- `FromElements` and `FromEntries`, so `[]` and `[:]` build user containers. Blocked by a receiver-less protocol method dispatched by expected type.
- A `Codable` protocol in place of the codec's hand-written verdicts. Designed, not started.
- Payload enums as `Map` keys, and a wire form for record-keyed maps. Blocked by a program that needs them.
- Call on an expression (`r.f()` on a field value). Blocked by `Expr::Call`, which carries a name.
- `any P` protocol values, mutable captures, capture by move. Blocked by a consumer.
- A call-shaped assignment target (`x.f(i) = v`). Blocked by a program that uses the `setF` form often enough to hurt.
- A dedicated `Range` type for stored views. Blocked by a program that holds (source, start, length) triples often enough to hurt.
- Ordered `insert`, `remove` and `truncate` on `Array`; `values()`, `entries()` and `for (k, v)` on `Map`. The last needs tuples.

## Standard library and web

- VON as the manifest format, VON imports with `where` checks, and a run-time `fromVon`. The last is blocked by `lex()`, which exists only during generation.
- `std/cli` subcommands and shell completions. Blocked by variant docs in reflection.
- `std/term` colour and environment options. Blocked by an environment-variable builtin and a decision on ambient authority.
- `std/cli` raw mode and a TUI. Blocked by a trap hook, because Vyrn does not unwind.
- A character-position API for columns and padding in `std/scan`, `std/strings` and `std/vyx`, which count bytes. Not designed.
- `std/regex` anchors, lazy repeats, `{m,n}` and captures. Each is blocked by a program that needs it.
- GraphQL: select before encoding so `lazy` fields pay off. Designed, not built. Introspection and variables are not designed.
- `std/openapi` should send read-only procedures as GET. Blocked by a wire design for the query string.
- Bidirectional WebSocket. Blocked by a per-message handler design.
- A `Layout` contract, so `vyxParseHead` can go. Blocked by a design.
- Compiled reactivity (dirty bits instead of render and diff). Blocked by a profile that shows the diff is the cost.
- Shared components inside `.vyx` pages; `.vyx` pages in the `pages()` router. Blocked by a components-directory convention and a page call convention.
- Column numbers in `.vyx` diagnostics. Blocked by `VNElem`, which carries no column.
- Host runtime gaps for server programs: fetch, child processes, environment variables, timers, TLS, SQL, cookies. Undecided which belong in the language.

## Tooling

- LSP find-all-references. Not built.
- `vyrn fmt` width-driven reflow. Deferred: a formatter that keeps the author's lines is predictable.
- Time the compiler front end in CI. Blocked by a harness: a `bench` block cannot time Rust that runs before it.
