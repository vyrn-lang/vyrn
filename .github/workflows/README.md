# The four workflows, and what to do with them

This file is for someone who has just arrived. The workflow files themselves
carry long comments explaining why each step exists; this one explains what the
four are for, when they run, and how to do the jobs people actually need to do.

## The four

| workflow | what it proves | when it runs |
| --- | --- | --- |
| `ci.yml` | the compiler builds, the tests pass, and the three backends agree | every push, and on demand |
| `docs.yml` | `rfcs/README.md` agrees with `rfcs/` | every pull request, docs-only ones included; `ci.yml` skips those |
| `site.yml` | the site builds and every route answers | on a pull request, after CI passes on `main`, and when a release is published |
| `release.yml` | a tagged commit becomes a published release with binaries | when a tag starting with `v` is pushed |

`site.yml` runs on a pull request **before** a merge, not only after. That is
deliberate: `ci.yml` never touches `site/`, so without the pull request trigger a
broken site would only be found once it was already on `main`.

`ci.yml` can also be started by hand. That exists for one case: `release.yml`
refuses to publish a tag whose commit has no successful CI run, and a docs-only
commit does not trigger CI at all. Start CI by hand against the tagged commit
and the release goes through.

## How to update the benchmarks

Read this part before trusting the benchmark gate.

**The gate does not catch regressions today.** `bench/baseline.json` is a
placeholder: it carries `placeholder: true` and an empty `benches` list, so
`vyrn bench --compare` reports every benchmark as `new` and exits 0. What the
gate proves is that every benchmark still builds, runs, and produces a report.
That is worth having and it is not what the name suggests.

To turn the regression half on, seed the baseline:

1. Push to `main`, or start `ci.yml` by hand. The `benchmarks` job runs and
   uploads an artifact called `bench-json`.
2. Download it. It contains a seedable baseline assembled from that run.
3. Replace `bench/baseline.json` with it and commit.

One thing to know before you do: the comparison runs per example, not over the
whole corpus. Once a baseline exists, every example's run will report the other
examples' entries as `missing-from-run`. That verdict is deliberately not
counted as a regression. The side effect is that **a deleted benchmark is
invisible**, and no threshold or setting fixes that. Making the comparison
whole-corpus is the change that would.

Benchmark numbers are only comparable against the hardware line they were taken
from. A baseline seeded on one runner does not describe another.

## How long a run takes, and why

A step-by-step timing of all three workflows is in
`rfcs/census/ci-step-timings.md`, taken from real runs. Two results from it are
worth knowing before anyone tries to speed things up.

**One minute per job is not reachable.** The floor is not the workflow. It is
the Vyrn interpreter executing test corpora — the site's own test blocks in one
case, forty parity programs in another. No change to a YAML file reaches it.

**What the site's test step is made of is now known.** Run
`vyrn test --profile site/export.vyrn` and it answers in one command: `slice`
from `std/strpred` is 59 percent of it over 508,664 calls, and `findSkipping` is
another 15. Two functions are three quarters of the step. There is nothing to
tune in the workflow and the write-up is
`rfcs/census/slice-is-half-the-site-build.md`.

**There are two bench gates and they check different things.**
`Bench --check`, in the `checks` job, runs every bench body once under the
interpreter. It never loads the bench harness at all, so nothing it does
exercises the native timing path. The `benchmarks` job below does run that path.
Knowing which is which matters: a defect lived in the harness merge for a long
time and neither gate saw it, because the corpus is the project's own files and
the defect was triggered by a name a user picks. `The native bench harness` step
in the `parity` job is the one that tests that, and it picks the names on
purpose.

**Read the correction at the end of that file before quoting a number from it.**
Its medians were sampled across runs that mostly predate a site optimisation, so
the site figures in the body are roughly twice what the workflow costs today.

## If a run fails and you did not expect it

Check these first, in this order. Each is cheap and each has caught a real
failure here.

1. **The formatting gate.** It runs first because it takes seconds and its fix
   is mechanical. Three manifests are checked, not one: two crates are excluded
   from the workspace and `cargo fmt` at the root never reaches them.
2. **The docs drift gate.** The committed standard library documentation under
   `docs/api/` must match what `vyrn doc` generates. Change a `///` comment in
   `std/` and you regenerate it in the same commit.
3. **The RFC index.** `rfcs/README.md` states a count, a range, and the gaps in
   the numbering, and a test checks all three against the directory. Adding an
   RFC means updating that sentence and adding its row.
4. **Path separators.** This project is developed on Windows and the CI runs on
   Linux, macOS and Windows. A backslash written into a test path passes locally
   and fails on the other two. That has happened.

## What is not here

Nothing in these workflows deploys the compiler, publishes a package, or writes
to any service other than this repository and its GitHub Pages site.
