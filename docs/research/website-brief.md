# Vyrn website — design brief

- **Status:** draft for review. No site code exists yet.
- **Audience:** the build wave that executes this brief.
- **Sources read:** `README.md`, `ROADMAP.md`, `web/`, `std/ui.vyrn`, `std/vyx.vyrn`,
  `std/tw.vyrn`, `std/scan.vyrn`, `rfcs/RFC-0069-universal-pages.md`,
  `rfcs/RFC-0063-ci-benchmarks.md`, `.github/workflows/ci.yml`, `compiler/Cargo.toml`,
  `examples/bin/`, `editor/vscode/icons/vyrn.svg`. External references: a
  design playbook, the Bun engineering-blog widget page, the Cursor CLI
  ASCII hero, and four screenshots of the Bun widgets.

---

## 1. What the site must prove

The site has one job. A visitor must believe, within thirty seconds, that Vyrn is
real and that it runs.

Three claims carry that weight. Each claim has evidence in the repository today.

1. **One program, three backends, identical bytes.** The interpreter, the native
   binary and the wasm module produce the same stdout, the same stderr and the
   same exit code, across the whole example corpus, including every trap.
2. **Types carry rules, not just shapes.** A validated type rejects a bad
   constant at compile time and generates the runtime check when it cannot.
3. **No garbage collector, and no ownership puzzle.** Ownership is declared.
   Memory is reclaimed at known points.

Every widget on the site must serve one of those three claims, or serve the
install path. A widget that serves neither is decoration. Cut it.

**The bar for a widget** comes from the Bun blog page: a widget replays *real
data*. The commit heatmap plots 6,502 real commits. The error burndown replays
1,610 real commits. The CI chart plots 135 real builds. Each widget carries a
caption that states where the data came from. We adopt that rule without change:

> **No widget renders a number the project cannot produce on demand.**

No fabricated benchmarks. No round marketing figures. If a number is not in a CI
artifact, a committed JSON file, or computed live in the page, it does not ship.

---

## 2. Decision: tech stack

### Recommendation — dogfood the site in Vyrn

Build the site with `std/ui` pages, `.vyx` components, `std/tw` for the
stylesheet, and a static export step. Use JavaScript only where the browser API
surface demands it (canvas painting, the WASI shim, the GitHub fetch).

**Why.** The project's dogfoods have each found real bugs: `examples/shelf` found
two loader bugs; `examples/bin` found a silent push-on-global-record-field bug;
`examples/vlog` produced RFC-0046. A public website is the largest UI-layer load
the project has ever put on itself. It will find the next bug, and finding it is
worth more than the week it costs. The site is also the argument: a page that
says "Vyrn builds web apps" while running on Astro loses the argument in its own
`view-source`.

The second reason is stronger than the culture reason. Almost every piece is
already built:

| Need | Status today |
| --- | --- |
| Components | `.vyx` single-file components, `std/vyx` (4621 lines), shipped |
| Routing | `std/ui` `pages(dir)`, file-based, typed params, shipped |
| Layouts, error pages, `head{}`, `load()` | RFC-0039/0041/0071, shipped |
| Client-side navigation | RFC-0069 universal pages, shipped, browser-verified |
| Styling | `std/tw` theme-derived classes, compile-checked, plus a plain CSS file |
| DOM runtime | `vyrn-dom.js`, keyed differ, zero dependencies |
| Wasm in a page | `wasi-min.js`, zero dependencies, shipped |
| JS interop, both directions | RFC-0012 `extern` / `export extern`, shipped |
| Content at build time | `gen fn` reads directories and files at compile time |

### The gaps, named

The build wave must close these. Each is sized honestly.

**G1 — Static export (small, ~1 day).** `pages()` synthesizes
`route(req: Request) -> Response`. GitHub Pages serves files, not a process.
But `Request` is an ordinary record: `examples/pagesdemo.vyrn` already builds one
by hand. So the export step is a Vyrn program that walks the generated route
table, calls `route()` once per path, and writes the body with `writeFile`
(RFC-0014). It must also write the RFC-0069 navigation payload per route — the
same call with the `?__vyrn=data` marker — so soft navigation keeps working with
no server. Roughly 60 lines. No compiler change. This gap is the one that would
have blocked the whole decision, and it is already open.

**G2 — SVG through the DOM runtime (small, hours).** Charts are SVG. `std/html`
builds elements by name, so a `<svg>` tree is expressible. `vyrn-dom.js` almost
certainly calls `document.createElement`, which produces an inert HTMLUnknownElement
for SVG tags. Fix: switch to `createElementNS` inside an `<svg>` subtree, and use
`setAttribute` rather than property assignment for SVG attributes. Ten lines,
plus a test. **Verify this before the build wave starts**; it is the first thing
to check, and it is a genuine dogfood find.

**G3 — Syntax highlighting (small, ~1 day, Vyrn only).** RFC-0054 exposes `lex()`
to Vyrn, and `std/scan` is a comment- and string-aware cursor. A Vyrn highlighter
is a `gen fn` that lexes each snippet at compile time and emits spans — zero
runtime cost, and correct by construction because it is the compiler's own lexer.
Rust, TypeScript and Go snippets on the comparison page do **not** get a real
parser. Give them keyword-set colouring from a short word list, and accept that
it is approximate. Do not write three more lexers.

**G4 — Markdown (medium, and deferred).** There is no `std/md`. The guide book
and the `docs/api/*.md` browser both need one. **Do not build it for v1.** v1
links to the repository for reference docs and ships one hand-written guide page
in `.vyx`. `std/md` arrives with the guide book in v2 — a `gen fn` over
`std/scan` covering headings, paragraphs, lists, fenced code, links, inline
emphasis and tables. Budget it at 600–900 lines of Vyrn when its turn comes.

**G5 — Canvas (not a gap, a shape).** Vyrn cannot call canvas APIs and should
not try. The correct division: **Vyrn computes, JavaScript paints.** The ASCII
hero exports one function that returns a frame's glyph rows; the host calls
`fillText`. That is one `extern` boundary crossing per frame, not thousands.

**G6 — Client bundle size (measure first).** `web/client.wasm` is 322 KB, built
through the old clang path. RFC-0077 emits wasm directly and much smaller
(`fib.wasm`: 1.4 KB against 277 KB). Confirm which path `vyrn dev` uses for the
client, then set a budget: **150 KB compressed for the page bundle**. If the
site's client module misses it, that is a compiler finding worth having, and the
landing page can ship prerendered with no client module at all while it is fixed.

**G7 — Trig (tiny).** `std/math` has `min`, `max`, `abs`, `clamp` and nothing
else. The hero field needs a sine. Write a minimax polynomial sine in Vyrn, about
ten lines, and make it its own proof: `-ffp-contract=off` (RFC-0083) means the
interpreter, the native binary and wasm agree bit for bit on the hero's floats.

### The alternative, weighed

A mainstream static stack (Astro or Nuxt) with Vyrn only in the playground would
ship faster in week one and would carry no risk of a UI-layer bug blocking a
release. It costs the argument, and it costs the bug reports. Given that G1 and
G2 are the only real blockers and both are small, the speed advantage is roughly
a week. That is not enough.

**Fallback, if it goes wrong:** the fallback is *scope*, not *stack*. If the
UI layer stalls, ship fewer pages, not a different framework. A single
prerendered landing page with the wasm hero and the install command is a
legitimate v1.

### JavaScript budget

The site is allowed exactly these JavaScript files, all already written or tiny,
all zero-dependency:

- `wasi-min.js` — existing.
- `vyrn-dom.js` — existing, plus the G2 namespace fix.
- `vyrn-nav.js` — existing, plus a static-payload path for G1.
- `hero.js` — new, about 150 lines, the canvas painter.
- `widgets.js` — new, about 60 lines: the shared harness of section 4.1
  (`onView`, `ease`, the count-up, the replay button). Every widget's own logic
  is a `render(p)` function of ten to thirty lines registered against it.
- `fresh.js` — new, about 40 lines, the GitHub release refresh (section 5).

No framework. No CDN. No build tool for the JavaScript. No charting library —
the geometry comes from Vyrn (section 4.3). If a seventh file appears, something
went wrong.

---

## 3. Information architecture

Routes, as `app/routes/` in the `examples/bin` layout:

| Route | Purpose | v |
| --- | --- | --- |
| `/` | Hero, ASCII canvas, the three claims, install command, one live snippet | v1 |
| `/philosophy` | Intent over mechanism. Predictability. Validated types. What Vyrn refuses | v1 |
| `/compare` | Honest tables against Rust, TypeScript, Go, plus live benchmark charts | v1 |
| `/install` | Quickstart per platform, real version, stable and pre-release channels | v1 |
| `/releases` | Release feed including pre-releases, with the RFC arc behind each one | v1 |
| `/play` | In-page playground | v1.1 |
| `/docs` | Reference entry. v1 is one curated page plus links out | v1 |
| `/guide/*` | The guide book | v2 socket |
| `/explorer` | Package and dependency explorer | v2 socket |

**The v2 sockets are structural, not decorative.** Reserve `/guide` and
`/explorer` in the route table from day one. The navigation renders a link only
when the route exists, so adding the guide book later is a directory drop, not a
navigation rewrite. Do not build placeholder pages that say "coming soon".

**Page shapes.**

- `/` is a vertical sequence with no sidebar. Hero, then one claim per band, each
  band anchored by exactly one widget. Four bands maximum.
- `/philosophy` is prose with a 65ch measure, punctuated by two widgets. It is
  the one page allowed to be quiet. It must also carry the **non-goals** — no
  garbage collector, no `async`/`await`, no macros, no higher-kinded types, no
  inheritance. What a language refuses is the sharpest statement of what it
  believes.
- `/compare` is the densest page: a fixed left rail of comparison axes, a
  scrolling main column. This is where high density is earned, because every row
  carries differentiating information.
- `/releases` is a feed. Newest first. Pre-releases marked, not hidden.

---

## 4. Widget inventory

Every widget shares one chassis, taken from the Bun page. The screenshots show it
clearly and it is worth copying exactly, because it makes provenance a structural
part of the design rather than a footnote:

```
┌───────────────────────────────────────────────────────┐
│ [PILL]  EYEBROW · IN MONO CAPS, WIDE TRACKING         │
│                                                       │
│ 6,502 commits          ← one huge number, the headline│
│                            ↻ replay / ▶ next  (control)│
│                                                       │
│ ……… the visualization ………                            │
│                                                       │
├───────────────────────────────────────────────────────┤
│ caption: what the data is, where it came from, what   │
│ the reader should notice. Muted. Always present.      │
└───────────────────────────────────────────────────────┘
```

Rules for the chassis: one headline number per widget, set large in mono with
`tabular-nums`; one primary control, labelled with a verb; a caption that names
the data source in plain words. The caption is mandatory. A widget without a
provenance caption does not ship.

### 4.1 The shared harness

The Bun page uses no framework and no bundler. Its five widgets share about 760
bytes of prelude. Copy that shape exactly; it is the whole engineering story.

- **`onView(root, cb)`** — one `IntersectionObserver`, thresholds
  `[0.1, 0.3, 0.5, 0.75]`, fires **once** then disconnects. It triggers when the
  ratio reaches 0.5 **or** when the visible height reaches half the viewport, so
  a widget taller than the screen still fires. Under
  `prefers-reduced-motion: reduce` it returns immediately and never animates.
- **Autoplay once on view, plus one replay button.** No pause. No scrub, except
  where the widget is a scrubber by design (W6).
- **`ease(t) = 1 - (1-t)³`** and a `requestAnimationFrame` count-up for every
  headline number.
- **Reduced motion renders the final frame directly.** Not a blank widget, not a
  frozen first frame. The end state.
- **No layout shift:** pin the replay button's width from its bounding box
  *before* changing its label; pin the stage's `min-height` from the tallest
  chapter; set `overflow-anchor: none` on the widget section so the page does not
  jump while content animates.
- **`<noscript>` fallback** for any widget whose content is meaningful without
  motion: a `<style>` block that reveals every step at once.

### 4.2 Two animation shapes — pick the first

**Shape A, "one scalar" (use this).** Pre-render the geometry. Run one
`requestAnimationFrame` loop that maps elapsed time to a progress value `p`, and
write `render(p)` as a **pure function** of `p` that re-renders everything: bar
opacities, a playhead position, counters read from a cumulative array, and a
ticker found by binary search. Nothing is stateful, so a scrubber and a replay
are the same code. The Bun CI-race and git-log widgets are both this shape, and
it is by far the cleanest of the five.

**Shape B, "staggered workers".** Independent `setTimeout` loops move absolutely
positioned elements with `transform`, anchored by `getBoundingClientRect` so the
animation survives responsive reflow. Richer, and roughly ten times the code.
Use it only for the memory-model replay (W6), if at all.

### 4.3 The data rule — and why it fits our build

Bun inlines every dataset as `JSON.parse("…")` of a string literal, in the page,
with the SVG geometry pre-computed on the server: fills, positions and `<title>`
tooltips are all in the markup before any script runs. There is no fetch and no
client-side chart library.

That maps onto our stack exactly, and it is the architectural rule for every
chart on this site:

> **Vyrn computes the geometry at build time. JavaScript animates one scalar.**

The static export (G1) already runs Vyrn at build time with the data files from
section 5 in hand. So a `.vyx` component reads `bench.json`, computes every bar's
position, height, fill and tooltip, and emits the `<svg>` — prerendered, visible
with JavaScript disabled, and correct before a single frame is drawn. The
harness then only fades, sweeps or counts. This is why G2 (SVG through the DOM
runtime) is the first thing to verify: six of the seven v1 widgets depend on it.

Accessibility follows from the same shape: `role="img"` and `aria-label` on each
data SVG, and native `<title>` elements for tooltips. No JavaScript tooltip
layer.

### v1 widgets

**W1 — ASCII hero.** *Data:* a Vyrn wasm module that computes a luminance field
per frame. *Interaction:* it runs; the pointer warps the field near the cursor;
one static frame under reduced motion. *Technique:* section 6.

**W2 — Parity visualizer.** The signature widget. *Data:* the parity harness
output, committed as JSON per release (example name, three stdout digests, three
exit codes), plus a live wasm run in the page for the third column. *Interaction:*
pick an example from a list; press Run; three columns fill — `interp`, `native`,
`wasm` — and the diff counter lands on `0 bytes`. A second control, "break it",
flips one byte of the expected output and shows the harness turning red, so the
reader sees that the check is real and not a picture of a green tick.
*Technique:* three text panes, a monospace diff strip, the wasm column executed
by `wasi-min.js` on the spot. Hold for 500 ms before the `0 bytes` lands. The
pause is the widget.

**W3 — Install command.** *Data:* the release feed (section 5). *Interaction:*
platform auto-detected with a manual override; a copy button; a channel toggle
between stable and pre-release that rewrites the command and the version.
*Technique:* plain DOM. It is the most important widget on the site. Put it above
the fold.

**W4 — Benchmark charts.** *Data:* `bench/baseline.json` and the `bench-json` CI
artifact (RFC-0063). Note that `bench/baseline.json` is a **placeholder today** —
seeding it from a CI run is a prerequisite for this widget, and the file itself
documents how. *Interaction:* hover for exact values; toggle between backends;
toggle a log scale. *Technique:* SVG bars built in `.vyx`. Show `minNs` as the
bar and `medianNs` as a tick, because the RFC names `min` as the stable
statistic. Print the sample count. State that the numbers come from a shared CI
runner and are noisy — that honesty is worth more than a bigger bar.

**W5 — Comparison toggles.** *Data:* a committed `site/compare/` corpus — the
same task written in Vyrn, Rust, TypeScript and Go — measured by a CI job so the
numbers regenerate. *Interaction:* tabs switch language; the metric strip
(lines, binary size, peak memory, wall time) animates between values rather than
cutting. *Technique:* prerendered highlighted code, one metric strip, CSS
transforms only.

**W6 — Memory-model replay.** *Data:* the real allocation counts from
`examples/membench.vyrn` and the RFC-0091 arc results (the P1 scenario went from
12.2 GB to 0.121 s). *Interaction:* a playhead sweeps a timeline, and the reader
can drag it. At any point the panel shows live allocations, the region depth, and
which rule freed what. *Technique:* Shape A. Vyrn prerenders one `<rect>` per
allocation lifetime; `render(p)` sets opacity by whether each lifetime has
started, moves the playhead with two attribute writes, and reads the counters
from a cumulative array. Because it is a pure function of `p`, the scrubber and
the replay button are the same three lines.

**W7 — Release feed and cadence.** *Data:* GitHub Releases and the commit log,
baked at build time (section 5). *Interaction:* hover a cell for its bucket;
click a release to expand its RFC list. *Technique:* the Bun heatmap, which needs
no `requestAnimationFrame` at all. Vyrn emits one `<svg>` with one
`<rect rx="4">` per day-hour bucket, fill already computed, each carrying a
native `<title>` ("12 August, 14:00–15:00 — 6 commits"). Every cell starts at
`fill-opacity="0"` with `transition: fill-opacity .45s ease-out`; one
`setTimeout` per cell, delayed by `hour / 24 × 4000` ms, produces a four-second
left-to-right sweep while the counter accumulates. Under reduced motion the
script returns early and the cells are simply already drawn. Reuse the parity
widget's colour ramp so the site reads as one system.

### v1.1 widgets

**W8 — Validated-types live demo.** *Data:* the compiler itself, compiled to
wasm (section 7). *Interaction:* edit the `where` clause or the value; the panel
below shows either the compile error, with the same wording the CLI prints, or
the note that the check was erased because the value was proven valid. This is
the single best demonstration the language has. It is v1.1 only because it
depends on the in-browser compiler.

**W9 — Playground.** Edit, run under the interpreter, build to wasm, see the
module size, run the module. Share by URL fragment. Same dependency as W8.

**W10 — Wasm size counter.** Compile `fib.vyrn` in the page and print the byte
count against the old clang-path figure. Live, not quoted.

**W11 — Generator explorer.** *Data:* `vyrn emit-gen` output, precomputed for a
set of inputs. *Interaction:* change the input `theme.json` or locale file and
watch the generated Vyrn module change, side by side. It shows that `gen fn`
generators are ordinary compile-time Vyrn, not compiler magic.

**W12 — Capability explainer.** Hover a parameter marked `read`, `modify`,
`consume` or `share`; the call sites in the snippet light up and the ones that
would fail dim with the compiler's own message. Static data, precomputed.

### Explicitly not in scope

No testimonial carousel. No logo wall. No "trusted by". No counter that ticks up
on scroll for a number that is not a measurement. No newsletter modal.

---

## 5. Data freshness

**Recommendation: bake at build time, upgrade at runtime.** The baked data is
what renders. A single client fetch may replace a stale version string. The page
never waits for the network to show a version.

### The pipeline

**Step 1 — releases exist.** There are no git tags in the repository today, and
no release workflow. This is a prerequisite, not a detail. Add
`.github/workflows/release.yml`: on a pushed tag `v*`, build the `vyrn` binary
for Linux, macOS and Windows, and publish a GitHub Release. A tag with a suffix
(`v0.2.0-rc.1`) publishes with `prerelease: true`. Nothing else on the site's
freshness path works until this exists.

**Step 2 — the site workflow.** Add `.github/workflows/site.yml`, triggered by:

- `release: [published, prereleased, edited]`
- `workflow_run` — CI completing successfully on `main`
- `workflow_dispatch`
- `schedule` — one daily run, as a safety net for a missed trigger

It writes `site/data/*.json` from facts it gathers itself, then prerenders and
deploys:

| File | Contents | Source |
| --- | --- | --- |
| `releases.json` | Latest stable, latest pre-release, last 20 entries with tag, date, notes, asset URLs | `gh api /repos/vyrn-lang/vyrn/releases` |
| `stats.json` | Test count, example count, RFC count, backend count, divergence count | `cargo test` output and `ls` counts from the same run |
| `bench.json` | The `bench-json` artifact from the informational bench job | `gh run download` |
| `parity.json` | Per-example digests and exit codes for the three backends | The parity harness, `--ignored`, in the site job |
| `cadence.json` | Commits per day-hour bucket, and RFC merge dates | `git log --pretty` |

Every file carries `generatedAt` and the commit SHA it came from. The site prints
that SHA in the footer. A reader can check it.

**Step 3 — the client upgrade.** `fresh.js`, about 40 lines: on load, read
`localStorage` for a cached release payload younger than one hour. If there is
none, fetch `https://api.github.com/repos/vyrn-lang/vyrn/releases?per_page=10`
once, unauthenticated. If a tag is newer than the baked one, swap the version
string in the install widget and show one line: "v0.3.0 released — this page was
built at v0.2.9". Never block rendering. Never retry. On any failure, stay with
the baked value silently.

Constraints that shape this: the unauthenticated GitHub API allows 60 requests
per hour per IP, so one call per visit with an hour of caching is safe; no token
may ever appear in the page; and pre-releases only appear in the API listing
endpoint, which is why the code uses `/releases` rather than `/releases/latest`.

**Step 4 — the channel toggle.** The install widget's pre-release switch is
purely client-side. Both versions are baked. No second fetch.

---

## 6. The ASCII hero

The reference implementation on the Cursor CLI page is a looping video
re-rendered as text on a 2D canvas, at 20 frames per second. It is not a shader
and not a `<pre>`. **We reproduce the rendering technique and replace the source.
Our field comes from a Vyrn wasm module.** That is the point: the hero is a
program in the language the page is selling.

### The loop, per frame

1. The host calls the module's exported `frame(t: Float64) -> String`. The module
   returns `cols × rows` bytes, one glyph index per cell.
2. The host maps each cell to a glyph and paints it with `fillText` at
   `(col × cellW, row × cellH)`, `textBaseline = "top"`.
3. Brightness rides on **alpha**, not on the glyph alone. Precompute a 256-entry
   table of `rgba(...)` strings once per theme. This removes about a hundred
   thousand string concatenations per second.

### Constants to reproduce, not copy

- **Grid:** `cols = clamp(20, min(160, floor(w / 7)))`,
  `rows = clamp(15, min(120, floor(h / 8.08)))`. The `8.08` is a measured
  monospace line box. A round number shears the grid.
- **Character aspect ratio:** measure it, do not assume 0.6. Set a 100px font,
  `measureText("M")`, then
  `charAR = width / (actualBoundingBoxAscent + actualBoundingBoxDescent)`, clamped
  to `[0.3, 2]`. Divide the field's fit by `charAR` or the picture stretches
  vertically.
- **Device pixel ratio:** `max(1, floor(devicePixelRatio))` — **integer only**.
  A fractional ratio blurs the glyph grid. Set the backing store to `w×dpr`,
  pin the CSS size in pixels, `setTransform(dpr,0,0,dpr,0,0)`, and set
  `imageSmoothingEnabled = false`.
- **Frame gate:** `requestAnimationFrame` with a manual
  `if (now - last < interval) return`, `interval = 50` (or 100 on Safari). Never
  `setInterval`.
- **Ramp:** the Cursor ramp spells its own product name in the mid-tones:
  `".:+=CURSOR#$&"`. Ours does the same with `VYRN`: **`".:+=VYRN#$&"`** — eleven
  glyphs, dark to light. Verify the mid-tone band actually reads as the word at
  the shipped cell size before accepting it.
- **Zero is transparent.** A cell whose luminance is exactly 0 draws nothing. It
  is not the first ramp glyph. This is what keeps the background clean.
- **Auto-levels:** rescale each frame between its own non-zero minimum and
  maximum. Without this the field flattens as it drifts.

### The field, in Vyrn

Two rotating plane waves, which need no noise library:

```
a = 0.6·sin(10·(u·cos(0.7t) + v·sin(0.5t)) + t)
  + 0.4·cos(10·(u·sin(0.4t) − v·cos(0.6t)) − 0.5t)
lum = clamp(0, 255, floor(255 · (0.5 + 0.5·a)))
```

`std/math` has no `sin`. Write a minimax polynomial sine in Vyrn — roughly ten
lines — and let that be part of the story: the hero's floating point is
bit-identical across all three backends because RFC-0083 forbids the FMA
contraction that would change it.

Add one interaction the reference does not have: a pointer term that lifts
luminance within a radius of the cursor, decaying over about 400 ms after the
pointer leaves. It must degrade to nothing on touch devices.

### Failure and accessibility

- **Reduced motion:** the reference returns nothing at all. We do better — paint
  **one static frame** and stop. A blank hero looks broken.
- **Off screen:** `IntersectionObserver` at `threshold: 0.1`, plus
  `visibilitychange`. Paint one frame, then stop the loop.
- **Resize:** `ResizeObserver`. Only assign canvas dimensions when they actually
  change; assigning clears and reallocates the buffer.
- **Circuit breaker:** three exceptions within three seconds swaps in a static
  text block permanently. If the wasm module fails to load, the same JavaScript
  evaluates the same two-wave formula directly, so the hero always paints
  something.
- The hero is decorative. Give the canvas `aria-hidden="true"` and put the real
  headline in text beside it.

### Cost

At 160×120 cells and 20 fps: about 19,200 `fillText` calls per frame, roughly
2–4 ms, plus the module call. Budget 5 ms per frame — about ten per cent of one
core. Use canvas 2D. WebGL buys nothing at this size and costs the whole
readback path. Two knobs if it is slow: lower the column cap, or raise the
interval.

---

## 7. The in-browser compiler

This deserves its own decision because it unlocks W8, W9 and W10, and because the
repository makes it unusually cheap.

`vyrn-frontend` — lexer, parser, checker and the tree-walking interpreter — has
**zero dependencies**. `vyrn-codegen` has exactly one, `wasm-encoder`, which is
itself pure Rust. Neither needs LLVM. A `wasm32-unknown-unknown` build of both is
plausible, and it would give the page a real compiler: type-check live, run under
the reference interpreter, **and** compile to wasm and run the result — the
parity claim demonstrated on the visitor's own machine with the visitor's own
code.

Known obstacles, sized:

- `std::fs` appears throughout the loader for module resolution. `ModuleResolver`
  is already a trait in `loader.rs`, and the LSP already implements a read-only
  variant. A resolver over an embedded `std/` map closes it.
- `std::thread` backs `spawn`/`join` in the interpreter. `wasm32-unknown-unknown`
  has no threads. Either run spawned tasks sequentially in the browser build —
  legitimate, since the language *proves* spawned functions are isolated and the
  result is schedule-independent — or refuse the concurrency examples in the
  playground with a clear message.
- `std::time` in the bench path and `std::process::exit` in the CLI path. Neither
  belongs in the browser build; gate them out.

**Recommendation:** treat this as a separate, parallel build, not a blocker. v1
ships prebuilt `.wasm` snippets, which already run today through `wasi-min.js`.
The playground and the live compiler land in v1.1 when the port is green. Size
the bundle before committing to shipping it on the landing page; if the compiler
module is large, load it only on `/play` and on demand.

---

## 8. Design language

Derived from the design playbook and the two reference pages.

### 8.0 Process gate — mandatory

The skill's rule is absolute and applies here with no exemption:

> Any task that produces new visual design — including tasks with a named style
> or a supplied brand — must first present **three differentiated directions**
> with real drafts, and stop for the operator to choose.

So the build wave's first deliverable is **three rendered hero directions**, not
a site. Rules that go with it:

- Three independent drafts, produced in parallel without cross-reading, each a
  complete single-file HTML plus a screenshot.
- The three must differ **structurally**. Three colour schemes over one skeleton
  is not three directions and gets caught immediately.
- Stop after showing them. Wait for the choice. Record it in
  `docs/research/direction-approved.md` before any site code is written.
- No console errors in any draft. Check at 1920×1080, 1440×900, 768×1024 and
  375×667 before showing.

### 8.1 Colour

**Sample, converge, argue** — never invent a palette.

*Sample.* The legal source is the project's own brand asset,
`editor/vscode/icons/vyrn.svg`. Its four values, eyedropped:

```
#123258   deep navy    (the outer stroke)
#1c7f9c   teal
#22b8d4   cyan
#4fe3f2   bright cyan  (the convergence point)
```

*Converge.* Convert those to `oklch()` — convert them properly, do not guess the
values — and keep **two chromatic colours plus one neutral ramp**:

- **Accent:** the cyan family. Chroma budget by use: large backgrounds
  `0.01–0.04`, brand and emphasis `0.08–0.15`, small accents such as links and
  buttons `0.15–0.22`. Never above `0.25` full-bleed.
- **Second chromatic:** the diagnostic red the compiler already prints for
  `error:`. It must sit at least 60° of hue from the cyan, which it does. It is
  reserved for failure states, the "break it" control, and the burndown widget.
  An amber, similarly sampled from the warning path, marks pre-releases.
- **Neutral ramp:** five lightness stops, `L 0.15 / 0.35 / 0.65 / 0.92 / 0.98`,
  at chroma `≤ 0.005` with a slightly **warm** hue.
- **One accent per surface.** Two accents on one page is one too many.

*Argue.* The sentence, to be kept as a comment in the stylesheet: *the mark's
three strokes converge on one point, which is what the language claims — three
backends, one result — so the ramp from navy to bright cyan is the parity claim,
and the widgets that show parity use exactly that ramp.*

**Base colours.** Near-black, not pure black; warm white, not pure white. Take
`#0B0D14`-class charcoal and `#F2F0EF`-class warm white as the starting pair.

**Forbidden:** `#0D1117` with generic cyan or violet neon glow. That is the
lazy GitHub-dark answer and the skill names it specifically. Our near-black must
be neutral or warm, never blue-black, precisely because our accent is cyan.

**Contrast floors:** body text ≥ 4.5:1, large text ≥ 3:1. No exemptions.

### 8.2 Typography

- **Pairing:** `Geist Mono` or `JetBrains Mono` for display, labels, numbers and
  code; a grotesque sans for body. Self-host subsets. No CDN.
- **Banned as display faces:** Inter, Space Grotesk, Fraunces, Playfair Display,
  Roboto, Arial, system stacks. Inter is allowed only as a 14–16px body worker.
- **Monospace is for labels, numbers, code and eyebrows — never for body
  paragraphs.** Monospace body inflates line length by about 30 per cent.
- **Scale ratio 1.2** (minor third). This is a dense documentation-and-data site
  with many levels, and 1.2 is the ratio for that. Base body 16–18px. Display
  sizes leave the scale and are set by viewport:
  `clamp(2rem, 1.2rem + 3.5vw, 4.5rem)`.
- **Maximum six levels.** More than that means the hierarchy is out of control.
- **Line length 65ch maximum on prose.** Unbounded line length is the single
  worst readability failure and it is free to fix.
- **Line height follows line length:** display 0.95–1.1, headings 1.1–1.3, short
  body 1.4–1.5, prose at the 65ch limit 1.6.
- **All numeric data:** `font-variant-numeric: tabular-nums slashed-zero`. Every
  benchmark table, every counter, every diff column. Without it the columns
  jitter and the data looks less trustworthy than it is.
- **Uppercase micro-labels** — the widget eyebrows — are the only place wide
  tracking is allowed: 12px, `letter-spacing: 0.08–0.15em`.
- `h1,h2,h3 { text-wrap: balance }`, `p { text-wrap: pretty }`,
  `font-synthesis: none` globally.

### 8.3 Layout

- **8pt grid.** Spacing values come from `8 / 16 / 24 / 32 / 48 / 64` and nowhere
  else. If `std/tw`'s theme spacing scale differs, change `theme.json` to match
  rather than deviating in a stylesheet.
- CSS Grid with named areas. `subgrid` to align card internals. Container
  queries for components that genuinely reflow.
- **Density is earned, per page.** `/compare` and `/releases` are dense: three or
  more pieces of *differentiating* information per screen. `/philosophy` is
  quiet. Density means content, not ornament.
- The dense-technical skeleton to reach for: a fixed left rail with mono section
  numbers (`01`, `02`), a scrolling main column, 1px hairline rules rather than
  cards.
- **Banned layouts:** bento grids by reflex; the hero → three-column features →
  testimonials → call-to-action template; grids where every card is identical.

### 8.4 Motion

- **Default easing is `expoOut`: `cubic-bezier(0.16, 1, 0.3, 1)`.** Not
  `ease-out`, not `linear`.
- Toggles and buttons use overshoot: `cubic-bezier(0.34, 1.56, 0.64, 1)`.
  Continuous motion, such as a scrubber, uses `ease-in-out`. Exits use `ease-in`.
- **Durations:** hover and tooltip 100–300 ms; panel, page and list transitions
  300–800 ms; a narrative replay 2–10 s, never longer.
- **Stagger 30 ms per item**, with `translateY(10px) → 0` and opacity.
- **Hold before a payoff: at least 300 ms, ideally 500 ms.** The parity widget
  must pause before the `0 bytes` lands. The pause is the widget.
- **Animate `transform` and `opacity` only.** Never `top`, `left`, `width`,
  `height` or `margin`.
- **Never `transition: all`.** Never a fade to black at the end of a sequence —
  end on a hard cut and hold.
- Prefer one orchestrated page entrance over scattered micro-interactions.
- **`prefers-reduced-motion`.** The skill omits this entirely; we do not. Every
  animated widget must have a static end state, reachable without motion. The
  hero paints one frame. Replays jump to their final state. Scrubbers still work
  because they are user-driven.

### 8.5 Components

- **Radii:** small components 4px, buttons 8px, cards 12px — or go sharp, with
  no radius, if the chosen direction is the Swiss-technical one. Pick once.
- **Banned outright:** the rounded card with a 4px coloured left border. It is
  the signature of generated design. Emphasise with background contrast, weight,
  or a plain rule.
- **Borders:** 1px hairlines on near-black do the structural work. Any glow is a
  very faint border highlight, never a real bloom.
- **Shadows:** two layers, one long and soft, one near and light. On a dark
  page, prefer a hairline over a shadow.
- **Icons:** Lucide, Heroicons or Phosphor. **No emoji anywhere** — not in
  headings, not in feature lists, not in the terminal snippets.
- **SVG is legal for icons, geometric decoration and charts only.** No SVG-drawn
  scenes, devices or people.
- **No fabricated statistics.** Restated here because it is a design rule as much
  as an editorial one: every metric card traces to section 5's data files.
- Style the scrollbars: `scrollbar-width: thin; scrollbar-color: <neutral-3>
  transparent`.

### 8.6 Voice

The prose follows the repository's writing standard: ASD-STE100, Orwell, GOV.UK.
Short sentences. Active voice. One word, one meaning. Keep the project's nouns —
parity, validated type, capability, generator, region — and define each once, on
first use. Do not sell. State what the thing does and show it running.

---

## 9. Hosting and deploy

**Recommendation: GitHub Pages, via `actions/deploy-pages`.**

The repository is public, so Pages is free and has no usage cap that matters. The
deployment lives in the same workflow file as the data-gathering step from
section 5, which means one artifact, one job, one place to debug. No new account
and no new secret. Pages serves `.wasm` with the correct `application/wasm` type,
which is the only MIME requirement the site has.

The constraint worth stating: Pages sets no custom headers, so there is no
`COOP`/`COEP` and therefore no `SharedArrayBuffer` and no wasm threads. The site
uses neither. If the in-browser compiler ever wants threads, that is the day to
revisit this decision, and not before.

Alternatives, and why not now: Cloudflare Pages has better caching, analytics
and header control, and costs one account plus one API token in repository
secrets. Netlify and Vercel add edge functions the site does not need, and both
put a vendor between a language project and its own website. Revisit if traffic
or a redirect requirement demands it.

Custom domain when a name is chosen: a `CNAME` file in the published artifact and
a DNS record. Until then the `github.io` URL is honest.

---

## 10. Scope ladder

### v1 — the argument

- `/`, `/philosophy`, `/compare`, `/install`, `/releases`, and a single `/docs`
  entry page that links out.
- W1 ASCII hero, W2 parity visualizer, W3 install command, W4 benchmark charts,
  W5 comparison toggles, W6 memory replay, W7 release feed.
- Static export (G1), the SVG namespace fix (G2), Vyrn syntax highlighting (G3).
- `release.yml` and `site.yml`, with a seeded `bench/baseline.json`.
- Route sockets reserved for `/guide` and `/explorer`.

**v1 is done when** a visitor with JavaScript disabled still reads every page and
can copy the install command, and a visitor with JavaScript sees the hero paint
and the parity widget reach `0 bytes`.

### v1.1 — the proof on their machine

- The `wasm32-unknown-unknown` build of the frontend and codegen (section 7).
- W8 validated-types live demo, W9 playground, W10 wasm size counter,
  W11 generator explorer, W12 capability explainer.

### v2 — the depth

- **Guide book** at `/guide/*`. Needs `std/md` (G4). The socket already exists.
- **Package and dependency explorer** at `/explorer`. Reads `vyrn.lock` and the
  module graph; shows the reproducible-import story concretely.
- Reference browser over `docs/api/`, once `std/md` lands.
- Search over the guide and the reference.
- Localisation through `std/i18n`. The repository already carries `en` and `uk`
  locale files, so the site would be the largest live proof of the i18n
  generator.

---

## 11. Risks

| Risk | Effect | Response |
| --- | --- | --- |
| `vyrn-dom.js` cannot render SVG (G2) | Every chart blocked | Check this **first**, before anything else is built |
| Client wasm bundle over budget (G6) | Slow first paint | Measure early; landing page can ship with no client module |
| `bench/baseline.json` is a placeholder | W4 has no data | Seed it from a CI run; it is a prerequisite, not a task |
| No tags, no release workflow | W3 and W7 have no data | `release.yml` is the first thing built in v1 |
| Browser compiler port is harder than estimated | W8–W10 slip | They are v1.1 by design; v1 does not depend on them |
| Three-direction gate skipped | Rework | The gate is the first deliverable, and it is not optional |

---

## 12. What the build wave delivers first

1. Verify G2 by rendering one SVG element through `std/html` and `vyrn-dom.js`.
   Report what happens.
2. Three hero directions, rendered, screenshotted, structurally different. Stop
   and wait for the choice.

Nothing else starts until item 2 is answered.
