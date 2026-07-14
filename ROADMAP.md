# Vela — status & roadmap

The forward-looking companion to the [RFCs](rfcs/). What ships today, what's next,
and the one decision the rest of the language waits on.

**Every feature below is verified**: the clang-compiled native binary produces the
same output and exit code as the tree-walking interpreter (the reference
semantics), across **38 examples** and **188 tests** (0 warnings).

---

## Shipped

### Language core
- `Int` / `Bool`, `let`/`mut`, arithmetic, `if`/`else`, `while`, `for`-in over
  arrays, functions, `print`.
- Immutable string literals (`==`, `!=`, record fields), statically allocated.

### Types
- **Validated types** — `type Age = Int where value >= 18`. Provable constants
  rejected at compile time; others checked at runtime; zero cost when proven.
- **Nominal types** over `Int`/`Bool`/`String`, and **fallible construction**
  `Age?(n) -> Option<Age>`.
- **Structural records** with width subtyping and mutable fields (`c.x = ...`).
- **Transformers** — `Omit` / `Pick` / `Merge` / `Partial` / `Readonly`, plus
  intersection `A & B`. Pure type-level, erased before codegen.
- **Enums / sum types** with multi-payload variants and exhaustive `match`.
- **Generics** — functions, records, enums — inferred per use and monomorphized,
  with built-in bounds `Eq` / `Ord` / `Num`.

### Errors & control
- `Option<T>`, `Result<T, E>`, `match`, and `?` propagation (no null). `Option` and
  `Result` payloads may be any type, so `Option<Ref<Node>>` gives a nil terminator.
- **Checked conversions** — `str(Int) -> String` and `parse(String) -> Option<Int>`
  (the fallible case is an explicit `None`, never a silent 0 or a crash).

### Data structures
- **Arrays** — growable `Array<T>` (a `Vec`: `[]` / `a.push(x)` / `a[i]` /
  `a.length`, a doubling heap buffer, bounds-checked; non-escaping arrays reclaimed
  automatically, `drop a;` for handoff) and fixed-size **`Array<T, N>`** (a const
  generic: stack `[N x T]`, no heap, array-literal `[a, b, c]` syntax). Both
  iterate with `for x in arr { .. }`. The surface is subject-first — no
  `verb(object, …)` builtins.
- **Recursive heap structures** — a singly-linked list and a binary tree. `Ref<T>`
  makes the node type finite, `Option<Ref>` terminates it, and a recursive `release`
  walk reclaims the whole structure (proven: 100,000 nodes cycled through a
  65536-cell slab). Both build/traverse/reclaim end to end, to a flat memory
  baseline — the `Option` payload is two words wide, so a `Ref` is stored inline
  with no heap box.

### Memory (RFC-0004)
- `consume` capability + move checking (using a consumed value is a compile error).
- `modify` capability — a parameter changed in place, visible to the caller
  (by-reference / call-by-value-result; the argument must be a `mut` variable).

### Concurrency (RFC-0004 §Q4)
- **Structured fork-join** — `spawn f(args) -> Task<T>` / `join`. The compiler
  *proves* a spawned function is isolated (no I/O, no shared mutable state,
  transitively), so tasks are data-race-free and the result is schedule-independent
  — which is what keeps interpreter == native. `share` is the concurrent-read
  capability. (Execution is eager/sequential today; a parallel scheduler is a
  drop-in backend optimisation the model already guarantees is safe.)
- **The heap** — dynamic strings (`concat` / `len`), malloc-backed.
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
  pure adapter: it calls `analyze` once on open/change, caches the `Analysis`,
  and serves `textDocument/publishDiagnostics`, `/hover`, `/definition`, and
  `/completion` from it (a request never re-parses). Excluded from the default
  workspace (pulls `lsp-server`/`lsp-types`); built with
  `cargo build --manifest-path compiler/vela-lsp/Cargo.toml`. The only compiler
  call is `vela_frontend::analyze`, so the editor and CLI report identical
  errors. Hover/go-to-definition/completion cover top-level functions, types,
  and variants, plus local bindings (params, `let`s, `for`-in vars) — a local
  shadows a same-named top-level symbol; local hover shows the declared type for
  params and annotated lets.
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
- **User `protocol`/`impl` method-call resolution** (`x.foo()` → an `impl`
  method). Built-in method calls (`arr.push`, `log.info`, `Ref.get`, …) now
  resolve for hover and `.foo` member completion: the receiver's type is read
  from the local index and the built-ins for that type are offered. What stays
  deferred is *user-defined* method dispatch: the checker's `call()` resolves
  `recv.foo(args)` only as the free call `foo(recv, …)` against the top-level
  function table (`sigs`) — `impl` methods are not in that table, so the
  checker itself does not resolve them yet, and no example/test exercises
  `protocol`/`impl`. That grows a real method-dispatch path in the checker
  first; the LSP then surfaces it.

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
