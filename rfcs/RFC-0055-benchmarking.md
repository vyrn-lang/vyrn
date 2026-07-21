# RFC-0055 — Benchmarking: `bench` Blocks, `blackBox`, `vyrn bench`

- **Status:** Implemented
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

---

## As landed

Landed exactly as specified. `bench` is a `test` twin end to end.

### `bench` blocks — a parallel field, stripped by construction

`Program` gains `benches: Vec<BenchDecl>` next to `tests`, and `BenchDecl` is a
field-for-field clone of `TestDecl` (`name`/`body`/`doc`/`module`/`line`). Every
place that special-cased tests now handles benches identically:

- **parser** — `bench_decl` mirrors `test_decl`; the top-level dispatch and
  `sync_to_decl` recognize `bench` as a contextual starter *only* directly before
  a string literal (`bench "…"`), so `let bench = 1` stays an ordinary binding.
- **checker** — `check_benches` type-checks each body as a synthetic
  `bench@<index>` Unit function (`in_bench` set), reports duplicate names per
  module, and extends the no-`main` library exemption (`exports OR tests OR
  benches ⇒ no main`).
- **movecheck / ownership** — synthetic `bench@<index>` functions, exactly the
  test path.
- **loader** — benches are module-tagged, merged (imported benches keep their
  tag), and namespace-rewritten (`walk_block` / `rewrite_module_refs` /
  `program_ref_names`) like test bodies.
- **codegen** walks only `functions`, so `run`/`build`/`emit-ir` never see a
  bench — the strip guarantee is free, and an integration test pins that neither
  a bench's marker string nor an `asm sideeffect` barrier leaks into a non-bench
  compile.
- **symbols/LSP** — each `bench "name"` is an outline `Method` with detail
  `bench "name"`; the LSP is a pure adapter, so the outline picked it up with no
  LSP-code change.

### `blackBox` — one builtin, gated, lowered per backend

`blackBox` joins the reserved/builtin names. The checker allows it **only** when
`in_test || in_bench` (assert/assertEq stay test-only), types it as identity
(`blackBox<T>(v: T) -> T`), and otherwise errors with `` `blackBox` is only
available inside a `bench` or `test` block ``. The interpreter is plain identity.
Codegen (in `gen_call`, before `print`) lowers it by the value's LLVM class:

- register-class (`i64`/`iN`/`double`/`float`/`i1`/`ptr`): an identity inline-asm
  that ties output to input — `call TY asm sideeffect "", "=r,0"(TY %v)` — so the
  optimizer treats the result as an unknown function of the input (divan's
  `black_box`);
- aggregate (`{…}`/`[…]` — record/array/Ref/…): a round-trip through an
  entry-block slot with a `~{memory}` clobber, so the store can't be dead and the
  reload can't be folded.

The guarantee is semantic ("used and unknowable"), verified at the IR level
(both barriers asserted present in emitted IR) rather than by a flaky timing
threshold.

### `vyrn bench` — a program transform before the backends

`bench_cmd` splits into `bench_check` and `bench_native`. **`--check`** calls the
new `interp::run_benches` (a `run_tests` twin): each selected root bench body runs
once, `bench "name" ... ok` / `... FAILED: <trap>` prints in declaration order, a
trap continues to the next bench, and any failure exits 1.

**Default (native)** is the transform the RFC locks at the boundary:

1. Pull in the harness runtime by loading a synthetic root (`import { benchOne }
   from "std/bench"`) and merging every module-tagged decl it brings —
   `std/bench` + its transitive `std/time` — into the loaded program, skipping any
   name the program already has (so a bench file that itself imports `std/time`
   doesn't double-define `monotonic`).
2. Lift each selected root bench body into an ordinary `__vyrn_bench_body_<slot>`
   Unit function (`blackBox` inside is fine — the program is already checked, and
   codegen is reached without re-checking).
3. Replace the user's `main` with a synthesized harness that calls
   `benchOne("name", <width>, __vyrn_bench_body_<slot>)` per bench (the body passed
   as a monomorphized RFC-0023 function value) then prints the footer.
4. Emit IR, link the shim, and `clang -O2` into a temp dir, then run it. clang
   discovery/errors are `vyrn build`'s verbatim.

The harness math and printing live in the new **`std/bench`** module —
`minOf`/`median`/`mean`/`formatDuration`/`padRight` are pure and unit-tested in
Vyrn (`test` blocks, `vyrn test std/bench.vyrn`), and `benchOne` runs the
divan-simplified loop (warmup 50 ms → auto-scale to ≥ 1 ms/sample → sample to
≥ 31 && ≥ 500 ms, cap 2 s) over `std/time`'s `monotonic()`.

### Deviations

- **Summary line wording.** `--check` prints `N ok, M failed` (not `passed`) —
  a bench "passing" `--check` only means it ran without trapping, so "ok"
  matches the per-line verb and avoids implying a correctness assertion. The
  `no benches` empty case and the exit codes are the test path verbatim.
- **`-O2` on the bench link.** `vyrn build` links unoptimized; `vyrn bench` adds
  `-O2` because divan-class numbers *are* optimized machine code (the whole point
  of compiling native). This is what makes `blackBox` load-bearing rather than
  cosmetic.
- **Nothing else deviates.** The one thing worth flagging for users (not a
  deviation, a property): at `-O2`, `blackBox` on an *output* only stops the
  result being folded — a benched pure function of a compile-time-constant input
  is still constant-folded (and LLVM's scalar evolution even closes-forms a plain
  `0..n` sum). `examples/benching.vyrn` demonstrates the idiom — `blackBox` the
  input too, and use a data-dependent recurrence — with an inline comment.

### Sample output

`vyrn bench examples/benching.vyrn` (numbers vary; shape is pinned by a regex
smoke test):

```text
bench "hash to 1000"   min 2.73 µs   median 2.75 µs   mean 2.78 µs   (351 samples × 512 iters)
bench "push 1000"      min 2.09 µs   median 2.78 µs   mean 2.75 µs   (354 samples × 512 iters)

2 benches
```

`vyrn bench examples/benching.vyrn --check` (byte-pinned, deterministic):

```text
bench "hash to 1000" ... ok
bench "push 1000" ... ok

2 ok, 0 failed
```

### Verification

- Workspace suite: **967 passed**, 6 ignored (`cd compiler && cargo test
  --workspace`) — up from 953, +14 (8 `vyrn bench` integration tests incl. the
  ignored native smoke, 2 codegen barrier tests, 3 parser/symbols unit tests, 1
  `std/bench` `vyrn test` covered by the CLI harness).
- LSP suite: **40 passed**, 1 ignored (`cd compiler/vyrn-lsp && cargo test`).
- Three-way parity: **75 checked, 0 failed** (`cargo test -p vyrn-cli --test
  parity -- --ignored`) — `examples/benching.vyrn` (main + benches) is
  byte-identical across interp/native/wasm because benches strip.
- Native smoke + `--check` byte-exact (incl. `array index 0 out of bounds` trap
  continuation and exit 1) both pass; blackBox placement + IR-barrier tests pass.
- `vyrn fmt --check` clean across all `examples/` + `std/`; fmt idempotent on
  bench blocks; 0 new clippy warnings (54 pre-existing locations untouched).
- `vyrn-lsp` release rebuilt and redeployed to
  `editor/vscode/server/vyrn-lsp.exe`; SHA-256 pair matches:
  `fedb5d3e9d6152b10b9ace4fe399e7ed0c62c288583ae248b88d9510baca4141`.
  Grammar (`bench` contextual starter), snippet (`bench`), and CodeLens
  ("▶ Run bench" / "▶ Run all benches" → `vyrn bench <file> --name "<name>"`)
  updated in the extension.
