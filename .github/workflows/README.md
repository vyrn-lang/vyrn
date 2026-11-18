# The three workflows, and what to do with them

This file is for someone who has just arrived. The workflow files themselves
carry long comments explaining why each step exists; this one explains what the
three are for, when they run, and how to do the jobs people actually need to do.

## The three

| workflow | what it proves | when it runs |
| --- | --- | --- |
| `ci.yml` | the compiler builds, the tests pass, and the wasm and native runs agree | every pull request, every push to `main` that changes more than Markdown or `editor/`, and on demand |
| `site.yml` | the site builds and every route answers | on a pull request, after CI passes on `main`, when a release is published or edited, daily, and on demand |
| `release.yml` | a tagged commit becomes a published release with binaries | when a tag starting with `v` is pushed |

`site.yml` runs on a pull request **before** a merge, not only after. That is
deliberate: `ci.yml` never touches `site/`, so without the pull request trigger a
broken site would only be found once it was already on `main`.

`ci.yml` can also be started by hand. That exists for one case: `release.yml`
refuses to publish a tag whose commit has no successful CI run, and a docs-only
commit does not trigger CI at all. Start CI by hand against the tagged commit
and the release goes through.

## How to update the benchmarks

The `benchmarks` job runs on a push to `main` only. It builds the commit the
push replaced and the pushed commit, times both on the same runner, and fails
when a bench's minimum grows past `BENCH_THRESHOLD` over the base's. A bench
named in `bench/ungated.txt` is timed and reported, never failed.

`bench/baseline.json` is the recorded trend. Each run compares against it and
prints the result, and that comparison never fails the job. To reseed it:

1. Push to `main`; a run started by hand skips this job. The `benchmarks` job
   runs and uploads an artifact called `bench-json`.
2. Download it. It contains a seedable baseline assembled from that run.
3. Replace `bench/baseline.json` with it and commit.

The comparison runs per example, not over the whole corpus, so every example's
run reports the other examples' entries as `missing-from-run`. That verdict is
deliberately not counted as a regression. The side effect is that **a deleted benchmark is
invisible**, and no threshold or setting fixes that. Making the comparison
whole-corpus is the change that would.

Benchmark numbers are only comparable against the hardware line they were taken
from. A baseline seeded on one runner does not describe another.

## How long a run takes, and why

Two results from timing real runs are worth knowing before anyone tries to
speed things up.

**One minute per job is not reachable.** The floor is not the workflow. It is
Vyrn executing test corpora — the site's own test blocks in one case, forty
route programs in another. No change to a YAML file reaches it.

**What the site's test step is made of is one command away.**
`vyrn test --profile site/export.vyrn` prints the time of each compile phase of
the step, and `vyrn run --profile` counts the operations of each function.

**There are two bench gates and they check different things.**
`Bench --check`, in the `checks` job, runs every bench body once, compiled. It
never loads the bench harness at all, so nothing it does
exercises the native timing path. The `benchmarks` job below does run that path.
Knowing which is which matters: a defect lived in the harness merge for a long
time and neither gate saw it, because the corpus is the project's own files and
the defect was triggered by a name a user picks. `The native bench harness` step
in the `route` job is the one that tests that, and it picks the names on
purpose.

## If a run fails and you did not expect it

Check these first, in this order. Each is cheap and each has caught a real
failure here.

1. **The formatting gate.** It runs first because it takes seconds and its fix
   is mechanical. Three manifests are checked, not one: two crates are excluded
   from the workspace and `cargo fmt` at the root never reaches them.
2. **The docs drift gate.** The committed standard library documentation under
   `docs/api/` must match what `vyrn doc` generates. Change a `///` comment in
   `std/` and you regenerate it in the same commit.
3. **Path separators.** This project is developed on Windows and the CI runs on
   Linux, macOS and Windows. A backslash written into a test path passes locally
   and fails on the other two. That has happened.

## What is not here

Nothing in these workflows deploys the compiler, publishes a package, or writes
to any service other than this repository and its GitHub Pages site.
