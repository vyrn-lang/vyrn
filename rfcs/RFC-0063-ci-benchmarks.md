# RFC-0063 — Benchmarks in CI: `--json`, `--compare`, and the bench job

- **Status:** Implemented
- **Depends on:** RFC-0055 (`vyrn bench` — this ships its deferred "CI
  regression tracking" item), the CI workflow (public repo, free minutes)
- **Evidence (user):** "what about some auto benchmarks for CI?"

Timing on shared runners is noisy; pretending otherwise produces flaky
red builds. The design splits the deterministic part (blocking) from the
timing part (informational tripwire).

---

## 1. `vyrn bench --json`

Machine-readable results to stdout:

```json
{ "backend": "native", "opt": "O2",
  "benches": [ { "name": "push 1000", "minNs": 2070, "medianNs": 2500,
                 "meanNs": 2540, "samples": 383, "iters": 512 } ] }
```

Emitted via `std/json` (RFC-0059 — the harness is Vyrn; it builds a
`Json` tree and `emit`s it). `--json` composes with `--name`; mutually
exclusive with `--check`.

## 2. `vyrn bench --compare <baseline.json> [--threshold <factor>]`

- Runs the benches, then compares each bench's **min** (the stable
  statistic) against the baseline entry by name: regression iff
  `min > baselineMin * threshold` (default `1.5` — generous on purpose;
  hosted-runner noise must not trip it).
- Output: one line per bench (`ok` / `REGRESSED xN.NN` / `new` /
  `missing-from-run`), exit 1 iff any REGRESSED. Missing/new benches are
  informational, never failures.
- Baseline lives at `bench/baseline.json`, committed, refreshed
  deliberately via `vyrn bench --json > bench/baseline.json` on a quiet
  machine (the README-style doc comment in the file says exactly this).
  The seed baseline is generated on CI hardware (a dedicated run whose
  artifact is committed), NOT on the dev machine — comparing across
  machines is meaningless.

## 3. CI wiring

Two additions to the existing workflow, push-to-main only:

- **Blocking (cheap, deterministic):** `vyrn bench --check` over every
  example with benches (`benching`, `smallarray`, and future ones —
  discover by scanning for `bench "` in `examples/*.vyrn`) as a step in
  the existing test job. Deterministic output, no timing.
- **Informational (timing):** a separate `bench` job that builds, runs
  `vyrn bench --compare bench/baseline.json` on the bench corpus, always
  uploads the `--json` output as a build artifact (the trend record),
  and is marked `continue-on-error: true` — a gross regression shows as
  a job failure annotation without blocking the merge. Clang is already
  present in CI; reuse the parity job's caching pattern.

## Verification

1. `--json` output round-trips through `std/json` `parseJson` and is
   stable-ordered (declaration order).
2. `--compare` unit-tested against synthetic baselines: ok / regressed /
   new / missing paths, threshold arithmetic, exit codes. Timing numbers
   themselves are NEVER asserted.
3. `--check` corpus discovery finds exactly the bench-bearing examples.
4. Workflow YAML validated by an actual green run (push) — bench job
   uploads its artifact, blocking steps stay deterministic.
5. Full suite + LSP + parity green; 0 new clippy warnings; no LSP
   redeploy expected (CLI + std only — state the unchanged hash).

## Out of scope

Cross-run trend dashboards, per-PR bench comments, criterion-style
statistical tests, and any blocking timing gate (the noise makes it a
false-positive machine).

---

## As landed

Landed as specified. The JSON is built in Vyrn (the RFC's locked choice), the
comparison core is pure Rust, and CI splits deterministic (blocking) from timing
(informational) exactly as designed.

### `--json` — the Vyrn harness builds the tree

The report is emitted from Vyrn via `std/json`, not reconstructed on the Rust
side from human text (which would be lossy — parsing `41.20 µs` back to ns).
`std/bench` gained:

- **`BenchResult`** — `{ name, minNs, medianNs, meanNs, samples, iters }`.
- **`benchMeasure(name, body) -> BenchResult`** — the divan-simplified loop
  (warmup / auto-scale / sample / stats), factored OUT of `benchOne`, which now
  just calls it and prints the human line. One loop, two faces.
- **`benchJson(results, backend, opt) -> String`** — builds a `JObj` tree
  (`{ backend, opt, benches: [ … ] }`, every number a `JNum` integer, declaration
  order) and `emit`s it compact.

`bench_native` gained `json`/`capture` flags. In `--json` mode the synthesized
harness `main` is a single `print(benchJson([benchMeasure("n0", body0), …],
"native", "O2"))` — the array literal coerces to `Array<BenchResult>` from the
parameter type, so no `let`/`push` synthesis is needed. `std/bench`'s
`import { benchOne }` already pulls the whole module, so `benchMeasure`/`benchJson`
and their transitive `std/json` are merged automatically.

### `--compare` — pure verdict core

`bench_compare` reads+parses the baseline first (a broken baseline is a fast
usage error), runs the benches native with stdout captured (`bench_native(..,
json=true, capture=true)`), parses the report, and delegates to a **pure**
`bench_verdicts(run, baseline, threshold)`: run benches first in declaration
order (`ok` / `REGRESSED xN.NN` where the factor is `min / baselineMin` / `new`),
then baseline-only benches as `missing-from-run`; it returns the regressed count
(exit 1 iff > 0). A regression is the strict `min > baselineMin * threshold`
(default `1.5`); a zero/absent baseline min is `new`, never a division by zero.
The purity is what lets the comparison be unit-tested against synthetic min
tables with **no clang and no real timing**.

`--check` is rejected alongside `--json`/`--compare` (deterministic vs timing).

### Placeholder baseline

`bench/baseline.json` ships a valid-schema placeholder: `placeholder: true`, an
empty `benches`, and a `_readme` array documenting the CI-hardware refresh flow
(JSON has no comments). `baseline_is_placeholder` (flag OR empty `benches`) makes
`--compare` report every bench `new` and exit 0 — comparing real timings against
an unseeded baseline is meaningless. The first REAL baseline is committed by a
human from the `bench` job's `bench-json` artifact.

### CI wiring

- **Blocking** — a `Bench --check` step in the existing `test` job scans
  `examples/*.vyrn` for `bench "`, runs `vyrn bench --check` over each
  (interpreter-only, deterministic, no clang). The corpus today is exactly
  `benching` + `smallarray`, pinned by an integration test so drift shows up.
- **Informational** — a new `bench` job (`if: push && ref == main`,
  `continue-on-error: true`): builds `vyrn`, runs `--json` (→ per-example
  artifacts, always uploaded) and `--compare bench/baseline.json` over the corpus,
  and `exit $regressed` so a gross regression is a job-failure annotation without
  blocking the merge. Reuses the `Swatinem/rust-cache` pattern with its own
  `prefix-key: bench`; clang is preinstalled on `ubuntu-latest`.

### Deviations

- **`serve`-less, so `min` re-run twice in CI.** `--json` (artifact) and
  `--compare` (tripwire) each compile+run the benches, so the `bench` job runs
  each corpus example native twice. Acceptable for an informational push-only
  job; a single-run "emit AND compare" mode was not worth the surface.
- **Comparison output shape.** One `bench "name" ... <verdict>` line per bench
  (mirroring `--check`'s line shape) plus a trailing `N regressed` /
  `no regressions (threshold xF)` summary — the RFC left the exact spelling open.
- **Nothing else deviates.**

### Verification

- Workspace suite: **1033 passed**, 10 ignored (`cargo test --workspace`) — up
  from 1027, +6 (the `bench_verdicts`/`bench_min_table`/placeholder unit tests, a
  corpus-discovery test, `--json`/`--compare` native integration tests behind
  `#[ignore]`, and a mutual-exclusion test).
- `std/bench` Vyrn tests: **5 passed** (`vyrn test std/bench.vyrn`) incl. the
  locked-schema + `parseJson` round-trip + stable-order tests.
- Native `--json`/`--compare` (clang) integration tests pass
  (`cargo test -p vyrn-cli --test benching -- --ignored`).
- Three-way parity green (`examples/benching.vyrn` still byte-identical —
  `std/bench` is bench-mode-only, never in a run/build compile).
- `vyrn fmt --check` clean on `std/bench.vyrn` + examples; 0 new clippy warnings
  (all pre-existing, in `vyrn-codegen`/`vyrn-frontend`, none in `vyrn-cli`).
- No LSP change from RFC-0063 (CLI + std only) — the deployed `vyrn-lsp.exe` hash
  is untouched by this RFC.
