# Prose sweep — cutting the noise out of every page

Branch `site/prose`. Nine commits, `d01830cc` to `5c9e304c`.

The three complaints this job answers: sentences that explain a mechanism to a
reader who came for a command; text that is dense and says nothing; RFC numbers
and other internal references a reader outside the project cannot use.

## The consumer/backstage split

`site/export.vyrn:54` (`sitePaths`) is the whole published route list. It names
`rootPath()`, `whyVyrnPath()`, `installPath()`, `toolingEditorsPath()`,
`docsGraphPath()`, `releasesPath()`, `docsPath()`, `guidePath()`, `webPath()`,
`toolingPath()`, `benchmarksPath()`, `explorePath()`, `playPath()`, the two
redirect stubs, one page per `std/` module, one per package, and one per
chapter. Eighty routes.

**Every published route is consumer.** `site/app/backstage.vyrn` still exists and
still has four passing test blocks, but `site/export.vyrn` does not import it and
no path in `sitePaths()` reaches it. `grep -rl backstage out --include=*.html`
returns nothing. So the exemption had nothing to apply to, and the RFC rule
applies to the whole site.

The classification, route by route:

| Routes | Classified |
| --- | --- |
| `/`, `/why-vyrn`, `/install`, `/releases`, `/play`, `/benchmarks`, `/explore`, `/explore/[package]` | consumer |
| `/docs`, `/docs/graph`, `/docs/std/[module]` (38) | consumer |
| `/guide`, `/guide/[chapter]` (12), `/web`, `/web/[chapter]` (4), `/tooling`, `/tooling/[chapter]` (3), `/tooling/editors` | consumer |
| `/philosophy`, `/compare`, `/editors`, `/guide/testing`, `/guide/web`, `/docs/editors` | consumer (redirect stubs) |
| `/backstage`, `/backstage/rfcs/*` | backstage — **not published**, not touched |

## 1. Words before and after

`wc -w` equivalent over the rendered text of every `out/**/*.html`, with
`<script>`, `<style>`, `<template>` and `<head>` removed and tags stripped.

| Section | Before | After | Delta |
| --- | ---: | ---: | ---: |
| root pages (`/`, `/install`, `/why-vyrn`, `/releases`, `/play`, `/benchmarks`, stubs) | 4810 | 4679 | −131 |
| `explore/` | 1726 | 1392 | −334 |
| `docs/` (landing and graph) | 1381 | 1344 | −37 |
| `docs/std/` (38 pages, generated from `std/`) | 56200 | 52647 | −3553 |
| `guide/` | 11820 | 11409 | −411 |
| `web/` | 1923 | 1879 | −44 |
| `tooling/` | 3384 | 2933 | −451 |
| **TOTAL** | **81244** | **76283** | **−4961 (−6.1%)** |

The `docs/std/` figure is one template edit multiplied by 38 pages. The largest
single page cut is `/tooling/editors`, 1655 → 1259 words (−24%).

## 2. Density

Measured over the rendered text with the same tool before and after.

| | Before | After |
| --- | ---: | ---: |
| Paragraphs over four sentences | 8 | 8 |
| Sentences over 25 words | 528 | 493 |
| Sentences over 25 words, excluding `docs/std/` | 93 | 57 |
| `RFC-00NN` in HTML | 315 | 303 |
| `RFC-00NN` in HTML outside `docs/std/` and `docs.html` | 12 | 0 |

All 8 over-long paragraphs are on `docs/std/` pages and come from `std/` doc
comments. The remaining 57 long sentences outside `docs/std/` are mostly a
limitation of the checker, not of the prose: it splits on a full stop followed by
a capital, so a sentence that starts with a `<code>` span reads as a continuation
of the one before it. Spot-checking every item over 30 words found two real ones,
both of which were split.

## 3. RFC references removed

**12 removed, on 2 pages.** Both were the same list.

| Page | What carried it |
| --- | --- |
| `/` (`site/app/routes/index.vyx:463`) | `Latest release` → a list of the three newest design records, each rendered as `RFC-0108` with a status word |
| `/releases` (`site/app/routes/releases.vyx:162`) | `Design records since the tag` → the same list, uncapped |

Nothing on this site publishes a design record, so the number was a label a
reader could not follow. `site/app/history.vyrn:700` builds the row with
`rfcTag(number)`, which is the string `RFC-0108` and nothing else — there is no
title in that row to fall back to. Both lists are gone, along with
`highlights()`, `sinceRows()` and the now-unused `arrivalList` imports.

### The 303 that remain, and why they are not in this job

Every one is on `/docs` or a `/docs/std/*` page, and every one is written in a
`///` doc comment in `std/*.vyrn`:

```
$ grep -rlE "RFC-[0-9]{4}" out --include=*.html | grep -v "^out/docs"
(nothing)
$ grep -n "RFC-0045" std/hash.vyrn
1:/// std/hash — non-cryptographic byte hashing (RFC-0045).
```

`site/app/apidoc.vyrn` lexes `std/*.vyrn` and renders those comments as the page.
The job scopes the removal to `site/` and forbids changing Vyrn code. Three
further reasons to leave them to the owner rather than take them here:

- The source count is 629 `RFC-` mentions across 38 `std/` modules
  (`grep -c RFC- std/*.vyrn`), of which about 265 reach a page. Many are load
  bearing sentences (`WRITER (RFC-0059): the emitted openapiJson() no longer …`),
  not detachable citations.
- `docs/api/*.md` is generated from the same comments and CI has a docs drift
  gate (`.github/workflows/ci.yml:126`). Editing the comments without
  regenerating that directory turns CI red.
- It also carries into `out/**/*.md` (252 occurrences) and `out/search.json`
  (33), which are the same text through other doors.

**RECOMMENDATION, NOT A DECISION.** A second job over `std/` doc comments plus a
`vyrn doc` regeneration would close it. It is a bigger job than it looks.

## 4. Links added

Rule 2 — a true fact the reader does not need right now, replaced with a link to
the page that owns it. Link text names the thing, never the action. No hover
tooltip was added anywhere.

| Source page | Link text | Target | Replaced |
| --- | --- | --- | --- |
| `/explore/[package]` | Projects and dependencies | `/tooling/projects.html` | "Fetches once, pins the sha256 in `vyrn.lock`, writes the alias into `vyrn.json`. The ref is `main`; a release tag works the same." |
| `/explore` | Projects and dependencies | `/tooling/projects.html` | "`vyrn add` pins its sha256 in `vyrn.lock`" |
| `/docs/std/[module]` (38 pages) | The import graph | `/docs/graph.html` | "Read from the module's own `import` lines while this page was built." (link text was "The whole graph") |
| `/benchmarks` | Testing and bench | `/tooling/testing.html` | "their baseline is an unseeded placeholder" |
| `/tooling/editors` | Projects and dependencies | `/tooling/projects.html` | "Remote imports are pinned in `vyrn.lock` and cached by sha256 under `~/.vyrn`" |
| `/guide/modules` | Projects and dependencies | `/tooling/projects.html` | "`vyrn add`, `vyrn update` and `vyrn vendor` manage them." |

Every one of these is checked by the export's own link gate, which is why the
rule is safe: a link to a route that does not exist fails
`vyrn run site/export.vyrn out`.

## 5. Facts with no home

True facts deleted under rule 3, because no page on this site owns them. The
owner decides whether any of these deserve a page.

1. **`/`, `/releases`** — the design records that arrived after the newest tag,
   with the status each holds (`Implemented`, `Partly`). The site publishes no
   design record, so there is no page to link the number at.
2. **`/explore/[package]`** — how the specifier picks its module: the one with
   the most exports that the project does not itself build from.
3. **`/explore/[package]`** — each imported file arrives with a lock line of its
   own. `/tooling/projects` covers `vyrn.lock`, not the per-file rule.
4. **`/explore/[package]`** — the `target` column in a project's manifest is a
   capability declaration, not a build flag. *This one genuinely wants to sit
   beside the table it labels.*
5. **`/docs/std/[module]`** — the `import` line on every module page is compiled
   as a program of its own on every build.
6. **`/docs/graph`** — an `import` inside a code quote is a string, not a wire,
   so the graph does not count it.
7. **`/tooling/editors`** — most feature rows name
   `compiler/vyrn-lsp/src/main.rs` because the server is one adapter with one
   handler per request.
8. **`/tooling/editors`** — `compiler/vyrn-lsp` is excluded from the main
   workspace and built on its own.
9. **`/tooling/editors`** — the full fallback order for `vyrn.serverPath` and
   `vyrn.path`: `PATH`, then beside the `vyrn` on `PATH`, then the repository's
   own debug build, then `cargo run`. Compressed to "searches `PATH` first and
   your checkout after". *The full order arguably wants to sit beside the
   setting.*
10. **`/why-vyrn`** — each capability pane is coloured by the compiler's own
    lexer, with the capability word tinted as the keyword it is in that
    position.
11. **`/`** — the showcase panels are coloured by the compiler's own lexer while
    the page is built.
12. **`/guide/generators`** — the site imported the generator while building the
    page, which is the same proof by another route.
13. **`/guide/styling`** — every glyph on this site, the search magnifier in the
    masthead included, arrives through `std/icons`.
14. **`/guide/getting-started`** — the site imports each guide program and calls
    `demo()` while the page builds, so the output under each block is what that
    program produced. `/guide` still says this once, which is why the per-chapter
    and per-block copies went.
15. **`/guide/*`** (every code block, about 40 of them) — "Built and run while
    this page was built; Run runs it here." The link to the file stayed.
16. **`/releases`** — every change is argued in a design record in `rfcs/`.
17. **`/benchmarks`** — the `vyrn bench` baseline is an unseeded placeholder.
18. **`/play`** — the playground's interpreter is the same tree-walking one
    `vyrn run` uses; the compiler's own recursion limit is 1,000 while the
    worker's stack stops near 466; a relative import has nowhere to resolve to
    and says so.

Two of the eighteen (4 and 9) genuinely want to sit beside the text they were
next to. That is under the threshold of ten the job sets, so the sweep did not
stop, and no note component was built.

## 6. Pages that needed no change

- `/philosophy`, `/compare`, `/editors`, `/docs/editors` — the four redirect
  stubs. Three sentences each, all of them the reader's business.
- `/error` — a status, a message and a link home.
- `/guide/[chapter]`, `/web/[chapter]`, `/tooling/[chapter]` — the chapter
  shells carry no prose of their own; every word on them comes from
  `site/app/guide.vyrn`, which was edited.
- `site/guide/*.vyrn` — the 25 runnable programs. Their header comments are read
  by users and were already two lines each. One edit only:
  `site/guide/cliargs.vyrn:2` dropped the aside "— and this page —".

## 7. Two facts that were wrong, and are now right

- `site/app/routes/guide/index.vyx:54` said "Eleven chapters" while
  `grep -c 'area: "guide"' site/app/guide.vyrn` is 12. It now renders
  `{{ chapterCount("guide") }}`, which prints 12 and cannot go stale again.
- `site/app/meta.vyrn:30` — the same wrong count, in the `<meta description>` a
  shared link to `/guide` previews as. It no longer states a number.

## 8. Left alone, and why

- **`site/public/style.css`** — another worktree owns it. Nothing in this sweep
  needed a rule change.
- **`/play`'s `?` control** (`site/app/routes/play.vyx:128`) is a hover-and-focus
  tooltip. It predates this job. It is keyboard reachable (`tabindex="0"` and
  `aria-describedby`) but not reachable by touch. Its text was cut from 88 words
  to 45 and every fact a playground reader needs — the five-second stop, the
  recursion limit, no filesystem — was kept. Turning it into a disclosure button
  would be a component, which this job must not add.
- **`/tooling/editors` breadcrumb** (`site/app/routes/tooling/editors.vyx:60`)
  reads `Reference` and points at `/docs.html`, while the page's own sidebar and
  its path say Tooling. The `<nav class="pager">` beside it has
  `aria-label="Reference"` for the same reason. That is a navigation defect, and
  this job must not change navigation. Reported, not fixed.
- **`site/app/backstage.vyrn`** — 242 lines and four passing test blocks for a
  section the export does not publish. Reported, not touched.
- **`site/app/routes/benchmarks.vyx:81`** shows the dataset path
  `rfcs/bench-0104/results/2026-08-19-v2.json`. That is the provenance of every
  number on the page, in a public repository, and it is not `RFC-00NN` as text.
  Kept.

## Gates

All run in `N:/wt-prose` at `5c9e304c`, against
`compiler/target/release/vyrn.exe` copied from `N:/lang`.

```
vyrn run site/export.vyrn out          exported 80 route(s) and 12 asset(s)
vyrn fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn   clean
vyrn test site/export.vyrn             31 passed, 0 failed
vyrn test site/app/*.vyrn              207 blocks over 29 files, 0 failed
vyrn test site/guide/*.vyrn            25 files, 0 failed
node --test "site/test/*.test.mjs"     tests 31, pass 31, fail 0
```

`site/data/history.json`, `site/data/demo.json` and `out/play.wasm` are build
inputs the site workflow writes; they were generated with
`scripts/site-history.py`, `scripts/site-demo.py` and a copy of the existing
`vyrn_play.wasm`, exactly as `site.yml` does.
