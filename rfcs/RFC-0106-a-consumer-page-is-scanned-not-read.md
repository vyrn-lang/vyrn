# RFC-0106 — A Consumer Page Is Scanned, Not Read

- **Status:** **Proposed.** No implementation. Milestones below; a milestone
  that fails its gate says so in this file.
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
adjective.

**M1 — the shell.** Navigation and CTA, display scale and density tokens, the
search overlay and its build-time index, OG meta, redirect stubs, copy-page,
RSS. Content untouched; every page ships at every commit. Gate: the search
answers on every page without script falling back sanely; every old URL
resolves; the a11y checklist rows for the new controls pass.

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

## What this RFC does not do

- No JavaScript framework, no CSS framework, no analytics, no dependency.
- No fabricated social proof of any kind.
- No i18n; out of scope, stated.
- No backstage changes beyond what the shell forces.
