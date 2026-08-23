# A9 — Where the CI minutes go

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

The owner wants every CI job under one minute. Today the Site job takes about
ten minutes and the CI workflow takes longer. Before anything is cut, this job
measures where the time actually goes, step by step, across the recent history.

It changes no workflow file. The cuts are the owner's call, because some of
these steps are gates the owner put there on purpose.

## Do not arm a watcher

Read finished runs. Do not wait for a run. Do not poll. See
`.claude/ox/RULES.md`.

## Part one — the step-level record

Use the GitHub CLI, which is authenticated in this environment.

For the last 20 completed runs of each of the three workflows in
`.github/workflows/` — `ci.yml`, `site.yml`, `release.yml` — pull every job and
every step with its start and end time.

```
gh run list --workflow ci.yml --status completed --limit 20 --json databaseId,headSha,conclusion,createdAt
gh api repos/vyrn-lang/vyrn/actions/runs/<id>/jobs --paginate
```

The jobs endpoint gives `steps` with `started_at` and `completed_at`. Compute
each step duration in seconds.

Note: `gh` returns the full 40-character `headSha`. Do not filter on a short
sha.

Build one row per step, per job, per operating system:

| workflow | job | os | step name | runs sampled | median seconds | p90 seconds | max seconds |

Sort by median descending. This table is the deliverable. Everything else is
commentary.

## Part two — what each expensive step is for

For every step whose median is over 30 seconds, read the workflow file and its
comments. The workflow files in this repository carry long comments explaining
why each step exists. Do not propose removing a step without quoting the comment
that justifies it.

One row each:

| step | median seconds | what it proves | what would go unnoticed if it were removed | is it cached today | what its cache key is |

## Part three — the cheap wins

Classify every expensive step into exactly one:

- `CACHEABLE` — the same inputs produce the same result and no cache exists, or
  the cache key is wider than it needs to be. State the correct key.
- `PARALLELISABLE` — it is a serial loop over independent items. State how many
  items and what the wall time would be at four-way and eight-way.
- `REDUNDANT` — another step already proves it. Name that step.
- `MISPLACED` — it runs in a job that does not need it, or it runs before a
  cheaper step that would have failed first.
- `IRREDUCIBLE` — it is a compile or a download and the only lever is doing less
  of it.

For `CACHEABLE` and `PARALLELISABLE`, estimate the saving in seconds and say how
you got the number.

## Part four — the arithmetic the owner asked for

The target is one minute per job. For each job, produce:

| job | median total | sum of IRREDUCIBLE steps | is one minute reachable | what would have to go |

Answer `is one minute reachable` with `YES`, `NO`, or `ONLY IF`, and be blunt.
If a job downloads a wasm sysroot and compiles a Rust workspace, one minute is
not reachable and the file should say so plainly with the number that proves it.
An honest `NO` with a floor is more useful than an optimistic plan.

## Part five — the caches that already exist

List every `actions/cache` and every `Swatinem/rust-cache` in the three
workflows, with:

| cache | key | what it holds | hit rate over the sampled runs | size |

Hit rate comes from the run logs. A cache that misses is worse than no cache,
because it pays the upload. Flag any that miss more than a quarter of the time.

## The output

One file: `rfcs/census/ci-step-timings.md`, with the five parts above and a
final section `The ten changes with the best seconds per unit of risk`, ranked.
Mark it `RECOMMENDATION, NOT A DECISION`.

## What this job must not do

- Do not edit any file in `.github/`.
- Do not push a commit that triggers CI in order to measure it. Read history.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
