# RFC-0055 — Benchmarking: `bench` Blocks, `blackBox`, `vyrn bench`

- **Status:** Locked design
- **Depends on:** RFC-0015 (testing — `bench` mirrors `test` structurally),
  RFC-0043 (time — `hostMonotonicNanos` is the clock), RFC-0021 (nothing
  gen-specific, but the strip-before-backends discipline is the same)
- **Evidence (user):** "what about built-in tool like divan" — a
  first-class, zero-ceremony benchmarking story in the toolchain, the way
  `test` blocks made testing a habit.

> **Motivation.** Vyrn is a systems language with three backends and no way
> to measure anything. divan's lesson is that benchmarking gets *used* when
> registration is trivial and output is immediately readable — no harness
> project, no manual loop, no statistics homework. One declaration form,
> one builtin, one command. This also lays the measurement rail for
> upcoming performance work (small-buffer collections, RFC-0056).

---

## Surface

```vyrn
import { render } from "./view"

bench "render 1k rows" {
    blackBox(render(1000))
}

bench "push 1k" {
    let mut xs: Array<Int64> = []
    for i in 0..1000 {
        xs.push(i)
    }
    blackBox(xs.length)
}
```

- **`bench "name" { body }`** — a top-level declaration exactly parallel to
  `test`: unique string name per file, body checked as a Unit function body
  with a synthetic unspellable name (`bench@<index>`), so movecheck /
  ownership / spawn purity apply unchanged. Lives in `Program.benches`.
- **Stripped from `run`/`build`/`emit-ir`** exactly like tests: a shipped
  binary contains no benches; string pool / regex collection ignore them.
  Parity is untouched by construction.
- **`blackBox(v)`** — identity with an optimizer-opacity guarantee: the
  value is treated as used and unknowable, so the work producing it cannot
  be deleted and its result cannot be constant-folded. Allowed **only
  inside `bench` and `test` bodies** (checker error elsewhere, same rule
  and wording style as `assert`). Generic: `blackBox<T>(v: T) -> T`.
  - native: the LLVM empty-`asm sideeffect` operand trick (or an opaque
    noinline call if the text-IR path can't express it for a given type).
  - interp: identity (the interpreter doesn't optimize).
  - wasm: same lowering as native through the shared IR.
  The *guarantee* is semantic ("used and unknowable"), not a specific
  instruction sequence.

## `vyrn bench [file] [--name <substring>] [--check]`

- **Default mode compiles NATIVE and times for real.** Timing the
  interpreter is a lie; divan-class numbers mean optimized machine code.
  Requires clang exactly like `vyrn build` (same discovery, same errors).
- Harness (divan-simplified, locked):
  1. per bench: warm up ~50 ms of iterations (discarded);
  2. auto-scale iterations per sample until one sample takes ≥ 1 ms;
  3. collect samples for ~500 ms or ≥ 31 samples, whichever is later
     (cap: 2 s per bench);
  4. report **min / median / mean** per-iteration time with human units
     (`ns`/`µs`/`ms`), plus sample × iteration counts.
  ```
  bench "render 1k rows"   min 41.2 µs   median 42.0 µs   mean 42.6 µs   (48 samples × 24 iters)
  bench "push 1k"          min  1.21 µs  median  1.29 µs  mean  1.33 µs  (52 samples × 812 iters)

  2 benches
  ```
  Declaration order, root file's benches only (a library's benches run when
  it is the argument) — the RFC-0015 rules verbatim.
- Clock: `hostMonotonicNanos` (RFC-0043) — never wall time.
- **`--check`**: run every (filtered) bench body **once** under the
  interpreter and print `bench "name" ... ok` / trap message, deterministic
  output, exit 1 on any trap. This is the CI face of benchmarking (timing
  numbers never appear) and what the harness's own tests pin.
- `--name <substring>` filters; manifest-aware (`vyrn.json` main) like
  every other command.

### Implementation shape (locked at the boundary, free inside)

Bench mode is a **program transform before the backends**: bench bodies
become ordinary functions and a synthesized `main` (the harness: warmup /
auto-scale / sample / stats / printing — plain Vyrn using `std/time`'s
`monotonic()`) replaces the user's. `run`/`build` never see any of it. The
harness's stats/formatting helpers live in a new `std/bench` module
(internal-ish but importable), so its math is unit-testable in Vyrn itself.
How the synthesis is assembled (loader hook vs. dedicated path) is the
implementor's choice; what is NOT free is leaking bench machinery into
non-bench compiles.

## Editor

- Grammar: `bench` as a contextual starter (identifier elsewhere — the
  `test` treatment); name string is a plain string scope.
- CodeLens: "▶ Run bench" above each block → `vyrn bench <file> --name
  "<name>"`; "▶ Run all benches" above the first.
- LSP outline: kind Method, detail `bench "name"` (next to tests).
- Snippet: `bench`.

## Verification

1. `--check` output pinned byte-exact (declaration order, trap
   continuation, exit codes) — the deterministic face.
2. Native run smoke test: a real `vyrn bench` invocation on a two-bench
   file asserts the report SHAPE (regex: names, unit suffixes, counts) —
   never the numbers.
3. `blackBox` placement rules (bench/test-only) + a codegen test that the
   benched work is not deleted (e.g. an empty-loop bench vs a work bench
   differ measurably — shape-level, not threshold-level, to stay
   deterministic-ish; if too flaky, assert the emitted IR retains the call).
4. Strip guarantee: an example with benches is byte-identical across
   backends in the parity corpus (`examples/benching.vyrn` — has `main` +
   benches, demonstrating the form; mirrors `examples/testing.vyrn`).
5. fmt: idempotent formatting of bench blocks; safety invariant holds.
6. Full suite + LSP + parity green, 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy (grammar/CodeLens changed ⇒ extension files too).

## Out of scope

Comparison mode (`vyrn bench --baseline`), allocation counting (divan's
alloc profiler — wants the RFC-0044 storage counters story first),
throughput/bytes-per-second annotations, parameterized benches
(`bench "n" for n in [10, 100]`), wasm-backend timing, and CI regression
tracking. All attractive; none block the habit forming.
