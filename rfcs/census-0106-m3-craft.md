# RFC-0106 M3 — the craft census

The user rejected the first M3 round: the defects they listed were "only a small
portion of issues". This file is the adversarial re-audit that followed, written
before anything was fixed. Every row was produced by measuring the exported tree
in a browser at 1280 and 375 in both palettes, page by page, against the four
reference pages the user named.

Status values: **FIXED**, **DEFERRED** (with the reason), **NOT A DEFECT** (a
probe hit that survived inspection, kept so nobody re-files it).

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
