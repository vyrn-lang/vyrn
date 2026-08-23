# Ox jobs

Prompts for a main agent that can run many subagents. Every job is bulk work:
reading, measuring, tabulating. None of them decides anything about the
language, the standard library, or the product. Those choices come back to the
repository owner.

Read `RULES.md` before any job. It is referenced by every prompt and it
overrides tool defaults.

## Run them in waves

**Wave A is safe to run all at once.** Nine jobs, each read-only on the
repository, each writing to its own new file under `rfcs/census/`. They cannot
collide.

**Wave B edits `site/`. Run one at a time.** Three jobs, three branches, three
pull requests. Two of them editing the same `.vyx` file at the same time will
conflict and one will lose work.

**Wave C is measurement that feeds a later decision.** Safe to run beside
wave A.

## Wave A — censuses

| job | question | writes |
| --- | --- | --- |
| `A1-bun-nav-gap.md` | Does Vyrn need what Bun documents? 114 entries. | `rfcs/census/bun-nav-gap.md` |
| `A2-std-quality.md` | Every `std/` module against thirty quality axes. 38 modules. | `rfcs/census/std-quality/` |
| `A3-ui-prior-art.md` | reka-ui, nuxt-ui, nuxt/hints, html-validate. | `rfcs/census/ui/` |
| `A4-reactivity.md` | Vue and Nuxt reactivity, and what Vyrn can and cannot get wrong. | `rfcs/census/reactivity.md` |
| `A5-profilers.md` | Five profilers, their formats, their editor integration. | `rfcs/census/profilers.md` |
| `A6-language-questions.md` | Prior art for six open language questions. | `rfcs/census/lang/` |
| `A7-hardcoded-data.md` | Every fact typed into code that should come from data. | `rfcs/census/hardcoded-data.md` |
| `A8-std-api-smells.md` | Call shapes that read wrong, and silent byte traps. | `rfcs/census/std-api-smells.md` |
| `A9-ci-timings.md` | Where the CI minutes go, step by step. | `rfcs/census/ci-step-timings.md` |

A2, A8 and A9 are the ones most likely to change what gets built next. A2
because a defect in three modules is a design problem. A8 because it looks for
live bugs, not opinions. A9 because it answers whether one minute per job is
reachable at all.

## Wave B — the site, one at a time

| job | branch | writes |
| --- | --- | --- |
| `B1-prose-sweep.md` | `ox/prose-sweep` | edits every page, report to `rfcs/census/prose-sweep.md` |
| `B2-social-metadata.md` | `ox/social-metadata` | metadata for every route, report to `rfcs/census/social-metadata.md` |
| `B3-visual-defects.md` | `ox/visual-defects` | five named defects, report to `rfcs/census/visual-defects.md` |

Order: B3, then B1, then B2. B3 fixes broken things. B1 removes text. B2 derives
metadata from the text B1 leaves behind, so it must run last.

## Wave C — benchmarks

| job | question | writes |
| --- | --- | --- |
| `C1-benchmark-gaps.md` | Where each of the eight programs loses, and to what. | `rfcs/census/benchmark-gaps.md` |
| `C2-blocked-benchmarks.md` | What would unblock regex-redux and mandelbrot. | `rfcs/census/blocked-*.md` |

## Running one

```bash
omp -p --cwd N:/lang "@.claude/ox/A1-bun-nav-gap.md"
```

**Use the default model, `ox-alpha`. Nothing else.** Do not pass `--model`.

Both GLM routes are closed and each closed for its own reason, measured on
2026-08-23:

- `glm-5` on the opencode provider returns `429 5-hour usage limit reached`. It
  is a rolling five-hour budget on the workspace and it is spent. Not to be used
  regardless.
- `z-ai/glm-5.2:free` on OpenRouter returns `429` with
  `limit_source: upstream_provider_shared_pool`. The capacity is shared with
  every other user of that free endpoint, so there is no quota to plan around.
  Measured: eight single requests, one every fifteen seconds, zero succeeded.

Ox is slower and less capable than GLM but it has separate capacity and it has
completed five of these jobs already. Three concurrent Ox jobs ran to completion
without a rate limit.

Stagger launches by 20 seconds. Run at most four at a time.

### Two jobs must run alone

`A2-std-quality` and `C1-benchmark-gaps` both time code with `vyrn bench`. Any
other job running beside them competes for the processor and corrupts their
numbers. Run each of those two by itself, with nothing else in flight.

`A8-std-api-smells` and `C2-blocked-benchmarks` run programs but do not time
them, so they may run beside other jobs.

## What is not here, and why

These questions are the repository owner's and no job may answer them:

- Which lambda syntax replaces `|x| ...`.
- Whether a union member can be selected by a fixed property value, and what
  that means for `Array<A | B>`.
- Whether Vyrn gets coroutines, and stackful or stackless.
- Whether Vyrn gets attributes, and whether they run a `gen fn`.
- Whether a validated type can discharge another type's check.
- Whether operators can be overloaded.
- Whether `joinWith(array, sep)` becomes `array.join(sep)`, and what that breaks.
- Whether a standard library string operation may work in bytes without saying
  so in its name.

Wave A gathers the evidence for every one of these. The choices happen after.
