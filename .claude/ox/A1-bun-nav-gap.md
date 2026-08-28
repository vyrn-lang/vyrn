# A1 — Does Vyrn need what Bun documents?

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

Bun ships a large documentation tree. The repository owner wants to know, entry
by entry, whether Vyrn already covers the same ground, should cover it, or has
no business covering it. This job answers that for every entry. It decides
nothing.

## The input

The list of Bun navigation entries is in `.claude/ox/A1-bun-nav.txt`. It has
four sections: Runtime, Package Manager, Test runner, and their sub-headings.
Treat every leaf entry as one row. There are roughly 130.

## The fan-out

Split the leaf entries into 20 to 32 groups by sub-heading, and give one
subagent each group. A subagent must not spawn a subagent.

Each subagent, for each entry in its group:

1. Read what Bun's page for that entry actually covers. Fetch
   `https://bun.com/docs/...` or `https://bun.sh/docs/...` for the entry. If the
   page cannot be fetched, say `PAGE NOT FETCHED` and judge from the title
   alone, marked as such.
2. Search this repository for a Vyrn equivalent. Search `std/`, `compiler/`,
   `rfcs/`, `docs/`, `site/`, and `examples/`. Use the RFC index at
   `rfcs/README.md` as a map.
3. Fill one row.

## The row format

Every row is one line of a Markdown table with these columns:

| Bun entry | what Bun's page covers, one sentence | Vyrn status | evidence | verdict |

`Vyrn status` is exactly one of:

- `HAS` — Vyrn does this today. Evidence must be a `path:LINE` or an RFC number
  that is marked Implemented.
- `PARTIAL` — Vyrn does part of it. Evidence must name what exists and what is
  missing.
- `NONE` — Vyrn does not do this.
- `N/A` — the entry is about JavaScript, npm, Node.js, or Bun's own history, and
  has no Vyrn meaning. Say in one clause why.

`verdict` is exactly one of:

- `GAP` — a Vyrn user would expect this and cannot get it.
- `DOC GAP` — Vyrn has it and does not document it. Give the `std/` path that
  exists and the missing `site/` route.
- `NOT WANTED` — Vyrn deliberately does not want this. Cite the RFC or the
  design note that says so. If no such note exists, use `UNDECIDED` instead.
- `UNDECIDED` — needs the owner's call.

Do not write `GAP` because Bun has a feature. Write `GAP` only when a Vyrn user
writing a Vyrn program would hit the wall.

## The output

Write exactly one file: `rfcs/census/bun-nav-gap.md`.

Structure:

1. One paragraph saying what was compared and on what date.
2. A counts table: how many `HAS`, `PARTIAL`, `NONE`, `N/A`, and how many of
   each verdict.
3. The full table, in Bun's own nav order, with its section headings kept.
4. A section `The twenty largest gaps`, ranked, each with two sentences: what a
   Vyrn user cannot do, and the smallest thing that would fix it. Mark the
   section `RECOMMENDATION, NOT A DECISION`.
5. A section `Documentation gaps` listing every `DOC GAP` row with the `std/`
   path that exists and the site route that should describe it.

## What this job must not do

- Do not add features.
- Do not write RFCs.
- Do not edit `site/`, `std/`, or `compiler/`.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
