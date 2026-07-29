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
- **M2 — the byte capabilities.** `read` and `list` as host imports, with the
  `allowed` mediation and read recording intact, and the canonical RFC-0014 error
  wording preserved. `std/tw` and `std/i18n` become servable.
- **M3 — reflection.** The `moduleInterface` import and its shared encoder.
  `std/rpc`, `std/openapi`, `std/graphql` and `std/ui` become servable.
- **M4 — `std/vyx`, and the measurement that matters.** The `.vyx` keystroke,
  end to end, against the 244 ms baseline.
- **M5 — fuel, traps, and the fallback.** Fuel mapping, trap-wording parity, and
  an explicit fallback to the interpreter for anything the wasm path refuses,
  so a generator can never become uncompilable by adopting this.

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
