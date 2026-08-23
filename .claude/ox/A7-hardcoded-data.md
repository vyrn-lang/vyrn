# A7 — Everything hardcoded that should be data

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

The owner's instruction: there must be no hardcoded stuff, make everything data
driven. This job finds every place the repository writes a fact into code that
should have come from a file, a manifest, or a generator. It changes nothing.

## What counts as hardcoded

A literal is hardcoded when all three are true:

1. It states a fact about the world, not about the program. A route name, a
   version, a package name, a person, a date, a benchmark result, a navigation
   label, an external URL, a file size, a count.
2. It appears in more than one place, or it will go stale on its own.
3. There is, or could be, a single source for it.

A literal is NOT hardcoded when it is a real constant of the program: a CSS
class name used once, a numeric limit chosen by design, a message the code
itself owns, a magic number with a comment saying why.

Do not report every string in the repository. Report the ones that will be
wrong one day, and say what will make them wrong.

## Where to look

Give each area its own subagent group. A subagent must not spawn a subagent.

| area | what to look for |
| --- | --- |
| `site/export.vyrn` | the route list, redirects, asset list, anything enumerated by hand |
| `site/app/nav.vyrn` | navigation rows, section titles, ordering |
| `site/app/routes/**` | any fact repeated between a `.vyx` file and a data file |
| `site/app/*.vyrn` | version numbers, tag names, counts, dates, external links |
| `site/public/style.css` | colour values that duplicate a token, breakpoints repeated |
| `std/icons.vyrn` | is the icon set generated or typed |
| `std/tw.vyrn` | are class names and breakpoints derived or listed |
| `.github/workflows/*.yml` | tool versions, paths, and counts that also live in `vyrn.json` or `vyrn.lock` |
| `compiler/**` | reserved word lists, builtin name lists, anything enumerated in more than one crate |
| `rfcs/README.md` | the RFC count sentence, which a CI test already checks |

The compiler row matters. A memory of this project records 83 reserved names
across roughly 45 lists. Confirm that number by counting, and list every place a
name appears.

## The row format

| what | where | why it will go stale | the single source it should come from | how it would be read | risk if wrong |

`the single source it should come from` must be a real path, existing or
proposed. `how it would be read` is one of `gen fn at compile time`, `readFile
at build time`, `derived from another value`, or `generated file, committed`.
Vyrn generators can read files and directories at compile time. That is the
mechanism most of these want, and the job should say so where it applies.

`risk if wrong` is `SILENT` or `LOUD`. `LOUD` means a test or a gate already
catches it. `SILENT` means it would ship wrong. Sort the output so `SILENT` comes
first, because those are the ones worth work.

## Also record what is already data driven

A section `Already correct`, listing every place the repository already derives a
fact instead of typing it. This section stops the next reader from re-solving a
solved problem, and it gives the fix pattern for the rest.

## The output

One file: `rfcs/census/hardcoded-data.md`.

1. Counts by area, and counts of `SILENT` against `LOUD`.
2. The full table, `SILENT` first.
3. `Already correct`.
4. `The ten worth fixing first`, ranked by risk times effort, each with the
   smallest change that would fix it. Mark it `RECOMMENDATION, NOT A DECISION`.

## What this job must not do

- Do not fix anything. This job is the survey. The fixes are a separate job, and
  they will conflict with each other if done here.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
