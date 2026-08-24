# Visual defects 4 and 5

Branch `site/codeblocks`. Three commits:

| commit | what |
|---|---|
| `a9db74f7` | a code block takes the room its lines take, and no more |
| `d85e5fd4` | the page's colour is decided before the first paint, and with no request |
| `c7b2fa56` | the light paper is a cream a reader can see, not a white |

Defects 1, 2 and 3 were fixed before this branch and are not touched here.

## What could not be verified

**Screenshots.** The browser pane in this session would not composite frames:
every `computer{action:"screenshot"}` call returned `the Browser pane is not
displayed, so the page is not compositing frames` after five seconds. Seven
attempts across three tabs and two servers, including a foreground tab and a
fresh `preview_start`. There are therefore NO before-and-after screenshots in
this report. Every visual claim below is instead a measurement read out of the
live page with `getComputedStyle` and `getBoundingClientRect`, which is what the
screenshots were wanted for and is stricter than reading a picture.

**The white frame itself.** For the same reason, no frame of a cold load was
looked at. The claim in defect 5 is NOT "I saw no white frame". It is the three
measurements in "The evidence" below: the theme script no longer blocks
rendering, the theme is applied with no request, and the browser is told the
page's colour scheme before any CSS is parsed. A reader who wants the frame
itself has to look at it.

## How this was measured

The site was built and served:

```
mkdir -p out/docs/std out/guide out/web out/tooling out/backstage/rfcs out/explore
python scripts/site-history.py > site/data/history.json
python scripts/site-demo.py --vyrn <vyrn> > site/data/demo.json
<vyrn> run site/export.vyrn out          # 80 routes, 14 assets
python -m http.server 8812 --directory /n/wt-css/out
```

The baseline was served the same way from a build of the branch point, and
`git show HEAD:site/public/style.css` was confirmed byte-identical to the
stylesheet that build came from, so the before numbers are this branch's own.

Three pages, each with a different block type:

| page | blocks |
|---|---|
| `/docs/std/http` | 23 `pre.code`, 2 `pre.doccode`, 1 `.cmd` |
| `/guide/coming-from` | 12 `pre.code` inside `.specplate` |
| `/guide/values` | 3 `.plate.block.play` — `pre.code.hl` under a textarea, with `.out` under it |

Viewport 1280x900 unless stated. Body text is `17px/27.2px`, so six lines of
body text stand 163px.

## Defect 4 — code blocks are too big and too clumsy

### Root cause

Four separate things, in `site/public/style.css`:

1. `pre.code` (was line 1188) set `line-height: 1.7` on a 13.5px face — a 22.95px
   line where the body's own line is 27.2px on a 17px face. A code line was
   costing 84% of a body line to carry 79% of the type.
2. It padded `var(--s2)`, 16px, top and bottom. Close enough to one line to read
   as a blank row above the code and another below it.
3. It carried `border: 1px solid var(--hair)` AND
   `background: color-mix(in oklab, var(--plate) 45%, transparent)`. Two ways to
   say where the box ends. `.cmd` (line 1237) did the same, and `.out`
   (line 1219) carried a 2px accent rule and a wash.
4. It set `margin: var(--s3) 0 0`, 24px, so consecutive blocks were 24px apart
   on top of their own padding.

Two more, found while fixing those:

5. `--t-code-s` was `12.5px`. It sizes `.specplate pre.code` (every block on
   `/guide/coming-from`), `.plate.block pre.code` (every block in the guide) and
   the playground editor. Below the 13px floor.
6. On a phone, `.cmd` stacked (`site/public/style.css:2746`, was
   `flex-direction: column`) and put the copy button in a second row 44px tall.
   Every command block on `/install` was 89px for one line of command.

### The fix

| what | before | after |
|---|---|---|
| `pre.code` line height | 1.7 | 1.5 |
| `pre.code` vertical padding | 16px | 12px |
| `pre.code` edges | border + background | background |
| `pre.code` margin | 24px | 16px |
| `pre.code` radius | 0 | 0 (unchanged) |
| `pre.doccode` line height | 1.7 | 1.5 |
| `pre.doccode` padding / margin | 16px / 24px | 12px / 16px |
| `.cmd` edges | border + background | background |
| `.cmd code` padding | 14px | 12px |
| `.out` edges | left rule + background | left rule |
| `.out` line height | 1.7 | 1.5 |
| `.play .editor pre.hl` + `textarea` | `--t-code`/1.7 | `--t-code`/1.5 |
| `.plate.block` blocks | 1.6 | 1.5 |
| `--t-code-s` | 12.5px | 13px |
| `pre.code[data-lang]` header bar | 30px + 16px pad-top | 24px + 12px |
| `.cmd` on a phone | second row, +44px | overlay, +0px |

The wash carries the edge on a program and on a command. The left rule carries
it on a fenced block in doc prose and on tool output, whose wash goes. Nothing
carries two.

The copy button now sits over the right end of the command row at 768px and
below, painting `color-mix(in oklab, var(--plate) 55%, var(--paper))` — the
block's own wash flattened onto the paper — so the command scrolls under it and
does not show through it. It keeps a 68x44 target.

### Measured, before and after

`/docs/std/http`, 1280px. `lines:height in px`.

| block | before | after | change |
|---|---|---|---|
| `pre.code`, 1 line | 57 | 44 | −23% |
| `pre.code`, 5 lines | 149 | 125 | −16% |
| `pre.code`, 10 lines | 263 | 227 | −14% |
| `pre.code`, 14 lines | 355 | 308 | −13% |
| `pre.code`, 22 lines | 539 | 470 | −13% |
| `pre.code`, 69 lines | 1617 | 1421 | −12% |
| `pre.doccode`, 7 lines | 187 | 161 | −14% |
| `.cmd`, 1 line | 51 | 45 | −12% |

`/guide/coming-from`, 1280px. The face got BIGGER here (12.5px to 13px) and the
blocks still shrank.

| block | before | after |
|---|---|---|
| 4 lines | 117 | 102 |
| 5 lines | 138 | 122 |
| 6 lines (x4) | 160 | 141 |
| 7 lines | 181 | 161 |
| 8 lines | 202 | 180 |
| 10 lines | 245 | 219 |

`/guide/values`, 1280px.

| block | before | after |
|---|---|---|
| `pre.code.hl`, 23 lines | 506 | 494 |
| `pre.code.hl`, 25 lines | 546 | 533 |
| `.out`, 3 lines | 97 | 92 |
| `.out`, 5 lines | 139 | 131 |

`/install` at 375px, the copy button:

| | before | after |
|---|---|---|
| `.cmd`, one line | 89 | 45 |
| button | second row, 343x44 | overlay, 68x45 |
| command width | 343 | 343 |

### Against the targets

| target | measured |
|---|---|
| line height at most 1.5 | 1.50 on `pre.code`, `pre.doccode`, `pre.hl`, `.out`, `.cmd code` |
| font at most body, at least 13px | body 17px; `--t-code` 13.5, `--t-code-s` 13, `--t-mono` 13, `--t-cmd` 14 |
| vertical padding at most one line height | 12px against a 20.25px line on `pre.code`, a 19.5px line on `pre.doccode` |
| at most one of border / fill / shadow | one on every block; no shadow anywhere |
| long lines scroll inside, page never scrolls sideways | `docScrollsX: false` on all three pages at 1280 and at 375; every block on `/guide/coming-from` reports `scrollWidth > clientWidth` and scrolls itself |
| the copy button adds no height | `.cmd` is 45px for one line at 375px and at 1280px |

The six-line rule: a six-line example on `/guide/coming-from` is 141px against
163px for six lines of body text. Six lines of `pre.code` at the default size
would be 6 x 20.25 + 24 = 145.5px.

### The same defect elsewhere

`site/public/style.css:1713`, `.demo .pane .out` — the terminal demo's output
pane, at `var(--t-code)/1.7`. It is on `/` and none of the three measured pages
render it, so it was found by the new test rather than by looking. Fixed in the
same commit.

Two near-misses left alone, both deliberate, both named here so the owner can
decide:

- `site/public/style.css:2626`, `.play .stdinbox textarea` — mono at 1.6, with a
  border and a wash. It is an input a reader types into, not a block a reader
  reads, and its height is fixed at 6.5em, so none of the targets bite.
- `site/public/style.css:2638`, `.play .diag` — a diagnostic button at 1.6, with
  a left rule and a wash. A control, not a code block.

`examples/fullstack/public/style.css` and `examples/shelf/public/style.css` were
checked: neither has a `pre` rule of any kind.

### The test

`site/test/codeblocks.test.mjs`, new, five assertions, all five numbers:

```
✔ no code block sets a line taller than 1.5
✔ a code block carries one edge, not three
✔ a code block's vertical padding is no taller than one of its lines
✔ no code block is set below 13px at the default root size
✔ the highlight layer and the textarea over it are declared together
```

The first of those is what found `.demo .pane .out`. The last one exists because
the playground draws its text once, in a `pre` under a transparent `textarea`:
the two boxes must agree to the pixel or the caret walks away from the glyph it
is under. Measured on `/guide/values` after the change:
`hl 13px/19.5px pad 12px/16px | ta 13px/19.5px pad 12px/16px | MATCH=true`.

`site/test/typescale.test.mjs:64` recorded `--t-code-s: 12.5px` and now records
`13px`, with the reason.

## Defect 5a — the light theme flashes white

### Root cause

`site/public/theme.js` was loaded as a classic `<script src>` in the head, from
`site/app/routes/layout.vyx:29` on the consumer front and
`site/app/backstage.vyrn:178` on the backstage.

A classic script blocks rendering, which was the point — the file's own header
said so, and the root attribute was on the element before anything was painted.
But it blocks on a REQUEST. Nothing is painted while that request is in flight,
so on a cold load the reader looks at the browser's own canvas until it
completes. The flash had been moved from after the first paint to before it, not
removed.

The second half: the stylesheet also blocks the first paint, and until it
arrives the browser paints a canvas of its own. A browser with no colour scheme
declared assumes light and paints white. Nothing in the document said otherwise
before the CSS, and the CSS is exactly what has not arrived yet.

### The fix

`withThemeBoot` in `site/export.vyrn` stamps two elements into the head of every
published page on both fronts. It is the outermost stamp in `publishedDocument`,
so nothing the export writes comes before it.

```
<meta name="color-scheme" content="light dark">
<script>try{var r=document.documentElement,t=localStorage.getItem("vyrn.theme");
if(t==="light"||t==="dark")r.setAttribute("data-theme",t);
if(sessionStorage.getItem("vyrn.mast")==="compact")r.classList.add("idecompact");
r.setAttribute("data-js","on")}catch(e){document.documentElement.setAttribute("data-js","on")}</script>
```

Three attributes, all of them wrong to set after a paint: `data-theme` is the
reader's choice, `idecompact` is the masthead height handed over from the last
page, and `data-js` unhides the theme control — set later, the control appears
one frame after the paint and moves the row it is in.

`site/public/theme.js` keeps the toggle, the cycle, the accessible name and the
delegated click listener, and lost exactly those three lines. It is a module on
both fronts now, so it is deferred like every other script on the page.

The boot is spliced after `<meta charset="utf-8">` and not after `<head>`. A
browser reads the encoding out of the first 1024 bytes of a document, `std/ui`
writes the charset after the title rather than first, and five existing stamps
already put it at byte 560. Putting 380 bytes of script in front of it left 55
bytes of margin — one longer description away from a browser guessing the
encoding. Nothing about the theme needs to precede the encoding: the script runs
while the head is parsed, which is before any layout and long before the
stylesheet it has to beat.

### The evidence

Measured in the page, `performance.getEntriesByType("resource")`, Chrome's
`renderBlockingStatus`:

| | baseline (`:8813`) | after (`:8812`) |
|---|---|---|
| `theme.js` | `blocking` | `non-blocking` |
| `style.css` | `blocking` | `blocking` |
| `widgets.js` | `non-blocking` | `non-blocking` |
| `<meta name="color-scheme">` | ABSENT | `light dark` |
| inline script in the head | none | present |

Head byte offsets in the published document, after:

| page | charset | colour scheme | stylesheet |
|---|---|---|---|
| `/docs/std/http` | 600 | 634 | 1025 |
| `/` | 575 | 609 | 1048 |
| `/guide/values` | 612 | 646 | 1055 |

The colour is decided 391 to 439 bytes before the stylesheet is even named, and
the encoding is still well inside the 1024-byte window.

The theme still lands. With `vyrn.theme` set to `dark` and the browser's system
set to LIGHT, a cold load of `/` reports:

```
theme: "dark", systemPrefersDark: false, bodyBG: oklch(0.155 0.005 60)
```

The toggle still cycles, three presses from the same button:

```
dark/Dark/oklch(0.155 0.005 60)
system/System/oklch(0.93 0.024 60)
light/Light/oklch(0.93 0.024 60)
dark/Dark/oklch(0.155 0.005 60)
aria: Theme: Dark. Press to use System.
```

`site/export.vyrn`, the test named `both fronts carry the theme control, and
carry it without a flash`, now asserts: the two head elements follow the charset
and precede the stylesheet; the inline piece reads `vyrn.theme` and writes
`data-theme` and `data-js`; there is no `<script src>` for `theme.js` anywhere in
the document; there IS a module tag for it in the head; and the charset is
inside the first 1024 bytes.

### The same defect elsewhere

- `examples/fullstack/public/style.css:1` and
  `examples/shelf/public/style.css:1` declare `color-scheme: light dark` in CSS,
  which is the same gap in principle — the CSS has to arrive first. Neither app
  stores a theme choice, neither loads a theme script, and both are served by
  `vyrn dev` from localhost. Not the same defect. Not touched.
- `web/*.html`, six browser demos, carry no `color-scheme` meta. They are wasm
  runtime demos, not site pages, and they declare no theme. Not touched.

## Defect 5b — the light theme is not pastel

### Root cause

`site/public/style.css`, the `:root` palette. `--n4`, the paper, was
`oklch(0.955 0.002 60)` — a near-white with a hint of warmth in it, which is a
white. The comment above it recorded the tenth-round note that asked for "less
flashing light" and the step from 0.975 to 0.955 that answered it. The same
complaint came back, so the first answer did not go far enough.

### The fix

Tokens only. Seven of them.

| token | before | after | sRGB after |
|---|---|---|---|
| `--n4` paper | `oklch(0.955 0.002 60)` | `oklch(0.93 0.024 60)` | `rgb(245, 229, 216)` |
| `--n3` plate | `oklch(0.905 0.003 60)` | `oklch(0.88 0.03 60)` | `rgb(238, 221, 207)` |
| `--n0` ink | `oklch(0.21 0.004 60)` | `oklch(0.2 0.006 60)` | `rgb(24, 21, 19)` |
| `--n1` muted | `oklch(0.42 0.004 60)` | `oklch(0.4 0.006 60)` | |
| `--n2` meta | `oklch(0.505 0.004 60)` | `oklch(0.475 0.006 60)` | |
| `--danger` | `oklch(0.5568 0.2078 25.33)` | `oklch(0.515 0.2078 25.33)` | |
| `--amber` | `oklch(0.52 0.1447 70.08)` | `oklch(0.487 0.1447 70.08)` | |

The paper stays on the ramp's own warm hue, 60, which the sheet's header picked
deliberately. It moves 0.025 in lightness and twelvefold in chroma, which is the
difference between a white with a hint in it and a cream.

The other six moved because that one did. Contrast is a ratio between two
colours, so darkening the ground takes a step out of every pair in the sheet,
and the three tightest pairs stood 0.10, 0.13 and 0.32 above the floor. Read as
`--paper` at 0.93 with the old ink and the old danger, five pairs failed.

The dark palette is untouched. It redefines every token this change touched,
which is why it could be.

### Contrast, computed

`site/test/contrast.test.mjs` parses the two token blocks, resolves `var()`,
`oklch()` and `color-mix(in oklab, …)` the way a browser does, converts OKLab to
linear sRGB with Ottosson's matrices, clamps to gamut, and takes the WCAG 2.1
relative-luminance ratio. Run it with `VYRN_CONTRAST_TABLE=1` to print the table.

The bar in this branch is higher than the one it inherited: body text is held at
7:1, not 4.5:1. Three pairs are new — the copy button's word over the command it
covers, the focus ring against the control it rings, and the filled call to
action's own label.

**Light.** Every pair, after.

| pair | ratio | needs |
|---|---|---|
| body text | 14.68:1 | 7:1 |
| body text on a plate | 13.69:1 | 7:1 |
| secondary prose (`.lede`, `.note`, `.notice`) | 7.47:1 | 4.5:1 |
| secondary prose on a plate | 6.97:1 | 4.5:1 |
| meta text (`.modlist .count`, `.rail .n`, chart axis) | 5.42:1 | 4.5:1 |
| meta text on a plate (line numbers, `.lines .cl.head`) | 5.06:1 | 4.5:1 |
| the copy button's word, over the command it copies | 4.98:1 | 4.5:1 |
| a link, and every accented heading | 10.49:1 | 4.5:1 |
| a link on a plate | 9.78:1 | 4.5:1 |
| failure (`.diag.error`, `.pill`, a trap) | 5.09:1 | 4.5:1 |
| a pre-release (`.pill.warn`, `.diag.warning`) | 5.27:1 | 4.5:1 |
| inline code, in its own wash | 12.49:1 | 4.5:1 |
| a keyword | 5.16:1 | 4.5:1 |
| a string | 5.58:1 | 4.5:1 |
| a comment | 6.97:1 | 4.5:1 |
| a type | 9.78:1 | 4.5:1 |
| a number | 4.92:1 | 4.5:1 |
| the focus ring | 10.49:1 | 3:1 |
| the focus ring on a plate | 9.78:1 | 3:1 |
| the focus ring against the control it rings | 9.59:1 | 3:1 |
| the filled call to action's own label | 10.49:1 | 4.5:1 |
| an ownership lane | 3.73:1 | 3:1 |
| the other ownership lane | 5.27:1 | 3:1 |
| the C line on the radar | 6.97:1 | 3:1 |
| the Rust line | 4.92:1 | 3:1 |
| the node line | 5.58:1 | 3:1 |
| the two Vyrn lines | 9.78:1 | 3:1 |
| the implemented tone | 4.92:1 | 3:1 |
| the partly-built tone | 5.16:1 | 3:1 |
| the open tone (draft, proposed) | 5.58:1 | 3:1 |
| the neutral tone (superseded, other) | 5.06:1 | 3:1 |
| a legend label | 6.97:1 | 4.5:1 |
| a legend label (Rust) | 4.92:1 | 4.5:1 |
| a legend label (node) | 5.58:1 | 4.5:1 |
| a legend label (Vyrn) | 9.78:1 | 4.5:1 |

The tightest pair in the light sheet is an ownership lane at 3.73:1, and it is a
graphic that carries an argument, so its bar is 3:1. The tightest TEXT pair is
the copy button's word at 4.98:1.

**Light, the pairs that moved most.** Before, at the old bar:

| pair | before | after |
|---|---|---|
| body text | 15.54:1 | 14.68:1 |
| failure | 4.60:1 | 5.09:1 |
| a pre-release | 4.96:1 | 5.27:1 |
| meta text on a plate | 4.82:1 | 5.06:1 |
| a number | 4.63:1 | 4.92:1 |
| a keyword | 5.59:1 | 5.16:1 |
| an ownership lane | 4.03:1 | 3.73:1 |

Body text gives up 0.86 of its 15.54 and stays more than twice the 7:1 bar. The
four pairs that were closest to failing all gained, because `--danger`,
`--amber` and `--n2` were darkened by more than the paper was.

**Dark.** Untouched, and re-measured against the raised bar to prove it clears
it.

| pair | ratio | needs |
|---|---|---|
| body text | 17.92:1 | 7:1 |
| body text on a plate | 16.83:1 | 7:1 |
| secondary prose | 7.87:1 | 4.5:1 |
| meta text on a plate | 5.04:1 | 4.5:1 |
| the copy button's word | 4.95:1 | 4.5:1 |
| failure | 7.32:1 | 4.5:1 |
| a pre-release | 10.20:1 | 4.5:1 |
| a keyword | 7.76:1 | 4.5:1 |
| a string | 9.54:1 | 4.5:1 |
| a number | 9.58:1 | 4.5:1 |
| the focus ring | 12.66:1 | 3:1 |
| the focus ring against the control it rings | 12.02:1 | 3:1 |
| an ownership lane | 8.26:1 | 3:1 |
| the neutral tone | 5.04:1 | 3:1 |

Tightest in dark: the copy button's word at 4.95:1 and the neutral tone at
5.04:1.

Confirmed live in the browser, light mode, `/`:

```
bodyBG oklch(0.93 0.024 60)   --paper oklch(0.93 0.024 60)
bodyColor oklch(0.2 0.006 60) --plate oklch(0.88 0.03 60)
--danger oklch(0.515 0.2078 25.33)  --amber oklch(0.487 0.1447 70.08)
```

## Gates

Run after each commit, from `N:/wt-css`.

```
<vyrn> run site/export.vyrn out            exported 80 route(s) and 14 asset(s)
<vyrn> fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn    clean
node --test "site/test/*.test.mjs"         34 pass, 6 fail
<vyrn> test <every module under site/app and site/guide>   210 passed, 0 failed
```

The six node failures are the same six that fail on the branch point, measured
by stashing the change and running them again: `the export is there to check`,
the three `every page, asset and fragment resolves under …`, `every module page
carries an import line`, and `every import line compiles`. They want
`out/play.wasm`, which `.github/workflows/site.yml` builds with `cargo` in a
separate step, and a compiler on a path this session did not set. Baseline
`29 pass / 6 fail`; after `34 pass / 6 fail` — five new passing tests, no new
failures.

`vyrn test` module floors, read off the source rather than off a total:
`site/export.vyrn` 31, `site/app/markdown.vyrn` 16, `site/app/bench.vyrn` 19,
`site/app/chart.vyrn` 14, `site/app/code.vyrn` 11, `site/app/guide.vyrn` 10,
`site/app/docs.vyrn` 8, `site/app/corpus.vyrn` 8, `site/app/snippets.vyrn` 8,
`site/app/apidoc.vyrn` 7, `site/app/packages.vyrn` 7, `site/app/pagemd.vyrn` 7,
`site/app/hl.vyrn` 6, `site/app/stdgraph.vyrn` 6, `site/app/nav.vyrn` 5,
`site/app/docshell.vyrn` 5, `site/app/repo.vyrn` 5, `site/app/backstage.vyrn` 4,
`site/app/editors.vyrn` 4, `site/app/facts.vyrn` 4, `site/app/history.vyrn` 4,
`site/app/search.vyrn` 4, `site/app/demo.vyrn` 3, `site/app/feed.vyrn` 3,
`site/app/meta.vyrn` 3, `site/app/deps.vyrn` 2, `site/app/guidecode.vyrn` 2,
`site/guide/tests.vyrn` 2, `site/app/github.vyrn` 1, `site/app/play.vyrn` 1.
`site/app/demohl.vyrn`, `site/app/icons.vyrn` and 22 of the 24 guide programs
carry no tests and print none, which is what they did before.

`site/app/hl.vyrn` is untouched: what is highlighted did not change, only how it
is spaced. Its six tests pass.
