# RFC-0105 — A Site Has Two Audiences

- **Status:** **Proposed.** No implementation. Milestones below; a milestone
  that fails its gate says so in this file.
- **Depends on:** the shipped site (`site/` — routes, `export.vyrn`, the
  document-relative URL rule), RFC-0026/0069 (std/ui and pages), RFC-0104 M3
  (the chart that will live on `/compare` and the backstage).
- **Evidence (user):** "there should be no lang related development stuff on
  consumer website part", "what about \"crates\" website explorer? At this
  moment docs and explore pages are same, chart and search should be moved to
  docs page", "there also must be info about support in code editors", "what
  about accessibility stuff on website, theme configuration".

---

## The problem, in four sentences

The site serves two readers with one navigation: a person deciding whether to
use Vyrn, and a person building Vyrn. `/docs` and `/explore` are two views
over the same standard library — a reference and a searchable index with an
import graph — so the explorer answers no question the reference could not.
Nothing on the site says the language has an editor story, though the VS Code
extension ships hover, go-to-definition, completion, formatting, CodeLenses
and semantic tokens. And the site has one theme and no accessibility
discipline written down.

## The design

**Two fronts, one build.** The consumer site keeps its voice: what the
language is, how to install it, the guide, the reference, the playground, the
releases. A **backstage** section — its own layout and navigation, visually
distinct, linked from the consumer footer — carries what a contributor needs:
the RFC index and rendered RFCs, the benchmark methodology and full datasets
(RFC-0104), and the development-facing detail that today leaks into consumer
pages. One export builds both; the boundary is which navigation a page hangs
off, not a second site.

**The explorer becomes a package explorer.** `/docs` absorbs what `/explore`
does better than it: the no-script search (the `data-q` row trick) and the
import graph move onto the reference. `/explore` is rebuilt in the shape of a
package registry — a card per package, a page per package with its summary,
exports, dependencies, docs link, and the import line a user copies — covering
`std/` modules and the repository's example projects today, with the page
schema ready for remote (`github:`) packages when a community exists. No
module list is rendered twice.

**Editors get a page.** `/editors`: the VS Code extension, what it does (the
LSP feature table, matched against `editor/vscode` reality, with per-feature
illustration), how to install it, and the honest state of anything else.

**Accessibility and theme become rules, not vibes.** A theme control with
three states — system, light, dark — persisted, defaulting to system via
`prefers-color-scheme`; a contrast pass to WCAG AA on both palettes; keyboard
reachability and labels for every interactive widget (the import graph, the
playground, RFC-0104's radar); reduced-motion respected; skip-to-content. The
checklist lives in this RFC and each item is checked in a browser, not by
reading the CSS.

## Milestones

**M1 — the backstage.** Census first: every development-facing thing on a
consumer page, listed here. Then the backstage section with its own layout and
navigation (RFCs rendered from `rfcs/*.md`, the index in reading order), the
consumer pages cleaned of what the census found, and a footer link each way.
Gate: the census table shows every row relocated or justified; no consumer
navigation entry leads to development content.

**M2 — the explorer split.** Search and the import graph move to `/docs`;
`/explore` becomes the package explorer as specified. Gate: no module list is
rendered on two pages; the search still works without script; every package
page's import line compiles when pasted.

**M3 — the editors page.** `/editors` in the consumer navigation. Gate: every
feature row on the page names the code in `editor/` that implements it; no
claimed feature without an implementation behind it.

**M4 — accessibility and theme.** The control, the palettes, the checklist —
each item verified in a browser and recorded here. Gate: the checklist has no
unchecked row, and the verification method for each row is named (keyboard
walk, contrast measurement, forced-colors, reduced-motion emulation).

## What this RFC does not do

- It does not add a second deployment, a second domain, or a build flag. One
  export, two navigations, the document-relative URL rule untouched.
- It does not add a JavaScript framework, a CSS framework, or any dependency.
- It does not invent a package registry. The explorer renders what exists —
  `std/` and the examples — in a shape that will fit a registry when one does.
