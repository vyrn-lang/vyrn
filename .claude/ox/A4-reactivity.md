# A4 — Reactivity in Vue and Nuxt, and what Vyrn does instead

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

Describe how Vue and Nuxt make a view update when data changes, list the ways
that model goes wrong for real users, then describe what Vyrn does today and
which of those failures it can and cannot have. The owner will decide what to
change. This job decides nothing.

## Part one — Vue and Nuxt, from the source and the documentation

Cover, each with a citation:

- The reactive primitives: `ref`, `reactive`, `computed`, `watch`,
  `watchEffect`, `shallowRef`, `toRefs`, `markRaw`.
- How the dependency graph is built and when it is torn down.
- What triggers a re-render, and what batches those triggers.
- Where the Proxy-based tracking cannot see a change. Array index assignment,
  `Map` and `Set`, class instances, and getters are the known cases. Confirm
  each from the documentation or the source, and cite it.
- Server-side rendering and hydration: what state crosses the boundary, how it
  is serialised, and what a hydration mismatch is.
- Nuxt on top: `useState`, `useAsyncData`, `useFetch`, payload serialisation,
  and the shared-state-between-requests hazard on the server.

## Part two — how it fails

Search the Vue and Nuxt issue trackers, and Stack Overflow, for the failures
users actually hit. Group them into classes. For each class:

| class | what the user sees | root cause | the documented workaround | could a compiler have caught it |

Include at least: lost reactivity from destructuring, a stale closure in a
watcher, a reactive object leaking between server requests, a hydration mismatch
from a non-deterministic render, an infinite update loop, and a memory leak from
an effect that is never stopped.

## Part three — Vyrn today

Read these before writing this part:

- Any RFC about the UI layer, plus `std/ui.vyrn`, `std/vyx.vyrn`,
  `std/html.vyrn`.
- The RFCs numbered 0067 and 0068, for soft navigation and validation.
- The RFCs numbered 0071 through 0075, for module contracts.
- The browser runtime in `web/` and the DOM differ, `vyrn-dom.js`.
- The components the site itself uses, under `site/app/routes/**/*.vyx`.

Answer, each with a line citation:

- What is the unit of state in a Vyrn page?
- What causes a re-render?
- Is there a dependency graph at all, or does the whole view recompute?
- Where does the diff happen, and what does the host do with it?
- How does state survive a soft navigation?
- What crosses the server-to-browser boundary, and in what format?

## Part four — the comparison

One table. One row per failure class from part two. Columns:

| failure class | can Vyrn have it | why | evidence |

`can Vyrn have it` is `IMPOSSIBLE BY CONSTRUCTION`, `POSSIBLE`, or `ALREADY
HAPPENS`. `ALREADY HAPPENS` must cite the file and line where it happens, or a
test that reproduces it. A claim of `IMPOSSIBLE BY CONSTRUCTION` must name the
language rule that makes it impossible.

Then a short section `What Vyrn pays for that`. A model that cannot have stale
closures may recompute more. Measure it with `vyrn bench` if you can. Write `NOT
MEASURED` if you cannot.

## The output

One file: `rfcs/census/reactivity.md`. Four parts in the order above, plus a
final section `Open questions for the owner`, which lists the choices this
census surfaced without answering them. Mark it `RECOMMENDATION, NOT A
DECISION`.

## What this job must not do

- Do not change `std/ui.vyrn`, `std/vyx.vyrn`, or anything in `web/`.
- Do not write an RFC.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
