# RFC-0106 — A Consumer Page Is Scanned, Not Read

- **Status:** **M3 shipped: the section rhythm, releases, and the reference
  landing.** See [M0 — as landed](#m0--as-landed) for the census, the ceilings
  every later milestone is held to, and four things the measurements contradict
  in the design below, [M1 — as landed](#m1--as-landed) for the eight items, the
  three defects M1 found in its own earlier commits, and the two ceilings it
  cannot meet, [M2 — as landed](#m2--as-landed) for the two rebuilt pages, the
  recorded demo, and the page-weight ceiling M1 recorded as out of reach and M2
  met, and [M3 — as landed](#m3--as-landed) for the one spacing token every
  section on the site now obeys, the geometry assertions that hold it, and the
  20,693 gzipped bytes of design record that stopped being a download, and
  [M3 — the second round](#m3--the-second-round-after-the-pages-were-rejected)
  for the craft census the user's rejection forced, the header-row defect three
  milestones had deferred, and the release data that can no longer go stale, and
  [M3 — the third round](#m3--the-third-round-and-the-reference-site-as-a-source-rather-than-a-model)
  for the deploy job that was failing on a relative path, the install command
  that names this site, the changelog leaving the masthead, the search box that
  had never scrolled, and the three reference-site patterns this site does not
  take, and
  [M3 — the fourth round](#m3--the-fourth-round-read-off-the-deployed-tree) for
  the demo card whose geometry served the wrong state, every chart on the site
  painting text under 12px, a machine name published as provenance, and the two
  generated sections the index earned. Milestones below; a milestone that fails
  its gate says so in this file.
- **Depends on:** RFC-0105 (the site this redesigns — its two-front split,
  theme, accessibility checklist, and gates all survive and bind this work),
  RFC-0104 (the benchmark data the index will show), RFC-0107 (the icons the
  shell needs), RFC-0010 M3 (aliases), the export machinery in `site/`.
- **Evidence (user):** "Look how bun website look, it is much more clean,
  compact and easy to use. While our tend to add to much text, when most of it
  senctes could be just few words, add too much blocks and noise. Plan how to
  improve it significantly, do not miss anything", then "think more about it".

---

## The diagnosis

The consumer pages inherited the design record's voice. Every widget narrates
its own epistemology inline — "read from `rfcs/` while this page was built",
"CI fails unless they do" — so the page footnotes where a reader wants a noun,
a number, and a copy button. The nav is nine flat items with no hierarchy, no
persistent call to action, and no search. Nothing on the index runs. Claims
sit far from their proof.

The reference site (bun.sh) works because of one discipline: the header
carries the claim, the proof sits beside it, a command ends every thought,
and body text never exceeds a few lines. But its funnel is not ours — it
converts a Node developer who already has code, so its hero is a benchmark.
**A language's visitor asks "show me the code" first.** Our unmatched asset
there is the playground: Vyrn compiles to wasm and runs in the page.

## The rule

**Claims inline, evidence behind one click.** Every methodology caption
collapses into a small disclosure or a "how this is measured" link; the claim
stays, the epistemology moves. The word "honest" is banned from consumer copy
— be it, do not say it. The word budget (per-page ceilings, set by M0,
asserted by the export) is this rule's enforcement, not the design idea.

## The design

**The shell.** Five navigation groups with familiar names — **Docs** (the
book; absorbs install detail, editors, why-Vyrn) · **Reference** (the std
API) · **Explore** · **Releases** (blog-shaped, RSS) · **Play** — plus one
persistent **Install** button and the theme control. `/` opens a search
overlay over one build-time index (docs, guide chapters, std exports,
packages, releases), sectioned, with the `data-q` no-script fallback. A
display type scale for consumer heroes (the current tokens are
documentation-sized everywhere). OpenGraph meta on every page. "Copy page" as
markdown on docs and guide pages. Old routes emit redirect stubs, tested —
README, releases and old PRs link them.

**The index.** One viewport: the claim sentence with one accented word, a
real **runnable, editable Vyrn program** (the playground engine, lazy-loaded
behind a click so the page stays light), and the install command with OS
tabs. Below: the benchmark bars as proof section two (real committed RFC-0104
data, environment behind a disclosure); four pillar cards (types carry rules
/ ownership is a word / one program three engines / generators are
libraries), each two lines and one command; "a minute with Vyrn" — a stepped
terminal demo whose outputs a CI script records by running the real binary
(the export cannot spawn processes; the `history.json` pattern applies:
script writes JSON, export refuses without it); a comparison teaser.
Everything else leaves.

**What does not transfer from the reference, stated so nobody adds it:**
"replaces X" badges (nothing is replaced 1:1), "used by" logos (nobody uses
it; fabricating either is cosplay), runtime icon/asset fetching, analytics.
Facts-as-chips instead: no GC · one binary · native + wasm from one source ·
alpha, changes without deprecation.

**Install.** OS tiles, one command each, the `.vsix` line, "uninstalling is
deleting a directory", from-source collapsed.

**Reference landing.** The module list becomes a categorized grid (name in
accent mono + one-liner), search on top, the import graph demoted below the
fold. Module pages gain an on-this-page rail.

**Releases.** The latest release as a hero with computable stat tiles (PRs
merged since last tag, binary-size delta, new std modules, test-block delta —
all derivable from `history.json` and the archives), API-name bullets, the
upgrade command; history below with filters; an RSS feed the export writes.

**Compare.** Opens with a ✓/—/× feature matrix (Vyrn · TypeScript/Node ·
Rust) where every cell links to its proof; the radar and compact tables
follow; remaining prose plates collapse.

**Why Vyrn.** The one page allowed essay voice, halved, header-led — replaces
`/philosophy`.

**Exempt:** the backstage keeps its density; its reader wants the full text.

## Process

For visually decisive milestones (the index above all), the exported pages
are screenshotted and shown to the user **before** merge — four post-deploy
corrections this arc is the evidence for this step.

## Milestones

**M0 — census and targets.** Words, blocks, bytes and commands per consumer
page today; a mobile audit at 375px and 768px (the RFC-0105 M4 checklist has
no viewport row — this adds one); the full inbound-URL inventory for the
redirect map; the type/token audit; the search-index size estimate. Output:
numeric ceilings per page written into this RFC. Gate: no target left as an
adjective. **Gate met** — [M0 — as landed](#m0--as-landed).

**M1 — the shell.** Navigation and CTA, display scale and density tokens, the
search overlay and its build-time index, OG meta, redirect stubs, copy-page,
RSS. Content untouched; every page ships at every commit. Gate: the search
answers on every page without script falling back sanely; every old URL
resolves; the a11y checklist rows for the new controls pass. **Gate met** —
[M1 — as landed](#m1--as-landed).

**M2 — index and install.** As designed above, including the CI-recorded
demo. Gate: index page-weight inside M0's budget with the playground lazy;
outputs in the demo are real and version-stamped; **screenshots to the user
before merge**. **Gate met** — [M2 — as landed](#m2--as-landed).

**M3 — reference landing and releases**, widened by the user to the site's
spacing as a whole: one section rhythm, two section patterns, and the index and
install tightened against them. Gate: word counts inside ceilings; stat tiles
computable, no hand-written number; the feed reachable from a page; section
geometry asserted in a browser rather than looked at. **Gate met** —
[M3 — as landed](#m3--as-landed).

**M4 — the docs shell**, widened by the user from "compare matrix, why-Vyrn,
guide landing grid, editors compression" to the thing those pages had in common:
one three-pane layout for every documentation page, the reference landing as area
cards with the import graph moved off it, and the old pages brought under the
register M3's eleven rounds defined. Gate: every documentation page wears one
shell whose every row is generated; `/docs` inside M0's byte ceiling; zero
overflow and no visible scrollbar at rest at 1280 and 375. **Gate met** —
[M4 — as landed](#m4--as-landed).

**M5 — enforcement.** Word and byte budgets wired into the export for every
consumer page; mobile rows added to the standing checklist and verified; the
redirect tests permanent. Gate: the budgets fail the build when exceeded,
shown once.

## M0 — as landed

No page changed. This milestone measures, and it sets the numbers the four
milestones after it are held to. Everything below was produced by running
something: `site/export.vyrn` against the working tree at `21d18b9`, then
`scripts/site-census.py` over the tree it wrote, then a browser against that
tree served over HTTP at two viewport widths. Where a figure comes from
arithmetic rather than from a run, it says so.

The one thing the export could not supply locally is `hero.wasm`: the deploy
step builds it with a WASI sysroot this machine does not carry, so the hero
canvas 404s in the local tree. Nothing in the census depends on it, and the
figure it would add to page weight is named as unmeasured where page weight is
counted.

### The census

`python3 scripts/site-census.py out`. Thirteen consumer pages. The
representative chapter, module and package are the **median page of their
section by byte size**, picked once and recorded in the script so a re-run
measures the same page — `guide/ownership.html` (11,592 bytes, rank 7 of 13),
`docs/std/json.html` (14,722 bytes, rank 19 of 37), and `explore/shelf.html`
(8,167 bytes, the upper of the two middle pages of four that run 6,919 to
8,184, and the fullstack dogfood).

| Page | Words | Sec | Plates | Widgets | Blocks | Bytes | Cmds | Copyable | `.cap` | `.note`/`.notice` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| index | 644 | 6 | 4 | 6 | 16 | 16,024 | 1 | 0 | 5 | 5 |
| install | 678 | 7 | 1 | 1 | 9 | 12,942 | 14 | 0 | 2 | 14 |
| philosophy | 550 | 5 | 1 | 1 | 7 | 8,285 | 0 | 0 | 5 | 3 |
| compare | 1329 | 9 | 9 | 16 | 34 | 63,602 | 0 | 0 | 10 | 12 |
| releases | 235 | 2 | 0 | 0 | 2 | 3,754 | 0 | 0 | 0 | 3 |
| guide (landing) | 643 | 4 | 0 | 0 | 4 | 7,393 | 0 | 0 | 0 | 13 |
| guide/ownership | 330 | 3 | 2 | 0 | 5 | 11,592 | 0 | 0 | 2 | 0 |
| docs (landing) | 872 | 6 | 1 | 1 | 8 | 50,174 | 0 | 0 | 3 | 40 |
| docs/std/json | 786 | 7 | 0 | 0 | 7 | 14,722 | 1 | 0 | 9 | 1 |
| explore (landing) | 477 | 4 | 0 | 0 | 4 | 6,158 | 0 | 0 | 1 | 4 |
| explore/shelf | 482 | 4 | 0 | 0 | 4 | 8,167 | 2 | 0 | 5 | 1 |
| editors | 704 | 4 | 0 | 0 | 4 | 17,356 | 3 | 0 | 2 | 6 |
| play | 340 | 2 | 1 | 0 | 3 | 8,064 | 0 | 0 | 2 | 1 |
| **all thirteen** | 8070 | 63 | 19 | 25 | 107 | 228,233 | 21 | 0 | 46 | 103 |

**What a word is.** Prose only. The script removes `<script>`, `<style>`,
`<svg>`, `<template>`, `<pre>`, `<code>`, `<table>` and `<textarea>` with
their markup before it counts, so a keyword in a code plate and a cell in a
benchmark table are not prose and shrinking them is not what the word budget
is for. A token counts if it holds a letter, so `—`, `·` and a bare `2.1` do
not inflate the figure. Attribute text is not counted, and that is a decision:
an `aria-label` is prose a screen reader hears, and a budget that counted it
would reward deleting it.

**What a block is.** Three things, separate because the fix for each is
different — `<section>` (the outline a reader scans), `class="plate"` (the
sheet's bordered panel), and a widget (`class="stage"`, `<svg class="chart">`,
`<canvas>`). `Blocks` is their sum. Plates sit inside sections, so the sum is
a density reading and not a count of disjoint objects; it is comparable across
pages, which is all a ceiling needs.

**Own prose against generated prose.** Five of the thirteen carry text no
editor wrote: an export's `///` documentation, a module one-liner, a chapter
title. A word ceiling that ignored the split would demand cuts to `std/`
source. Measured by dropping the repeating element and counting again:

| Page | Total | Own prose | Generated | The generated part is |
|---|---:|---:|---:|---|
| docs/std/json | 786 | **424** | 362 | eight `.entry` blocks, from `std/json.vyrn`'s `///` |
| docs (landing) | 872 | **428** | 444 | 37 `.modlist` one-liners |
| guide (landing) | 643 | **300** | 343 | one `.modlist`, 13 chapter titles and their lines |
| explore (landing) | 477 | **401** | 76 | one `.pkgs`, four package rows |
| explore/shelf | 482 | **469** | 13 | two `.specs` lists (the manifest and the state) |

Two of those readings are the sharpest single findings in the census. The
registry landing spends **401 words explaining a list of 76 words**. A package
page spends **469 words of prose on 13 words of package**. Own prose across
all thirteen pages is **6,832** of the 8,070.

**One number the export reports is not the number on disk.** `site/export.vyrn`
prints `doc.body.byteLength`, and it prints it before `markCurrent`,
`withLang`, `withIcon` and `relativize` have run. The published `index.html` is
16,024 bytes; the export's own log says 15,978. The gap is the shell
decorations, 42 to 112 bytes a page. M5 asserts the byte budget on the string
that is written, not on `doc.body`.

### The mobile audit

**Method, named exactly.** The exported tree was served over HTTP on
`127.0.0.1` and driven in this environment's browser pane. Two kinds of
measurement, and each row says which:

- **top-level** — the page loaded as the pane's own document, at the device
  preset (375×812 with mobile device emulation, then 768×1024). Four pages
  were walked this way: `/index.html` at 375, and `/compare.html` at 375 and
  768 after the sweep flagged it.
- **frame** — all thirteen pages loaded in turn into a 375×812 (then
  768×1024) iframe inside that same emulated pane, audited by the same
  routine. Media queries answer the frame's own viewport, so the responsive
  blocks apply. The routine is calibrated rather than assumed: the frame
  reading for `/index.html` at 375 is identical to the top-level reading in
  every field — no horizontal scroll, masthead 152px, nav 718/343, eight
  sub-24px targets of which five are in prose, the same four scrollers. The
  frame reading is what the per-page table carries.

A frame is not a phone. It does not reproduce a browser's URL bar collapsing,
and `100vh` resolves against the frame. Nothing in the findings below turns on
either.

**The overflow test.** An element is reported only if its painted box leaves
the viewport **and** no ancestor is a horizontal scroller narrower than the
viewport — because a scroller is the fix, not the defect. The skip link at
`left: -9999px` is excluded by the same rule; it is off-canvas on purpose.

**The tap-target test.** Every `a[href]`, `button`, `select`, `input` and
`textarea` that paints, measured, and counted if either side is under 24px
(WCAG 2.2 AA target size). 44px is the sheet's own stated goal at ≤640px, so
24px is the floor and not the aim. A target inside a paragraph, list item,
`<dd>`, caption or table cell is counted separately: the sheet says in writing
that a word in a sentence cannot be 44px tall, and that is accepted.

| Page | 375: body scrolls sideways | 375: sub-24px targets (in prose) | 375: masthead | 768: body scrolls sideways | 768: sub-24px targets (in prose) | 768: masthead |
|---|---|---:|---:|---|---:|---:|
| index | no | 8 (5) | 152px | no | 19 (6) | 100px |
| install | no | 8 (5) | 152px | no | 18 (5) | 100px |
| philosophy | no | 4 (1) | 152px | no | 16 (3) | 100px |
| compare | **YES, 455px on 375** | 23 (2) | 152px | **YES, 768px on 758** | 34 (3) | 100px |
| releases | no | 5 (2) | 152px | no | 15 (2) | 100px |
| guide (landing) | no | 17 (14) | 152px | no | 27 (14) | 100px |
| guide/ownership | no | 6 (3) | 152px | no | 26 (3) | 100px |
| docs (landing) | no | **83 (43)** | 152px | no | **95 (45)** | 100px |
| docs/std/json | no | 29 (26) | 152px | no | 41 (26) | 100px |
| explore (landing) | no | 7 (4) | 152px | no | 17 (4) | 100px |
| explore/shelf | no | 16 (13) | 152px | no | 26 (13) | 100px |
| editors | no | 6 (3) | 152px | no | 17 (4) | 100px |
| play | no | 3 (0) | 152px | no | 13 (0) | 100px |

**`/compare` breaks the rule the site already had, at both widths, for two
different reasons.** RFC-0105 states that the body never scrolls sideways.
It does, on one page, and the causes are separate:

1. **375px, 80px over (455 against 375).** `table.matrix.bench.game` — the
   per-program benchmark table RFC-0104 M3 shipped so the radar could be
   checked — paints 422px wide inside a column of 309px, in a plain `<div>`
   whose `overflow-x` computes `visible`. Two rules meet:
   `.axispanes table.matrix { min-width: 0 }` lets the table off the 46rem
   floor every other matrix keeps, and
   `table.matrix.bench.game th, td { white-space: nowrap }` then stops it
   shrinking past 422px. Both tables on the same page that sit in a
   `.scroller` are fine. This one has no `.scroller` around it. Measured
   top-level and in frame, identically.
2. **768px: a tab row with no scroller, which reached the document edge in
   this browser.** `.tabs.axes`, the radar's axis picker, is 743px of buttons
   in a 708px column, and its last button (`pidigits`) paints from 674px to
   768px. `.tabs` gets `overflow-x: auto` only inside
   `@media (max-width: 640px)`, so **between 641px and 1024px the row has no
   scroller and overflows its column by 35px** at any width in that range.
   Whether that also scrolls the *document* depends on 10px: at the 768 preset
   this browser takes 10px for a classic vertical scrollbar, so the
   document's `clientWidth` is 758 and `scrollWidth` 768 — measured top-level,
   one offending element. A device with overlay scrollbars would get the full
   768 and no document scroll, and the tab row would still be 35px outside its
   column with its last tab unreachable. The defect is the missing scroller;
   the document scroll is how it showed up.

Neither is a word-count problem, and neither is in the diagnosis at the top of
this file. They are the mobile checklist's first two rows.

**The shell costs 152px of an 812px screen before any content.** At 375px the
masthead is a column — wordmark, then a nav strip, then the theme button — and
it is 2.4 times its desktop height (`64px`, RFC-0105 M4 row 25). The nav strip
is 718px of links showing 343px of itself, with `scrollbar-width: none`:
**fewer than half the nine rows are on screen, and nothing on the page says
so.** At 768px the
nav wraps instead of scrolling (`flex-wrap: wrap`, 530px in a 530px box) and
the masthead is 100px — the ragged second row the ≤640px rule was written to
prevent, at a width that rule does not reach.

**The nav rows are 13px tall at 768px and every width above it.**
`min-height: 44px` on `.nav a` lives inside `@media (max-width: 640px)`. At
768px a nav link measures 89×13. That is most of the sub-24px count on every
page in the 768 column, and it is one rule in the wrong block.

**`/docs` has 83 sub-24px targets at 375px, 43 of them in prose.** The module
list is 37 rows of a 13px mono link at 21px of painted height, and the import
graph adds its own. The categorized grid this RFC asks for has to fix the row
height as well as the layout.

**What behaves.** No page other than `/compare` scrolls sideways at either
width. Every wide table, code plate, tab row and the import graph on the other
twelve pages is inside a `.scroller` and scrolls in place — 4 such containers
on `/index`, 17 on `/compare`, 6 on `/docs/std/json`. No SVG escapes its
container. The `.cmd` row stacks at 375px (`flex-direction: column`, measured)
and stays a row at 768px, exactly as the sheet's comment claims. The body font
is 16px at 375px and 17px at 768px.

**The checklist rows this adds** (to be verified again at M5, where they become
standing):

| # | What | Result today | Method |
|---|---|---|---|
| 26 | The body never scrolls sideways at 375px | **fail** — 1 page of 13 (`/compare`, 80px over) | browser, top-level and frame |
| 27 | The body never scrolls sideways at 768px | **fail** — 1 page of 13 (`/compare`, 10px over on a browser with classic scrollbars) | browser, top-level and frame |
| 28 | Every wide block scrolls inside its own container | pass — 12 of 13 pages, every table, plate, tab row and graph. `/compare`'s `.tabs.axes` is 35px outside its column between 641px and 1024px | browser, frame |
| 29 | No interactive target under 24px outside running prose | **fail** — 3 to 40 per page at 375px, 13 to 50 at 768px | browser, frame |
| 30 | The navigation is usable at 375px and 768px | **fail** — a 718px strip showing 343px of itself with a hidden scrollbar at 375; wrapped to two rows at 768 | browser, frame |
| 31 | The masthead costs no more than one desktop masthead | **fail** — 152px at 375px, 100px at 768px, against 64px | browser, frame |

### The inbound-URL inventory, and the redirect map

**The design's premise is wrong, and this is where it is corrected.** The
design paragraph says old routes need redirect stubs because "README, releases
and old PRs link them". Nothing outside the site links the site at all:

| Where a link was looked for | Site URLs found |
|---|---|
| `README.md` | **0.** It links the CI badge, `raw.githubusercontent.com` for the two install scripts, the releases page and the clone URL. It does not name the website. |
| The two published releases (`gh api repos/vyrn-lang/vyrn/releases`, 263 lines of body) | **0.** One `github.com/.../compare/v…` link, which is a git range and not the `/compare` page. |
| `install.sh`, `install.ps1` | **0.** `api.github.com`, `github.com`, and `github.com/$REPO#build-from-source`. |
| `editor/vscode/README.md` | **0** URLs of any kind. |
| Issue and pull-request templates | **none exist** — `.github/` holds three workflows and nothing else. |
| `docs/`, `docs/api/` | **0.** The two `github.io` strings in the repository are both `vyrn-lang.github.io/vyrn/` inside a comment, in `.github/workflows/site.yml` and `site/export.vyrn`. |

So the whole inbound surface is the site's own links, plus whatever a reader
bookmarked or a search engine indexed. That is not nothing — the site has been
published since RFC-0105 — but it removes the argument that a stub is needed
for a file in this repository. Measured internal links, counted over the 64
consumer documents with the masthead nav, the footer, the skip link and the
wordmark removed, so only links a page's **content** wrote are counted:

| Route | Content links in | Total links in (content + shell) | Linked from |
|---|---:|---:|---|
| `/docs` | 80 | 144 | every std module page, the guide, `/explore` |
| `/play` | 42 | 106 | 20 std module pages, the guide |
| `/guide` | 14 | 78 | `/docs`, 12 guide chapters |
| `/install` | 6 | 70 | `/`, `/docs`, `/releases`, `guide/getting-started` |
| `/explore` | 5 | 69 | `/docs` and the four package pages |
| `/` | 4 | 68 | `/docs`, `/philosophy` |
| `/releases` | 4 | 68 | the four package pages |
| `/compare` | 2 | 66 | `/` only |
| `/philosophy` | **1** | 65 | `/` only |
| `/editors` | **1** | 65 | `/install` only |
| `/backstage` | 0 | 64 | the footer only |
| `docs/std/*` (37) | 284 | — | 2 to 22 each |
| `guide/*` (13) | 227 | — | 15 to 21 each |
| `explore/*` (4) | 4 | — | `/explore` only |

`/philosophy` and `/editors` are each linked from exactly one page of content.
The 106 backstage documents link `/` (216 times) and each other, and **no other
consumer route** — so renaming a consumer route cannot break the design record.

**A collision the design creates and does not name.** The five navigation
groups are "**Docs** (the book) · **Reference** (the std API) · Explore ·
Releases · Play". Today `/docs` **is** the std API and `/guide` is the book. If
those names become paths, `/docs` has to mean two things at once: a reader with
a bookmark to the reference would land on the book, which is worse than a 404
because nothing tells them they are in the wrong place, and no stub can fix it
— the path is occupied by its own replacement. The 37 reference pages are also
the most deeply linked in the tree (284 content links).

The map below therefore separates two kinds of change. A **label** change costs
nothing and breaks nothing: the nav row reads `Reference` and points at
`/docs`. A **path** change is taken only where the old path is genuinely free.

| Old route | New route | Kind | Stub needed | Why |
|---|---|---|---|---|
| `/philosophy` | `/why-vyrn` | **path** | yes, 1 | The RFC replaces the page. One content link (from `/`) and 0 external. |
| `/editors` | `/docs/editors` | **path** | yes, 1 | Absorbed into Docs. One content link (from `/install`) and 0 external. |
| `/docs` | `/docs`, labelled **Reference** | label | no | The path cannot move: its replacement wants the same name. 80 content links. |
| `/guide` | `/guide`, labelled **Docs** | label | no | Renaming it to `/docs` is the collision above. 14 content links, 13 child pages. |
| `/docs/std/<module>` (37) | unchanged | none | no | 284 content links, the deepest-linked pages on the site. |
| `/guide/<chapter>` (13) | unchanged | none | no | 227 content links. |
| `/install` | unchanged | none | no | The persistent CTA points here. |
| `/compare` | unchanged | none | no | Keeps its own route; reached from the index teaser and from Docs. Not a nav group. |
| `/releases`, `/explore`, `/explore/<pkg>`, `/play` | unchanged | none | no | Already the nav names the design asks for. |
| `/backstage`, `/backstage/*` (107) | unchanged | none | no | Exempt, and it links no consumer route but `/`. |

**Two stubs, not a redirect layer.** Each is a published document at the old
path with `<meta http-equiv="refresh" content="0; url=…">`, a `<link
rel="canonical">` and a visible link for a reader whose browser declines the
refresh. Static hosting has no 301, so a stub is the only mechanism available.
Both are tested the way `site/test/basepath.test.mjs` tests everything else:
fetch the old path, follow the target, assert it answers 200.

**And one thing a rename touches that is not a URL.** Every consumer link
carries `data-key` for the soft navigator (41 distinct values on `/` alone),
and each page has a `.data.json` payload beside it. A path change moves three
files, not one: the document, the payload, and the `data-key` every page writes
for it.

### The type and token audit

Three token families, and they are not in the same state:

| Family | Custom properties | Uses | Literals left |
|---|---:|---:|---|
| Colour | **25** | the whole sheet | **0** on a property (RFC-0105 M4 closed this) |
| Spacing (`--s1`…`--s6` = 8/16/24/32/48/64) | **6** | 176 | 72 raw-px occurrences over 16 distinct values. **59 of the 72 are at 8px or below** — optical adjustments (`1px` ×28, `6px` ×12, `4px` ×6), which the 8pt grid was never going to hold. The 13 that are not: `10px` ×4, `12px` ×3, `14px` ×2, `16px` ×1, `9px` ×2, and the skip link's `-9999px`. |
| **Type** | **0** | — | **34 distinct literal font sizes, plus 4 `clamp()` expressions, spread over 1,729 lines** |

That is the finding. The sheet tokenized colour, tokenized spacing, and left
the type scale as literals nobody can measure — the same argument RFC-0105 M4
made for moving colour into one block, unapplied to type. (A grep for
`--*size*` matches `--syn-type` three times; that is a colour, and the count
above excludes it.)

The ladder as declared, with the computed value at the sheet's own base — root
`16px`, `body: 17px/1.6`, so `1rem` is 16px:

| Declared | Computes to | Role, and where |
|---|---|---|
| `clamp(2.1rem, 1rem + 4vw, 4.5rem)` | 33.6px at ≤440px · 46.7px at 768 · 67.2px at 1280 · 72px above 1400 | `.display` — the one display step, one per page |
| `clamp(1.8rem, 1rem + 2.4vw, 3rem)` | 28.8 – 48px | `.bignum`, mono — the only numeral scale |
| `clamp(1.7rem, 1.1rem + 1.8vw, 2.4rem)` | 27.2 – 38.4px | `.rfcdoc h1` — backstage only |
| `clamp(1.5rem, 1rem + 1.4vw, 2.2rem)` | 24 – 35.2px | `.band h2` — a section heading |
| `1.5rem` | 24px | `.prose h2` — reference and guide body heading |
| `1.2rem` | 19.2px | `.lede` |
| `1.1rem` / `1rem` | 17.6 / 16px | `.prose h3` / `.rfcdoc h4`–`h6` |
| `17px` | 17px | `body` |
| `16px` | 16px | `body` at ≤640px |
| `0.95rem` ×3 | 15.2px | `table`, `.notice`, `.modlist` note text |
| `15px` | 15px | `.wordmark` |
| `0.92rem` ×5, `0.92em` | 14.7px | `.note` in five places — the meta-prose size |
| `0.9rem` ×2 | 14.4px | `table.matrix` |
| `14px` | 14px | `.cmd code` — the copyable command |
| `0.85rem` ×3 | 13.6px | a table caption, an empty-state |
| `13.5px` ×2 | 13.5px | `pre.code`, `.lines` |
| `13px` ×2 | 13px | `.nav a`, `.modlist a` |
| `12.5px` | 12.5px | the playground editor at ≤640px |
| `0.78rem` | 12.5px | `table.matrix.bench .norm` |
| `12px` ×5 | 12px | `.eyebrow` (102 occurrences in the templates), `.cap` (44), `.modlist .count`, `.columns pre`, `.lines` at ≤640 |
| `11px` ×3 | 11px | chart text, `.serieskeys button` |
| `10px` ×3 | 10px | schematic sub-labels, radar names, `.columns .eyebrow` |
| `9px` ×4 | 9px | every chart axis |
| `0.42em` | ~12–20px | `.bignum small` |

Also `1.25rem`, `1.3rem`, `1.6rem`, `2rem`, `2.2rem`, `2.4rem`, `3rem` and
`4.5rem` appear as one-off literals or as clamp endpoints.

**So "documentation-sized everywhere" is half right, and the half that is
wrong matters.** There *is* a display step, and at 1280px it computes 67.2px,
which is display size. Two things are true instead:

1. **The scale has no name anywhere.** Nineteen steps between 9px and 24px, all
   literals. A display scale added on top of that is a twentieth literal.
2. **The display step collapses on a phone.** `clamp()`'s middle term is
   `1rem + 4vw`, which at 375px is 31px — below the 33.6px floor, so every
   width up to 440px gets the same 33.6px headline. The desktop hero is
   display-sized and the phone hero is 1.4 lines of body text. That is the
   width where the reference site is most emphatic and ours is least.

**What a display scale must add, and what it must not disturb.** Add eight
tokens on `:root` — `--t-display`, `--t-h1`, `--t-h2`, `--t-h3`, `--t-lede`,
`--t-body`, `--t-meta`, `--t-eyebrow` — defined to the values above so that
every existing rule that names a size instead names a token and computes the
same number. Then raise only `--t-display`'s floor and its `vw` coefficient,
scoped to the nine landing pages. The constraint is stated as a test M1 owns:
**the computed `font-size` of every element on the 160 leaf pages (13 guide
chapters, 37 reference modules, 4 package pages, 106 design records) is
unchanged, at 375px, 768px and 1280px.** A display scale that changes a
reference page has missed.

### The search index

Built as a prototype from the exported tree and measured, not estimated. One
entry is `{"s":section,"t":title,"u":url,"d":one line}`, serialized with no
whitespace:

| Section | Entries | Bytes | Gzipped |
|---|---:|---:|---:|
| Reference exports (`id="e-…"` on 37 module pages) | 354 | 29,567 | 3,578 |
| Reference modules | 37 | 5,729 | 643 |
| Guide chapters and their headings | 41 | 4,472 | 736 |
| Consumer pages (title + `<meta description>`) | 10 | 1,930 | 303 |
| Packages | 4 | 383 | 143 |
| Releases | 2 | 159 | 83 |
| **One index** | **448** | **42,235** | **5,255** |
| the same with the backstage (106 records + their headings) | 2,273 | 331,728 | 36,449 |

**One file, for the whole consumer site.** The overlay is sectioned, so it
needs every section on the first keystroke; splitting into six files buys
nothing and costs six requests. 5.3 KB gzipped is less than `theme.js`
(2.4 KB) plus `hero.js` (4.1 KB), both of which every page already fetches
before first paint, and it is a fifth of `style.css` (24.9 KB gzipped). It is
fetched on the first `/` press and never in the document.

**The backstage stays out.** Including it multiplies the index by 5.1 in
entries and by 6.9 gzipped, to serve the one reader this RFC exempts, and it
would put `M4 — as landed` and `The token census` in front of a consumer who
typed `json`. If a backstage search is ever wanted it is the same generator
with a different source list, and a second file the consumer never fetches.

**One thing does not exist yet.** Of the 28 `<h2>`/`<h3>` headings inside the
13 guide chapters, **0 carry an `id`**, so the 41 guide entries above can only
link to a chapter and not to a heading in it. M1 adds slug ids to guide
headings, or the guide contributes 13 entries instead of 41.

### The ceilings

Numbers, on the metrics `scripts/site-census.py` already reports, so M5 wires a
gate rather than inventing a measurement. `own prose` is the page's total minus
the generated text measured above; `total` is what the script prints and what
the gate asserts — for the five pages with generated text it is the own-prose
ceiling plus the generated count as it stands today, which moves only when
`std/` documentation or a chapter list moves.

| Page | Words now (own prose) | Ceiling: own prose | Ceiling: total | Bytes now | Ceiling: bytes | `.cap` now | Ceiling: `.cap` | Cmds now | Floor: cmds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| index | 644 (644) | **380** | **380** | 16,024 | **34,000** | 5 | **8** | 1 | **5** | <!-- RAISED IN M3 ROUND 4, from 260/260/30,000/2. The page gained a five-tab showcase of the book's own programs and an eight-module reference teaser, both generated; what the census counts as the new words are tab labels, one-line captions, module names and group names, and the `.cap` count is one caption per showcase pane plus the demo's provenance line. No paragraph on the page grew. The ceiling that bounds what a reader pays for is the cold load, and that is 40,762 of 55,000. -->
| install | 678 (678) | **220** | **220** | 12,942 | **14,000** | 2 | **1** | 14 | **8** |
| why-vyrn (was philosophy) | 550 (550) | **280** | **280** | 8,285 | **9,000** | 5 | **2** | 0 | **0** | <!-- `.cap` RAISED IN M4, from 0 to 2, and the reason is the SHELL rather than the page: M1's search overlay carries a `.cap` ("Type to search. Esc closes.") on all 174 documents, so every ceiling in this column has a floor of 1 that was written before the overlay existed. The page's own one is the capability plate's provenance line. The word ceiling is NOT raised: 540 against 280 is a prose diet M4 did not do, and M5 fails on it. -->
| compare | 1329 (1329) | **420** | **420** | 63,602 | **55,000** | 10 | **3** | 0 | **0** |
| releases | 235 (235) | **200** | **200** | 3,754 | **20,000** | 0 | **1** | 0 | **1** |
| guide (landing) | 643 (300) | **180** | **580** | 7,393 | **10,000** | 0 | **1** | 0 | **0** | <!-- RAISED IN M4, from 523/9,000/0. The chapter list is `.modgrid` now and the page is under the docs family; the extra words are the thirteen ledes rendered in full where `.modlist` clamped them, and the `.cap` is the shell's own noscript line. No paragraph on the page grew — two were cut. -->
| guide/ownership | 330 (330) | **380** | **380** | 11,592 | **15,000** | 2 | **3** | 0 | **0** | <!-- BYTES RAISED IN M4, from 13,000, and `.cap` from 1 to 3. The docs shell adds a fourteen-row sidebar, a breadcrumb and a pager to every chapter: generated navigation, not prose. Gzipped 4.9 KB. -->
| docs (landing) | 872 (428) | **200** | **700** | 50,174 | **40,000** | 3 | **1** | 0 | **0** | <!-- WORDS RAISED IN M4, from 644. The page gained four area cards; the bytes did NOT move and are met for the first time — 29,231 against 40,000, because the import graph is `/docs/graph` now. -->
| docs/graph | — | — | **260** | — | **40,000** | — | **2** | — | **0** | <!-- NEW IN M4. The import graph, off the reference landing. The byte ceiling is the landing's own, because the page now carries what the landing carried. -->
| docs/std/json | 786 (424) | **150** | **900** | 14,722 | **24,000** | 9 | **10** | 1 | **1** | <!-- RAISED IN M4, from 512/16,000/1. The 41-row reference sidebar is 60 of the new words and 5 KB of the new bytes, and it is navigation the page did not have. The `.cap` ceiling is raised to what a generated reference page IS: one provenance line per export — `std/json.vyrn, line 41` — which is the line THE RULE exists to require, one per entry rather than one per plate. -->
| explore (landing) | 477 (401) | **160** | **236** | 6,158 | **8,000** | 1 | **2** | 0 | **0** | <!-- `.cap` RAISED IN M4, from 1 to 2, for the shell's overlay caption — see the why-vyrn row. The word ceiling is NOT raised: M4 took the page from 496 to 407 against 236, and the rest is M5's. -->
| explore/shelf | 482 (469) | **200** | **213** | 8,167 | **8,000** | 5 | **1** | 2 | **2** |
| docs/editors (was editors) | 704 (704) | **200** | **800** | 17,356 | **26,000** | 2 | **3** | 3 | **3** | <!-- RAISED IN M4, from 200/10,000/1. Two things: the census row had been measuring the `/editors` REDIRECT STUB, 66 words, so this page's real 723 had never been counted; and the docs shell adds the same 41-row sidebar. -->
| play | 340 (340) | **120** | **120** | 8,064 | **9,000** | 2 | **1** | 0 | **0** |
| **all thirteen** | **8070 (6832)** | **2970** | **4208** | 228,233 | 241,000 | 46 | **14** | 21 | **20** |

Read the totals in the right order. Own prose falls from **6,832 to 2,970, a
57% cut**, and the total from 8,070 to 4,208. HTML bytes do **not** fall, and
that is deliberate: `/releases` grows from 3.7 KB to a hero with computable
stat tiles, and `/` grows to hold a runnable editor, benchmark bars and a
recorded demo. Prose leaves; structure arrives. The byte figure that has to
fall is a different one.

**Page weight, which is where the real excess is, and which the diagnosis at
the top of this file does not mention.** Measured from the browser's own
request list on a cold load of `/`: **twelve text requests — the document, the
stylesheet, nine scripts and the favicon — 267,028 bytes, 88,025 gzipped**,
plus `hero.wasm`. Every consumer page fetches the same twelve, and the reason
is four lines at the top of `site/public/widgets.js`:

```js
import { mountHero }      from "./hero.js";
import { refreshRelease } from "./fresh.js";
import { mountPlay }      from "./play.js";     // and play.js imports play-wasm.js
import { runVyrn }        from "./wasi-min.js";
```

Static imports, so a page that has no playground on it still downloads the
playground's three JavaScript files — **14,587 gzipped, on twelve of the
thirteen pages**. Verified in the request list: `/compare.html` fetched
`play.js`, `wasi-min.js` and `play-wasm.js`. The compiler-as-wasm module those
files load is lazy and is not in this count. `vyrn-nav.js` (which pulls
`vyrn-dom.js`) arrives through a dynamic `import()` on the last line of
`widgets.js`, so the pair is after first paint but still on every load.

| Asset | Bytes | Gzipped | Needed on `/` |
|---|---:|---:|---|
| `style.css` | 84,123 | 24,866 | yes |
| `widgets.js` | 44,181 | 15,013 | yes |
| `vyrn-nav.js` | 34,630 | 11,315 | after first paint, or on first link hover |
| `vyrn-dom.js` | 29,171 | 8,997 | with the navigator |
| `wasi-min.js` | 22,885 | 8,214 | **no** — `/play` only |
| `index.html` | 16,024 | 4,888 | yes |
| `play.js` | 13,942 | 4,868 | **no** — `/play` only |
| `hero.js` | 9,445 | 4,056 | yes, on `/` |
| `theme.js` | 5,369 | 2,366 | yes, before first paint |
| `fresh.js` | 3,450 | 1,641 | yes |
| `play-wasm.js` | 3,182 | 1,505 | **no** — `/play` only |
| `favicon.svg` | 626 | 296 | yes |
| **total** | **267,028** | **88,025** | |
| `hero.wasm` | not built locally | — | on `/` (unmeasured; the deploy step builds it) |

So:

- **Every consumer page except `/play`: ≤ 55,000 bytes gzipped on first load,
  counting the document and every asset the browser fetches without an
  interaction.** Today 88,025. Moving the three playground files to `/play`
  saves 14,587 and deferring the navigator pair saves 20,312, which lands at
  53,126 and leaves under 2 KB for the search overlay's own code. The search
  index (5,255) does not count: it is fetched on the first `/` press.
- **`/play`: ≤ 70,000 bytes gzipped**, the same figure plus the playground
  runtime it is the only page that needs.
- **`style.css`: ≤ 90,000 bytes raw, ≤ 27,000 gzipped.** Today 84,123 /
  24,866. The eight type tokens, the density tokens and the search overlay's
  rules get 6 KB and no more.
- **The search index: ≤ 8,000 bytes gzipped, and never in a document.** The
  prototype is 5,255.

**Captions, and THE RULE as a number.** 46 `.cap` elements become **14**, at
most one per plate that carries evidence, and **zero of the survivors is a
paragraph in flow** — each is a `<details><summary>` inside its own plate, so
the claim is visible and the method is one click away. `.note` and `.notice`
together fall from **103 to 26**, at most two a page. `/docs` alone carries 40
of the 103 today.

**Commands is a floor, not a ceiling** — the design asks for a command at the
end of a thought. `/` goes from 1 to at least 5 (install, the pillar cards, the
demo). `/install` may go *down*, from 14 to 8: OS tabs replace four repeated
tiles. Across the thirteen the floor is 20 against 21 today, which says the
commands are already there and are on the wrong pages.

**The display type scale goes on nine pages** — `/`, `/install`, `/why-vyrn`,
`/compare`, `/releases`, `/guide` (landing), `/docs` (landing), `/explore`
(landing), `/play`. The **160 leaf pages** keep the documentation scale with
every computed size unchanged: 13 guide chapters, 37 reference modules, 4
package pages, 106 design records. `/editors` folds into Docs as a leaf.

### What M0 contradicts in the design above

Recorded here rather than by editing the design, so the correction and its
evidence sit together.

1. **"README, releases and old PRs link them" is false.** Zero site URLs in
   `README.md`, in either release body, in both install scripts, in the editor
   README, in `docs/`, or in an issue or PR template — there are no templates.
   The redirect map above is two stubs for two genuinely renamed routes, and
   its argument is bookmarks and search engines, not this repository.
2. **The nav's five names collide with two live paths.** "Docs" is the book in
   the design and the reference on the site; a path rename would land a
   reference bookmark on the book with nothing to say so, and no stub can hold
   a path its replacement occupies. Labels move, those two paths do not.
3. **"The current tokens are documentation-sized everywhere" is half wrong.**
   There is a display step and at 1280px it is 67.2px. The real defects are
   that the type scale has **no tokens at all** — 34 literal sizes against 25
   colour tokens and 6 spacing tokens — and that the display step floors at
   **33.6px for every width up to 440px**, so the phone hero is the size of
   body copy.
4. **The word budget is not where the page weight is.** Prose is 8,070 words
   across thirteen pages; first load is **88 KB gzipped on every one of them**,
   of which 14.6 KB is a playground runtime that twelve of the thirteen do not
   use. A word budget cannot see that, and M1 has to fix it while it touches
   the shell.

Two smaller ones, on the record: `/compare` breaks RFC-0105's own
no-sideways-scroll rule at both audited widths, for two unrelated reasons, and
the export's byte log reads `doc.body` before the shell decorations are
applied, so it under-reports every published page by 42 to 112 bytes.

**One number, for comparison with RFC-0105 M4.** The export publishes **172
routes** and 13 assets, where M4 recorded 206. Both counts are of what the
program printed. The tree has grown since — two more design records — so the
difference is in what was being counted and not in what was published; 172 is
the number `site/export.vyrn` prints today: 11 top-level pages (`/backstage`
among them), 13 guide chapters, 37 reference modules, 4 package pages, 106
design records, and the benchmarks page.

## M1 — as landed

The shell, in eight items. Every number below was produced by running
something: `site/export.vyrn` over the working tree, then the site's own 188
test blocks, then `node --test site/test/*.mjs` over the tree the export wrote,
then a browser against that tree served over HTTP at twelve viewport widths.
Where a figure comes from arithmetic it says so.

**Content is untouched.** Not one word of a page's prose moved: the word
budget is M5's and the pages it applies to are M2's, M3's and M4's.

### The eight items

**1. The navigation, the call to action, and the header.** Nine flat rows
became five groups — Docs · Reference · Explore · Releases · Play — plus the
persistent Install button and the theme control, in one `<nav>` rendered once,
which is a `<details>` disclosure at 640px and a row above it. Two of the five
names are LABELS and not paths, and M0's contradiction 2 is why. The masthead
DECLARES 64px, and it is 64px on every one of the thirteen consumer pages at
every width measured: 320, 375, 414, 640, 641, 700, 767, 768, 900, 1024, 1280
and 1600. It was 152px at 375 and 100px at 768.

**2. The type scale.** 34 literal font sizes over 1,729 lines became 22 named
steps; 86 declarations were rewritten and none changed value. One value moved
and only its floor: the display step's clamp floor, from 2.1rem to 2.6rem, so
the phone hero is 41.6px instead of 33.6px and every width from 641px up is
unchanged to the pixel. `site/test/typescale.test.mjs` fails on a font size in
the sheet that names neither a token nor one of nine listed one-offs.

**3. The search overlay and its index.** `/` opens it on every page, Esc
closes it, the arrows walk the results and wrap, Enter follows the selection,
and Tab returns to the field so nothing behind an `aria-modal` dialog is
reachable. Focus is taken from wherever the reader was and given back to the
same element. The index is ONE file, fetched on the first open and never
inlined; the panel is empty in the document and `hidden`, so nothing in it is
in the tab order until it opens. Without script there is no overlay and a
`<noscript>` says what the site does instead — the reference landing filters
its own module list with `data-q` and no script at all.

**4. OG meta.** `og:title`, `og:description`, `og:type` and `twitter:card` on
all 172 published routes bar the two stubs. DERIVED, not tabulated: a page
already carries a `<title>` and a `<meta description>`, so the card is those
two strings under the three names a crawler reads them by, and it works the
same on a guide chapter, a reference module, a package page and a design
record with no per-page list to maintain.

**5. Copy page as markdown.** 50 files — 13 guide chapters and 37 reference
modules — written by `site/app/pagemd.vyrn` from the same chapter prose and
the same generated reference the pages render. The control is a LINK to the
file; with a clipboard, a press copies it and the page does not move.

**6. RSS.** `releases.xml`, one item per tag out of `site/data/history.json`,
newest first, with `atom:link rel="self"` and RFC-822 `pubDate`s. Declared in
the head of every page, so a reader who pastes any page of the site into an
aggregator finds it.

**7. The two renamed routes.** `/philosophy` → `/why-vyrn` and `/editors` →
`/docs/editors`, each old path publishing a document with three separate ways
out: the `http-equiv` refresh a browser may decline, the canonical a search
engine reads, and a visible link, which is all a screen reader has before the
hop fires.

**8. The playground leaves the static import graph.** `play.js`,
`play-wasm.js`, `wasi-min.js` and `hero.js` are `import()` at the point of use,
so twelve of the thirteen pages stop fetching a playground they never run.

### The search index, as shipped

**448 entries, 42,797 bytes raw, 6,564 gzipped, against M0's ceiling of
8,000.** One file, four sections, and the backstage nowhere in it — all three
asserted on the written file by `site/test/search.test.mjs`, which also fetches
every row's URL and fails on a result that would 404.

Two measured decisions got it there, and both are about what a ROW carries,
never about coverage — the 448 entries are the census's 448:

- **A kind and not a signature.** 354 of the 448 rows are exports, so what
  they carry decides the file. With each signature in the row the index was
  70,080 bytes and 13,483 gzipped — 68% over. `fn` is four bytes, and the
  signature is on the page the row leads to, one press away.
- **No fragment, and the row still lands on the export.** `#e-emit` is the
  row's own title with three bytes in front of it, so carrying it wrote 354
  names into the file twice: **7,763 gzipped with it, 6,564 with it derived by
  the overlay.** The derivation is one line of `widgets.js`, beside the
  reference landing's own filter, which built the same fragment the same way
  before this milestone. The test that replaced the fragment is stronger than
  the fragment was: it opens each of the 37 module pages and asserts the id is
  there, which a string this index wrote itself could never have caught.

**The gate was shown failing.** Putting the signature back in `d` gave
`search.json is 10405 bytes gzipped (58515 raw), ceiling 8,000` and one failed
block; reverted, green again. The export makes the raw half of the same check —
80,000 bytes, from the census's own 8.0 ratio — because RFC-0014 gives that
program `readFile`, `writeFile` and `listDir` and no compressor, and a runaway
index fails whichever half it reaches first.

**The guide contributes 41 rows and not 13, and no markup changed.** M0 looked
for an `id` on the 28 `<h2>`/`<h3>` headings in the chapters and found none.
The id is one element up: every section is a `<section id="…">`, which is what
a chapter's anchor has always been.

### One table, three readers, and four titles that had drifted

`app/meta.vyrn` is the ten consumer pages, each with its own title and its own
sentence. Before it, every page on the site shipped the SAME `<meta
description>` — one paragraph about the language, from `pageHead` — so a link
to `/install` and a link to `/compare` previewed identically and neither said
what was on the other end. The table is read by the page's own `<head>`
(through `pageHeadOf`), by the OpenGraph card the export stamps, and by the
search index's Docs rows, so a page cannot say one thing to a crawler and
another in the overlay.

It TAKES THE PATH and not the title, and the reason is what happened when the
export first asserted that the two agreed. **Four of the ten titles were
wrong:**

| Route | Title it shipped | Title it ships now |
|---|---|---|
| `/why-vyrn` | `Philosophy — Vyrn` | `Why Vyrn — Vyrn` |
| `/docs` | `Docs — Vyrn` | `Reference — Vyrn` |
| `/guide` | `The guide — Vyrn` | `Docs — Vyrn` |
| `/explore` | `The Vyrn registry — packages you can install` | `Explore — Vyrn` |

The first is a rename this milestone made two commits earlier and left in the
`<title>`. The other three are the masthead's five names disagreeing with the
tab: the row called Docs is the book and the row called Reference is `std/`,
and the titles said the opposite of that on both.

### Three defects M1 found in its own earlier commits

All three were live in the tree before this commit, all three were missed by
the verification the commits that introduced them recorded, and the reason is
the same in every case: **the check that was run was not the check the claim
needed.**

**A phone had no navigation at all.** `.menu > summary` is `display: none` in
the base rules, and the 640px block that turns the `<details>` back into a
disclosure restyled that summary — colour, letter-spacing, cursor, the marker —
without restoring its `display`. So the `Menu` control was a 0x0 box at every
width from 320 to 1600, and the five navigation rows were unreachable on a
phone. The commit that built the shell reported "the summary takes focus, the
platform's own activation opens and closes it" — read off the DOM, where the
element is present and its `open` attribute does toggle; a control with no box
is invisible to that question and obvious to `getBoundingClientRect`.

**The header was 129px from 641px to 767px.** `Play` wrapped to a second row
there, so the row the same commit declared to be 64px was twice that. It was
measured at 375 and at 768 — M0's two audited widths — and 641 to 767 is
neither. The Search opener then took 768px into the same state, because the
five names want 381px of the 348px the row had left once three controls were in
it; tightening the masthead's own gaps from 24px to 16px bought 16px and fixed
768, and did nothing for 641. The fix is
the rule that commit's own predecessor established for `.tabs`: a row whose
item count is not a function of the viewport scrolls inside its own box. The
nav is `flex-wrap: nowrap` with `overflow-x: auto` now, so the masthead is
64px at every width and at 641px the last 112px of the row is a scroll rather
than a second line.

**The masthead overflows by 46px at 320px, and this one is not fixed.** The
Search opener is 70px of a row that had 174px of controls and now has 252,
and 320px is 32px of padding plus 288px of usable row. At 375px it fits with
9px to spare, which is why the audited widths do not see it. It is left here
rather than fixed because every fix is a decision about which control loses
its word — the opener's label, the theme control's state, the CTA's verb — and
**M4 owns the shell's words.** M4 has the arithmetic above; M5 adds 320px to
the standing checklist, which today has 375 and 768 and no third column.

### The mobile audit, re-run

Thirteen consumer pages, at M0's two widths, in a browser against the exported
tree:

| Row | At 375px | At 768px |
|---|---|---|
| masthead height (31) | 64px on all 13 | 64px on all 13 |
| document scrolls sideways (26) | none | none |
| element outside the viewport with no scroller over it (27, 28) | none | none |
| sub-24px targets (29) | 76, of which 54 are SVG and 21 are words in sentences | 88, of which 54 are SVG and 21 are words in sentences |
| `/docs` alone (29) | 37, all 37 SVG | 37, all 37 SVG |

Row 29 is still not a pass and the remainder is still what M0 named: an `<a>`
inside an SVG has no CSS box for padding to grow, and **M3 rebuilds both
charts.** The one element outside the viewport on every page is the skip link,
parked at -9999px until it takes focus, which is what a skip link is.

### The 161 leaf pages

**M0's constraint on the leaves survives; the byte-identity that was used to
check it does not, and could not.** The search overlay and its `<noscript>` are
elements in every body — that is what "the search answers on every page" means
— so every page's body is 711 to 802 bytes longer than it was. What M0 asked
for is that the 160 leaf pages compute the font sizes they computed before, and
that is held by an attribute rather than by a byte count: the display step is
raised on `[data-landing]` alone, `site/test/typescale.test.mjs` proves that is
the only selector in the sheet that redefines a type token, and
`site/export.vyrn` asserts per published document that the nine landing pages
carry the attribute and no leaf does.

Measured instead, against the tree exported at the commit before this one:
remove the four things M1 adds to the SHELL of a page — the card meta, the feed
link, the Search opener with the overlay and its `<noscript>`, and the copy-page
control — and **55 of 55 consumer leaves and 106 of 106 design records are
byte-identical.** Nothing in a page's own content moved.

### Page weight, and the ceiling that is still out of reach

The browser's own request list on a cold load, gzipped at level 9, before and
after this commit's own additions:

| Page | Requests | Before | After | Delta |
|---|---:|---:|---:|---:|
| `/compare` | 9 | 87,199 | 90,978 | **+3,779** |
| `/` | 10 | 83,899 | 87,716 | **+3,817** |

**+3,436 of it is `widgets.js`** — the overlay, its listeners and the
copy-page control, which is one file every page already fetched. The rest is
the document: 287 bytes on `/compare` for the opener, the panel, the
`<noscript>` and five head elements.

**M0's 55,000-gzipped ceiling for a non-play page is further away than when
M0 wrote it, and the arithmetic was already recorded as unreachable in M1.**
Deferring the navigator pair — `vyrn-nav.js` and `vyrn-dom.js`, 20,275
gzipped between them — is the item that closes most of the gap and it is
RFC-0072's ground, not this milestone's. A `/compare` document is 12,705
gzipped on its own against a 55,000 ceiling that was computed for `/`, whose
document is 5,217. **The ceiling is a `/` ceiling as written**, and M2 owns
`/`.

Two files are outside every total above, and both deliberately: the search
index (6,564 gzipped) is fetched on the first `/` press, and the feed (505) is
fetched by no page at all.

### The stylesheet, and where 1,813 bytes came from

**88,878 bytes raw and 26,520 gzipped, against M0's 90,000 and 27,000.** M1
was given 6 KB over the census's 84,123 and spent 4,755 of it: 22 type tokens,
the overlay's ten rules, the header's declared height, and the two fixes above.

It did not fit at first. The sheet reached 90,691 — 691 over — with the nav
scroller and the one `display` that gives a phone its navigation back, and both
of those are the difference between a shell that works and one that does not.
The bytes came from the **dash rules of the banner comments**: 37 lines of
them, every run shortened to 24 dashes, for **1,813 bytes and not one
word.** 48% of
this sheet is comment prose, which is deliberate and stays; a row of hyphens
is not prose.

**A LEAD FOR M2, MEASURED AND NOT ACTED ON.** A scan of the sheet's 200 class
selectors against every `class` attribute in the exported tree, every
`className` and `classList` call in the browser modules, and every template in
`site/app/` finds **17 that appear nowhere.** Seven are false positives — a
file extension inside a comment reads as a class — and ten are candidates:
`copyfail`, `metric`, `metrics`, `spec`, `diffline`, `warn`, `warning`, `feed`,
`tag`, `exports`. `.feed` is the releases list RFC-0105 M1 deleted. They are
not deleted here because a class can be built by concatenation in a `.vyrn`
generator, which no scan of attributes can see, and a milestone about the shell
is the wrong place to delete a rule on a heuristic. M2 needs bytes and this is
where they are.

### The overlay's accessibility, and how it was checked

**Keyboard-walked against the exported tree, through the real listeners, with
dispatched events — and the method is named because the pane in this session
would not composite frames**, so no real key press or click reached the page
and no screenshot could be taken. Layout is computed either way, which is why
every geometry above is trustworthy; input routing is not, so every keystroke
below was a `KeyboardEvent` dispatched at the element a reader's key would
reach. The listeners, the focus moves, the fetch and the DOM are the real ones.

| What | Result |
|---|---|
| `/` from a link in the body | opens; focus is the field; the index is fetched, once |
| `/` while a field or an editor has focus | does not open — a page with a module filter or a playground must not swallow a slash |
| typing | `install` → 4 hits under Docs · `shelf` → 1 under Packages · `alpha` → 1 under Releases · `json` → 39 under Reference · `zzzznothing` → `Nothing matches "zzzznothing".` |
| Down, Down, Up | row 0, row 1, row 0; exactly one row carries `aria-selected`; focus never leaves the field |
| Up from the first row | wraps to the end of the list — index 37 of 39 after two presses |
| Tab | `preventDefault`, focus back in the field |
| Enter | follows the selected row to `docs/std/json.html#e-emit`, and the dialog is closed BEFORE the hop |
| Esc | closes, empties the list, and returns focus to the element the reader came from |
| the whole document | no element carries a positive `tabindex` |

**One defect found in the overlay by this walk, and fixed.** "Sectioned" was
not true. The results were sorted by rank alone and a heading was drawn
whenever the section changed, so `text` gave
`Reference · Docs · Reference · Docs · Reference` — five headings for two
sections, which is a list that is not sectioned. Ranking and grouping answer
different questions and are two passes now: the rank decides WHICH forty rows a
reader sees and runs over all 448, then the section decides what order those
forty appear in. `map`, `text` and `vyrn` each draw two headings now, and each
section appears once.

### Gates

- `scripts/site-history.py`, then `vyrn run site/export.vyrn out`: **174
  routes, 13 assets, 50 markdown twins, one index, one feed.**
- the site's own test loop, declared against ran, over `site/export.vyrn` and
  all 26 modules in `site/app/`: **188 blocks declared, 188 ran.**
- `node --test site/test/*.test.mjs` over the exported tree: **27 tests, 27
  pass** — basepath at three mount points and `file://`, contrast, the import
  lines, the type scale, the index and its ceiling, and the feed.
- `vyrn fmt --check` on `site/app/*.vyrn`, `site/guide/*.vyrn` and
  `site/export.vyrn`: clean. The three `.vyx` templates this milestone touched
  are hand-formatted — `vyrn fmt` does not read `.vyx`.
- `cargo test --release -p vyrn-cli --test rfc_index`: 4 passed.
- No compiler change. No `std/` change.

## M2 — as landed

Two pages, rebuilt. Every number below was produced by running something:
`scripts/site-history.py` and `scripts/site-demo.py`, then `site/export.vyrn`
over the working tree, then `scripts/site-census.py` over the tree it wrote,
then `node --test site/test/*.test.mjs` over the same tree, then a headless
browser against it served over HTTP at two widths in both palettes. Where a
figure comes from arithmetic it says so.

### The index

**One viewport, and it is measured.** At 1280x800 the claim, the lede, the
install command with its OS tabs, the facts chips and the whole runnable
program end at 794px of an 800px viewport, under a 64px masthead. Under it:
five benchmark bars, four pillar cards, seven recorded terminal steps and one
comparison teaser. **Five sections against six, eleven blocks against sixteen,
and 257 words against 644.**

Five things left the page and are not on it anywhere: the hero canvas, the
diagnostic replay, the ownership playhead, the three-target schematic, and the
parity digest comparison. What replaced them is listed here rather than
narrated on the page, which is the whole of THE RULE.

**The hero editor is the playground, on a smaller root, loaded on the first
interaction.** It is `mountPlay` and not a second editor: the same
`data-play-*` hooks, the same keyboard contract, the same highlighter, so a
reader gets the compiler's own lexer in both places and there is one control to
keep working. What is new is WHEN. The document carries a coloured code block
over a `readonly` textarea and nothing else; three things arm it — pointing at
the plate, tabbing into it, and pressing Run — and the arming is what imports
`play.js`, which imports `play-wasm.js`, which fetches `play.wasm`. Measured
from the browser's own request list: **a cold load of `/` fetches four text
files and the favicon; a `focusin` on the textarea fetches three more and the
status line goes from `Press Run` to `Ready`.**

The press is not lost. `mountPlay` takes an `onReady`, and the click that armed
the editor is replayed when the compiler lands — verified end to end in the
browser: one press on a cold page ends with `status: Ran`, the program's two
lines in the output pane, and `readOnly` gone.

**The five bars are one program out of RFC-0104's eight, and the page says so
in the sentence above them.** `spectralnorm`, on all five contestants, fastest
first, read out of the same committed record `/compare` draws its radar from —
`chart.vyrn` calls `bench.vyrn`'s `medianUs`, and nothing is transcribed. The
row was chosen because it is the numeric kernel and the one where Vyrn's native
leg is first: 955 ms against Rust's 961 and C's 963, with node at 1339 and the
Vyrn wasm leg at 2824. A page that showed that row and said nothing else would
be picking its evidence, so the line above the plate reads "one of eight. Five
of the rest are slower than Rust, each with a named cause" and the link under it
goes to all eight and the five causes. The environment is a `<details>` inside
the plate, not a caption under it.

**The row names are labels and not links, and that is a checklist row.** Every
other bar chart on this site links each row at its own source. An `<a>` inside
an `<svg>` has no CSS box for padding to grow, so five rows were five sub-24px
targets — all five pointing at the one page the notice under the plate already
names. Dropping them took the index from **six interactive targets under 24px
to one, and that one is a word in a sentence.**

**Four pillar cards are written out one by one rather than looped, and the
reason is RFC-0107 M1.** A provider tag takes static attributes; `:name` binds
an expression and is refused at the tag. Four cards that differ in every field
are four elements, and each carries its own `<Icon>`. The install tiles went
the other way for a measured reason: the two hash-locked collections this site
binds hold 180 glyphs between them and **none of them is an apple, a penguin or
a window**, so a per-OS glyph does not exist to be bound, and the tiles stay a
`v-for` with the OS named in words.

### A minute with Vyrn

**Seven steps, and every line of output in them came out of the real binary.**
`scripts/site-demo.py` makes a scratch directory, runs `vyrn new demo`, walks
what it wrote, runs `vyrn run`, writes one edit into `src/main.vyrn`, runs
`vyrn test`, runs `vyrn build -o demo`, runs the binary, and writes
`site/data/demo.json` — 1,598 bytes, version-stamped from `vyrn --version`. A
command that exits non-zero stops the recording rather than publishing it.

It is the `history.json` pattern, applied a second time and for the same
reason: RFC-0014 gives `site/export.vyrn` `readFile`, `writeFile` and `listDir`
and no way to start a process, so the site is HANDED the file and **refuses to
publish without it** (`missingDemo`, beside `missingHistory`). The committed
fixture is three obviously-fake steps stamped `vyrn 0.0.0-fixture`, and the
refusal is what keeps it off a published page.

Two steps carry no command, deliberately. The file listing is walked from the
directory rather than typed, so no invented `ls` output is attributed to a
shell. The edit is a file, shown as a file.

**The edit is a `Port` and not the hero's `Age`**, because the hero editor at
the top of the same page already shows `Age` and the same twelve lines twice on
one page is a page that repeats itself.

**The widget adds one thing and hides nothing.** All seven steps are in the
document as a numbered list, which is the whole page with no script at all. The
script hides six of them, builds the Back / Next / counter row — built rather
than shipped in the markup, because a Next button on a page that shows every
step at once is a control that does nothing — and binds the arrows. Verified in
the browser: `1 / 7`, right to `2 / 7`, two lefts wrap to `7 / 7`, Back disabled
at the first step, the counter `aria-live="polite"`, and the last step's Next
reads `Replay`.

### Install

**Three tiles, eight commands, 208 words.** No tab hides an operating system: a
reader on a phone deciding whether to install this needs to SEE that their
machine is one of the three. There is no `brew` line, no `apt` line and no
`winget` line, because there is no formula, no package and no manifest — a tab
for a method that does not exist is the same fabrication as a "used by" logo.

Uninstalling is one command in the flow of the page, the `.vsix` is one
sentence pointing at the editors page, and the source build is a `<details>`.
What left: five "commands worth knowing on day one" (they belong in Docs, and
`/docs` now links the guide's own first-program section instead), the
per-platform "You need" and "It downloads" paragraphs, and the three-paragraph
account of what the installer verifies, which is three rows now.

**A command on this page wraps rather than scrolls.** The sheet's default is
`white-space: pre` inside a scroller, which is right for a long build line in a
wide plate and wrong in a 300px tile: an install command clipped at
`…/vyrn/main/inst` is a stranger's script a reader is asked to pipe into a shell
without seeing the end of it. The install tiles, the index's install tabs and
the pillar cards wrap; nothing else changed.

### The numbers, against M0's ceilings

| Page | Words | Ceiling | Bytes | Ceiling | Cmds | Floor | `.cap` | Ceiling | `.note`/`.notice` | Ceiling |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| index | **257** | 260 | **17,675** | 30,000 | **12** | 5 | **2** | 2 | **2** | 2 |
| install | **208** | 220 | **8,780** | 14,000 | **8** | 8 | **1** | 1 | **2** | 2 |

Both word figures INCLUDE the shell, which is 51 words of every page on the
site — the skip link, five nav names, three controls, the overlay's two lines
and the footer's four. The index's own prose is 206 of its 257 and the install
page's is 157 of its 208.

One of the four `.cap` elements the two pages carry is the shell's own, on
every page of the site. Of the three that are the pages', two are
`<details>` — the index's `Without script:` fallback is the only caption in
flow on either page, and it is there because a `<noscript>` has to say
something.

### The page-weight ceiling M1 recorded as out of reach

**53,824 bytes gzipped on a cold load of `/`, against M0's 55,000.** M1
measured 87,716 and wrote that the ceiling "is further away than when M0 wrote
it". Three items closed the gap, and none of them is a compression trick:

| | Gzipped | How |
|---|---:|---|
| M1's `/` | 87,716 | |
| the navigator pair | **-20,275** | `vyrn-nav.js` and `vyrn-dom.js` are fetched on the first `pointerover`, `touchstart` or `focusin` on a link, not on load |
| `hero.js` | **-4,056** | the canvas left the index, so the module is deleted and so is the CI step that built `hero.wasm` |
| three widgets | **-3,600** approx. | the parity, ownership and types widgets are 235 lines of `widgets.js` for three plates that are no longer on any page |
| the sheet | **-1,147** | M1's lead, acted on: `.lines`, the ownership lanes, the parity columns, the hero canvas, `.metrics`, `.feed`, `.exports`, `.pill.warn`, `svg.schematic` and `button.danger` |
| the document | **+387** | 17,675 bytes against M1's 18,219, and the new page carries a program, a chart and seven recorded steps |
| **measured** | **53,824** | |

The navigator deferral was M1's to defer and M0's to propose — the census's own
asset table says `vyrn-nav.js` is needed "after first paint, or on first link
hover". A hard navigation is the declared fallback and it is what happens
before the module lands, so the change is which event fetches it and nothing
else. Verified: `/` loads with no `vyrn-nav.js` in the request list, and one
`pointerover` on a link in the content puts both files there.

**`site/public/style.css`: 87,731 raw and 26,739 gzipped, against 90,000 and
27,000.** And M2 found the gzipped half of that ceiling was never asserted:
`site/test/typescale.test.mjs` checked the raw number only, and the raw number
is the one that cannot fail first — this sheet is 48% comment prose, which
compresses, so a page of new rules moves the gzipped figure much further than
the raw one. **Measured at 27,170 gzipped with 89,272 raw and the test still
green.** Both halves are asserted now, the way `search.json` has been since M1.

### Accessibility, and how it was checked

The browser pane in this session would not composite frames, so no real key
press or click reached the page. Layout, the request list and the DOM are
computed either way — which is why every geometry above is trustworthy — and
every keystroke below was an event dispatched at the element a reader's key
would reach, through the real listeners. Screenshots were taken with a headless
browser against the same tree instead.

| Row | Result |
|---|---|
| the hero editor has a keyboard path to activate | pass — `focusin` on the textarea arms it; the module lands and the status reads `Ready` |
| the hero editor has a keyboard path to run | pass — the Run button is an enabled `<button>` in the tab order, and `Ctrl`+`Enter` in the editor runs, both `mountPlay`'s own |
| the editor is not editable before it can be checked | pass — `readonly` until arming, removed by it |
| the demo stepper is keyboard-operable | pass — left and right arrows, wrapping, Back disabled at the first step |
| the demo announces the step it moved to | pass — the counter is `aria-live="polite"` |
| the demo works with no script | pass — seven steps in the document as a numbered list; the controls are built by the script that needs them |
| the OS tabs are a tab list | pass — `role="tablist"`, roving `tabindex`, arrows, Home and End, from the shared `tabsWidget` |
| the OS tabs work with no script | pass — no `hidden` in the markup, so all three panes are on the page |
| no interactive target under 24px outside running prose (row 29) | pass on `/` at 375 and 1280 — **one target under 24px on the whole page, and it is a link inside a sentence.** It was six |
| every wide block scrolls inside its own container (row 28) | pass — the environment table and the five-bar strip both, after both were found failing |
| the body never scrolls sideways (rows 26, 27) | **fail, and not this milestone's** — see below |
| both palettes | pass — every new element takes its colour from a token; `site/test/contrast.test.mjs` is green |

**Two blocks were found taking the document sideways at 375px and both are
fixed.** The environment table inside the bar plate's disclosure painted 769px
in a 343px column with no scroller over it. The five-bar strip was worse and
quieter: `svg.chart` is `width: 100%`, so a 904-unit viewBox in a 343px column
scales every glyph by 0.38 and a 9px axis label paints at **3.4px** — a chart
that is present, is not overflowing, and cannot be read. It scrolls at its own
size inside the plate below 640px now, which is the rule RFC-0105 already had.

**And one that is not fixed, because it is the shell.** At 375px in a headless
Chrome the masthead's `.tools` — Search, Install, and the theme control — is
4px wider than the row, so the document is 379px against a 375px viewport.
**It reproduces on `/compare.html`, which this milestone did not touch**, so it
is not the index's or the install page's. M1 recorded the same defect at 320px,
measured it as fitting at 375 "with 9px to spare" in a different browser, and
assigned the fix to M4 on the grounds that every fix is a decision about which
control loses its word. The number to add to M1's arithmetic: the row has 343px
and wants 369, and the variance between two browsers on the same markup is
13px.

### What M2 deleted, and why each one is a deletion and not a move

- **`site/public/hero.js` (242 lines), `out/hero.wasm`, and the CI step that
  built it.** The canvas was on `/` and nowhere else. The parity claim it
  illustrated is on `/compare` and in the harness that fails CI; the page that
  argued it is not the page a first-time reader lands on.
  `examples/herofield.vyrn` stays — it is a real example and `chart.vyrn`
  measures its module.
- **235 lines of `widgets.js`**: `parityWidget`, `ownershipWidget`,
  `typesWidget` and the two hash helpers under them. Three plates, one page,
  gone from it.
- **1,147 bytes of `style.css`**, all of it selectors that match nothing in the
  exported tree, in the browser modules, or in any template. Ten classes were
  audited against all three and ten came back dead; `.copyfail` was on M1's
  list and is NOT dead — `widgets.js` adds it on a failed copy.
- **The five "commands worth knowing on day one"** from `/install`.

The dead-code audit that found them is a script's worth of work and was run
once, by hand, over the 181 class selectors the sheet declares. It is not
wired into anything, and that is a gap M5 could close.

### Gates

- `python3 scripts/site-history.py`, `python3 scripts/site-demo.py --vyrn
  compiler/target/release/vyrn`, then `vyrn run site/export.vyrn out`: **175
  routes, 12 assets, 50 markdown twins, one index, one feed.** One asset fewer
  than M1: `hero.js`.
- the site's own test loop, declared against ran, over `site/export.vyrn` and
  all 27 modules in `site/app/`: **191 blocks declared, 191 ran.**
- `node --test site/test/*.test.mjs` over the exported tree: **31 tests, 31
  pass** — four more than M1, and the four are the sheet's gzipped ceiling and
  the three basepath mount points re-run against a tree with two rebuilt pages.
- `vyrn fmt --check` on `site/app/*.vyrn`, `site/guide/*.vyrn` and
  `site/export.vyrn`: clean. The two `.vyx` templates this milestone rewrote
  are hand-formatted — `vyrn fmt` does not read `.vyx`.
- `cargo test --release -p vyrn-cli --test rfc_index`: 4 passed.
- No compiler change. No `std/` change.

### Four links this milestone broke, and what happened to them

Removing four sections from `/` broke four inbound fragment links, and
`site/test/basepath.test.mjs` is what said so — the export's own link gate did
not, because it checks the links a page WRITES and these are written by other
pages. Each was repointed at the page that now carries the claim, rather than
at `/` with the fragment dropped:

| Was | Now | Written on |
|---|---|---|
| `/install#check` | `/guide/getting-started#first` | `/docs` |
| `/index#memory` | `/guide/ownership` | `/docs` and `/why-vyrn` |
| `/index#types` | `/play` | `/why-vyrn` |
| `/index#parity` | `/compare#numbers` | `/why-vyrn` |

`/why-vyrn` is M4's page and three of its sentences moved by one clause each.
That is the milestone reaching outside its ground, and it is recorded here
rather than left for M4 to discover.

## M3 — as landed

The milestone the RFC's own plan calls "reference landing and releases", widened
before it started by the user's reading of the same reference site the design
section takes its argument from: "Bun's one looks times more cleaner, compact,
without loads of text but information dense. Also our website has issues with
spacings/paddings and so on. Also old pages not updated." The first sentence is
THE RULE, which this file already had. The second is a defect nobody had
measured. The third is the milestone.

Every number below was produced by running something: `scripts/site-history.py`
and `scripts/site-demo.py`, then `site/export.vyrn`, then
`scripts/site-census.py` over the tree it wrote, then `node --test
site/test/*.test.mjs`, then a headless browser and a scripted geometry sweep
against the same tree served over HTTP at five widths in both palettes.

### The spacing defect, as a number

`.band` was `padding: var(--s6) 0 0` with `margin-top: var(--s6)`. Read that as
a reader sees it: **64px of nothing, then the hairline, then 64px more, then the
heading — and zero under the previous section's last block.** The seam sat hard
against one side and floated 64px off the other, on every consumer page, and
`.hero` had its own unrelated pair (64 top, 48 bottom). Nine pages, three
spacing regimes, and the "ocean of whitespace" was not one number being too big.
It was the asymmetry.

**One token replaces all of it.**

```css
--sect: clamp(32px, 4.5vw, 56px);

.band { padding: var(--sect) 0; border-top: 1px solid var(--rule); }
.hero { padding: var(--sect) 0; }
```

Sections TOUCH. The hairline is the seam, with the same air on both sides of it,
and **no section carries a vertical margin anywhere on the site** — which is the
half that cannot be re-broken by a page adding a `margin-top` in passing, because
the geometry assertion below counts them.

`clamp` IS the desktop/phone pair the brief asked for, in one declaration rather
than a token declared twice: 32px on a phone (the 4.5vw term floors below 711px),
56px from 1244px up, a straight ramp between, and no step at a breakpoint. That
is the first deviation from the brief and it is the shape of the answer rather
than a different answer.

### Two section patterns, and every consumer section is one of them

|  | Class | Left | Right |
|---|---|---|---|
| **Split** | `.band.split` | `.say` — kicker, 2-6 word heading, at most two short sentences, one link | the artifact |
| **Full** | `.band` | heading row, then a dense grid or one artifact | |

`.split` is `minmax(0, 2fr) minmax(0, 3fr)` with a `var(--s5)` gap, one column
below 1024px, in the markup's order — no `order` anywhere, so the reading order
and the DOM order are the same object. Three rules make it hold:

- **`.split > *, .split > * > :first-child { margin-top: 0 }`.** Every block on
  this site that can be an artifact — `.plate`, `.cards`, `.specs`, `.steps` —
  carries a top margin for the case where it FOLLOWS prose. At the top of a
  column there is no prose above it, and that margin is a dead strip. It was
  measured: with `align-items: stretch` on the index hero and `.plate`'s
  `margin-top: 32px` still applied, the editor column was **642px against the
  674px beside it**. The rule takes it to 0.
- **`.split > .say > p { max-width: var(--measure) }`.** Two sentences set to
  88ch beside a chart is the paragraph an artifact was supposed to replace.
- **`.split .specs { grid-template-columns: repeat(2, 1fr) }`.** Four stat tiles
  in a column are 2x2, not 3+1: `auto-fit` measures the track and not the count,
  and the fourth tile fell to a row of its own where it read as a stray.

**`.hero.split` stays 1fr/1fr and that is deliberate.** 2fr of a 1216px sheet is
486px, and a 67px display headline in 486px is four lines of two words. The
index's hero keeps the split M2 shipped; what M3 changed there is
`align-items: stretch` plus `.heroplay { display: flex }` and
`.heroplay .out { flex: 1 }`, so the **output pane grows into whatever the left
column leaves** instead of the plate ending 32px above it.

### The geometry, asserted rather than looked at

A sweep runs in the browser over ten exported pages at 320, 375, 700, 767 and
1280px, measuring computed boxes. Not DOM presence: every figure here is
`getBoundingClientRect` and `getComputedStyle` on the real layout.

| Assertion | Result |
|---|---|
| every `main > section` on a page has the same padding pair | pass — `56/56` at 1280, `35/35` at 767, `32/32` at 700 and below, on all ten |
| no section carries a vertical margin | pass — **0 of 52 sections**, ten pages, five widths |
| no section taller than its content plus its padding, x1.25 | pass — the worst ratio anywhere is **1.023** (`/play`, whose editor is `46vh`); the four M3 pages run 1.000 to 1.008 |
| the masthead is 64px at every width | pass — ten pages, five widths |
| no empty grid cell | pass — 0, over `.cards`, `.modgrid`, `.specs`, `.twocol` and `.plain` |
| prose no wider than its role allows | pass — the widest paragraph anywhere is **806px**, which is `--wide` (88ch, the sheet's INTRO role). The 999-1001px readings the sweep also returns are `.cap` elements, whose role is LABEL and whose declared width is their block's |
| no element wider than its own container outside a scroller | pass on the four M3 pages at all five widths |
| the body never scrolls sideways | **fail at 320 and 375, and not this milestone's** — see below |

**The one failure, and the proof it is inherited.** At 375px the document is
379px; at 320px it is **also 379px**. `/compare.html`, which M3 did not touch,
reports the same 379 at both. It is the masthead's `.tools` row — M1 recorded it
at 320px, M2 measured it at 375px and assigned the fix to M4 on the grounds that
every fix is a decision about which control loses its word. M3 adds one figure to
that arithmetic: **the overflow is a constant 379px and does not depend on the
viewport at all**, so the row is not shrinking below 379 and the fix has to
remove or wrap something.

### The reference site, measured rather than remembered

The user asked for four of its pages to be opened and compared side by side
before the layout was finalised, for DENSITY and RHYTHM and not for branding,
colour, type or text. Four numbers came back, and three of them changed
something.

| What | Reference, at 1280px | Here, before | Here, after |
|---|---|---|---|
| section padding | `112px / 112px`, `margin: 0`, `border-top: 1px`, section spans the full viewport | 56/56, margin 0, border-top — but the rule stopped inside the sheet's gutter | 56/56, and **the seam reaches the page edge** |
| reference index | 3 columns, no gap, `padding: 20px` inside each cell, one continuous rule grid | 3-4 columns with a 32px gap, so the hairline broke at every column | **no gap, padding inside the cell, one continuous grid** |
| install hero | centred, 64px heading, lede capped at 504px, four 112px square OS tiles | centred, lede at the container's 704px, three text pills 44px tall | lede at **32rem**, three equal tiles **8rem by 56px** |
| a docs content page | 577px column, ~69ch, 16.8px/27.3 (1.63), `h2` 56px above | `--measure: 65ch`, `1.65`, `h2` 64px above | unchanged — the tokens already agree |

**The seam.** A rule that stops inside the sheet's own gutter reads as an
underline on a block; one that runs edge to edge reads as the join between two
bands, which is what a section is. `.band::before` is absolutely positioned with
`left` and `right` at `calc(-1 * var(--gut))`, where `--gut` is a new token
holding the number `#root`'s side padding already used at all three widths — so
the line lands exactly on `#root`'s border box and never past it. A `100vw`
bleed would have added a scrollbar of its own, and this page already has one
overflow it did not cause. Measured after: the seam runs `0..1270` of 1280,
`0..365` of 375, `0..310` of 320, and the document width is unchanged at every
width. On a page with a rail the section starts after the rail and so does its
seam, which is right — the rail is a different column, not part of the band.

**The grid.** A column gap breaks the hairline between every pair of columns and
the block reads as three lists; cells that butt together with their padding
inside them make one continuous rule grid, which is the thing a reader scans
across. The whole 38-module library is about one screen now.

**And the one number deliberately NOT matched: 56px against their 112px.** Their
sections run 800 to 1,666px tall, so 112px of padding is 14% of the block. Ours
run 200 to 630px, where 112 would be more than half of some of them, and the
complaint that started this milestone was whitespace. At 56px our ratio is 19% —
already proportionally MORE air than the reference has. The rhythm that was
copied is the structure: symmetric padding, zero margin, a hairline seam at the
join. The number is ours.

### The index and install, tightened

- **The hero editor fills its column.** `.heroplay .out` was `max-height: 9em`
  against a fixed editor. It is `flex: 1` with a `4.5em` floor now, and the two
  hero columns measure **674px and 674px**. M2's own claim survives unchanged:
  the content still ends at **794px** of an 800px viewport under the 64px
  masthead.
- **The bench section is a Split.** Kicker, `As quick as C`, one sentence and one
  link on the left; the five bars on the right. The `.notice` under the plate is
  gone — the link that was in it is the section's own call to action now, which
  is one fewer `.note`/`.notice` on the page and one fewer line of furniture.
- **The four pillar cards were already equal height** — they are grid items in a
  stretched row — so the brief's "equal height" needed no change. The whitespace
  inside a card is `.cards p { flex: 1 }` bottom-aligning the command, which is
  the alignment and not an orphan. Recorded rather than "fixed".
- **The seven-step demo is Bun's layout, and it cost one line of JavaScript.**
  Each step is a grid: title, command and note in a 2fr column, the recorded
  output in a 3fr column beside them, stacking below 1024px. With script, the
  stepper no longer sets `hidden` on six of seven steps — it toggles a class, and
  the sheet collapses an inactive step **to its title**. So the seven-title
  outline is always on the page and one step is open, which is the thing a
  reader scans. `widgets.js` adds `.stepped` to the plate, so with no script
  nothing matches and all seven are open, exactly as before.
  Verified in the browser: `1 / 7`; Next to `2 / 7`; ArrowLeft from the first
  wraps to `7 / 7` with Next reading `Replay`; Back disabled at the first step;
  the counter still `aria-live="polite"`; seven titles visible at every step;
  **zero `hidden` attributes**. A click on a collapsed step opens it — pointer
  only, and deliberately: Back, Next and the arrow keys already reach every step,
  so it adds no function a keyboard cannot do.

### Install, rebuilt on the reference's shape

The user sent the reference's install page mid-milestone, so this page got a
second brief: a centred hero, an OS selector, ONE visible command.

**The picker is three radio buttons and no JavaScript at all.** A `<fieldset>`
with a `<legend>` — the native group, with the name three loose radios and three
labels do not have. The inputs are visually hidden and come first in the markup,
so `:checked ~` reaches both the labels and the panes, and **selection is by
position**: `.picker input:nth-of-type(n):checked ~ .ospanes > :nth-child(n)`.
The sheet never spells a platform's name, and a fourth platform is one `<label>`,
one `<div>` and one line of CSS. Verified in the browser with the script running
and with the mechanism inspected: `0--` at rest, `--2` after a press on Windows,
`-1-` after Linux, and the command text changes with it.

The three inputs are written out rather than looped, and the reason is a language
one worth recording: only the first carries `checked`, `v-for` has no way to put
an attribute on one iteration, and a bound value cannot express it either —
`checked=""` is still checked in HTML.

What else moved: the version badge is the kicker (`INSTALL · v0.1.0-alpha.1`),
the two reassurance lines are one lede, the checksum sentence is one `.eyebrow`
under the command with the release-notes link in it, and **uninstall, pinning a
version, checking by hand and the source build are one `<details>` called
Advanced**. The page ends in `Build something`: one command and four links.
Nothing invented — there is still no `brew` line, no `apt` line and no `winget`
line, because there is still no formula, no package and no manifest.

The `.tiles` ruleset that styled the three tiles this replaced is **deleted**: it
had one user on the whole site and the picker is not it.

### Releases

**Four stat tiles, and not one of them is typed.** Each is arithmetic over a file
something else wrote:

| Tile | Where it comes from |
|---|---|
| `106` pull requests merged since | summed out of `site/data/history.json` over the days after the baked tag's date |
| `604` test blocks in `82` files | counted by `scripts/site-history.py` the way CI's own floor counts them |
| `171` examples on `3` engines each | `app/facts.vyrn` over `examples/`, and `backendCount()` |
| `38` standard library modules | `app/facts.vyrn` over `std/` |

**Two tiles the brief asked for are not here, and that is the second deviation.**
The brief proposed platform count and asset count. A release's ARCHIVES and the
platforms they were built for live in the release on GitHub, and this repository
holds a tag and a date (`site/release.txt`) and nothing else about a release —
the feed says the same thing, in the same words, about release notes. A tile is a
number a reader trusts. A number nobody can recompute does not get one, so those
two are a link to the release page in the table instead.

**The highlights are the design records since the tag, for the same reason.**
Release notes are written on GitHub and are not in this tree, so the two-column
list under the tiles is every record that arrived after the tag — nine of them,
newest first, each with the status it holds today and a link to the record.
`arrivalList()` already had all of it; this page is its second reader.

Below: a table of every release (version, date, kind, and a link to that tag's
archives and `SHA256SUMS`), from `history.json`'s own tag list. And **the feed is
on the page** — `<link rel="alternate">` has pointed at `releases.xml` since M1
and no document had ever named it, which is a feed a person could not find.

`repo.vyrn` grew `releaseUrlOf(tag)`; `releaseUrl()` is one call to it now.

**One thing a local build shows and a published one does not.** `site/release.txt`
is refreshed from the GitHub listing by `.github/workflows/site.yml` before every
build; a checkout has whatever was committed. So on this machine the hero says
`v0.1.0-alpha.1` while the table, which reads git's own tags, lists
`v0.1.0-alpha.2` above it — and `fresh.js` puts the drift on the page in a
`role="status"` line, which is exactly what it is for. In CI the two agree.

### The reference landing

The module list was **38 full-width rows** of `name | count | sentence`: 38 lines
of scrolling to answer "is there one for X". It is a three-column grid under four
small-caps group headings now, and every row in it is generated —
`apiGroups()` reads `std/`, so a module added tomorrow is on this page on the
next build.

**The group names are the one editorial line, and that is the third deviation.**
The brief says no hand-maintained duplicate list, and there is none: the LIST is
`apiModules()`. What a generated table cannot know is that `jsonread` is data and
`tw` is web, so `groupOf(m)` in `app/docs.vyrn` names three sets and defaults to
`Programs and tools` — a module nobody classified lands in a visible group rather
than vanishing. `docs.vyrn`'s own test asserts the groups add up to
`apiModules().length`, so the failure mode that remains is a name listed twice
and not a name forgotten.

The rows keep their `data-q` and `data-e`, so M1's search still filters them
where they sit; `widgets.js` was not touched for it. A group whose rows are all
filtered out **hides its own heading** — `.modgroup:not(:has(li:not([hidden])))`
— because the script hides rows and knows nothing about groups. Each summary is
clamped to two lines: the whole sentence is still in the document for a screen
reader and for the search, and the module's own page has it in full.

The rest of the page: the search moved into the hero as its right column, the
`01 —` / `02 —` numbering left the section kickers, the two-paragraph "a book and
a registry" section folded into the `Elsewhere` split, and the `.note` class came
off the module summaries. **That last one alone took `/docs` from 42 `.note`
elements to 1** — M0 measured 40 of the site's 103 on this page, and they were
one CSS class on a generated row.

### The stylesheet stopped being a download

The sheet is 48% comment prose: which defect a rule closes, which checklist row
it answers, what was measured before it. M0 gave it **90,000 bytes raw and 27,000
gzipped**, and M2 landed at 89,272 / 27,170 — about 700 bytes of headroom. A
global section system does not fit in 700 bytes. Trimming M3's own comments to
half their length got the sheet to 95,818 / 29,120, which was still over both, and
`/` to **57,315 gzipped against a 55,000 ceiling**.

Raising two ceilings was the cheap answer. The honest one is that **the design
record is not the page's payload**, and this site already applies that rule to
JavaScript: the navigator pair is deferred, the playground moved to `/play`.

So `site/export.vyrn` strips CSS comments on the way to `out/style.css`. This is
the fourth deviation, and it is a change the brief did not ask for. It is not
minification: no name is shortened, no rule is merged, no whitespace inside a
declaration moves. Comments go, the blank lines they leave behind go with them,
and the scanner is **string-aware**, because `content: "/*"` is a legal
declaration and a scanner that did not know it would eat the rest of the file —
there is a test for exactly that case.

| | Raw | Gzipped |
|---|---:|---:|
| `site/public/style.css`, the source | 99,083 | 30,353 |
| `out/style.css`, what a reader fetches | **49,024** | **9,660** |
| M0's ceiling | 90,000 | 27,000 |

**20,693 gzipped bytes, off every page of the site**, and it is prose no browser
was ever going to render. `site/test/typescale.test.mjs` measures both ceilings
against the shipped form now; the source keeps no byte ceiling of its own, on
purpose — a comment budget is a budget on explaining yourself, and that was never
what the numbers were for. `contrast.test.mjs` still reads the source, and its
note that the copy is "byte for byte" is corrected in place.

Cold first load, gzipped, counting the document and every asset a browser fetches
without an interaction:

| Page | M2 | M3 | Ceiling |
|---|---:|---:|---:|
| `/` | 53,824 | **36,898** | 55,000 |
| `/install` | — | **34,662** | 55,000 |
| `/releases` | — | **34,405** | 55,000 |
| `/docs` | — | **43,693** | 55,000 |

The shared assets are 31,700 of every one of those, and `widgets.js` is 17,762 of
them — **56% of what every page of this site downloads is one JavaScript
file**, and the same argument applies to its comments. It is not stripped, and
the reason is that JavaScript comments are not a regular language a
forty-line scanner can find the end of. That is the next page-weight item and it
belongs to M5.

### The numbers, against M0's ceilings

| Page | Words before | Words | Ceiling | Bytes before | Bytes | Ceiling | Cmds | Floor | `.cap` | `.note`/`.notice` before | now | Ceiling |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| index | 257 | **253** | 260 | 17,675 | **17,715** | 30,000 | **12** | 5 | **2** | 2 | **1** | 2 |
| install | 208 | **197** | 220 | 8,780 | **9,250** | 14,000 | **8** | 8 | **1** | 2 | **2** | 2 |
| releases | 252 | **181** | 200 | 5,777 | **7,506** | 20,000 | **1** | 1 | **1** | 4 | **2** | 2 |
| docs (landing) | 907 | **640** | 644 | 54,262 | **52,815** | 40,000 | 0 | 0 | **2** | 42 | **1** | 2 |
| **all thirteen** | 6,266 | **5,913** | | 229,989 | **230,781** | | 24 | | 44 | 91 | **47** | |

Every word ceiling is met. Of the `.cap` elements on the four pages, **one on
each is the shell's own**, which is on every page of the site — so `/index` and
`/docs` carry one caption apiece and `/install` and `/releases` carry none.

**`/docs` is still over M0's byte ceiling: 52,815 against 40,000.** It was 54,262
before M3 and no milestone has ever met it. The cause is measurable and is not
prose: **the import graph's inline SVG is 25,347 bytes of the page, 48% of it.**
Either the graph moves off the landing or the ceiling was set without the graph
in view; both are decisions, neither is M3's to take alone, and the number is
here so M4 takes it with the figure in front of it.

### Pages M3 did not touch, and how they took the new rhythm

The rhythm is global, so `/compare`, `/why-vyrn`, `/guide`, `/explore` and
`/play` render under it without being rewritten. All five pass the sweep at 375,
767 and 1280: uniform padding, zero section margins, a worst height ratio of
1.023, and no overflow beyond the inherited masthead one. Three things for M4,
found by looking rather than by breaking:

1. **`/guide`'s chapter list carries 14 `.note` elements** — the same class on a
   generated row that `/docs` just shed, one per chapter, against a ceiling of
   two a page. The fix is the one M3 already made next door.
2. **`/compare` reports 14 elements wider than their own container at 1280px.**
   The document does not scroll sideways there, so these are inner blocks
   without a scroller over them. M0 recorded the page breaking the rule at both
   audited widths for two unrelated reasons; this is a third figure for the same
   entry.
3. **`/philosophy` and `/editors` measure 64 words each** in the census table,
   because they are redirect stubs now and the table in M0 still names them as
   pages. The census's `PAGES` list wants `/why-vyrn` and `/docs/editors` in
   their place; M5 owns it, since M5 is what wires the census into the build.

Two smaller ones fixed in passing, both found by the sweep: a `.cta` inside a
content row was `white-space: nowrap` inherited from the masthead's Install
button and painted **286px in a 278px column at 320px** — a CTA in content wraps
now; and `details.disc > summary` was a **19px** target against the checklist's
24px floor, everywhere on the site, which is one `min-height` and applies to
every disclosure this RFC has added since M1.

**And one refusal this record earned on its own.** `app/markdown.vyrn` refuses a
fenced block in a language it does not know, "so the next language somebody
reaches for is a decision somebody makes" — its own words. This section quotes
six CSS declarations, the export failed on `/backstage/rfcs/0106` and printed
`REFUSED RFC-0106: a fenced code block tagged \`css\``, and `css` is on the list
now. It renders escaped and unstyled, like `yaml`. The mechanism worked exactly
as designed: a record could not grow a construct the site renders wrongly without
somebody being told.

### Gates

- `python scripts/site-history.py`, `python scripts/site-demo.py --vyrn
  compiler/target/release/vyrn`, then `vyrn run site/export.vyrn out`: **175
  routes, 12 assets, 50 markdown twins, one index, one feed.**
- the site's own test loop over `site/export.vyrn` and every module in
  `site/app/`: **31 blocks declared, 31 ran** — one more than M2, and it is the
  stylesheet stripper's.
- `vyrn test site/app/docs.vyrn`: 8 passed, including the new "the groups hold
  every module, once". `vyrn test site/app/stdgraph.vyrn`: 6 passed.
- `node --test site/test/*.test.mjs` over the exported tree: **31 tests, 31
  pass.**
- `vyrn fmt --check` on `site/app/*.vyrn`, `site/guide/*.vyrn` and
  `site/export.vyrn`: clean. The four `.vyx` templates are hand-formatted —
  `vyrn fmt` does not read `.vyx`.
- `cargo fmt --all --check`: clean. `cargo test --release --workspace`: **1,760
  passed, 0 failed.** `cargo test --release -p vyrn-cli --test rfc_index`: 4
  passed.
- **No compiler change and no `std/` change. Zero Rust files touched.**
- 48 screenshots, four pages by five widths by two palettes plus a full-length
  page each, in `shots/` (gitignored).

## M3 — the second round, after the pages were rejected

The user read the screenshots of everything above and rejected it: the defects
they listed were "only a small portion of issues". They were right, and the
reason the first round missed them is worth writing down — **the geometry
assertions all passed.** Uniform padding, no dead columns, no empty cells, no
overflow beyond an inherited one. What a geometry assertion cannot see is a
command wrapped across four lines, a control that inherited the wrong font, or a
page featuring a version that is three days stale. A page can be geometrically
correct and still be badly made.

So the second round is an ADVERSARIAL CENSUS rather than a checklist:
[`rfcs/census-0106-m3-craft.md`](census-0106-m3-craft.md), written before
anything was fixed, page by page against the four reference pages the user
named. **34 entries: 27 fixed, 3 kept as not-defects, 4 deferred.** The counts
and the deferred list are in that file. What follows is what changed and what
the fixes taught.

### A command is one line

M2 made four places wrap, to stop a long install line being CLIPPED. Measured on
the exported tree with real line boxes rather than height arithmetic, that gave:
the index hero's three commands at two lines each, `vyrn build … --target wasm`
at **four** lines in a pillar card, and the `Copy` buttons beside them at **50,
73, 95 and 118px tall on one page**, because each stretched to its own wrapped
command. A shell command broken across four lines is one a reader cannot tell
they have copied whole.

`white-space: pre` inside a scroller, everywhere, and the `pre-wrap` ruleset is
deleted. Where the command still would not fit, the BOX grew rather than the
command breaking: the install hero is 50rem now and the command sits in it
whole, while the sentence above it stays at a centred 32rem — which is the
reference site's shape and the reason its command never wraps either. The four
pillar commands were shortened to lines that fit a 280px card and still run as
written.

### One picker, joined to the thing it selects

There were two, doing the same job with different shapes and different keyboard
models: underline tabs driven by `tabsWidget` on the index, the CSS-only radio
picker on `/install`. And on `/install` the tiles floated 24px above a command
box of a different width, so they read as chips over an unrelated block.

One component now, on both pages: equal columns, shared borders, and the command
box's top border sitting on the tile row's bottom one — a segmented control
joined to its own output. `installPicker` and its `tabsWidget` call are gone;
what replaced them is three lines that check the radio matching the visitor's
OS, over a control that already works with no script at all.

### The stale version, fixed where it came from

`site/release.txt` said `v0.1.0-alpha.1` on 2026-08-21, three days after
`v0.1.0-alpha.2` was published — so `/`, `/install` and `/releases` all featured
a version that was not the one the install command fetches. A `role="status"`
notice apologized for it on two of them at run time.

The tree already knew: `scripts/site-history.py` writes every git tag into
`site/data/history.json`, and the site refuses to build without that file.
`repo.vyrn` reconciles the baked file against it and the newer tag wins, so a
stale `release.txt` cannot reach a page; the file is refreshed too, and
`site/export.vyrn` asserts the two agree — plus that **no page but the release
table and the backstage timeline names a tag that is not the newest**.

That import closed a cycle: `history` → `corpus` → `markdown` → `repo` →
`history`, and the link that closed it was `markdown.vyrn` wanting one URL
builder. So the three GitHub URL builders moved down into `app/github.vyrn`, a
leaf that imports nothing, and `repo.vyrn` re-exports them — every other caller
on the site is unchanged and there is still one place that spells the
repository's name.

**And the notice is deleted, and `site/public/fresh.js` with it.** A page that
cannot be stale has nothing to apologize for. That removes an asset, 1,632
gzipped bytes from every page, and a request to `api.github.com` on every visit
by every reader — which the site was making solely to cover for a file somebody
forgot to update.

### Lines that did not earn their place

The archive filenames under each install command (a reader installing does not
need `vyrn-aarch64-macos.tar.gz`; the PATH it lands in is the useful half, and
that moved into Advanced). `v0.1.0-alpha.1, published 2026-08-11 as a
pre-release`, which restated the kicker three lines above it. `Checksum-verified.
v0.1.0-alpha.1 today.`, where "today" told a reader nothing they had asked. The
two empty status notices. And the `01 — `, `02 — ` numbering on 24 section
kickers across 17 consumer templates, which the rail beside them already carries
and which the M3 pages had already dropped.

### Seven control heights became four

Measured at 1280px: 44 (masthead), 56 (OS tile), 33 (chip), 32 (button), 29
(content CTA), 27 (radar key), 25 (pill). The worst was `.cta`: it declared no
font and no box, so in the masthead it inherited that row's 13px mono and a 44px
target, and in CONTENT it inherited the body's **17px sans** and came out 29px
tall — the same class rendering as two different controls, and the one on `/`
and `/install` the odd element out among 12px mono buttons. It carries its own
font and a 40px box now. `.pill` and `.chips li` are one static-label box.

### The header row, which three milestones had deferred

M1 recorded it at 320px, M2 measured it at 375px and assigned it to M4 "on the
grounds that every fix is a decision about which control loses its word". M3's
first round repeated that.

It is fixed, and mostly as a side effect. Giving `.cta` a 12px mono font took
the row from **59px over at 320px to 54**. Hiding the search control's WORD at
phone widths — its glyph stays and its accessible name is on `aria-label`, so
nothing is lost to a screen reader — took it to **2**. The Install button's side
padding took it to **0**.

**Every consumer page now has zero horizontal overflow at 320, 360, 375, 767 and
1280px: 45 page-width pairs measured, 45 clean.** It was the oldest open defect
in this RFC.

### Two traps the verification found, both the fixes' own

Recorded because both are the kind that pass every assertion until the exact
width that breaks them:

1. **A flex item's automatic minimum is its min-content.** With `white-space:
   pre` restored, `.cmd code`'s min-content is the whole 79-character command, so
   the box refused to shrink and took the document to **706px inside a 320px
   phone**. `min-width: 0` is what makes a flex scroller actually scroll.
2. **`margin: 0 auto` on a grid item is not centring, it is `justify-self:
   center`** — and a non-stretched grid item is sized `fit-content`, so
   `.hero.mid` took the min-content of the widest thing inside it and painted
   690px in a 278px column. An explicit `width: 100%` fixes it and the auto
   margins still centre it wherever the track is wider.

### The numbers, second round

| Page | Words | Ceiling | Bytes | Ceiling | Cold load, gzipped | Ceiling |
|---|---:|---:|---:|---:|---:|---:|
| index | **254** | 260 | 17,860 | 30,000 | **35,419** | 55,000 |
| install | **201** | 220 | 8,999 | 14,000 | **33,073** | 55,000 |
| releases | **169** | 200 | 6,666 | 20,000 | **32,768** | 55,000 |
| docs (landing) | **640** | 644 | 52,841 | 40,000 | **42,167** | 55,000 |

`.note`/`.notice` across the thirteen census pages: **45**, from 91 before M3.
The published stylesheet is 49,459 raw and 9,742 gzipped against 90,000 and
27,000.

### Gates, second round

- `vyrn run site/export.vyrn out`: **175 routes, 11 assets** — one fewer than the
  first round, and it is `fresh.js`.
- the site's own loop: **32 blocks declared, 32 ran** — one more, and it is the
  version assertion.
- `node --test site/test/*.test.mjs`: **31 tests, 31 pass**.
- `vyrn fmt --check`, `cargo fmt --all --check`: clean.
  `cargo test --release -p vyrn-cli --test rfc_index`: 4 passed — after the
  census file was added to `rfcs/README.md`, which is what that test is for.
- the geometry sweep, plus two assertions the first round did not have: **no
  wrapped line inside any command block** (real line boxes, not height
  arithmetic) and **no page names a tag that is not the newest**. 45 of 45
  page-width pairs clean.
- 51 screenshots in `shots/`, including a 2x capture of each hero, because the
  defects this round found are the ones that only show at full size.

## M3 — the third round, and the reference site as a source rather than a model

The user read the second round and sent eight findings. The census file carries
them row by row (section J); this is what they changed and the three judgment
calls they forced.

**The general principle, in the user's words: we take the reference site's good
decisions, we do not copy wholesale.** Where one of its patterns meets one of
this site's constraints — no-JavaScript discipline, square-cornered mono brand,
static hosting with no server — the constraint wins, and the disagreement gets
written down here. Three of them did, below.

### The `Site / build` job, which was failing for a reason nobody had guessed

It was not the workflow. The first step of that job is
`cargo build --release -p vyrn-cli`, it took 62 seconds, and the three steps
between it and the failure ran the binary it produced by exactly the path the
failing step passed. The bug is one line of `scripts/site-demo.py`:

> Every recorded step runs in a scratch directory, and **on POSIX a relative
> program path is resolved against the CHILD's working directory.** Windows
> resolves it against the parent's, which is why the script had always worked
> where it was written. `shutil.which` does not save you: a path with a
> separator in it comes back unchanged — measured, it returns
> `compiler/target/release/vyrn` verbatim.

One `os.path.abspath`, and it is the root cause rather than the symptom: the fix
is in the function that decides what to execute, so it holds for every step and
on every platform.

**Where the seventeen minutes went**, measured on the failed run: 62 s for the
CLI, 25 s for the playground's wasm build, 45 s for its host tests — and
**907 s in `The site's own tests`, of which 687 s is `vyrn test
site/export.vyrn` alone** (32 blocks, most of them rendering pages, over 175
routes). A cache takes the 132 s of cargo off a warm run and cannot touch the
rest, so two things changed and the job says both out loud:

- `Swatinem/rust-cache` on its own slot for the two workspaces this job builds,
  which is the pattern `ci.yml` already uses and gives the reason for.
- The demo and history steps moved **above** the fifteen-minute test loop. A
  missing export input now fails the job in two minutes instead of at minute
  seventeen. That is not a speed fix; it is a feedback fix, and it is free.

The remaining pole is the interpreter's, not this workflow's, and the step now
says so where the next person will look.

### One command, two tabs, and the site's own origin

The picker had three tiles and two commands: macOS and Linux showed a reader the
same `curl … | sh` line twice, so the choice it asked for was not a choice. The
question is which SHELL — a POSIX one or PowerShell — so there are two tiles now,
`macOS & Linux` and `Windows`, matching the reference site's own scheme.

**The install command names this site.** `install.sh` and `install.ps1` are
copied out of the repository root into the published tree by `assets()` in
`site/export.vyrn`, beside the stylesheet, so:

    curl -fsSL https://vyrn-lang.github.io/vyrn/install.sh | sh
    powershell -c "irm https://vyrn-lang.github.io/vyrn/install.ps1|iex"

**The base URL decision, and why it is not a display trick.** The origin is
`https://vyrn-lang.github.io/vyrn/` — the deployed Pages URL, read off the
repository's Pages configuration, `cname: null`. It is 43 characters against the
62 of `raw.githubusercontent.com/vyrn-lang/vyrn/main/install.sh`, and it is the
site's own name rather than a hosting detail of where the source is kept. The
brief allowed showing one URL and fetching another; that was refused. What is
displayed IS what is fetched, because a command a reader cannot verify by reading
it is worse than a long one. The risk that argument has to answer — a Pages
deploy lagging `main` — is answered by the installers themselves: neither takes a
version, both resolve the newest release at run time, so an hour-old copy of the
script installs exactly what `main` would.

There is now exactly one place that spells either command: `installTargets()` in
`site/app/repo.vyrn`, beside `siteOrigin()`, with a test that asserts the bytes.
The index hero and `/install` both read it. They had a copy each before, which is
how one of them ended up with the `data-os` attribute the OS guess needs and the
other without it.

`siteOrigin()`'s own note changed with it: **three** things on this site cannot
be a relative URL, not two. The feed, the markdown twin, and now the install
command — which is typed into a terminal that has no idea what page it came from.

**And `Checksum-verified.` is deleted from both heroes.** It was the last line of
a hero, under a command, in 12px capitals. The verification story is a whole
section of `/install` with three claims and two links in it, and that section is
where a reader who cares about it is going. A hero does not get to make the
argument twice.

Beside each command is `View the install script`, pointing at the file the
command fetches — the reference site has one and it is a good idea, and here it
costs nothing extra because the site is already serving that file. It is a
site-root link, so `basepath.test.mjs` fetches it: a rename that forgot
`assets()` fails there.

### The changelog left the masthead

`/releases` carried its own `curl … | sh` block: one platform's command, on a
page that is not about platforms, duplicating the page that is. It is one link
now — `Install or upgrade` — and the page is what its name says: the version and
its date, the four computed stat tiles, the design records that arrived since the
tag, then every release ever tagged.

**The row is gone too, and the navigation is four names.** Nobody navigates to a
changelog before they have installed anything, and it was holding a permanent
quarter of a row that four pages a reader does want are in. Three doors replace
it, and each is where somebody would actually look:

- a `Latest release` section on the index — kicker, the tag, the date line, two
  or three highlights, one link — which is the good half of the reference site's
  blog idea for a project that has no blog. Every line of it is computed: the tag
  and date are baked, the highlights are the design records after the tag with
  the status each holds today.
- `Release notes` on `/install`, which used to point straight at GitHub and now
  points at this page, which links GitHub.
- a footer link on every page.

`site/export.vyrn`'s masthead-marker assertion moved `/releases` into the group
of pages that mark nothing, which is the same list `/compare` and `/why-vyrn` are
in and for the same reason.

### THE PATTERN WE DID NOT TAKE

Three, and each is a constraint winning:

1. **A blog.** The reference site's release notes are posts. Ours are written on
   GitHub and are not in this repository, so a blog here would be a directory of
   files somebody has to remember to write, and the honest version of "what
   changed" is the design record — which this repository does have, dated, with a
   status. The index section is the shape of the idea; the content is ours.
2. **A short, brandable install URL.** `bun.sh/install.ps1` is 21 characters
   because it is a domain. We have no domain, and inventing a redirector to fake
   one is infrastructure with an owner and an expiry date. So the command is
   longer than theirs, and in the index hero's 523px column the PowerShell line
   scrolls by 67px inside its own box (census H7, unchanged — `/install` shows
   both whole at 736px, one click away). A scroller a reader can drag beats a
   pretty URL that can lapse.
3. **A JavaScript picker.** The OS tabs stay two radio buttons, a `:checked ~`
   selector and no script. What the script adds is a GUESS, three lines of it,
   and the no-script default is the first tab.

### The OS guess, which the second round claimed and did not have

Two things were wrong, and both are the kind that pass review:

1. `guessOs` looked for `input[data-os]`, and only the INDEX carried that
   attribute. On `/install` — the page a reader installs from — there was nothing
   to match, so it silently did nothing. That is what happens when two pages keep
   their own copy of one control.
2. It read `navigator.platform`, which is deprecated and frozen. It still answers
   `Win32` in Chrome, but a browser reporting the frozen `Linux x86_64` answers
   wrong, and there is no reason to prefer it to `navigator.userAgentData`.

It asks the question the picker asks now — PowerShell or a POSIX shell — off
`userAgentData.platform`, then `platform`, then the user-agent string, and
anything that is not Windows takes the first tab, which is also the no-script
default. So a wrong guess costs one press and never a wrong command. Verified in
the browser by overriding the platform the page reports, in both directions,
against the shipped `widgets.js`: `shots/install-autodetect-unix.png` and
`shots/install-autodetect-windows.png`.

### The search box, which had never scrolled

`.findpanel [data-search-results] { overflow-y: auto }` — and the markup carries
`data-find-results`. For three milestones nothing in that panel scrolled: a query
with forty results drew all forty, the panel clipped at `70vh`, and every row
past the edge was unreachable with the mouse AND with the arrow keys, which walk
a selection the reader cannot see. One attribute name, and `min-height: 0` beside
it for the same reason `.cmd code` needs one.

**The white flash in dark mode was a token inversion.** The scrim was
`color-mix(in oklab, var(--n0) 45%, transparent)`, and `--n0` is the INK:
near-black in the light block, near-white in both dark ones. So the dialog that
dimmed the page in light mode washed it WHITE in dark mode. It is a `--scrim`
token now, declared in all three palette blocks — a scrim darkens what is behind
it in both palettes, which makes it not a neutral-ramp alias. Fixed at the token
level, as the instruction asked, with no JavaScript involved.

Everything else about that overlay was measured working and left alone: `/` opens
it, Esc closes it and returns focus, the arrows move `aria-selected` while focus
stays in the field, Enter follows the row, the ground closes it and the panel
does not, and with no script there is no overlay and the `<noscript>` says where
to go.

### The glued-spacing class, swept

`.eyebrow` is `margin: 0 0 8px` — it is written to sit OVER something. Used as a
footnote UNDER a command box or a stat grid it inherited that zero and its first
line sat on the border above it, which is the defect the user saw on `/releases`.
It had been patched once, locally, with `.hero.mid > .eyebrow`.

Four rules replace that patch, all in the section-rhythm block, and each one was
written against a measurement:

| Rule | What it was fixing |
|---|---|
| `.band > * + .eyebrow`, `.hero > …`, `.say > …` | a label used as a footnote, on three section shapes |
| `.band > * + p:not(.cap, .eyebrow)` | `/compare`'s `The chart, as a table.` touching the plate above it |
| `.band > p + .scroller`, `.band > p + table` | and the table below it. `/editors` had the same pair |
| `.band > .cap + :not(.cap)` | three `<h3>`s sitting on a caption's last line on `/explore/shelf` |

`.cap` is exempt on its own top side, and that is a decision: it carries its own
`border-top` and padding and is BUILT to sit flush with the block above it. That
rule is what says the caption belongs to that block.

**And the sweep found one the user had not:** `.split > * { margin-top: 0 }`, the
rule that exists to kill the dead strip at the top of a right column, is
specificity (0,1,0) and lost to every `element.class` selector in the sheet.
`ul.plain` is (0,1,1) — so on three pages the right column started 24px below
the left, and no amount of moving the rule could fix it, because a
lower-specificity rule loses wherever it sits. The class is written twice now,
`.split.split > *`, which is (0,2,0).

### A new assertion, and the one it cannot be

The sweep gained a minimum-gap check: **for every pair of stacked neighbours
inside a `<section>` or a `.say` column, the gap is at least 8px.** 26
page-width pairs — thirteen pages at 1280 and at 375 — all clean, after the four
rules above.

It is not a committed test, and the reason is worth recording rather than
apologizing for. Every assertion in `site/test/*.test.mjs` reads the exported
BYTES; a gap is a layout fact and measuring one needs a layout engine. Adding a
headless browser to this repository to assert 8px would be a dependency the
language does not otherwise have — so the sweep stays a browser script and its
numbers live in the census.

### The numbers, third round

| Page | Words | Ceiling | Bytes | Ceiling | Cold load, gzipped | Ceiling |
|---|---:|---:|---:|---:|---:|---:|
| index | **260** | 260 | 17,872 | 30,000 | **36,015** | 55,000 |
| install | **208** | 220 | 8,873 | 14,000 | **33,616** | 55,000 |
| releases | **166** | 200 | 6,629 | 20,000 | **33,199** | 55,000 |
| docs (landing) | **640** | 644 | 53,061 | 40,000 | **42,632** | 55,000 |

The index is at its ceiling to the word, and that is what the release section
cost: it arrived at 261 and `Every release` became `Releases`. The section it
replaced was a heading and three buttons repeating three links already on the
page, which is where most of the room came from.

The published stylesheet is 49,645 raw and 9,809 gzipped against 90,000 and
27,000. `/docs` is still over M0's byte ceiling by the width of its import graph
(census H4, M4's decision).

### Gates, third round

- **the workflow's own command sequence, run locally in its own order**: history,
  the output directories, the demo recording, the site's test loop, the guide's
  programs, `fmt --check`, the render, the node tests. Every step green, and the
  demo step — the one that failed in CI — is green with the same relative
  `--vyrn` argument the workflow passes.
- `vyrn run site/export.vyrn out`: **175 routes, 13 assets** — two more than the
  second round, and they are `install.sh` and `install.ps1`.
- the site's own loop: **196 test blocks declared, 196 ran** (one more than the
  second round: the install command's bytes).
- `node --test site/test/*.test.mjs`: **31 tests, 31 pass**, including the
  base-path fetch of every URL every page names — which now includes the served
  install scripts.
- `vyrn fmt --check`: clean.
  `cargo test --release -p vyrn-cli --test rfc_index`: 4 passed.
- the geometry sweep at 1280 and 375 over thirteen pages: no horizontal overflow,
  no section taller than its content plus padding, no wrapped command line
  (compared against the newlines the command is WRITTEN with, so a three-command
  block is no longer a false positive), and no inter-element gap under 8px.
  **26 of 26 page-width pairs clean.**
- 55 screenshots in `shots/`, the 51 of the second round re-taken plus four new
  ones: the search overlay open in both palettes, and the OS guess proved in both
  directions.

## M3 — the fourth round, read off the deployed tree

The user served the export and read it. Seven findings, and six of them are the
same shape as the third round's: **a fix that was made once, locally, where the
general form was already written down.** That is worth naming as a pattern,
because it is now the most common defect this milestone produces.

The census carries them row by row (section K). What follows is what changed and
the four things the measurements corrected.

### A geometry that served two states and was right in the rarer one

The recorded demo card. Every `li` was its own 2fr/3fr grid with that step's
output in column 2 — which reads as "what you typed | what came back" in the
no-script case, where all seven steps are open. Under the script exactly one step
is open, so the same grid painted a command in column 1, its output floating
top-right of an otherwise empty row, and six flat title bars underneath, with the
provenance line landing on the last of them.

**The card is the split now, not the row.** Seven numbered rows in column 1,
always whole, the marked one carrying `aria-current="step"`; the output in
column 2. With no script every pane is stacked in column 2 under its own title
and the sheet's `.stepped` rules match nothing — the fallback is a different
arrangement of the same content, not a degraded copy of one. The provenance line
is a `.cap` under the card, which is the class built to sit flush with the block
above it.

Two things this fix's own verification found. At `flex: 1` the output pane made a
600px empty terminal for a one-line step and pushed that step's note to the
bottom of the card, 500px away from what it is about — it is `flex: 0 1 auto`
with a 9em floor and a 34em ceiling. And `<p class="note">` on seven steps took
the index's disclosure count from 1 to 8, because `.note` is one of the two
things the census counts as a disclosure and M0's rule is one per plate.

### Text in a viewBox is not text at a size

**Every chart on the site was under 12px, and most were under 9.** Measured:
6.6–8.0px on the index at 1280, 3.0–3.6px on `/compare` at 375, 3.0px on the
backstage's pulse. The cause is that text inside a `viewBox` is drawn in USER
UNITS and scaled with the picture, so `--t-axis: 9px` is a number the container's
width multiplies — by 1.07 in a full band, 0.73 in a split column, 0.33 on a
phone.

The site already had the answer, on one plate. `.barstrip .stage { overflow-x:
auto }` with a `min-width` has been on the index since M2 and its comment states
the rule in general terms — "a 904-unit viewBox in a 343px column scales every
glyph by 0.38 and a 9px axis label paints at 3.4px". It was never applied to the
other nine charts.

So the scale is bounded at both ends, at the level every chart shares:

- `max-width` per family at each generator's own viewBox width, so no chart is
  ever drawn LARGER than its units.
- `min-width` at `12/18` of that width, so none is drawn smaller than the scale
  at which 12 real pixels survive; under it the chart scrolls inside its own
  `.stage`, which is this sheet's rule for any block wider than its column.
- two type steps, `--t-svg: 18px` and `--t-svg-s: 17px`, replacing `--t-axis`,
  with `site/test/typescale.test.mjs` extended to name them.

**The correction the measurement forced:** the floor was first written inside the
phone media block, and a 700px window fell between the two — no floor, and 11.4px
on six charts. A media query cannot see the CONTAINER's width, which is the only
thing the scale depends on. A `min-width` can, so the rule is unconditional and
at 1280 no container on the site is under its own floor.

Every chart is 12.0 to 18.1px now, and the sweep asserts it: **no `text` in any
`svg.chart` or `svg.graph` under 12 effective pixels, on thirteen pages at five
widths.**

### A machine's name is not part of a measurement

`rfcs/bench-0104/harness/run.py` recorded `socket.gethostname()` in every
`environment` block **and named the output file after it**. So four committed
records carried a machine's name twice over, and the caption under the index's
bar strip and `/compare`'s tables published it as the provenance of a number.

Closed at all three ends: the field is gone from `environment()`, the file name
is the date alone, the four records are rewritten without it and renamed, and
`envRows()` drops the `Host` row. Everything that makes a number checkable stays
— CPU, cores, memory, OS, clang, rustc, node, python, the compiler's commit,
wasmtime's version and sha256, and every flag. `bench.vyrn`'s test asserts both
halves: no row labelled `Host`, and no `"host"` key in the record it reads.

**The numbers were not re-measured, and that is deliberate.** Re-running the
corpus on another machine would publish different medians under the same date,
which is a worse thing to ship than a stripped identifier. What was regenerated
is the identifier, not the measurement.

### A hairline that was thinner than its neighbours

The picker built its unit out of four borders per label, `border-left: 0` on the
siblings, and two negative margins to pull the checked tile and the command box
back over the borders they doubled. Over subpixel grid columns that is a lottery:
at a 523px container a `1fr` column is 261.5px, so a shifted 1px border straddles
two device pixels and paints as two half-covered ones — thinner and lighter than
its neighbours, differently at 1x and at 2x, and only on the tile a reader is
looking at.

The row draws its own box, the divider is one border on one side of one element,
and the selection is three inset shadows — top and both sides, so the tile stays
open at the bottom into the command box it selects. **A shadow takes no space, so
nothing has to be pulled back over anything.** Measured on both tabs at 1280 and
375: tab row `1px 0`, labels `0` and `1px` left, command box top border 1px, and
the gap between the two boxes exactly 0.

### The masthead search, widened where the row has room

It was an 83px icon-and-word button. It is a 272px field with its placeholder and
a `/` chip above 1024px, and **exactly what it was below that** — the word alone
to 1023px, the glyph alone to 640px. That split is not timidity: the masthead row
fits at 320px with zero pixels to spare (second round, I3), and the phone band's
rules are what bought that. Measured after: the masthead is still 64px.

It is still a BUTTON. A real `<input>` there would be a second search field that
has to hand its keystrokes to the overlay's own — two fields, one query, and a
no-script page with a text box that does nothing. `aria-label` remains the
accessible name, so the placeholder and the chip are never announced twice.

### The hero editor, finished rather than rebuilt

Most of what the brief asked for was already there: `Ctrl+Enter` runs, the status
line says `Press Run` → `Loading the compiler…` → `Running…` → `Ran` (or `Did not
compile`, or `Stopped` after the five-second kill), and the exit code prints under
the output with `stdout` and `stderr` kept apart. What was missing:

- **A way back.** `Reset` restores the program the editor started from — the
  picked example, the program a shared link carried, or the hero's own snippet —
  and the pane's idle line with it.
- **A pane that does not resize.** Idle is one line, `…` is one, a run is three;
  the box grew and shrank on every press. A floor holds the common case.
- **An honest failure.** When the module does not load, the pane says so, the
  textarea goes back to read-only and both controls are disabled — instead of a
  live Run button over an editor with no compiler behind it.

Verified on the page: `Ran` with `admitted at 30 / refused: 5 is under 18 / exit
0`; a program returning 3 shows `The program wrote nothing. exit 3`; `Ctrl+Enter`
runs; `Reset` restores; the pane is 111px in every state.

### Two sections the index earned, both computed

The user asked for more substance and had already set the bound: take good ideas,
not everything. Both new sections are generated from the tree, and neither adds a
snippet anybody has to keep true.

**Five tabs of real code.** Every snippet is a file in `site/guide/` — so it is
compiled while the index renders, RUN by `/guide` while that page renders, and
checked by `vyrn test site/guide/*.vyrn` in CI. The index reads them through
`site/app/guide.vyrn` rather than calling the generator itself, because the
generator's cache is keyed on its argument string and a second call site with a
different `../` prefix would generate a second module whose exports collide
program-wide. The tab strip is the install page's own picker, extended from two
positions to five — one tab control on this site, which is what the second round
established when it deleted the other one.

**Eight standard-library modules**, the two biggest of each of the reference's
four groups with their export counts, from the same generator the reference
landing reads. "Notable" would have been a list somebody maintains; biggest by
export count is a fact, and the same instruction that asked for the grid forbade
hand-listing it.

Two things the brief named are not there, because the tree does not hold them:
`std/stream` has no chapter program, and `protocols` is 31 lines against this
set's 10 to 19, which would have resized the section under the reader on one tab.

### The ceilings, raised on purpose

**The index goes from 260 words to 380 and from 30,000 bytes to 34,000.** It
measures 372 and 32,023. No paragraph on the page grew: what the census counts
here are five tab labels, five one-line captions, eight module names, four group
names and two headings, wrapped around code and generated names. The ceiling that
bounds what a reader actually pays for is the cold load, and that is unchanged in
kind: 40,762 gzipped against 55,000.

### The numbers, fourth round

| Page | Words | Ceiling | Bytes | Ceiling | Cold load, gzipped | Ceiling |
|---|---:|---:|---:|---:|---:|---:|
| index | **372** | 380 | 32,023 | 34,000 | **40,762** | 55,000 |
| install | **210** | 220 | 8,943 | 14,000 | **34,716** | 55,000 |
| releases | **168** | 200 | 6,699 | 20,000 | **34,298** | 55,000 |
| docs (landing) | **642** | 644 | 53,131 | 40,000 | **43,736** | 55,000 |

The published stylesheet is 52,951 raw and 10,325 gzipped against 90,000 and
27,000. Every page gained two words, and they are the search field's placeholder.

### Gates, fourth round

- `vyrn run site/export.vyrn out`: **175 routes, 13 assets**.
- the site's own loop: **196 test blocks declared, 196 ran**.
- `node --test site/test/*.test.mjs`: **31 tests, 31 pass**, including the type
  scale, which now names the two chart steps.
- `vyrn fmt --check`: clean.
- the sweep, at **1280, 767, 700, 375 and 320 over thirteen pages — 65 page-width
  pairs**: no horizontal overflow, no wrapped command line, no inter-element gap
  under 8px, and **no chart text under 12 effective pixels**, which is the
  assertion this round adds.
- one defect the sweep found and did not fix: `/backstage` overflows by 139px at
  1280 and 405px at 375. Measured identical against the stylesheet as committed
  before this round, by serving the old sheet under the same document — it is
  pre-existing, it is the developer front, and it is not one of this RFC's
  thirteen census pages. Census K8 has the numbers.

## M3 — the fifth round: craft against the reference site, by hand

The fourth round left the pages correct and still visibly behind bun.sh. The
gap, read off a side-by-side at 1440px, was not layout — it was finish, in six
places. Census section L carries the entries; the changes:

- **Headings get a display face.** The sheet set every heading in the body
  stack, so an 80px claim rendered as a bolded paragraph. Headings now take
  `--sans-d` — the optical display cuts the visitor already has (Segoe UI
  Variable Display, SF Pro Display) — at weight 720, 0.95 leading, −0.042em
  tracking, and the display step's curve rises to a 5.1rem cap. Zero font bytes
  shipped. The typescale test's records moved with it, and the landing-raise
  invariant (floor-only, same curve) held by raising the base curve, not by
  widening the exception.
- **No scrollbar at rest, anywhere.** Every command scroller and the hero
  editor drew a permanent bar. They scroll the same and draw no bar; a clipped
  command dissolves through a 24px mask fade instead of slicing.
- **A card is one box.** The pillar cards were three stacked rectangles of
  equal border weight (card, command, Copy). The command sits on a wash now,
  Copy is a 30px quiet strip, and the card's own border is the only hard edge.
  One size down (13px) lets all four pillar commands fit their 280px track.
- **A content CTA is a link.** The filled box is the masthead Install's shape;
  in content the same class is an accent arrow-link. Four "primary" actions
  per screen became one.
- **The demo's commands are lines.** `$ vyrn new demo` under its step title,
  not a bordered plate per row inside a bordered card.
- **Bars carry pigment.** 0.18 fill-opacity was a watermark; the focal bar is
  0.9, the field 0.45, and values are 600-weight ink.
- Cut outright: the hero chips row (it restated the headline two inches below
  it) and the editor's Ctrl+Enter chip (now the Run button's title; the
  shortcut itself is unchanged).

## M4 — as landed

The milestone the RFC's own line called "compare matrix, why-Vyrn, guide landing
grid, editors compression". The user widened it before it started, to the thing
those four pages had in common and none of them had: **the documentation had no
shell.** A reference module page, a guide chapter and the editors page each wore
their own partial furniture, and not one of them could say where the page sat in
the whole.

### The docs shell

One layout, three panes, on every documentation page — `/docs/std/*` (37 pages),
`/guide/*` (13), `/docs/editors` and the new `/docs/graph`.

- **Left: the section's tree.** The reference's is `apiGroups()`, the generator
  the reference landing renders; the book's is `chapters()`, the generator
  `/guide` renders. Nothing is hand-listed, so a module added to `std/` or a
  chapter added to the book is in the sidebar of every page of its section on the
  next build. The page being read is marked with `aria-current="page"`.
- **Centre: a one-line breadcrumb, then the content.** `Reference / std / json`,
  `Docs / Chapter 4 of 13`. Every step but the last is a link, because a link to
  the page you are on is a link that does nothing. It replaced three different
  `.eyebrow` lines that each half-named the section and half-said something else.
- **Right: "On this page".** The page's own headings, from the same array the
  `<h2>`s are rendered from. It is a plain list of links in the document — no
  script is needed for it to exist or to work; `widgets.js` marking the entry a
  reader is looking at is the enhancement.
- **Foot: previous and next.** Derived from the tree the sidebar was built from,
  by `pagerFor(groups)` — not from a second table, which could point somewhere
  the sidebar does not. `.chapternav` became `.pager`: one name for one
  component, now that the reference has the same foot the book had.

`site/app/docshell.vyrn` is the data half, 200 lines including its own tests;
`.page.docs` in the stylesheet is the other half.

**The one thing that had to be measured rather than reasoned about.** A pane
spans the whole column, so it takes `grid-row: 1 / span 60` — and under
`display: contents`, which is what every other page on this site is, those sixty
rows are the SHEET's rows, the same grid the masthead and the footer are
auto-placed into. Both of those span all twelve columns, so neither could fit
beside the rails and both were pushed past them: on the first build the masthead
painted at **y=928, under a sidebar**, with the page's own content starting at
y=992. `grid-template-columns: subgrid` is the exact tool — the shell takes the
sheet's twelve column tracks, so the rail still lines up with the rule grid and
the section seams, and keeps its own rows, which is the only thing it needed to
stop sharing.

**On a phone the panes change shape rather than shrink.** The tree becomes the
masthead's own disclosure — a `<summary>` over the full width, shut until
pressed — and the right pane is not shown at all: a sidebar of the page's own
headings helps a reader scan a two-metre column with a mouse; on one column it is
a second copy of the page above the page. The `<details>` carries no `open` in
the markup, exactly as `.menu` does not, because `open` is an attribute a
stylesheet cannot remove: the element is shut by default and the desktop block
forces it open. Everything works with no JavaScript at either width.

### The reference landing

**Four area cards above the module grid**, each an icon through the existing
`<Icon>` provider, a title, one generated line and an arrow-link: the book
(`chapters().length`), the library (`apiModules().length` and its export total),
the tools, and the graph (`edgeCount()`, `leafCount()`). The page had opened on
its own module list, which answers "which module does X" and nothing else — a
reader arriving from a search result could not see that this site also holds a
book, a driver and a graph.

**The import graph moved to `/docs/graph`.** M3 deferred this as row H4 with the
numbers: the landing was 52,815 bytes against M0's 40,000 and the inline `<svg>`
was 25,347 of them, downloaded by every reader of the reference whether or not
they scrolled to it. It is **29,231 now** — under the ceiling, honestly, with the
drawing on a page that gives it the whole column and a URL a reader can send to
somebody. Nothing about the drawing changed.

### The old pages, under the register

| Page | What it was | What was done |
|---|---|---|
| `/compare` | census K3/H3: "14 blocks wider than their container at 1280" | **Already closed by M3's `.scroller` work** — re-measured at 1280 and 375, zero elements outside their scroll container, document width equals the viewport. The three `.notice` blocks of method above the engine chart became one `<details>`, which is M0's own rule for a method note |
| `/guide` landing | 14 `.note` elements, `.modlist` rows | The chapter list is `.modgrid` — the component `/docs` already uses — so thirteen `class="note"` ledes became thirteen `.d` rows. **14 disclosures to 1**, and the one left is the shell's `<noscript>`, which every page carries |
| `/explore` | 5 `.note`, 496 words | `.pkgs .note` → `.pkgs .d`, the same correction. **5 to 1**, 496 words to 407 |
| `/why-vyrn` | 6 `.cap`, 4 `.note`/`.notice` | Four of the six `.cap` were the capability panes' own claim, carrying two inline styles that undid `.cap`'s border and padding — the tell that they were never captions. **6 to 2, 4 to 1** |
| `/backstage` | census K8: the document 139px wide at 1280, 405px at 375 | `.legend.statuses li` was `display: flex`, so each row was ONE item of a four-column grid holding four flex children and the template described a grid nothing was in. `display: contents` — **1,399px to 1,270px, zero overflow at both widths.** Nothing else on the backstage changed |

### The census list, and the two rows that named a stub

`scripts/site-census.py` still measured `/philosophy` and `/editors`, which M1
turned into redirect stubs — so two of its thirteen rows had been reporting **66
words of "this page moved"** as the page itself. `/why-vyrn` is 540 words and
`/docs/editors` is 786; neither had ever been counted. The list names the pages
now, and `/docs/graph` joins them: fourteen rows.

### The numbers

| Page | Words | Ceiling | Bytes | Ceiling | `.cap` | Ceiling | `.note`/`.notice` |
|---|---:|---:|---:|---:|---:|---:|---:|
| index | 358 | 380 | 31,965 | 34,000 | 8 | 8 | 1 |
| install | 210 | 220 | 9,002 | 14,000 | 1 | 1 | 1 |
| why-vyrn | **540** | 280 ✗ | 10,343 | 9,000 ✗ | **2** | 2 | **1** |
| compare | **1,198** | 420 ✗ | 65,324 | 55,000 ✗ | 11 | 3 ✗ | **9** |
| releases | 168 | 200 | 6,699 | 20,000 | 1 | 1 | 1 |
| guide (landing) | **571** | 580 | 9,421 | 10,000 | 1 | 1 | **1** |
| guide/ownership | 362 | 380 | 14,512 | 15,000 | 3 | 3 | 1 |
| docs (landing) | 685 | 700 | **29,231** | 40,000 | 1 | 1 | 1 |
| docs/graph | 217 | 260 | 35,502 | 40,000 | 2 | 2 | 1 |
| docs/std/json | **880** | 900 | **22,588** | 24,000 | 10 | 10 | 2 |
| explore (landing) | **407** | 236 ✗ | 7,830 | 8,000 | 2 | 2 | **1** |
| explore/shelf | 501 | 213 ✗ | 10,764 | 8,000 ✗ | 6 | 1 ✗ | 2 |
| docs/editors | **786** | 800 | **24,414** | 26,000 | 3 | 3 | 7 |
| play | 359 | 120 ✗ | 10,513 | 9,000 ✗ | 3 | 1 ✗ | 2 |

**Six ceilings are raised here, and each is the shell's own cost.** The three
panes add roughly sixty words and five kilobytes of navigation furniture to every
page that wears them — 41 tree rows on a reference page, 14 on a chapter, plus a
breadcrumb, a pager and two rail headings. That is generated navigation, not
prose: `docs/std/json`'s own prose did not move, and its gzipped weight went from
5.3 KB to 6.0 KB. The raised rows are `guide (landing)` 523→580 and 9,000→10,000,
`guide/ownership` 13,000→15,000, `docs (landing)` 644→700, `docs/std/json`
512→900 and 16,000→24,000, `docs/editors` 200→800 and 10,000→26,000, and
`/docs/graph` is new at 260 words and 40,000 bytes — the same byte ceiling the
reference landing has, because it now carries what that landing carried.

**The `.cap` ceilings on the two reference pages are raised to what they are, and
that is honest rather than met.** `docs/std/json` carries one `.cap` per export —
"std/json.vyrn, line 41" — which is the provenance line THE RULE exists to
require, one per entry rather than one per plate. A page of ten generated entries
has ten of them by construction.

**And every `.cap` ceiling in M0's table has a floor of 1 that M0 could not have
known about.** M1's search overlay ships a `.cap` — "Type to search. Esc closes."
— in the shell of all 174 documents. `/why-vyrn`'s ceiling was **0** and the page
carries two: the overlay's, and the capability plate's own provenance line. Both
that row and `/explore`'s are raised by exactly one, and the reason is recorded
at the row.

**Four rows are still over, marked ✗ above, and M4 did not re-ceiling them.**
`/compare` at 1,198 words against 420, `/explore/shelf` at 501 against 213,
`/play` at 359 against 120, and `/why-vyrn` at 540 against 280. M4 cut where its
scope reached — compare lost 150 words to a disclosure, explore 89, why-vyrn 15 —
and what is left on each is a prose diet on a page this milestone's scope named
only for its register defects. Recorded rather than quietly re-ceilinged: **M5
owns the budgets, and these four are what M5 will fail on.**

### Found by the verification, and fixed

Six defects the verification found that nobody had filed, all in census section R:
a `pre.code` on `/explore/shelf` and a `pre.doccode` on `/docs/std/json` still
drew a scrollbar at rest, five rounds after L2 said none should; the area cards'
`minmax(360px, 1fr)` floor is a HARD floor, so it took `/docs` 12px wider than a
375px screen; the editors table broke `analyze_document` across two lines
mid-word once the content column narrowed; and the icons gate keyed its
template-to-page map by file NAME, which stopped being unique the moment a second
`index.vyx` carried a glyph. The sixth was found by LOOKING at a 2x crop, which
is the only thing that could have found it: the right pane carried a
`border-left`, and the two panes are different heights, so the sheet drew a
full-height hairline on the left and a stub that stopped halfway on the right.

### Gate

- Every documentation page wears one shell, and every row in it is generated.
- `/docs` is 29,231 bytes against M0's 40,000, with the graph on its own page.
- Zero horizontal overflow and zero visible scrollbar at rest, on all fifteen
  pages at 1280 and at 375, measured in a browser against the exported tree.
- `/backstage`'s K8 overflow is closed: 1,399px to 1,270px.
- The census measures the thirteen real pages plus `/docs/graph`.

## M5 — as landed: the documentation splits into areas

The user put two screenshots side by side — our two-row subnav over a single
thirteen-chapter book, and the reference site's row of areas, each with a
sidebar of its own — and asked for the same shape: "different categories at
top and related stuff on each."

### One table, four areas

A `Chapter` now names its **area** — `"guide"`, `"web"` or `"tooling"` — and
everything else is derived from that one field. `chapterHref` puts the page
under its area's directory; `chapterNumber`, `prevSlug` and `nextSlug` count
and chain within a shelf, so a pager never walks a reader out of the area they
are reading; `areaChapters(area)` is the sidebar, the landing grid, the export's
route list and the search index, because all four read the same run of the same
table. The subnav is rendered from `areas()` in `docshell.vyrn` — key, landing,
label, glyph, tree-group name — and `areaTree(area, current)` builds any
shelf's sidebar from the same rows, so an area cannot be in the subnav and
missing from a tree.

- **Guide** keeps the book: eleven chapters, first program to CLI apps.
- **Web** (`/web/`) is new: *Views and HTML* (the old chapter 12's tree
  section, with the differ and soft navigation given their own words),
  *Components: .vyx*, *Routes, RPC and the dev server*, *Styling and icons*.
- **Tooling** (`/tooling/`) is new: *The CLI* (run/check/build, fmt/doc, and
  `vyrn why` — every command checked against `vyrn --help` before it was
  written), *Testing and bench* (chapter 10, moved whole), *Projects and
  dependencies* (manifest, lock, toolchain pinning), and Editors at the end of
  the shelf. The first cut left Editors at `/docs/editors` with only its group
  changed; the user asked why the path disagreed with the shelf, and it was
  the right question — the page lives at `/tooling/editors` now, the old path
  is a stub, and M1's `/editors` stub points at the final home rather than at
  another stub, which is the chain the stub test's no-loop assertion refuses.
- **Reference** is unchanged.

### The moves

`/guide/testing` and `/guide/web` retired. Each publishes a stub at its old
path through the SAME `redirects()` machinery M1 built — refresh, canonical,
visible link — off one `movedChapters()` table that the stub routes also render
their body from, so a stub and its refresh cannot point different ways. Two
rules the gates forced: a stub marks nothing in the masthead (`currentNav`
answers "" for a redirect path now, where before the rule lived only in the
test), and a stub wears no subnav band — the icon census caught it carrying
four glyphs it had no template for.

### What the gates caught

- The reference pages' "used by a program in the guide" link built
  `"/guide/" + chapter` by hand and 404'd for a block whose chapter moved
  shelves; the fragment gate named it (`/docs/std/html.html` →
  `/guide/views.html#html`). It reads `chapterHref` now, as does the markdown
  twin path list, which had the same hand-built prefix.
- The masthead-marks gate refused the stubs before `currentNav` knew the rule.
- 185 routes against M4's 176: two landings and seven area pages.
- The area pager walks the TREE, not the chapter chain, so a shelf's
  hand-listed rows are neighbours too: `/tooling/projects` offers Editors as
  Next (user caught the asymmetry).
- The masthead marker now follows a soft navigation: `vyrn-nav.js` mirrors
  `aria-current` from the fetched document's header, keyed by `data-key` — it
  had been frozen at whatever page the session started on (user).
- Merging main surfaced two audit assertions that had never run anywhere:
  the escaped-pipe cell test expected the table splitter to collapse `\`
  (the inline pass owns backslash escapes — collapsing twice would halve a
  run of backslashes twice), and the search-haystack test's witness was a
  `<script>` the vyx summary has never contained. CI's per-module loop dies
  at the first red module, alphabetically before both.

## M5 — the third round: named groups, a registry front, an editor

Five user directions in one message, all landed:

- **The shelves read as named groups.** A `group` field on the chapter table;
  the sidebar, the landing grids and a contiguity test read it. The guide is
  *First steps / The memory model / Abstractions / Programs*; Web is *The page
  / The server* (styling moved ahead of server so the runs stay contiguous);
  Tooling is *Commands / Workspace*. The two-digit numbers are gone — a
  category names what a run of pages is about, which a prefix never did.
- **Explore is a registry** (user: "like npm or crates"): a search field over
  the index — the reference landing's own scriptless-degrading `data-q`
  mechanism, with a `data-search-noun` so the count says "packages" — one row
  per package with its `vyrn add` line and a Copy button, the mechanism in
  side cards, the numbered rail deleted. The one-module-list gate now states
  the real invariant: each front searches its own list, and neither links into
  the other's.
- **Play is an editor** (user: "like vscode"): toolbar with a `main.vyrn` tab
  and the run controls, the editor filling the window, a docked panel for
  problems, stdin and output, a status bar naming the engine. The prose lives
  behind "About this playground". Every `data-play-*` element survived
  unchanged, and the redesign was verified by pressing Run in the page.
  `main.page` is `display: contents` — the shell grid owns its children — so
  the IDE's column is one `.idewrap` box inside the grid.
- The typescale gate caught the round's one literal: the registry row's name
  size is `--t-h5` now, not `1.05rem`.

## What this RFC does not do

- No JavaScript framework, no CSS framework, no analytics, no dependency.
- No fabricated social proof of any kind.
- No i18n; out of scope, stated.
- No backstage changes beyond what the shell forces.
