# Vela — status & roadmap

The forward-looking companion to the [RFCs](rfcs/). What ships today, what's next,
and the one decision the rest of the language waits on.

**Every feature below is verified three ways**: the clang-compiled native
binary AND the `wasm32-wasi` module produce byte-identical stdout, stderr, and
exit codes against the tree-walking interpreter (the reference semantics),
across **37 examples** and **418 tests** (0 warnings) — including every runtime
trap path (one canonical `error: ...` wording on stderr, exit 1, everywhere).
The permanent corpus harness is
`cargo test -p vela-cli --test parity -- --ignored` (needs clang; the wasm
column runs when `tools/` holds a wasi-sysroot + wasmtime, or via
`$WASI_SYSROOT`/`$VELA_WASMTIME`; the known-divergent list is empty and must
stay that way).

**WebAssembly**: `velac build prog.vela --target wasm` compiles the same
LLVM IR against wasi-libc (`--target=wasm32-wasip1`). The runtime is
libc-portable: stream handles and all size_t-sensitive calls (`strlen`,
`malloc`, `realloc`, `strncmp`, `snprintf`) route through a tiny embedded C
shim with 64-bit-clean prototypes, and the C `main` lives in the shim (the IR
exports `vela_entry`), so MSVC, glibc, and wasi-libc all link the same module.
Exit codes are portable in 0..126 (WASI's constraint). **The browser demo
ships** (`web/`): a hand-rolled ~100-line WASI preview1 shim (`wasi-min.js`,
zero dependencies) runs any `velac`-built module in a page — a velac module
imports exactly five preview1 functions (`fd_write`, `fd_fdstat_get`,
`fd_close`, `fd_seek`, `proc_exit`), stdout/stderr stream into the page, and
trap parity holds end-to-end (division by zero prints the canonical
`error: division by zero` + exit 1 in the browser, byte-identical to interp
and native). `web/build.ps1` builds the example modules; any static server
serves it. Next steps on the browser path: a JS interop layer (`extern`
imports/exports + string glue), then an event-loop story — the long-range
goal is same-language server (native SSR) + client (wasm), with validated
types as the wire contract.

A 2026-07-15 hardening pass fixed ~40 reviewed defects: native
use-after-free/heap-corruption bugs (cell `set`, region escapes, `list` in
regions), invalid-IR shapes (dead `ret`, `phi void`, unpooled predicate
strings), interp/native numeric divergences (wrapping overflow semantics,
sized-int operand truncation, Float64 refinements, NaN, division traps),
validated-type soundness holes (nominal predicated records, match-arm
laundering, `modify` width subtyping, generic capability checks, spawn purity
through protocols, movecheck gaps), and a lexer/parser diagnostics batch.
A follow-up closed the rest of that review's deferred list: the `?`
propagate path now frees in-scope owned temporaries exactly like `return`
(was leaking every owned local on the early exit); region nesting past the
arena stack's 64 slots now traps (`error: region nesting exceeds 64`) in
both backends instead of corrupting memory past the fixed `[64 x ptr]`
global — and the checker's return-path analysis learned that a `region`
body that always returns satisfies the enclosing function (it runs exactly
once, unlike a loop). Allocation failure traps (`error: out of memory`)
instead of dereferencing null — the C shim's `__vela_malloc`/`__vela_realloc`
check once at the choke point, including the ILP32 guard against a 64-bit
size silently truncating in the `(size_t)` cast on wasm32. `schemaOf(T)`
was enriched: `Schema` now carries `name`, the full base spelling (sized
ints included), the `///` `doc`, `multipleOf`, `minLength`/`maxLength`,
and the regex `pattern` — enough to assemble real OpenAPI fragments in
ordinary Vela code (see `examples/reflection.vela`). Along the way, a
`///` block separated from a declaration by a blank line (a file header)
no longer glues onto that declaration's doc.

**Modules (RFC-0010)**: `import { names } from "./path"` / `export fn|type|protocol`
— TS-style, resolved relative to the importing file (`.vela` appended), with
`std/...` reserved for the standard library (itself written in Vela: `std/math`,
`std/strings` — parity for free). A loader/linker stage parses each file once,
enforces exports/visibility (a module only sees foreign names it imported;
importing an enum brings its variants, a protocol its methods), rejects cycles
and cross-module name collisions, then links everything into ONE Program — the
checker, interpreter, both code generators, and the parity harness are unaware
modules exist. I/O lives behind a `ModuleResolver` trait (filesystem in the
CLI, in-memory maps in tests). **JSON Schema type imports** (M2):
`import type { User } from "./api.schema.json"` synthesizes validated types
from a schema document — the exact inverse of `jsonSchema(T)` (bounds/lengths/
patterns become `where` clauses, `required` steers `Option<T>`, `$defs`,
`#/$defs/..` and root `#` refs resolve, `enum`-of-strings becomes a
payload-less Vela enum, constrained fields become synthetic `User.age`
types), byte-exact round-trip with the emitter, and any inexpressible keyword
is a hard error. The emitter side is correspondingly rich: named nested types
render as `$ref`s into a `$defs` section (recursion is a real `$ref` — `"#"`
for the root — not a lossy comment), sized ints carry their width bounds as
part of the wire contract, and payload-less enums emit `enum` arrays. **Project manifest** (M3): an optional `vela.json`
(`name`/`main`/`dependencies`) found by walking up from the cwd — `velac run/
check/build` need no file argument in a project, bare import specifiers
(`import { x } from "money"`) resolve through the `dependencies` map (an
import map; targets are relative-to-manifest or `std/` for now), and `velac
new <name>` scaffolds a runnable project, `velac deps` prints the resolved
module graph. Bare `velac run file.vela` stays manifest-free forever.
**Reproducible remote imports** (M4): `github:owner/repo@ref/path`,
`gist:user/id[@rev]/file`, and `https://...` specifiers (inline or as manifest
targets). The first resolve pins each dep in `vela.lock`
(`specifier ⇥ immutable-url ⇥ sha256`, floating refs frozen to a commit via
`git ls-remote`); content lives in the content-addressed
`~/.vela/cache/sha256/` and is hash-verified on EVERY load (tampering fails
loudly). `--offline`/`VELA_OFFLINE=1` builds never touch the network;
`velac add <spec> [--name alias]` fetches+pins+records, `velac update [alias]`
is the only way a pin changes, and `velac vendor [--check]` copies the lock's
blobs into `./vela_vendor/` — a committed checkout builds forever even if the
upstream is deleted (any copy of a file with the locked hash restores it).
Remote modules are sandboxed: relative imports stay inside their pinned base,
no local paths, no bare specifiers. Zero new crates (hand-rolled SHA-256 with
NIST vectors; `curl`/`git ls-remote` subprocesses, all in vela-cli).

---

## Shipped

### Language core
- `Int64` / `Bool`, `let`/`mut`, arithmetic, `if`/`else`, `while`, `for`-in over
  arrays, functions, `print`.
- Immutable string literals (`==`, `!=`, record fields), statically allocated.
  Concatenate two Strings with `a + b`; a String's byte length is the `s.length`
  field. (The old `concat(a, b)` / `len(s)` free-functions were removed — a
  user-written call to either reports a migration hint.)
- **String encoding.** A `String` is an immutable sequence of **UTF-8 bytes**.
  `s.length` counts **bytes**, not code points — equal to the code-point count
  for ASCII text, larger for text with multi-byte characters (e.g. `"é"` has
  `length` 2, `"日"` has `length` 3). The regex engine matches
  byte-wise for the same reason, and `.` matches one byte. Source files are read
  as UTF-8. One documented divergence follows from this: JSON-Schema
  `minLength`/`maxLength` bounds derived from a `String where` predicate carry
  the byte count through unchanged, so for non-ASCII text they bound bytes where
  a JSON validator counts UTF-16 code units — a deliberate, noted trade-off (a
  code-point index would cost an O(n) scan on every `.length`).

### Types
- **Validated types** — `type Age = Int64 where value >= 18`, or inline on a
  record field, Zod/ArkType style: `type User = { age: Int64 where value >= 18 }`
  (desugars to a synthetic `User.age` validated type; the trailing record-level
  `where` stays the cross-field invariant, like Zod's object `.refine`) — with
  **automatic, exhaustive validation**: every value boundary (`let` annotation, assignment,
  call argument, return, record field, array element) checks a plain value
  flowing into a validated type by itself — no explicit constructor call
  needed. Provably-false constants are compile errors; provably-true ones cost
  nothing; dynamic values trap at runtime (`error: validation failed for
  \`T\``, both backends byte-identical). Field mutation on validated data is
  rejected — rebuild the value, which re-validates. Explicit `Age(n)` and
  fallible `Age?(n) -> Option<Age>` remain.
- **Nominal types** over `Int64`/`Bool`/`String` (a nominal type *without* a
  predicate still requires explicit construction — it is documentation).
- Every numeric type names its size: `Int8`–`Int64`, `UInt8`–`UInt64`,
  `Float32`/`Float64`. There is no unsized `Int`/`Float`.
- **Structural records** with width subtyping and mutable fields (`c.x = ...`).
- **Transformers** — `Omit` / `Pick` / `Merge` / `Partial` / `Readonly`, plus
  intersection `A & B`. Pure type-level, erased before codegen.
- **Enums / sum types** with multi-payload variants and exhaustive `match`.
- **Generics** — functions, records, enums — inferred per use and monomorphized,
  with built-in bounds `Eq` / `Ord` / `Num`.

### Errors & control
- `Option<T>`, `Result<T, E>`, `match`, and `?` propagation (no null). `Option` and
  `Result` payloads may be any type, so `Option<Ref<Node>>` gives a nil terminator.
- **Checked conversions** — `x.toString() -> String` (a method on every number,
  `Bool`, and `String`; it replaced the `str(x)` builtin) and
  `parse(String) -> Option<Int64>` (the fallible inverse is an explicit `None`,
  never a silent 0 or a crash).

### Data structures
- **Arrays** — growable `Array<T>` (a `Vec`: `[]` / `a.push(x)` / `a[i]` read /
  `a[i] = v` in-place store / `a.pop()` → `Option<T>` / `a.swapRemove(i)` → `T`
  (O(1) unordered remove) / `a.length`, a doubling heap buffer, bounds-checked;
  non-escaping arrays reclaimed automatically, `drop a;` for handoff) and
  fixed-size **`Array<T, N>`** (a const generic: stack `[N x T]`, no heap,
  array-literal `[a, b, c]` syntax; element store allowed, `pop`/`swapRemove`
  rejected — it cannot shrink). The element store and shrinking ops are RFC-0011;
  a validated element type auto-validates on store. An
  array literal written where an `Array<T>` is expected (a `let` annotation, a
  call argument, a return) is that growable heap array directly — contextual,
  replacing the old `list([..])` builtin. Both iterate with `for x in arr { .. }`.
  The surface is subject-first — no `verb(object, …)` builtins.
- **Recursive heap structures** — a singly-linked list and a binary tree. `Ref<T>`
  makes the node type finite, `Option<Ref>` terminates it, and a recursive `release`
  walk reclaims the whole structure (proven: 100,000 nodes cycled through a
  65536-cell slab). Both build/traverse/reclaim end to end, to a flat memory
  baseline — the `Option` payload is two words wide, so a `Ref` is stored inline
  with no heap box.

#### ECS notes — what a Structure-of-Arrays ECS can do today

`examples/ecs.vela` is a working SoA entity-component-system toy (parallel
`Array<Int64>` component stores, a movement system, spawn/despawn churn, a
deterministic checksum) verified interp == native == wasm. Writing it mapped out
exactly where the language helps and where it doesn't:

**Efficient today**
- **Contiguous scalar SoA stores.** A growable `Array<T>` lowers to
  `{ ptr, len, cap }` over a single realloc'd buffer, `a[i]` is a
  `getelementptr` + `load` at an element stride, and `a[i] = v` the matching
  `store` — genuinely cache-friendly. A system that streams over one component
  array per tick, reading and writing in place, is doing tight, linear memory
  access. Both halves of an ECS (iterate AND mutate) are good.
- **In-place update + O(1) despawn (RFC-0011).** The movement system integrates
  each survivor with an element store (`xs[i] = nx`) and despawns departing
  entities with `swapRemove` across all four SoA arrays in lockstep — no
  per-tick rebuild, no fresh allocation. This is the alive-compaction a real
  ECS does, expressed directly.
- **Cheap append-spawn.** `a.push(x)` is amortised O(1) with geometric growth.
- **Deterministic, reclaimed.** Arrays auto-drop when they don't escape; the whole
  world tears down without a GC, identically on every backend.

**Remaining gaps**
- **`Array<Record>` (AoS) is copy-only.** Indexing an `Array<Record>` returns a
  *copy*, and `a[i].field = v` write-through is still out of scope (RFC-0011
  "Out of scope"), so you cannot mutate a component struct in place through the
  array. This is why the example is SoA, not one `Array<Entity>`: the SoA scalar
  element stores *are* the write path. An AoS ECS needs place-expression field
  assignment through an index on top of what exists.
- **No order-preserving `insert`/`remove`/`truncate`/`clear`.** The shrinking
  surface is `pop` (last) and `swapRemove` (O(1), unordered); an ordered removal
  or a bulk shrink still means an explicit shift loop. Additive, future work.
- **No generational entity handles yet at the array level.** `Ref<T>` gives
  generation-checked cells, but there's no built-in dense/sparse-set or
  generational-index allocator; entity ids here are plain array positions, and
  `swapRemove` reorders survivors (fine for this toy, not for stable cross-frame
  handles).

**Verdict.** Both the storage model and the mutation model are now ready for an
efficient in-place SoA ECS: contiguous component arrays, per-element writes, and
O(1) despawn, all reallocation-free per tick. The open items (AoS field
write-through, ordered `insert`/`remove`, generational handles) are additive and
none blocks the core loop.

### Memory (RFC-0004)
- `consume` capability + move checking (using a consumed value is a compile error).
- `modify` capability — a parameter changed in place, visible to the caller
  (by-reference / call-by-value-result; the argument must be a `mut` variable).

### Concurrency (RFC-0004 §Q4)
- **Structured fork-join** — `spawn f(args) -> Task<T>` / `t.join()`. The compiler
  *proves* a spawned function is isolated (no I/O, no shared mutable state,
  transitively), so tasks are data-race-free and the result is schedule-independent
  — which is what keeps interpreter == native. `share` is the concurrent-read
  capability. (Execution is eager/sequential today; a parallel scheduler is a
  drop-in backend optimisation the model already guarantees is safe.)
- **The heap** — dynamic strings (`a + b` concatenation, `s.length`), malloc-backed.
- **Deterministic reclamation, Path A (no GC):**
  - `region { .. }` arenas free a whole *group* of allocations at block exit.
  - **ownership auto-drop** frees an *individual* heap value proven not to escape
    its block — a string, a reference cell, or a growable array (including the
    `a = push(a, x)` self-update), all via one escape analysis.
  - **ownership transfer** lets a function return a fresh value the *caller* then
    owns and frees (inferred by a call-graph fixpoint).
  - Measured flat (~3 MB) where the same million-allocation loop leaks 1.2 GB.
    Every allocation is owned by exactly one mechanism, so nothing is freed twice;
    what can't be proven single-owned leaks (always safe).
- **Generational references (Path B)** — a `Ref<T>` is a freely-copyable handle to
  a mutable heap cell holding any `T` (`cell` / `get` / `set` / `release`; the
  payload is boxed, so the handle is fixed-size). Each access is generation-checked,
  so a use-after-release traps instead of dangling — even after the slot is reused.
  The answer to the *aliasing* case. A record may hold a `Ref` to its own type
  without becoming infinite.
- **Inferred `release`** — the *same* ownership analysis that frees non-escaping
  strings auto-releases non-escaping cells, so Path A and Path B are one
  mechanism. Aggressive reclamation is safe here precisely because a missed alias
  traps cleanly instead of dangling.

### Backend
- Text LLVM-IR backend; `velac build prog.vela` emits IR and links a native exe
  with `clang`. (The Inkwell in-memory backend also works now — builds against an
  LLVM 22 dev SDK and links a `fib` exe whose exit code matches the interpreter —
  but stays excluded from the default workspace and covers only the v0.1 subset;
  the text-IR path remains the full reference backend.)

### Tooling
- **Structured diagnostics as a core API** — `vela_frontend::diagnostics(source)`
  returns every problem as a `Diagnostic { line, col, end_col, severity, stage,
  message }` with a precise position. Both `velac check` (prints
  `file:line:col: message`) and the LSP consume the same API; no duplication.
  Accumulation is bounded: lexer/parser stop at the first error, but once a file
  parses, every type/ownership error across all functions and types is reported.
- **Symbol query as a core API** — `vela_frontend::analyze(source)` runs the
  pipeline (lex→parse→check→movecheck) once and returns an `Analysis {
  diagnostics, symbols, tokens }`: the diagnostics, a `Symbol` per top-level
  function/type/variant/method with a precise name column (reused from the
  lexer's `Token.col` — the AST carries line only), and the identifier tokens.
  `resolve(analysis, line, col)` maps a cursor to its declaration; `
  completions(analysis)` lists top-level symbols. Non-invasive: no AST/parser
  span threading. `diagnostics()` delegates to `analyze()`, so one pipeline.
- **`vela-lsp`** — a synchronous `lsp-server` LSP server (no async runtime) and a
  pure adapter: it calls `analyze_linked` once on open/change, caches the
  `Analysis`, and serves `textDocument/publishDiagnostics`, `/hover`,
  `/definition`, and `/completion` from it (a request never re-parses). Excluded
  from the default workspace (pulls `lsp-server`/`lsp-types`); built with
  `cargo build --manifest-path compiler/vela-lsp/Cargo.toml`. The only compiler
  calls are `vela_frontend::analyze_linked` + the query layer, so the editor and
  CLI report identical errors. **Multi-file aware** (RFC-0010): the server
  resolves a document's `import`s through the module loader — local files from
  disk, `std/` via the same discovery as `velac`, manifest aliases from
  `vela.json`, and *pinned* remote modules read-only from `vela_vendor/` or the
  user cache (the editor never fetches; unpinned remotes get a "run `velac
  check` once" diagnostic). Errors inside an imported file surface in the open
  document as `in <file>: …` at the top. Hover/go-to-definition/completion cover
  top-level functions, types, and variants of the open document, plus local
  bindings (params, `let`s, `for`-in vars) — a local shadows a same-named
  top-level symbol; local hover shows the declared type for params and annotated
  lets. **Cross-file hover/go-to-definition**: names the root imports are
  indexed from the linked program with their source file — hover shows the
  imported signature, F12 jumps into the imported module (declaration line;
  columns are whole-line, the foreign token stream isn't indexed). An imported
  enum brings its variants, an imported protocol its methods. Remote modules
  (`github:...`) get hover but no jump (no local file). Imported names appear
  in completions.
- **VS Code extension** (`editor/vscode/`) — plain-JavaScript (no compile step)
  extension that spawns `vela-lsp` and ships a TextMate grammar for colors. `F5`
  from the repo root runs it against `examples/`: colored, squiggled, with hover
  / F12 go-to-definition / completion.

---

## The memory model — decided (RFC-0004 §5)

The founding notes said to settle the memory model by *prototyping and measuring*,
not by argument. Both lowerings were built behind the same capability surface and
measured — and the decision is now made: **a hybrid that defaults to ownership.**
Ownership + regions handle single-owner values with zero per-access overhead and no
annotations; generational references handle the *aliasing* case, where the check
proved essentially free in a hot loop (within noise in steady state; ~10 % cold, on
a loop doing nothing but access). You reach for `Ref<T>` exactly when you need
shared mutable state — which is also where the type makes that choice legible.

Both prototypes:

- **Path A — ownership + regions.** ✅ Reclaims owned `String`s — regions,
  ownership auto-drop, and ownership transfer. Measured flat vs. a 1.2 GB leak.
- **Path B — generational references.** ✅ Prototyped. A freely-copyable `Ref<T>`
  (over any element type) carries a generation tag; the cell carries a counter;
  each access validates the tag, so a stale alias fails a cheap check instead of
  dangling. This is what makes the *aliasing* case safe.

**The two paths share one analysis.** `release` is inferred: the same escape
analysis that frees non-escaping strings auto-releases non-escaping cells. So the
capability surface stays uniform — you write neither `free` nor `release` in
ordinary code — and reclaiming aggressively is safe on Path B because a missed
alias traps cleanly rather than dangling.

The decision is recorded in RFC-0004 §5. What's left is *surface refinement*, not a
change of mechanism: inferred/invisible regions, `modify`/`share` reference
inference, and concurrency.

---

## Next / gated

Each needs dynamic allocation or references; the heap unblocks them, but most wait
on the reclamation decision above.

- **Parallel execution of tasks** — the concurrency *model* and its safety ship
  today (eager/sequential scheduler); running tasks on real threads is a portable
  threading runtime — runtime work, not language design, and it changes no answers.
- **`share`-by-reference** — pass large shared data without copying (an
  optimisation; observably identical to today's by-value `share`).
- **More conversions** — `parse` for other types; formatting helpers.

### Editor (deferred from the LSP work)
- **Parser error recovery** → multiple *parse* errors per pass. **Top-level
  recovery ships**: `parse_accum` records a bad `fn`/`type`/`protocol`/`impl`/
  `logging` declaration, synchronizes to the next top-level starter (brace-depth
  aware), and continues — so one bad declaration no longer hides a later one
  (`velac check` and the LSP now report each). What stays first-error is recovery
  *within* a declaration (two errors in one body still report the first) — the
  same statement/declaration boundary the checker and movecheck accumulate at.
- **User `protocol`/`impl` method-call resolution — shipped.** The checker
  resolves `x.foo()` through its protocol registries (RFC-0002 §5, static
  dispatch), and the LSP surfaces it: `.foo` member completion offers the
  methods of every `impl P for T` matching a concrete receiver's type, and a
  bounded generic receiver (`fn f<T: Show>(x: T)` → `x.`) offers each bound
  protocol's method signatures. Hover on the method name at a call site
  resolves to the `impl` method declaration (a user symbol, so F12 works).
  Built-in method calls (`arr.push`, `log.info`, `Ref.get`, …) resolve the
  same way, and record-field member completion works too: `u.` on a record
  receiver offers the declaration's fields, with refined fields rendered as
  written (`age: Int64 where value >= 18`).

---

## RFC status

| RFC | Title | Status | Notes |
|-----|-------|--------|-------|
| 0001 | Vision | accepted | Principles & non-goals. |
| 0002 | Type system | mostly shipped | Records, enums, generics, transformers. |
| 0003 | Validated types | shipped | The signature feature, end to end. |
| 0004 | Capabilities & memory | decided | `consume` + both lowerings shipped; model settled as a hybrid defaulting to ownership (§5.2), measured. Surface refinements remain. |
| 0005 | Error handling | shipped | `Option` / `Result` / `match` / `?`. |
| 0006 | Diagnostics | draft | Message style used by the checker. |
