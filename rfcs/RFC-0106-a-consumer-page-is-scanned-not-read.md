# RFC-0106 — A Consumer Page Is Scanned, Not Read

- **Status:** **M1 shipped: the shell.** See
  [M0 — as landed](#m0--as-landed) for the census, the ceilings every later
  milestone is held to, and four things the measurements contradict in the
  design below, and [M1 — as landed](#m1--as-landed) for the eight items, the
  three defects M1 found in its own earlier commits, and the two ceilings it
  cannot meet. Milestones below; a milestone that fails its gate says so in
  this file.
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
before merge**.

**M3 — reference landing and releases.** Gate: word counts inside ceilings;
stat tiles computable, no hand-written number; RSS validates.

**M4 — compare matrix, why-Vyrn, guide landing grid, editors compression.**
Gate: every matrix cell links to proof; word counts inside ceilings.

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
| index | 644 (644) | **260** | **260** | 16,024 | **30,000** | 5 | **2** | 1 | **5** |
| install | 678 (678) | **220** | **220** | 12,942 | **14,000** | 2 | **1** | 14 | **8** |
| why-vyrn (was philosophy) | 550 (550) | **280** | **280** | 8,285 | **9,000** | 5 | **0** | 0 | **0** |
| compare | 1329 (1329) | **420** | **420** | 63,602 | **55,000** | 10 | **3** | 0 | **0** |
| releases | 235 (235) | **200** | **200** | 3,754 | **20,000** | 0 | **1** | 0 | **1** |
| guide (landing) | 643 (300) | **180** | **523** | 7,393 | **9,000** | 0 | **0** | 0 | **0** |
| guide/ownership | 330 (330) | **380** | **380** | 11,592 | **13,000** | 2 | **1** | 0 | **0** |
| docs (landing) | 872 (428) | **200** | **644** | 50,174 | **40,000** | 3 | **1** | 0 | **0** |
| docs/std/json | 786 (424) | **150** | **512** | 14,722 | **16,000** | 9 | **1** | 1 | **1** |
| explore (landing) | 477 (401) | **160** | **236** | 6,158 | **8,000** | 1 | **1** | 0 | **0** |
| explore/shelf | 482 (469) | **200** | **213** | 8,167 | **8,000** | 5 | **1** | 2 | **2** |
| editors | 704 (704) | **200** | **200** | 17,356 | **10,000** | 2 | **1** | 3 | **3** |
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

## What this RFC does not do

- No JavaScript framework, no CSS framework, no analytics, no dependency.
- No fabricated social proof of any kind.
- No i18n; out of scope, stated.
- No backstage changes beyond what the shell forces.
