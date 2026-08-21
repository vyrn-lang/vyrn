# RFC-0106 M3 — the craft census

The user rejected the first M3 round: the defects they listed were "only a small
portion of issues". This file is the adversarial re-audit that followed, written
before anything was fixed. Every row was produced by measuring the exported tree
in a browser at 1280 and 375 in both palettes, page by page, against the four
reference pages the user named.

Status values: **FIXED**, **DEFERRED** (with the reason), **NOT A DEFECT** (a
probe hit that survived inspection, kept so nobody re-files it).

**Sections A to I are the second round. Section J is the third**, added after the
user read the second round and sent back eight findings — six of which are
defects the second round created or left standing, plus two the sweep written for
them found on its own.

## A. Command blocks that wrap — the defect the user led with

`.cmd code` was `white-space: pre-wrap` on the index, the install page and the
pillar cards. M2 introduced that rule to stop a command being CLIPPED at
`…/vyrn/main/inst`, and it traded one defect for a worse one: a shell command is
one line, and a command broken across three is a command a reader cannot trust
they have copied whole. Line counts below are real line boxes (a `Range` over
the text node), not height arithmetic.

| # | Page | Element | Lines | Box | Status |
|---|---|---|---:|---:|---|
| A1 | index | hero install command, macOS | 2 | 521px | FIXED |
| A2 | index | hero install command, Linux | 2 | 521px | FIXED |
| A3 | index | hero install command, Windows | 2 | 521px | FIXED |
| A4 | index | pillar card, `vyrn check` | 2 | 186px | FIXED |
| A5 | index | pillar card, `vyrn why --memory` | 3 | 186px | FIXED |
| A6 | index | pillar card, `vyrn build --target wasm` | 4 | 186px | FIXED |
| A7 | index | pillar card, `vyrn emit-gen` | 2 | 186px | FIXED |
| A8 | install | hero install command | 2 | 640px | FIXED |
| A9 | releases | upgrade command | 2 | 523px | FIXED |

Fix: `white-space: pre` with `overflow-x: auto` on the code element — the block
scrolls, the page does not, and the command is never broken. The `pre-wrap`
ruleset M2 added for `.ospane`, `[data-install]` and `.cards` is deleted.

| # | Defect | Status |
|---|---|---|
| A10 | the hero command box was capped by the lede's measure, so a 90-character command had 521px | FIXED — the command block is wider than the lede on both heroes, as the reference's is |
| A11 | `Copy` buttons were 50, 73, 95 and 118px tall on one page, because each stretched to its wrapped command | FIXED — a consequence of A1-A9; every `.cmd` is one row now |

## B. The OS picker

| # | Page | Defect | Status |
|---|---|---|---|
| B1 | index / install | TWO different pickers for the same job: the index had underline tabs driven by `tabsWidget`, the install page had the CSS-only radio picker | FIXED — one component, the radio picker, on both |
| B2 | install | the tiles floated free of the command block: a 24px gap, and the tile row was narrower than the box under it | FIXED — tiles and command are one bordered unit, borders merged, no gap |
| B3 | install | tile widths were content-sized (`macOS` narrower than `Windows`) | FIXED — equal columns |
| B4 | index | the picker guessed the visitor's OS; the CSS picker always opens on macOS | FIXED — three lines of script check the matching radio, and with no script the first is still checked |

## C. Stale release data

| # | Defect | Status |
|---|---|---|
| C1 | `site/release.txt` is committed as `v0.1.0-alpha.1` (2026-08-11) while `v0.1.0-alpha.2` was published 2026-08-18. The index, install and releases heroes all featured the older one | FIXED at the source, twice over — see below |
| C2 | a `.notice` box on install and releases apologized for C1 at run time ("A newer release, v0.1.0-alpha.2, is available…") | FIXED — deleted, along with `fresh.js` and the request it made to GitHub on every visit |
| C3 | nothing failed when the two disagreed | FIXED — an assertion in `site/export.vyrn` |

The root cause is that `release.txt` is a hand-refreshed fallback, and the tree
already holds the answer: `scripts/site-history.py` writes every git tag into
`site/data/history.json`. `repo.vyrn` now reconciles the two and takes the newer
tag, so a stale `release.txt` cannot reach a page. `release.txt` is also updated.

## D. Lines that do not earn their place

| # | Page | Line | Verdict | Status |
|---|---|---|---|---|
| D1 | install | `vyrn-aarch64-macos.tar.gz into ~/.vyrn/bin` (and two more) | the archive's filename is not a fact a reader installing needs; the install PATH is | FIXED — the filenames go, one line about the path moves into Advanced |
| D2 | install | `v0.1.0-alpha.1, published 2026-08-11 as a pre-release.` | restates the kicker three lines above it | FIXED — reduced to the `Release notes` link |
| D3 | index | `Checksum-verified. v0.1.0-alpha.1 today. All platforms` | "today" means nothing to a reader; the version restates nothing they asked for | FIXED — `Checksum-verified.` and the link |
| D4 | install, releases | the empty `role="status"` notice | apologizes for C1 | FIXED — deleted |
| D5 | explore, guide, why-vyrn | section kickers numbered `01 — `, `02 — `, `03 — ` | the rail beside them already numbers the sections; the M3 pages dropped the prefix and these did not | FIXED — swept |
| D6 | play | the notice explaining which engine the playground runs | a real disclosure a reader cannot infer | NOT A DEFECT |
| D7 | index | `Recorded by running vyrn 0.1.0-alpha.2 on 2026-08-21.` | provenance for output claimed to be real | NOT A DEFECT |
| D8 | releases | `Upgrading is the same line.` | answers the question the page raises by showing an install command | NOT A DEFECT |

## E. Control craft

Seven different control heights at 1280px, measured: 44 (masthead), 56 (OS
tile), 33 (chip), 32 (button), 29 (content CTA), 27 (radar key), 25 (pill).

| # | Defect | Status |
|---|---|---|
| E1 | `.cta` in content was **17px sans, 29px tall, no min-height** — it inherited the body font, where every other control on the site is 12px mono small-caps. Four of them on the index, four on install | FIXED — `.cta` carries its own font and box; one appearance in the masthead and in content |
| E2 | `.chips li` 33px against `.pill` 25px, for two things that are both static labels | FIXED — one static-label box, 26px |
| E3 | `.cmd button` had no fixed height, so `Copy` was a different size on every page | FIXED with A11 |
| E4 | the OS tile at 56px was the tallest control on the site and did not match the command box it sat over | FIXED with B2 |

## F. Section rhythm holes

The gap scale is otherwise consistent across all nine pages (kicker 8, heading
16, artifact 32, CTA row 24, notice 24).

| # | Defect | Status |
|---|---|---|
| F1 | a kicker followed by an artifact instead of a heading got 32px on the index, 16px on install and 8px on releases | FIXED — one rule, 24px |

## G. Probe hits that are not defects

| # | Hit | Why not |
|---|---|---|
| G1 | "two different left edges inside a section" on every `.split` | that is what a two-column grid is |
| G2 | `.band` reports `scrollWidth > clientWidth` | the seam's own bleed, bounded by `--gut`, and the document width is unchanged |
| G3 | `.cap` paragraphs at 999px | LABEL role: a caption takes the width of the block it names |

## I. Found by the verification itself, after the fixes

Three defects only appeared once the fixes above were measured, and two of them
were the fixes' own.

| # | Defect | Status |
|---|---|---|
| I1 | with `white-space: pre` restored, the install command took the DOCUMENT to 706px inside a 320px phone: a flex item's automatic minimum is its min-content, and the min-content of a `pre` command is the whole command | FIXED — `min-width: 0` on `.cmd code`, which is what makes a flex scroller actually scroll |
| I2 | `.hero.mid` painted 690px in a 278px column. `margin: 0 auto` on a grid item makes it self-align `center` instead of `stretch`, and a non-stretched grid item is sized `fit-content` — so the section took the min-content of the widest thing in it | FIXED — an explicit `width: 100%`; the auto margins still centre it where the track is wider |
| I3 | the masthead row, deferred by M1, M2 and M3's first round | **FIXED, and it was not planned.** Giving `.cta` its own 12px mono font (E1) took the row from 59px over at 320 to 54. The search control's word going at phone widths took it to 2. The Install button's side padding took it to **0** |

**Every consumer page now has zero horizontal overflow at 320, 360, 375, 767 and
1280px.** 45 page-width pairs measured, 45 clean.

## H. Deferred

| # | Item | Why |
|---|---|---|
| H1 | ~~the masthead takes the document to 379px at 320 and 375~~ | **not deferred after all — fixed, see I3** |
| H7 | the index's and the releases page's install command scrolls rather than showing whole: their columns are 470-590px and the command is 664px | the instruction was `nowrap` plus a scroller, and this is that. The install page is one click away and shows it whole at 800px, which is the page a reader goes to in order to READ it |
| H2 | `/guide`'s chapter list carries 14 `.note` elements | M4's page; the fix is the one `/docs` just took |
| H3 | `/compare` has 14 blocks wider than their container at 1280px | M4's page, and M0 already opened the entry |
| H4 | `/docs` is 52,815 bytes against M0's 40,000 | the import graph is 25,347 of them; moving it off the landing is M4's decision |
| H5 | `widgets.js` is 17,762 of the 31,700 gzipped bytes every page fetches | its comments cannot be stripped by a scanner the size of the stylesheet's |
| H6 | the census page list still names `/philosophy` and `/editors`, now redirect stubs | M5 owns the census wiring |

## J. The third round — the eight the user sent back

The user read the second round and returned eight findings. They are recorded
here in their own section rather than mixed into the tables above, because six of
the eight are defects the second round CREATED or left standing, and that is the
more useful thing to keep.

| # | Page / file | Defect | Status |
|---|---|---|---|
| J1 | `.github/workflows/site.yml` | the `Site / build` job failed: `site-demo.py` raised `FileNotFoundError` on `compiler/target/release/vyrn` | **FIXED** — the path, not the workflow. See below |
| J2 | index, install | three OS tiles, two of which carried the same command; the target was `raw.githubusercontent.com`; `Checksum-verified.` in the hero | **FIXED** — two tiles, the site's own origin, and the line is gone |
| J3 | install | the OS guess did nothing on the page a reader installs from | **FIXED** — `data-os` was on the index's radios and not on the install page's, so the selector matched nothing there |
| J4 | releases, nav | an install command duplicating `/install`, and a fifth masthead row for a changelog | **FIXED** — the block is one link, the row is gone, and the index carries a `Latest release` section |
| J5 | search overlay | the result list did not scroll, and the scrim painted white in dark mode | **FIXED** — one rule named an attribute the markup does not carry, and the scrim was an alias of the ink |
| J6 | every page | elements sitting closer to a neighbour than the rhythm allows | **FIXED** — three rules, and a fourth defect the sweep found on its own (J10) |
| J7 | — | "we take the reference site's good decisions, we do not copy wholesale" | recorded in the RFC's as-landed section |
| J8 | index | the page carries a release teaser now and the word budget is 260 | **FIXED** — 260 exactly, after `Every release` became `Releases` |

### J1, the one that blocked everything

The workflow DOES build the binary — `cargo build --release -p vyrn-cli`, the
first step, 62 seconds — and the three steps between the build and the failure
ran that same binary by that same path. The bug is in `scripts/site-demo.py`:
every recorded step runs in a scratch directory, and **on POSIX a relative
program path is resolved against the CHILD's working directory**, not the
parent's. Windows resolves it against the parent's, which is why the script had
always worked on the machine it was written on. `shutil.which` does not help: a
path with a separator in it comes back unchanged. One `os.path.abspath`.

Where the seventeen minutes went, measured on the failed run: 62 s to build the
CLI, 25 s + 45 s for the playground's wasm build and host tests — and **907 s in
`The site's own tests`, of which 687 s is `vyrn test site/export.vyrn` alone**.
A cache takes the 132 s of cargo work off a warm run and cannot touch the rest.
Two things changed: `Swatinem/rust-cache` on its own slot, the pattern `ci.yml`
already uses, and the demo and history steps moved ABOVE the fifteen-minute test
loop, so a broken export input fails the job in two minutes instead of at
minute seventeen.

### J5, and why a search box can pass every test and still be broken

`.findpanel [data-search-results] { overflow-y: auto }` — and the markup carries
`data-find-results`. Nothing in the panel had ever scrolled: a query with forty
results drew all forty, the panel clipped at `70vh`, and the rows past the edge
could not be reached with the mouse or with the arrow keys. `min-height: 0` is
the other half, for the same reason `.cmd code` needs it.

The scrim was `color-mix(in oklab, var(--n0) 45%, transparent)`, and `--n0` is
the INK: near-black in the light block, near-white in the two dark ones. So the
dialog dimmed the page in light mode and washed it white in dark mode. It is a
`--scrim` token now, defined in all three blocks, because a scrim darkens what is
behind it in both palettes and is not a neutral-ramp alias.

### J9 and J10 — found by the sweep, not by the user

| # | Defect | Status |
|---|---|---|
| J9 | `.band` prose glued to the block above and below it: `/compare`'s radar section had `The chart, as a table.` touching the plate above and the table below; `/editors` had the same pair; `/explore/shelf` had three `<h3>`s sitting on a caption's last line | **FIXED** — three rules, all in the section-rhythm block |
| J10 | `.split > * { margin-top: 0 }` — the rule that exists to kill the dead strip at the top of a right column — is specificity (0,1,0) and lost to every `element.class` selector in the sheet. `ul.plain` is (0,1,1), so the reset never reached the block it is most often applied to: three pages started their right column 24px below their left | **FIXED** — the class is written twice, `.split.split > *`, which is (0,2,0). Order cannot fix this: a lower-specificity rule loses wherever it sits |

### The gap assertion, and what it can and cannot be

The sweep gained one: **for every pair of stacked neighbours inside a `<section>`
or a `.say` column, the gap is at least 8px.** 26 page-width pairs (13 pages ×
1280 and 375), all clean.

`.cap` is exempt, and that is a decision rather than a hole: `.cap` carries its
own `border-top` and padding and is built to sit flush with the block above it —
that rule is what says the caption belongs to that block. Its OTHER side is not
exempt, which is J9's third row.

It is not a committed test. Every assertion in `site/test/*.test.mjs` reads the
exported bytes; a gap is a layout fact, and measuring one needs a layout engine.
Adding a headless browser to this repository to assert 8px is a dependency the
site does not otherwise have, so the sweep stays a browser script and the numbers
stay here.

### Probe hits from the third round that are not defects

| # | Hit | Why not |
|---|---|---|
| J11 | `/docs`'s `.modgrid` is 32px wider than its container at every width | the rule grid's own bleed, `margin: 0 calc(-1 * var(--s2))`, bounded by `--gut` and documented at the rule. The document width is unchanged |
| J12 | the install page's `git clone … / cd … / cargo build …` block reports three lines | it is three commands and is written with three newlines. The wrap assertion compares line boxes to newlines now, not to 1 |
| J13 | `/play` at 375: the example `<select>` is 22px inside a 4px `.pickbox` | real, pre-existing, and not this round's: `/play` is not a page M3 touched and the fix is a decision about that toolbar's flex model. Deferred, recorded here so it is not re-filed |
