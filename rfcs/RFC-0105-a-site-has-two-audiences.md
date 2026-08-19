# RFC-0105 — A Site Has Two Audiences

- **Status:** **M1 implemented.** M2–M4 proposed. Milestones below; a milestone
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

**M1 — the backstage.** *Implemented.* Census first: every development-facing
thing on a consumer page, listed here. Then the backstage section with its own
layout and navigation (RFCs rendered from `rfcs/*.md`, the index in reading
order), the consumer pages cleaned of what the census found, and a footer link
each way. Gate: the census table shows every row relocated or justified; no
consumer navigation entry leads to development content.

### M1 — the census, and what happened to each row

Every development-facing thing that was on a consumer page. "Backstage" means it
is on `/backstage` now; "gone" means it left and the row says why; "stays" means
it is consumer-facing after all and the row says why.

| # | What | Where it was | Where it went |
|---|------|--------------|---------------|
| 1 | `cargo test -p vyrn-cli --test parity -- --ignored` | `/` §04, under the parity widget | **Backstage** — "what holds the record honest", beside the index test |
| 2 | The "Design records — *N* — every decision, with its argument" tile | `/` hero specs | **Backstage** — the record is a section now, not a number on a landing page |
| 3 | "…the examples and **the design record**" | `/` §05, the Docs card | **Gone** — the card names what `/docs` holds, and `/docs` no longer holds the record |
| 4 | `RFC-0089` as the ownership plate's label | `/philosophy` §02 | **Gone** — the plate shows the four words; the record that argued them is one footer link away |
| 5 | The design-record plate: the count, the `rfcs/` label, the link to the directory | `/philosophy` §04 | **Backstage** — the section keeps the predictability claim and points at the widget that demonstrates it |
| 6 | The RFC strip: 104 cells, each opening a file on GitHub | `/releases` §02 | **Backstage** — same strip, and a cell now opens the record rendered on this site |
| 7 | "The eight most recent" design records | `/releases` §03 | **Backstage** — the whole index, in reading order, with each record's status |
| 8 | "The whole index, with the status each record carries, is in the repository" | `/releases` §03 | **Backstage** — the index is here, so the sentence is not needed |
| 9 | "The design record — start with RFC-0001, RFC-0003 and RFC-0004" | `/docs` §03 | **Backstage** |
| 10 | "Compiler notes — the crate map, the build notes, and how to build the excluded crates" | `/docs` §03 | **Gone** — contributor reading; the footer's source link reaches the repository |
| 11 | `rfcCount()` in "…and *N* design records live in the repository" | `/docs` §03 | **Gone** — the count is on the backstage, beside the records it counts |
| 12 | "The design record" (footer link to the `rfcs/` directory) | Every page, the shared footer | **Became the door**: "Backstage — the design record", the one consumer link into the section |
| 13 | `rfcs/census-call-arguments.md` as the leak plate's source | `/compare` §06, label, caption and link | **Stays** — a measured number must name where it was measured, and that file is a measurement record, not an RFC |
| 14 | `RFC-0077` as the module-size bars' link | `/compare` §05 and `/` §03 | **Stays** — same reason as 13; it leaves for the repository rather than opening the backstage, so no consumer widget leads into development content |
| 15 | "CI fails unless they do", "CI enforces that on every push" | `/`, `/releases`, `/docs/std/*` | **Stays** — what is verified is why a consumer should believe the claim beside it. The command you would type to verify it yourself is what moved (row 1) |
| 16 | `cargo build --release --manifest-path vyrn-lsp/Cargo.toml`, `editor/vscode/` | `/install` §06 | **Stays until M3** — building the extension is today the only way a reader gets it. M3 gives editors a page and this row is rewritten there |
| 17 | "The full parity harness also needs a `wasmtime`, through `$VYRN_WASMTIME`" | `/install` §03 | **Stays** — it is one clause in the from-source section, and it is what that path needs |
| 18 | 630 `RFC-NNNN` mentions in reference prose and module summaries | `/docs`, `/docs/std/*`, `/explore` | **Stays** — every one is read off `std/` by a generator. Editing them would make the reference disagree with the source it is generated from, which is the defect the reference exists to prevent |
| 19 | `git clone` + `cargo build -p vyrn-cli` | `/install` §03 | **Stays** — an install path, and the only one on a platform with no published archive |

Gate: met. Rows 1–12 relocated or removed with the reason; rows 13–19 justified.
No navigation row reaches `/backstage` — checked by a test in `site/export.vyrn`,
not by reading the markup — and the backstage carries its own masthead, its own
navigation and its own accent, reached only from the consumer footer and leaving
only through its own.

### M1 — the markdown the record is written in

The backstage renders `rfcs/*.md` with `site/app/markdown.vyrn`. The subset was
measured over all 104 records (2,525,911 bytes, 45,821 lines) rather than
guessed, and the renderer implements exactly what the measurement found. A
construct outside it is an `Err`, which fails the export, which fails the build.

| Construct | Count | Rendered as |
|---|---:|---|
| Paragraph lines | 19,362 | `<p>`, lines joined |
| Continuation lines (1–3 spaces) | 8,418 | part of the block above |
| Bullet items (`-` 2,290 / `*` 9 / `+` 7, nested to two levels) | 2,306 | `<ul>`, items rendered as markdown of their own |
| Fenced code (13 language tags, 77 untagged) | 354 | `<pre class="code" data-lang="…">`, escaped |
| Table rows / delimiter rows (alignment colons included) | 1,396 / 266 | `<table>` in a `.scroller`, `text-align` from the colons |
| Blockquote lines | 1,087 | `<blockquote>`, contents rendered as markdown |
| Headings `#`–`#####` | 1,831 | `<h1>`–`<h5>` with GitHub's own anchor slug |
| Ordered items (`1.` 437, `1)` 1) | 438 | `<ol>` |
| Thematic breaks | 331 | `<hr/>` |
| Indented code blocks | 4 | `<pre class="code">` |
| Inline code spans (single and double backtick, some spanning lines) | 26,865 | `<code>`, escaped |
| `**strong**` | 5,072 | `<strong>` |
| `*emphasis*` | 860 | `<em>` |
| Backslash escapes | 127 | the escaped byte, as text |
| Links `[text](target)` | 23 | `<a>`: `#anchor` stays, another RFC becomes its page here, anything else becomes its file in the repository |
| `~~struck~~` | 11 | `<del>` |
| Bare `RFC-NNNN` mentions | thousands | a link to that record's page, when the index carries it |

Three decisions the measurement made, each of which would have been wrong the
other way:

- **Underscores are never emphasis.** All 28 `_x_` and `__x__` in the corpus are
  identifiers — `__loading__`, `__json$Json__`, `_pN_`. Emphasis would corrupt
  every one of them.
- **A fence marker with a backtick after it is not a fence.** RFC-0058 writes
  ``` ``` `at(xs, i)` was removed ``` ``` inline, four times. CommonMark's rule
  (no backtick in an info string) is why, and without it the rest of that section
  is swallowed as code.
- **Cells with no delimiter row are not a table.** RFC-0077 has three such lines.
  GitHub prints the pipes; so does this.

What is refused: a fenced block tagged with a language not in the list, a fence
that is never closed, and a block of raw HTML. The corpus has none of the three
today, which is the point — the day one appears, the build says so.

Two numbers, since the export now renders 2.5 MB of markdown: the export takes
**1m20s** (from about 10s) and publishes **5.2 MB** into `out/`. Two things keep
that from being worse. The record pages are exported from their own list rather
than from `sitePaths()`, because the per-page gates in `site/export.vyrn` walk
that list six to ten times and walking these hundred pages with them took
`vyrn test site/export.vyrn` from under a minute to ten; what those gates check
is checked on the exported tree for every page by `site/test/basepath.test.mjs`,
plus one representative record in Vyrn. And a backstage page publishes no
`.data.json` payload: the section loads no script, so nothing would ever fetch
one, and it would have been a second copy of every rendered record.

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
