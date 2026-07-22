# RFC-0063 — Benchmarks in CI: `--json`, `--compare`, and the bench job

- **Status:** Locked design
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
