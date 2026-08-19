# RFC-0105 — A Site Has Two Audiences

- **Status:** **Implemented.** Four milestones and four pull requests. The
  backstage carries the design record and no consumer navigation row reaches it;
  `/docs` is the one reference and `/explore` is the package explorer; `/editors`
  names the file behind every feature it claims; and the theme control, the two
  measured palettes and the accessibility checklist are in
  [M4 — as landed](#m4--as-landed). Milestones below; a milestone that fails its
  gate says so in this file.
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

**M2 — the explorer split.** *Implemented.* Search and the import graph move to
`/docs`; `/explore` becomes the package explorer as specified. Gate: no module
list is rendered on two pages; the search still works without script; every
package page's import line compiles when pasted.

### M2 — as landed

**What moved.** The search field, the module rows that ARE its index, and the
import graph are on `/docs`, in the reference's own voice and inside its own
sections. The reference's list did not gain a copy of the explorer's: the list
that was already there grew the two attributes the filter reads (`data-q`, the
lowercased haystack; `data-e`, the export names) and the finder above it.
`site/app/explore.vyrn` is `site/app/stdgraph.vyrn` now — same contents, and it
lost the name of the page it no longer serves — and `widgets.js` reads
`data-search-*` where it read `data-explore-*`, because a hook named after the
page it is not on is a hook somebody follows to the wrong file.

**The package model.** A package is a unit somebody could depend on. Two kinds
exist in this repository, and each answers the same four card questions from its
own source (`site/app/packages.vyrn`):

| | `std` — a module | `project` — a directory in `examples/` with a `vyrn.json` |
|---|---|---|
| Name | `std/json` | `examples/shelf` |
| Summary | the reference's own, read off the module's `///` header | the first SENTENCE of its first entry module's `///` header |
| What it gives | its exports, with signatures | the artifacts the manifest declares: name, entry, target |
| What it needs | the `std/` modules its `import` lines name | the `std/` modules any file under it imports |
| What you copy | `import { … } from "std/json"`, every export named | the manifest itself |
| Where it links | its reference page, and its source | its directory in the repository |

The artifact rule is the compiler's, not a second reading of it:
`compiler/vyrn-frontend/src/artifacts.rs` says `main` and `server` name native
artifacts and `client` a browser one, and the `artifacts` map spells the same
thing out — so `examples/shelf`, which writes both spellings of the same two
entry points, declares two artifacts and not four.

**What a remote package would add, and where.** `github:owner/repo@tag` fits the
`Package` record with two more fields and no new shape: the version an import
pins and the sha256 `vyrn.lock` records (RFC-0010 M4). Neither is declared and no
page renders a slot waiting for one — a cell nothing fills is a dead cell on
every card. The comment on `packages.vyrn` says exactly this, and `/explore`'s
third section says it to a reader in prose rather than as an empty row.

**Where the import-line gate lives.** `site/test/importline.test.mjs`, run by the
same `node --test "site/test/*.test.mjs"` line the base-path suite already uses.
It takes the `data-import` attribute off every package page in the EXPORTED
tree, writes each line into a program of its own, and runs `vyrn check` on it —
eight at a time, 0.3 s for all thirty-seven. Two reasons it is not in
`site/export.vyrn`: that program cannot start another one, and M1's timing lesson
says a walk of thirty-seven compiler runs does not belong beside gates that walk
`sitePaths()` six to ten times. What the export DOES check is the contract the
node test depends on: every module page carries the line as an attribute as well
as in the box, both from one `importLine`, and no project page carries an empty
one.

Gate: met, in three parts.

- **No module list on two pages.** Proven by construction and by a test rather
  than by promise. `/docs` renders `apiModules()`; `/explore` renders
  `packages()`; neither page renders the other's. The test that holds it
  (`export.vyrn`, "one module list, and the pages that do not render it") checks
  where each row LEADS: every module row on the reference opens that module's
  reference page and no row on the explorer index does, and every card on the
  explorer opens that package's page and no row on the reference does. It also
  checks that the search input and the row list exist once, on `/docs`.
- **The search works without script.** The rows carry their own haystack, so
  there is nothing to fetch and nothing to run: with no script the reader gets
  every module, its first line, its export count and a link. Verified by reading
  the exported html — 37 `data-q` and 37 `data-e` attributes on `docs.html`, one
  per module, asserted by the same test.
- **Every import line compiles.** 37 lines, 37 programs, `vyrn check` on each,
  green — and shown red once on purpose, by putting a name `std/json` does not
  export into the emitted attribute: `` `std/json.vyrn` does not define
  `notAnExport` ``. A gate nobody has seen fail is a gate nobody has tested.

Three things this milestone found or conceded, in the place they happened:

- **A std module and a package share a name, and that is the fact rather than a
  duplication.** The gate sentence says "no module list is rendered on two
  pages", and the 34 module NAMES do appear on both — because a module is a
  package. What is not rendered twice is the list: not its source, not its
  shape, not its destination, and not the search over it. The test above is
  written as the strongest form of that claim that is true.
- **A project has no summary anywhere in the repository.** `std/` modules carry
  a `///` header the reference already renders; `examples/shelf` carries one on
  its server root, describing the module rather than the application. The first
  sentence of it is what the card shows, because the alternative was four
  sentences written here, which would be four sentences to keep true. It reads
  as what it is: a sentence about the entry point.
- **`vyrn fmt` cannot format a `.vyx` template.** It says so and skips, which it
  did before this milestone. The four templates M2 touched are formatted by hand
  to the house shape; the six `.vyrn` files are `fmt --check` clean.

Two numbers, since M1 left the export at a minute and a quarter: it now takes
**74 s** and publishes **5.6 MB**, against **68 s** and **5.2 MB** for the same
tree on the same machine without this milestone. The 41 package pages are 457 KB
of html and six seconds, and they are in `sitePaths()` rather than in a list of
their own — a package page is a few kilobytes rendered from a lookup, not 25 KB
of markdown, so the reason M1 moved the record pages out does not apply to
these. One run of each, on one machine, which is enough to say what changed and
what did not.

**M3 — the editors page.** *Implemented.* `/editors` in the consumer navigation.
Gate: every feature row on the page names the code in `editor/` that implements
it; no claimed feature without an implementation behind it.

### M3 — as landed

**The table.** Eighteen rows, in `site/app/editors.vyrn`, each carrying the file
that implements it and the declaration inside that file. The order is the order a
reader meets them: what answers while you type, then what you ask for, then what
the extension draws itself.

| Feature | Implemented by |
|---|---|
| Errors while you type | `compiler/vyrn-lsp/src/main.rs` — `analyze_doc` / `publish` |
| Across the files you import | `compiler/vyrn-lsp/src/main.rs` — `load_context` |
| Hover | `compiler/vyrn-lsp/src/main.rs` — `handle_hover` |
| Go to definition | `compiler/vyrn-lsp/src/main.rs` — `handle_definition` |
| Completion | `compiler/vyrn-lsp/src/main.rs` — `handle_completion` |
| Outline and breadcrumbs | `compiler/vyrn-lsp/src/main.rs` — `handle_document_symbol` |
| Highlight the binding, not the word | `compiler/vyrn-lsp/src/main.rs` — `handle_document_highlight` |
| Rename, including what a generator derived | `compiler/vyrn-lsp/src/rename.rs` — `rename_at` |
| Quick fix on a contract | `compiler/vyrn-lsp/src/main.rs` — `handle_code_action` |
| Format, and format on save | `compiler/vyrn-lsp/src/main.rs` — `handle_formatting` |
| Inlay hints | `compiler/vyrn-lsp/src/main.rs` — `handle_inlay_hint` |
| Semantic colour | `compiler/vyrn-lsp/src/main.rs` — `handle_semantic_tokens_full` |
| Templates are the same language | `compiler/vyrn-lsp/src/templates.rs` — `classify` |
| Syntax highlighting | `editor/vscode/vyrn.tmLanguage.json` |
| Snippets | `editor/vscode/snippets/vyrn.json` |
| Run it from the file | `editor/vscode/extension.js` — `provideCodeLenses` |
| ▶ Run dev server, where it means something | `compiler/vyrn-lsp/src/main.rs` — `handle_is_dev_entry` |
| The wire path above the procedure | `compiler/vyrn-lsp/src/main.rs` — `handle_route_lenses` |

**What moved.** M1's census row 16 said the extension's build commands stayed on
`/install` §06 *until M3*. They are here now. `/install` §06 is three sentences
that stop at the compiler and point at this page; `lspBuild()` is deleted from
`install.vyx`, because the command it held was the wrong one (below). The
section keeps the `vyrn.json` / `vyrn.lock` notice, which is about resolving
modules and not about an editor.

**Where the gate lives.** `site/export.vyrn`, "every feature the editors page
claims names a file that is in this repository". It renders `/editors`, reads
`data-impl` off the RENDERED page with the same attribute scanner the fragment
and import-line gates use, and opens every path with `readFile` relative to the
repository root. It reads the page rather than the table it was rendered from,
so a row that is in the data and not in the markup fails it too, and it asserts
the count against `features()` so a scan that found nothing cannot pass in
silence. Shown failing once, on purpose, by pointing one row at
`compiler/vyrn-lsp/src/refactor.rs`:

```
the editors page claims `compiler/vyrn-lsp/src/refactor.rs`, which this repository does not have: cannot read `compiler/vyrn-lsp/src/refactor.rs`
test "every feature the editors page claims names a file that is in this repository" ... FAILED
```

Gate: met. Eighteen rows, eighteen `data-impl` paths, eighteen files that exist,
checked on every export.

**Four contradictions, found by reading the code instead of the README.** The
brief asked for the page to be written against `editor/` reality. It is, and
four things the repository says about itself are not true:

- **`/install` printed a build that does not make F5 work.** §06 said
  `cargo build --release --manifest-path vyrn-lsp/Cargo.toml`, and
  `extension.js` resolves its development server at
  `compiler/vyrn-lsp/target/debug/<exe>` — the release binary is in a directory
  it never looks in. A reader who followed the page got "language server not
  found". The two paths need two different builds, and the page's install
  section now names each one for what it is: `npm run package` builds *release*
  and puts the binary inside the `.vsix`; the F5 host wants the *debug* build
  (which is what the repository's own `build-lsp` task does).
- **Two commands are registered and not declared.** `extension.js` registers
  `vyrn.bench` and `vyrn.benchAll`; `package.json` `contributes.commands` lists
  only `vyrn.run`, `vyrn.test`, `vyrn.testAll` and `vyrn.dev`. The bench
  CodeLenses work — a lens invokes a registered command directly — but neither
  appears in the Command Palette. The page therefore claims the bench *lens* and
  claims no palette entry for it.
- **The extension's README is stale in two places.** It ends by pointing at
  `editor/vscode/ROADMAP.md`, which does not exist, and it lists user
  `protocol`/`impl` method resolution as deferred while
  `compiler/vyrn-frontend/src/symbols.rs` builds the `impl_members` and
  `protocol_members` tables that `.`-completion answers from. The page follows
  the code. The README is not this milestone's ground and is left for a fix of
  its own.
- **`.von` is coloured and never analyzed.** `package.json` declares `von` as a
  language with the Vyrn grammar, and the client's document selector names
  `vyrn` and `vyx` only. So the page says the grammar colours all three
  extensions, and says in the same section that a `.von` file gets nothing else —
  rather than letting a reader infer a language server from a language id.

**Other editors, checked before claiming it.** `main.rs` starts with
`Connection::stdio()` and there is no socket path anywhere in the crate, so the
page says what is true: any editor with a generic LSP client can spawn
`vyrn-lsp` and get every row above except the lenses, which the client draws;
there is no port to connect to.

**One number.** The export publishes 206 routes rather than 205 and the new page
is 16 KB. Nothing else moved.

**M4 — accessibility and theme.** *Implemented.* The control, the palettes, the
checklist — each item verified in a browser and recorded here. Gate: the
checklist has no unchecked row, and the verification method for each row is
named (keyboard walk, contrast measurement, forced-colors, reduced-motion
emulation).

### M4 — as landed

**The control.** Three buttons in a group labelled `Theme` — System, Light,
Dark — in the masthead of both fronts, from one function (`themeControl` in
`site/app/nav.vyrn`). The consumer layout writes it into `<template>` and the
backstage builds its masthead as a string; both call the same code, so the two
fronts cannot drift.

It writes one attribute on `<html>`: `data-theme="light"`, `data-theme="dark"`,
or nothing at all for system. Nothing is the default, and nothing is the only
state a browser with no script can be in.

**Why it is a fourth file and not part of `widgets.js`.** `site/public/theme.js`
is a *classic* script in `<head>`. `widgets.js` is a module, and a module is
deferred — it runs after the document is parsed and after the first paint, which
is exactly the flash the file exists to prevent: a reader who chose dark would
get a white page and then a dark one, on every navigation. A classic head script
blocks rendering, so the attribute is on the element before anything is painted.
Four hundred bytes on every page is the price of not flashing, and the export
asserts both halves of it — the tag is `<script src>` and not
`<script type="module">`, and it is before `<body>`.

**And the no-script rule.** The group is `display: none` until `theme.js` marks
the document with `data-js`. A control that renders, takes focus and does
nothing is worse than no control: it claims a choice the page cannot honour.
With no script the site is what it was before this milestone —
`prefers-color-scheme` decides, and nothing on the page says otherwise.

**The palettes.** The light palette is the base token block on `:root`. The dark
one is one block of twenty tokens, applied by two selectors, because the explicit
choice has to beat the system in *both* directions and that takes two rules:

| The reader has chosen | The system says | What applies | Through |
|---|---|---|---|
| nothing | light | light | the base `:root` block |
| nothing | dark | dark | `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) }` |
| light | dark | **light** | the guard above matches nothing on that document |
| dark | light | **dark** | `:root[data-theme="dark"]` |

A selector list cannot hold a media query, so the block is written twice. The
second copy is held identical to the first by a test and not by care —
`site/test/contrast.test.mjs`, "the explicit dark choice and the system dark
default carry the same tokens", which also fails if the guard is ever dropped.

**The token census.** The sheet was already nearly all tokens, and this says what
actually moved rather than claiming a sweep that was not needed:

| | Before | After |
|---|---:|---:|
| Colour literals on a property (not a token definition) | 2 | **0** |
| Distinct colour tokens | 20 | **25** |
| Places in the file where a colour is redefined per theme | 3 | **1** (one block, two selectors) |

The two literals were the string colour in the syntax highlighter, light and
dark. The five new tokens are the syntax roles (`--syn-kw`, `--syn-str`,
`--syn-com`, `--syn-type`, `--syn-num`); `--lane-a` and `--lane-b` moved up from
a `:root` rule of their own. The point of the move is not tidiness: a colour
written on a `.s` rule eight hundred lines down is a colour nothing can measure,
and the whole palette is now in one block that a program reads.

**The contrast checker.** `site/test/contrast.test.mjs`, run by the same
`node --test "site/test/*.test.mjs"` line the base-path and import-line suites
use. It reads `site/public/style.css`, parses the two token blocks, resolves
`var()`, `oklch()` and `color-mix(in oklab, …)` the way a browser does —
including a mix with `transparent`, which is the other side at that alpha,
composited onto the backdrop each pair names — converts OKLab to linear sRGB and
computes the WCAG ratio. Twenty pairs, both palettes, forty measurements:

| Palette | Pair | Measured | Needs |
|---|---|---:|---:|
| light | body text | 17.50:1 | 4.5:1 |
| light | body text on a plate | 16.26:1 | 4.5:1 |
| light | secondary prose (`.lede`, `.note`, `.notice`) | 7.88:1 | 4.5:1 |
| light | secondary prose on a plate | 7.32:1 | 4.5:1 |
| light | meta text (`.modlist .count`, `.rail .n`, chart axis) | 4.91:1 | 4.5:1 |
| light | meta text on a plate (line numbers, `.lines .cl.head`) | 4.57:1 | 4.5:1 |
| light | a link, and every accented heading | 12.04:1 | 4.5:1 |
| light | a link on a plate | 11.19:1 | 4.5:1 |
| light | failure (`.diag.error`, `.pill`, a trap) | 4.88:1 | 4.5:1 |
| light | a pre-release (`.pill.warn`, `.diag.warning`) | 5.26:1 | 4.5:1 |
| light | inline code, in its own wash | 14.82:1 | 4.5:1 |
| light | a keyword | 5.90:1 | 4.5:1 |
| light | a string | 6.38:1 | 4.5:1 |
| light | a comment | 7.32:1 | 4.5:1 |
| light | a type | 11.19:1 | 4.5:1 |
| light | a number | 4.89:1 | 4.5:1 |
| light | the focus ring | 12.04:1 | 3:1 |
| light | the focus ring on a plate | 11.19:1 | 3:1 |
| light | an ownership lane | 4.28:1 | 3:1 |
| light | the other ownership lane | 5.26:1 | 3:1 |
| dark | body text | 17.92:1 | 4.5:1 |
| dark | body text on a plate | 16.83:1 | 4.5:1 |
| dark | secondary prose | 7.87:1 | 4.5:1 |
| dark | secondary prose on a plate | 7.40:1 | 4.5:1 |
| dark | meta text | 5.36:1 | 4.5:1 |
| dark | meta text on a plate | 5.04:1 | 4.5:1 |
| dark | a link, and every accented heading | 12.66:1 | 4.5:1 |
| dark | a link on a plate | 11.89:1 | 4.5:1 |
| dark | failure | 7.32:1 | 4.5:1 |
| dark | a pre-release | 10.20:1 | 4.5:1 |
| dark | inline code, in its own wash | 13.91:1 | 4.5:1 |
| dark | a keyword | 7.76:1 | 4.5:1 |
| dark | a string | 9.54:1 | 4.5:1 |
| dark | a comment | 7.40:1 | 4.5:1 |
| dark | a type | 11.89:1 | 4.5:1 |
| dark | a number | 9.58:1 | 4.5:1 |
| dark | the focus ring | 12.66:1 | 3:1 |
| dark | the focus ring on a plate | 11.89:1 | 3:1 |
| dark | an ownership lane | 8.26:1 | 3:1 |
| dark | the other ownership lane | 10.20:1 | 3:1 |

**Four of those pairs were failing when the checker was first run**, all in the
light palette, and each was a colour somebody had picked by eye: meta text at
3.01:1 and 2.80:1 (`--n2`, on `.modlist .count`, the rail numbers, the chart
axis and every line number), the pre-release amber at 3.50:1, and the keyword
teal at 3.98:1 on a code plate. Four tokens were darkened until the checker
passed. That is the argument for mechanizing this row: the sheet had a written
colour discipline, an author who cared, and four failures in it anyway.

Shown failing on purpose, by putting `--n2` back where it was:

```
✖ the light palette meets WCAG AA
  AssertionError: light: 2 pair(s) below AA
    meta text (.modlist .count, .rail .n, chart axis): 3.01:1, needs 4.5:1 (var(--n2) on var(--paper))
    meta text on a plate (line numbers, .lines .cl.head): 2.80:1, needs 4.5:1 (var(--n2) on color-mix(in oklab, var(--plate) 45%, var(--paper)))
```

**Two SVGs full of links, and two opposite answers.** The import graph on
`/docs` and the record strip on `/backstage` were both `role="img"` with an
`aria-label`, and both contain links — so both had dozens of tab stops present in
the tab order and absent from the accessibility tree, which is the worst of both.
The fix is not the same, and what decides it is whether the same information is
anywhere else on the page:

- **The graph says who imports whom, and nothing else on the site does.** It is
  a `role="group"` now, each node's link named with what the highlight draws —
  `std/json, imports 2 modules, imported by 5 modules` — and the hover lighting
  answers to `:focus-visible` as well, drawn on the hit rectangle the node
  already carries. A keyboard reader gets the same two facts a mouse reader gets.
- **The strip is the index table twenty lines below it, record for record.** It
  stays a `role="img"` with a text alternative, which is what it is, and its 104
  cells leave the tab order. The page's tab stops went from 323 to 219.

The five charts on `/compare` are the graph's case: their rows link to the
example on GitHub and nothing else on the page does, so they are groups now and
each row's link is named from the same sentence its `<title>` carries —
`examples/fib.vyrn -> 1522 bytes of wasm`. The schematic on `/` has no links and
stays a `role="img"`, correctly.

**Where a soft navigation leaves the reader.** Measured, and it was nowhere:
`vyrn-nav.js` swaps `<main>` and scrolls to the top, and the link that was
clicked went with the old `<main>`, so focus fell to `<body>` and the next Tab
started at the masthead again. A screen reader was told nothing at all, because
no document load happened. Both halves are fixed in `widgets.js` rather than in
`vyrn-nav.js` — the shell this site wears is this site's business, and the
navigator is shared with two other applications. Focus goes to `#main`, the
sized-nothing anchor the skip link already lands on, and the new `document.title`
goes into the live region the copy buttons already report into.

**The playground says what it did.** `Run` is a button press with no page load
behind it, so without a live region a screen reader learns nothing about the
result. Both output panes — the diagnostics and the program's output — are
`aria-live="polite"` now. Polite and not assertive: the reader asked for this,
and it can wait for the sentence they are on.

**Forced colours.** Three things on the sheet said something in colour alone and
now say it in a way that survives a forced palette: the pressed theme button (a
box), the playhead and changed rows in a code plate (an outline), and a pill (a
border). Everything else is hairlines, type and structure, and the navigation
marker was already an underline rather than a colour.

### M4 — the checklist

No unchecked row. The method column says how each one was checked, and says it
exactly: **browser** means it was done in a real browser against the exported
tree served over HTTP; **read** means the emitted HTML, CSS or JavaScript was
read; **program** means a test measured it. Where a row says *read*, it says so
because the browser available in this environment emulates `prefers-color-scheme`
and does not emulate `forced-colors` or `prefers-reduced-motion`.

| # | What | Result | Method |
|---|---|---|---|
| 1 | The theme control is reachable by keyboard, in tab order, with a visible focus ring | pass | **browser** — keyboard walk from a cold load: Tab 1 is the skip link (on screen, 2px ring), then the wordmark, the nine navigation rows, then `Follow the system theme`, `Light theme`, `Dark theme`, each with a 2px solid ring, then the page's own content |
| 2 | Each state of the control has an accessible name and states whether it is on | pass | **browser** — the accessibility tree reads `button "Follow the system theme"`, `button "Light theme"`, `button "Dark theme"`; `aria-pressed` tracks the choice (`system:true` → `light:true` → `dark:true`) |
| 3 | An explicit **light** choice beats a system set to **dark** | pass | **browser** — system emulated dark, `Light` pressed: `data-theme="light"`, `background-color: oklch(0.975 0.002 60)`, `color-scheme: light` |
| 4 | An explicit **dark** choice beats a system set to **light** | pass | **browser** — system emulated light, `Dark` pressed: `data-theme="dark"`, `background-color: oklch(0.155 0.005 60)` |
| 5 | **System** clears the choice and follows `prefers-color-scheme` again | pass | **browser** — `System` pressed: attribute removed, `localStorage` key removed, palette back to the system's |
| 6 | The choice persists across a reload | pass | **browser** — chose light, reloaded: `data-theme="light"` already applied, `light:true` marked, paper light |
| 7 | No flash of the wrong theme on load | pass | **browser** + **program** — the live document's head holds `classic theme.js` before `module widgets.js`, so the attribute is set by a render-blocking script before the first paint; `site/export.vyrn` asserts the tag is classic and before `<body>` on a root page, a page three deep and a backstage page |
| 8 | With no script the control is not shown, and the system decides | pass | **browser** — with `data-js` removed the group computes `display: none` (with it, `flex`); the palette still follows the media query, which is the pre-M4 behaviour |
| 9 | The backstage carries the same control and the same palette | pass | **browser** — `/backstage.html`: three buttons, `Dark` gives `oklch(0.155 0.005 60)`, `System` restores; the page loads `theme.js` and nothing else |
| 10 | Both palettes meet WCAG AA on text (4.5:1) and non-text (3:1) | pass | **program** — `site/test/contrast.test.mjs`, 20 pairs × 2 palettes, table above; shown failing once on a deliberately broken `--n2` |
| 11 | The explicit dark block and the system dark block cannot drift | pass | **program** — same file, "the explicit dark choice and the system dark default carry the same tokens"; it also fails if the `:not([data-theme="light"])` guard is dropped |
| 12 | Skip-to-content is the first tab stop and lands before the content | pass | **browser** — Tab 1 on a cold load focuses `a.skip`, which moves to `left: 16px` and takes the ring; `site/export.vyrn` asserts one skip link and one `#main.skip-target` on all three page shapes |
| 13 | Every focusable thing on a page has an accessible name | pass | **browser** — every `a[href]`, `button`, `input`, `select`, `textarea` and positive-`tabindex` element audited on `/`, `/docs`, `/explore`, `/compare`, `/play` and `/backstage`: 104, 323 (now 219), 21 and the rest, **0 unnamed** |
| 14 | The module search is labelled and reports its result to a screen reader | pass | **browser** — `<label for="std-q">Search modules and exports`, and the count is `aria-live="polite"`; the rows carry their own haystack, so it still works with no script (RFC-0105 M2) |
| 15 | The import graph lights on focus and not only on hover, and its nodes are named | pass, with one part read rather than walked | **browser** — `role="group"`, 37 links, each named `std/…, imports N modules, imported by N modules`; a `focusin` on a node lights that node, its neighbours and its wires, checked in the live page. The step not walked is the last one: a keyboard Tab *into* an SVG `<a>`. `element.focus()` on an SVG link in this engine sets `document.activeElement` and fires no focus event at all, so it does not stand in for a Tab, and the pane stopped accepting real key input before that walk could be made. What the handler answers to is `focusin`, which is what a real Tab fires; the focus ring is CSS (`:focus-visible` on the node's hit rectangle) and needs no event |
| 16 | No SVG puts focusable links inside a `role="img"` | pass | **browser** — audited every `<svg>` on `/`, `/docs`, `/compare` and `/backstage`: the graph and the five charts are groups with named links; the strip's 104 cells are `tabindex="-1"` behind a labelled `role="img"`; the schematic has no links |
| 17 | The playground's controls are reachable and named | pass | **browser** — 21 focusable, 0 unnamed; the editor is `aria-label="Vyrn source"`, standard input `aria-label="Standard input"`, the example picker has an `sr-only` label, `Copy link` and `Run` name themselves |
| 18 | The playground says what running produced | pass | **read** — both output panes are `aria-live="polite"` in `site/app/routes/play.vyx` |
| 19 | A soft navigation puts focus somewhere and announces the new page | pass | **browser** — followed a link out of `<main>`: `activeElement` is `#main.skip-target` and the live region reads `Install — Vyrn`; before this milestone focus was on `<body>` and nothing was announced |
| 20 | Landmarks and navigation labels | pass | **browser** — one `<header>`, one `<main>`, one `<footer>` per page; the consumer navigation is `<nav aria-label="Site">` and the backstage's is `<nav aria-label="Backstage">`, so the two fronts are told apart |
| 21 | No duplicate `id` on a page | pass | **browser** — audited on `/` and `/docs`: none |
| 22 | Every image has a text alternative | pass | **browser** — no `<img>` without `alt` on any page audited; the only raster surface is the hero `<canvas>`, which is decorative |
| 23 | Motion is behind `prefers-reduced-motion` | pass | **read** — 12 declarations in the sheet name a transition or an animation, 0 `@keyframes`, 1 `scroll-behavior: smooth`; all of them are covered by the reduced-motion block's `*, ::before, ::after { transition-duration: 1ms !important; animation-duration: 1ms !important }` and `html { scroll-behavior: auto }`, which the live document's CSSOM confirms is present. The two scripts guard themselves as well: `widgets.js` reads the query once and consults it at six sites, `hero.js` at three |
| 24 | Forced colours keep every colour-only distinction | pass | **read** — a `@media (forced-colors: active)` block gives the pressed theme button a box, the playhead and changed code rows an outline, and a pill a border; confirmed present in the live document's CSSOM |

Gate: met. Twenty-four rows, no unchecked one, and the method named on each.

**One number, and one contradiction.** The export publishes the same 206 routes
and one more asset (`theme.js`, 4 KB); it takes **73 s** against M2's 74 s, which
is noise. The contradiction is row 13's own history: the site had a written
colour discipline, a skip link, a focus-ring rule, a `prefers-reduced-motion`
block and `focusin` handlers on the graph *before this milestone* — and it still
had four contrast failures, thirty-seven unnamed tab stops inside a `role="img"`,
104 duplicate ones on the backstage, and a soft navigation that dropped focus on
the floor. Every one of those was found by running something rather than by
reading the CSS, which is the sentence the design paragraph opened with.

## What this RFC does not do

- It does not add a second deployment, a second domain, or a build flag. One
  export, two navigations, the document-relative URL rule untouched.
- It does not add a JavaScript framework, a CSS framework, or any dependency.
- It does not invent a package registry. The explorer renders what exists —
  `std/` and the examples — in a shape that will fit a registry when one does.
