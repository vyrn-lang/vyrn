# Vyrn compiler (prototype)

The Rust workspace that implements Vyrn. Every feature below is verified to
produce identical results three ways: the tree-walking interpreter (the
reference semantics), the clang-linked native binary, and the wasm module:

- **Core**: `Int64`, `Bool`, immutable + dynamic `String`, `let`/`mut`, arithmetic,
  `if`/`else`, `while`, `for`-in over arrays, functions, `print`, and `str`/`parse`
  conversions (`parse` is checked — returns `Option<Int64>`).
- **Collections, subject-first**: `[]` builds a growable array, `a.push(x)` grows
  it, `a[i]` indexes (bounds-checked), `a.length` is the count, and `drop a;`
  reclaims it explicitly (usually inferred — `drop` is the handoff escape hatch and
  a *consume*, so use-after-drop is a compile error).
- **Validated / nominal types** (RFC-0002 §2, RFC-0003): `type Age = Int64 where
  value >= 18`; `type UserId = String`; fallible construction `Age?(n)`.
- **`Option`/`Result`/`match`/`?`** (RFC-0005). `Option` and `Result` payloads may
  be any type, so `Option<Handle<Node>>` gives **recursive heap structures** — a
  linked list and a binary tree, each built, traversed, and reclaimed.
- **Structural records** with width subtyping, **intersection** `A & B`, and the
  **`Omit`/`Pick`/`Merge`/`Partial`/`Readonly`** transformers (RFC-0002).
- **Enums / sum types** with multi-payload variants and exhaustive `match`.
- **Arrays** — growable `Array<T>` (a `Vec`: `[]` / `xs.push(v)` / `xs[i]` /
  `xs.length`, a doubling heap buffer) and fixed-size `Array<T, N>` (a const generic,
  the stack `[N x T]` with array-literal `[a, b, c]` syntax); both bounds-checked.
- **Generics** — functions, records, enums — with inference, monomorphization,
  and **built-in bounds** `Eq`/`Ord`/`Num` (RFC-0002 §6).
- **Capabilities** (RFC-0004): `consume` (move checking) and `modify` (a parameter
  changed in place, visible to the caller via call-by-value-result).
- **Structured concurrency**: `spawn f(args) -> Task<T>` / `join` — a deterministic
  fork-join. The compiler *proves* a spawned function is isolated (no I/O, no shared
  mutable state, transitively), so it's data-race-free; `share` = concurrent read.
  A `Task<T>` is linear (RFC-0095): the join consumes it, `drop t` waits and
  discharges it, and its frame, record and OS handle go back at that one site.
- **Heap + deterministic reclamation** (RFC-0004 §4, no GC): dynamic strings
  (`concat`/`len`), a `region { .. }` block that frees a whole group of
  allocations at exit (escaping a heap value from a region is a compile error),
  an **ownership** pass that auto-frees individual heap temporaries proven not to
  escape their block, and **ownership transfer** so a function can return a fresh
  heap value that the caller then owns and frees (inferred by call-graph
  fixpoint). Every allocation is owned by exactly one mechanism, so nothing is
  freed twice; unprovable cases leak (safe).
- **Generational handles** (RFC-0090, `std/slots`): a `Handle<T>` is a
  freely-copyable value of three words — a slot, the generation live when the
  handle was issued, and the identity of the container that issued it. It owns no
  heap. `s[h]` checks the generation and yields the element's place, so a handle
  used after its element is removed traps on a compare instead of dangling. This
  is the answer to the aliasing case single ownership cannot express. The earlier
  `Ref<T>` cell (RFC-0004 §4, Path B) was deleted by RFC-0090 M4: `cell`, `get`,
  `set` and `release` no longer exist in any engine.

## What builds today (no LLVM needed)

The rule for the default workspace is that `cargo build` and `cargo test` need
**no LLVM, no clang and no wasi sysroot** — not that the resolve holds zero
crates. `wasm-encoder` is the one external crate, and it does not touch that
rule (see `Cargo.toml`).

```bash
cd compiler
cargo test        # the workspace suite: lexer/parser/checker/interpreter/codegen/movecheck/ownership
cargo build       # builds the `vyrn` binary
```

| Crate | Role |
|-------|------|
| `vyrn-frontend` | lexer → parser → AST → type checker → move checker → tree-walking **interpreter**; also the structured-`Diagnostic` API (`diagnostics(source)`) |
| `vyrn-codegen`  | emits **textual LLVM IR** (a string; no LLVM libs to produce it) **and the wasm module directly** (RFC-0077: `src/direct.rs`, `src/wasm.rs` — no LLVM, no clang, no sysroot) |
| `vyrn-cli`      | the `vyrn` driver |
| `vyrn-lsp`      | Language Server Protocol server (excluded — pulls `lsp-server`/`lsp-types`; see below) |
| `vyrn-genwasm`  | RFC-0076: runs `gen fn` generators as compiled wasm (excluded — pulls `wasmtime`; optional in both `vyrn-cli` and `vyrn-lsp`, feature `wasm-gen`) |

## Running programs

```bash
# interpret (process exits with main's return value)
cargo run -p vyrn-cli -- run    ../examples/fib.vyrn     # prints 55, exit code 55
cargo run -p vyrn-cli -- run    ../examples/fib.vyrn     # exit code 55

# type-check only
cargo run -p vyrn-cli -- check  ../examples/fib.vyrn     # -> ok
# type-check, multi-error: reports every type/ownership error across all functions
#   e.g. a return-type mismatch in f() and an arithmetic error in g() are BOTH shown:
#   bad.vyrn:1:0: return type mismatch: expected Int64, found Bool
#   bad.vyrn:2:0: arithmetic needs matching numeric operands, found Int64 and String

# emit LLVM IR to stdout
cargo run -p vyrn-cli -- emit-ir ../examples/fib.vyrn

# print every module a generator import synthesizes (RFC-0021)
cargo run -p vyrn-cli -- emit-gen ../examples/gendemo.vyrn
#   // ==== generated by palette("./data") at gendemo.vyrn ====
#   export type Count = Int64 where value >= 1
#   export fn colorCount() -> Count { return 3 }
#   ...
```

### Generator imports (RFC-0021)

`gen fn` is user code that runs at compile time and returns Vyrn source; an
import target may be a call to one, whose result is linked as an ordinary module:

```vyrn
import { palette } from "./lib/gen_palette"
import { colorCount, firstTheme } from palette("./data")   // runs at compile time
```

A `gen fn` is comptime-pure (no `extern`/`spawn`/module-state/`writeFile`); it
may read via mediated, path-scoped `readFile`/`listDir`/`moduleInterface`.
Generation is deterministic and cached (`~/.vyrn/cache/gen`), so rebuilds and the
LSP hit the cache. `vyrn emit-gen <file>` dumps the synthesized source.

### Function values (RFC-0023)

A `fn`-typed parameter takes a lambda literal or a named function; there are no
runtime function values (every use is monomorphized away — no function pointer in
any backend):

```vyrn
import { map, filter, fold } from "std/arrays"

fn main() -> Int64 {
    let xs: Array<Int64> = [1, 2, 3]
    let doubled = map(xs, |x| x * 2)          // lambda literal
    let bump = 10
    let bumped = map(xs, |x| x + bump)         // captures `bump` by read
    return fold(bumped, 0, |acc, x| acc + x)   // 46
}
```

`fn(T) -> R` is legal only as a parameter type; a lambda (`|x| expr` /
`|x, y| { block }` / `|| expr`) or a named function is legal only as such an
argument. Captures are read-only and fixed at the call site. `std/arrays` ships
`map`/`filter`/`fold`/`any`/`all` fully generic.

### Validated types (RFC-0003)

```bash
# compile-time rejection of a provably-invalid value:
echo 'type Age = Int64 where value >= 18; fn main() -> Int64 { let b = Age(5); return 0; }' > bad.vyrn
cargo run -p vyrn-cli -- check bad.vyrn
#   error: line 1: 5 does not satisfy `Age` (predicate `where value >= 18` is false)

# runtime validation compiled to native code (see examples/validate_fail.vyrn):
cargo run -p vyrn-cli -- build ../examples/validate_fail.vyrn -o vfail.exe
./vfail.exe ; echo $?     # prints "Vyrn: validation failed", exit 1
```

## Getting a native executable  ✅ verified working

The text IR targets **LLVM 15+** (opaque pointers) and has been compiled and run
natively with `clang` (tested against clang 22 on Windows). The `build`
subcommand does emit-IR + link in one shot:

```bash
# one-shot: emits <out>.ll next to the binary, then links with clang
cargo run -p vyrn-cli -- build ../examples/fib.vyrn -o fib.exe
./fib.exe ; echo $?      # prints 55, exit code 55
```

`vyrn build` finds clang via `$CLANG`, then PATH, then
`C:\Program Files\LLVM\bin\clang.exe`. Or do it by hand:

```bash
cargo run -p vyrn-cli -- emit-ir ../examples/fib.vyrn > fib.ll
clang fib.ll -o fib.exe
```

> Note: native output uses the platform C runtime, so on Windows `print` lines end
> with `\r\n`; the interpreter (`vyrn run`) uses `\n`. Same text, same exit codes
> — a benign line-ending artifact, not a semantic difference.

## Editor support — diagnostics + symbol query + LSP (core API)

Structured diagnostics and a symbol query are **core** responsibilities of the
front end, not editor-specific add-ons. `vyrn_frontend::diagnostics(source)`
returns every problem as a `Diagnostic { line, col, end_col, severity, stage,
message }` with a precise position, and `vyrn_frontend::analyze(source)` runs
the same pipeline *plus* a symbol index — both `vyrn check` and the LSP
consume the *same* API, no duplication.

- **Accumulation** is bounded: the lexer and parser stop at the first problem
  (recovery is future work), but once a file parses, every type/ownership error
  across all functions and types is reported — an error in one function does
  not suppress errors in the others. Inside a single function body the check is
  still first-error.
- **`vyrn check`** prints each as `file:line:col: message` (`col` is `0` when a
  stage knows only the line).
- **`vyrn_frontend::analyze`** (`src/symbols.rs`) returns an `Analysis {
  diagnostics, symbols, tokens, locals, decl_lines, fn_lines }` in one parse:
  the diagnostics, a `Symbol` per top-level function/type/variant/method (with a
  precise name column reused from the lexer's `Token.col` — the AST carries line
  only, no span), the identifier tokens for cursor→token mapping, and a
  `LocalBinding` per function parameter / `let` / `for`-in var (for variable
  hover + go-to-definition). `resolve(analysis, line, col)` maps a cursor to the
  declaration it names — a local in the cursor's enclosing function wins over a
  same-named top-level symbol (shadowing), with the latest binding at or before
  the cursor winning; `completions(analysis)` lists top-level symbols.
  `diagnostics()` delegates to `analyze()` and returns its `.diagnostics`, so
  there is one pipeline. The approach is deliberately non-invasive: no AST/parser
  span threading, just the token positions already on `Token`. Top-level names
  are unique (no shadowing), so top-level resolution is robust; local scope is
  line-based (an over-approximation — a `let` inside an `if` is treated as
  visible to the function's end; acceptable for hover/go-to-def).
- **`vyrn-lsp`** is a tiny, **synchronous** `lsp-server` server (no tokio, no
  async) and a **pure adapter**: it calls `analyze` once on open/change, caches
  the `Analysis`, and serves `textDocument/publishDiagnostics`, `/hover`,
  `/definition`, and `/completion` from it — a request never re-parses. It is
  **excluded** from the default workspace so the zero-dependency property holds;
  build it explicitly:

  ```bash
  cargo build --manifest-path compiler/vyrn-lsp/Cargo.toml
  # binary: compiler/vyrn-lsp/target/debug/vyrn-lsp(.exe)
  ```

  Hover/go-to-definition/completion cover top-level functions, types, and
  variants, **and local bindings** — function parameters, `let` bindings, and
  `for`-in variables (a local shadows a same-named top-level symbol; the latest
  binding at or before the cursor wins). Local hover shows the declared type for
  params and annotated `let`s; unannotated `let`s and `for`-vars still get
  go-to-definition (the inferred type isn't retained without a checker change).
  Still deferred: inferred-`let`-type hover, method-call resolution (`x.foo()`),
  `.foo` member completion, and parser recovery.

### VS Code extension (`editor/vscode/`)

A minimal, **plain-JavaScript** extension (no TypeScript compile step) that
spawns the `vyrn-lsp` binary and contributes a TextMate grammar for colors. To
try it: open this repo in VS Code and press **F5** (build the server first with
the `cargo build` above — the launch config no longer rebuilds it, to avoid a
Windows file-lock on the running binary aborting the launch); an Extension
Development Host window opens with `.vyrn` files colored, squiggled, and with
hover / F12 go-to-definition / completion. See `editor/vscode/README.md`.

## Semantics contract

All three execution paths — the interpreter, the native binary (textual IR
linked by `clang`), and the direct wasm module — must agree; the parity harness
(`vyrn-cli/tests/parity.rs`) is the gate. The interpreter in
`vyrn-frontend/src/interp.rs` is the executable reference; its unit tests
(`fib`, `while`+`mut`, arithmetic) plus the `examples/` are the shared
conformance cases. Verified match points include: `print` of a `Bool` prints
`true`/`false`; a compile-time-proven validated construction has no runtime
check; a failed runtime validation exits with code 1 (native prints
`Vyrn: validation failed`, interpreter prints a detailed message).

## Layout

```
compiler/
├── Cargo.toml              workspace (excludes vyrn-lsp, vyrn-genwasm)
├── vyrn-frontend/          lexer, parser, ast, checker, movecheck, interp, types, diagnostics (+ tests)
├── vyrn-codegen/           textual LLVM IR emitter + the direct wasm backend (+ unit tests)
├── vyrn-cli/               vyrn: run | check | emit-ir | emit-gen | build
├── vyrn-lsp/               LSP server (excluded — pulls lsp-server/lsp-types)
├── vyrn-genwasm/           wasm generation engine (excluded — pulls wasmtime; feature `wasm-gen`)

editor/vscode/             VS Code extension: extension.js (LSP client) + TextMate grammar
```
