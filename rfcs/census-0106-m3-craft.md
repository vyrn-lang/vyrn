# RFC-0106 M3 — the craft census

The user rejected the first M3 round: the defects they listed were "only a small
portion of issues". This file is the adversarial re-audit that followed, written
before anything was fixed. Every row was produced by measuring the exported tree
in a browser at 1280 and 375 in both palettes, page by page, against the four
reference pages the user named.

Status values: **FIXED**, **DEFERRED** (with the reason), **NOT A DEFECT** (a
probe hit that survived inspection, kept so nobody re-files it).

**Sections A to I are the second round, J is the third, K is the fourth.** Each
was added after the user read the round before it: eight findings in the third
round, seven in the fourth, and in both rounds most of what came back were
defects the previous round had created or left half-made — plus the ones the
sweep written for them found on its own.

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

## K. The fourth round — seven from the live export

The user read the deployed tree and sent seven. Six are defects a fix in an
earlier round created or left half-made; the seventh is the index being thin.

| # | Page / file | Defect | Status |
|---|---|---|---|
| K1 | index | the recorded demo: one step's output floating top-right of an empty card, six flat title bars under it, the caption overlapping the last row | **FIXED** — the CARD is the split now, not the row |
| K2 | every chart | SVG text at 3.0 to 11.8 px, measured. Nothing on the site reached 12 | **FIXED** — two chart type steps, a ceiling and a floor on the scale |
| K3 | bench records | the machine's host name in four committed records, in their file names, and in a caption on two pages | **FIXED** at the recorder, the records and the display |
| K4 | index, install | the selected tab's border thinner than its neighbours | **FIXED** — the cause was two negative margins over a subpixel grid |
| K5 | masthead | an 83px icon-and-word button where the reference ships a field | **FIXED** — a 272px field above 1024px, the old two forms below it |
| K6 | index, play | the hero editor: no reset, an output pane that resized between states, a failure path that left the controls live | **FIXED** — and `Ctrl+Enter` and the exit code were already there |
| K7 | index | "bun has more interesting stuff on index page" | **FIXED** — two sections, both generated from the tree |

### K1, and why it only broke in the state a reader sees

Every `li` was its own 2fr/3fr grid with the step's output in column 2 — a shape
drawn for the no-script case, where all seven steps are open and each row reads
"what you typed | what came back". Under the script exactly ONE step is open, so
that grid painted a command in column 1, its output floating top-right of the
same row, and six title bars underneath. **A geometry serving two states and
correct in the rarer one.** The card is the split now: seven numbered rows in
column 1, always whole, and the output in column 2. Every one of the seven was
checked, including the two that are not commands — the file listing, which is
walked, and the edit, which is a file — and those are title rows on purpose.

The provenance line moved OUT of the plate to a `.cap` under it, which is the
class built to sit flush with the block above it. As an `.eyebrow` inside the
card it had landed on the last row.

Two things the fix's own verification found: the output pane at `flex: 1` made a
600px empty terminal for a one-line step and pushed that step's note to the
bottom of the card, 500px from what it is about (now `flex: 0 1 auto` with a
floor and a ceiling); and `<p class="note">` on seven steps took the index's
disclosure count from 1 to 8, because `.note` is what the census counts as a
disclosure and M0's rule is one per plate. It is `.stepnote` now.

### K2, measured before and after

| Chart | Where | Before | After |
|---|---|---:|---:|
| bars | index, 1280 | 6.6–8.0 | 12.4–13.2 |
| bars | index, 375 | 9.0–11.0 | 12.0–12.7 |
| bars | compare, 1280 | 9.6–11.8 | 17.0–18.0 |
| bars | compare, 375 | **3.0–3.6** | 12.0–12.7 |
| radar | compare, 1280 | 7.0–7.8 | 13.2–14.0 |
| radar | compare, 375 | **3.8–4.3** | 12.1–12.9 |
| pulse, arrivals | backstage, 375 | **3.0** | 12.1–12.8 |
| cites | backstage, 1280 | 8.2 | 15.5–16.5 |

Text in a `viewBox` is drawn in user units and scaled with the picture, so a
token's number is not what a reader gets: the scale is the container's width over
the viewBox's, and it ran from 0.33 to 1.07. **The site already had the fix, on
one plate.** `.barstrip .stage { overflow-x: auto }` with a `min-width` has been
on the index since M2, with its reason written in general terms — "a 904-unit
viewBox in a 343px column scales every glyph by 0.38 and a 9px axis label paints
at 3.4px" — and it was never applied to the other nine charts. The same shape as
`.hero.mid > .eyebrow` in the third round: the general fix, made local.

Now: `--t-svg` and `--t-svg-s` at 18 and 17 user units, a `max-width` per family
so no chart is drawn larger than its own units, and a `min-width` at `12/18` of
each viewBox so none is drawn smaller. **The correction the measurement forced:**
the floor was first written inside the phone media block, and a 700px window fell
between the two — no floor, and 11.4px on six charts. A media query cannot see
the CONTAINER's width, which is what the scale depends on; a `min-width` can.

### K3, where the host name came from

`rfcs/bench-0104/harness/run.py` recorded `socket.gethostname()` in every
`environment` block **and named the output file after it** — which is how four
committed records carried a machine's name twice over, and how a caption on `/`
and `/compare` printed `2026-08-19-LOCUST-v2.json` as the provenance of a number.

All three ends are closed: the field is gone from `environment()`, the file name
is the date alone, the four records are rewritten without the field and renamed,
and `envRows()` drops the `Host` row. The CPU, the memory, the OS and every
toolchain version and flag stay, because those are what make a number checkable.
`bench.vyrn`'s own test asserts both halves — no row labelled `Host`, and no
`"host"` key in the record it reads.

**The numbers were NOT re-measured.** Re-running the corpus on another machine
would produce different medians under the same date, which is a worse thing to
publish than a stripped field. What was regenerated is the identifier, not the
measurement.

### K4, the thinner border

The picker built its unit out of four borders per label, `border-left: 0` on the
siblings, and then `margin-left: -1px` and `margin-top: -1px` to pull the checked
tile and the command box back over the borders they doubled. Two negative margins
over subpixel grid columns is a lottery: at a 523px container a `1fr` column is
261.5px, so a shifted 1px border straddles two device pixels and paints as two
half-covered ones — thinner and lighter than its neighbours, differently at 1x
and at 2x, and only on the tile a reader is looking at.

The row draws its own box, the divider is one border on one side of one element,
and the selection is three inset shadows — top and both sides, leaving the tile
open at the bottom into its command box. A shadow takes no space, so nothing has
to be pulled back over anything. Measured at 1280 and 375, on both tabs: the tab
row's border is `1px 0`, the labels are `0` and `1px` left, the command box's top
border is 1px, and the gap between the two boxes is exactly 0.

### K5 and K6, briefly

The search control is a 272px field with its placeholder and a `/` chip above
1024px, and the two narrower forms below it are exactly what they were — the
masthead row fits at 320px with zero pixels to spare (third round, I3), so the
wide form is added only where the row has slack. Measured after: the masthead is
still 64px.

The editor already had `Ctrl+Enter`, the run state and the exit code; what it did
not have was a way back and an honest failure. `Reset` restores the program the
editor started from — the example, the shared link, or the hero's own snippet —
and the pane's idle line with it. The output box has a floor, so idle, running
and ran are the same height. And when the module fails to load, the pane says so,
the textarea goes back to read-only and both controls are disabled, instead of a
live Run button over an editor with no compiler behind it.

Verified in the browser, on the page rather than in an iframe: `Press Run` →
`Loading the compiler…` → `Running…` → `Ran`, with `admitted at 30 / refused: 5
is under 18 / exit 0` in the pane; a program returning 3 shows `The program wrote
nothing. exit 3`; `Ctrl+Enter` runs; `Reset` puts the source, the status and the
idle line back; and the pane is 111px in every state.

### K7, the two sections, and what is NOT in them

Both are generated, and that is the whole design:

- **Five tabs of real code.** Every snippet is a file in `site/guide/` — compiled
  while the index renders, run by `/guide` while that page renders, and checked
  by `vyrn test site/guide/*.vyrn` in CI. No snippet literal was added anywhere.
  The tab strip is the install picker's own control, extended from two positions
  to five.
- **Eight standard-library modules**, the two biggest of each of the reference's
  four groups, with their export counts, from the same generator the reference
  landing reads.

Two things the brief asked for are missing because the tree does not hold them:
`std/stream` has no chapter program, and `protocols` is 31 lines against this
set's 10 to 19, which would have made the section resize under the reader on one
tab. A snippet written for either would have been the one block on the page that
nothing compiles.

**The index ceilings are raised, deliberately: 260 words to 380, and 30,000 bytes
to 34,000.** The page is 372 and 32,023. The prose did not grow — what the census
counts as words here are five tab labels, five captions, eight module names, four
group names and two headings, around code and generated names. The cold load is
40,762 gzipped against 55,000, which is the ceiling that bounds what a reader
actually pays for.

### Found by the fourth round's sweep, and not fixed

| # | Defect | Status |
|---|---|---|
| K8 | `/backstage` takes the document 139px wide at 1280 and 405px at 375 — two `span.name`/`span.note` pairs in a row that does not wrap | **DEFERRED, and pre-existing**: measured identical with the stylesheet as committed before this round, by serving the old sheet against the same document. The backstage is the developer front and is not one of RFC-0106's thirteen census pages; recorded here with the numbers so the milestone that owns it has them |

## L. The fifth round — the finish gap, against the reference site at 1440

The user's verdict on the fourth round: still not bun.sh. Read side by side,
the distance was finish, not structure. Six entries, all fixed:

| # | Element | Defect | Fix |
|---|---|---|---|
| L1 | Every heading | Set in the body stack: an 80px claim rendered as a bolded paragraph | `--sans-d` display stack (Segoe UI Variable Display / SF Pro Display), weight 720, 0.95 leading, −0.042em; display cap 4.5rem → 5.1rem, `--t-h2` cap 2.2rem → 2.75rem; typescale records updated, floor-only landing invariant kept |
| L2 | Command scrollers, hero editor | A scrollbar at rest under the hero command, inside three pillar cards, beside the editor | Bars hidden, panning kept; 24px mask fade where text clips; `resize` grip off the hero textarea |
| L3 | Pillar cards | Three stacked rectangles of equal border weight; two commands clipped | Command on a wash, no inner border; Copy a 30px strip; 13px mono fits all four commands |
| L4 | Content CTAs | Solid accent blocks — four "primary" actions per screen | Accent arrow-links; the masthead Install is the one filled box |
| L5 | Demo step commands | A bordered plate per row inside a bordered card | `$ `-prefixed plain lines |
| L6 | Index bars | 0.18 fill-opacity read as a watermark; values floated over nothing | Focal 0.9, field 0.45, values 600-weight ink |

Cut, not restyled: the hero chips row (restated the headline two inches below
it) and the editor's Ctrl+Enter chip (the binding stays; the hint moved to the
Run button's title). The headline takes two explicit breaks — at 5.1rem,
auto-wrap orphaned "No" at a line end.

## M. The sixth round — the loudness gap

The user's verdict on the fifth: still dirty, elements too attention-grabbing,
not alive. The reference site whispers everywhere except two deliberate pops;
this site set twelve controls per screen in the kicker's own voice — uppercase,
tracked-out, hard-boxed. Entries, all fixed:

| # | Element | Defect | Fix |
|---|---|---|---|
| M1 | Every control | Uppercase 0.1em mono on buttons, nav, tabs, table headers, theme control — the kicker voice on the whole chrome | Caps and tracking stay on `.eyebrow` alone; controls are natural-case 0.02em, on a wash behind a hairline; hover earns the accent |
| M2 | "Runs here" pill | A bordered chip shouting beside two buttons | A breathing accent dot and a quiet label; `prefers-reduced-motion` stills it |
| M3 | Module teaser | Eight boxed inline-code pills read as a tag cloud | Plain accent mono names, no boxes |
| M4 | Theme control | Bordered box at rest | A word; the box appears on hover |
| M5 | Passive frames | `pre.code` and the editor at full `--rule` weight | `--hair`; interactive `.cmd` keeps `--rule` |
| M6 | Full-scale bar | Ran under its own value once bars got pigment | Paper-stroke halo on `.val` (`paint-order: stroke`) |
| M7 | Cards | Inert | Border warms toward the accent on hover, 160ms |

## N. The seventh round — three components against their reference shapes

The user named three components that still read heavier than bun.sh:

| # | Element | Defect | Fix |
|---|---|---|---|
| N1 | Install picker | A fused 44px two-cell segmented bar — the heaviest element in the hero, wider than the command it selects | Small detached chips in the control voice; command first in the index hero (the reference front page's order), chips centred above on `/install`; same radio mechanism, zero script |
| N2 | "A minute with Vyrn" | A Back/Next toolbar over the whole card | The pager lives at the bottom of the pane it pages — `1 / 7 ‹ ›` — the reference site's own placement; steps stay clickable |
| N3 | "Runs here" + "Press Run" | Two adjacent caps labels reading as one confusing element | One state: the breathing dot moved onto the status itself (`● Ready` → `Running…` → `Ran`), on the index hero and the playground both |

## O. The eighth round — chrome is sans, code is mono

Seven rounds in, every control and label was still monospace and still
outlined: the hero viewport held roughly twelve stroked rectangles where the
reference site holds five, and a page whose chrome is all mono reads as
terminal output regardless of case. The material decision the reference site
actually made — sans for chrome, mono only for what is code or data — applied:

| # | Element | Change |
|---|---|---|
| O1 | Masthead, nav, buttons, tabs, footer | Sans at the command size; mono stays on the wordmark, kickers, commands, counters and values |
| O2 | Command boxes | A fill first, hairline second — a lighter field on the ground, not an outlined rectangle |
| O3 | Editor internals | Inner stroke transparent (metrics kept for the overlay); the plate is the frame |
| O4 | Section rhythm | `--sect` 32–56px → 40–76px; compact sections earn more air between them |
| O5 | Eyebrow-links ("Other ways to install", "View the install script", "Release notes") | The arrow-link voice, not caps-mono underline — the loudest links once the chrome quieted |

## P. The ninth round — the plate, the label, the prompt

Against the reference hero one more time:

| # | Element | Change |
|---|---|---|
| P1 | The page | Two full-height hairlines frame a 1232px content column — the reference site's quietest structural device; the section seams already bled to this exact border box |
| P2 | Install cluster | "Install Vyrn <tag>" label above the command (the version is the link), the shell's real prompt in the accent (`$` sh, `>` PowerShell, as pseudo so Copy never captures it), Copy's divider gone, tabs and "Other ways to install →" on one row |

## Q. The tenth round — three components the user cropped

| # | Element | Defect | Fix |
|---|---|---|---|
| Q1 | Demo step commands | The eighth round's command fill leaked into the borderless step lines — a wash strip inside every row, fighting the active row's own wash | `background: none` on step commands and on the pane output; the card's ground is the terminal's ground |
| Q2 | OS switch | Two detached chips | One pill: a single bordered container, quiet word segments, selection a lighter fill inside it — the reference switch. `.tabrow` added so the link out never enters the container; the picker's sibling selectors route through it |
| Q3 | Hero editor | The code floated as a lighter washed box inset in the plate — a box in a box by background | One surface: head, code and output split by hairlines only. The override needed three classes; `.play .editor pre.hl` is declared later and an equal-specificity rule loses — the sheet's own documented trap, hit again |
