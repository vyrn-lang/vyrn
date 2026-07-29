# RFC-0076 — Generators as Wasm: Compile the Generator, Don't Interpret It

- **Status:** Draft
- **Depends on:** RFC-0021 (`gen fn`, the comptime sandbox, the content-addressed
  generator cache), RFC-0012 (`extern` both directions, the String ABI),
  RFC-0014 (the WASI shim and its canonical error wording), RFC-0031
  (`moduleInterface` and its reachable type closure)
- **Evidence (measured, this repo):** a representative generator workload — scan
  a template byte buffer, accumulate output, look up line numbers — run as the
  same program both ways, producing identical output:

  | | time |
  |---|---|
  | interpreted (`vyrn run`) | **2,326 ms** |
  | wasm, total under the wasmtime CLI | 120 ms |
  | wasmtime process-launch floor | 106 ms |
  | **wasm, execution alone** | **~14 ms** |

  About **165x** on execution. Everything else in this document follows from
  that number and from what it costs to collect it.

---

## The problem

A `gen fn` is ordinary Vyrn run by the tree-walking interpreter. That was the
right call when generators were small: it needs no separate toolchain, it shares
one semantics with the rest of the language, and it made RFC-0021 tractable.

It stopped being cheap. `std/vyx` compiles a `.vyx` page by scanning bytes and
accumulating output — 4,536 lines of Vyrn, interpreted, per page, per keystroke.
After a long optimization arc (memoized DFAs, copy-on-write collections, an
incremental checker, a `lineAt` builtin) a `.vyx` keystroke is ~244 ms, and
roughly 200 ms of that is `std/vyx` being walked. The remaining profile is flat:
no dominant leaf, just a compiler written in Vyrn running about 165x slower than
the same program compiled.

The interpreter is not badly written. It is an interpreter.

## The change

Compile a generator module to wasm once, cache the artifact, and run generation
in an embedded wasm runtime. The interpreter stays — as the fallback, and as the
reference the wasm path is checked against.

Nothing about the language changes. A `gen fn` is still ordinary Vyrn, still
sandboxed, still returns a module source string. What changes is which engine
executes it.

## Why this is safe HERE

Swapping the execution engine of a macro system is normally a correctness
gamble: you are asserting that two implementations agree, on every program, on
output that is then compiled. Most languages cannot make that assertion.

Vyrn already does, continuously. The sacred invariant is that **interp == native
== wasm, byte-identical, including traps**, and CI proves it over every example
on every commit. That invariant is not a nice property here — it is *exactly the
correctness condition this RFC needs*. A generator is a Vyrn program; parity says
a Vyrn program means the same thing under the interpreter and under wasm;
therefore its emitted source is the same under either engine.

This is the rare case where a long-standing discipline pays for a feature that
was not anticipated when the discipline was adopted.

## The boundary

`interp::generate` already has the shape this needs:

```rust
pub fn generate(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: GenInputs<'_>,
) -> Result<GenOutput, String>
```

with `GenOutput { source, reads }` and `GenInputs { resolver, opts, importer_dir,
allowed, fuel, max_output }`. The wasm path implements the same function with the
same contract. The loader does not learn a new concept; it gains an engine
choice.

A generator's only interactions with the outside world are the three mediated
capabilities, and they become host imports:

| capability | host import | notes |
|---|---|---|
| `readFile` | `read(path) -> bytes` | mediated by `allowed`; every read is RECORDED into `GenOutput.reads`, which is what the cache validates against |
| `listDir` | `list(dir) -> names` | sorted by the host, as the interpreter already sorts, so output is deterministic |
| `moduleInterface` | `module_interface(spec) -> bytes` | the hard one — see below |

`fuel` and `max_output` map onto wasmtime's own fuel metering and a host-side
size check, so the guardrails RFC-0021 specified survive the move rather than
being reimplemented.

### `moduleInterface` is the interesting import

`readFile` and `listDir` are byte pipes. `moduleInterface` is not: it links a
module and returns structured reflection over its reachable type closure
(RFC-0031). That is compiler machinery, and it stays in the host.

So the import returns the reflection **serialized**, and the guest decodes it.
The serialization must be exactly what the interpreter already materializes, or
the two engines diverge — which makes this the one place where "parity proves it"
needs help from a shared encoder rather than from two implementations that
happen to agree.

The natural encoder is the one already in the language: `std/json` with the
existing `TypeInfo`/`ModuleInterface` records. Same shape, one writer, one
reader.

## Cost, and where it lands

Compiling a generator to wasm is not free, and doing it per keystroke would
trade one problem for another. It is not per keystroke:

- The generator's compiled artifact is content-addressed on **the generator's own
  module closure** — the same closure RFC-0072's self-describing cache entry
  already records for invalidation. Editing `std/vyx` recompiles it; editing a
  page does not.
- The artifact cache is the existing on-disk generator cache, which already
  persists across sessions. (Worth noting: rust-analyzer does NOT persist its
  proc-macro caches, and reopening a project pays for it. Vyrn already does.)
- The 106 ms in the measurement above is **process launch of the wasmtime CLI**,
  not execution. Embedding the runtime replaces process spawn with module
  instantiation. Instantiation of a cached, already-compiled module is
  milliseconds.

Expected: a `.vyx` keystroke of ~244 ms becomes execution (~14 ms) plus
instantiation plus the unchanged load/check work — call it 50–80 ms, pending
measurement. That is the number this RFC will be judged on, not the 165x.

## What this does NOT do

- It does not change the language. `gen fn` is the same construct.
- It does not remove the interpreter. `vyrn run` still interprets; the
  interpreter remains the parity reference and the fallback for any generator the
  wasm path cannot yet serve.
- It does not make generation incremental. Editing a page still recompiles that
  page. Element-granular generation is a separate question, and this RFC lowers
  the cost enough that it may never need answering.
- It does not apply to `vyrn build`'s own speed beyond generation.

## How the runtime is hosted — the open decision

Measured after the first draft, and it changes the shape of this RFC.

The wasmtime CLI's ~106 ms is **process launch**, not module compilation.
Precompiling to `.cwasm` does not help: a trivial module still costs 121 ms, and
the real one 129 ms. So spawning a process per generator call is a non-starter —
a `.vyx` keystroke runs two generators, which is ~240 ms of launch, exactly the
cost this RFC exists to remove.

The runtime must therefore be long-lived. Three ways, and the choice is a
project-level one because it trades against a stated property of this workspace:
the default members build with **zero external dependencies**, which is why the
LLVM backend and the LSP are excluded crates rather than optional features.

**(A) Feature-gated dependency.** `wasmtime` as an optional dependency of
`vyrn-frontend`, off by default. `cargo build` stays dependency-free; the fast
path is `--features wasm-gen`. CI builds both. Simplest to implement and to
reason about; the cost is that the zero-dep property becomes "zero by default"
rather than absolute, and the shipped binary is the one with the feature on.

**(B) Excluded crate.** Consistent with `vyrn-codegen-llvm` and `vyrn-lsp`. But
those depend INTO the workspace; nothing in the default members may depend back
out, so `vyrn` itself could not use the engine. It would mean a second driver
binary, which splits the tool.

**(C) A persistent generation server.** One wasmtime process for the session,
fed work over a pipe — which is how rust-analyzer hosts proc-macro expansion, and
which needs no Rust dependency at all. Launch is paid once per session instead of
per call. `readFile`/`listDir` can go through WASI preopens, which the wasm
target already uses (RFC-0014), so M1 and M2 need no custom host functions.
`moduleInterface` is the difficulty: it is a callback from guest to host, which
over a pipe is a request/response protocol rather than an import — and the plain
wasmtime CLI cannot host a custom import at all, so M3 would still need an
embedding or a redesign of how reflection reaches the guest.

Recommendation: **(A)**, with (C) as the fallback if the dependency is judged too
expensive. (A) keeps one binary, one code path, and lets reflection stay a normal
host import instead of a protocol. The zero-dep property survives where it
matters — a contributor without wasmtime can still build and test everything.

## M1 validation (done, by hand, before writing the engine)

The engine's plan is: take the generator module, clear `is_gen` on the target
function, synthesize a `main` that calls it with the constant arguments and
prints the result, compile that to wasm, and take stdout as the module source.
That was validated manually on a capability-free generator emitting 200
functions:

| | sha256 of emitted source |
|---|---|
| wrapper, interpreted | `50edb49d…` |
| wrapper, compiled to wasm | `50edb49d…` |

Identical. Clearing `is_gen` compiles cleanly, and the emitted source survives the
engine change — which is the property the whole RFC rests on, now checked rather
than assumed.

**And a negative worth keeping.** On that same generator, timed end to end:

| | total | of which process launch |
|---|---|---|
| interpreted | 85 ms | ~30 ms |
| wasm via the wasmtime CLI | 103 ms | ~106 ms |

Execution fell to approximately zero and launch swamped it, so **wasm-via-
subprocess is SLOWER than interpreting for a small generator**. This is the same
finding as the `.cwasm` measurement from a different angle, and it settles
something: the subprocess route is adequate for validating correctness — it is how
the table above was produced — and useless for performance. The engine only pays
once the runtime is embedded and instantiation replaces process spawn. Any
milestone that reports a speedup from a subprocess measurement is measuring the
wrong thing.

## M1 shipped, and the blocker it exposed

M1 is implemented: `vyrn-cli/src/genwasm.rs`, behind `--features wasm-gen`. It
clears `is_gen`, synthesizes a `main` that calls the generator with `args()` and
prints the result, compiles that to wasm, and runs it in an embedded wasmtime
with a hand-written 10-import WASI (the same shim `web/wasi-min.js` is, in Rust).
Output is byte-identical to the interpreted run. Arguments travel as **argv**
rather than baked-in literals, so one compiled artifact serves every call — four
calls to one generator compile once.

Then it was measured, and it is **slower than the interpreter**:

| heavy capability-free generator, 4 calls | |
|---|---|
| interpreted | 1,496 ms |
| wasm (one compile, four executions) | 2,548 ms |

Phase trace: clang 257 ms, cranelift 37 ms, and then **~600 ms per execution** —
against ~370 ms interpreted. Execution was supposed to be the fast part.

Worth stating plainly: M1 serves **zero** generators in this repo. Every one of
`std/vyx`, `std/tw`, `std/i18n`, `std/rpc`, `std/openapi`, `std/graphql` and
`std/ui` reaches `readFile` or `moduleInterface`, so all nine generator-using
examples decline to the interpreter and emit byte-identical sources. That is M2
and M3's work, and it is correctly ordered after the blocker below.

The cause is not wasm. It is that the compiled backend has no in-place string
append, and the interpreter does:

```vyrn
let mut out = ""
while i < 4000 { out = out + "export fn f\{i}() -> Int64 { return \{i} }\n"; i = i + 1 }
```

| | time |
|---|---|
| interpreted | **52 ms** |
| native (`vyrn build`) | **512 ms** |

The interpreter appends in place when a local `String` accumulates onto itself.
`emit_str_concat` allocates a fresh buffer and `strcpy`+`strcat`s both halves
every iteration, so the same loop is quadratic compiled and linear interpreted —
**10x slower compiled, on the single idiom every generator is built out of.**

Compute is unaffected and confirms the original premise: a 5M-iteration
arithmetic loop is 2,166 ms interpreted against 337 ms native (which includes
~300 ms of process launch).

So the 165x was real for compute-bound generators and unreachable for the
string-building ones, which is all of them. **RFC-0076 was blocked on a codegen
fix, not on anything in this RFC**: `s = s + x` on a local `String` had to append
in place, with capacity, the way the interpreter already does.

## M1.5, and the number the RFC was written for

Fixed: a local `String` that only ever accumulates onto itself now appends in
place with amortized growth, guarded by a whitelist eligibility rule (every
occurrence of the name must be a self-append root, a `.field` read, an
`@str`/`@concat` operand, or a tail `return` — anything else, including an
unknown callee, disqualifies it). Accumulation went from quadratic to flat:

| 50,000 appends | before | after | interpreted |
|---|---|---|---|
| | 28,692 ms | **47 ms** | 103 ms |

And with that, the same four-call generator workload measured above:

| | cold, 4 calls |
|---|---|
| interpreted | 1,484 ms |
| wasm | **255 ms** |

Phase trace: clang 158 ms, cranelift 25 ms, first execution ~3 ms — and calls
two, three and four cost **1 ms each**, because the artifact is
argument-independent and cached. Per-execution that is ~370x, against ~370 ms
interpreted. Compilation is now the entire cost, paid once, and a long-lived
process pays it once per generator rather than once per call.

## M2 shipped, and what it was actually blocked on

`readFile`, `readFileBytes` and `listDir` are now host imports in the module
`vyrn_gen`, backed by `GenInputs.resolver` — not by the filesystem, because in
the LSP the resolver serves UNSAVED buffers and a guest that opened files itself
would read different bytes than the interpreter. Two imports, so the host never
allocates inside guest memory:

| import | signature | |
|---|---|---|
| `vyrn_gen.read` | `(path: i32, mode: i32) -> i64` | resolves, mediates, reads, records; returns `(status << 32) \| len` and stashes the bytes host-side |
| `vyrn_gen.fetch` | `(dest: i32)` | copies the stash into a buffer the GUEST allocated |

`mode` is 0 `readFile` / 1 `readFileBytes` / 2 `listDir` (whose stash is the
sorted names joined by `\n` — the interpreter's own recording encoding). `status`
is the alphabet the compiled `readFile` caller already renders errors from (0 ok
/ 1 io / 3 embedded NUL), which is why the RFC-0014 wording needed no new
agreement: `@.io.readerr`/`@.io.nulerr` already say exactly what
`Interp::gen_read_file` says. The mediation rule is not reimplemented — the
interpreter's was extracted into `interp::gen_scoped_path` and both engines call
it; a rejected read is a `wasmtime::Error` that unwinds out of `_start`, so the
guest can never observe it as a value. Measured: the cache key for
`examples/gendemo` is byte-identical under both engines, i.e. the recorded reads
are.

Only `readFile` and `readFileBytes` needed no codegen change. `listDir` had no
lowering at all, and must keep having none in the language, so it lowers behind
`emit_gen_host` — a second entry point beside `emit`, one flag, two `if`s.

**And the milestone's premise was wrong.** `std/tw` and `std/i18n` do not become
servable, and neither reaches for a capability this milestone lacks: both build
their output with RFC-0054 code quotes. `Code` is a comptime-only *value type* (a
piece list rendered with origin directives, spliced with context-dependent
escaping, produced by a `lex()` that is a whole Vyrn lexer), so serving those
generators needs that type lowered — a milestone of its own, not a host import.
`contractOf` joins `moduleInterface` on the M3 side for the same reason. Sorted
by what actually blocks each example: `@codeSplice` (tw, i18n, pages, vyx),
`lex` (vyx), `contractOf`/`moduleInterface` (rpc, ui). The generator M2 was
really about is `examples/gendemo`, which reads a file AND lists a directory, and
which the engine now serves with byte-identical output.

Timings, cold, cache cleared, `examples/gendemo`: interpreted **33 ms**, wasm
**~240 ms** — of which clang is 163 ms and cranelift 27 ms. On a generator that
small the engine loses, exactly as §M1's negative predicted for anything where
compilation dominates. On a synthetic read-driven generator doing 40,000 appends:
interpreted 200 ms, wasm 380 ms of which 199 ms is compilation and ~9 ms is
execution — the execution gap is real (~18x) and the compile is the whole cost,
paid once per artifact per process. Nothing in this repo yet calls a servable
generator often enough to amortize it, which is another way of saying M3 is where
the win lives.

## What M2 found, and the plan it produced

M2 shipped the byte capabilities and served exactly one generator:
`examples/gendemo`. The capabilities were never the hard part. What actually
blocks the generators that matter is two things this RFC did not anticipate:

| generator | blocked by |
|---|---|
| `std/tw`, `std/i18n` | RFC-0054 code quotes (`@codeSplice`) |
| `std/openapi`, `std/graphql` | `moduleInterface` |
| `std/rpc`, `std/ui` | `contractOf` + `moduleInterface` |
| `std/vyx` | `lex` + `moduleInterface` |

And the target, measured on the real pages (`.vyx` keystroke, LSP end to end):
`examples/bin/routes/index.vyx` **243 ms**, `about.vyx` 163 ms,
`widgets/CreateForm.vyx` 87 ms, `routes/layout.vyx` 49 ms.

### The unifying idea: keep compiler machinery in the host

Every remaining blocker is a comptime builtin that needs something only the
compiler has — a lexer, a linker, a piece list with exact splice semantics. None
of it should be reimplemented guest-side, because a second implementation is a
second chance to disagree, and disagreement here means two different programs.

So each one becomes a host import, and the only question per builtin is what
crosses the boundary:

- **`Code` is an opaque handle.** `Type::Named("Code")` lowers to `i64`, an index
  into a host-side arena of `Vec<CodePiece>`. `@codeText`, `@codeSplice`, `raw`,
  `rawAt`, `render` and `Code + Code` all become host imports operating on
  handles. The splice rules, the string escaping and the float formatting stay in
  the one Rust implementation, so they are byte-identical by construction rather
  than by testing. `Code` is comptime-only and never escapes into runtime data,
  which is exactly what makes an opaque handle a faithful representation.
- **`lex` returns data, so it is serialized.** The token record shape is fixed and
  known to codegen (`{kind, text, line, col}`), so the guest decoder is
  mechanical. The lexer itself stays single-sourced in the host — a Vyrn lexer
  written in Vyrn would be a second lexer to keep in agreement, which is the
  thing worth avoiding above all.
- **Reflection is serialized too**, per the seam this RFC already identified.
  `moduleInterface` cannot be pre-evaluated and spliced: all eighteen call sites
  in `std/` take a runtime-computed path (`m.modPath`, `contract`, `modPath`),
  never a literal.

### Revised milestones

- **M3a — `Code` as an opaque host handle.** DONE, and it did unblock `std/tw`
  and `std/i18n` — see below.
- **M3b — structured host results.** DONE, and it served every remaining
  generator — see below. `lex`, `moduleInterface` and `contractOf` are one
  problem, not three: each returns *a value of a known named type*
  (`Array<Token>`, `ModuleInterface`, `ContractInfo`) built entirely out of
  strings, ints, bools, records and arrays. So build the mechanism once — a host
  encoder and a synthesized decoder that BOTH walk the static type, which is
  what makes them agree — instead of a JSON schema two sides have to interpret
  the same way. (The earlier plan split these and reached for `std/json`; the
  type is already the schema, so it does not need one.)
- **M4 — a home the LSP can reach.** The engine lives in `vyrn-cli` today and
  `vyrn-lsp` is a separate excluded crate; the payoff is entirely in the
  long-lived process, so this is where the 243 ms is finally measured.
- **M5 — fuel, traps, and the on-disk artifact cache**, so a cold editor session
  does not pay clang on its first keystroke.

## M3a shipped: `Code` as a handle, and the rules stayed put

`Type::Named("Code")` lowers to `i64` on the generator-host path — an index into
a per-run arena of `Vec<CodePiece>` living in the wasm store — and every
operation on it is an import in the `vyrn_gen` module: `text`, `splice`, `rawAt`,
`concat`, `render`. Nothing guest-side knows what a piece is. `render_code` and
the splice table run in the host, which is the interpreter's own code, so the
escaping, the identifier validation and the shortest-roundtrip float formatting
are byte-identical by construction rather than by testing. The one new frontend
API is `interp::gen_code_splice`, which is `code_splice` with a `String` error
instead of a `Ctrl` — the `gen_scoped_path` precedent, for the same reason.

`@codeSplice`'s value crosses as a **tag plus one 64-bit word** (plus a pointer
when it is a String), because the host needs the value and cannot chase a guest
pointer to anything else: `(i32 tag, i64 bits, ptr, i64 ctx) -> i64`. The tag
names the interpreter `Val` the host rebuilds and is a COMPILE-TIME constant —
codegen knows the static type at every call site — so there is no runtime
dispatch. It covers exactly what the splice rule accepts: String, `Code` (as its
handle), Bool, signed and unsigned integers (`sext`/`zext` to agree with the
tag), `Float64` and `Float32` as bit patterns, which is lossless and leaves the
formatting where it belongs. Equality on `Code` needed no import: the checker
permits only `+` on it.

The imports are declared in the emitted IR with the same `wasm-import-module`
attribute groups an RFC-0012 `extern` uses, so the C shim gained nothing. It
could not have, cheaply: an unused `extern` in C emits nothing, so a shim
declaration would have needed a pass-through call per import just to keep its
attributes alive.

Blast radius on the emitter: one `llt` arm, one `gen_binary` branch, one dispatch
in `gen_call`, and the declarations. An ordinary build still refuses a code quote
with the RFC-0054 wording — "`render` is only available during generation" —
which is now a test.

Measured, cold, cache cleared, three runs, medians:

| | interpreted | wasm | of which clang | cranelift | execution |
|---|---|---|---|---|---|
| `twdemo` | 113 ms | 352 ms | 250 ms | 61 ms | ~2 ms |
| `i18ndemo` | 72 ms | 404 ms | 295 ms | 77 ms | ~2 ms |

Against a 37 ms baseline for the same command with the on-disk generation cache
warm (load, check and emit, no generation at all), interpreted generation is
~76 ms for `twdemo` and ~35 ms for `i18ndemo`, against ~2 ms executed — 38x and
18x. And the total is still worse, for the third milestone running, because one
`emit-gen` process calls each generator exactly once and pays a whole clang for
it. That is not a new finding, it is the same one M1 and M2 recorded, and M4 is
the milestone that resolves it: in a long-lived process the compile is paid once
and every keystroke after it costs the ~2 ms.

`std/vyx` still declines on `lex` (M3b) and the reflection generators on
`contractOf`/`moduleInterface` (M3c), as expected. All twelve generator-using
examples emit byte-identical sources under both engines; `examples/bin/server`,
`examples/shelf/server` and `examples/shelf/client` now serve two generators each
while declining the rest, which is the fallback working per call rather than per
module.

## M3b shipped: one transfer, walked from both ends

`lex`, `moduleInterface` and `contractOf` were built as one mechanism, because
they are one problem: each returns a value of a known named type. A host import
`reflect(kind, arg)` computes that value — the real lexer, the real linker, the
real contract table, all still in the compiler — and leaves it as a flat stream
of atoms. `nextInt`/`nextStr` pull the atoms back; `nextStr` answers with a
length and the guest allocates, the same stash protocol M2 already had.

**What makes the two sides agree is that both walk the static type.** An array
pushes its length first, a String its bytes, an Option a presence atom, a record
its fields in the DECLARATION's order — and nothing else is tagged, because the
reader always knows what it is about to read. There is no self-describing
format, so there is no format for two implementations to read differently. Both
sides get the field order from the same `record_fields`, so the order is not a
convention either has to remember. The host pulls each field out of the
reflection literal BY NAME rather than positionally: `module_interface_lit`
happens to build them in declaration order today, and depending on that would be
a silent, load-bearing coincidence.

The decoder is **not hand-written IR**. The engine synthesizes it as ordinary
Vyrn functions appended to the wrapper program, so the arrays it grows, the
records it builds and the Options it unwraps are the ones every other Vyrn
program gets — a change to how codegen lowers a record cannot make the two walks
disagree. Codegen's whole share is redirecting the three builtins' call sites to
those entry points plus lowering three stream primitives. `contractOf` gets one
nullary entry per declared contract, since its argument is a declaration, not a
value; `lex` gets one only when no user function claims the name, which is how
the shadowing rule survives without being restated.

The atom stream is also its own tripwire. A `nextInt` that finds a string, a
read past the end, or an unread atom left when the value is finished all trap
with a message saying the two walks disagreed — the one failure this design can
have, made loud instead of silent.

`moduleInterface` is the one that had to be exactly right, because getting it
wrong is a stale cache hit rather than a wrong answer. It is not reimplemented:
`Interp::gen_module_interface` became the free function
`interp::gen_module_interface_lit` and both engines call it, so the
`RecordingResolver` link and the reachable type closure (RFC-0031) record
identically. Measured on a generator reflecting a module whose types are
declared in a THIRD file: the on-disk cache entries — which are the recorded
read lists — are byte-identical under both engines, and editing that third file
(never a generator argument) misses the cache under both. That is now a test.

**Every generator in the repo is served.** All twelve generator-using examples
emit byte-identical sources with zero declines, where M3a still declined
`std/vyx`, `std/rpc`, `std/ui`, `std/openapi` and `std/graphql`.

Measured cold, cache cleared, medians of three:

| | interpreted | wasm | clang | cranelift | execution |
|---|---|---|---|---|---|
| `vyxdemo` (`lex` + reflection) | 72 ms | 567 ms | 399 | 94 | ~2 ms |
| `rpc` (`moduleInterface` + `contractOf`) | 50 ms | 441 ms | 329 | 54 | ~2 ms |

Against a 41 ms / 37 ms baseline for the same command with generation cached,
interpreted generation is ~31 ms and ~12 ms against ~2 ms executed. The total is
worse for the fourth milestone running, for the fourth identical reason: one
`emit-gen` process calls each generator once and pays a whole clang for it.
`examples/bin/server.vyrn` shows both halves of that at once — ten generator
calls, of which two hit the artifact cache and cost **2 ms and 1 ms** while the
other eight each pay a fresh ~350 ms compile. The artifact is
argument-independent and the cache works; there is simply nothing in a one-shot
process to amortize it against. M4 is that milestone.

## Risks, honestly

**A new dependency.** wasmtime is a large crate, and this workspace deliberately
has none — see the hosting decision above, which is the main open question in
this RFC rather than a footnote to it. The counter is that the alternative — a
bytecode VM for comptime — is both more work and, on the evidence, a much smaller
win: GoAWK's real-world suite gained 13% moving from tree-walking to bytecode, and
a Prolog study measured bytecode 25–60% SLOWER than its AST interpreter. A
rewrite with an uncertain 1.1–4x, versus a dependency with a measured 165x.

**Two engines can drift.** Mitigated by the parity suite, and by keeping the
interpreter as the reference rather than deleting it. A generator whose wasm and
interpreted outputs differ is a parity failure, reported like any other.

**The `moduleInterface` encoder is a genuine seam.** It is the one place where
correctness rests on a shared serialization rather than on shared semantics. It
gets its own tests, comparing host-side reflection against guest-side decode for
every example that uses reflection.

**Fuel semantics must match.** RFC-0021's step budget and wasmtime's fuel are not
the same unit. Exhausting either must produce the same canonical message, or a
runaway generator traps differently depending on the engine.

## M4 shipped: the long-lived process, and the number

The engine moved out of `vyrn-cli` (a binary crate, so nothing could depend on
it) into `compiler/vyrn-genwasm`, EXCLUDED for the same reason
`vyrn-codegen-llvm` and `vyrn-lsp` are: `cargo build` and `cargo test` at
`compiler/` must still resolve to path dependencies only. Both consumers depend
on it optionally, behind a feature spelled `wasm-gen` in each — off by default in
the CLI, ON by default in the LSP, which is the process the engine exists for.

The move needed one thing that was not a move. `genwasm.rs` reached into its
parent crate for the C runtime shim and for clang/wasi-sysroot discovery, and the
ordinary `vyrn build` path uses those too — so they could not follow the engine
into a crate the driver depends on only optionally. They went DOWN instead, into
`vyrn-codegen::toolchain`, which both crates already depend on and which is where
they belong on their own account: the shim is the C half of what codegen emits,
and clang is what codegen's output is fed to.

Installing an engine is not analysis, so the LSP stays a pure adapter: one call
at startup, the same shape and the same `VYRN_NO_WASM_GEN` escape the CLI's
`real_main` has. No clang, or no wasi sysroot, is a `decline` — the editor falls
back to the interpreter and is slower, never broken.

Measured with the same release LSP binary, engine on against
`VYRN_NO_WASM_GEN=1`, eight keystrokes each:

| page | keystroke, interpreted | keystroke, wasm (min/median/max) | cold `didOpen` |
|---|---|---|---|
| `examples/bin/routes/index.vyx` | 243 ms | 53 / **54** / 59 ms | 315 → 1,220 ms |
| `examples/bin/routes/about.vyx` | 162 ms | 52 / **54** / 57 ms | 318 → 1,176 ms |
| `examples/bin/widgets/CreateForm.vyx` | 87 ms | 35 / **37** / 38 ms | 290 → 584 ms |
| `examples/bin/routes/layout.vyx` | 44 ms | 34 / **37** / 44 ms | 843 → 765 ms |
| `examples/shelf/widgets/ShelfApp.vyx` | 91 ms | 32 / **34** / 35 ms | 653 → 740 ms |

**4.5x on the page this RFC was written about**, and the interpreted column
reproduces the pre-M4 baseline exactly, so the two columns really are the same
build differing only in engine.

The spread is the finding, and it is the opposite of what was expected: the
keystrokes are FLAT (53–59 ms on `index.vyx`, no first-keystroke outlier), and
the whole compilation cost lands in `didOpen`, which pays clang for all seven
artifacts the page's imports reach — **+905 ms, once per editor session**. Per
keystroke the trace is two generator calls at 2.5–4 ms of execution each, against
~190 ms interpreted. Break-even is about five keystrokes; a session is thousands.

Two smaller measurements, both of which had to be checked rather than assumed:

- **The artifact key costs 1.1–1.9 ms per call**, so ~3 ms of a 54 ms keystroke.
  It is a `Debug`-hash of the whole generator program (`std/vyx` is 4,536 lines)
  and it now runs on every keystroke — about 40% of what a cache-hit generation
  costs, which makes it the next thing worth attacking, but not yet.
- **A `.vyrn` keystroke did not move**: `examples/shelf/server.vyrn` is 23–27 ms
  under both engines across three paired runs, and the trace shows zero generator
  calls during those keystrokes. Nothing was paid by files that do not generate.

### Is M5's on-disk artifact cache urgent?

Not urgent, and it is now measurable rather than speculative. The cost it removes
is exactly the +905 ms on the FIRST page opened per session, amortized over every
keystroke after it. That is a real editor-startup cost and worth removing, but it
is paid once per session against a saving of ~190 ms per keystroke — the ordering
in the milestone list was right.

## M5 shipped: the guardrails, and what a cold session costs now

### Fuel, which was a correctness bug rather than polish

The engine honoured no budget at all. A generator that fails loudly under the
interpreter's step budget ran forever under wasm — and since M4 that means it
hung the editor, which is the one failure mode this whole path was supposed to
remove.

Fuel metering (`Config::consume_fuel`, `Store::set_fuel`), NOT epoch
interruption: an epoch is wall-clock, so the same generator would die on a slow
machine and pass on a fast one, and determinism is what every claim in this
document rests on.

The units cannot be reconciled — the interpreter spends a step per Vyrn
STATEMENT, wasmtime a fuel per wasm INSTRUCTION — so the mapping is biased
deliberately loose, and the multiplier is measured rather than guessed. Every
generator call in the repo, run under both engines (`VYRN_GEN_STEPS` against
`VYRN_GENWASM_TRACE`, which is why the first of those is now a permanent
one-line trace beside the second):

| generator | steps, interpreted | fuel, wasm | fuel/step |
|---|---|---|---|
| `std/tw` | 91,234 | 28,017,086 | 307 |
| `std/vyx` (`vyxPageThemed`) | 253,128 | 38,481,467 | 152 |
| `std/i18n` | 165,554 | 12,256,992 | 74 |
| `std/rpc` (`rpcClient`) | 560 | 250,965 | 448 |
| `examples/gendemo` | 31 | 23,416 | 755 |

The two small ones are the fixed ~23k of wasi-libc startup showing through; the
worst SUSTAINED ratio, once that is discounted, is ~410. So: **1,000 fuel per
step, plus a flat 1,000,000** — ~2.4x above the worst measured, with the flat
term absorbing the startup so a generator of a few dozen statements is not killed
by it. Anything inside the interpreter's budget is inside this one, which leaves
the only divergence in a band where wasm succeeds and the interpreter would have
failed. That direction never breaks a generator that worked.

It is a margin and not a proof — one Vyrn statement can copy an unbounded number
of bytes — and the comment on `wasm_fuel` says so. What it buys is that the
default budget now burns out in **~1.6 s** against ~3.4 s for the same runaway
interpreted, with byte-identical wording. That is a test, not a manual check.

### Trap wording, where the engines really did differ

`error: division by zero` against `division by zero`. The compiled runtime
prefixes a trap on its way to stderr because at the TOP level the CLI prints the
same prefix for an interpreted trap — that is what parity compares. Inside
generation there is no CLI: the interpreter hands the loader a bare message and
the loader supplies the context. So the engine strips the prefix, and three trap
kinds (array index, division by zero, string index) are now asserted identical
end to end, which is what the user actually reads.

### The artifact key stopped being a hash of the program

It was a `Debug`-format hash of the whole generator program — 1.1–1.9 ms of a
54 ms keystroke, ~40% of what a cache-hit generation costs, on 4,536 lines of
`std/vyx`, every keystroke. It is now the loader's own content hashes of the
generator's module closure, handed over as `GenInputs.sources_fingerprint`:
**0.05–0.07 ms**, and free, because the loader hashes exactly those files anyway
to write the generation cache entry. The hashing simply moved above the run from
below it.

Correctness first, since a stale artifact is a silently wrong program: the
fingerprint covers every module in the generator's closure (the same graph whose
hashes decide whether the OUTPUT cache entry is still valid), plus the generator
module key and the std root, because the contract restamping spells modules
against those. When the closure contains something no resolver can re-read — a
generated module — there is no honest fingerprint, and the `Debug` hash stays the
fallback rather than a cheap key that could miss an edit.

### The compiled artifact persists

`Module::serialize` output, beside the generation cache at
`~/.vyrn/cache/gen/wasm`, keyed the same way as the in-process cache and carrying
the compiler binary's own identity (every crate here is version `0.0.0`, so the
executable's size and mtime are the only honest answer to "which build emitted
this"). Deserializing skips cranelift AND clang, which is the entire cold cost.

`Module::deserialize` is `unsafe` and trusts its input completely, so it is
confined to one function whose input is a file this cache directory wrote, and
every failure — missing, truncated, foreign, corrupt — is a MISS that recompiles.
Checked by corrupting and truncating artifacts on disk: the run recompiles and
emits the same 130,764 bytes. wasmtime's own header carries its version and
configuration and refuses anything foreign, which is what makes a wasmtime
upgrade a miss rather than a crash.

Measured on `examples/bin/routes/index.vyx`, same LSP binary, generation cache
cleared for both so only the artifacts differ:

| | cold `didOpen` | keystroke, median of 8 |
|---|---|---|
| no artifacts on disk | 3,934 ms | 58 ms |
| artifacts on disk | **201 ms** | 61 ms |
| everything warm | 134 ms | 62 ms |
| `VYRN_NO_WASM_GEN=1` | 391 ms | 262 ms |

**20x on opening the first page of a session**, which was seven artifacts of
clang, and the keystroke is unmoved — 58–63 ms against 56 ms before, the spread
of the measurement itself, with fuel metering now on. The interpreted column
reproduces its own baseline, so the two are still the same build differing only
in engine.

## Milestones

- **M1 — embed the runtime, one generator, no capabilities.** DONE. wasmtime as
  a library; compile a `gen fn` with no `readFile`/`listDir`/`moduleInterface` to
  wasm; run it; the emitted source is byte-identical to the interpreted run.
  Proves the boundary and kills the 106 ms — and shows the next milestone is not
  the one that was planned.
- **M1.5 — in-place string append in codegen.** DONE. `s = s + x` on a local
  `String` appends with amortized capacity instead of reallocating and copying,
  matching what the interpreter does. Was blocking every milestone after it, and
  was worth doing on its own account.
- **M2 — the byte capabilities.** DONE. `read` and `list` as host imports, with
  the `allowed` mediation and read recording intact, and the canonical RFC-0014
  error wording preserved. `std/tw` and `std/i18n` do NOT become servable — see
  below.
- **M3 — reflection.** DONE (M3a `Code` as a handle, M3b the structured
  results). Every generator in the repo is served, with byte-identical output.
- **M4 — a long-lived process, and the measurement that matters.** DONE. The
  engine moved to its own excluded crate and the LSP installs it; the `.vyx`
  keystroke fell from 243 ms to 54 ms — see below.
- **M5 — fuel, traps, and the artifact cache.** DONE. A measured fuel mapping,
  the trap-wording divergence found and fixed, an artifact key that costs
  0.05 ms instead of 1.5 ms, and compiled artifacts that persist across
  sessions — a cold `didOpen` of 3,934 ms became 201 ms. The fallback to the
  interpreter was already there from M1 and is what every decline uses.

## Acceptance

- A generator's emitted source is byte-identical under both engines, for every
  example in the repo, asserted in CI beside the existing parity tier.
- The generator cache invalidates on the generator's own sources exactly as it
  does today (RFC-0072's self-describing entries), now covering the compiled
  artifact too.
- `vyrn emit-gen` output stays byte-identical across runs and across engines.
- A trap inside a generator — fuel exhaustion, an out-of-bounds index, a rejected
  read — produces the same message under both engines.
- The `.vyx` keystroke is measured, before and after, on `examples/bin`.
- The interpreter path still works with the wasm path disabled, and that
  configuration is exercised in CI.
